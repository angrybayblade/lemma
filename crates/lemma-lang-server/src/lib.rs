//! A language server for `.lm` modules, speaking LSP over a reader and a
//! writer (`lm lsp` uses standard input and output).
//!
//! Each open document is checked as the entry module of its program, with
//! every open document's unsaved text read in place of its file, so
//! diagnostics follow typing. Hovers, definitions, symbols and completions
//! come from the last check. See [`analysis`]. Formatting needs no check;
//! see [`formatting`].
//!
//! A thread reads the client's messages into a channel. The server takes
//! every message already waiting there, applies their changes in order,
//! and checks once, so a burst of edits costs one check instead of one
//! each. A request is answered after the changes sent before it have been
//! checked, so every answer is the one the client would get if the server
//! had handled the messages one by one.

mod analysis;
mod formatting;

use std::collections::{BTreeMap, BTreeSet};
use std::io::{self, BufRead, Write};
use std::path::{Path, PathBuf};
use std::sync::mpsc::{self, Receiver};

use lemma_graph::lm::CheckoutSources;
use lemma_graph::repos::Checkout;
use lemma_lang::check_program;
use lemma_lsp::lsp_types::{
    self, CompletionOptions, DocumentFormattingParams, HoverProviderCapability, InitializeParams,
    InitializeResult, OneOf, PositionEncodingKind, PublishDiagnosticsParams, ServerCapabilities,
    ServerInfo, TextDocumentPositionParams, TextDocumentSyncCapability, TextDocumentSyncKind, Url,
};
use lemma_lsp::{PositionEncoding, read_message, write_message};
use serde_json::{Value, json};

pub use analysis::Analysis;

/// Why serving stopped.
#[derive(Debug, thiserror::Error)]
pub enum ServerError {
    /// Reading from or writing to the client failed.
    #[error("cannot talk to the client")]
    Io(#[from] std::io::Error),
    /// The thread that reads the client's messages could not start.
    #[error("cannot start the thread that reads the client's messages")]
    Reader(#[source] std::io::Error),
}

/// Serves one client until it exits or its input ends. `cache` holds Lemma's
/// clones of pinned repositories and the written-out standard library.
pub fn serve(
    input: impl BufRead + Send + 'static,
    output: &mut impl Write,
    cache: &Path,
) -> Result<(), ServerError> {
    let messages = read_on_thread(input)?;
    let mut server = Server {
        documents: BTreeMap::new(),
        analyses: BTreeMap::new(),
        published: BTreeMap::new(),
        encoding: PositionEncoding::Utf16,
        cache: cache.to_path_buf(),
        is_stale: false,
    };
    while let Ok(first) = messages.recv() {
        let waiting: Vec<io::Result<Value>> =
            std::iter::once(first).chain(messages.try_iter()).collect();
        if server.handle_batch(waiting, output)? == Flow::Exit {
            return Ok(());
        }
    }
    Ok(())
}

/// Reads the client's messages on a thread of their own, so the server can
/// see every message already sent before it checks. The thread ends with
/// the input, after a read error, or once the server stops listening.
fn read_on_thread(
    mut input: impl BufRead + Send + 'static,
) -> Result<Receiver<io::Result<Value>>, ServerError> {
    let (sender, receiver) = mpsc::channel();
    std::thread::Builder::new()
        .name("lemma-lm-reader".to_owned())
        .spawn(move || {
            while let Some(message) = read_message(&mut input).transpose() {
                let is_failure = message.is_err();
                if sender.send(message).is_err() || is_failure {
                    return;
                }
            }
        })
        .map_err(ServerError::Reader)?;
    Ok(receiver)
}

/// Whether to keep serving.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Flow {
    /// Read the next messages.
    Continue,
    /// The client said `exit`.
    Exit,
}

/// The JSON-RPC code for a method the server does not know.
const METHOD_NOT_FOUND: i64 = -32601;
/// The JSON-RPC code for parameters that do not parse.
const INVALID_PARAMS: i64 = -32602;
/// The LSP code for a valid request that failed.
const REQUEST_FAILED: i64 = -32803;

struct Server {
    /// Open documents' texts, by path.
    documents: BTreeMap<PathBuf, String>,
    /// Each open document's last check, as its program's entry.
    analyses: BTreeMap<PathBuf, Analysis>,
    /// The files each open document's check published diagnostics for.
    published: BTreeMap<PathBuf, BTreeSet<PathBuf>>,
    encoding: PositionEncoding,
    cache: PathBuf,
    /// Whether a document changed since the last check.
    is_stale: bool,
}

impl Server {
    /// Handles messages in order, then checks once if any changed a
    /// document.
    fn handle_batch(
        &mut self,
        messages: Vec<io::Result<Value>>,
        output: &mut impl Write,
    ) -> Result<Flow, ServerError> {
        for message in messages {
            if self.handle(&message?, output)? == Flow::Exit {
                return Ok(Flow::Exit);
            }
        }
        self.check_if_stale(output)?;
        Ok(Flow::Continue)
    }

