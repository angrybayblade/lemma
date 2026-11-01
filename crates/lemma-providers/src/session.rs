//! A running provider and the Lemma operations it answers.

use std::collections::{HashMap, VecDeque};
use std::fs;
use std::path::{Path, PathBuf};
use std::thread;
use std::time::{Duration, Instant};

use lemma_lsp::lsp_types::notification::{
    DidChangeTextDocument, DidChangeWatchedFiles, DidCloseTextDocument, DidOpenTextDocument,
};
use lemma_lsp::lsp_types::request::{GotoDefinition, GotoImplementation, HoverRequest, References};
use lemma_lsp::lsp_types::{
    self, DidChangeTextDocumentParams, DidChangeWatchedFilesParams, DidCloseTextDocumentParams,
    DidOpenTextDocumentParams, FileChangeType, FileEvent, GotoDefinitionParams,
    GotoDefinitionResponse, HoverContents, HoverParams, MarkedString, ReferenceContext,
    ReferenceParams, TextDocumentContentChangeEvent, TextDocumentIdentifier, TextDocumentItem,
    TextDocumentPositionParams, Url, VersionedTextDocumentIdentifier,
};
use lemma_lsp::{LaunchConfig, LspClient, LspError, PositionEncoding, line_text};
use lemma_model::{Language, Location, LocationQuery, Position, ProviderAnswer, Span};
use serde_json::Value;

use crate::semantic::{SemanticProvider, SourcePoint};
use crate::{ProviderDefinition, ProviderError};

/// How long a server must stay silent, with no work in progress, before it is
/// treated as ready. Only used when it does not publish diagnostics first.
const READY_QUIET_PERIOD: Duration = Duration::from_millis(500);

/// How a file changed since a server last looked at it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FileChange {
    /// The file is new.
    Created,
    /// The file's contents changed.
    Changed,
    /// The file no longer exists.
    Deleted,
}

/// A started language server scoped to one workspace root.
pub struct ProviderSession {
    provider_id: String,
    root: PathBuf,
    client: LspClient,
    /// Whether the server has been seen ready once. After that it has loaded
    /// the workspace, and later lookups need not wait again.
    has_been_ready: bool,
    /// Documents left open between lookups. Reopening a file makes a server
    /// analyse it again, which costs more than the lookup itself.
    open_documents: HashMap<Url, OpenDocument>,
    /// Open documents, least recently used first.
    open_order: VecDeque<Url>,
    /// Files marking projects the server only searches while one of their
    /// files is open; empty for servers that search the whole workspace.
    project_markers: Vec<String>,
    /// The project directory each directory belongs to, as found so far.
    project_of_directory: HashMap<PathBuf, Option<PathBuf>>,
    /// Whether projects were just opened and may still be loading. The
    /// server answers meanwhile, from the projects it has, and no message
    /// reliably says when it has finished.
    projects_loading: bool,
}

/// A document the server has open, as Lemma last sent it.
struct OpenDocument {
    version: i32,
    text_hash: blake3::Hash,
    /// The project the document belongs to, for project-scoped servers.
    project: Option<PathBuf>,
}

/// How often to ask again while newly opened projects load, and for how
/// long at most.
const SETTLE_INTERVAL: Duration = Duration::from_secs(1);
const SETTLE_LIMIT: Duration = Duration::from_secs(20);

/// Most projects opened for one question. A symbol mentioned in hundreds of
/// projects would otherwise make the server load all of them first.
const MAX_PROJECTS_PER_QUESTION: usize = 48;

/// Most documents kept open at once; the least recently used is closed.
const MAX_OPEN_DOCUMENTS: usize = 64;

