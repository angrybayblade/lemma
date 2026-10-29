//! A background process that keeps language servers warm between commands.
//!
//! Starting a language server and letting it load a large workspace takes
//! seconds; asking one that is already loaded takes milliseconds. The first
//! `lemma refs` that needs a server starts a daemon for its index database.
//! Later calls send their lookups to it over a Unix socket. The daemon exits
//! after a period without requests, stopping its servers.

use std::fs;
use std::io::{BufRead, BufReader, Write};
use std::os::unix::fs::PermissionsExt;
use std::os::unix::net::{UnixListener, UnixStream};
use std::os::unix::process::CommandExt;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::sync::{Arc, Mutex, PoisonError};
use std::thread;
use std::time::{Duration, Instant};

use anyhow::{Context, bail};
use lemma_model::{Provenance, ReferenceOutcome};
use lemma_query::{LookupContext, ReferenceOptions, ReferenceReport, SessionPool};
use lemma_store::{Store, StoredSymbol};
use serde::{Deserialize, Serialize};

/// How long a starting daemon has to begin accepting connections.
const STARTUP_LIMIT: Duration = Duration::from_secs(5);
/// How often to retry connecting while a daemon starts or stops.
const CONNECT_RETRY: Duration = Duration::from_millis(10);
/// Extra time, beyond the lookup's own timeout, to wait for an answer.
const ANSWER_GRACE: Duration = Duration::from_secs(30);

/// A message from a command to the daemon, one JSON object per line.
#[derive(Debug, Serialize, Deserialize)]
#[serde(tag = "command", rename_all = "snake_case")]
enum Request {
    References {
        /// Identifies the `lemma` binary, so a daemon from an older build
        /// is replaced instead of answering with old code.
        build: String,
        root: String,
        symbol_id: i64,
        include_declaration: bool,
        refresh: bool,
        timeout_ms: u64,
        providers_file: Option<PathBuf>,
    },
    Shutdown,
}

/// The daemon's reply, one JSON object per line.
#[derive(Debug, Serialize, Deserialize)]
#[serde(tag = "status", rename_all = "snake_case")]
enum Response {
    Answer {
        outcome: ReferenceOutcome,
        provenance: Option<Provenance>,
        from_cache: bool,
        skipped_providers: Vec<String>,
    },
    /// The daemon is from a different build and is exiting; start a new one.
    Restart,
    Failed {
        message: String,
    },
    Stopping,
}

/// A reference lookup to send to the daemon.
pub(crate) struct LookupRequest<'a> {
    pub db_path: &'a Path,
    pub root: &'a str,
    pub symbol: StoredSymbol,
    pub options: ReferenceOptions,
    pub providers_file: Option<&'a Path>,
}

/// Runs a lookup in the database's daemon, starting the daemon if needed.
pub(crate) fn find_references(request: LookupRequest<'_>) -> anyhow::Result<ReferenceReport> {
    let socket = socket_path(request.db_path)?;
    let message = Request::References {
        build: build_id(),
        root: request.root.to_owned(),
        symbol_id: request.symbol.id,
        include_declaration: request.options.include_declaration,
        refresh: request.options.refresh,
        timeout_ms: u64::try_from(request.options.request_timeout.as_millis()).unwrap_or(u64::MAX),
        providers_file: request.providers_file.map(Path::to_owned),
    };
    let answer_limit = request.options.request_timeout + ANSWER_GRACE;

    let mut response = exchange(&socket, request.db_path, &message, answer_limit)?;
    if matches!(response, Response::Restart) {
        wait_until_gone(&socket);
        response = exchange(&socket, request.db_path, &message, answer_limit)?;
    }
    match response {
        Response::Answer {
            outcome,
            provenance,
            from_cache,
            skipped_providers,
        } => Ok(ReferenceReport {
            symbol: request.symbol,
            outcome,
            provenance,
            from_cache,
            skipped_providers,
        }),
        Response::Failed { message } => bail!("{message}"),
        Response::Restart | Response::Stopping => bail!("the lemma daemon stopped unexpectedly"),
    }
}

