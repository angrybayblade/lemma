//! A background process that runs Lemma's commands with warm state.
//!
//! Every command spends most of its time on things a long-lived process can
//! do once: opening the database, and starting language servers that take
//! seconds to load a large project. So `lemma` forwards its arguments,
//! working directory and `PATH` over a Unix socket to the daemon of the
//! workspace the command is about, and prints what comes back
//! (`.specs/21-workspace-daemons.md`). The first command in a workspace
//! starts its daemon. It exits after a period without commands, stopping
//! its servers.

use std::collections::BTreeMap;
use std::ffi::OsString;
use std::fmt::Write as _;
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
use rustix::event::{PollFd, PollFlags, Timespec, poll};

use crate::anchors::UsesRequest;
use crate::cli::Cli;
use crate::config::{self, Autostart, Config};
use crate::engine::{Engine, Invocation};
use crate::sentry::Sentry;
use crate::workspace::{Workspace, named_repositories};

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
        /// The directory the command is about, which the daemon must serve.
        place: PathBuf,
        /// The caller's `PATH`, as raw bytes.
        path_env: Option<Vec<u8>>,
    },
    /// Whether this daemon serves a directory; asked by a client walking
    /// up from it to find its daemon.
    Serves {
        place: PathBuf,
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
        /// The directory of the module the check starts from.
        place: PathBuf,
        /// The asking process's environment, for the commands a check runs.
        environment: BTreeMap<String, String>,
        /// A journal token whose changes the ask is about, from a client
        /// that follows the journal: the changes up to it were collected,
        /// so sentry need not wait for its watchers first.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        as_of: Option<String>,
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

impl Request {
    /// The directory a request is about, when it is about one: a daemon
    /// answers it only if it serves that directory.
    fn place(&self) -> Option<&Path> {
        match self {
            Self::Run { place, .. } | Self::Serves { place } | Self::SentryAsk { place, .. } => {
                Some(place)
            }
            Self::Uses { question, .. } => Some(&question.root),
            Self::Sentry {
                action: SentryAction::Start { manifest },
                ..
            } => manifest.parent(),
            Self::Sentry { .. }
            | Self::Maintain
            | Self::Prepare
            | Self::Status
            | Self::Shutdown => None,
        }
    }
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
    /// The daemon serves the directory asked about.
    Served,
    /// The directory asked about is not in the daemon's workspace: another
    /// daemon serves it, or none does yet.
    Outside,
}

/// Who asks a daemon, which decides whether one is started for it when none
/// serves its directory (`[daemon] autostart`).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Asker {
    /// A command run from a shell, which can do its work itself.
    Command,
    /// The MCP server or the editor's server, which stays running.
    Server,
}

/// Whether `asker` may start a daemon, by the `autostart` setting.
fn may_start(autostart: Autostart, asker: Asker) -> bool {
    match autostart {
        Autostart::Always => true,
        Autostart::Clients => asker == Asker::Server,
        Autostart::Never => false,
    }
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

/// Runs the command in the daemon, starting one if none serves it and
/// `asker` may. `None` means no daemon serves it or could be reached, and
/// the caller should run the command itself.
pub(crate) fn forward(args: Vec<String>, cwd: PathBuf, asker: Asker) -> Option<Finished> {
    let place = canonical(&place_of(&args, &cwd));
    let starts = may_start(config::autostart(), asker);
    let daemon = daemon_to_ask(&place, starts)?;
    let request = Request::Run {
        build: build_id(),
        args,
        cwd,
        place,
        path_env: std::env::var_os("PATH").map(OsString::into_vec),
    };
    log::debug!("forwarding to the daemon at {}", daemon.socket.display());
    let mut reply = exchange(&daemon, &request, starts).ok()?;
    if matches!(reply, Reply::Restart) {
        // A daemon was wanted here, whoever started it: it is replaced.
        log::info!("the running daemon is another build; replacing it");
        wait_until_gone(&daemon.socket).ok()?;
        reply = exchange(&daemon, &request, true).ok()?;
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
        Reply::Outside => {
            log::warn!(
                "the daemon found does not serve {}",
                daemon.socket.display()
            );
            None
        }
        Reply::Uses { .. }
        | Reply::Restart
        | Reply::Stopping
        | Reply::NotServed
        | Reply::Accepted
        | Reply::Served
        | Reply::Prepared { .. } => None,
    }
}

/// Where a symbol is used, answered by the running daemon. `None` means no
/// daemon of this build is running; none is started, since the caller can
/// start its own servers as quickly.
pub(crate) fn uses(question: &UsesRequest) -> Option<UsesAnswer> {
    let daemon = serving(&canonical(&question.root))?;
    let mut stream = UnixStream::connect(&daemon.socket).ok()?;
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
        | Reply::Served
        | Reply::Outside
        | Reply::Prepared { .. } => None,
    }
}