impl ProviderSession {
    /// Starts the provider's server on a workspace root.
    pub fn start(
        definition: &ProviderDefinition,
        executable: &Path,
        root: &Path,
        request_timeout: Duration,
    ) -> Result<Self, ProviderError> {
        let config = LaunchConfig {
            command: executable.to_owned(),
            args: definition.args.clone(),
            root: root.to_owned(),
            initialization_options: definition.initialization_options.clone(),
            settings: definition.settings.clone().unwrap_or(Value::Null),
            request_timeout,
            ready_messages: definition.ready_messages.clone(),
        };
        let client = LspClient::start(&config).map_err(|source| ProviderError::Server {
            provider: definition.id.clone(),
            source,
        })?;
        Ok(Self {
            provider_id: definition.id.clone(),
            root: root.to_owned(),
            client,
            has_been_ready: false,
            open_documents: HashMap::new(),
            open_order: VecDeque::new(),
            project_markers: definition.project_markers.clone(),
            project_of_directory: HashMap::new(),
            projects_loading: false,
        })
    }

    /// Gets the server ready before any lookup: opens a file of the
    /// language so the server loads its project, and waits until it has.
    /// Path is relative to the root.
    pub fn warm_up(&mut self, path: &str, language: Language) -> Result<(), ProviderError> {
        let absolute_path = self.root.join(path);
        let source =
            fs::read_to_string(&absolute_path).map_err(|source| ProviderError::ReadSource {
                path: absolute_path.clone(),
                source,
            })?;
        let uri = Url::from_file_path(&absolute_path).map_err(|()| ProviderError::InvalidPath {
            path: absolute_path.clone(),
        })?;
        let opened_at = Instant::now();
        self.show_document(&uri, language, source)?;
        self.client
            .wait_until_ready(&uri, opened_at, READY_QUIET_PERIOD);
        self.has_been_ready = true;
        Ok(())
    }

    /// Tells the server about files that changed on disk since it started or
    /// last heard, so a long-lived server does not answer from stale files.
    /// Paths are relative to the root.
    ///
    /// Open documents among them are closed, so the server reads the new
    /// contents instead of keeping the text Lemma sent earlier.
    pub fn files_changed(&mut self, changes: &[(String, FileChange)]) -> Result<(), ProviderError> {
        if changes.is_empty() {
            return Ok(());
        }
        let mut events = Vec::with_capacity(changes.len());
        for (path, change) in changes {
            let absolute_path = self.root.join(path);
            let uri =
                Url::from_file_path(&absolute_path).map_err(|()| ProviderError::InvalidPath {
                    path: absolute_path,
                })?;
            if self.open_documents.remove(&uri).is_some() {
                self.open_order.retain(|open| open != &uri);
                self.close_document(uri.clone())?;
            }
            events.push(FileEvent::new(uri, file_change_type(*change)));
        }
        self.client
            .notify::<DidChangeWatchedFiles>(DidChangeWatchedFilesParams { changes: events })
            .map_err(|source| self.server_error(source))
    }

    /// The id of the server's process, which also names its process group.
    pub fn process_id(&self) -> u32 {
        self.client.process_id()
    }

    /// Pauses the server and the processes it started; see
    /// [`LspClient::pause`]. Returns whether the pause was delivered.
    pub fn pause(&self) -> bool {
        self.client.pause()
    }

    /// Resumes a paused server. Returns whether the signal was delivered.
    pub fn resume(&self) -> bool {
        self.client.resume()
    }

    /// The version the server reported, if any.
    pub fn provider_version(&self) -> Option<String> {
        self.client
            .server()
            .version
            .as_deref()
            .map(normalize_version)
    }

    /// Stops the server politely.
    pub fn shutdown(self) {
        self.client.shutdown();
    }

    /// Makes sure the server has the document open with `text`: opens it,
    /// sends the new text if it changed, or does nothing if it is current.
    fn show_document(
        &mut self,
        uri: &Url,
        language: Language,
        text: String,
    ) -> Result<(), ProviderError> {
        let text_hash = blake3::hash(text.as_bytes());
        self.open_order.retain(|open| open != uri);
        self.open_order.push_back(uri.clone());
        let Some(open) = self.open_documents.get_mut(uri) else {
            let project = self.project_of_uri(uri);
            self.open_documents.insert(
                uri.clone(),
                OpenDocument {
                    version: 1,
                    text_hash,
                    project,
                },
            );
            self.open_document(uri, language, text)?;
            return self.close_least_recent();
        };
        if open.text_hash == text_hash {
            return Ok(());
        }
        open.version += 1;
        open.text_hash = text_hash;
        let version = open.version;
        self.client
            .notify::<DidChangeTextDocument>(DidChangeTextDocumentParams {
                text_document: VersionedTextDocumentIdentifier {
                    uri: uri.clone(),
                    version,
                },
                content_changes: vec![TextDocumentContentChangeEvent {
                    range: None,
                    range_length: None,
                    text,
                }],
            })
            .map_err(|source| self.server_error(source))
    }

