//! A background process that runs Lemma's commands with warm state.
//!
//! Every command spends most of its time on things a long-lived process can
//! do once: opening the database, and starting language servers that take
//! seconds to load a large project. So `lemma` forwards its arguments,
//! working directory and `PATH` to a per-user daemon over a Unix socket and
//! prints what comes back. The first command starts the daemon. It exits
//! after a period without commands, stopping its servers.

use std::collections::BTreeMap;
use std::ffi::OsString;
use std::fs;
use std::io::{BufRead, BufReader, Write};
use std::os::unix::ffi::OsStringExt;
use std::os::unix::fs::{MetadataExt, PermissionsExt};
use std::os::unix::net::{UnixListener, UnixStream};
use std::os::unix::process::CommandExt;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::sync::{Arc, Mutex, PoisonError};
use std::thread;
use std::time::{Duration, Instant, SystemTime};

use anyhow::{Context, bail};
use clap::Parser;
use serde::{Deserialize, Serialize};

use lemma_query::UsesAnswer;

use crate::anchors::UsesRequest;
use crate::cli::Cli;
use crate::config::{self, Config};
use crate::engine::{Engine, Invocation};
use crate::sentry::Sentry;

/// How long a starting daemon has to begin accepting connections.
const STARTUP_LIMIT: Duration = Duration::from_secs(5);
/// How often to retry connecting while a daemon starts or stops.
const CONNECT_RETRY: Duration = Duration::from_millis(2);
/// How long `lemma daemon stop` waits for the daemon to exit, and a new
/// daemon for a stopping one: it stops sentry and its language servers
/// first.
const STOP_LIMIT: Duration = Duration::from_secs(30);
/// How often idle language servers are checked.
const MAINTENANCE_INTERVAL: Duration = Duration::from_secs(15);

/// A message to the daemon, one JSON object per line.
#[derive(Debug, Serialize, Deserialize)]
#[serde(tag = "request", rename_all = "snake_case")]
enum Request {
    Run {
        /// Identifies the `lemma` binary, so a daemon from another build is
        /// replaced instead of answering with different code.
        build: String,
        /// Arguments after the program name.
        args: Vec<String>,
        cwd: PathBuf,
        /// The caller's `PATH`, as raw bytes.
        path_env: Option<Vec<u8>>,
    },
    /// Where a symbol under a root that need not be indexed is used, asked
    /// by `lemma verify` of the daemon's warm language servers.
    Uses {
        build: String,
        question: UsesRequest,
        cwd: PathBuf,
        path_env: Option<Vec<u8>>,
    },
    /// A check for sentry to answer, if it is attached to the check's
    /// workspace; asked by `lemma check` of a daemon already running.
    SentryAsk {
        build: String,
        /// Arguments after the program name.
        args: Vec<String>,
        cwd: PathBuf,
        /// The asking process's environment, for the commands a check runs.
        environment: BTreeMap<String, String>,
    },
    /// Attach sentry, detach it, or report on it.
    Sentry {
        build: String,
        action: SentryAction,
    },
    /// Apply the idle policy; sent by the daemon's own timer.
    Maintain,
    /// Get ready for the next graph through the workspaces whose files
    /// changed; sent by the daemon's own timer.
    Prepare,
    /// Report the running language servers.
    Status,
    Shutdown,
}

/// What `lemma daemon sentry` asks.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "action", rename_all = "snake_case")]
pub(crate) enum SentryAction {
    /// Attach to the workspace whose manifest is this absolute path.
    Start {
        manifest: PathBuf,
    },
    Stop,
    Status,
    /// What happened since a token, waiting up to `wait_ms` for something.
    Events {
        since: Option<String>,
        wait_ms: u64,
        json: bool,
    },
}

/// The daemon's reply, one JSON object per line.
#[derive(Debug, Serialize, Deserialize)]
#[serde(tag = "reply", rename_all = "snake_case")]
enum Reply {
    Done {
        stdout: String,
        stderr: String,
        exit_code: u8,
        /// The command's log lines, at the level it asked for.
        #[serde(default)]
        logs: String,
    },
    /// The answer to a `Uses` request.
    Uses {
        answer: UsesAnswer,
    },
    /// The daemon is from a different build and is exiting; start a new one.
    Restart,
    Stopping,
    /// Sentry is not attached to the check's workspace: check it yourself.
    NotServed,
    /// Sentry took the check; its answer follows on the same connection.
    Accepted,
    /// The answer to a `Prepare` request: whether any workspace is being
    /// kept ready for graphs, and so worth looking at again soon.
    Prepared {
        is_any_kept_ready: bool,
    },
}

/// What a forwarded command printed and returned.
pub(crate) struct Finished {
    pub stdout: String,
    pub stderr: String,
    pub exit_code: u8,
}

