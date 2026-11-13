//! Warm runtimes: the daemon keeps a Python process per interpreter and
//! environment that has imported what a repository's Python imports from the
//! interpreter's own directories, and forks it for each run a check asks
//! for. See `.specs/20-warm-runtimes.md`.
//!
//! A check asks over a socket of its own, beside the daemon's, so a run
//! never waits behind a lookup. Whatever goes wrong here, the check runs
//! the command itself, as it does with no daemon.

use std::collections::BTreeMap;
use std::io::{BufRead, BufReader, Write};
use std::os::unix::fs::PermissionsExt;
use std::os::unix::net::{UnixListener, UnixStream};
use std::path::{Path, PathBuf};
use std::process::{Child, ChildStdin, Command, Stdio};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::mpsc::{Receiver, RecvTimeoutError};
use std::sync::{Arc, Mutex, PoisonError};
use std::time::{Duration, Instant};

use lemma_graph::runtime::{PythonCode, PythonRun};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};

/// The host every warm runtime runs.
const HOST: &str = include_str!("runtime_host.py");

/// How long a host may take to start and import what it is to hold.
const START_LIMIT: Duration = Duration::from_secs(120);

/// How long a run may take in a host no run has finished in yet.
const FIRST_RUN_LIMIT: Duration = Duration::from_secs(600);

/// How long a check waits to reach the daemon before running the command
/// itself.
const CONNECT_LIMIT: Duration = Duration::from_millis(200);

/// How far past its limit a host may be before it is started again: one
/// module's import can carry it that far, and the limit is looked at
/// between imports.
const LIMIT_SLACK: u64 = 32 * 1024 * 1024;

/// Runs that failed in a host before it is given up on.
const FAILURES_ALLOWED: u32 = 3;

/// How a user sets what warm runtimes may use: the memory they may hold,
/// and how long an idle one is kept. The two say where a machine sits
/// between checks that start everything again and checks that find it
/// ready.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub(crate) struct Allowance {
    /// The memory warm runtimes may hold, in bytes; none are kept at `0`.
    pub memory_bytes: u64,
    /// How long an idle runtime is kept, in milliseconds.
    pub stop_after_ms: u64,
}

impl Default for Allowance {
    fn default() -> Self {
        Self {
            memory_bytes: 512 * 1024 * 1024,
            stop_after_ms: 30 * 60 * 1000,
        }
    }
}

/// A run asked of the daemon, one JSON object on a line.
#[derive(Debug, Serialize, Deserialize)]
struct Ask {
    /// The asking binary: a daemon of another build answers nothing.
    build: String,
    run: PythonRun,
    allowance: Allowance,
}

/// The daemon's answer, one JSON object on a line.
#[derive(Debug, Serialize, Deserialize)]
#[serde(tag = "answer", rename_all = "snake_case")]
enum Answer {
    /// The run ended; a caller takes only a clean exit for an answer.
    Ran { exit_code: i32, stdout: String },
    /// No warm runtime ran it: run it yourself.
    NotServed { why: String },
}

/// The socket checks ask for runs on, beside the daemon's own.
pub(crate) fn socket_beside(daemon_socket: &Path) -> PathBuf {
    daemon_socket.with_extension("runtime.sock")
}

/// Asks the daemon listening at `socket` for `run`; its standard output if
/// a warm runtime ran it and it exited cleanly.
pub(crate) fn ask(
    socket: &Path,
    build: &str,
    run: &PythonRun,
    allowance: Allowance,
) -> Option<String> {
    let mut stream = UnixStream::connect(socket).ok()?;
    stream.set_write_timeout(Some(CONNECT_LIMIT)).ok()?;
    let request = Ask {
        build: build.to_owned(),
        run: run.clone(),
        allowance,
    };
    let mut line = serde_json::to_vec(&request).ok()?;
    line.push(b'\n');
    stream.write_all(&line).ok()?;
    let mut answer = String::new();
    BufReader::new(stream).read_line(&mut answer).ok()?;
    match serde_json::from_str(&answer).ok()? {
        Answer::Ran {
            exit_code: 0,
            stdout,
        } => Some(stdout),
        Answer::Ran { exit_code, .. } => {
            log::debug!("runtime: the warm run exited with {exit_code}; running it here");
            None
        }
        Answer::NotServed { why } => {
            log::debug!("runtime: not served ({why}); running it here");
            None
        }
    }
}

