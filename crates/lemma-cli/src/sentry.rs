//! Sentry: the daemon's attachment to one workspace (`.specs/14-sentry.md`).
//!
//! A worker thread watches the workspace's repositories, and in small
//! batches drops what the daemon keeps of each repository that changed. It
//! evaluates nothing ahead of time: a check asked of it runs on what is
//! kept, after the watcher's barrier confirms every earlier change was
//! seen, and an answer is given again only when nothing it read changed.

use std::collections::{BTreeMap, hash_map::DefaultHasher};
use std::fmt::Write as _;
use std::hash::{Hash, Hasher};
use std::os::unix::net::UnixStream;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::mpsc::{self, Receiver, RecvTimeoutError, Sender};
use std::thread::{self, JoinHandle};
use std::time::{Duration, Instant};

use crate::check::{LanguageServers, Served, Streams};
use crate::cli::CheckArgs;

/// How often the worker collects changes reported by the watchers.
const BATCH_INTERVAL: Duration = Duration::from_millis(200);

/// A check asked of sentry, with the connection to answer on.
pub(crate) struct Ask {
    pub question: Question,
    pub stream: UnixStream,
}

/// What a check asked of sentry asks.
pub(crate) struct Question {
    pub args: CheckArgs,
    pub json: bool,
    /// The command line, for telling an identical ask from another.
    pub raw: Vec<String>,
    pub cwd: PathBuf,
    pub environment: BTreeMap<String, String>,
}

/// What the worker answers with.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct Answer {
    pub stdout: String,
    pub stderr: String,
    pub exit_code: u8,
}

enum Message {
    Ask(Box<Ask>),
    Status(Sender<String>),
}

/// Sentry attached to one workspace, served by its worker thread.
pub(crate) struct Sentry {
    manifest: PathBuf,
    sender: Option<Sender<Message>>,
    worker: Option<JoinHandle<()>>,
}

impl Sentry {
    /// Attaches to the workspace whose manifest is `manifest` (absolute):
    /// starts watching its repositories before the first check reads them,
    /// so no change made after a read goes unseen.
    pub(crate) fn attach(manifest: &Path) -> anyhow::Result<Self> {
        let manifest = manifest.canonicalize()?;
        let (sender, receiver) = mpsc::channel();
        let (ready, started) = mpsc::channel();
        let watched = manifest.clone();
        let worker = thread::Builder::new()
            .name("sentry".to_owned())
            .spawn(move || match Worker::start(watched) {
                Ok(mut worker) => {
                    let _ = ready.send(Ok(()));
                    worker.serve(&receiver);
                }
                Err(error) => {
                    let _ = ready.send(Err(error));
                }
            })?;
        started
            .recv()
            .map_err(|_| anyhow::anyhow!("sentry stopped while starting"))??;
        Ok(Self {
            manifest,
            sender: Some(sender),
            worker: Some(worker),
        })
    }

    /// The manifest of the workspace it is attached to.
    pub(crate) fn manifest(&self) -> &Path {
        &self.manifest
    }

    /// Whether the workspace is gone, such as a worktree deleted after use.
    pub(crate) fn is_gone(&self) -> bool {
        !self.manifest.exists()
    }

    /// Hands a check to the worker, which answers on its connection.
    pub(crate) fn ask(&self, ask: Ask) {
        if let Some(sender) = &self.sender {
            let _ = sender.send(Message::Ask(Box::new(ask)));
        }
    }

    /// What it watches and has answered, a line each.
    pub(crate) fn status(&self) -> String {
        let (reply, answer) = mpsc::channel();
        let asked = self
            .sender
            .as_ref()
            .is_some_and(|sender| sender.send(Message::Status(reply)).is_ok());
        let detail = asked
            .then(|| answer.recv_timeout(Duration::from_secs(5)).ok())
            .flatten()
            .unwrap_or_else(|| "busy\n".to_owned());
        format!("sentry attached to {}\n{detail}", self.manifest.display())
    }
}

impl Drop for Sentry {
    fn drop(&mut self) {
        // Closing the channel stops the worker once its current ask is
        // answered.
        self.sender = None;
        if let Some(worker) = self.worker.take() {
            let _ = worker.join();
        }
    }
}

/// A repository sentry watches.
struct Watched {
    root: PathBuf,
    watch: Option<lemma_index::Watch>,
    /// Why it is not watched, when watching failed.
    failure: Option<String>,
    /// Bumped whenever its files may have changed.
    generation: u64,
}