    /// The project directory a file belongs to: the nearest directory at
    /// or above it, within the root, holding a project marker.
    fn project_of(&mut self, file: &Path) -> Option<PathBuf> {
        if self.project_markers.is_empty() {
            return None;
        }
        let mut searched = Vec::new();
        let mut found = None;
        let mut directory = file.parent();
        while let Some(current) = directory {
            if !current.starts_with(&self.root) {
                break;
            }
            if let Some(known) = self.project_of_directory.get(current) {
                found.clone_from(known);
                break;
            }
            searched.push(current.to_owned());
            if self
                .project_markers
                .iter()
                .any(|marker| current.join(marker).is_file())
            {
                found = Some(current.to_owned());
                break;
            }
            directory = current.parent();
        }
        for directory in searched {
            self.project_of_directory.insert(directory, found.clone());
        }
        found
    }

    fn project_of_uri(&mut self, uri: &Url) -> Option<PathBuf> {
        let file = uri.to_file_path().ok()?;
        self.project_of(&file)
    }

    fn close_least_recent(&mut self) -> Result<(), ProviderError> {
        while self.open_order.len() > MAX_OPEN_DOCUMENTS {
            let Some(uri) = self.open_order.pop_front() else {
                break;
            };
            self.open_documents.remove(&uri);
            self.close_document(uri)?;
        }
        Ok(())
    }

    fn open_document(
        &self,
        uri: &Url,
        language: Language,
        text: String,
    ) -> Result<(), ProviderError> {
        self.client
            .notify::<DidOpenTextDocument>(DidOpenTextDocumentParams {
                text_document: TextDocumentItem {
                    uri: uri.clone(),
                    language_id: language_id(language).to_owned(),
                    version: 1,
                    text,
                },
            })
            .map_err(|source| self.server_error(source))
    }

    fn close_document(&self, uri: Url) -> Result<(), ProviderError> {
        self.client
            .notify::<DidCloseTextDocument>(DidCloseTextDocumentParams {
                text_document: TextDocumentIdentifier { uri },
            })
            .map_err(|source| self.server_error(source))
    }

    /// Opens the file at `at` in the server, waits for the server to load
    /// its project the first time, and returns where to point a request.
    fn prepare(
        &mut self,
        at: SourcePoint<'_>,
    ) -> Result<TextDocumentPositionParams, ProviderError> {
        let absolute_path = self.root.join(at.path);
        let source =
            fs::read_to_string(&absolute_path).map_err(|source| ProviderError::ReadSource {
                path: absolute_path.clone(),
                source,
            })?;
        let uri = Url::from_file_path(&absolute_path).map_err(|()| ProviderError::InvalidPath {
            path: absolute_path.clone(),
        })?;
        let encoding = self.client.server().position_encoding;
        let character =
            encoding.encode_column(line_text(&source, at.position.line), at.position.column);

        let opened_at = Instant::now();
        self.show_document(&uri, at.language, source)?;
        // Some servers answer before loading the workspace and return empty
        // results that look real. If readiness times out, ask anyway: the
        // answer is the best available.
        if !self.has_been_ready {
            self.client
                .wait_until_ready(&uri, opened_at, READY_QUIET_PERIOD);
            self.has_been_ready = true;
        }
        Ok(TextDocumentPositionParams {
            text_document: TextDocumentIdentifier { uri },
            position: lsp_types::Position {
                line: at.position.line,
                character,
            },
        })
    }