    /// Applies a notification, or answers a request once the changes sent
    /// before it have been checked.
    fn handle(&mut self, message: &Value, output: &mut impl Write) -> Result<Flow, ServerError> {
        let method = message
            .get("method")
            .and_then(Value::as_str)
            .unwrap_or_default()
            .to_owned();
        if method == "exit" {
            return Ok(Flow::Exit);
        }
        let params = message.get("params").cloned().unwrap_or(Value::Null);
        let Some(id) = message.get("id").cloned() else {
            self.notify(&method, &params, output)?;
            return Ok(Flow::Continue);
        };
        if method.is_empty() {
            return Ok(Flow::Continue);
        }
        self.check_if_stale(output)?;
        let response = match self.request(&method, params) {
            Ok(result) => json!({ "jsonrpc": "2.0", "id": id, "result": result }),
            Err((code, error)) => {
                json!({ "jsonrpc": "2.0", "id": id, "error": { "code": code, "message": error } })
            }
        };
        write_message(output, &response)?;
        Ok(Flow::Continue)
    }

    /// Checks the open documents and publishes, if one changed since the
    /// last check.
    fn check_if_stale(&mut self, output: &mut impl Write) -> Result<(), ServerError> {
        if !self.is_stale {
            return Ok(());
        }
        self.is_stale = false;
        self.reanalyze(output)
    }

    fn request(&mut self, method: &str, params: Value) -> Result<Value, (i64, String)> {
        match method {
            "initialize" => {
                let params: InitializeParams = parse(params)?;
                Ok(self.initialize(&params))
            }
            "shutdown" => Ok(Value::Null),
            "textDocument/hover" => {
                self.at_position(&params, |analysis, module, offset, encoding| {
                    serde_json::to_value(analysis.hover(module, offset, encoding))
                        .unwrap_or(Value::Null)
                })
            }
            "textDocument/definition" => {
                self.at_position(&params, |analysis, module, offset, encoding| {
                    serde_json::to_value(analysis.definition(module, offset, encoding))
                        .unwrap_or(Value::Null)
                })
            }
            "textDocument/completion" => {
                self.at_position(&params, |analysis, module, offset, _| {
                    serde_json::to_value(analysis.completions(module, offset))
                        .unwrap_or(Value::Null)
                })
            }
            "textDocument/documentSymbol" => {
                let params: lsp_types::DocumentSymbolParams = parse(params)?;
                let path = file_path(&params.text_document.uri)?;
                let symbols = self
                    .analysis_of(&path)
                    .map(|(analysis, module)| analysis.symbols(module, self.encoding))
                    .unwrap_or_default();
                Ok(serde_json::to_value(symbols).unwrap_or(Value::Null))
            }
            "textDocument/formatting" => {
                let params: DocumentFormattingParams = parse(params)?;
                self.formatting(&params)
            }
            _ => Err((METHOD_NOT_FOUND, format!("`{method}` is not supported"))),
        }
    }

    fn initialize(&mut self, params: &InitializeParams) -> Value {
        let offered = params
            .capabilities
            .general
            .as_ref()
            .and_then(|general| general.position_encodings.as_ref());
        let prefers_bytes =
            offered.is_some_and(|kinds| kinds.contains(&PositionEncodingKind::UTF8));
        let kind = if prefers_bytes {
            PositionEncodingKind::UTF8
        } else {
            PositionEncodingKind::UTF16
        };
        self.encoding = PositionEncoding::negotiated(Some(&kind));
        let result = InitializeResult {
            capabilities: ServerCapabilities {
                position_encoding: Some(kind),
                text_document_sync: Some(TextDocumentSyncCapability::Kind(
                    TextDocumentSyncKind::FULL,
                )),
                hover_provider: Some(HoverProviderCapability::Simple(true)),
                definition_provider: Some(OneOf::Left(true)),
                document_symbol_provider: Some(OneOf::Left(true)),
                document_formatting_provider: Some(OneOf::Left(true)),
                completion_provider: Some(CompletionOptions {
                    trigger_characters: Some(vec![".".to_owned()]),
                    ..CompletionOptions::default()
                }),
                ..ServerCapabilities::default()
            },
            server_info: Some(ServerInfo {
                name: "lemma".to_owned(),
                version: Some(env!("CARGO_PKG_VERSION").to_owned()),
            }),
        };
        serde_json::to_value(result).unwrap_or(Value::Null)
    }