/// Asks the database's daemon to exit. Returns whether one was running.
pub(crate) fn stop(db_path: &Path) -> anyhow::Result<bool> {
    let socket = socket_path(db_path)?;
    let Ok(mut stream) = UnixStream::connect(&socket) else {
        return Ok(false);
    };
    send(&mut stream, &Request::Shutdown)?;
    receive::<Response>(&mut stream)?;
    wait_until_gone(&socket);
    Ok(true)
}

/// Serves lookups for one database until idle for `idle_limit`.
pub(crate) fn serve(db_path: &Path, idle_limit: Duration) -> anyhow::Result<()> {
    let socket = socket_path(db_path)?;
    let Some(listener) = bind(&socket)? else {
        // Another daemon for this database won the race to start.
        return Ok(());
    };
    let mut index = OpenIndex::open(db_path)?;
    let mut pool = SessionPool::long_lived();
    let last_request = Arc::new(Mutex::new(Instant::now()));
    watch_for_idleness(socket.clone(), Arc::clone(&last_request), idle_limit);

    for stream in listener.incoming() {
        let Ok(mut stream) = stream else {
            continue;
        };
        *lock(&last_request) = Instant::now();
        let Ok(request) = receive::<Request>(&mut stream) else {
            continue;
        };
        let response = match request {
            Request::Shutdown => Response::Stopping,
            Request::References { ref build, .. } if *build != build_id() => Response::Restart,
            Request::References {
                root,
                symbol_id,
                include_declaration,
                refresh,
                timeout_ms,
                providers_file,
                ..
            } => {
                let options = ReferenceOptions {
                    include_declaration,
                    refresh,
                    request_timeout: Duration::from_millis(timeout_ms),
                };
                let lookup = DaemonLookup {
                    root: &root,
                    symbol_id,
                    options: &options,
                    providers_file: providers_file.as_deref(),
                };
                match index.current() {
                    Ok(store) => answer(store, &mut pool, &lookup),
                    Err(error) => Response::Failed {
                        message: format!("{error:#}"),
                    },
                }
            }
        };
        *lock(&last_request) = Instant::now();
        let is_exiting = matches!(response, Response::Stopping | Response::Restart);
        // A caller that gave up is not a reason to stop serving others.
        let _ = send(&mut stream, &response);
        if is_exiting {
            break;
        }
    }

    pool.shutdown();
    let _ = fs::remove_file(&socket);
    Ok(())
}

/// The daemon's connection to the index, reopened when the database file
/// is replaced, as `lemma index` does when rebuilding from scratch.
struct OpenIndex {
    path: PathBuf,
    store: Store,
    inode: Option<u64>,
}

impl OpenIndex {
    fn open(path: &Path) -> anyhow::Result<Self> {
        let store = Store::open(path)
            .with_context(|| format!("could not open index {}", path.display()))?;
        Ok(Self {
            path: path.to_owned(),
            store,
            inode: inode_of(path),
        })
    }

    fn current(&mut self) -> anyhow::Result<&mut Store> {
        if inode_of(&self.path) != self.inode {
            *self = Self::open(&self.path)?;
        }
        Ok(&mut self.store)
    }
}

fn inode_of(path: &Path) -> Option<u64> {
    use std::os::unix::fs::MetadataExt;
    fs::metadata(path).ok().map(|metadata| metadata.ino())
}

struct DaemonLookup<'a> {
    root: &'a str,
    symbol_id: i64,
    options: &'a ReferenceOptions,
    providers_file: Option<&'a Path>,
}

fn answer(store: &mut Store, pool: &mut SessionPool, lookup: &DaemonLookup<'_>) -> Response {
    match run_lookup(store, pool, lookup) {
        Ok(report) => Response::Answer {
            outcome: report.outcome,
            provenance: report.provenance,
            from_cache: report.from_cache,
            skipped_providers: report.skipped_providers,
        },
        Err(error) => Response::Failed {
            message: format!("{error:#}"),
        },
    }
}

