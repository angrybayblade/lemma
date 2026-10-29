//! Running a language server process and exchanging requests with it.

use std::collections::HashMap;
use std::io::BufReader;
use std::path::PathBuf;
use std::process::{Child, ChildStdin, ChildStdout, Command, Stdio};
use std::sync::atomic::{AtomicI64, Ordering};
use std::sync::mpsc::{self, RecvTimeoutError, Sender};
use std::sync::{Arc, Condvar, Mutex, MutexGuard, PoisonError};
use std::thread;
use std::time::{Duration, Instant};

use lsp_types::notification::{Cancel, Exit, Initialized, Notification};
use lsp_types::request::{Initialize, Request, Shutdown};
use lsp_types::{
    CancelParams, ClientCapabilities, ClientInfo, DynamicRegistrationClientCapabilities,
    GeneralClientCapabilities, InitializeParams, InitializedParams, NumberOrString, OneOf,
    PositionEncodingKind, ServerCapabilities, TextDocumentClientCapabilities,
    TextDocumentSyncClientCapabilities, Url, WindowClientCapabilities, WorkspaceClientCapabilities,
    WorkspaceFolder,
};
use serde_json::{Value, json};

use crate::LspError;
use crate::activity::{Activity, Readiness};
use crate::encoding::PositionEncoding;
use crate::message::{self, Incoming, ResponseError, ServerRequestContext};
use crate::process;
use crate::transport;

/// How long to wait for a polite shutdown before killing the process.
const SHUTDOWN_GRACE: Duration = Duration::from_secs(2);

/// How to start a language server.
#[derive(Debug, Clone)]
pub struct LaunchConfig {
    /// Resolved path to the server executable.
    pub command: PathBuf,
    /// Arguments, such as `--stdio`.
    pub args: Vec<String>,
    /// Workspace root the server analyzes. Also its working directory.
    pub root: PathBuf,
    /// Sent as `initializationOptions` during the handshake.
    pub initialization_options: Option<Value>,
    /// Returned when the server asks for `workspace/configuration`.
    pub settings: Value,
    /// How long to wait for any single request, including initialization.
    pub request_timeout: Duration,
    /// Patterns (`*` matches anything) for a `window/logMessage`, or the title
    /// of a finished `$/progress` report, that mean the server has loaded the
    /// workspace. Empty to rely on heuristics.
    pub ready_messages: Vec<String>,
}

/// What the server said about itself during initialization.
#[derive(Debug, Clone)]
pub struct ServerDetails {
    /// The server's name, if it reported one.
    pub name: Option<String>,
    /// The server's version, if it reported one.
    pub version: Option<String>,
    /// The unit the server counts columns in.
    pub position_encoding: PositionEncoding,
    /// Everything the server claims to support.
    pub capabilities: ServerCapabilities,
}

impl ServerDetails {
    /// Whether the server answers `textDocument/references`.
    pub fn supports_references(&self) -> bool {
        matches!(
            self.capabilities.references_provider,
            Some(OneOf::Left(true) | OneOf::Right(_))
        )
    }
}

/// A running, initialized language server.
///
/// Dropping the client kills the server; call [`LspClient::shutdown`] to stop
/// it politely.
pub struct LspClient {
    connection: Arc<Connection>,
    server: ServerDetails,
    request_timeout: Duration,
    // Declared last so it is dropped, and the process killed, after the
    // connection is no longer usable.
    process: ServerProcess,
}

