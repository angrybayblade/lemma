//! `lemma check --lsp`: a language server whose diagnostics are what each
//! repository's uncommitted changes break, in that repository and in the
//! others of its workspace.
//!
//! It watches the workspace's repositories as sentry does
//! (`.specs/14-sentry.md`) and keeps what it read between checks. When a
//! repository's changes settle, it checks that repository as `lemma check
//! --change` would and publishes the findings the change introduced: each
//! on the line of the code it names, whichever repository that is in, and
//! a summary on each file the change touched that links to all of them.
//! See [`diagnostics`].
//!
//! Checks read files from disk, so it follows saved edits, made by an
//! editor or by anything else, not unsaved ones.

mod diagnostics;

use std::collections::{BTreeMap, BTreeSet, VecDeque};
use std::io::{self, BufRead, BufReader, Write};
use std::path::{Path, PathBuf};
use std::process::ExitCode;
use std::sync::Arc;
use std::sync::mpsc::{self, Receiver, RecvTimeoutError};
use std::time::{Duration, Instant};

use lemma_graph::Report;
use lemma_lsp::lsp_types::{
    Diagnostic, DiagnosticRelatedInformation, DiagnosticSeverity, InitializeParams,
    InitializeResult, Location, MessageType, NumberOrString, Position, PositionEncodingKind,
    PublishDiagnosticsParams, Range, ServerCapabilities, ServerInfo, Url,
};
use lemma_lsp::{PositionEncoding, line_text, read_message, write_message};
use serde_json::{Value, json};

use crate::check::Served;
use crate::cli::CheckArgs;
use crate::watched::{BATCH_INTERVAL, WatchedWorkspace};
use diagnostics::{FileDiagnostics, LineDiagnostic};

/// The description `lemma check` reads when none is named.
const DEFAULT_MANIFEST: &str = "mod.lm";
/// The JSON-RPC code for a method the server does not know.
const METHOD_NOT_FOUND: i64 = -32601;
/// How long to wait for the editor to accept a progress report before
/// checking without one.
const PROGRESS_WAIT: Duration = Duration::from_millis(500);
/// At most this many of a repository's changed files carry its summary.
const TOUCHED_LIMIT: usize = 20;

/// Serves diagnostics over standard input and output until the editor
/// says `exit` or closes the stream.
pub(crate) fn run(args: &CheckArgs) -> ExitCode {
    let messages = match read_on_thread(BufReader::new(io::stdin())) {
        Ok(messages) => messages,
        Err(error) => {
            eprintln!("error: cannot read the editor's messages: {error}");
            return ExitCode::FAILURE;
        }
    };
    let mut output = io::stdout().lock();
    let mut server = Server::new(args);
    match server.serve(&messages, &mut output) {
        Ok(()) => ExitCode::SUCCESS,
        Err(error) => {
            eprintln!("error: cannot talk to the editor: {error}");
            ExitCode::FAILURE
        }
    }
}

/// Reads the editor's messages on a thread of their own, so the server can
/// look at the workspace between them.
fn read_on_thread(
    mut input: impl BufRead + Send + 'static,
) -> io::Result<Receiver<io::Result<Value>>> {
    let (sender, receiver) = mpsc::channel();
    std::thread::Builder::new()
        .name("lemma-check-lsp-reader".to_owned())
        .spawn(move || {
            while let Some(message) = read_message(&mut input).transpose() {
                let is_failure = message.is_err();
                if sender.send(message).is_err() || is_failure {
                    return;
                }
            }
        })?;
    Ok(receiver)
}

/// Whether to keep serving.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Flow {
    Continue,
    Exit,
}

struct Server<'a> {
    args: &'a CheckArgs,
    encoding: PositionEncoding,
    /// Whether the editor shows progress the server reports.
    reports_progress: bool,
    /// The workspace's description, absolute, once known.
    manifest: Option<PathBuf>,
    workspace: Option<WatchedWorkspace>,
    /// Each watched repository's directory, as a check names it.
    repositories: Vec<PathBuf>,
    /// Repositories, by the directory a check names them by, whose changes
    /// settled and wait for a check.
    pending: BTreeSet<PathBuf>,
    /// Repositories with uncommitted changes when the server started, checked
    /// one at a time while no settled change waits.
    at_start: VecDeque<PathBuf>,
    /// Each checked repository's diagnostics, by the name its change goes by.
    diagnosed: BTreeMap<String, FileDiagnostics>,
    /// What the editor was last given, by file.
    published: BTreeMap<PathBuf, Vec<Diagnostic>>,
    /// Messages that arrived while waiting for a reply, to handle next.
    deferred: VecDeque<Value>,
    /// The last request id the server gave out.
    last_id: u64,
}