/// Prints log lines the daemon kept for this caller, as they would have
/// appeared had the command run here.
fn print_logs(logs: &str) {
    // Lines that cannot be written (a closed pipe) are not worth reporting.
    let _ = std::io::Write::write_all(&mut std::io::stderr(), logs.as_bytes());
}

/// Runs the command in the daemon, starting one if needed. `None` means no
/// daemon could be reached, and the caller should run the command itself.
pub(crate) fn forward(args: Vec<String>, cwd: PathBuf) -> Option<Finished> {
    let socket = socket_path().ok()?;
    let request = Request::Run {
        build: build_id(),
        args,
        cwd,
        path_env: std::env::var_os("PATH").map(OsString::into_vec),
    };
    log::debug!("forwarding to the daemon at {}", socket.display());
    let mut reply = exchange(&socket, &request).ok()?;
    if matches!(reply, Reply::Restart) {
        log::info!("the running daemon is another build; replacing it");
        wait_until_gone(&socket).ok()?;
        reply = exchange(&socket, &request).ok()?;
    }
    match reply {
        Reply::Done {
            stdout,
            stderr,
            exit_code,
            logs,
        } => {
            print_logs(&logs);
            log::info!("ran in the daemon, exit code {exit_code}");
            Some(Finished {
                stdout,
                stderr,
                exit_code,
            })
        }
        Reply::Uses { .. }
        | Reply::Restart
        | Reply::Stopping
        | Reply::NotServed
        | Reply::Accepted
        | Reply::Prepared { .. } => None,
    }
}

/// Where a symbol is used, answered by the running daemon. `None` means no
/// daemon of this build is running; none is started, since the caller can
/// start its own servers as quickly.
pub(crate) fn uses(question: &UsesRequest) -> Option<UsesAnswer> {
    let socket = socket_path().ok()?;
    let mut stream = UnixStream::connect(&socket).ok()?;
    let request = Request::Uses {
        build: build_id(),
        question: question.clone(),
        cwd: std::env::current_dir().ok()?,
        path_env: std::env::var_os("PATH").map(OsString::into_vec),
    };
    send(&mut stream, &request).ok()?;
    match receive::<Reply>(&mut stream).ok()? {
        Reply::Uses { answer } => Some(answer),
        Reply::Done { .. }
        | Reply::Restart
        | Reply::Stopping
        | Reply::NotServed
        | Reply::Accepted
        | Reply::Prepared { .. } => None,
    }
}

/// The running daemon's language servers, or `None` if no daemon runs.
pub(crate) fn status() -> anyhow::Result<Option<String>> {
    let socket = socket_path()?;
    let Ok(mut stream) = UnixStream::connect(&socket) else {
        return Ok(None);
    };
    send(&mut stream, &Request::Status)?;
    match receive::<Reply>(&mut stream)? {
        Reply::Done { stdout, .. } => Ok(Some(stdout)),
        Reply::Uses { .. }
        | Reply::Restart
        | Reply::Stopping
        | Reply::NotServed
        | Reply::Accepted
        | Reply::Prepared { .. } => Ok(None),
    }
}

/// Asks the daemon to exit. Returns whether one was running.
pub(crate) fn stop() -> anyhow::Result<bool> {
    let socket = socket_path()?;
    let Ok(mut stream) = UnixStream::connect(&socket) else {
        return Ok(false);
    };
    send(&mut stream, &Request::Shutdown)?;
    receive::<Reply>(&mut stream)?;
    wait_until_gone(&socket)?;
    Ok(true)
}

/// Serves commands until idle for the configured time, or `idle_override`.
/// The config file is read again whenever it changes.
pub(crate) fn serve(
    idle_override: Option<Duration>,
    logger: &'static crate::logging::Logger,
) -> anyhow::Result<()> {
    let socket = socket_path()?;
    let Some(lock) = take_lock(&socket)? else {
        // Another daemon serves, or won the race to start.
        return Ok(());
    };
    let Some(listener) = bind(&socket)? else {
        return Ok(());
    };
    let mut settings = Settings::load(config::config_path().as_deref())?;
    let mut engine = Engine::long_lived();
    engine.set_graph_watches(settings.config.graph_watches);
    let last_request = Arc::new(Mutex::new(Instant::now()));
    let idle_limit = idle_override.or(settings.config.idle_exit);
    if let Some(idle_limit) = idle_limit {
        watch_for_idleness(socket.clone(), Arc::clone(&last_request), idle_limit);
    }
    send_maintenance_ticks(socket.clone());
    send_prepare_ticks(socket.clone());
    let mut sentry: Option<Sentry> = None;
    let runtimes = start_runtimes(&socket);

    for stream in listener.incoming() {
        let Ok(mut stream) = stream else {
            continue;
        };
        let Ok(request) = receive::<Request>(&mut stream) else {
            continue;
        };
        let Some((request, mut stream)) = route_sentry(request, stream, &mut sentry, &settings)
        else {
            continue;
        };
        let is_status = matches!(request, Request::Status);
        if matches!(request, Request::Maintain) {
            maintain_runtimes(&runtimes, &mut engine, &settings.config);
        }
        let mut reply = reply_to(request, &mut engine, &mut settings, &last_request, logger);
        if let (true, Reply::Done { stdout, .. }) = (is_status, &mut reply) {
            stdout.push_str(&runtimes.status());
            stdout.push_str(&sentry.as_ref().map(Sentry::status).unwrap_or_default());
        }
        let is_exiting = matches!(reply, Reply::Stopping | Reply::Restart);
        // A caller that gave up is not a reason to stop serving others.
        let _ = send(&mut stream, &reply);
        if is_exiting {
            break;
        }
    }

    // Not serving from here: new connections are refused, and a caller runs
    // without this daemon or starts another, which waits for the lock.
    drop(listener);
    let _ = fs::remove_file(&socket);
    let _ = fs::remove_file(crate::runtime::socket_beside(&socket));
    drop(sentry);
    engine.shutdown();
    // Held until the process ends, when the system lets go of it: that is
    // the moment `lemma daemon stop` waits for.
    std::mem::forget(lock);
    Ok(())
}