/// What each daemon asked reports: the one serving `place`, or with `all`
/// every daemon of this user. Empty when none runs.
pub(crate) fn status(place: &Path, all: bool) -> anyhow::Result<Vec<String>> {
    let mut reports = Vec::new();
    for socket in running(place, all)? {
        let Ok(mut stream) = UnixStream::connect(&socket) else {
            continue;
        };
        send(&mut stream, &Request::Status)?;
        if let Reply::Done { stdout, .. } = receive::<Reply>(&mut stream)? {
            reports.push(stdout);
        }
    }
    Ok(reports)
}

/// Asks the daemon serving `place` to exit, or with `all` every daemon of
/// this user, and waits for each. How many were running.
pub(crate) fn stop(place: &Path, all: bool) -> anyhow::Result<usize> {
    let mut stopped = 0;
    for socket in running(place, all)? {
        let Ok(mut stream) = UnixStream::connect(&socket) else {
            continue;
        };
        send(&mut stream, &Request::Shutdown)?;
        receive::<Reply>(&mut stream)?;
        wait_until_gone(&socket)?;
        stopped += 1;
    }
    Ok(stopped)
}

/// Starts the daemon of the workspace `place` belongs to, in the
/// background, unless one already serves `place`. The workspace's root
/// (none for the daemon `LEMMA_DAEMON_SOCKET` names), and whether a daemon
/// was already serving.
pub(crate) fn start(place: &Path) -> anyhow::Result<(Option<PathBuf>, bool)> {
    let place = canonical(place);
    if let Some(daemon) = serving(&place).filter(Daemon::answers) {
        return Ok((daemon.root, true));
    }
    let daemon = daemon_for(&place)?;
    connect_or_start(&daemon)?;
    Ok((daemon.root, false))
}