impl<'a> Server<'a> {
    fn new(args: &'a CheckArgs) -> Self {
        Self {
            args,
            encoding: PositionEncoding::Utf16,
            reports_progress: false,
            manifest: None,
            workspace: None,
            repositories: Vec::new(),
            pending: BTreeSet::new(),
            at_start: VecDeque::new(),
            diagnosed: BTreeMap::new(),
            published: BTreeMap::new(),
            deferred: VecDeque::new(),
            last_id: 0,
        }
    }

    fn serve(
        &mut self,
        messages: &Receiver<io::Result<Value>>,
        output: &mut impl Write,
    ) -> io::Result<()> {
        loop {
            let next = match self.deferred.pop_front() {
                Some(message) => Ok(Ok(message)),
                None => messages.recv_timeout(BATCH_INTERVAL),
            };
            let flow = match next {
                Ok(message) => self.handle(&message?, output)?,
                Err(RecvTimeoutError::Timeout) => Flow::Continue,
                Err(RecvTimeoutError::Disconnected) => Flow::Exit,
            };
            if flow == Flow::Exit {
                return Ok(());
            }
            self.collect_settled();
            self.check_pending(messages, output)?;
        }
    }

    /// Answers a request or applies a notification. Replies to the
    /// server's own requests, and notifications it has no use for, are
    /// ignored.
    fn handle(&mut self, message: &Value, output: &mut impl Write) -> io::Result<Flow> {
        let method = message
            .get("method")
            .and_then(Value::as_str)
            .unwrap_or_default();
        let params = message.get("params").cloned().unwrap_or(Value::Null);
        match (method, message.get("id")) {
            ("exit", _) => return Ok(Flow::Exit),
            ("initialized", None) => self.start_watching(output)?,
            ("", _) | (_, None) => {}
            ("initialize", Some(id)) => {
                let result = self.initialize(params);
                reply(output, id, &result)?;
            }
            ("shutdown", Some(id)) => reply(output, id, &Value::Null)?,
            (other, Some(id)) => {
                let error = json!({
                    "code": METHOD_NOT_FOUND,
                    "message": format!("`lemma check --lsp` does not answer {other}"),
                });
                write_message(
                    output,
                    &json!({ "jsonrpc": "2.0", "id": id, "error": error }),
                )?;
            }
        }
        Ok(Flow::Continue)
    }

    fn initialize(&mut self, params: Value) -> Value {
        let params: InitializeParams = serde_json::from_value(params).unwrap_or_default();
        let offered = params
            .capabilities
            .general
            .as_ref()
            .and_then(|general| general.position_encodings.as_ref());
        let kind = if offered.is_some_and(|kinds| kinds.contains(&PositionEncodingKind::UTF8)) {
            PositionEncodingKind::UTF8
        } else {
            PositionEncodingKind::UTF16
        };
        self.encoding = PositionEncoding::negotiated(Some(&kind));
        self.reports_progress = params
            .capabilities
            .window
            .as_ref()
            .and_then(|window| window.work_done_progress)
            .unwrap_or(false);
        self.manifest = chosen_manifest(self.args, &params);
        let result = InitializeResult {
            capabilities: ServerCapabilities {
                position_encoding: Some(kind),
                ..ServerCapabilities::default()
            },
            server_info: Some(ServerInfo {
                name: "lemma check".to_owned(),
                version: Some(env!("CARGO_PKG_VERSION").to_owned()),
            }),
        };
        serde_json::to_value(result).unwrap_or(Value::Null)
    }