/// The daemon's warm runtimes, listening for checks beside `socket`. One
/// that cannot listen only leaves checks to run their own commands.
fn start_runtimes(socket: &Path) -> Arc<crate::runtime::Runtimes> {
    let directory = lemma_graph::repos::cache_directory().join("runtimes");
    let runtimes = Arc::new(crate::runtime::Runtimes::new(directory, build_id()));
    if let Err(error) = runtimes.listen(&crate::runtime::socket_beside(socket)) {
        log::warn!("warm runtimes are not listening: {error}");
    }
    runtimes
}

/// Stops the warm runtimes idle longer than they are kept, and all of them
/// when they and the language servers together hold more than the servers'
/// memory budget: a runtime is rebuilt in a second, a server in many.
fn maintain_runtimes(runtimes: &crate::runtime::Runtimes, engine: &mut Engine, config: &Config) {
    runtimes.stop_idle();
    let Some(budget) = config.idle_policy.memory_budget_bytes else {
        return;
    };
    let held = runtimes.memory_bytes();
    if held > 0 && engine.servers_memory_bytes().saturating_add(held) > budget {
        runtimes.stop_all();
    }
}

/// The running daemon's warm runtimes, for a check of the description in
/// `directory`: `None` when no daemon runs, or the settings keep none warm.
/// No daemon is started for them.
pub(crate) fn warm_runtime(directory: &Path) -> Option<lemma_graph::runtime::Runtime> {
    let allowance = match config::runtime_allowance(directory) {
        Ok(allowance) => allowance,
        Err(error) => {
            log::warn!("warm runtimes are not used: {error:#}");
            return None;
        }
    };
    let socket = crate::runtime::socket_beside(&socket_path().ok()?);
    if allowance.memory_bytes == 0 || !socket.exists() {
        return None;
    }
    let asked = crate::runtime::Asked::new(socket, build_id(), allowance);
    Some(lemma_graph::runtime::Runtime(Arc::new(asked)))
}

/// Serves what is sentry's: a check asked of it, attaching or detaching it,
/// and detaching it from a workspace that is gone. Everything else comes
/// back, with its connection, for the daemon to serve.
fn route_sentry(
    request: Request,
    mut stream: UnixStream,
    sentry: &mut Option<Sentry>,
    settings: &Settings,
) -> Option<(Request, UnixStream)> {
    match request {
        Request::SentryAsk {
            build,
            args,
            cwd,
            environment,
        } => {
            let asked = Asked {
                build,
                args,
                cwd,
                environment,
            };
            hand_to_sentry(asked, stream, sentry, settings);
            None
        }
        Request::Sentry {
            build,
            action:
                SentryAction::Events {
                    since,
                    wait_ms,
                    json,
                },
        } if build == build_id() && sentry.is_some() => {
            if let Some(attached) = sentry.as_ref() {
                attached.events(crate::sentry::Wait {
                    since,
                    limit: Duration::from_millis(wait_ms),
                    json,
                    stream,
                });
            }
            None
        }
        Request::Sentry { build, action } => {
            let reply = control_sentry(&build, action, sentry);
            let _ = send(&mut stream, &reply);
            None
        }
        Request::Maintain if sentry.as_ref().is_some_and(Sentry::is_gone) => {
            log::info!("sentry: the workspace is gone; detaching");
            *sentry = None;
            Some((Request::Maintain, stream))
        }
        other => Some((other, stream)),
    }
}

/// A check asked of sentry, before its arguments are read.
struct Asked {
    build: String,
    args: Vec<String>,
    cwd: PathBuf,
    environment: BTreeMap<String, String>,
}