/// Serves the workspace rooted at `root`, or else the one the current
/// directory belongs to, until idle for the configured time or
/// `idle_override`. The config file is read again whenever it changes.
pub(crate) fn serve(
    root: Option<&Path>,
    idle_override: Option<Duration>,
    logger: &'static crate::logging::Logger,
) -> anyhow::Result<()> {
    let (socket, mut serving) = Serving::open(root)?;
    let Some(lock) = take_lock(&socket)? else {
        // Another daemon serves, or won the race to start.
        return Ok(());
    };
    let Some(listener) = bind(&socket)? else {
        return Ok(());
    };
    let mut settings = Settings::load(config::config_path().as_deref())?;
    let mut engine = engine_with(&settings.config, &listener);
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
        if let Some(reply) = reply_about_place(&request, &mut serving) {
            let _ = send(&mut stream, &reply);
            continue;
        }
        let Some((request, mut stream)) = route_sentry(request, stream, &mut sentry, &settings)
        else {
            continue;
        };
        let is_status = matches!(request, Request::Status);
        let is_maintenance = matches!(request, Request::Maintain);
        if is_maintenance {
            maintain_runtimes(&runtimes, &mut engine, &settings.config);
        }
        let mut reply = reply_to(request, &mut engine, &mut settings, &last_request, logger);
        if let (true, Reply::Done { stdout, .. }) = (is_status, &mut reply) {
            stdout.insert_str(0, &serving.describe());
            stdout.push_str(&runtimes.status());
            stdout.push_str(&sentry.as_ref().map(Sentry::status).unwrap_or_default());
        }
        let is_exiting = matches!(reply, Reply::Stopping | Reply::Restart);
        // A caller that gave up is not a reason to stop serving others.
        let _ = send(&mut stream, &reply);
        let is_cold = is_maintenance
            && sentry.is_none()
            && is_cold(&engine, &runtimes, &last_request, &settings.config);
        if is_exiting || is_cold {
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

/// Whether, sentry aside, the daemon holds nothing warm and has been idle
/// long enough to exit by itself.
fn is_cold(
    engine: &Engine,
    runtimes: &crate::runtime::Runtimes,
    last_request: &Mutex<Instant>,
    config: &Config,
) -> bool {
    let holds_nothing = runtimes.status().is_empty() && engine.holds_nothing_warm();
    let idle_for = lock(last_request).elapsed();
    has_nothing_to_stay_for(holds_nothing, idle_for, config.idle_policy.stop_after)
}

/// Whether a daemon exits by itself: it holds nothing warm, and no command
/// has come for as long as an idle language server is kept before it is
/// stopped. A workspace nobody is using then has no daemon. With
/// `stop_after` unset servers are kept, and so is the daemon.
fn has_nothing_to_stay_for(
    is_cold: bool,
    idle_for: Duration,
    stop_after: Option<Duration>,
) -> bool {
    is_cold && stop_after.is_some_and(|kept| idle_for >= kept)
}

/// The daemon's engine with `config`'s settings, which gives way while a
/// caller waits on `listener`.
fn engine_with(config: &Config, listener: &UnixListener) -> Engine {
    let mut engine = Engine::long_lived();
    engine.set_graph_watches(config.graph_watches);
    engine.set_graphs_kept_ready(config.graphs_kept_ready);
    engine.set_when_slow(config.when_slow);
    if let Ok(waited_on) = listener.try_clone() {
        engine.give_way_when(Arc::new(move || has_caller_waiting(&waited_on)));
    }
    engine
}

/// The reply to a request about a directory `serving` does not serve, or
/// to one asking only whether it does; `None` for a request to go on with.
fn reply_about_place(request: &Request, serving: &mut Serving) -> Option<Reply> {
    let is_served = serving.serves(request.place()?);
    if matches!(request, Request::Serves { .. }) && is_served {
        return Some(Reply::Served);
    }
    (!is_served).then_some(Reply::Outside)
}

/// Whether a caller has connected to `listener` and not been accepted
/// yet: a command, or one of the daemon's own timers, waiting for what is
/// being served to finish. False when the system cannot say.
fn has_caller_waiting(listener: &UnixListener) -> bool {
    let mut waited_on = [PollFd::new(listener, PollFlags::IN)];
    let at_once = Timespec {
        tv_sec: 0,
        tv_nsec: 0,
    };
    matches!(poll(&mut waited_on, Some(&at_once)), Ok(ready) if ready > 0)
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
    if allowance.memory_bytes == 0 {
        return None;
    }
    let socket = crate::runtime::socket_beside(&serving(&canonical(directory))?.socket);
    if !socket.exists() {
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
            as_of,
            place: _,
        } => {
            let asked = Asked {
                build,
                args,
                cwd,
                environment,
                as_of,
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
    as_of: Option<String>,
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
            as_of: asked.as_of,
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
            let canonical = manifest.canonicalize().unwrap_or_else(|_| manifest.clone());
            if let Some(attached) = sentry
                .as_ref()
                .filter(|attached| attached.manifest() == canonical)
            {
                return done(format!(
                    "sentry (experimental) is attached to {}\n",
                    attached.manifest().display()
                ));
            }
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
    ask_sentry_as_of(args, cwd, environment, None)
}

/// [`ask_sentry`] for a client that follows sentry's journal, about the
/// changes up to the journal token `as_of`.
pub(crate) fn ask_sentry_as_of(
    args: Vec<String>,
    cwd: PathBuf,
    environment: BTreeMap<String, String>,
    as_of: Option<String>,
) -> Option<crate::sentry::Answer> {
    let place = canonical(&place_of(&args, &cwd));
    let daemon = serving(&place)?;
    let mut stream = UnixStream::connect(&daemon.socket).ok()?;
    let request = Request::SentryAsk {
        build: build_id(),
        args,
        cwd,
        place,
        environment,
        as_of,
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

/// Attaches the daemon's sentry to the workspace whose manifest is
/// `manifest` (absolute), starting a daemon, or replacing one of another
/// build, if needed. For `lemma lsp`, whose editor setting is the person's
/// opt-in; `lemma check` never starts a daemon for sentry. Whether sentry
/// serves the workspace: not when it is attached to another.
pub(crate) fn attach_sentry(manifest: &Path) -> anyhow::Result<bool> {
    let manifest = &canonical(manifest);
    let starts = may_start(config::autostart(), Asker::Server);
    let workspace = manifest.parent().unwrap_or(manifest);
    let Some(daemon) = daemon_to_ask(workspace, starts) else {
        return Ok(false);
    };
    let request = Request::Sentry {
        build: build_id(),
        action: SentryAction::Start {
            manifest: manifest.clone(),
        },
    };
    let mut reply = exchange(&daemon, &request, starts)?;
    if matches!(reply, Reply::Restart) {
        log::info!("the running daemon is another build; replacing it");
        wait_until_gone(&daemon.socket)?;
        reply = exchange(&daemon, &request, true)?;
    }
    match reply {
        Reply::Done { exit_code: 0, .. } => Ok(true),
        Reply::Done { stderr, .. } => {
            log::info!(
                "sentry does not serve {}: {}",
                manifest.display(),
                stderr.trim()
            );
            Ok(false)
        }
        _ => Ok(false),
    }
}

/// Sends `action` to the sentry of the daemon serving `place`; `None` when
/// no daemon serves it, since sentry only attaches to one already running.
pub(crate) fn sentry_control(
    place: &Path,
    action: SentryAction,
) -> anyhow::Result<Option<(String, String, u8)>> {
    let Some(daemon) = serving(&canonical(place)) else {
        return Ok(None);
    };
    let Ok(mut stream) = UnixStream::connect(&daemon.socket) else {
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
        Request::SentryAsk { .. } | Request::Sentry { .. } | Request::Serves { .. } => {
            Reply::NotServed
        }
        Request::Maintain => {
            settings.reload_if_changed(config::config_path().as_deref());
            let config = &settings.config;
            engine.set_graph_watches(config.graph_watches);
            engine.set_graphs_kept_ready(config.graphs_kept_ready);
            engine.set_when_slow(config.when_slow);
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
fn exchange(daemon: &Daemon, request: &Request, starts: bool) -> anyhow::Result<Reply> {
    let mut stream = if starts {
        connect_or_start(daemon)?
    } else {
        UnixStream::connect(&daemon.socket)?
    };
    send(&mut stream, request)?;
    receive(&mut stream)
}

fn connect_or_start(daemon: &Daemon) -> anyhow::Result<UnixStream> {
    let socket = &daemon.socket;
    if let Ok(stream) = UnixStream::connect(socket) {
        return Ok(stream);
    }
    log::info!("no daemon is running; starting one");
    start_daemon(daemon.root.as_deref())?;
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

/// Starts a daemon for the workspace rooted at `root`, or the one
/// `LEMMA_DAEMON_SOCKET` names when there is no root. The daemon is never
/// waited for: it outlives this command on purpose.
fn start_daemon(root: Option<&Path>) -> anyhow::Result<()> {
    let executable = std::env::current_exe().context("could not find the lemma executable")?;
    let mut command = Command::new(executable);
    command.arg("daemon");
    if let Some(root) = root {
        command.arg("--root").arg(root);
    }
    command
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

/// The socket `LEMMA_DAEMON_SOCKET` names: one daemon for every directory,
/// such as one per benchmark run. It is kept in the daemon's own
/// environment, as every `LEMMA_` variable is.
fn named_socket() -> Option<PathBuf> {
    std::env::var_os("LEMMA_DAEMON_SOCKET").map(PathBuf::from)
}

/// Where this user's daemons listen, one socket for each workspace: the
/// user's runtime directory, or else a directory in `/tmp` named by the user
/// id. Not the temporary directory `TMPDIR` names: a daemon does not keep
/// the `TMPDIR` of the command that started it, and both must look in the
/// same place. Socket paths are limited to about 100 bytes, which rules out
/// the workspace's own directory.
fn sockets_directory() -> anyhow::Result<PathBuf> {
    let runtime = std::env::var_os("XDG_RUNTIME_DIR")
        .map(|runtime| PathBuf::from(runtime).join("lemma"))
        .filter(|directory| directory.is_absolute() && fits_a_socket(directory));
    if let Some(directory) = runtime {
        return Ok(directory);
    }
    let home = std::env::var_os("HOME").context("HOME is not set")?;
    let user = fs::metadata(&home)
        .with_context(|| format!("could not read {}", Path::new(&home).display()))?
        .uid();
    Ok(Path::new("/tmp").join(format!("lemma-{user}")))
}

/// Whether a socket in `directory` has a path the system accepts: about
/// 100 bytes at most, with the socket's name and what is put beside it.
fn fits_a_socket(directory: &Path) -> bool {
    const LONGEST_PATH: usize = 100;
    const LONGEST_NAME: usize = "/0123456789abcdef.runtime.sock".len();
    directory.as_os_str().len() + LONGEST_NAME <= LONGEST_PATH
}

/// The socket of the daemon serving the workspace rooted at `root`, named
/// by a digest of the root so that any client can work it out.
fn socket_of(root: &Path) -> anyhow::Result<PathBuf> {
    let digest = blake3::hash(root.as_os_str().as_encoded_bytes()).to_hex();
    Ok(sockets_directory()?.join(format!("{}.sock", &digest[..16])))
}

/// A daemon to send a request to: its socket, and the root of the
/// workspace to start it for when it is not running. The one
/// `LEMMA_DAEMON_SOCKET` names has no root: it serves every directory.
struct Daemon {
    socket: PathBuf,
    root: Option<PathBuf>,
}

impl Daemon {
    /// Whether it is running.
    fn answers(&self) -> bool {
        UnixStream::connect(&self.socket).is_ok()
    }
}

/// The daemon that serves `place` (canonical): the one
/// `LEMMA_DAEMON_SOCKET` names, running or not, or else the nearest one
/// running at or above `place` that says `place` is its to answer about.
fn serving(place: &Path) -> Option<Daemon> {
    if let Some(socket) = named_socket() {
        return Some(Daemon { socket, root: None });
    }
    place.ancestors().find_map(|above| {
        let socket = socket_of(above).ok()?;
        let mut stream = UnixStream::connect(&socket).ok()?;
        let asked = Request::Serves {
            place: place.to_path_buf(),
        };
        send(&mut stream, &asked).ok()?;
        matches!(receive::<Reply>(&mut stream), Ok(Reply::Served)).then(|| Daemon {
            socket,
            root: Some(above.to_path_buf()),
        })
    })
}

/// The daemon to ask about `place` (canonical): the one serving it, or else
/// the one to start, at the root of the workspace `place` belongs to and
/// not at `place` itself, so that it does not matter where in a workspace
/// its first command ran.
fn daemon_for(place: &Path) -> anyhow::Result<Daemon> {
    if let Some(daemon) = serving(place) {
        return Ok(daemon);
    }
    let root = Workspace::of(place, &named_repositories)
        .root()
        .to_path_buf();
    Ok(Daemon {
        socket: socket_of(&root)?,
        root: Some(root),
    })
}

/// The daemon to ask about `place` (canonical): the one serving it, or,
/// when one may be started (`starts`), the one that would be.
fn daemon_to_ask(place: &Path, starts: bool) -> Option<Daemon> {
    if starts {
        return daemon_for(place).ok();
    }
    serving(place)
}

/// The sockets of the daemons a `status` or `stop` is for: the one serving
/// `place`, or with `all` every one of this user's.
fn running(place: &Path, all: bool) -> anyhow::Result<Vec<PathBuf>> {
    if !all || named_socket().is_some() {
        return Ok(serving(&canonical(place))
            .map(|daemon| daemon.socket)
            .into_iter()
            .collect());
    }
    let Ok(entries) = fs::read_dir(sockets_directory()?) else {
        return Ok(Vec::new());
    };
    let mut sockets: Vec<PathBuf> = entries
        .filter_map(Result::ok)
        .map(|entry| entry.path())
        .filter(|path| is_daemon_socket(path))
        .collect();
    sockets.sort();
    Ok(sockets)
}

/// Whether a file in the sockets' directory is a daemon's own socket, not
/// the one its warm runtimes listen on beside it.
fn is_daemon_socket(path: &Path) -> bool {
    let is_socket = path
        .extension()
        .is_some_and(|extension| extension == "sock");
    let stem = path.file_stem().and_then(|stem| stem.to_str());
    is_socket && !stem.is_some_and(|stem| stem.ends_with(".runtime"))
}

/// `path` with its symbolic links resolved, or as given when it cannot be
/// read: workspaces are compared and named by canonical paths.
fn canonical(path: &Path) -> PathBuf {
    path.canonicalize().unwrap_or_else(|_| path.to_path_buf())
}

/// The directory a command is about, given its arguments and where it was
/// run: what `index` or `daemon lsp` is pointed at, the directory of the
/// module a `check` starts from, or else where it was run. A command is
/// served by the daemon of that directory's workspace.
fn place_of(args: &[String], cwd: &Path) -> PathBuf {
    use crate::cli::{Command as Asked, DaemonAction, LspAction};
    let arguments = std::iter::once("lemma".to_owned()).chain(args.iter().cloned());
    let Ok(cli) = Cli::try_parse_from(arguments) else {
        return cwd.to_path_buf();
    };
    let named = match cli.command {
        Asked::Index { path, .. } => Some(path),
        Asked::Check(check) => Some(check.manifest),
        Asked::Daemon(daemon) => match daemon.action {
            Some(DaemonAction::Lsp(servers)) => match servers.action {
                Some(LspAction::List(target) | LspAction::Stop(target)) => target.path,
                Some(LspAction::Start(start)) => start.target.path,
                None => servers.target.path,
            },
            _ => None,
        },
        _ => None,
    };
    let Some(named) = named.map(|path| cwd.join(path)) else {
        return cwd.to_path_buf();
    };
    if named.is_dir() {
        return named;
    }
    named.parent().map_or(named.clone(), Path::to_path_buf)
}

/// What a daemon answers about.
enum Serving {
    /// Every directory: the daemon `LEMMA_DAEMON_SOCKET` names, as a gate
    /// or a benchmark runs one.
    Everything,
    /// One workspace, and when its root module was last read for the
    /// repositories it names.
    Workspace(Workspace, Option<SystemTime>),
}

impl Serving {
    /// What a daemon started for `root` serves, and the socket it listens
    /// on; without a root, the workspace the current directory belongs to.
    fn open(root: Option<&Path>) -> anyhow::Result<(PathBuf, Self)> {
        if let Some(socket) = named_socket() {
            return Ok((socket, Self::Everything));
        }
        let workspace = if let Some(root) = root {
            Workspace::rooted_at(canonical(root), &named_repositories)
        } else {
            let cwd = std::env::current_dir().context("could not read the current directory")?;
            Workspace::of(&canonical(&cwd), &named_repositories)
        };
        let directory = sockets_directory()?;
        fs::create_dir_all(&directory)
            .with_context(|| format!("could not create {}", directory.display()))?;
        // Only this user may reach the daemons.
        fs::set_permissions(&directory, fs::Permissions::from_mode(0o700))?;
        let socket = socket_of(workspace.root())?;
        let read = root_module_modified(&workspace);
        Ok((socket, Self::Workspace(workspace, read)))
    }

    /// Whether `place` is this daemon's to answer about. The root module is
    /// read again first if it changed, since it may name a repository it
    /// did not when the daemon started.
    fn serves(&mut self, place: &Path) -> bool {
        let Self::Workspace(workspace, read) = self else {
            return true;
        };
        if workspace.serves(place) {
            return true;
        }
        let modified = root_module_modified(workspace);
        if modified == *read {
            return false;
        }
        *workspace = Workspace::rooted_at(workspace.root().to_path_buf(), &named_repositories);
        *read = modified;
        workspace.serves(place)
    }

    /// The lines of `lemma daemon status` saying what is served, with a
    /// warning for each repository the root module names outside the root:
    /// commands run there do not find this daemon.
    fn describe(&self) -> String {
        let Self::Workspace(workspace, _) = self else {
            return "serving every directory (LEMMA_DAEMON_SOCKET)\n".to_owned();
        };
        let root = workspace.root();
        let mut lines = format!("workspace: {}\n", root.display());
        for outside in workspace.members_outside() {
            let _ = writeln!(
                lines,
                "warning: {} is outside this workspace's root. Commands run there will not \
                 find this daemon: start Lemma from a directory holding both, or move it \
                 under {}.",
                outside.display(),
                root.display()
            );
        }
        lines
    }
}

/// When a workspace's root module last changed, if it has one.
fn root_module_modified(workspace: &Workspace) -> Option<SystemTime> {
    let manifest = workspace.root().join(crate::workspace::ROOT_MODULE);
    fs::metadata(manifest)
        .and_then(|metadata| metadata.modified())
        .ok()
}

/// The running binary's path and modification time. Rebuilding or upgrading
/// Lemma changes it, which retires daemons started by the old binary.
// TODO: take the identity once, when the daemon starts: read at each
// request, a daemon whose binary a build replaced in place takes the new
// build's identity for its own and keeps answering with the old code.
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
            cwd: PathBuf::from("/repo/src"),
            place: PathBuf::from("/repo"),
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
    fn each_workspace_has_a_socket_of_its_own_that_any_client_works_out() {
        let billing = socket_of(Path::new("/code/billing")).unwrap();
        let web = socket_of(Path::new("/code/web")).unwrap();

        assert_ne!(billing, web);
        assert_eq!(billing, socket_of(Path::new("/code/billing")).unwrap());
    }

    #[test]
    fn a_daemons_socket_is_told_from_the_one_its_runtimes_listen_on() {
        let socket = Path::new("/run/user/1000/lemma/15b63ad8873686e4.sock");

        assert!(is_daemon_socket(socket));
        assert!(!is_daemon_socket(&crate::runtime::socket_beside(socket)));
        assert!(!is_daemon_socket(&lock_path(socket)));
    }

    fn arguments(args: &[&str]) -> Vec<String> {
        args.iter().map(|&arg| arg.to_owned()).collect()
    }

    #[test]
    fn a_command_is_about_the_directory_it_names_or_else_where_it_ran() {
        let directory = tempfile::tempdir().unwrap();
        let cwd = directory.path().join("here");
        let other = directory.path().join("other");
        fs::create_dir_all(&cwd).unwrap();
        fs::create_dir_all(&other).unwrap();
        fs::write(other.join("mod.lm"), "module other\n").unwrap();
        let elsewhere = other.to_str().unwrap();
        let module = other.join("mod.lm");

        assert_eq!(place_of(&arguments(&["refs", "greet"]), &cwd), cwd);
        assert_eq!(place_of(&arguments(&["index", elsewhere]), &cwd), other);
        assert_eq!(
            place_of(&arguments(&["check", module.to_str().unwrap()]), &cwd),
            other
        );
        assert_eq!(
            place_of(&arguments(&["daemon", "lsp", "start", elsewhere]), &cwd),
            other
        );
        assert_eq!(place_of(&arguments(&["not-a-command"]), &cwd), cwd);
    }

    #[test]
    fn a_daemon_exits_once_nothing_is_warm_and_it_was_idle_as_long_as_servers_are_kept() {
        let kept = Some(Duration::from_secs(7200));
        let long = Duration::from_secs(7200);
        let short = Duration::from_secs(60);

        assert!(has_nothing_to_stay_for(true, long, kept));
        assert!(!has_nothing_to_stay_for(true, short, kept));
        assert!(!has_nothing_to_stay_for(false, long, kept));
        assert!(!has_nothing_to_stay_for(true, long, None));
    }

    #[test]
    fn who_may_start_a_daemon_runs_from_any_command_to_none() {
        let starts =
            |autostart| [Asker::Command, Asker::Server].map(|asker| may_start(autostart, asker));

        assert_eq!(starts(Autostart::Always), [true, true]);
        assert_eq!(starts(Autostart::Clients), [false, true]);
        assert_eq!(starts(Autostart::Never), [false, false]);
    }

    #[test]
    fn a_runtime_directory_too_deep_for_a_socket_is_not_used() {
        let deep = Path::new(
            "/home/someone/with/a/runtime/directory/set/far/too/deep/for/any/socket/to/be/bound/in/lemma",
        );

        assert!(fits_a_socket(Path::new("/run/user/1000/lemma")));
        assert!(!fits_a_socket(deep));
    }

    #[test]
    fn a_workspaces_socket_path_is_short_enough_for_unix_sockets() {
        let deep =
            Path::new("/a/very/long/path/to/a/workspace/far/below/the/home/directory/of/its/user");

        assert!(socket_of(deep).unwrap().as_os_str().len() < 104);
    }
}