/// The daemon's warm runtimes, as a check asks them.
pub(crate) struct Asked {
    socket: PathBuf,
    build: String,
    allowance: Allowance,
}

impl Asked {
    /// The runtimes of the daemon of `build` listening at `socket`, kept
    /// within `allowance`.
    pub(crate) fn new(socket: PathBuf, build: String, allowance: Allowance) -> Self {
        Self {
            socket,
            build,
            allowance,
        }
    }
}

impl lemma_graph::runtime::WarmRuntime for Asked {
    fn run(&self, run: &PythonRun) -> Option<String> {
        let started = Instant::now();
        let answered = ask(&self.socket, &self.build, run, self.allowance);
        if answered.is_some() {
            log::info!(
                "runtime: {} ran warm in {:.2}s",
                run.python,
                started.elapsed().as_secs_f64()
            );
        }
        answered
    }
}

/// The daemon's warm runtimes, by the key of the interpreter and
/// environment each serves.
pub(crate) struct Runtimes {
    directory: PathBuf,
    build: String,
    hosts: Mutex<BTreeMap<String, Arc<Mutex<Slot>>>>,
    next_run: AtomicU64,
}

/// One key's runtime: a host, or why there is none to ask.
enum Slot {
    Empty,
    Running(Box<Host>),
    GivenUp(String),
}

/// A host process and what it last said of itself.
struct Host {
    python: String,
    child: Child,
    requests: ChildStdin,
    answers: Receiver<Value>,
    /// Where each run's output and report are written.
    scratch: PathBuf,
    modules: u64,
    memory_bytes: u64,
    last_used: Instant,
    stop_after: Duration,
    failures: u32,
    /// The longest a run that succeeded took, which bounds the next.
    slowest: Option<Duration>,
}

impl Drop for Host {
    fn drop(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
        let _ = std::fs::remove_dir_all(&self.scratch);
    }
}

/// Where one runtime's files are.
struct HostFiles {
    /// The host's code.
    host: PathBuf,
    /// The modules the host is to hold, kept from run to run.
    list: PathBuf,
    /// A directory of the host's own, with nothing in it to import: a
    /// relative search path is told apart from a run's by it.
    home: PathBuf,
    /// Where each run's output and report are written.
    scratch: PathBuf,
}

/// What a host says, a JSON object a line, as it says it.
fn listen_to(stdout: std::process::ChildStdout) -> Receiver<Value> {
    let (sender, answers) = std::sync::mpsc::channel();
    std::thread::spawn(move || {
        let said = BufReader::new(stdout)
            .lines()
            .map_while(Result::ok)
            .filter_map(|line| serde_json::from_str::<Value>(&line).ok());
        for one in said {
            if sender.send(one).is_err() {
                return;
            }
        }
    });
    answers
}

/// Why a host could not be asked, or did not answer.
struct Failed {
    why: String,
    /// Whether the host can no longer be used at all.
    is_final: bool,
}

impl Failed {
    fn once(why: impl Into<String>) -> Self {
        Self {
            why: why.into(),
            is_final: false,
        }
    }

    fn for_good(why: impl Into<String>) -> Self {
        Self {
            why: why.into(),
            is_final: true,
        }
    }
}

impl Runtimes {
    /// Runtimes keeping their files under `directory`, answering binaries
    /// of `build`.
    pub(crate) fn new(directory: PathBuf, build: String) -> Self {
        Self {
            directory,
            build,
            hosts: Mutex::new(BTreeMap::new()),
            next_run: AtomicU64::new(1),
        }
    }

    /// Listens at `socket` until the process ends, answering each ask on a
    /// thread of its own.
    pub(crate) fn listen(self: &Arc<Self>, socket: &Path) -> std::io::Result<()> {
        let _ = std::fs::remove_file(socket);
        let listener = UnixListener::bind(socket)?;
        // Only this user may ask: a run executes what the asker names.
        std::fs::set_permissions(socket, std::fs::Permissions::from_mode(0o600))?;
        let runtimes = Arc::clone(self);
        std::thread::spawn(move || {
            for stream in listener.incoming().flatten() {
                let runtimes = Arc::clone(&runtimes);
                std::thread::spawn(move || runtimes.answer(stream));
            }
        });
        Ok(())
    }