/// Gives a check to sentry when it is attached to the check's workspace,
/// attaching it first if the user opted in to that; otherwise replies that
/// the caller should check itself. Sentry answers on `stream` itself, so
/// the daemon goes on serving lookups meanwhile.
fn hand_to_sentry(
    asked: Asked,
    mut stream: UnixStream,
    sentry: &mut Option<Sentry>,
    settings: &Settings,
) {
    let not_served = |mut stream: UnixStream| {
        let _ = send(&mut stream, &Reply::NotServed);
    };
    if asked.build != build_id() {
        return not_served(stream);
    }
    let words = std::iter::once("lemma".to_owned()).chain(asked.args.iter().cloned());
    let Ok(cli) = Cli::try_parse_from(words) else {
        return not_served(stream);
    };
    let crate::cli::Command::Check(args) = cli.command else {
        return not_served(stream);
    };
    let manifest = crate::check::entry_manifest_in(&asked.cwd, &args.manifest);
    let manifest = manifest.canonicalize().unwrap_or(manifest);
    let wants_attach = settings.config.sentry_auto_attach
        && !asked.environment.contains_key("CI")
        && asked
            .environment
            .get("LEMMA_SENTRY")
            .is_none_or(|value| value != "off");
    if sentry.is_none() && wants_attach {
        match Sentry::attach(&manifest) {
            Ok(attached) => *sentry = Some(attached),
            Err(error) => log::warn!(
                "sentry: could not attach to {}: {error:#}",
                manifest.display()
            ),
        }
    }
    let Some(attached) = sentry
        .as_ref()
        .filter(|attached| attached.manifest() == manifest)
    else {
        return not_served(stream);
    };
    if send(&mut stream, &Reply::Accepted).is_err() {
        return;
    }
    attached.ask(crate::sentry::Ask {
        question: crate::sentry::Question {
            args,
            json: cli.json,
            raw: asked.args,
            cwd: asked.cwd,
            environment: asked.environment,
        },
        stream,
    });
}

/// Attaches, detaches or reports on sentry, for `lemma daemon sentry`.
fn control_sentry(build: &str, action: SentryAction, sentry: &mut Option<Sentry>) -> Reply {
    if build != build_id() {
        return Reply::Restart;
    }
    let failed = |message: String| Reply::Done {
        stdout: String::new(),
        stderr: format!("error: {message}\n"),
        exit_code: 1,
        logs: String::new(),
    };
    match action {
        SentryAction::Start { manifest } => {
            if let Some(attached) = sentry.as_ref() {
                return failed(format!(
                    "sentry is attached to {}; `lemma daemon sentry stop` first",
                    attached.manifest().display()
                ));
            }
            match Sentry::attach(&manifest) {
                Ok(attached) => {
                    let text = format!(
                        "sentry (experimental) attached to {}\n",
                        attached.manifest().display()
                    );
                    *sentry = Some(attached);
                    done(text)
                }
                Err(error) => failed(format!(
                    "could not attach to {}: {error:#}",
                    manifest.display()
                )),
            }
        }
        SentryAction::Stop => match sentry.take() {
            Some(attached) => done(format!(
                "sentry detached from {}\n",
                attached.manifest().display()
            )),
            None => failed("sentry is not attached".to_owned()),
        },
        SentryAction::Status | SentryAction::Events { .. } => match sentry.as_ref() {
            Some(attached) => done(attached.status()),
            None => done("sentry is not attached\n".to_owned()),
        },
    }
}

/// Sends a check's answer on the asking connection.
pub(crate) fn reply_answer(mut stream: UnixStream, answer: &crate::sentry::Answer) {
    let reply = Reply::Done {
        stdout: answer.stdout.clone(),
        stderr: answer.stderr.clone(),
        exit_code: answer.exit_code,
        logs: String::new(),
    };
    let _ = send(&mut stream, &reply);
}

/// Asks the running daemon's sentry to answer a check. `None` when no
/// daemon of this build runs (none is started for this) or sentry does not
/// serve the check's workspace.
pub(crate) fn ask_sentry(
    args: Vec<String>,
    cwd: PathBuf,
    environment: BTreeMap<String, String>,
) -> Option<crate::sentry::Answer> {
    let socket = socket_path().ok()?;
    let mut stream = UnixStream::connect(&socket).ok()?;
    let request = Request::SentryAsk {
        build: build_id(),
        args,
        cwd,
        environment,
    };
    send(&mut stream, &request).ok()?;
    let mut reader = BufReader::new(stream);
    if !matches!(read_reply(&mut reader)?, Reply::Accepted) {
        return None;
    }
    match read_reply(&mut reader)? {
        Reply::Done {
            stdout,
            stderr,
            exit_code,
            ..
        } => {
            log::info!("answered by sentry, exit code {exit_code}");
            Some(crate::sentry::Answer {
                stdout,
                stderr,
                exit_code,
            })
        }
        _ => None,
    }
}