    /// Watches the workspace's repositories, and checks at once each that
    /// has uncommitted changes, so an editor opened on work in progress
    /// shows what it breaks.
    fn start_watching(&mut self, output: &mut impl Write) -> io::Result<()> {
        let Some(manifest) = self.manifest.clone() else {
            return show(
                output,
                MessageType::WARNING,
                "Lemma: no mod.lm found above the workspace folder, so changes are not checked. \
                 Set lemma.checkManifest to the workspace's mod.lm.",
            );
        };
        let mut workspace = match WatchedWorkspace::start(&manifest) {
            Ok(workspace) => workspace,
            Err(error) => {
                let message = format!("Lemma: cannot watch {}: {error:#}", manifest.display());
                return show(output, MessageType::ERROR, &message);
            }
        };
        // Takes what the watchers report as they start, which is no edit.
        workspace.collect(true);
        self.repositories = workspace
            .repositories()
            .iter()
            .map(|watched| watched.spelled().to_path_buf())
            .collect();
        self.at_start = self
            .repositories
            .iter()
            .filter(|repository| !uncommitted_files(repository).is_empty())
            .cloned()
            .collect();
        let message = format!(
            "watching {} repositories of {}",
            self.repositories.len(),
            manifest.display()
        );
        self.workspace = Some(workspace);
        log(output, MessageType::INFO, &message)
    }

    /// Notes each repository whose changes have settled, leaving out
    /// changes to the repositories inside it, which are watched on their
    /// own: a workspace's directory holds its repositories.
    fn collect_settled(&mut self) {
        let Some(workspace) = self.workspace.as_mut() else {
            return;
        };
        workspace.collect(false);
        for settled in workspace.take_settled(Instant::now()) {
            let own = own_paths(&settled.repository, &settled.paths, &self.repositories);
            log::debug!(
                "changes settled in {}: {own:?} of {} paths{}",
                settled.repository.display(),
                settled.paths.len(),
                if settled.is_bulk { ", in bulk" } else { "" }
            );
            // Nothing of its own changed: only Git's files, which a check's
            // own `git status` rewrites, or files of a repository inside it.
            if own.is_empty() && !settled.is_bulk {
                continue;
            }
            self.pending.insert(settled.repository);
        }
    }

    /// Checks each repository whose changes settled, publishing after each;
    /// with none waiting, the next repository that had changes at the
    /// start, so an edit made meanwhile is not kept waiting behind them.
    fn check_pending(
        &mut self,
        messages: &Receiver<io::Result<Value>>,
        output: &mut impl Write,
    ) -> io::Result<()> {
        let mut checking: Vec<PathBuf> = std::mem::take(&mut self.pending).into_iter().collect();
        if checking.is_empty() {
            checking.extend(self.at_start.pop_front());
        }
        for repository in checking {
            self.at_start.retain(|waiting| *waiting != repository);
            self.check_repository(&repository, messages, output)?;
            self.publish(output)?;
        }
        Ok(())
    }

    fn check_repository(
        &mut self,
        repository: &Path,
        messages: &Receiver<io::Result<Value>>,
        output: &mut impl Write,
    ) -> io::Result<()> {
        let (Some(manifest), Some(workspace)) = (self.manifest.clone(), self.workspace.as_mut())
        else {
            return Ok(());
        };
        // Drops what is kept of every change reported so far. This does not
        // wait for the watchers' barrier, as an ask to sentry does, since
        // that would forget which paths of other repositories changed: a
        // change reported after this check gets a check of its own.
        workspace.collect(false);
        let warm = Arc::clone(workspace.warm());
        let name = change_name(repository, &manifest);
        let progress = self.begin_progress(&name, messages, output)?;
        let started = Instant::now();
        let report = check_change(self.args, &manifest, &name, warm);
        let seconds = started.elapsed().as_secs_f64();
        if let Some(token) = progress {
            end_progress(output, &token)?;
        }
        let report = match report {
            Ok(report) => report,
            Err(error) => {
                let message = format!("could not check {name}: {error:#}");
                return log(output, MessageType::ERROR, &message);
            }
        };
        let touched = uncommitted_files(repository);
        let introduced = report
            .findings
            .iter()
            .filter(|finding| finding.introduced)
            .count();
        let message = format!("checked {name} in {seconds:.1}s: {introduced} findings introduced");
        self.diagnosed
            .insert(name, diagnostics::diagnose(&report, &touched));
        log(output, MessageType::INFO, &message)
    }