    fn answer(&self, stream: UnixStream) {
        let mut line = String::new();
        let Ok(reading) = stream.try_clone() else {
            return;
        };
        if BufReader::new(reading).read_line(&mut line).is_err() {
            return;
        }
        let answer = match serde_json::from_str::<Ask>(&line) {
            Ok(ask) if ask.build == self.build => self.run(&ask.run, ask.allowance),
            Ok(_) => Answer::NotServed {
                why: "the daemon is another build".to_owned(),
            },
            Err(error) => Answer::NotServed {
                why: error.to_string(),
            },
        };
        let mut stream = stream;
        if let Ok(mut line) = serde_json::to_vec(&answer) {
            line.push(b'\n');
            let _ = stream.write_all(&line);
        }
    }

    /// Runs `run` in the warm runtime for its key, starting one if the
    /// allowance lets any be kept.
    fn run(&self, run: &PythonRun, allowance: Allowance) -> Answer {
        if allowance.memory_bytes == 0 {
            return not_served("no memory is allowed for warm runtimes");
        }
        let slot = Arc::clone(
            lock(&self.hosts)
                .entry(run.key.clone())
                .or_insert_with(empty),
        );
        // A runtime in the middle of a run is not waited for: the caller
        // runs beside it, as two checks at once always have.
        let Ok(mut slot) = slot.try_lock() else {
            return not_served("the warm runtime is running something else");
        };
        if let Slot::GivenUp(why) = &*slot {
            return not_served(why.clone());
        }
        if slot.should_start_again(allowance.memory_bytes) {
            *slot = Slot::Empty;
        }
        if matches!(*slot, Slot::Empty) {
            match self.start(run, allowance) {
                Ok(host) => *slot = Slot::Running(Box::new(host)),
                Err(failed) => {
                    *slot = Slot::GivenUp(failed.why.clone());
                    return not_served(failed.why);
                }
            }
        }
        let Slot::Running(host) = &mut *slot else {
            return not_served("the warm runtime is not running");
        };
        host.stop_after = Duration::from_millis(allowance.stop_after_ms);
        let identifier = self.next_run.fetch_add(1, Ordering::Relaxed);
        let answered = host.run(identifier, run, allowance.memory_bytes);
        host.last_used = Instant::now();
        let answer = match answered {
            Ok(answer) => answer,
            Err(failed) => {
                host.failures += 1;
                let given_up = failed.is_final || host.failures >= FAILURES_ALLOWED;
                log::info!(
                    "runtime: {} did not run it: {}{}",
                    host.python,
                    failed.why,
                    if given_up { "; given up on" } else { "" }
                );
                if given_up {
                    *slot = Slot::GivenUp(failed.why.clone());
                }
                not_served(failed.why)
            }
        };
        drop(slot);
        self.keep_within(allowance.memory_bytes, &run.key);
        answer
    }