/// How much sentry has done, for status.
#[derive(Default)]
struct Counts {
    batches: u64,
    evaluated: u64,
    reused: u64,
    last: Option<(Duration, bool)>,
}

struct Worker {
    manifest: PathBuf,
    watched: Vec<Watched>,
    warm: Arc<lemma_graph::Warm>,
    /// The last answer, and the fingerprint of what it read.
    last: Option<(u64, Answer)>,
    counts: Counts,
}

impl Worker {
    fn start(manifest: PathBuf) -> anyhow::Result<Self> {
        let options = lemma_graph::CheckOptions {
            change: None,
            level: None,
            cache: lemma_graph::repos::cache_directory(),
            facts_cache: None,
            evaluation: lemma_verify::Evaluation::Demand,
            list_links: false,
            run_checkers: false,
            runs_base_checkers: false,
            select: None,
            warm: None,
            environment: None,
        };
        let roots: Vec<PathBuf> = lemma_graph::repository_roots(&manifest, &options)?
            .into_iter()
            .map(|root| root.canonicalize().unwrap_or(root))
            .collect();
        let watched = roots
            .iter()
            .map(|root| {
                // A workspace's directory holds its repositories, each
                // watched on its own.
                let inner: Vec<PathBuf> = roots
                    .iter()
                    .filter(|other| *other != root && other.starts_with(root))
                    .cloned()
                    .collect();
                watch(root.clone(), &inner)
            })
            .collect();
        Ok(Self {
            manifest,
            watched,
            warm: Arc::default(),
            last: None,
            counts: Counts::default(),
        })
    }

    fn serve(&mut self, receiver: &Receiver<Message>) {
        loop {
            match receiver.recv_timeout(BATCH_INTERVAL) {
                Ok(Message::Ask(ask)) => {
                    let Ask { question, stream } = *ask;
                    crate::daemon::reply_answer(stream, &self.respond(&question));
                }
                Ok(Message::Status(reply)) => {
                    let _ = reply.send(self.status());
                }
                Err(RecvTimeoutError::Timeout) => self.collect(false),
                Err(RecvTimeoutError::Disconnected) => return,
            }
        }
    }

    /// Drops what is kept of each repository whose files changed. On an
    /// ask, `confirming` waits for every watcher's barrier, so a change
    /// made just before the ask counts; between asks only repositories with
    /// reported changes are looked at.
    fn collect(&mut self, confirming: bool) {
        for watched in &mut self.watched {
            let Some(watch) = &mut watched.watch else {
                // Unwatched: anything may have changed.
                self.warm.forget_working(&watched.root);
                watched.generation += 1;
                continue;
            };
            if !confirming && !watch.has_pending() {
                continue;
            }
            let observed = watch.changes();
            if observed.needs_full_walk || !observed.paths.is_empty() {
                self.warm.forget_working(&watched.root);
                watched.generation += 1;
                self.counts.batches += 1;
            }
        }
    }

    /// The answer to `question`: the last one again when nothing it read
    /// has changed since, otherwise a check on what is kept.
    fn respond(&mut self, question: &Question) -> Answer {
        let started = Instant::now();
        self.collect(true);
        let fingerprint = self.fingerprint(question);
        let kept = self
            .last
            .as_ref()
            .filter(|(seen, _)| *seen == fingerprint)
            .map(|(_, answer)| answer.clone());
        let reused = kept.is_some();
        let answer = kept.unwrap_or_else(|| self.evaluate(question));
        if reused {
            self.counts.reused += 1;
        } else {
            self.last = Some((fingerprint, answer.clone()));
            self.counts.evaluated += 1;
        }
        self.counts.last = Some((started.elapsed(), reused));
        answer
    }

    fn evaluate(&self, ask: &Question) -> Answer {
        let manifest = crate::check::entry_manifest_in(&ask.cwd, &ask.args.manifest);
        let served = Served {
            manifest: &manifest,
            warm: Arc::clone(&self.warm),
            environment: Arc::new(ask.environment.clone()),
        };
        let lsp = LanguageServers {
            asks_daemon: false,
            providers_file: None,
        };
        let (mut out, mut err) = (Vec::new(), Vec::new());
        let mut streams = Streams {
            out: &mut out,
            err: &mut err,
        };
        let exit_code =
            crate::check::answer(&ask.args, &lsp, ask.json, Some(&served), &mut streams);
        Answer {
            stdout: String::from_utf8_lossy(&out).into_owned(),
            stderr: String::from_utf8_lossy(&err).into_owned(),
            exit_code,
        }
    }