impl LspClient {
    /// Starts the server and completes the initialize handshake.
    pub fn start(config: &LaunchConfig) -> Result<Self, LspError> {
        let mut process = ServerProcess::spawn(config)?;
        let (stdin, stdout) = process.take_pipes()?;

        let root_uri =
            Url::from_directory_path(&config.root).map_err(|()| LspError::InvalidRoot {
                path: config.root.clone(),
            })?;
        let root_name = config.root.file_name().map_or_else(
            || "workspace".to_owned(),
            |name| name.to_string_lossy().into_owned(),
        );
        let context = ServerRequestContext {
            settings: config.settings.clone(),
            workspace_folders: json!([{ "uri": root_uri, "name": root_name }]),
        };

        let connection = Arc::new(Connection::new(stdin, config.ready_messages.clone()));
        let reader_connection = Arc::clone(&connection);
        thread::Builder::new()
            .name("lsp-reader".to_owned())
            .spawn(move || read_loop(stdout, &reader_connection, &context))
            .map_err(LspError::Thread)?;

        let params = initialize_params(root_uri, root_name, config.initialization_options.clone());
        let initialized = connection.request::<Initialize>(params, config.request_timeout)?;
        connection.notify::<Initialized>(InitializedParams {})?;

        let server_info = initialized.server_info;
        let server = ServerDetails {
            name: server_info.as_ref().map(|info| info.name.clone()),
            version: server_info.and_then(|info| info.version),
            position_encoding: PositionEncoding::negotiated(
                initialized.capabilities.position_encoding.as_ref(),
            ),
            capabilities: initialized.capabilities,
        };
        Ok(Self {
            connection,
            server,
            request_timeout: config.request_timeout,
            process,
        })
    }

    /// What the server reported during initialization.
    pub fn server(&self) -> &ServerDetails {
        &self.server
    }

    /// The server's process id, which also names its process group.
    pub fn process_id(&self) -> u32 {
        self.process.child.id()
    }

    /// Pauses the server and every process it started, so an idle server
    /// uses no CPU and the operating system can compress or swap out its
    /// memory. Returns whether the pause was delivered.
    pub fn pause(&self) -> bool {
        cfg!(unix) && process::signal_group(self.process_id(), "STOP")
    }

    /// Resumes a paused server. Returns whether the signal was delivered.
    pub fn resume(&self) -> bool {
        cfg!(unix) && process::signal_group(self.process_id(), "CONT")
    }

    /// Sends a request and waits for its answer, up to the configured timeout.
    pub fn request<R: Request>(&self, params: R::Params) -> Result<R::Result, LspError> {
        self.connection.request::<R>(params, self.request_timeout)
    }

    /// Sends a notification.
    pub fn notify<N: Notification>(&self, params: N::Params) -> Result<(), LspError> {
        self.connection.notify::<N>(params)
    }

    /// Waits until the server looks ready to answer about a document opened
    /// at `opened_at`: it published diagnostics for it, or it went quiet for
    /// `quiet_period` with no work in progress. Gives up after the request
    /// timeout.
    pub fn wait_until_ready(
        &self,
        document: &Url,
        opened_at: Instant,
        quiet_period: Duration,
    ) -> Readiness {
        self.connection.wait_until_ready(
            document.as_str(),
            opened_at,
            quiet_period,
            self.request_timeout,
        )
    }

    /// Asks the server to exit, then kills it if it does not within a grace period.
    pub fn shutdown(mut self) {
        // Best effort: the server may already be gone, and the process is
        // killed on drop either way.
        let _ = self.connection.request::<Shutdown>((), SHUTDOWN_GRACE);
        let _ = self.connection.notify::<Exit>(());
        self.process.wait_for_exit(SHUTDOWN_GRACE);
    }
}

/// The child process, killed when dropped so a failed start never leaks it.
struct ServerProcess {
    child: Child,
}

impl ServerProcess {
    fn spawn(config: &LaunchConfig) -> Result<Self, LspError> {
        let mut command = Command::new(&config.command);
        command
            .args(&config.args)
            .current_dir(&config.root)
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::null());
        // Its own process group, so pausing or stopping it reaches the
        // processes it starts as well.
        #[cfg(unix)]
        std::os::unix::process::CommandExt::process_group(&mut command, 0);
        let child = command.spawn().map_err(|source| LspError::Spawn {
            command: config.command.display().to_string(),
            source,
        })?;
        Ok(Self { child })
    }

    fn take_pipes(&mut self) -> Result<(ChildStdin, ChildStdout), LspError> {
        let stdin = self.child.stdin.take().ok_or(LspError::Closed)?;
        let stdout = self.child.stdout.take().ok_or(LspError::Closed)?;
        Ok((stdin, stdout))
    }

    fn wait_for_exit(&mut self, grace: Duration) {
        let deadline = Instant::now() + grace;
        while Instant::now() < deadline {
            if !matches!(self.child.try_wait(), Ok(None)) {
                return;
            }
            thread::sleep(Duration::from_millis(20));
        }
    }
}

