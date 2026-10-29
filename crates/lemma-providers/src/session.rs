//! A running provider and the Lemma operations it answers.

use std::collections::{HashMap, VecDeque};
use std::fs;
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use lemma_lsp::lsp_types::notification::{
    DidChangeTextDocument, DidChangeWatchedFiles, DidCloseTextDocument, DidOpenTextDocument,
};
use lemma_lsp::lsp_types::request::References;
use lemma_lsp::lsp_types::{
    self, DidChangeTextDocumentParams, DidChangeWatchedFilesParams, DidCloseTextDocumentParams,
    DidOpenTextDocumentParams, FileChangeType, FileEvent, Location, ReferenceContext,
    ReferenceParams, TextDocumentContentChangeEvent, TextDocumentIdentifier, TextDocumentItem,
    TextDocumentPositionParams, Url, VersionedTextDocumentIdentifier,
};
use lemma_lsp::{LaunchConfig, LspClient, LspError, PositionEncoding, line_text};
use lemma_model::{Language, Position, Reference, Span};
use serde_json::Value;

use crate::{ProviderDefinition, ProviderError};

/// How long a server must stay silent, with no work in progress, before it is
/// treated as ready. Only used when it does not publish diagnostics first.
const READY_QUIET_PERIOD: Duration = Duration::from_millis(500);

/// A provider's answer to a reference lookup, before Lemma adds context.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ReferenceAnswer {
    /// References, sorted and without duplicates.
    Found(Vec<Reference>),
    /// The server answered with nothing.
    Empty,
    /// The server does not support reference lookup.
    Unsupported,
}

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
}

/// A document the server has open, as Lemma last sent it.
struct OpenDocument {
    version: i32,
    text_hash: blake3::Hash,
}

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
    pub fn files_changed(&self, changes: &[(String, FileChange)]) -> Result<(), ProviderError> {
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
            events.push(FileEvent::new(uri, file_change_type(*change)));
        }
        self.client
            .notify::<DidChangeWatchedFiles>(DidChangeWatchedFilesParams { changes: events })
            .map_err(|source| self.server_error(source))
    }

    /// The version the server reported, if any.
    pub fn provider_version(&self) -> Option<String> {
        self.client
            .server()
            .version
            .as_deref()
            .map(normalize_version)
    }

    /// Finds references to the symbol whose identifier is at `position` in a
    /// file relative to the root.
    pub fn references(
        &mut self,
        path: &str,
        language: Language,
        position: Position,
        include_declaration: bool,
    ) -> Result<ReferenceAnswer, ProviderError> {
        if !self.client.server().supports_references() {
            return Ok(ReferenceAnswer::Unsupported);
        }

        let absolute_path = self.root.join(path);
        let source =
            fs::read_to_string(&absolute_path).map_err(|source| ProviderError::ReadSource {
                path: absolute_path.clone(),
                source,
            })?;
        let uri = Url::from_file_path(&absolute_path).map_err(|()| ProviderError::InvalidPath {
            path: absolute_path.clone(),
        })?;
        let encoding = self.client.server().position_encoding;
        let character = encoding.encode_column(line_text(&source, position.line), position.column);

        let opened_at = Instant::now();
        self.show_document(&uri, language, source)?;
        // Some servers answer before loading the workspace and return empty
        // results that look real. If readiness times out, ask anyway: the
        // answer is the best available.
        if !self.has_been_ready {
            self.client
                .wait_until_ready(&uri, opened_at, READY_QUIET_PERIOD);
            self.has_been_ready = true;
        }
        let locations = self.client.request::<References>(ReferenceParams {
            text_document_position: TextDocumentPositionParams {
                text_document: TextDocumentIdentifier { uri: uri.clone() },
                position: lsp_types::Position {
                    line: position.line,
                    character,
                },
            },
            work_done_progress_params: lsp_types::WorkDoneProgressParams::default(),
            partial_result_params: lsp_types::PartialResultParams::default(),
            context: ReferenceContext {
                include_declaration,
            },
        });
        let locations = locations
            .map_err(|source| self.server_error(source))?
            .unwrap_or_default();
        if locations.is_empty() {
            return Ok(ReferenceAnswer::Empty);
        }
        Ok(ReferenceAnswer::Found(
            self.to_references(locations, encoding),
        ))
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
            self.open_documents.insert(
                uri.clone(),
                OpenDocument {
                    version: 1,
                    text_hash,
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

    fn to_references(
        &self,
        locations: Vec<Location>,
        encoding: PositionEncoding,
    ) -> Vec<Reference> {
        let mut sources = SourceCache::default();
        let mut references: Vec<_> = locations
            .into_iter()
            .map(|location| {
                let (path, is_external) = self.display_path(&location.uri);
                let source = location
                    .uri
                    .to_file_path()
                    .ok()
                    .and_then(|file| sources.get(file));
                Reference {
                    path,
                    span: decode_range(location.range, encoding, source),
                    is_external,
                    enclosing_symbol: None,
                }
            })
            .collect();
        references.sort();
        references.dedup();
        references
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