fn read_reply(reader: &mut BufReader<UnixStream>) -> Option<Reply> {
    let mut line = String::new();
    reader.read_line(&mut line).ok()?;
    serde_json::from_str(&line).ok()
}

/// Sends `action` to the running daemon's sentry; `None` when no daemon
/// runs, since sentry only attaches to one already running.
pub(crate) fn sentry_control(action: SentryAction) -> anyhow::Result<Option<(String, String, u8)>> {
    let socket = socket_path()?;
    let Ok(mut stream) = UnixStream::connect(&socket) else {
        return Ok(None);
    };
    send(
        &mut stream,
        &Request::Sentry {
            build: build_id(),
            action,
        },
    )?;
    match receive::<Reply>(&mut stream)? {
        Reply::Done {
            stdout,
            stderr,
            exit_code,
            ..
        } => Ok(Some((stdout, stderr, exit_code))),
        Reply::Restart => anyhow::bail!(
            "the running daemon is another build of lemma; stop it with `lemma daemon stop`"
        ),
        _ => Ok(None),
    }
}

/// The daemon's reply to one request.
fn reply_to(
    request: Request,
    engine: &mut Engine,
    settings: &mut Settings,
    last_request: &Mutex<Instant>,
    logger: &crate::logging::Logger,
) -> Reply {
    match request {
        Request::Shutdown => Reply::Stopping,
        // The accept loop serves these itself; a stray one is not served.
        Request::SentryAsk { .. } | Request::Sentry { .. } => Reply::NotServed,
        Request::Maintain => {
            settings.reload_if_changed(config::config_path().as_deref());
            let config = &settings.config;
            engine.set_graph_watches(config.graph_watches);
            engine.maintain(&config.idle_policy, config.warms_up_on_activity);
            done(String::new())
        }
        Request::Prepare => {
            let config = &settings.config;
            // A workspace is kept ready for as long as an idle language
            // server is kept running: both are for work in progress.
            let asked_within = config.idle_policy.pause_after;
            Reply::Prepared {
                is_any_kept_ready: engine.prepare_graphs(
                    config.graph_prepare,
                    asked_within,
                    Instant::now(),
                ),
            }
        }
        Request::Status => done(engine.status()),
        Request::Run { ref build, .. } | Request::Uses { ref build, .. }
            if *build != build_id() =>
        {
            Reply::Restart
        }
        Request::Uses {
            question,
            cwd,
            path_env,
            ..
        } => {
            let invocation = invocation(cwd, path_env);
            busy(last_request, || Reply::Uses {
                answer: engine.uses(&question, &invocation),
            })
        }
        Request::Run {
            args,
            cwd,
            path_env,
            ..
        } => {
            let invocation = invocation(cwd, path_env);
            busy(last_request, || run(engine, args, &invocation, logger))
        }
    }
}

/// Where a caller's request comes from.
fn invocation(cwd: PathBuf, path_env: Option<Vec<u8>>) -> Invocation {
    Invocation {
        cwd,
        path_env: path_env.map(OsString::from_vec),
    }
}

/// Runs `work` for a caller, counting the daemon as busy, not idle, until
/// it finishes.
fn busy<T>(last_request: &Mutex<Instant>, work: impl FnOnce() -> T) -> T {
    *lock(last_request) = Instant::now();
    let result = work();
    *lock(last_request) = Instant::now();
    result
}

/// The config in force, and when its file last changed.
struct Settings {
    config: Config,
    modified: Option<SystemTime>,
}

impl Settings {
    fn load(path: Option<&Path>) -> anyhow::Result<Self> {
        Ok(Self {
            config: config::load(path)?,
            modified: config::modified(path),
        })
    }

    /// Picks up edits to the config file. A file that no longer parses
    /// keeps the previous settings rather than stopping the daemon.
    fn reload_if_changed(&mut self, path: Option<&Path>) {
        let modified = config::modified(path);
        if modified == self.modified {
            return;
        }
        self.modified = modified;
        if let Ok(config) = config::load(path) {
            self.config = config;
        }
    }
}

fn done(stdout: String) -> Reply {
    Reply::Done {
        stdout,
        stderr: String::new(),
        exit_code: 0,
        logs: String::new(),
    }
}

/// Asks the daemon to apply its idle policy every
/// [`MAINTENANCE_INTERVAL`], by sending itself a request, so the accept
/// loop needs no timeout.
fn send_maintenance_ticks(socket: PathBuf) {
    thread::spawn(move || {
        loop {
            thread::sleep(MAINTENANCE_INTERVAL);
            let Ok(mut stream) = UnixStream::connect(&socket) else {
                return;
            };
            if send(&mut stream, &Request::Maintain).is_err() {
                return;
            }
            let _ = receive::<Reply>(&mut stream);
        }
    });
}

