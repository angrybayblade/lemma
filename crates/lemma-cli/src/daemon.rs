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
use std::io::{BufRead, BufReader, Read, Write};
use std::os::unix::ffi::OsStringExt;
use std::os::unix::fs::{MetadataExt, PermissionsExt};
use std::os::unix::net::{UnixListener, UnixStream};
use std::os::unix::process::CommandExt;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::mpsc::{self, Receiver, Sender};
use std::sync::{Arc, Mutex, PoisonError};
use std::thread;
use std::time::{Duration, Instant, SystemTime};

use anyhow::{Context, bail};
use clap::Parser;
use serde::{Deserialize, Serialize};

use lemma_query::UsesAnswer;

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
        /// Whether the command asks for sentry on its workspace
        /// (`[sentry] autostart`).
        #[serde(default)]
        wants_sentry: bool,
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
    /// A `check` to answer, through sentry if it is attached to the check's
    /// workspace and by itself otherwise, or a `verify`.
    Check {
        build: String,
        /// Arguments after the program name.
        args: Vec<String>,
        cwd: PathBuf,
        /// The directory of the module the check starts from.
        place: PathBuf,
        /// Whether the check asks for sentry on its workspace
        /// (`[sentry] autostart`).
        #[serde(default)]
        wants_sentry: bool,
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
    /// Attach sentry to the workspace of a module for a client that stays
    /// running, and keep it for as long as the client keeps this connection
    /// open: asked by the MCP server and the editor's server.
    Hold {
        build: String,
        manifest: PathBuf,
    },
    /// A client that held sentry has gone; sent by the daemon to itself.
    Released,
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
            Self::Run { place, .. } | Self::Serves { place } | Self::Check { place, .. } => {
                Some(place)
            }
            Self::Uses { question, .. } => Some(&question.root),
            Self::Sentry {
                action: SentryAction::Start { manifest },
                ..
            }
            | Self::Hold { manifest, .. } => manifest.parent(),
            Self::Sentry { .. }
            | Self::Released
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
    /// The request is not one this daemon answers, such as a check from
    /// another build: do it yourself.
    NotServed,
    /// The check was taken; its answer follows on the same connection.
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

/// Whether a request from `asker` asks for sentry on its workspace, by
/// `[sentry] autostart`: a command does when any command may, unless its
/// environment turns sentry off (`LEMMA_SENTRY=off`) or says it runs in CI.
/// The MCP server and the editor's server ask once, when they start, and
/// hold it ([`hold_sentry`]).
fn asks_for_sentry(asker: Asker) -> bool {
    asker == Asker::Command
        && config::sentry_autostart() == Autostart::Always
        && !crate::sentry::is_turned_off()
        && std::env::var_os("CI").is_none()
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
        wants_sentry: asks_for_sentry(asker),
    };
    log::debug!("forwarding to the daemon at {}", daemon.socket.display());
    if let Some(root) = &daemon.root {
        log::info!("asking the daemon of the workspace {}", root.display());
    }
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
///
/// This thread only reads requests and passes each to the lane that
/// answers it, so that a lookup waiting for a provider holds up no other
/// kind of request: `.specs/21-workspace-daemons.md`, "Lanes".
pub(crate) fn serve(
    root: Option<&Path>,
    idle_override: Option<Duration>,
    logger: &'static crate::logging::Logger,
) -> anyhow::Result<()> {
    let (socket, serving) = Serving::open(root)?;
    let Some(lock) = take_lock(&socket)? else {
        // Another daemon serves, or won the race to start.
        return Ok(());
    };
    let Some(listener) = bind(&socket)? else {
        return Ok(());
    };
    let settings = Settings::load(config::config_path().as_deref())?;
    let last_request = Arc::new(Mutex::new(Instant::now()));
    let idle_limit = idle_override.or(settings.config.idle_exit);
    if let Some(idle_limit) = idle_limit {
        watch_for_idleness(socket.clone(), Arc::clone(&last_request), idle_limit);
    }
    send_maintenance_ticks(socket.clone());
    send_prepare_ticks(socket.clone());
    let lanes = Lanes::start(&settings, &socket, &last_request, logger)?;
    let mut reader = Reader {
        serving,
        sentry: Attachment::new(socket.clone()),
        settings,
        lanes,
    };

    for stream in listener.incoming() {
        if reader.lanes.progress.is_over.load(Ordering::Relaxed) {
            break;
        }
        let Ok(stream) = stream else {
            continue;
        };
        if !reader.pass_on(stream) {
            break;
        }
    }

    // Not serving from here: new connections are refused, and a caller runs
    // without this daemon or starts another, which waits for the lock.
    drop(listener);
    let _ = fs::remove_file(&socket);
    let _ = fs::remove_file(crate::runtime::socket_beside(&socket));
    let Reader { sentry, lanes, .. } = reader;
    drop(sentry);
    lanes.finish();
    // Held until the process ends, when the system lets go of it: that is
    // the moment `lemma daemon stop` waits for.
    std::mem::forget(lock);
    Ok(())
}

/// The thread that reads the daemon's requests: it answers what needs no
/// engine itself and passes the rest to a lane.
struct Reader {
    serving: Serving,
    sentry: Attachment,
    /// Its own copy of the settings; the provider lane keeps another.
    settings: Settings,
    lanes: Lanes,
}

impl Reader {
    /// Reads the request on `stream` and answers it or passes it on.
    /// False once the provider lane has gone: the daemon is exiting.
    fn pass_on(&mut self, mut stream: UnixStream) -> bool {
        let Ok(request) = receive::<Request>(&mut stream) else {
            return true;
        };
        if let Some(reply) = reply_about_place(&request, &mut self.serving) {
            let _ = send(&mut stream, &reply);
            return true;
        }
        if matches!(request, Request::Maintain) {
            self.settings
                .reload_if_changed(config::config_path().as_deref());
        }
        let routed = route_sentry(request, stream, &mut self.sentry, &self.settings);
        let Some((request, mut stream)) = routed else {
            return true;
        };
        let about = match request {
            Request::Status => Around {
                serving: self.serving.describe(),
                sentry: self.sentry.status(),
            },
            _ => Around::default(),
        };
        let progress = &self.lanes.progress;
        if let (Request::Status, Some(busy)) = (&request, progress.busy()) {
            let status = format!("{}{busy}{}", about.serving, about.sentry);
            let _ = send(&mut stream, &done(status));
            return true;
        }
        let request = match (index_command(&request), request) {
            (Some(cli), Request::Run { cwd, path_env, .. }) => {
                let lookup = IndexLookup {
                    cli,
                    invocation: invocation(cwd, path_env),
                    stream,
                };
                return self.lanes.to_index.send(lookup).is_ok();
            }
            (_, other) => other,
        };
        progress.waiting.fetch_add(1, Ordering::Relaxed);
        let queued = Queued {
            request,
            stream,
            is_sentry_attached: self.sentry.sentry.is_some(),
            about,
        };
        self.lanes.to_providers.send(queued).is_ok()
    }
}

/// The daemon's lanes: each answers its requests one at a time, in the
/// order they came, on a thread of its own.
struct Lanes {
    /// To the provider lane: every request that may ask a provider, and
    /// the daemon's upkeep.
    to_providers: Sender<Queued>,
    /// To the index lane: lookups answered from the index alone.
    to_index: Sender<IndexLookup>,
    progress: Arc<Progress>,
    threads: Vec<thread::JoinHandle<()>>,
}

impl Lanes {
    fn start(
        settings: &Settings,
        socket: &Path,
        last_request: &Arc<Mutex<Instant>>,
        logger: &'static crate::logging::Logger,
    ) -> anyhow::Result<Self> {
        let progress = Arc::new(Progress::default());
        let engine = engine_with(settings, &progress);
        let beside = engine.beside();
        let provider_lane = ProviderLane {
            engine,
            settings: settings.clone(),
            runtimes: start_runtimes(socket),
            last_request: Arc::clone(last_request),
            logger,
            progress: Arc::clone(&progress),
            socket: socket.to_path_buf(),
        };
        let (to_providers, for_providers) = mpsc::channel();
        let with_providers = thread::Builder::new()
            .name("provider-lane".to_owned())
            .spawn(move || provider_lane.serve(&for_providers))?;
        let (to_index, for_index) = mpsc::channel();
        let last_request = Arc::clone(last_request);
        let from_index = thread::Builder::new()
            .name("index-lane".to_owned())
            .spawn(move || serve_index_lane(beside, &for_index, &last_request, logger))?;
        Ok(Self {
            to_providers,
            to_index,
            progress,
            threads: vec![with_providers, from_index],
        })
    }

    /// Lets each lane finish what it is answering and shut its engine down.
    fn finish(self) {
        drop(self.to_providers);
        drop(self.to_index);
        for thread in self.threads {
            let _ = thread.join();
        }
    }
}

/// What the provider lane is doing, for the thread that reads requests.
#[derive(Default)]
struct Progress {
    /// Requests passed to it that it has not taken up yet.
    waiting: AtomicUsize,
    /// The command it is answering, and since when.
    answering: Mutex<Option<(String, Instant)>>,
    /// Set once it has stopped serving: the daemon is exiting.
    is_over: AtomicBool,
}

impl Progress {
    /// What `lemma daemon status` says in place of the engine's lines
    /// while a command is being answered: which, for how long, and how
    /// many requests wait behind it. `None` when none is.
    fn busy(&self) -> Option<String> {
        let answering = lock(&self.answering);
        let (command, since) = answering.as_ref()?;
        Some(format!(
            "busy with `lemma {command}` for {}s, {} waiting behind it; its language servers are listed once it is answered\n",
            since.elapsed().as_secs(),
            self.waiting.load(Ordering::Relaxed)
        ))
    }
}

/// What `lemma daemon status` says around the engine's own lines: what
/// the daemon serves, before them, and its sentry, after.
#[derive(Default)]
struct Around {
    serving: String,
    sentry: String,
}

/// A request for the provider lane, with what the thread that read it
/// knows and the lane does not.
struct Queued {
    request: Request,
    stream: UnixStream,
    is_sentry_attached: bool,
    about: Around,
}

/// The provider lane: every request that may ask a language server, and
/// the daemon's upkeep, one at a time in the order they came.
struct ProviderLane {
    engine: Engine,
    settings: Settings,
    runtimes: Arc<crate::runtime::Runtimes>,
    last_request: Arc<Mutex<Instant>>,
    logger: &'static crate::logging::Logger,
    progress: Arc<Progress>,
    socket: PathBuf,
}

impl ProviderLane {
    /// Serves until the daemon is told to stop or holds nothing warm, then
    /// has the thread reading requests stop too.
    fn serve(mut self, queue: &Receiver<Queued>) {
        for queued in queue {
            self.progress.waiting.fetch_sub(1, Ordering::Relaxed);
            if !self.answer(queued) {
                break;
            }
        }
        self.progress.is_over.store(true, Ordering::Relaxed);
        // Wakes the thread reading requests, which is waiting for one.
        drop(UnixStream::connect(&self.socket));
        self.engine.shutdown();
    }

    /// Answers one request; false when the daemon is to exit after it.
    fn answer(&mut self, queued: Queued) -> bool {
        let Queued {
            request,
            mut stream,
            is_sentry_attached,
            about,
        } = queued;
        self.settings
            .follow_sentry(is_sentry_attached, &mut self.engine);
        let is_status = matches!(request, Request::Status);
        let is_maintenance = matches!(request, Request::Maintain);
        if is_maintenance {
            maintain_runtimes(&self.runtimes, &mut self.engine, &self.settings.config);
        }
        let command = command_of(&request).map(|command| (command, Instant::now()));
        *lock(&self.progress.answering) = command;
        let mut reply = reply_to(
            request,
            &mut self.engine,
            &mut self.settings,
            &self.last_request,
            self.logger,
        );
        *lock(&self.progress.answering) = None;
        if is_status {
            let after = self.runtimes.status() + &about.sentry;
            complete_status(&mut reply, &about.serving, &after);
        }
        let is_exiting = matches!(reply, Reply::Stopping | Reply::Restart);
        // A caller that gave up is not a reason to stop serving others.
        let _ = send(&mut stream, &reply);
        let config = &self.settings.config;
        let is_cold = is_maintenance
            && !is_sentry_attached
            && is_cold(&self.engine, &self.runtimes, &self.last_request, config);
        !(is_exiting || is_cold)
    }
}

/// The command a request runs, as its caller wrote it; `None` for the
/// daemon's own upkeep and for questions about the daemon.
fn command_of(request: &Request) -> Option<String> {
    match request {
        Request::Run { args, .. } => Some(args.join(" ")),
        Request::Uses { .. } => Some("check".to_owned()),
        _ => None,
    }
}

/// A lookup answered from the index alone: it asks no provider, and so
/// need not wait behind one that does.
struct IndexLookup {
    cli: Cli,
    invocation: Invocation,
    stream: UnixStream,
}

/// The command `request` runs, when this build answers it from the index
/// alone.
fn index_command(request: &Request) -> Option<Cli> {
    let Request::Run { build, args, .. } = request else {
        return None;
    };
    if *build != build_id() {
        return None;
    }
    answered_from_index(args)
}

/// The command `args` spell, when it is one answered from the index alone.
fn answered_from_index(args: &[String]) -> Option<Cli> {
    // Reading a command line costs about a millisecond, which a request
    // for the provider lane is spared: its lane reads it once.
    let may_be_one = args
        .iter()
        .any(|word| word == "symbols" || word == "outline");
    if !may_be_one {
        return None;
    }
    let arguments = std::iter::once("lemma".to_owned()).chain(args.iter().cloned());
    let cli = Cli::try_parse_from(arguments).ok()?;
    let is_one = matches!(
        cli.command,
        crate::cli::Command::Symbols(_) | crate::cli::Command::Outline { .. }
    );
    is_one.then_some(cli)
}

/// The index lane: answers lookups that ask no provider, one at a time,
/// from `engine`, which shares its indexes with the provider lane's.
fn serve_index_lane(
    mut engine: Engine,
    asked: &Receiver<IndexLookup>,
    last_request: &Mutex<Instant>,
    logger: &'static crate::logging::Logger,
) {
    let mut tidied = Instant::now();
    for lookup in asked {
        let IndexLookup {
            cli,
            invocation,
            mut stream,
        } = lookup;
        let lines = Lines::OfThisThread(logger);
        let reply = busy(last_request, || {
            run_parsed(&mut engine, cli, &invocation, lines)
        });
        let _ = send(&mut stream, &reply);
        // The provider lane's upkeep does not reach this engine, which
        // would otherwise keep the files of deleted directories open.
        if tidied.elapsed() >= MAINTENANCE_INTERVAL {
            engine.forget_vanished();
            tidied = Instant::now();
        }
    }
    engine.shutdown();
}

/// Puts around the engine's status what the daemon serves, `before` it,
/// and its warm runtimes and sentry, `after` it.
fn complete_status(reply: &mut Reply, before: &str, after: &str) {
    if let Reply::Done { stdout, .. } = reply {
        stdout.insert_str(0, before);
        stdout.push_str(after);
    }
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

/// The daemon's engine with the settings in force, which gives way while a
/// caller waits on `listener`.
fn engine_with(settings: &Settings, progress: &Arc<Progress>) -> Engine {
    let config = &settings.config;
    let mut engine = Engine::long_lived();
    engine.set_graph_watches(settings.graph_watches());
    engine.set_graphs_kept_ready(config.graphs_kept_ready);
    engine.set_when_slow(config.when_slow);
    let progress = Arc::clone(progress);
    engine.give_way_when(Arc::new(move || {
        progress.waiting.load(Ordering::Relaxed) > 0
    }));
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

/// Serves what does not go through the engine: a check, attaching or
/// detaching sentry, and detaching it from a workspace that is gone. Everything else comes
/// back, with its connection, for the daemon to serve.
fn route_sentry(
    request: Request,
    mut stream: UnixStream,
    attachment: &mut Attachment,
    settings: &Settings,
) -> Option<(Request, UnixStream)> {
    match request {
        Request::Run {
            wants_sentry: true,
            ref place,
            ..
        } => {
            attachment.use_for_commands(crate::workspace::entry_module_above(place));
            Some((request, stream))
        }
        Request::Check { .. } => {
            if let Some(asked) = Asked::of(request) {
                hand_check(asked, stream, attachment);
            }
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
        } if build == build_id() && attachment.sentry.is_some() => {
            if let Some(attached) = attachment.sentry.as_ref() {
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
            let reply = attachment.asked_by_hand(&build, action);
            let _ = send(&mut stream, &reply);
            None
        }
        Request::Hold { build, manifest } => {
            attachment.hold(&build, manifest, stream);
            None
        }
        Request::Released => {
            attachment.release();
            let _ = send(&mut stream, &done(String::new()));
            None
        }
        Request::Maintain => {
            attachment.let_go_if_done(settings.config.idle_policy.stop_after);
            Some((Request::Maintain, stream))
        }
        other => Some((other, stream)),
    }
}

/// The daemon's sentry, and what keeps it attached: a client asked for it
/// and is still connected, or it was asked for by hand and is in use.
struct Attachment {
    sentry: Option<Sentry>,
    /// The clients that asked for it and are still connected: an MCP
    /// server, the editor's server.
    holders: usize,
    /// Whether a command asked for it: `lemma daemon sentry start`, or any
    /// command where `[sentry] autostart` has commands ask. It then stays
    /// without a client, until no command has used it for as long as an
    /// idle language server is kept.
    is_kept_by_hand: bool,
    /// When it was attached or a command last used it.
    used: Instant,
    /// An entry module it could not be attached to for a command, which is
    /// not tried again until the daemon next looks after itself.
    refused: Option<PathBuf>,
    /// The daemon's own socket, to tell it that a client has gone.
    socket: PathBuf,
}

impl Attachment {
    fn new(socket: PathBuf) -> Self {
        Self {
            sentry: None,
            holders: 0,
            is_kept_by_hand: false,
            used: Instant::now(),
            refused: None,
            socket,
        }
    }

    /// Attaches sentry for a command that asks for it, to the workspace of
    /// the entry module `manifest`, unless it is attached already, and
    /// counts the command as using it. A module it cannot attach to, such
    /// as one that does not load, is left for a while.
    fn use_for_commands(&mut self, manifest: Option<PathBuf>) {
        self.used = Instant::now();
        let Some(manifest) = manifest.filter(|manifest| self.refused.as_ref() != Some(manifest))
        else {
            return;
        };
        if self.sentry.is_some() {
            return;
        }
        match Sentry::attach(&manifest) {
            Ok(attached) => {
                log::info!("sentry: attached to {} for a command", manifest.display());
                self.sentry = Some(attached);
                self.is_kept_by_hand = true;
            }
            Err(error) => {
                log::warn!(
                    "sentry: could not attach to {}: {error:#}",
                    manifest.display()
                );
                self.refused = Some(manifest);
            }
        }
    }

    /// Serves `lemma daemon sentry`: attaches, detaches or reports.
    fn asked_by_hand(&mut self, build: &str, action: SentryAction) -> Reply {
        let is_start = matches!(action, SentryAction::Start { .. });
        let is_stop = matches!(action, SentryAction::Stop);
        let reply = control_sentry(build, action, &mut self.sentry);
        let is_done = matches!(reply, Reply::Done { exit_code: 0, .. });
        if is_start && is_done {
            self.is_kept_by_hand = true;
            self.used = Instant::now();
        }
        if is_stop {
            self.is_kept_by_hand = false;
        }
        reply
    }

    /// Attaches sentry to the workspace of `manifest` for the client on
    /// `stream`, which is told whether it is attached, and keeps it until
    /// that client closes the connection.
    fn hold(&mut self, build: &str, manifest: PathBuf, mut stream: UnixStream) {
        let reply = control_sentry(build, SentryAction::Start { manifest }, &mut self.sentry);
        let is_attached = matches!(reply, Reply::Done { exit_code: 0, .. });
        let is_told = send(&mut stream, &reply).is_ok();
        if !is_attached {
            return;
        }
        self.used = Instant::now();
        self.holders += 1;
        if !is_told || tell_when_gone(stream, self.socket.clone()).is_err() {
            self.release();
        }
    }

    /// A client that held sentry has gone: sentry is let go once no client
    /// holds it, unless it was also asked for by hand.
    fn release(&mut self) {
        self.holders = self.holders.saturating_sub(1);
        if self.holders == 0 && !self.is_kept_by_hand && self.sentry.take().is_some() {
            log::info!("sentry: the clients that asked for it have gone; detaching");
        }
    }

    /// Lets sentry go when its workspace is gone, or when no client holds
    /// it and no check has used it for `kept`, the time an idle language
    /// server is kept.
    fn let_go_if_done(&mut self, kept: Option<Duration>) {
        self.refused = None;
        let Some(attached) = &self.sentry else {
            return;
        };
        let is_unused = self.holders == 0 && kept.is_some_and(|kept| self.used.elapsed() >= kept);
        if attached.is_gone() {
            log::info!("sentry: the workspace is gone; detaching");
        } else if is_unused {
            log::info!("sentry: not used for a while by anyone; detaching");
        } else {
            return;
        }
        self.sentry = None;
        self.is_kept_by_hand = false;
    }

    /// The lines of `lemma daemon status` about sentry and who keeps it.
    fn status(&self) -> String {
        let Some(attached) = &self.sentry else {
            return String::new();
        };
        let clients = match self.holders {
            1 => "1 connected client".to_owned(),
            holders => format!("{holders} connected clients"),
        };
        let kept_by = match (self.holders, self.is_kept_by_hand) {
            (0, _) => "kept for commands".to_owned(),
            (_, false) => format!("kept by {clients}"),
            (_, true) => format!("kept by {clients}, and for commands"),
        };
        format!("{}sentry is {kept_by}\n", attached.status())
    }
}

/// Waits on another thread until the client on `stream` closes it, then
/// tells the daemon listening on `socket`, which is this one, that a client
/// holding sentry has gone.
fn tell_when_gone(mut stream: UnixStream, socket: PathBuf) -> std::io::Result<()> {
    let waiting = thread::Builder::new().name("sentry-holder".to_owned());
    waiting.spawn(move || {
        let mut read = [0_u8; 64];
        while matches!(stream.read(&mut read), Ok(count) if count > 0) {}
        if let Ok(mut daemon) = UnixStream::connect(&socket) {
            let _ = send(&mut daemon, &Request::Released);
            let _ = receive::<Reply>(&mut daemon);
        }
    })?;
    Ok(())
}

/// A check asked of the daemon, before its arguments are read.
struct Asked {
    build: String,
    args: Vec<String>,
    cwd: PathBuf,
    environment: BTreeMap<String, String>,
    as_of: Option<String>,
    wants_sentry: bool,
}

impl Asked {
    /// What a `Check` request asks; `None` for any other request.
    fn of(request: Request) -> Option<Self> {
        let Request::Check {
            build,
            args,
            cwd,
            environment,
            as_of,
            wants_sentry,
            place: _,
        } = request
        else {
            return None;
        };
        Some(Self {
            build,
            args,
            cwd,
            environment,
            as_of,
            wants_sentry,
        })
    }
}

/// Takes a `check` or a `verify` asked of the daemon. Either is answered
/// on `stream` from elsewhere (sentry's thread, or a process of its own),
/// so the daemon goes on serving lookups meanwhile. One from another build, or any other command, is left to its
/// asker.
fn hand_check(asked: Asked, mut stream: UnixStream, attachment: &mut Attachment) {
    let words = std::iter::once("lemma".to_owned()).chain(asked.args.iter().cloned());
    let command = Cli::try_parse_from(words)
        .ok()
        .filter(|_| asked.build == build_id());
    let (command, json) = match command {
        Some(cli) => (Some(cli.command), cli.json),
        None => (None, false),
    };
    match command {
        Some(crate::cli::Command::Check(args)) => {
            check_through(attachment, Checked { asked, args, json }, stream);
        }
        Some(crate::cli::Command::Verify(_)) if send(&mut stream, &Reply::Accepted).is_ok() => {
            let alone = Alone {
                args: asked.args,
                cwd: asked.cwd,
                environment: asked.environment,
            };
            answer_alone(alone, stream);
        }
        Some(crate::cli::Command::Verify(_)) => {}
        _ => {
            let _ = send(&mut stream, &Reply::NotServed);
        }
    }
}

/// A check asked of the daemon, with its arguments read.
struct Checked {
    asked: Asked,
    args: crate::cli::CheckArgs,
    json: bool,
}

/// Answers a check: through sentry when it is attached to the check's
/// workspace and the asker has not turned it off (`LEMMA_SENTRY=off`), and
/// otherwise in a process of its own, with nothing kept from earlier
/// checks.
fn check_through(attachment: &mut Attachment, check: Checked, mut stream: UnixStream) {
    let Checked { asked, args, json } = check;
    let manifest = crate::check::entry_manifest_in(&asked.cwd, &args.manifest);
    let manifest = manifest.canonicalize().unwrap_or(manifest);
    let uses_sentry = asked
        .environment
        .get("LEMMA_SENTRY")
        .is_none_or(|value| value != "off");
    if asked.wants_sentry && uses_sentry {
        attachment.use_for_commands(Some(manifest.clone()));
    }
    if send(&mut stream, &Reply::Accepted).is_err() {
        return;
    }
    let attached = attachment
        .sentry
        .as_ref()
        .filter(|attached| uses_sentry && attached.manifest() == manifest);
    let Some(attached) = attached else {
        let alone = Alone {
            args: asked.args,
            cwd: asked.cwd,
            environment: asked.environment,
        };
        return answer_alone(alone, stream);
    };
    attachment.used = Instant::now();
    let question = crate::sentry::Question {
        args,
        json,
        raw: asked.args,
        cwd: asked.cwd,
        environment: asked.environment,
        as_of: asked.as_of,
    };
    attached.ask(crate::sentry::Ask { question, stream });
}

/// A command the daemon runs in a process of its own for whoever asked:
/// its arguments, and the directory and environment it was asked from.
struct Alone {
    args: Vec<String>,
    cwd: PathBuf,
    environment: BTreeMap<String, String>,
}

/// Answers a `check` no sentry serves, or a `verify`, by running it in a
/// process of its own, as its asker would have, and passing on what it
/// prints. The daemon waits for it on another thread, so lookups go on.
///
/// A process, not a thread of the daemon's: what a check allocates is given
/// back only when its process ends. Answering 214 checks of a large
/// workspace on threads left the daemon holding 1.3 GB. If the process
/// cannot be run, the connection closes unanswered and the asker does the
/// work itself.
fn answer_alone(alone: Alone, stream: UnixStream) {
    let waiting = thread::Builder::new()
        .name("alone".to_owned())
        .spawn(move || match run_alone(&alone) {
            Ok(answer) => reply_answer(stream, &answer),
            Err(error) => log::warn!("could not run `lemma {}`: {error:#}", alone.args.join(" ")),
        });
    if let Err(error) = waiting {
        log::warn!("could not start a thread to wait for a command: {error}");
    }
}

/// Runs `alone` to its end: what it printed and returned.
fn run_alone(alone: &Alone) -> anyhow::Result<crate::sentry::Answer> {
    let executable = std::env::current_exe().context("could not find the lemma executable")?;
    log::info!("running `lemma {}` for a caller", alone.args.join(" "));
    let output = Command::new(executable)
        .args(&alone.args)
        .arg("--in-own-process")
        .current_dir(&alone.cwd)
        .env_clear()
        .envs(&alone.environment)
        .stdin(Stdio::null())
        .output()
        .context("could not start it")?;
    let exit_code = output
        .status
        .code()
        .and_then(|code| u8::try_from(code).ok());
    Ok(crate::sentry::Answer {
        stdout: String::from_utf8_lossy(&output.stdout).into_owned(),
        stderr: String::from_utf8_lossy(&output.stderr).into_owned(),
        exit_code: exit_code.unwrap_or(1),
    })
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

/// A check to ask a daemon for: its command line, where it was run, and
/// the environment of whoever ran it.
pub(crate) struct AskedCheck {
    pub args: Vec<String>,
    pub cwd: PathBuf,
    pub environment: BTreeMap<String, String>,
    /// A journal token whose changes the check is about, from a client
    /// that follows sentry's journal.
    pub as_of: Option<String>,
}

/// Asks the daemon of the check's workspace to answer it, starting one if
/// none serves it and `asker` may. `None` when no daemon of this build
/// answers, and the caller should check by itself.
pub(crate) fn ask_for_check(
    args: Vec<String>,
    cwd: PathBuf,
    environment: BTreeMap<String, String>,
    asker: Asker,
) -> Option<crate::sentry::Answer> {
    let check = AskedCheck {
        args,
        cwd,
        environment,
        as_of: None,
    };
    ask_for_check_as_of(check, asker)
}

/// [`ask_for_check`] for a client that follows sentry's journal, about the
/// changes up to the journal token `check.as_of`.
pub(crate) fn ask_for_check_as_of(
    check: AskedCheck,
    asker: Asker,
) -> Option<crate::sentry::Answer> {
    let place = canonical(&place_of(&check.args, &check.cwd));
    let starts = may_start(config::autostart(), asker);
    let daemon = daemon_to_ask(&place, starts)?;
    let mut stream = if starts {
        connect_or_start(&daemon).ok()?
    } else {
        UnixStream::connect(&daemon.socket).ok()?
    };
    let request = Request::Check {
        build: build_id(),
        args: check.args,
        cwd: check.cwd,
        place,
        wants_sentry: asks_for_sentry(asker),
        environment: check.environment,
        as_of: check.as_of,
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
            log::info!("answered by the daemon, exit code {exit_code}");
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

/// Sentry, kept attached for a client that stays running. The daemon lets
/// go of sentry once every client that asked for it has dropped this, or
/// has gone.
pub(crate) struct SentryHold {
    _connection: UnixStream,
}

/// Asks the daemon of the workspace whose entry module is `manifest` to
/// attach sentry to it and keep it for this client, starting a daemon, or
/// replacing one of another build, if needed and `[daemon] autostart`
/// allows. `None` when sentry does not serve the workspace: no daemon may
/// be started, or it could not attach.
pub(crate) fn hold_sentry(manifest: &Path) -> anyhow::Result<Option<SentryHold>> {
    let manifest = canonical(manifest);
    let starts = may_start(config::autostart(), Asker::Server);
    let workspace = manifest.parent().unwrap_or(&manifest);
    let Some(daemon) = daemon_to_ask(workspace, starts) else {
        return Ok(None);
    };
    let request = Request::Hold {
        build: build_id(),
        manifest: manifest.clone(),
    };
    let (mut reply, mut connection) = held(&daemon, &request, starts)?;
    if matches!(reply, Reply::Restart) {
        log::info!("the running daemon is another build; replacing it");
        wait_until_gone(&daemon.socket)?;
        (reply, connection) = held(&daemon, &request, true)?;
    }
    match reply {
        Reply::Done { exit_code: 0, .. } => Ok(Some(SentryHold {
            _connection: connection,
        })),
        Reply::Done { stderr, .. } => {
            log::info!(
                "sentry does not serve {}: {}",
                manifest.display(),
                stderr.trim()
            );
            Ok(None)
        }
        _ => Ok(None),
    }
}

/// Sends `request` to `daemon` and reads its reply, keeping the connection.
fn held(daemon: &Daemon, request: &Request, starts: bool) -> anyhow::Result<(Reply, UnixStream)> {
    let mut stream = if starts {
        connect_or_start(daemon)?
    } else {
        UnixStream::connect(&daemon.socket)?
    };
    send(&mut stream, request)?;
    let reply = receive(&mut stream)?;
    Ok((reply, stream))
}

/// Sends `action` to the sentry of the daemon serving `place`. Attaching
/// starts the workspace's daemon if none serves it and `[daemon] autostart`
/// lets a command start one. `None` when no daemon serves `place`.
pub(crate) fn sentry_control(
    place: &Path,
    action: SentryAction,
) -> anyhow::Result<Option<(String, String, u8)>> {
    let is_start = matches!(action, SentryAction::Start { .. });
    let starts = is_start && may_start(config::autostart(), Asker::Command);
    let Some(daemon) = daemon_to_ask(&canonical(place), starts) else {
        return Ok(None);
    };
    let connected = if starts {
        connect_or_start(&daemon).ok()
    } else {
        UnixStream::connect(&daemon.socket).ok()
    };
    let Some(mut stream) = connected else {
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
        Request::Check { .. }
        | Request::Sentry { .. }
        | Request::Hold { .. }
        | Request::Released
        | Request::Serves { .. } => Reply::NotServed,
        Request::Maintain => {
            settings.reload_if_changed(config::config_path().as_deref());
            let config = &settings.config;
            engine.set_graph_watches(settings.graph_watches());
            engine.set_graphs_kept_ready(config.graphs_kept_ready);
            engine.set_when_slow(config.when_slow);
            engine.maintain(&config.idle_policy, settings.warms_up_on_activity());
            done(String::new())
        }
        Request::Prepare => {
            let config = &settings.config;
            // A workspace is kept ready for as long as an idle language
            // server is kept running: both are for work in progress.
            let asked_within = config.idle_policy.pause_after;
            Reply::Prepared {
                is_any_kept_ready: engine.prepare_graphs(
                    settings.graph_prepare(),
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
            busy(last_request, || {
                run(engine, args, &invocation, Lines::OfTheCommand(logger))
            })
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
#[derive(Clone)]
struct Settings {
    config: Config,
    modified: Option<SystemTime>,
    /// Whether sentry is attached. Work ahead of a request is sentry's:
    /// without it no workspace is watched for graphs, no graph is got
    /// ready, and no server is started again when files change.
    is_sentry_attached: bool,
}

impl Settings {
    fn load(path: Option<&Path>) -> anyhow::Result<Self> {
        Ok(Self {
            config: config::load(path)?,
            modified: config::modified(path),
            is_sentry_attached: false,
        })
    }

    /// Has what sentry does ahead of a request start and stop with it:
    /// `engine` watches workspaces for graphs only while it is attached.
    fn follow_sentry(&mut self, is_attached: bool, engine: &mut Engine) {
        if self.is_sentry_attached == is_attached {
            return;
        }
        self.is_sentry_attached = is_attached;
        engine.set_graph_watches(self.graph_watches());
    }

    /// How many watches graphs across a workspace may take: none without
    /// sentry, when Git is asked about each repository at every graph.
    fn graph_watches(&self) -> Option<usize> {
        if self.is_sentry_attached {
            return self.config.graph_watches;
        }
        Some(0)
    }

    /// How far the daemon gets ready for the next graph once files change:
    /// not at all without sentry.
    fn graph_prepare(&self) -> config::Prepare {
        if self.is_sentry_attached {
            return self.config.graph_prepare;
        }
        config::Prepare::Off
    }

    /// Whether stopped servers are started again when files change.
    fn warms_up_on_activity(&self) -> bool {
        self.is_sentry_attached && self.config.warms_up_on_activity
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

/// Whose log lines a command run in the daemon sends back to its caller.
#[derive(Clone, Copy)]
enum Lines<'a> {
    /// Every thread's but those kept on their own: the command may ask
    /// providers, whose lines come from other threads.
    OfTheCommand(&'a crate::logging::Logger),
    /// The lines of the thread it runs on, beside another command.
    OfThisThread(&'a crate::logging::Logger),
}

impl Lines<'_> {
    fn start(self, level: Option<crate::logging::LogLevel>) {
        match self {
            Self::OfTheCommand(logger) => logger.start_capture(level),
            Self::OfThisThread(logger) => logger.start_capture_of_thread(level),
        }
    }

    fn finish(self) -> String {
        match self {
            Self::OfTheCommand(logger) => logger.finish_capture(None),
            Self::OfThisThread(logger) => logger.finish_capture_of_thread(),
        }
    }
}

/// Runs one forwarded command the way `main` would, capturing its output
/// and, at the level it asks for, its log lines, which go before its own
/// error output.
fn run(engine: &mut Engine, args: Vec<String>, invocation: &Invocation, lines: Lines<'_>) -> Reply {
    let arguments = std::iter::once("lemma".to_owned()).chain(args);
    match Cli::try_parse_from(arguments) {
        Ok(cli) => run_parsed(engine, cli, invocation, lines),
        Err(error) => Reply::Done {
            stdout: String::new(),
            stderr: error.render().to_string(),
            exit_code: u8::try_from(error.exit_code()).unwrap_or(1),
            logs: String::new(),
        },
    }
}

/// Runs the command `cli` names, as [`run`] does one it has read.
fn run_parsed(engine: &mut Engine, cli: Cli, invocation: &Invocation, lines: Lines<'_>) -> Reply {
    let mut stdout = Vec::new();
    lines.start(cli.logging);
    log::info!("running in the daemon (pid {})", std::process::id());
    let result = engine.execute(cli, invocation, &mut stdout);
    let logs = lines.finish();
    let (stderr, exit_code) = match result {
        Ok(code) => (String::new(), code),
        Err(error) => (format!("error: {error:#}\n"), 1),
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

/// The root of the workspace `place` belongs to: where its daemon is, or
/// would be started.
pub(crate) fn workspace_root_of(place: &Path) -> PathBuf {
    let place = canonical(place);
    Workspace::of(&place, &named_repositories)
        .root()
        .to_path_buf()
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
        Asked::Verify(verify) => Some(verify.module),
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

    fn words(command: &str) -> Vec<String> {
        command.split(' ').map(str::to_owned).collect()
    }

    #[test]
    fn a_symbol_search_and_an_outline_are_answered_from_the_index() {
        assert!(answered_from_index(&words("symbols price --exact")).is_some());
        assert!(answered_from_index(&words("-l info outline src/m.py")).is_some());
    }

    #[test]
    fn a_lookup_that_asks_a_provider_is_not_answered_from_the_index() {
        assert!(answered_from_index(&words("refs price")).is_none());
        assert!(answered_from_index(&words("graph price")).is_none());
    }

    #[test]
    fn a_provider_lookup_for_a_name_like_a_command_is_not_one_from_the_index() {
        assert!(answered_from_index(&words("refs symbols")).is_none());
        assert!(answered_from_index(&words("definition outline")).is_none());
    }

    #[test]
    fn a_command_line_that_does_not_read_is_left_to_the_lane_that_says_so() {
        assert!(answered_from_index(&words("symbols --no-such-option")).is_none());
    }

    #[test]
    fn status_names_the_command_being_answered_and_how_many_wait() {
        let progress = Progress::default();
        assert_eq!(progress.busy(), None);

        *lock(&progress.answering) = Some(("refs price".to_owned(), Instant::now()));
        progress.waiting.store(2, Ordering::Relaxed);

        let busy = progress.busy().unwrap();
        assert!(
            busy.starts_with("busy with `lemma refs price` for "),
            "{busy}"
        );
        assert!(busy.contains(", 2 waiting behind it"), "{busy}");
    }

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
            wants_sentry: false,
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
            Lines::OfTheCommand(&logger),
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
    fn what_is_done_ahead_of_a_request_is_done_only_while_sentry_is_attached() {
        let mut settings = Settings {
            config: Config::default(),
            modified: None,
            is_sentry_attached: false,
        };
        let configured = settings.config.graph_watches;

        assert_eq!(settings.graph_watches(), Some(0));
        assert_eq!(settings.graph_prepare(), config::Prepare::Off);
        assert!(!settings.warms_up_on_activity());

        settings.is_sentry_attached = true;

        assert_eq!(settings.graph_watches(), configured);
        assert_eq!(settings.graph_prepare(), config::Prepare::Graph);
        assert!(settings.warms_up_on_activity());
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