    /// Asks until the answer stops growing: two answers in a row with the
    /// same, non-zero number of locations elsewhere than the asked
    /// position, or the time limit.
    fn settled_locations(
        &self,
        query: LocationQuery,
        position: &TextDocumentPositionParams,
    ) -> Result<Vec<lsp_types::Location>, ProviderError> {
        let started = Instant::now();
        let mut previous = self
            .request_locations(query, position.clone())
            .map_err(|source| self.server_error(source))?;
        while started.elapsed() < SETTLE_LIMIT {
            thread::sleep(SETTLE_INTERVAL);
            let current = self
                .request_locations(query, position.clone())
                .map_err(|source| self.server_error(source))?;
            // While projects load the server may return only the asked
            // position itself, the declaration; that is not an answer yet.
            let found = elsewhere(&current, position);
            // Projects were opened because their files mention the name, so
            // an empty answer means the server is still loading them; only
            // the time limit ends the wait without one.
            let is_settled = found > 0 && found == elsewhere(&previous, position);
            previous = current;
            if is_settled {
                break;
            }
        }
        Ok(previous)
    }

    fn supports(&self, query: LocationQuery) -> bool {
        let server = self.client.server();
        match query {
            LocationQuery::References => server.supports_references(),
            LocationQuery::Definitions => server.supports_definitions(),
            LocationQuery::Implementations => server.supports_implementations(),
        }
    }

    fn request_locations(
        &self,
        query: LocationQuery,
        position: TextDocumentPositionParams,
    ) -> Result<Vec<lsp_types::Location>, LspError> {
        let work_done_progress_params = lsp_types::WorkDoneProgressParams::default();
        let partial_result_params = lsp_types::PartialResultParams::default();
        let response = match query {
            LocationQuery::References => {
                let found = self.client.request::<References>(ReferenceParams {
                    text_document_position: position,
                    work_done_progress_params,
                    partial_result_params,
                    context: ReferenceContext {
                        include_declaration: true,
                    },
                })?;
                return Ok(found.unwrap_or_default());
            }
            LocationQuery::Definitions => {
                self.client
                    .request::<GotoDefinition>(GotoDefinitionParams {
                        text_document_position_params: position,
                        work_done_progress_params,
                        partial_result_params,
                    })?
            }
            LocationQuery::Implementations => self.client.request::<GotoImplementation>(
                lsp_types::request::GotoImplementationParams {
                    text_document_position_params: position,
                    work_done_progress_params,
                    partial_result_params,
                },
            )?,
        };
        Ok(response.map(flatten_goto).unwrap_or_default())
    }

    fn to_locations(
        &self,
        found: Vec<lsp_types::Location>,
        encoding: PositionEncoding,
    ) -> Vec<Location> {
        let mut sources = SourceCache::default();
        let mut locations: Vec<_> = found
            .into_iter()
            .map(|location| {
                let (path, is_external) = self.display_path(&location.uri);
                let source = location
                    .uri
                    .to_file_path()
                    .ok()
                    .and_then(|file| sources.get(file));
                Location {
                    path,
                    span: decode_range(location.range, encoding, source),
                    is_external,
                }
            })
            .collect();
        locations.sort();
        locations.dedup();
        locations
    }

    /// The path to show for a URI: relative to the root when inside it.
    fn display_path(&self, uri: &Url) -> (String, bool) {
        let Ok(file) = uri.to_file_path() else {
            return (uri.to_string(), true);
        };
        match relative_to(&self.root, &file) {
            Some(relative) => (relative, false),
            None => (file.display().to_string(), true),
        }
    }

    fn server_error(&self, source: LspError) -> ProviderError {
        ProviderError::Server {
            provider: self.provider_id.clone(),
            source,
        }
    }
}

impl SemanticProvider for ProviderSession {
    fn version(&self) -> Option<String> {
        self.provider_version()
    }

    fn is_project_scoped(&self) -> bool {
        !self.project_markers.is_empty()
    }