    fn start(&self, run: &PythonRun, allowance: Allowance) -> Result<Host, Failed> {
        let io = |error: std::io::Error| Failed::for_good(error.to_string());
        let files = self.files_of(&run.key).map_err(io)?;
        let mut child = Command::new(&run.python)
            .arg(&files.host)
            .arg(&files.list)
            .arg(allowance.memory_bytes.to_string())
            .env_clear()
            .envs(&run.environment)
            .current_dir(&files.home)
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::null())
            .spawn()
            .map_err(io)?;
        let (Some(requests), Some(stdout)) = (child.stdin.take(), child.stdout.take()) else {
            let _ = child.kill();
            return Err(Failed::for_good("the host has no pipes"));
        };
        let mut host = Host {
            python: run.python.clone(),
            child,
            requests,
            answers: listen_to(stdout),
            scratch: files.scratch,
            modules: 0,
            memory_bytes: 0,
            last_used: Instant::now(),
            stop_after: Duration::from_millis(allowance.stop_after_ms),
            failures: 0,
            slowest: None,
        };
        let started = Instant::now();
        let first = host
            .answers
            .recv_timeout(START_LIMIT)
            .map_err(|_| Failed::for_good("the host did not start"))?;
        host.note(&first)?;
        log::info!(
            "runtime: {} started holding {} modules, {} MB, in {:.2}s",
            host.python,
            host.modules,
            host.memory_bytes / (1024 * 1024),
            started.elapsed().as_secs_f64()
        );
        Ok(host)
    }

    /// The files of the runtime for `key`, with its directories made.
    fn files_of(&self, key: &str) -> std::io::Result<HostFiles> {
        std::fs::create_dir_all(&self.directory)?;
        let host = self.directory.join(format!(
            "host-{}.py",
            &lemma_model::content_hash(HOST.as_bytes())[..16]
        ));
        if !host.exists() {
            std::fs::write(&host, HOST)?;
        }
        let files = HostFiles {
            host,
            list: self.directory.join(format!("{key}.modules.json")),
            home: self.directory.join(key).join("home"),
            scratch: self.directory.join(key).join("runs"),
        };
        std::fs::create_dir_all(&files.home)?;
        std::fs::create_dir_all(&files.scratch)?;
        Ok(files)
    }

    /// Stops the runtimes least recently used until what is left holds no
    /// more than `memory_bytes`; the one just used, `kept`, goes last.
    fn keep_within(&self, memory_bytes: u64, kept: &str) {
        let hosts = lock(&self.hosts);
        let mut held: Vec<(Instant, u64, &Arc<Mutex<Slot>>)> = Vec::new();
        for (key, slot) in hosts.iter() {
            let Ok(slot_now) = slot.try_lock() else {
                continue;
            };
            if let (Slot::Running(host), true) = (&*slot_now, key != kept) {
                held.push((host.last_used, host.memory_bytes, slot));
            }
        }
        let kept_bytes = hosts
            .get(kept)
            .and_then(|slot| slot.try_lock().ok().map(|slot| slot.memory_bytes()))
            .unwrap_or_default();
        let mut total: u64 = kept_bytes + held.iter().map(|(_, bytes, _)| bytes).sum::<u64>();
        held.sort_by_key(|(last_used, _, _)| *last_used);
        for (_, bytes, slot) in held {
            if total <= memory_bytes {
                break;
            }
            if let Ok(mut slot) = slot.try_lock() {
                *slot = Slot::Empty;
                total = total.saturating_sub(bytes);
            }
        }
    }

    /// Stops the runtimes idle longer than they are kept.
    pub(crate) fn stop_idle(&self) {
        for slot in lock(&self.hosts).values() {
            let Ok(mut slot) = slot.try_lock() else {
                continue;
            };
            let is_idle = matches!(&*slot, Slot::Running(host) if host.last_used.elapsed() >= host.stop_after);
            if is_idle {
                *slot = Slot::Empty;
            }
        }
    }

    /// The memory the runtimes hold.
    pub(crate) fn memory_bytes(&self) -> u64 {
        lock(&self.hosts)
            .values()
            .filter_map(|slot| Some(slot.try_lock().ok()?.memory_bytes()))
            .sum()
    }

    /// Stops every runtime not in the middle of a run.
    pub(crate) fn stop_all(&self) {
        for slot in lock(&self.hosts).values() {
            let Ok(mut slot) = slot.try_lock() else {
                continue;
            };
            if matches!(*slot, Slot::Running(_)) {
                *slot = Slot::Empty;
            }
        }
    }

    /// A line for each runtime, for `lemma daemon status`.
    pub(crate) fn status(&self) -> String {
        let lines: Vec<String> = lock(&self.hosts)
            .values()
            .filter_map(|slot| match slot.try_lock() {
                Ok(mut slot) => slot.described(),
                Err(_) => Some("warm runtime  running a check's command".to_owned()),
            })
            .collect();
        if lines.is_empty() {
            return String::new();
        }
        lines.join("\n") + "\n"
    }
}