/// How often the daemon looks for changes to get ready for while a
/// workspace is being kept ready for graphs, and how often it looks
/// whether one is when none was.
const PREPARE_INTERVAL: Duration = Duration::from_millis(250);
const PREPARE_IDLE_INTERVAL: Duration = Duration::from_secs(2);

/// Asks the daemon, by sending itself a request as the maintenance timer
/// does, to get ready for the next graph through each workspace whose
/// files changed: often while one is being kept ready, seldom otherwise.
fn send_prepare_ticks(socket: PathBuf) {
    thread::spawn(move || {
        let mut interval = PREPARE_IDLE_INTERVAL;
        loop {
            thread::sleep(interval);
            let Ok(mut stream) = UnixStream::connect(&socket) else {
                return;
            };
            if send(&mut stream, &Request::Prepare).is_err() {
                return;
            }
            interval = match receive::<Reply>(&mut stream) {
                Ok(Reply::Prepared {
                    is_any_kept_ready: true,
                }) => PREPARE_INTERVAL,
                _ => PREPARE_IDLE_INTERVAL,
            };
        }
    });
}

/// Runs one forwarded command the way `main` would, capturing its output
/// and, at the level it asks for, its log lines, which go before its own
/// error output.
fn run(
    engine: &mut Engine,
    args: Vec<String>,
    invocation: &Invocation,
    logger: &crate::logging::Logger,
) -> Reply {
    let mut stdout = Vec::new();
    let arguments = std::iter::once("lemma".to_owned()).chain(args);
    let (stderr, exit_code, logs) = match Cli::try_parse_from(arguments) {
        Err(error) => (
            error.render().to_string(),
            u8::try_from(error.exit_code()).unwrap_or(1),
            String::new(),
        ),
        Ok(cli) => {
            logger.start_capture(cli.logging);
            log::info!("running in the daemon (pid {})", std::process::id());
            let result = engine.execute(cli, invocation, &mut stdout);
            let logs = logger.finish_capture(None);
            match result {
                Ok(code) => (String::new(), code, logs),
                Err(error) => (format!("error: {error:#}\n"), 1, logs),
            }
        }
    };
    Reply::Done {
        stdout: String::from_utf8_lossy(&stdout).into_owned(),
        stderr,
        exit_code,
        logs,
    }
}

/// Sends a request and reads the reply, starting a daemon if none answers.
fn exchange(socket: &Path, request: &Request) -> anyhow::Result<Reply> {
    let mut stream = connect_or_start(socket)?;
    send(&mut stream, request)?;
    receive(&mut stream)
}

fn connect_or_start(socket: &Path) -> anyhow::Result<UnixStream> {
    if let Ok(stream) = UnixStream::connect(socket) {
        return Ok(stream);
    }
    log::info!("no daemon is running; starting one");
    start_daemon()?;
    let deadline = Instant::now() + STARTUP_LIMIT;
    loop {
        if let Ok(stream) = UnixStream::connect(socket) {
            return Ok(stream);
        }
        if Instant::now() >= deadline {
            bail!("the lemma daemon did not start");
        }
        thread::sleep(CONNECT_RETRY);
    }
}

/// The daemon is never waited for: it outlives this command on purpose.
/// An installed service starts it; otherwise it is started here, with the
/// environment the service would give it.
fn start_daemon() -> anyhow::Result<()> {
    // The login service runs the daemon of the usual socket, not of one
    // `LEMMA_DAEMON_SOCKET` names.
    if std::env::var_os("LEMMA_DAEMON_SOCKET").is_none() {
        match crate::service::start() {
            Ok(true) => return Ok(()),
            Ok(false) => {}
            Err(error) => {
                log::info!("could not start the daemon's service, so starting it here: {error:#}");
            }
        }
    }
    let executable = std::env::current_exe().context("could not find the lemma executable")?;
    Command::new(executable)
        .arg("daemon")
        .env_clear()
        .envs(daemon_environment(std::env::vars_os()))
        .current_dir("/")
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        // Its own process group, so Ctrl-C in the terminal that started it
        // does not stop it along with the command.
        .process_group(0)
        .spawn()
        .context("could not start the lemma daemon")?;
    Ok(())
}