    /// The edits that format an open document; `null` for a document that
    /// is not open or has syntax errors.
    fn formatting(&self, params: &DocumentFormattingParams) -> Result<Value, (i64, String)> {
        let path = file_path(&params.text_document.uri)?;
        let Some(text) = self.documents.get(&path) else {
            return Ok(Value::Null);
        };
        let edits = formatting::edits(text, self.encoding)
            .map_err(|error| (REQUEST_FAILED, error.to_string()))?;
        Ok(serde_json::to_value(edits).unwrap_or(Value::Null))
    }

    /// Answers a request at a position with `answer`, from the document's
    /// last check; `null` when the document was never checked.
    fn at_position(
        &self,
        params: &Value,
        answer: impl Fn(&Analysis, &str, u32, PositionEncoding) -> Value,
    ) -> Result<Value, (i64, String)> {
        let params: TextDocumentPositionParams = parse_position(params)?;
        let path = file_path(&params.text_document.uri)?;
        let Some((analysis, module)) = self.analysis_of(&path) else {
            return Ok(Value::Null);
        };
        let Some(offset) = analysis.offset(module, params.position, self.encoding) else {
            return Ok(Value::Null);
        };
        Ok(answer(analysis, module, offset, self.encoding))
    }

    /// The check that has `path` as its entry, and the module it is.
    fn analysis_of(&self, path: &Path) -> Option<(&Analysis, &str)> {
        let analysis = self.analyses.get(path)?;
        let module = analysis.module_at(path)?;
        Some((analysis, module))
    }

    /// Applies a notification. A change to a document is checked later,
    /// with every other change waiting by then.
    fn notify(
        &mut self,
        method: &str,
        params: &Value,
        output: &mut impl Write,
    ) -> Result<(), ServerError> {
        let document = |params: &Value| {
            params
                .pointer("/textDocument/uri")
                .and_then(Value::as_str)
                .and_then(|uri| Url::parse(uri).ok())
                .and_then(|uri| uri.to_file_path().ok())
        };
        let Some(path) = document(params) else {
            return Ok(());
        };
        match method {
            "textDocument/didOpen" => {
                let text = params
                    .pointer("/textDocument/text")
                    .and_then(Value::as_str)
                    .unwrap_or_default();
                self.documents.insert(path.clone(), text.to_owned());
            }
            "textDocument/didChange" => {
                let changes = params.get("contentChanges").and_then(Value::as_array);
                let last = changes.and_then(|changes| changes.last());
                if let Some(text) = last
                    .and_then(|change| change.get("text"))
                    .and_then(Value::as_str)
                {
                    self.documents.insert(path.clone(), text.to_owned());
                }
            }
            "textDocument/didSave" => {}
            "textDocument/didClose" => {
                self.documents.remove(&path);
                self.analyses.remove(&path);
                let cleared = self.published.remove(&path).unwrap_or_default();
                return self.publish_cleared(&cleared, output);
            }
            _ => return Ok(()),
        }
        self.is_stale = true;
        Ok(())
    }

    /// Checks every open document again, since an edit to one may change
    /// what another imports, and publishes what changed.
    fn reanalyze(&mut self, output: &mut impl Write) -> Result<(), ServerError> {
        let paths: Vec<PathBuf> = self.documents.keys().cloned().collect();
        for path in paths {
            let analysis = self.analyze(&path);
            let diagnostics = analysis.diagnostics(self.encoding);
            let files: BTreeSet<PathBuf> = diagnostics.keys().cloned().collect();
            let previous = self
                .published
                .insert(path.clone(), files.clone())
                .unwrap_or_default();
            for (file, found) in diagnostics {
                publish(output, &file, found)?;
            }
            let stale: BTreeSet<PathBuf> = previous.difference(&files).cloned().collect();
            self.publish_cleared(&stale, output)?;
            self.analyses.insert(path, analysis);
        }
        Ok(())
    }