impl Drop for ServerProcess {
    fn drop(&mut self) {
        // The whole group, so helpers such as tsserver go too. Killing
        // processes that already exited fails harmlessly.
        if cfg!(unix) {
            process::signal_group(self.child.id(), "KILL");
        }
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}

type PendingRequests = HashMap<i64, Sender<Result<Value, ResponseError>>>;

/// The shared half of a client: used by callers to send, and by the reader
/// thread to deliver answers and respond to the server.
struct Connection {
    writer: Mutex<ChildStdin>,
    pending: Mutex<PendingRequests>,
    next_id: AtomicI64,
    activity: Mutex<Activity>,
    activity_changed: Condvar,
}

impl Connection {
    fn new(stdin: ChildStdin, ready_messages: Vec<String>) -> Self {
        Self {
            writer: Mutex::new(stdin),
            pending: Mutex::new(HashMap::new()),
            next_id: AtomicI64::new(1),
            activity: Mutex::new(Activity::new(Instant::now(), ready_messages)),
            activity_changed: Condvar::new(),
        }
    }

    fn request<R: Request>(
        &self,
        params: R::Params,
        timeout: Duration,
    ) -> Result<R::Result, LspError> {
        let params = to_params(R::METHOD, params)?;
        let id = self.next_id.fetch_add(1, Ordering::Relaxed);
        let (sender, receiver) = mpsc::channel();
        lock(&self.pending).insert(id, sender);

        let message = json!({ "jsonrpc": "2.0", "id": id, "method": R::METHOD, "params": params });
        if let Err(error) = self.send(&message) {
            lock(&self.pending).remove(&id);
            return Err(error);
        }

        match receiver.recv_timeout(timeout) {
            Ok(Ok(result)) => {
                serde_json::from_value(result).map_err(|source| LspError::InvalidResponse {
                    method: R::METHOD,
                    source,
                })
            }
            Ok(Err(error)) => Err(LspError::Server {
                method: R::METHOD,
                code: error.code,
                message: error.message,
            }),
            Err(RecvTimeoutError::Timeout) => {
                lock(&self.pending).remove(&id);
                // The server may have exited; a failed cancel changes nothing.
                let _ = self.notify::<Cancel>(CancelParams {
                    id: NumberOrString::Number(id_as_i32(id)),
                });
                Err(LspError::Timeout {
                    method: R::METHOD,
                    timeout,
                })
            }
            Err(RecvTimeoutError::Disconnected) => Err(LspError::Closed),
        }
    }

    fn notify<N: Notification>(&self, params: N::Params) -> Result<(), LspError> {
        let params = to_params(N::METHOD, params)?;
        self.send(&json!({ "jsonrpc": "2.0", "method": N::METHOD, "params": params }))
    }

    fn send(&self, message: &Value) -> Result<(), LspError> {
        transport::write_message(&mut *lock(&self.writer), message).map_err(LspError::Write)
    }

    fn deliver(&self, id: i64, outcome: Result<Value, ResponseError>) {
        let Some(sender) = lock(&self.pending).remove(&id) else {
            return;
        };
        // The caller may have timed out and stopped listening.
        let _ = sender.send(outcome);
    }

    fn record_activity(&self, method: Option<&str>, params: &Value) {
        lock(&self.activity).record(method, params, Instant::now());
        self.activity_changed.notify_all();
    }