    /// Asks the editor to show that `repository` is being checked; the
    /// progress token, if it agreed in time.
    fn begin_progress(
        &mut self,
        repository: &str,
        messages: &Receiver<io::Result<Value>>,
        output: &mut impl Write,
    ) -> io::Result<Option<String>> {
        if !self.reports_progress {
            return Ok(None);
        }
        self.last_id += 1;
        let id = json!(format!("lemma-check-{}", self.last_id));
        let token = format!("lemma-check-{}", self.last_id);
        write_message(
            output,
            &json!({
                "jsonrpc": "2.0",
                "id": id,
                "method": "window/workDoneProgress/create",
                "params": { "token": token },
            }),
        )?;
        if !self.await_reply(&id, messages)? {
            return Ok(None);
        }
        let begin = json!({
            "kind": "begin",
            "title": "Lemma",
            "message": format!("checking what {repository} changes break"),
        });
        notify(
            output,
            "$/progress",
            &json!({ "token": token, "value": begin }),
        )?;
        Ok(Some(token))
    }

    /// Waits a moment for the reply to the server's request `id`; whether
    /// it came. Other messages are kept for the main loop.
    fn await_reply(
        &mut self,
        id: &Value,
        messages: &Receiver<io::Result<Value>>,
    ) -> io::Result<bool> {
        let deadline = Instant::now() + PROGRESS_WAIT;
        while let Some(left) = deadline.checked_duration_since(Instant::now()) {
            let Ok(message) = messages.recv_timeout(left) else {
                return Ok(false);
            };
            let message = message?;
            let is_reply = message.get("id") == Some(id) && message.get("method").is_none();
            if is_reply {
                return Ok(message.get("error").is_none());
            }
            self.deferred.push_back(message);
        }
        Ok(false)
    }

    /// Gives the editor each file's diagnostics that changed since it was
    /// last given them, and empty ones for files that have none now.
    fn publish(&mut self, output: &mut impl Write) -> io::Result<()> {
        let mut wanted: BTreeMap<&Path, Vec<&LineDiagnostic>> = BTreeMap::new();
        for files in self.diagnosed.values() {
            for (path, diagnostics) in files {
                wanted.entry(path).or_default().extend(diagnostics);
            }
        }
        let next: BTreeMap<PathBuf, Vec<Diagnostic>> = wanted
            .into_iter()
            .map(|(path, diagnostics)| {
                let converted = to_lsp(path, &diagnostics, self.encoding);
                (path.to_path_buf(), converted)
            })
            .collect();
        let paths: BTreeSet<&PathBuf> = self.published.keys().chain(next.keys()).collect();
        for path in paths {
            let now = next.get(path).cloned().unwrap_or_default();
            if self.published.get(path).cloned().unwrap_or_default() == now {
                continue;
            }
            let Ok(uri) = Url::from_file_path(path) else {
                continue;
            };
            let params = PublishDiagnosticsParams {
                uri,
                diagnostics: now,
                version: None,
            };
            let params = serde_json::to_value(params).unwrap_or(Value::Null);
            notify(output, "textDocument/publishDiagnostics", &params)?;
        }
        self.published = next;
        Ok(())
    }
}

/// Checks the change to the repository named `name` as `lemma check
/// --change` would, reading through `warm`.
fn check_change(
    args: &CheckArgs,
    manifest: &Path,
    name: &str,
    warm: Arc<lemma_graph::Warm>,
) -> anyhow::Result<Report> {
    let asked = CheckArgs {
        manifest: manifest.to_path_buf(),
        change: Some(name.to_owned()),
        // An editor's server has no one to ask, and cloning behind its back
        // would surprise.
        no_clone: true,
        since: None,
        lsp: false,
        ..args.clone()
    };
    let served = Served {
        manifest,
        warm,
        environment: Arc::new(std::env::vars().collect()),
    };
    crate::check::evaluate(&asked, Some(&served))
}

/// The description to check: the editor's `manifest` initialization
/// option, the one given on the command line, or the nearest `mod.lm` above
/// the editor's first folder that declares a workspace (else the nearest
/// `mod.lm`).
fn chosen_manifest(args: &CheckArgs, params: &InitializeParams) -> Option<PathBuf> {
    let configured = params
        .initialization_options
        .as_ref()
        .and_then(|options| options.get("manifest"))
        .and_then(Value::as_str)
        .filter(|manifest| !manifest.trim().is_empty());
    if let Some(manifest) = configured {
        return std::path::absolute(manifest).ok();
    }
    if args.manifest != Path::new(DEFAULT_MANIFEST) {
        return std::path::absolute(&args.manifest).ok();
    }
    let folder = params
        .workspace_folders
        .as_ref()
        .and_then(|folders| folders.first())
        .and_then(|folder| folder.uri.to_file_path().ok())
        .or_else(|| std::env::current_dir().ok())?;
    nearest_workspace_manifest(&folder)
}