/// The variables a daemon keeps from the command that starts it.
///
/// The daemon outlives that command and serves every later one, so it must
/// not keep what one shell set for itself: a sandbox's `GOCACHE`, a
/// temporary `XDG_CACHE_HOME` or `TMPDIR` would redirect every later build
/// and cache, and break them once removed. Kept are who the user is, where
/// programs are, the locale, how to reach the network and Lemma's own
/// settings.
fn daemon_environment(
    variables: impl IntoIterator<Item = (OsString, OsString)>,
) -> Vec<(OsString, OsString)> {
    const KEPT: [&str; 13] = [
        "HOME",
        "USER",
        "LOGNAME",
        "PATH",
        "SHELL",
        "LANG",
        "LANGUAGE",
        "XDG_CONFIG_HOME",
        "XDG_RUNTIME_DIR",
        "SSH_AUTH_SOCK",
        "HTTP_PROXY",
        "HTTPS_PROXY",
        "NO_PROXY",
    ];
    let mut kept: Vec<(OsString, OsString)> = variables
        .into_iter()
        .filter(|(name, _)| {
            let Some(name) = name.to_str() else {
                return false;
            };
            let upper = name.to_ascii_uppercase();
            KEPT.contains(&upper.as_str()) || name.starts_with("LC_") || name.starts_with("LEMMA_")
        })
        .collect();
    kept.sort();
    kept
}

/// Listens on the socket, or returns `None` if a live daemon already does.
/// A socket file left by a daemon that crashed is replaced.
fn bind(socket: &Path) -> anyhow::Result<Option<UnixListener>> {
    if UnixStream::connect(socket).is_ok() {
        return Ok(None);
    }
    let _ = fs::remove_file(socket);
    let listener = UnixListener::bind(socket)
        .with_context(|| format!("could not listen on {}", socket.display()))?;
    // Only this user may send commands.
    fs::set_permissions(socket, fs::Permissions::from_mode(0o600))?;
    Ok(Some(listener))
}

/// Asks the daemon to stop once it has been idle for `idle_limit`, by sending
/// itself a shutdown request, so the accept loop needs no timeout.
fn watch_for_idleness(socket: PathBuf, last_request: Arc<Mutex<Instant>>, idle_limit: Duration) {
    thread::spawn(move || {
        loop {
            let idle_for = lock(&last_request).elapsed();
            if idle_for >= idle_limit {
                break;
            }
            thread::sleep(idle_limit.saturating_sub(idle_for));
        }
        if let Ok(mut stream) = UnixStream::connect(&socket) {
            let _ = send(&mut stream, &Request::Shutdown);
            let _ = receive::<Reply>(&mut stream);
        }
    });
}

/// Waits, up to [`STOP_LIMIT`], until the daemon of `socket` has exited:
/// its lock is free.
fn wait_until_gone(socket: &Path) -> anyhow::Result<()> {
    let deadline = Instant::now() + STOP_LIMIT;
    while try_lock(socket)?.is_none() && Instant::now() < deadline {
        thread::sleep(CONNECT_RETRY);
    }
    Ok(())
}

/// The lock a daemon holds for as long as it runs: the system lets go of it
/// when the daemon's process ends, however it ends, so a free lock means no
/// daemon of `socket` runs or is still stopping.
fn lock_path(socket: &Path) -> PathBuf {
    socket.with_extension("lock")
}

/// The daemon's lock if it is free; `None` while a daemon holds it.
fn try_lock(socket: &Path) -> anyhow::Result<Option<fs::File>> {
    let path = lock_path(socket);
    let file = fs::OpenOptions::new()
        .create(true)
        .truncate(false)
        .write(true)
        .open(&path)
        .with_context(|| format!("could not open {}", path.display()))?;
    match rustix::fs::flock(&file, rustix::fs::FlockOperation::NonBlockingLockExclusive) {
        Ok(()) => Ok(Some(file)),
        Err(error) if error == rustix::io::Errno::WOULDBLOCK => Ok(None),
        Err(error) => Err(error).with_context(|| format!("could not lock {}", path.display())),
    }
}

/// The daemon's lock for a daemon about to start, waiting, up to
/// [`STOP_LIMIT`], for one that is stopping to finish; `None` when another
/// daemon serves or is starting.
fn take_lock(socket: &Path) -> anyhow::Result<Option<fs::File>> {
    let deadline = Instant::now() + STOP_LIMIT;
    loop {
        if let Some(lock) = try_lock(socket)? {
            return Ok(Some(lock));
        }
        // Held by a daemon that answers, or will once it is listening.
        let is_serving = UnixStream::connect(socket).is_ok();
        if is_serving || Instant::now() >= deadline {
            return Ok(None);
        }
        thread::sleep(CONNECT_RETRY);
    }
}

fn send(stream: &mut UnixStream, message: &impl Serialize) -> anyhow::Result<()> {
    let mut line = serde_json::to_vec(message)?;
    line.push(b'\n');
    stream.write_all(&line)?;
    Ok(())
}

fn receive<T: for<'de> Deserialize<'de>>(stream: &mut UnixStream) -> anyhow::Result<T> {
    let mut line = String::new();
    BufReader::new(stream).read_line(&mut line)?;
    if line.is_empty() {
        bail!("the lemma daemon closed the connection");
    }
    Ok(serde_json::from_str(&line)?)
}