    /// What an answer rests on besides the files watched: the command, its
    /// directory and environment, each repository's generation, the
    /// workspace's own manifests, and the date (allowances expire).
    fn fingerprint(&self, ask: &Question) -> u64 {
        let mut hasher = DefaultHasher::new();
        ask.raw.hash(&mut hasher);
        ask.cwd.hash(&mut hasher);
        ask.environment.hash(&mut hasher);
        for watched in &self.watched {
            watched.generation.hash(&mut hasher);
        }
        manifest_stamp(&self.manifest).hash(&mut hasher);
        today().hash(&mut hasher);
        hasher.finish()
    }

    fn status(&self) -> String {
        let mut text = String::new();
        for watched in &self.watched {
            let line = match (&watched.watch, &watched.failure) {
                (Some(watch), _) => match watch.report() {
                    lemma_index::WatchReport::Watching {
                        directories: Some(count),
                    } => format!("  watching {}: {count} directories", watched.root.display()),
                    lemma_index::WatchReport::Watching { directories: None } => {
                        format!("  watching {}", watched.root.display())
                    }
                    lemma_index::WatchReport::Walking { reason } => {
                        format!("  not watching {}: {reason}", watched.root.display())
                    }
                },
                (None, failure) => format!(
                    "  not watching {}: {}",
                    watched.root.display(),
                    failure.as_deref().unwrap_or("unknown")
                ),
            };
            text.push_str(&line);
            text.push('\n');
        }
        let counts = &self.counts;
        let last = counts.last.map_or_else(String::new, |(took, reused)| {
            let how = if reused { "reused" } else { "checked" };
            format!("; last {how} in {:.2}s", took.as_secs_f64())
        });
        let _ = writeln!(
            text,
            "  {} checks answered ({} reused){last}; {} batches of changes",
            counts.evaluated + counts.reused,
            counts.reused,
            counts.batches
        );
        text
    }
}

/// A watcher for the repository at `root` (canonical), leaving out the
/// repositories `inner` to it, or why there is none.
fn watch(root: PathBuf, inner: &[PathBuf]) -> Watched {
    let selection = lemma_index::Selection::everything();
    let started = lemma_index::Watch::start_without(&root, &selection, inner);
    let (watch, failure) = match started {
        Ok(watch) => (Some(watch), None),
        Err(reason) => {
            log::warn!("sentry: not watching {}: {reason}", root.display());
            (None, Some(reason))
        }
    };
    Watched {
        root,
        watch,
        failure,
        generation: 0,
    }
}

/// The size and modification time of the workspace's own manifests, which
/// sit beside, not inside, the repositories watched.
fn manifest_stamp(manifest: &Path) -> Vec<(u64, u128)> {
    let directory = manifest.parent().unwrap_or(manifest);
    ["lemma.yaml", "mod.lm"]
        .iter()
        .map(|name| {
            std::fs::metadata(directory.join(name)).map_or((0, 0), |metadata| {
                let modified = metadata
                    .modified()
                    .ok()
                    .and_then(|time| time.duration_since(std::time::UNIX_EPOCH).ok())
                    .map_or(0, |elapsed| elapsed.as_nanos());
                (metadata.len(), modified)
            })
        })
        .collect()
}

/// Days since the Unix epoch.
fn today() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0, |elapsed| elapsed.as_secs() / 86_400)
}

/// Asks the running daemon's sentry to answer a check, given its command
/// line. `None` when it cannot: no daemon runs (none is started for this),
/// sentry is turned off or attached elsewhere, or the command asks for
/// logs, which only a check run here can print.
pub(crate) fn ask(raw: Vec<String>) -> Option<Answer> {
    if is_turned_off()
        || raw
            .iter()
            .any(|arg| arg == "-l" || arg.starts_with("--logging"))
    {
        return None;
    }
    let cwd = std::env::current_dir().ok()?;
    let environment: BTreeMap<String, String> = std::env::vars().collect();
    crate::daemon::ask_sentry(raw, cwd, environment)
}

/// Whether `LEMMA_SENTRY=off` turns sentry off for this process.
pub(crate) fn is_turned_off() -> bool {
    std::env::var("LEMMA_SENTRY").is_ok_and(|value| value == "off")
}

