//! A background process that runs Lemma's commands with warm state.
//!
//! Every command spends most of its time on things a long-lived process can
//! do once: opening the database, and starting language servers that take
//! seconds to load a large project. So `lemma` forwards its arguments,
//! working directory and `PATH` to a per-user daemon over a Unix socket and
//! prints what comes back. The first command starts the daemon. It exits
//! after a period without commands, stopping its servers.

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

use crate::cli::Cli;
use crate::config::{self, Config};
use crate::engine::{Engine, Invocation};

/// How long a starting daemon has to begin accepting connections.
const STARTUP_LIMIT: Duration = Duration::from_secs(5);
/// How often to retry connecting while a daemon starts or stops.
const CONNECT_RETRY: Duration = Duration::from_millis(2);
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
    /// Apply the idle policy; sent by the daemon's own timer.
    Maintain,
    /// Report the running language servers.
    Status,
    Shutdown,
}

/// The daemon's reply, one JSON object per line.
#[derive(Debug, Serialize, Deserialize)]
#[serde(tag = "reply", rename_all = "snake_case")]
enum Reply {
    Done {
        stdout: String,
        stderr: String,
        exit_code: u8,
    },
    /// The daemon is from a different build and is exiting; start a new one.
    Restart,
    Stopping,
}

/// What a forwarded command printed and returned.
pub(crate) struct Finished {
    pub stdout: String,
    pub stderr: String,
    pub exit_code: u8,
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
    let mut reply = exchange(&socket, &request).ok()?;
    if matches!(reply, Reply::Restart) {
        wait_until_gone(&socket);
        reply = exchange(&socket, &request).ok()?;
    }
    match reply {
        Reply::Done {
            stdout,
            stderr,
            exit_code,
        } => Some(Finished {
            stdout,
            stderr,
            exit_code,
        }),
        Reply::Restart | Reply::Stopping => None,
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
        Reply::Restart | Reply::Stopping => Ok(None),
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
    wait_until_gone(&socket);
    Ok(true)
}

/// Serves commands until idle for the configured time, or `idle_override`.
/// The config file is read again whenever it changes.
pub(crate) fn serve(idle_override: Option<Duration>) -> anyhow::Result<()> {
    let socket = socket_path()?;
    let Some(listener) = bind(&socket)? else {
        // Another daemon won the race to start.
        return Ok(());
    };
    let config_path = config::config_path();
    let mut settings = Settings::load(config_path.as_deref())?;
    let mut engine = Engine::long_lived();
    let last_request = Arc::new(Mutex::new(Instant::now()));
    let idle_limit = idle_override.or(settings.config.idle_exit);
    if let Some(idle_limit) = idle_limit {
        watch_for_idleness(socket.clone(), Arc::clone(&last_request), idle_limit);
    }
    send_maintenance_ticks(socket.clone());

    for stream in listener.incoming() {
        let Ok(mut stream) = stream else {
            continue;
        };
        let Ok(request) = receive::<Request>(&mut stream) else {
            continue;
        };
        let reply = match request {
            Request::Shutdown => Reply::Stopping,
            Request::Maintain => {
                settings.reload_if_changed(config_path.as_deref());
                let config = &settings.config;
                engine.maintain(&config.idle_policy, config.warms_up_on_activity);
                done(String::new())
            }
            Request::Status => done(engine.status()),
            Request::Run { ref build, .. } if *build != build_id() => Reply::Restart,
            Request::Run {
                args,
                cwd,
                path_env,
                ..
            } => {
                *lock(&last_request) = Instant::now();
                let invocation = Invocation {
                    cwd,
                    path_env: path_env.map(OsString::from_vec),
                };
                let reply = run(&mut engine, args, &invocation);
                *lock(&last_request) = Instant::now();
                reply
            }
        };
        let is_exiting = matches!(reply, Reply::Stopping | Reply::Restart);
        // A caller that gave up is not a reason to stop serving others.
        let _ = send(&mut stream, &reply);
        if is_exiting {
            break;
        }
    }

    let _ = fs::remove_file(&socket);
    engine.shutdown();
    Ok(())
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

/// Runs one forwarded command the way `main` would, capturing its output.
fn run(engine: &mut Engine, args: Vec<String>, invocation: &Invocation) -> Reply {
    let mut stdout = Vec::new();
    let arguments = std::iter::once("lemma".to_owned()).chain(args);
    let (stderr, exit_code) = match Cli::try_parse_from(arguments) {
        Err(error) => (
            error.render().to_string(),
            u8::try_from(error.exit_code()).unwrap_or(1),
        ),
        Ok(cli) => match engine.execute(cli, invocation, &mut stdout) {
            Ok(code) => (String::new(), code),
            Err(error) => (format!("error: {error:#}\n"), 1),
        },
    };
    Reply::Done {
        stdout: String::from_utf8_lossy(&stdout).into_owned(),
        stderr,
        exit_code,
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
fn start_daemon() -> anyhow::Result<()> {
    let executable = std::env::current_exe().context("could not find the lemma executable")?;
    Command::new(executable)
        .arg("daemon")
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

fn wait_until_gone(socket: &Path) {
    let deadline = Instant::now() + STARTUP_LIMIT;
    while UnixStream::connect(socket).is_ok() && Instant::now() < deadline {
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

        let reply = run(&mut engine, vec!["no-such-command".to_owned()], &invocation);

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
    fn the_socket_path_is_short_enough_for_unix_sockets() {
        assert!(socket_path().unwrap().as_os_str().len() < 104);
    }
}