impl Slot {
    /// What status says of this runtime, if there is one to speak of.
    fn described(&mut self) -> Option<String> {
        match self {
            Slot::Empty => None,
            Slot::GivenUp(why) => Some(format!("warm runtime  not used: {why}")),
            Slot::Running(host) => {
                host.hear();
                Some(format!(
                    "warm runtime  {}  {} modules  {} MB  idle {}s",
                    host.python,
                    host.modules,
                    host.memory_bytes / (1024 * 1024),
                    host.last_used.elapsed().as_secs()
                ))
            }
        }
    }

    /// Whether the host should be replaced by a new one: it holds more
    /// than `memory_bytes`, which a setting made smaller since now allows.
    fn should_start_again(&mut self, memory_bytes: u64) -> bool {
        let Slot::Running(host) = self else {
            return false;
        };
        host.hear();
        host.memory_bytes > memory_bytes.saturating_add(LIMIT_SLACK)
    }

    fn memory_bytes(&self) -> u64 {
        match self {
            Slot::Running(host) => host.memory_bytes,
            Slot::Empty | Slot::GivenUp(_) => 0,
        }
    }
}

fn empty() -> Arc<Mutex<Slot>> {
    Arc::new(Mutex::new(Slot::Empty))
}

fn not_served(why: impl Into<String>) -> Answer {
    Answer::NotServed { why: why.into() }
}

impl Host {
    /// Takes in what the host says of itself; an error if it says it can
    /// no longer be forked from.
    fn note(&mut self, said: &Value) -> Result<(), Failed> {
        let Some(state) = said.get("state") else {
            return Ok(());
        };
        self.modules = state["modules"].as_u64().unwrap_or(self.modules);
        self.memory_bytes = state["memory_bytes"].as_u64().unwrap_or(self.memory_bytes);
        match state["unsafe"].as_str() {
            Some(why) => Err(Failed::for_good(why)),
            None => Ok(()),
        }
    }

    /// Takes in whatever the host has said since it was last listened to.
    fn hear(&mut self) {
        while let Ok(said) = self.answers.try_recv() {
            let _ = self.note(&said);
        }
    }

    /// Asks the host for one run and waits for it to end.
    fn run(
        &mut self,
        identifier: u64,
        run: &PythonRun,
        limit_bytes: u64,
    ) -> Result<Answer, Failed> {
        self.hear();
        let stdout = self.scratch.join(format!("{identifier}.out"));
        let stderr = self.scratch.join(format!("{identifier}.err"));
        let modules = self.scratch.join(format!("{identifier}.modules"));
        let (kind, target) = match &run.code {
            PythonCode::Module(name) => ("module", name),
            PythonCode::Script(path) => ("script", path),
            PythonCode::Code(text) => ("code", text),
        };
        let request = json!({
            "id": identifier,
            "kind": kind,
            "target": target,
            "args": run.arguments,
            "cwd": run.directory,
            "env": run.environment,
            "stdout": stdout,
            "stderr": stderr,
            "modules": modules,
            "limit_bytes": limit_bytes,
        });
        let started = Instant::now();
        writeln!(self.requests, "{request}")
            .and_then(|()| self.requests.flush())
            .map_err(|error| Failed::for_good(error.to_string()))?;
        let limit = self.slowest.map_or(FIRST_RUN_LIMIT, |slowest| {
            slowest * 3 + Duration::from_secs(5)
        });
        let ended = self.wait_for(identifier, limit);
        let output = std::fs::read(&stdout).unwrap_or_default();
        for file in [&stdout, &stderr, &modules] {
            let _ = std::fs::remove_file(file);
        }
        let exit_code = ended?;
        if exit_code == 0 {
            let took = started.elapsed();
            self.slowest = Some(self.slowest.map_or(took, |slowest| slowest.max(took)));
        }
        Ok(Answer::Ran {
            exit_code,
            stdout: String::from_utf8_lossy(&output).into_owned(),
        })
    }