    fn open_projects(&mut self, paths: &[&str]) -> Result<(), ProviderError> {
        let mut loaded: std::collections::HashSet<PathBuf> = self
            .open_documents
            .values()
            .filter_map(|open| open.project.clone())
            .collect();
        let mut opened = 0;
        for path in paths {
            if opened == MAX_PROJECTS_PER_QUESTION {
                break;
            }
            let absolute_path = self.root.join(path);
            let Some(project) = self.project_of(&absolute_path) else {
                continue;
            };
            let Some(language) = absolute_path
                .extension()
                .and_then(|extension| Language::from_extension(&extension.to_string_lossy()))
            else {
                continue;
            };
            if !loaded.insert(project) {
                continue;
            }
            let Ok(source) = fs::read_to_string(&absolute_path) else {
                continue;
            };
            let uri =
                Url::from_file_path(&absolute_path).map_err(|()| ProviderError::InvalidPath {
                    path: absolute_path,
                })?;
            self.show_document(&uri, language, source)?;
            opened += 1;
        }
        if opened > 0 {
            self.projects_loading = true;
        }
        Ok(())
    }

    fn locations(
        &mut self,
        query: LocationQuery,
        at: SourcePoint<'_>,
    ) -> Result<ProviderAnswer<Vec<Location>>, ProviderError> {
        if !self.supports(query) {
            return Ok(ProviderAnswer::Unsupported);
        }
        let position = self.prepare(at)?;
        let found = if self.projects_loading {
            self.projects_loading = false;
            self.settled_locations(query, &position)?
        } else {
            self.request_locations(query, position)
                .map_err(|source| self.server_error(source))?
        };
        if found.is_empty() {
            return Ok(ProviderAnswer::Empty);
        }
        let encoding = self.client.server().position_encoding;
        Ok(ProviderAnswer::Found(self.to_locations(found, encoding)))
    }

    fn hover(&mut self, at: SourcePoint<'_>) -> Result<ProviderAnswer<String>, ProviderError> {
        if !self.client.server().supports_hover() {
            return Ok(ProviderAnswer::Unsupported);
        }
        let position = self.prepare(at)?;
        let hover = self
            .client
            .request::<HoverRequest>(HoverParams {
                text_document_position_params: position,
                work_done_progress_params: lsp_types::WorkDoneProgressParams::default(),
            })
            .map_err(|source| self.server_error(source))?;
        let text = hover
            .map(|hover| hover_text(hover.contents))
            .unwrap_or_default();
        if text.trim().is_empty() {
            return Ok(ProviderAnswer::Empty);
        }
        Ok(ProviderAnswer::Found(text))
    }
}

/// How many locations are not the asked position itself.
fn elsewhere(locations: &[lsp_types::Location], asked: &TextDocumentPositionParams) -> usize {
    locations
        .iter()
        .filter(|location| {
            location.uri != asked.text_document.uri
                || location.range.start.line != asked.position.line
        })
        .count()
}

/// Definition and implementation answers come in three shapes; links point
/// at the name inside the target, which is what Lemma reports.
fn flatten_goto(response: GotoDefinitionResponse) -> Vec<lsp_types::Location> {
    match response {
        GotoDefinitionResponse::Scalar(location) => vec![location],
        GotoDefinitionResponse::Array(locations) => locations,
        GotoDefinitionResponse::Link(links) => links
            .into_iter()
            .map(|link| lsp_types::Location {
                uri: link.target_uri,
                range: link.target_selection_range,
            })
            .collect(),
    }
}

/// Hover contents as one Markdown string.
fn hover_text(contents: HoverContents) -> String {
    let marked = |marked: MarkedString| match marked {
        MarkedString::String(text) => text,
        MarkedString::LanguageString(code) => {
            format!("```{}\n{}\n```", code.language, code.value)
        }
    };
    match contents {
        HoverContents::Scalar(single) => marked(single),
        HoverContents::Array(parts) => parts
            .into_iter()
            .map(marked)
            .collect::<Vec<_>>()
            .join("\n\n"),
        HoverContents::Markup(markup) => markup.value,
    }
}

/// File contents read on demand for column conversion. Unreadable files are
/// remembered as missing so they are not retried.
#[derive(Default)]
struct SourceCache {
    files: HashMap<PathBuf, Option<String>>,
}