    fn wait_until_ready(
        &self,
        uri: &str,
        opened_at: Instant,
        quiet_period: Duration,
        limit: Duration,
    ) -> Readiness {
        let deadline = Instant::now() + limit;
        let mut activity = lock(&self.activity);
        activity.touch(Instant::now());
        loop {
            let now = Instant::now();
            if let Some(readiness) = activity.readiness(uri, opened_at, quiet_period, now) {
                return readiness;
            }
            if now >= deadline {
                return Readiness::TimedOut;
            }
            // Wake on new activity, when the quiet period could end, or at the
            // deadline, whichever is first.
            let wake_after = activity
                .time_until_quiet(quiet_period, now)
                .max(Duration::from_millis(10))
                .min(deadline - now);
            activity = self
                .activity_changed
                .wait_timeout(activity, wake_after)
                .unwrap_or_else(PoisonError::into_inner)
                .0;
        }
    }

    /// Drops every waiting sender so blocked callers see the connection close.
    fn close(&self) {
        lock(&self.pending).clear();
    }
}

fn read_loop(stdout: ChildStdout, connection: &Connection, context: &ServerRequestContext) {
    let mut reader = BufReader::new(stdout);
    while let Ok(Some(raw)) = transport::read_message(&mut reader) {
        match message::classify(raw) {
            Some(Incoming::Response { id, outcome }) => {
                connection.record_activity(None, &Value::Null);
                connection.deliver(id, outcome);
            }
            Some(Incoming::Request { id, method, params }) => {
                connection.record_activity(None, &Value::Null);
                let answer = message::answer_server_request(&method, &params, context);
                // If the write fails the server is gone and the loop ends next read.
                let _ = connection.send(&message::response(&id, answer));
            }
            Some(Incoming::Notification { method, params }) => {
                connection.record_activity(Some(&method), &params);
            }
            None => {}
        }
    }
    connection.close();
}

fn to_params(method: &'static str, params: impl serde::Serialize) -> Result<Value, LspError> {
    serde_json::to_value(params).map_err(|source| LspError::InvalidRequest { method, source })
}

/// A poisoned lock only means another thread panicked mid-update; the maps and
/// pipe it guards are still usable.
fn lock<T>(mutex: &Mutex<T>) -> MutexGuard<'_, T> {
    mutex.lock().unwrap_or_else(PoisonError::into_inner)
}

/// Ids start at 1 and a single client never sends billions of requests.
fn id_as_i32(id: i64) -> i32 {
    i32::try_from(id).unwrap_or(i32::MAX)
}

#[expect(
    deprecated,
    reason = "some servers still read root_uri instead of workspace folders"
)]
fn initialize_params(
    root_uri: Url,
    root_name: String,
    initialization_options: Option<Value>,
) -> InitializeParams {
    InitializeParams {
        process_id: Some(std::process::id()),
        root_uri: Some(root_uri.clone()),
        initialization_options,
        capabilities: client_capabilities(),
        workspace_folders: Some(vec![WorkspaceFolder {
            uri: root_uri,
            name: root_name,
        }]),
        client_info: Some(ClientInfo {
            name: "lemma".to_owned(),
            version: Some(env!("CARGO_PKG_VERSION").to_owned()),
        }),
        ..InitializeParams::default()
    }
}

/// Only what Lemma actually uses. Claiming more invites servers to send
/// requests the client cannot handle.
fn client_capabilities() -> ClientCapabilities {
    ClientCapabilities {
        general: Some(GeneralClientCapabilities {
            position_encodings: Some(vec![
                PositionEncodingKind::UTF8,
                PositionEncodingKind::UTF16,
            ]),
            ..GeneralClientCapabilities::default()
        }),
        text_document: Some(TextDocumentClientCapabilities {
            synchronization: Some(TextDocumentSyncClientCapabilities {
                dynamic_registration: Some(false),
                ..TextDocumentSyncClientCapabilities::default()
            }),
            references: Some(DynamicRegistrationClientCapabilities {
                dynamic_registration: Some(false),
            }),
            ..TextDocumentClientCapabilities::default()
        }),
        workspace: Some(WorkspaceClientCapabilities {
            configuration: Some(true),
            workspace_folders: Some(true),
            ..WorkspaceClientCapabilities::default()
        }),
        window: Some(WindowClientCapabilities {
            work_done_progress: Some(true),
            ..WindowClientCapabilities::default()
        }),
        ..ClientCapabilities::default()
    }
}