    /// The exit status of run `identifier`, waited for no longer than
    /// `limit`.
    fn wait_for(&mut self, identifier: u64, limit: Duration) -> Result<i32, Failed> {
        let deadline = Instant::now() + limit;
        loop {
            let left = deadline.saturating_duration_since(Instant::now());
            let said = match self.answers.recv_timeout(left) {
                Ok(said) => said,
                Err(RecvTimeoutError::Timeout) => {
                    let _ = writeln!(self.requests, "{}", json!({ "stop": identifier }));
                    return Err(Failed::for_good(format!(
                        "a run took longer than {}s",
                        limit.as_secs()
                    )));
                }
                Err(RecvTimeoutError::Disconnected) => {
                    return Err(Failed::for_good("the host ended"));
                }
            };
            self.note(&said)?;
            if said["id"].as_u64() != Some(identifier) {
                continue;
            }
            if let Some(why) = said["refused"].as_str() {
                return Err(Failed::for_good(why));
            }
            if let Some(signal) = said["signal"].as_i64() {
                return Err(Failed::once(format!(
                    "the run was ended by signal {signal}"
                )));
            }
            let exit = said["exit"].as_i64().unwrap_or(1);
            return Ok(i32::try_from(exit).unwrap_or(1));
        }
    }
}

// A poisoned lock only means a thread answering an ask panicked; the map of
// runtimes is whole.
fn lock<T>(mutex: &Mutex<T>) -> std::sync::MutexGuard<'_, T> {
    mutex.lock().unwrap_or_else(PoisonError::into_inner)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A repository whose `pkg.dump` prints a value from `pkg`, a helper
    /// found through a relative search path, and its arguments.
    fn repository() -> tempfile::TempDir {
        let directory = tempfile::tempdir().unwrap();
        let root = directory.path();
        std::fs::create_dir_all(root.join("pkg")).unwrap();
        std::fs::create_dir_all(root.join("src")).unwrap();
        std::fs::write(root.join("pkg/__init__.py"), "VALUE = 1\n").unwrap();
        std::fs::write(root.join("src/helper.py"), "NAME = 'from src'\n").unwrap();
        std::fs::write(
            root.join("pkg/dump.py"),
            "import decimal, json, sys\nfrom pkg import VALUE\nimport helper\n\
             print(json.dumps([VALUE, helper.NAME, sys.argv[1:], __name__]))\n",
        )
        .unwrap();
        std::fs::write(root.join("pkg/broken.py"), "raise RuntimeError('no')\n").unwrap();
        directory
    }

    /// This process's environment, with the repository's `src` searched
    /// and no bytecode kept: an edit made within the second of the last
    /// run would otherwise be hidden by the bytecode, from any interpreter.
    fn environment() -> BTreeMap<String, String> {
        let mut environment: BTreeMap<String, String> = std::env::vars().collect();
        environment.insert("PYTHONPATH".to_owned(), "src".to_owned());
        environment.insert("PYTHONDONTWRITEBYTECODE".to_owned(), "1".to_owned());
        environment
    }

    fn run_of(root: &Path, module: &str) -> PythonRun {
        PythonRun {
            python: "python3".to_owned(),
            key: "test".to_owned(),
            code: PythonCode::Module(module.to_owned()),
            arguments: vec!["a".to_owned()],
            directory: root.to_owned(),
            environment: environment(),
        }
    }

    fn cold(root: &Path, module: &str) -> String {
        let output = Command::new("python3")
            .args(["-m", module, "a"])
            .current_dir(root)
            .env_clear()
            .envs(environment())
            .output()
            .unwrap();
        String::from_utf8(output.stdout).unwrap()
    }

    fn runtimes(directory: &Path) -> Runtimes {
        Runtimes::new(directory.join("runtimes"), "build".to_owned())
    }

    fn output(answer: Answer) -> Option<String> {
        match answer {
            Answer::Ran {
                exit_code: 0,
                stdout,
            } => Some(stdout),
            Answer::Ran { .. } | Answer::NotServed { .. } => None,
        }
    }

    #[test]
    fn a_warm_run_prints_what_a_new_interpreter_prints() {
        let repository = repository();
        let root = repository.path();
        let runtimes = runtimes(root);
        let run = run_of(root, "pkg.dump");

        let first = output(runtimes.run(&run, Allowance::default()));
        let warm = output(runtimes.run(&run, Allowance::default()));

        let expected = cold(root, "pkg.dump");
        assert_eq!(expected, "[1, \"from src\", [\"a\"], \"__main__\"]\n");
        assert_eq!(first.as_deref(), Some(expected.as_str()));
        assert_eq!(warm.as_deref(), Some(expected.as_str()));
        assert!(
            runtimes.status().contains("modules"),
            "{}",
            runtimes.status()
        );
    }

    #[test]
    fn an_edit_to_the_repositorys_code_is_seen_by_the_next_run() {
        let repository = repository();
        let root = repository.path();
        let runtimes = runtimes(root);
        let run = run_of(root, "pkg.dump");
        let before = output(runtimes.run(&run, Allowance::default()));

        std::fs::write(root.join("pkg/__init__.py"), "VALUE = 'edited'\n").unwrap();
        let after = output(runtimes.run(&run, Allowance::default()));

        assert_eq!(before, Some(cold_with_value("1")));
        assert_eq!(after, Some(cold_with_value("\"edited\"")));
    }

    fn cold_with_value(value: &str) -> String {
        format!("[{value}, \"from src\", [\"a\"], \"__main__\"]\n")
    }

    #[test]
    fn a_run_that_fails_is_no_answer_and_the_runtime_stays() {
        let repository = repository();
        let root = repository.path();
        let runtimes = runtimes(root);

        let failed = runtimes.run(&run_of(root, "pkg.broken"), Allowance::default());
        let next = output(runtimes.run(&run_of(root, "pkg.dump"), Allowance::default()));

        assert!(
            matches!(failed, Answer::Ran { exit_code: 1, .. }),
            "{failed:?}"
        );
        assert_eq!(next, Some(cold(root, "pkg.dump")));
    }

    #[test]
    fn with_no_memory_allowed_nothing_is_kept_warm() {
        let repository = repository();
        let root = repository.path();
        let runtimes = runtimes(root);
        let none = Allowance {
            memory_bytes: 0,
            ..Allowance::default()
        };

        let answer = runtimes.run(&run_of(root, "pkg.dump"), none);

        assert!(matches!(answer, Answer::NotServed { .. }), "{answer:?}");
        assert_eq!(runtimes.status(), "");
    }

    #[test]
    fn a_runtime_idle_longer_than_it_is_kept_is_stopped() {
        let repository = repository();
        let root = repository.path();
        let runtimes = runtimes(root);
        let not_kept = Allowance {
            stop_after_ms: 0,
            ..Allowance::default()
        };
        output(runtimes.run(&run_of(root, "pkg.dump"), not_kept)).unwrap();

        runtimes.stop_idle();

        assert_eq!(runtimes.status(), "");
    }

    #[test]
    fn a_check_asks_over_the_socket_and_another_build_is_not_answered() {
        let repository = repository();
        let root = repository.path();
        let runtimes = Arc::new(runtimes(root));
        let socket = root.join("runtime.sock");
        runtimes.listen(&socket).unwrap();
        let run = run_of(root, "pkg.dump");

        let answered = ask(&socket, "build", &run, Allowance::default());
        let other_build = ask(&socket, "another", &run, Allowance::default());

        assert_eq!(answered, Some(cold(root, "pkg.dump")));
        assert_eq!(other_build, None);
        let mode = std::fs::metadata(&socket).unwrap().permissions().mode();
        assert_eq!(mode & 0o077, 0, "only its user may ask: {mode:o}");
    }

    #[test]
    fn a_host_that_would_hold_code_from_outside_the_interpreter_is_not_used() {
        let repository = repository();
        let root = repository.path();
        let runtimes = runtimes(root);
        let mut run = run_of(root, "pkg.dump");
        // A search path that is not relative: the host finds `helper` from
        // where it starts, and its kept list tells it to hold it.
        let outside = root.join("src").display().to_string();
        run.environment.insert("PYTHONPATH".to_owned(), outside);
        std::fs::create_dir_all(root.join("runtimes")).unwrap();
        std::fs::write(root.join("runtimes/test.modules.json"), "[\"helper\"]").unwrap();

        let answer = runtimes.run(&run, Allowance::default());

        assert!(matches!(answer, Answer::NotServed { .. }), "{answer:?}");
        assert!(
            runtimes.status().contains("not used"),
            "{}",
            runtimes.status()
        );
    }
}