impl SourceCache {
    fn get(&mut self, path: PathBuf) -> Option<&str> {
        self.files
            .entry(path)
            .or_insert_with_key(|path| fs::read_to_string(path).ok())
            .as_deref()
    }
}

/// Some servers, such as gopls, report their whole build info as JSON in
/// the version field. Keep just the version from it.
fn normalize_version(reported: &str) -> String {
    serde_json::from_str::<Value>(reported)
        .ok()
        .and_then(|build_info| build_info.get("Version")?.as_str().map(str::to_owned))
        .unwrap_or_else(|| reported.to_owned())
}

/// The identifier LSP expects for each language.
fn file_change_type(change: FileChange) -> FileChangeType {
    match change {
        FileChange::Created => FileChangeType::CREATED,
        FileChange::Changed => FileChangeType::CHANGED,
        FileChange::Deleted => FileChangeType::DELETED,
    }
}

fn language_id(language: Language) -> &'static str {
    match language {
        Language::TypeScript => "typescript",
        Language::Tsx => "typescriptreact",
        Language::Python => "python",
        Language::Go => "go",
    }
}

/// Converts a server range to UTF-8 columns. Without the file's text the
/// columns are kept as the server sent them.
fn decode_range(range: lsp_types::Range, encoding: PositionEncoding, source: Option<&str>) -> Span {
    let decode = |position: lsp_types::Position| Position {
        line: position.line,
        column: source.map_or(position.character, |source| {
            encoding.decode_column(line_text(source, position.line), position.character)
        }),
    };
    Span {
        start: decode(range.start),
        end: decode(range.end),
    }
}

/// A `/`-separated path relative to `root`, or `None` if `file` is outside it.
fn relative_to(root: &Path, file: &Path) -> Option<String> {
    let relative = file
        .strip_prefix(root)
        .ok()
        .map(Path::to_owned)
        .or_else(|| {
            // The server may report a different spelling of the same path,
            // such as /var versus /private/var on macOS.
            let canonical = file.canonicalize().ok()?;
            canonical.strip_prefix(root).ok().map(Path::to_owned)
        })?;
    let parts: Vec<_> = relative
        .components()
        .map(|part| part.as_os_str().to_string_lossy())
        .collect();
    Some(parts.join("/"))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn lsp_range(line: u32, start: u32, end: u32) -> lsp_types::Range {
        lsp_types::Range {
            start: lsp_types::Position {
                line,
                character: start,
            },
            end: lsp_types::Position {
                line,
                character: end,
            },
        }
    }

    #[test]
    fn ranges_are_converted_to_utf8_columns() {
        let source = "const é = 1;\nuse(é);\n";

        let span = decode_range(lsp_range(1, 4, 5), PositionEncoding::Utf16, Some(source));

        assert_eq!(span.start, Position { line: 1, column: 4 });
        assert_eq!(span.end, Position { line: 1, column: 6 });
    }

    #[test]
    fn ranges_without_source_keep_server_columns() {
        let span = decode_range(lsp_range(0, 3, 7), PositionEncoding::Utf16, None);
        assert_eq!(span.end, Position { line: 0, column: 7 });
    }

    #[test]
    fn json_build_info_is_reduced_to_its_version() {
        let reported =
            r#"{"GoVersion":"go1.27.1","Path":"golang.org/x/tools/gopls","Version":"v0.24.0"}"#;
        assert_eq!(normalize_version(reported), "v0.24.0");
    }

    #[test]
    fn plain_versions_are_kept() {
        assert_eq!(normalize_version("7.0.2"), "7.0.2");
    }

    #[test]
    fn paths_inside_the_root_become_relative() {
        let relative = relative_to(Path::new("/repo"), Path::new("/repo/src/app.ts"));
        assert_eq!(relative.as_deref(), Some("src/app.ts"));
    }

    #[test]
    fn paths_outside_the_root_are_not_relative() {
        assert_eq!(
            relative_to(Path::new("/repo"), Path::new("/elsewhere/lib.d.ts")),
            None
        );
    }
}