/// One socket per user, in the temporary directory because socket paths are
/// limited to about 100 bytes. The user id keeps users apart on systems that
/// share one temporary directory.
fn socket_path() -> anyhow::Result<PathBuf> {
    // A separate daemon, such as one per benchmark run; kept in the
    // daemon's own environment, as every `LEMMA_` variable is.
    if let Some(socket) = std::env::var_os("LEMMA_DAEMON_SOCKET") {
        return Ok(PathBuf::from(socket));
    }
    let home = std::env::var_os("HOME").context("HOME is not set")?;
    let user = fs::metadata(&home)
        .with_context(|| format!("could not read {}", Path::new(&home).display()))?
        .uid();
    Ok(std::env::temp_dir().join(format!("lemma-{user}.sock")))
}

/// The running binary's path and modification time. Rebuilding or upgrading
/// Lemma changes it, which retires daemons started by the old binary.
fn build_id() -> String {
    let executable = std::env::current_exe().unwrap_or_default();
    let modified = fs::metadata(&executable)
        .and_then(|metadata| metadata.modified())
        .ok();
    format!(
        "{} {} {modified:?}",
        env!("CARGO_PKG_VERSION"),
        executable.display()
    )
}

fn lock<T>(mutex: &Mutex<T>) -> std::sync::MutexGuard<'_, T> {
    mutex.lock().unwrap_or_else(PoisonError::into_inner)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_daemon_keeps_who_and_where_but_not_a_shells_caches() {
        let variables = [
            ("PATH", "/usr/bin"),
            ("HOME", "/home/me"),
            ("LEMMA_CONFIG", "/etc/lemma.toml"),
            ("https_proxy", "http://proxy"),
            ("LC_ALL", "C.UTF-8"),
            ("GOCACHE", "/tmp/sandbox/go"),
            ("XDG_CACHE_HOME", "/tmp/sandbox/cache"),
            ("TMPDIR", "/tmp/sandbox"),
        ]
        .map(|(name, value)| (OsString::from(name), OsString::from(value)));

        let kept: Vec<String> = daemon_environment(variables)
            .into_iter()
            .map(|(name, _)| name.to_string_lossy().into_owned())
            .collect();

        assert_eq!(
            kept,
            ["HOME", "LC_ALL", "LEMMA_CONFIG", "PATH", "https_proxy"]
        );
    }

    #[test]
    fn requests_round_trip_as_one_json_line() {
        let request = Request::Run {
            build: "1.0".to_owned(),
            args: vec!["refs".to_owned(), "#7".to_owned()],
            cwd: PathBuf::from("/repo"),
            path_env: Some(b"/usr/bin:/bin".to_vec()),
        };

        let line = serde_json::to_string(&request).unwrap();
        let parsed: Request = serde_json::from_str(&line).unwrap();

        assert!(!line.contains('\n'));
        let Request::Run { args, path_env, .. } = parsed else {
            panic!("expected a run request, got {line}");
        };
        assert_eq!(args, ["refs", "#7"]);
        assert_eq!(path_env.as_deref(), Some(&b"/usr/bin:/bin"[..]));
    }

    #[test]
    fn parse_errors_are_reported_like_the_command_line() {
        let mut engine = Engine::one_shot();
        let invocation = Invocation {
            cwd: PathBuf::from("/"),
            path_env: None,
        };
        let logger = crate::logging::Logger::new();

        let reply = run(
            &mut engine,
            vec!["no-such-command".to_owned()],
            &invocation,
            &logger,
        );

        let Reply::Done {
            stderr, exit_code, ..
        } = reply
        else {
            panic!("expected a finished command");
        };
        assert_eq!(exit_code, 2);
        assert!(stderr.contains("no-such-command"));
    }

    #[test]
    fn a_daemons_lock_is_held_until_it_lets_go() {
        let directory = tempfile::tempdir().unwrap();
        let socket = directory.path().join("d.sock");

        let daemons = try_lock(&socket).unwrap();
        let another_meanwhile = try_lock(&socket).unwrap();
        drop(daemons);
        let another_after = try_lock(&socket).unwrap();

        assert!(another_meanwhile.is_none());
        assert!(another_after.is_some());
    }

    #[test]
    fn waiting_for_a_daemon_ends_once_its_lock_is_free() {
        let directory = tempfile::tempdir().unwrap();
        let socket = directory.path().join("d.sock");
        let daemons = try_lock(&socket).unwrap().unwrap();
        let path = socket.clone();
        let stop = thread::spawn(move || wait_until_gone(&path).unwrap());

        drop(daemons);
        stop.join().unwrap();

        assert!(try_lock(&socket).unwrap().is_some());
    }

    #[test]
    fn the_socket_path_is_short_enough_for_unix_sockets() {
        assert!(socket_path().unwrap().as_os_str().len() < 104);
    }
}