fn run_lookup(
    store: &mut Store,
    pool: &mut SessionPool,
    lookup: &DaemonLookup<'_>,
) -> anyhow::Result<ReferenceReport> {
    let Some(snapshot) = store.snapshot_for_root(lookup.root)? else {
        bail!("{} is no longer indexed; run `lemma index`", lookup.root);
    };
    let Some(symbol) = store.symbol(snapshot.id, lookup.symbol_id)? else {
        bail!("the symbol is no longer indexed; run `lemma index` and try again");
    };
    let registry = crate::load_registry(Path::new(lookup.root), lookup.providers_file)?;
    let context = LookupContext {
        snapshot: &snapshot,
        registry: &registry,
        options: lookup.options,
    };
    Ok(lemma_query::find_references(store, &context, symbol, pool)?)
}

/// Sends a request and reads the reply, starting a daemon if none answers.
fn exchange(
    socket: &Path,
    db_path: &Path,
    request: &Request,
    answer_limit: Duration,
) -> anyhow::Result<Response> {
    let mut stream = connect_or_start(socket, db_path)?;
    stream.set_read_timeout(Some(answer_limit))?;
    send(&mut stream, request)?;
    receive(&mut stream)
}

fn connect_or_start(socket: &Path, db_path: &Path) -> anyhow::Result<UnixStream> {
    if let Ok(stream) = UnixStream::connect(socket) {
        return Ok(stream);
    }
    start_daemon(db_path)?;
    let deadline = Instant::now() + STARTUP_LIMIT;
    loop {
        if let Ok(stream) = UnixStream::connect(socket) {
            return Ok(stream);
        }
        if Instant::now() >= deadline {
            bail!("the lemma daemon did not start; retry with --no-daemon");
        }
        thread::sleep(CONNECT_RETRY);
    }
}

/// The daemon is never waited for: it outlives this command on purpose.
fn start_daemon(db_path: &Path) -> anyhow::Result<()> {
    let executable = std::env::current_exe().context("could not find the lemma executable")?;
    Command::new(executable)
        .arg("--db")
        .arg(db_path)
        .arg("daemon")
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
    // Only this user may send lookups.
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
            let _ = receive::<Response>(&mut stream);
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

/// One socket per index database, in the temporary directory because socket
/// paths are limited to about 100 bytes.
fn socket_path(db_path: &Path) -> anyhow::Result<PathBuf> {
    let db_path = db_path
        .canonicalize()
        .with_context(|| format!("could not resolve {}", db_path.display()))?;
    let hash = blake3::hash(db_path.as_os_str().as_encoded_bytes()).to_hex();
    Ok(std::env::temp_dir().join(format!("lemma-{}.sock", &hash[..16])))
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
        let request = Request::References {
            build: "1.0".to_owned(),
            root: "/repo".to_owned(),
            symbol_id: 7,
            include_declaration: false,
            refresh: true,
            timeout_ms: 60_000,
            providers_file: None,
        };

        let line = serde_json::to_string(&request).unwrap();
        let parsed: Request = serde_json::from_str(&line).unwrap();

        assert!(!line.contains('\n'));
        assert!(matches!(
            parsed,
            Request::References {
                symbol_id: 7,
                refresh: true,
                ..
            }
        ));
    }

    #[test]
    fn answers_keep_the_outcome() {
        let response = Response::Answer {
            outcome: ReferenceOutcome::Empty,
            provenance: None,
            from_cache: false,
            skipped_providers: vec!["pyright: not found".to_owned()],
        };

        let line = serde_json::to_string(&response).unwrap();
        let parsed: Response = serde_json::from_str(&line).unwrap();

        let Response::Answer {
            outcome,
            skipped_providers,
            ..
        } = parsed
        else {
            panic!("expected an answer, got {line}");
        };
        assert_eq!(outcome, ReferenceOutcome::Empty);
        assert_eq!(skipped_providers, ["pyright: not found"]);
    }

    #[test]
    fn each_database_gets_its_own_short_socket_path() {
        let directory = tempfile::tempdir().unwrap();
        let first = directory.path().join("a.db");
        let second = directory.path().join("b.db");
        fs::write(&first, "").unwrap();
        fs::write(&second, "").unwrap();

        let first_socket = socket_path(&first).unwrap();

        assert_eq!(first_socket, socket_path(&first).unwrap());
        assert_ne!(first_socket, socket_path(&second).unwrap());
        assert!(first_socket.as_os_str().len() < 104);
    }
}