/// The nearest `mod.lm` at or above `folder` that declares a workspace,
/// or else the nearest `mod.lm`.
fn nearest_workspace_manifest(folder: &Path) -> Option<PathBuf> {
    let found: Vec<PathBuf> = folder
        .ancestors()
        .map(|directory| directory.join(DEFAULT_MANIFEST))
        .filter(|manifest| manifest.is_file())
        .collect();
    let declares_workspace = |manifest: &&PathBuf| {
        std::fs::read_to_string(manifest).is_ok_and(|text| declares_workspace(&text))
    };
    found
        .iter()
        .find(declares_workspace)
        .or_else(|| found.first())
        .cloned()
}

/// Whether a module's text declares a `workspace { ... }` block.
fn declares_workspace(text: &str) -> bool {
    text.lines().any(|line| {
        let line = line.trim_start();
        line.strip_prefix("workspace")
            .is_some_and(|rest| rest.trim_start().starts_with('{'))
    })
}

/// The name `--change` takes for the repository in `directory`: `self`
/// for the workspace's own directory, otherwise the directory's name.
fn change_name(directory: &Path, manifest: &Path) -> String {
    let same = |left: &Path, right: &Path| {
        let canonical = |path: &Path| path.canonicalize().ok();
        left == right || canonical(left).is_some_and(|left| Some(left) == canonical(right))
    };
    if manifest
        .parent()
        .is_some_and(|parent| same(parent, directory))
    {
        return "self".to_owned();
    }
    directory
        .file_name()
        .map_or_else(String::new, |name| name.to_string_lossy().into_owned())
}

/// `paths`, relative to `repository`, that are not inside any other of
/// `repositories`.
fn own_paths<'p>(repository: &Path, paths: &'p [String], repositories: &[PathBuf]) -> Vec<&'p str> {
    let inner: Vec<&PathBuf> = repositories
        .iter()
        .filter(|other| *other != repository && other.starts_with(repository))
        .collect();
    paths
        .iter()
        .filter(|path| {
            let absolute = repository.join(path);
            !inner.iter().any(|other| absolute.starts_with(other))
        })
        .map(String::as_str)
        .collect()
}

/// The files Git says the working directory changed, added or left
/// untracked, absolute and sorted; none when Git cannot say, or when the
/// directory is not a repository's top, as a workspace's directory inside
/// another repository is not.
fn uncommitted_files(repository: &Path) -> Vec<PathBuf> {
    if !repository.join(".git").exists() {
        return Vec::new();
    }
    let output = std::process::Command::new("git")
        .arg("-C")
        .arg(repository)
        .args(["status", "--porcelain=v1", "-z", "--untracked-files=all"])
        .output();
    let Ok(output) = output else {
        return Vec::new();
    };
    if !output.status.success() {
        return Vec::new();
    }
    let mut files: Vec<PathBuf> = changed_paths(&String::from_utf8_lossy(&output.stdout))
        .into_iter()
        .map(|path| repository.join(path))
        .filter(|path| path.is_file())
        .collect();
    files.sort();
    files.truncate(TOUCHED_LIMIT);
    files
}

/// The paths in `git status --porcelain=v1 -z` output, each once: a rename
/// or copy gives its new path, not the old one that follows it.
fn changed_paths(status: &str) -> Vec<&str> {
    let mut paths = Vec::new();
    let mut entries = status.split('\0').filter(|entry| entry.len() > 3);
    while let Some(entry) = entries.next() {
        let (code, path) = entry.split_at(3);
        paths.push(path);
        if code.starts_with('R') || code.starts_with('C') {
            entries.next();
        }
    }
    paths
}