    fn publish_cleared(
        &self,
        files: &BTreeSet<PathBuf>,
        output: &mut impl Write,
    ) -> Result<(), ServerError> {
        let still_published: BTreeSet<&PathBuf> = self.published.values().flatten().collect();
        for file in files.iter().filter(|file| !still_published.contains(file)) {
            publish(output, file, Vec::new())?;
        }
        Ok(())
    }

    /// Checks the program whose entry is the document at `path`.
    fn analyze(&self, path: &Path) -> Analysis {
        let (root, entry) = entry_of(path);
        let checkout = Checkout { root, commit: None };
        let mut sources =
            CheckoutSources::new(checkout, &self.cache).with_overlay(self.documents.clone());
        let checked = check_program(&mut sources, &entry);
        let entry_repository = checked
            .loaded
            .modules
            .get(&checked.loaded.entry)
            .map(|module| module.repository.clone())
            .unwrap_or_default();
        let library = self.cache.join("stdlib");
        write_library(&library);
        Analysis {
            roots: sources.roots(&entry_repository),
            checked,
            library,
        }
    }
}

/// The repository root a document belongs to, and its path from there. The
/// nearest `mod.lm` above the document says where the root is: it declares
/// the repository's module there, and a directory's module below it, one
/// directory down per name after the first (`billing.checks` in
/// `checks/mod.lm`). Without one, the document's own directory is its root.
fn entry_of(path: &Path) -> (PathBuf, String) {
    let directory = path.parent().map(Path::to_path_buf).unwrap_or_default();
    let mut candidate = Some(directory.as_path());
    while let Some(current) = candidate {
        let module_file = current.join(lemma_lang::load::MODULE_FILE);
        if module_file.is_file() {
            let depth = declared_module(&module_file)
                .map_or(0, |name| name.split('.').count().saturating_sub(1));
            let root = current.ancestors().nth(depth).unwrap_or(current);
            if let Ok(relative) = path.strip_prefix(root) {
                let entry = relative.to_string_lossy().replace('\\', "/");
                return (root.to_path_buf(), entry);
            }
        }
        candidate = current.parent();
    }
    let name = path
        .file_name()
        .map(|name| name.to_string_lossy().into_owned())
        .unwrap_or_default();
    (directory, name)
}

/// The module a file's `module` line declares, if it reads and has one.
fn declared_module(file: &Path) -> Option<String> {
    let text = std::fs::read_to_string(file).ok()?;
    let parsed = lemma_lang::syntax::parse(&text);
    parsed.file.module.map(|name| name.dotted())
}

/// Writes the standard library's modules under `directory`, so a
/// definition in one has a file to open. Failing only loses that.
fn write_library(directory: &Path) {
    for module in lemma_lang::stdlib::module_names() {
        let Some(text) = lemma_lang::stdlib::source(module) else {
            continue;
        };
        let path = directory.join(format!("{}.lm", module.replace('.', "/")));
        let is_current = std::fs::read_to_string(&path).is_ok_and(|existing| existing == text);
        if is_current {
            continue;
        }
        if let Some(parent) = path.parent() {
            let _ = std::fs::create_dir_all(parent);
        }
        if let Err(error) = std::fs::write(&path, text) {
            log::debug!("cannot write {}: {error}", path.display());
        }
    }
}

fn publish(
    output: &mut impl Write,
    file: &Path,
    diagnostics: Vec<lsp_types::Diagnostic>,
) -> Result<(), ServerError> {
    let Ok(uri) = Url::from_file_path(file) else {
        return Ok(());
    };
    let params = PublishDiagnosticsParams {
        uri,
        diagnostics,
        version: None,
    };
    let message = json!({
        "jsonrpc": "2.0",
        "method": "textDocument/publishDiagnostics",
        "params": params,
    });
    write_message(output, &message)?;
    Ok(())
}

fn parse<T: serde::de::DeserializeOwned>(params: Value) -> Result<T, (i64, String)> {
    serde_json::from_value(params).map_err(|error| (INVALID_PARAMS, error.to_string()))
}

/// The document and position of a request that has more fields besides.
fn parse_position(params: &Value) -> Result<TextDocumentPositionParams, (i64, String)> {
    let text_document = params.get("textDocument").cloned().unwrap_or(Value::Null);
    let position = params.get("position").cloned().unwrap_or(Value::Null);
    parse(json!({ "textDocument": text_document, "position": position }))
}

fn file_path(uri: &Url) -> Result<PathBuf, (i64, String)> {
    uri.to_file_path()
        .map_err(|()| (INVALID_PARAMS, format!("`{uri}` is not a file")))
}

#[cfg(test)]
mod tests;