#[cfg(test)]
mod tests {
    use super::*;

    fn git(directory: &Path, args: &[&str]) {
        let status = std::process::Command::new("git")
            .arg("-C")
            .arg(directory)
            .args([
                "-c",
                "user.name=t",
                "-c",
                "user.email=t@t",
                "-c",
                "commit.gpgsign=false",
            ])
            .args(args)
            .status()
            .unwrap();
        assert!(status.success(), "git {args:?}");
    }

    fn write(path: &Path, text: &str) {
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(path, text).unwrap();
    }

    const SPEC: &str = r#"{ "openapi": "3.0.0", "info": { "title": "B", "version": "1" }, "paths": { "/v1/charges": { "post": { "operationId": "createCharge", "responses": { "200": { "description": "ok" } } } } } }"#;

    /// Billing, whose operation web calls, under `root`, committed.
    fn shop(root: &Path) {
        let billing = root.join("billing");
        write(&billing.join("openapi.json"), SPEC);
        write(
            &billing.join("mod.lm"),
            "module billing\n\nimport * as http from lemma.http\n\nexport service api {\n    operations: http.openapi(\"openapi.json\")\n}\n",
        );
        git(&billing, &["init", "-q"]);
        git(&billing, &["add", "."]);
        git(&billing, &["commit", "-q", "-m", "billing"]);
        let web = root.join("web");
        write(
            &web.join("src/charge.ts"),
            "export async function charge() {\n  return fetch(\"/v1/charges\", { method: \"POST\" });\n}\n",
        );
        write(
            &web.join("mod.lm"),
            "module web\n\nexport service web {\n    index: { include: [\"src\"] }\n}\n",
        );
        git(&web, &["init", "-q"]);
        git(&web, &["add", "."]);
        git(&web, &["commit", "-q", "-m", "web"]);
        write(
            &root.join("mod.lm"),
            "module shop\n\nworkspace {\n    repo billing = local(\"billing\")\n    repo web = local(\"web\")\n    config: { level: check }\n}\n",
        );
    }

    fn question(root: &Path) -> Question {
        let raw: Vec<String> = ["check", "--change", "billing"].map(str::to_owned).to_vec();
        let words = std::iter::once("lemma".to_owned()).chain(raw.iter().cloned());
        let cli = <crate::cli::Cli as clap::Parser>::try_parse_from(words).unwrap();
        let crate::cli::Command::Check(args) = cli.command else {
            panic!("not a check");
        };
        Question {
            args,
            json: false,
            raw,
            cwd: root.to_owned(),
            environment: std::env::vars().collect(),
        }
    }

    /// The same check with nothing kept: what a fresh check answers.
    fn fresh(question: &Question) -> Answer {
        let manifest = crate::check::entry_manifest_in(&question.cwd, &question.args.manifest);
        let served = Served {
            manifest: &manifest,
            warm: Arc::default(),
            environment: Arc::new(question.environment.clone()),
        };
        let lsp = LanguageServers {
            asks_daemon: false,
            providers_file: None,
        };
        let (mut out, mut err) = (Vec::new(), Vec::new());
        let mut streams = Streams {
            out: &mut out,
            err: &mut err,
        };
        let exit_code =
            crate::check::answer(&question.args, &lsp, false, Some(&served), &mut streams);
        Answer {
            stdout: String::from_utf8_lossy(&out).into_owned(),
            stderr: String::from_utf8_lossy(&err).into_owned(),
            exit_code,
        }
    }

    #[test]
    fn sentry_answers_as_a_fresh_check_reusing_an_answer_only_while_nothing_changes() {
        let root = tempfile::tempdir().unwrap();
        shop(root.path());
        let mut worker = Worker::start(root.path().join("mod.lm").canonicalize().unwrap()).unwrap();
        let asked = question(root.path());

        let first = worker.respond(&asked);
        let fresh_first = fresh(&asked);
        let again = worker.respond(&asked);
        let removed = SPEC.replace(r#""/v1/charges""#, r#""/v1/payments""#);
        write(&root.path().join("billing/openapi.json"), &removed);
        let after_edit = worker.respond(&asked);
        let fresh_after_edit = fresh(&asked);

        assert_eq!(first, fresh_first);
        assert_eq!(again, first);
        assert_eq!(after_edit, fresh_after_edit);
        assert_ne!(after_edit, first);
        assert_eq!((worker.counts.evaluated, worker.counts.reused), (2, 1));
    }
}