/// `diagnostics` of the file at `path` in LSP's terms, each spanning its
/// line from the first character that is not a space.
fn to_lsp(
    path: &Path,
    diagnostics: &[&LineDiagnostic],
    encoding: PositionEncoding,
) -> Vec<Diagnostic> {
    let text = std::fs::read_to_string(path).unwrap_or_default();
    diagnostics
        .iter()
        .map(|diagnostic| {
            let line = line_text(&text, diagnostic.line);
            let start = to_u32(line.len() - line.trim_start().len());
            let end = to_u32(line.trim_end().len()).max(start);
            let range = Range {
                start: Position::new(diagnostic.line, encoding.encode_column(line, start)),
                end: Position::new(diagnostic.line, encoding.encode_column(line, end)),
            };
            let related: Vec<DiagnosticRelatedInformation> = diagnostic
                .related
                .iter()
                .filter_map(|place| {
                    let uri = Url::from_file_path(&place.path).ok()?;
                    let at = Position::new(place.line, 0);
                    Some(DiagnosticRelatedInformation {
                        location: Location::new(uri, Range::new(at, at)),
                        message: place.message.clone(),
                    })
                })
                .collect();
            Diagnostic {
                range,
                severity: Some(severity(diagnostic.severity)),
                code: Some(NumberOrString::String(diagnostic.code.clone())),
                source: Some("lemma".to_owned()),
                message: diagnostic.message.clone(),
                related_information: (!related.is_empty()).then_some(related),
                ..Diagnostic::default()
            }
        })
        .collect()
}

fn severity(severity: lemma_graph::Severity) -> DiagnosticSeverity {
    match severity {
        lemma_graph::Severity::Error => DiagnosticSeverity::ERROR,
        lemma_graph::Severity::Warning => DiagnosticSeverity::WARNING,
        lemma_graph::Severity::Info => DiagnosticSeverity::INFORMATION,
    }
}

fn to_u32(value: usize) -> u32 {
    u32::try_from(value).unwrap_or(u32::MAX)
}

fn reply(output: &mut impl Write, id: &Value, result: &Value) -> io::Result<()> {
    write_message(
        output,
        &json!({ "jsonrpc": "2.0", "id": id, "result": result }),
    )
}

fn notify(output: &mut impl Write, method: &str, params: &Value) -> io::Result<()> {
    write_message(
        output,
        &json!({ "jsonrpc": "2.0", "method": method, "params": params }),
    )
}

fn end_progress(output: &mut impl Write, token: &str) -> io::Result<()> {
    let end = json!({ "kind": "end" });
    notify(
        output,
        "$/progress",
        &json!({ "token": token, "value": end }),
    )
}

/// Writes a line to the editor's log for this server.
fn log(output: &mut impl Write, kind: MessageType, message: &str) -> io::Result<()> {
    let params = json!({ "type": kind, "message": message });
    notify(output, "window/logMessage", &params)
}

/// Shows the person a message.
fn show(output: &mut impl Write, kind: MessageType, message: &str) -> io::Result<()> {
    let params = json!({ "type": kind, "message": message });
    notify(output, "window/showMessage", &params)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_rename_in_git_status_gives_only_its_new_path() {
        let status = " M src/link.ts\0R  new.ts\0old.ts\0?? added.py\0";

        let paths = changed_paths(status);

        assert_eq!(paths, ["src/link.ts", "new.ts", "added.py"]);
    }

    #[test]
    fn a_module_with_a_workspace_block_declares_a_workspace() {
        let workspace = "module shop\n\nworkspace {\n    repo a = local(\"a\")\n}\n";
        let service = "module billing\n\nservice api { }\n";

        assert!(declares_workspace(workspace));
        assert!(!declares_workspace(service));
    }

    #[test]
    fn changes_inside_a_repository_within_another_are_not_the_outer_ones() {
        let workspace = PathBuf::from("/work");
        let repositories = [workspace.clone(), PathBuf::from("/work/repos/billing")];
        let paths = ["mod.lm".to_owned(), "repos/billing/src/api.ts".to_owned()];

        let own = own_paths(&workspace, &paths, &repositories);

        assert_eq!(own, ["mod.lm"]);
    }

    #[test]
    fn the_workspace_directory_is_changed_as_self() {
        let manifest = Path::new("/work/mod.lm");

        assert_eq!(change_name(Path::new("/work"), manifest), "self");
        assert_eq!(
            change_name(Path::new("/work/repos/billing"), manifest),
            "billing"
        );
    }
}
