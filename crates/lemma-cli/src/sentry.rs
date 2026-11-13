//! Sentry: the daemon's attachment to one workspace (`.specs/14-sentry.md`).
//!
//! A worker thread watches the workspace's repositories, and in small
//! batches drops what the daemon keeps of each repository that changed. It
//! evaluates nothing ahead of time: a check asked of it runs on what is
//! kept, after the watcher's barrier confirms every earlier change was
//! seen, and an answer is given again only when nothing it read changed.
//!
//! Every answer and batch has a place in a journal, named by a token: a
//! check `--since` a token lists what came and went since that answer, and
//! `lemma daemon sentry events` lists what happened since a token.

use std::collections::{BTreeMap, BTreeSet, VecDeque, hash_map::DefaultHasher};
use std::fmt::Write as _;
use std::hash::{Hash, Hasher};
use std::os::unix::net::UnixStream;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::mpsc::{self, Receiver, RecvTimeoutError, Sender};
use std::thread::{self, JoinHandle};
use std::time::{Duration, Instant};

use lemma_graph::{Finding, Report, Severity};

use crate::check::{Delta, SentryNote, Served, Streams};
use crate::cli::CheckArgs;
use crate::watched::{BATCH_INTERVAL, Watched, WatchedWorkspace};

/// How many events the journal keeps.
const JOURNAL_LIMIT: usize = 1000;
/// How many earlier answers it keeps per set of options, for `--since`.
const ANSWERS_KEPT: usize = 8;
/// How many answers it holds ready to give again. Each set of options has
/// its own, so a check of one repository does not displace another's.
const ANSWERS_HELD: usize = 16;

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

/// A wait for events, with the connection to answer on.
pub(crate) struct Wait {
    pub since: Option<String>,
    pub limit: Duration,
    pub json: bool,
    pub stream: UnixStream,
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
    Events(Box<Wait>),
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
            .spawn(move || match Worker::start(&watched) {
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
        self.send(Message::Ask(Box::new(ask)));
    }

    /// Hands a wait for events to the worker, which answers on its
    /// connection when there are some or the wait is over.
    pub(crate) fn events(&self, wait: Wait) {
        self.send(Message::Events(Box::new(wait)));
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

    fn send(&self, message: Message) {
        if let Some(sender) = &self.sender {
            let _ = sender.send(message);
        }
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

/// How much sentry has done, for status.
#[derive(Default)]
struct Counts {
    batches: u64,
    prepared: u64,
    read_ahead: usize,
    evaluated: u64,
    reused: u64,
    last: Option<(Duration, bool)>,
}

/// Something that happened, kept in the journal under its position.
#[derive(Debug, Clone, serde::Serialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
enum Event {
    /// A repository's files changed, so what was kept of it was dropped.
    Batch { repository: String, paths: usize },
    /// A check was answered.
    Answer {
        command: String,
        errors: usize,
        warnings: usize,
        informational: usize,
        reused: bool,
        seconds: f64,
    },
}

/// An answer held ready: its place in the journal, when it was last given,
/// the interpreters it read, the changes to unread files it has seen, and
/// the report, or the error the check stopped with.
struct Kept {
    position: u64,
    last_given: u64,
    interpreters: BTreeMap<String, String>,
    unread_generations: u64,
    report: Result<Arc<Report>, String>,
}

impl Kept {
    /// Whether it still holds, beyond the fingerprint it is held under.
    fn holds(&self, unread_generations: u64) -> bool {
        let ran_behavior = self
            .report
            .as_ref()
            .is_ok_and(|report| !report.behaviors.is_empty());
        lemma_graph::Warm::stamps_hold(&self.interpreters)
            && (!ran_behavior || self.unread_generations == unread_generations)
    }
}

/// One ask being answered: what was asked, the options that name it, and
/// the fingerprint of what its answer rests on.
struct Asked<'a> {
    question: &'a Question,
    options: String,
    fingerprint: u64,
}

/// The connection of a wait for events, and what it waits for.
struct Waiting {
    since: Option<u64>,
    is_reset: bool,
    deadline: Instant,
    json: bool,
    stream: UnixStream,
}

struct Worker {
    workspace: WatchedWorkspace,
    /// Names this attachment in tokens, so a token from another is known.
    instance: String,
    journal: VecDeque<(u64, Event)>,
    /// The last position given out.
    position: u64,
    /// Answers held ready, by the fingerprint of what each rests on.
    held: BTreeMap<u64, Kept>,
    /// Earlier answers by their options, for `--since`.
    answers: BTreeMap<String, VecDeque<(u64, Arc<Report>)>>,
    waiting: Vec<Waiting>,
    counts: Counts,
}

impl Worker {
    fn start(manifest: &Path) -> anyhow::Result<Self> {
        let workspace = WatchedWorkspace::start(manifest)?;
        Ok(Self {
            workspace,
            instance: instance(),
            journal: VecDeque::new(),
            position: 0,
            held: BTreeMap::new(),
            answers: BTreeMap::new(),
            waiting: Vec::new(),
            counts: Counts::default(),
        })
    }

    fn serve(&mut self, receiver: &Receiver<Message>) {
        self.prepare_attached();
        loop {
            match receiver.recv_timeout(BATCH_INTERVAL) {
                Ok(Message::Ask(ask)) => {
                    let Ask { question, stream } = *ask;
                    crate::daemon::reply_answer(stream, &self.respond(&question));
                }
                Ok(Message::Events(wait)) => self.wait(*wait),
                Ok(Message::Status(reply)) => {
                    let _ = reply.send(self.status());
                }
                Err(RecvTimeoutError::Timeout) => {
                    self.collect(false);
                    self.prepare_settled();
                }
                Err(RecvTimeoutError::Disconnected) => return,
            }
            self.serve_waiting();
        }
    }

    /// Lists each repository, so the first ask does not. Nothing else is
    /// known before a check says how its repositories are read.
    fn prepare_attached(&self) {
        let wanted: Vec<(PathBuf, Option<String>)> = self
            .workspace
            .repositories()
            .iter()
            .map(|watched| (watched.spelled().to_path_buf(), None))
            .collect();
        self.workspace.warm().verify().listings().prefetch(&wanted);
    }

    /// Prepares each repository whose changes have settled. Nothing is
    /// evaluated: only listings and the facts of the files that changed.
    fn prepare_settled(&mut self) {
        for settled in self.workspace.take_settled(Instant::now()) {
            let verify = self.workspace.warm().verify();
            self.counts.read_ahead += verify.prepare(&settled.repository, &settled.paths);
            self.counts.prepared += 1;
        }
    }

    /// Drops what is kept of what changed in each repository, and records
    /// each repository's changes as a batch. On an ask, `confirming` waits
    /// for every watcher's barrier, so a change made just before the ask
    /// counts.
    fn collect(&mut self, confirming: bool) {
        for batch in self.workspace.collect(confirming) {
            self.counts.batches += 1;
            self.record(Event::Batch {
                repository: batch.repository,
                paths: batch.paths,
            });
        }
    }

    /// Keeps `event` in the journal; its position.
    fn record(&mut self, event: Event) -> u64 {
        self.position += 1;
        self.journal.push_back((self.position, event));
        while self.journal.len() > JOURNAL_LIMIT {
            self.journal.pop_front();
        }
        self.position
    }

    /// The answer to `question`: the one held for it when nothing it read
    /// has changed since, otherwise a check on what is kept.
    fn respond(&mut self, question: &Question) -> Answer {
        let started = Instant::now();
        self.collect(true);
        let options = options_key(&question.raw);
        let fingerprint = self.fingerprint(question, &options);
        let asked = Asked {
            question,
            options,
            fingerprint,
        };
        let unread_generations: u64 = self
            .workspace
            .repositories()
            .iter()
            .map(Watched::unread_generation)
            .sum();
        let reused = self
            .held
            .get(&fingerprint)
            .is_some_and(|kept| kept.holds(unread_generations));
        if !reused {
            let report = self.evaluate(question).map(Arc::new);
            self.held.insert(
                fingerprint,
                Kept {
                    position: 0,
                    last_given: 0,
                    interpreters: self.workspace.warm().interpreter_stamps(),
                    unread_generations,
                    report,
                },
            );
        }
        self.account(&asked, started.elapsed(), reused);
        self.render(&asked)
    }

    /// Counts the answer and keeps it in the journal; a new answer's token
    /// is its event's place there, and a reused one keeps its token.
    fn account(&mut self, asked: &Asked<'_>, took: Duration, reused: bool) {
        if reused {
            self.counts.reused += 1;
        } else {
            self.counts.evaluated += 1;
        }
        self.counts.last = Some((took, reused));
        let report = self
            .held
            .get(&asked.fingerprint)
            .and_then(|kept| kept.report.as_ref().ok())
            .cloned();
        let count = |severity: Severity| {
            report.as_ref().map_or(0, |report| {
                report
                    .findings
                    .iter()
                    .filter(|finding| finding.severity == severity)
                    .count()
            })
        };
        let event = Event::Answer {
            command: asked.question.raw.join(" "),
            errors: count(Severity::Error),
            warnings: count(Severity::Warning),
            informational: count(Severity::Info),
            reused,
            seconds: took.as_secs_f64(),
        };
        let position = self.record(event);
        if let Some(kept) = self.held.get_mut(&asked.fingerprint) {
            kept.last_given = position;
            if !reused {
                kept.position = position;
            }
        }
        self.hold_fewer();
        if reused {
            return;
        }
        if let Some(report) = report {
            let earlier = self.answers.entry(asked.options.clone()).or_default();
            earlier.push_back((position, report));
            while earlier.len() > ANSWERS_KEPT {
                earlier.pop_front();
            }
        }
    }

    /// Lets go of the answers given longest ago, past [`ANSWERS_HELD`].
    fn hold_fewer(&mut self) {
        while self.held.len() > ANSWERS_HELD {
            let oldest = self
                .held
                .iter()
                .min_by_key(|(_, kept)| kept.last_given)
                .map(|(fingerprint, _)| *fingerprint);
            let Some(oldest) = oldest else {
                return;
            };
            self.held.remove(&oldest);
        }
    }

    /// The answer held for `asked`, printed: with its token, and with what
    /// changed since the answer `--since` names.
    fn render(&self, asked: &Asked<'_>) -> Answer {
        let Some(kept) = self.held.get(&asked.fingerprint) else {
            return failed("sentry has no answer");
        };
        let report = match &kept.report {
            Ok(report) => report,
            Err(error) => return failed(&format!("error: {error}")),
        };
        let token = self.token(kept.position);
        let question = asked.question;
        let delta = question.args.since.as_deref().map(|since| Delta {
            since,
            changes: self
                .earlier(since, &asked.options)
                .map(|earlier| changes(earlier, report)),
        });
        let note = SentryNote {
            token: &token,
            delta,
        };
        let (mut out, mut err) = (Vec::new(), Vec::new());
        let mut streams = Streams {
            out: &mut out,
            err: &mut err,
        };
        let exit_code = crate::check::print(
            report,
            &question.args,
            question.json,
            Some(&note),
            &mut streams,
        );
        Answer {
            stdout: String::from_utf8_lossy(&out).into_owned(),
            stderr: String::from_utf8_lossy(&err).into_owned(),
            exit_code,
        }
    }

    /// The report of the answer `token` names, for the same options, if it
    /// is kept.
    fn earlier(&self, token: &str, options: &str) -> Option<&Report> {
        let position = self.position_of(token)?;
        self.answers
            .get(options)?
            .iter()
            .find(|(kept, _)| *kept == position)
            .map(|(_, report)| report.as_ref())
    }

    fn token(&self, position: u64) -> String {
        format!("{}-{position}", self.instance)
    }

    /// The journal position `token` names, if it is this attachment's.
    fn position_of(&self, token: &str) -> Option<u64> {
        let (instance, position) = token.rsplit_once('-')?;
        if instance != self.instance {
            return None;
        }
        position.parse().ok()
    }

    fn evaluate(&self, question: &Question) -> Result<Report, String> {
        let manifest = crate::check::entry_manifest_in(&question.cwd, &question.args.manifest);
        let served = Served {
            manifest: &manifest,
            warm: Arc::clone(self.workspace.warm()),
            environment: Arc::new(question.environment.clone()),
        };
        crate::check::evaluate(&question.args, Some(&served)).map_err(|error| format!("{error:#}"))
    }

    /// What an answer rests on besides the files watched: the command
    /// (without `--since`), its directory and environment, each
    /// repository's generation, the workspace's own manifests, and the
    /// date (allowances expire).
    fn fingerprint(&self, question: &Question, options: &str) -> u64 {
        let mut hasher = DefaultHasher::new();
        options.hash(&mut hasher);
        question.json.hash(&mut hasher);
        question.cwd.hash(&mut hasher);
        question.environment.hash(&mut hasher);
        self.workspace.state().hash(&mut hasher);
        today().hash(&mut hasher);
        hasher.finish()
    }

    /// Waits for events after `wait.since`, answered at the next turn of
    /// the loop when there are some or the wait is over.
    fn wait(&mut self, wait: Wait) {
        let since = wait.since.as_deref().map(|token| self.position_of(token));
        let is_reset = matches!(since, Some(None));
        self.waiting.push(Waiting {
            since: since.flatten(),
            is_reset,
            deadline: Instant::now() + wait.limit,
            json: wait.json,
            stream: wait.stream,
        });
    }

    /// Answers each wait that has events to see or has waited long enough.
    fn serve_waiting(&mut self) {
        let now = Instant::now();
        let pending = std::mem::take(&mut self.waiting);
        let (ready, waiting): (Vec<Waiting>, Vec<Waiting>) = pending
            .into_iter()
            .partition(|waiting| waiting.deadline <= now || self.has_events_after(waiting.since));
        self.waiting = waiting;
        for waiting in ready {
            let answer = Answer {
                stdout: self.list_events(&waiting),
                stderr: String::new(),
                exit_code: 0,
            };
            crate::daemon::reply_answer(waiting.stream, &answer);
        }
    }

    fn has_events_after(&self, since: Option<u64>) -> bool {
        self.journal
            .back()
            .is_some_and(|(position, _)| since.is_none_or(|since| *position > since))
    }

    /// The events after a wait's token, as text or JSON, with the latest
    /// token.
    fn list_events(&self, waiting: &Waiting) -> String {
        let events: Vec<&(u64, Event)> = self
            .journal
            .iter()
            .filter(|(position, _)| waiting.since.is_none_or(|since| *position > since))
            .collect();
        let latest = self.token(self.position);
        if waiting.json {
            let listed: Vec<serde_json::Value> = events
                .iter()
                .map(|(position, event)| {
                    let mut value = serde_json::to_value(event).unwrap_or_default();
                    value["token"] = serde_json::Value::String(self.token(*position));
                    value
                })
                .collect();
            let body = serde_json::json!({
                "events": listed,
                "token": latest,
                "reset": waiting.is_reset,
            });
            return format!(
                "{}\n",
                serde_json::to_string_pretty(&body).unwrap_or_default()
            );
        }
        let mut text = String::new();
        if waiting.is_reset {
            text.push_str("the token is not this sentry's, so every event kept follows\n");
        }
        for (position, event) in events {
            let _ = writeln!(text, "{} {}", self.token(*position), describe(event));
        }
        let _ = writeln!(text, "sentry token: {latest}");
        text
    }

    fn status(&self) -> String {
        let mut text = String::new();
        for watched in self.workspace.repositories() {
            let _ = writeln!(text, "  {}", watched.status_line());
        }
        let counts = &self.counts;
        let last = counts.last.map_or_else(String::new, |(took, reused)| {
            let how = if reused { "reused" } else { "checked" };
            format!("; last {how} in {:.2}s", took.as_secs_f64())
        });
        let _ = writeln!(
            text,
            "  {} checks answered ({} reused){last}; {} batches of changes, {} prepared with {} \
             files read ahead; token {}",
            counts.evaluated + counts.reused,
            counts.reused,
            counts.batches,
            counts.prepared,
            counts.read_ahead,
            self.token(self.position)
        );
        text
    }
}

/// A failed answer, saying why.
fn failed(message: &str) -> Answer {
    Answer {
        stdout: String::new(),
        stderr: format!("{message}\n"),
        exit_code: 1,
    }
}

/// What tells two findings apart for a delta.
type FindingKey = (Severity, String, String, String, bool);

fn finding_key(finding: &Finding) -> FindingKey {
    (
        finding.severity,
        finding.link.clone(),
        finding.check.clone(),
        finding.message.clone(),
        finding.introduced,
    )
}

/// The findings in `now` not in `earlier`, then those in `earlier` gone
/// from `now`, told apart by severity, link, requirement, message and
/// whether the change introduced them.
fn changes<'a>(earlier: &'a Report, now: &'a Report) -> (Vec<&'a Finding>, Vec<&'a Finding>) {
    let before: BTreeSet<FindingKey> = earlier.findings.iter().map(finding_key).collect();
    let after: BTreeSet<FindingKey> = now.findings.iter().map(finding_key).collect();
    let new = now
        .findings
        .iter()
        .filter(|finding| !before.contains(&finding_key(finding)))
        .collect();
    let gone = earlier
        .findings
        .iter()
        .filter(|finding| !after.contains(&finding_key(finding)))
        .collect();
    (new, gone)
}

/// The command line without `--since` and its token: answers to it with
/// any token are one set of options.
fn options_key(raw: &[String]) -> String {
    let mut words = Vec::new();
    let mut skips_next = false;
    for word in raw {
        if skips_next {
            skips_next = false;
            continue;
        }
        if word == "--since" {
            skips_next = true;
            continue;
        }
        if !word.starts_with("--since=") {
            words.push(word.as_str());
        }
    }
    words.join("\u{0}")
}

fn describe(event: &Event) -> String {
    match event {
        Event::Batch { repository, paths } => format!("changed {repository} ({paths} paths)"),
        Event::Answer {
            command,
            errors,
            warnings,
            informational,
            reused,
            seconds,
        } => {
            let how = if *reused { "reused" } else { "checked" };
            format!(
                "lemma {command}: {errors} errors, {warnings} warnings, {informational} \
                 informational ({how} in {seconds:.2}s)"
            )
        }
    }
}

/// A name for this attachment, different each time sentry attaches.
fn instance() -> String {
    let started = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0, |elapsed| elapsed.as_nanos());
    let mut hasher = DefaultHasher::new();
    (started, std::process::id()).hash(&mut hasher);
    format!("{:08x}", hasher.finish() & 0xffff_ffff)
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
        let (mut out, mut err) = (Vec::new(), Vec::new());
        let mut streams = Streams {
            out: &mut out,
            err: &mut err,
        };
        let exit_code = crate::check::answer(&question.args, false, Some(&served), &mut streams);
        Answer {
            stdout: String::from_utf8_lossy(&out).into_owned(),
            stderr: String::from_utf8_lossy(&err).into_owned(),
            exit_code,
        }
    }

    /// The answer without the token line sentry adds under a report.
    fn without_token(answer: &Answer) -> Answer {
        let mut stdout = String::new();
        for line in answer.stdout.lines() {
            if !line.starts_with("sentry token: ") {
                stdout.push_str(line);
                stdout.push('\n');
            }
        }
        Answer {
            stdout,
            ..answer.clone()
        }
    }

    fn token_of(answer: &Answer) -> String {
        answer
            .stdout
            .lines()
            .find_map(|line| line.strip_prefix("sentry token: "))
            .unwrap()
            .to_owned()
    }

    #[test]
    fn sentry_answers_as_a_fresh_check_reusing_an_answer_only_while_nothing_changes() {
        let root = tempfile::tempdir().unwrap();
        shop(root.path());
        let mut worker =
            Worker::start(&root.path().join("mod.lm").canonicalize().unwrap()).unwrap();
        let asked = question(root.path());

        let first = worker.respond(&asked);
        let fresh_first = fresh(&asked);
        let again = worker.respond(&asked);
        let removed = SPEC.replace(r#""/v1/charges""#, r#""/v1/payments""#);
        write(&root.path().join("billing/openapi.json"), &removed);
        let after_edit = worker.respond(&asked);
        let fresh_after_edit = fresh(&asked);

        assert_eq!(without_token(&first), fresh_first);
        assert_eq!(again, first);
        assert_eq!(without_token(&after_edit), fresh_after_edit);
        assert_ne!(after_edit, first);
        assert_eq!((worker.counts.evaluated, worker.counts.reused), (2, 1));
    }

    #[test]
    fn a_check_since_a_token_lists_what_came_and_went() {
        let root = tempfile::tempdir().unwrap();
        shop(root.path());
        let mut worker =
            Worker::start(&root.path().join("mod.lm").canonicalize().unwrap()).unwrap();
        let asked = question(root.path());
        let token = token_of(&worker.respond(&asked));

        let removed = SPEC.replace(r#""/v1/charges""#, r#""/v1/payments""#);
        write(&root.path().join("billing/openapi.json"), &removed);
        let mut since = question(root.path());
        since.args.since = Some(token.clone());
        since.raw.extend(["--since".to_owned(), token.clone()]);
        let delta = worker.respond(&since);
        let waiting = Waiting {
            since: worker.position_of(&token),
            is_reset: false,
            deadline: Instant::now(),
            json: false,
            stream: UnixStream::pair().unwrap().0,
        };
        let events = worker.list_events(&waiting);

        assert!(
            delta
                .stdout
                .starts_with(&format!("since {token}: 2 new, 0 gone\n"))
        );
        assert!(delta.stdout.contains("new  error   #web"));
        assert_eq!(delta.exit_code, 4);
        assert!(events.contains("changed "), "{events}");
        assert!(
            events.contains("lemma check --change billing --since"),
            "{events}"
        );
    }

    /// `billing` with a Python operation read through the interpreter at
    /// `.venv/bin/python`, which `web` links to.
    fn python_shop(root: &Path, handlers: &str) {
        let billing = root.join("billing");
        write(&billing.join(".gitignore"), ".venv/\n");
        write(
            &billing.join(handlers),
            "class Req:\n    id: str\n\ndef charge(r: Req) -> Req:\n    return r\n",
        );
        write(
            &billing.join("mod.lm"),
            &format!(
                "module billing\n\nimport * as python from lemma.python\n\nlet handlers = python.file(\"{handlers}\")\n\nexport service api {{\n    runtime: {{ python: \".venv/bin/python\" }}\n    index: {{ include: [\"{handlers}\"] }}\n\n    export operation charge {{\n        implementation: handlers.symbol(\"charge\")\n        request: handlers.type(\"Req\")\n        response: handlers.type(\"Req\")\n    }}\n}}\n"
            ),
        );
        let interpreter = billing.join(".venv/bin/python");
        // One that cannot import Pydantic, so types are read by Tree-sitter.
        write(&interpreter, "#!/bin/sh\nexit 1\n");
        std::fs::set_permissions(
            &interpreter,
            std::os::unix::fs::PermissionsExt::from_mode(0o755),
        )
        .unwrap();
        write(&billing.join(".venv/pyvenv.cfg"), "version = 3\n");
        std::fs::create_dir_all(billing.join(".venv/lib/python3/site-packages")).unwrap();
        git(&billing, &["init", "-q"]);
        git(&billing, &["add", "."]);
        git(&billing, &["commit", "-q", "-m", "billing"]);
        let web = root.join("web");
        write(
            &web.join("c.ts"),
            "export type Req = { id: string }\nexport function call(r: Req): Req { return r }\n",
        );
        write(
            &web.join("mod.lm"),
            "module web\n\nimport * as typescript from lemma.typescript\nimport { Link } from lemma.links\nimport { api as billing_api } from billing\n\nrepo billing = local(\"../billing\")\n\nlet client = typescript.file(\"c.ts\")\n\nexport service web {\n    index: { include: [\"c.ts\"] }\n\n    export client operation charge {\n        implementation: client.symbol(\"call\")\n        sends: client.type(\"Req\")\n        accepts: client.type(\"Req\")\n    }\n}\n\nexport graph links : Link {\n    web.charge -> billing_api.charge\n}\n",
        );
        git(&web, &["init", "-q"]);
        git(&web, &["add", "."]);
        git(&web, &["commit", "-q", "-m", "web"]);
        write(
            &root.join("mod.lm"),
            "module shop\n\nworkspace {\n    repo billing = local(\"billing\")\n    repo web = local(\"web\")\n    config: { level: check }\n}\n",
        );
    }

    #[test]
    fn an_installed_package_keeps_sentry_from_giving_an_answer_again() {
        let root = tempfile::tempdir().unwrap();
        python_shop(root.path(), "api.py");
        let mut worker =
            Worker::start(&root.path().join("mod.lm").canonicalize().unwrap()).unwrap();
        let asked = question(root.path());

        let first = worker.respond(&asked);
        worker.respond(&asked);
        // Modification times are only as fine as the file system's clock.
        std::thread::sleep(Duration::from_millis(20));
        std::fs::create_dir(
            root.path()
                .join("billing/.venv/lib/python3/site-packages/added"),
        )
        .unwrap();
        worker.respond(&asked);

        assert_eq!(first.exit_code, 0, "{first:?}");
        assert_eq!((worker.counts.evaluated, worker.counts.reused), (2, 1));
    }

    #[test]
    fn each_set_of_options_keeps_its_own_answer() {
        let root = tempfile::tempdir().unwrap();
        shop(root.path());
        let mut worker =
            Worker::start(&root.path().join("mod.lm").canonicalize().unwrap()).unwrap();
        let billing = question(root.path());
        let mut web = question(root.path());
        web.args.change = Some("web".to_owned());
        web.raw = ["check", "--change", "web"].map(str::to_owned).to_vec();

        let first = worker.respond(&billing);
        worker.respond(&web);
        let again = worker.respond(&billing);
        worker.respond(&web);

        assert_eq!(again, first);
        assert_eq!((worker.counts.evaluated, worker.counts.reused), (2, 2));
    }

    #[test]
    fn an_edit_to_a_file_no_check_reads_gives_the_answer_again() {
        let root = tempfile::tempdir().unwrap();
        shop(root.path());
        let mut worker =
            Worker::start(&root.path().join("mod.lm").canonicalize().unwrap()).unwrap();
        let asked = question(root.path());
        let first = worker.respond(&asked);

        write(&root.path().join("web/src/charge.test.ts"), "// a test\n");
        write(&root.path().join("web/README.md"), "# web\n");
        let again = worker.respond(&asked);

        assert_eq!(again, first);
        assert_eq!(without_token(&again), fresh(&asked));
        assert_eq!((worker.counts.evaluated, worker.counts.reused), (1, 1));
    }

    #[test]
    fn a_test_file_a_description_names_is_still_read() {
        let root = tempfile::tempdir().unwrap();
        python_shop(root.path(), "src/tests/handlers.py");
        let mut worker =
            Worker::start(&root.path().join("mod.lm").canonicalize().unwrap()).unwrap();
        let asked = question(root.path());
        worker.respond(&asked);

        write(
            &root.path().join("billing/src/tests/handlers.py"),
            "class Req:\n    id: int\n\ndef charge(r: Req) -> Req:\n    return r\n",
        );
        let after_edit = worker.respond(&asked);

        assert_eq!(worker.counts.evaluated, 2);
        assert_eq!(without_token(&after_edit), fresh(&asked));
    }

    /// What a seeded sequence of changes does to a workspace: the things a
    /// developer or an agent does between two checks.
    struct Changes {
        root: PathBuf,
        state: u64,
        written: u64,
    }

    const DOCUMENTS: [&str; 3] = ["/v1/charges", "/v1/payments", "/v2/charges"];

    fn document(path: &str) -> String {
        SPEC.replace("/v1/charges", path)
    }

    impl Changes {
        /// A small generator of its own, so a sequence is the same on
        /// every run and machine.
        fn next(&mut self, limit: u64) -> u64 {
            self.state ^= self.state << 13;
            self.state ^= self.state >> 7;
            self.state ^= self.state << 17;
            self.state % limit
        }

        fn path(&self, relative: &str) -> PathBuf {
            self.root.join(relative)
        }

        fn call(&mut self) -> String {
            let path = DOCUMENTS[usize::try_from(self.next(3)).unwrap()];
            format!(
                "export async function charge() {{\n  return fetch(\"{path}\", {{ method: \"POST\" }});\n}}\n"
            )
        }

        fn change_document(&mut self, relative: &str) -> String {
            let path = DOCUMENTS[usize::try_from(self.next(3)).unwrap()];
            write(&self.path(relative), &document(path));
            format!("{relative} serves {path}")
        }

        fn change_call(&mut self) -> String {
            let call = self.call();
            write(&self.path("web/src/charge.ts"), &call);
            "edited a call".to_owned()
        }

        fn add_file(&mut self) -> String {
            self.written += 1;
            let call = self.call();
            let name = format!("web/src/generated/extra{}.ts", self.written);
            write(&self.path(&name), &call);
            format!("added {name}")
        }

        fn remove_directory(&mut self) -> String {
            let _ = std::fs::remove_dir_all(self.path("web/src/generated"));
            "removed a directory".to_owned()
        }

        fn rename_directory(&mut self) -> String {
            let (from, to) = (self.path("web/src/generated"), self.path("web/src/moved"));
            let _ = std::fs::remove_dir_all(&to);
            let _ = std::fs::rename(from, to);
            "renamed a directory".to_owned()
        }

        fn commit(&mut self, repository: &str) -> String {
            let directory = self.path(repository);
            git(&directory, &["add", "-A"]);
            git(&directory, &["commit", "-q", "--allow-empty", "-m", "step"]);
            format!("committed {repository}")
        }

        fn check_out_first_commit(&mut self, repository: &str) -> String {
            let directory = self.path(repository);
            let first = std::process::Command::new("git")
                .arg("-C")
                .arg(&directory)
                .args(["rev-list", "--max-parents=0", "HEAD"])
                .output()
                .unwrap();
            let first = String::from_utf8(first.stdout).unwrap();
            git(&directory, &["checkout", "-q", "-f", first.trim()]);
            format!("checked out {repository}'s first commit")
        }

        fn change_include(&mut self) -> String {
            let include = if self.next(2) == 0 {
                "src"
            } else {
                "src/charge.ts"
            };
            write(
                &self.path("web/mod.lm"),
                &format!(
                    "module web\n\nexport service web {{\n    index: {{ include: [\"{include}\"] }}\n}}\n"
                ),
            );
            format!("included {include}")
        }

        fn edit_test_file(&mut self) -> String {
            let call = self.call();
            write(&self.path("web/src/charge.test.ts"), &call);
            "edited a test file".to_owned()
        }

        fn edit_prose(&mut self) -> String {
            self.written += 1;
            write(
                &self.path("web/README.md"),
                &format!("# {}\n", self.written),
            );
            "edited prose".to_owned()
        }

        fn apply(&mut self) -> String {
            match self.next(13) {
                0 | 1 => self.change_document("billing/openapi.json"),
                2 => self.change_document("billing/.config/extra.json"),
                3 | 4 => self.change_call(),
                5 => self.add_file(),
                6 => self.remove_directory(),
                7 => self.rename_directory(),
                8 => self.commit("web"),
                9 => self.check_out_first_commit("billing"),
                10 => self.edit_test_file(),
                11 => self.edit_prose(),
                _ => self.change_include(),
            }
        }
    }

    /// `shop`, with a second document under a hidden directory.
    fn shop_with_hidden_document(root: &Path) {
        shop(root);
        let billing = root.join("billing");
        write(
            &billing.join(".config/extra.json"),
            &document("/v2/charges"),
        );
        write(
            &billing.join("mod.lm"),
            "module billing\n\nimport * as http from lemma.http\n\nexport service api {\n    operations: [http.openapi(\"openapi.json\"), http.openapi(\".config/extra.json\")]\n}\n",
        );
        git(&billing, &["add", "-A"]);
        git(&billing, &["commit", "-q", "-m", "second document"]);
    }

    /// Lets the worker see its changes and prepare them, as it does between
    /// asks.
    fn settle(worker: &mut Worker) {
        for _ in 0..3 {
            worker.collect(false);
            std::thread::sleep(BATCH_INTERVAL);
            worker.prepare_settled();
        }
    }

    /// Runs the worker's turns until it has read a file ahead, for a
    /// busy machine's late reports; gives up after ten seconds.
    fn settle_until_read_ahead(worker: &mut Worker) {
        let deadline = Instant::now() + Duration::from_secs(10);
        while worker.counts.read_ahead == 0 && Instant::now() < deadline {
            settle(worker);
        }
    }

    #[test]
    fn a_settled_change_is_parsed_before_it_is_asked_about() {
        let root = tempfile::tempdir().unwrap();
        shop(root.path());
        let mut worker =
            Worker::start(&root.path().join("mod.lm").canonicalize().unwrap()).unwrap();
        let asked = question(root.path());
        worker.respond(&asked);

        write(
            &root.path().join("web/src/charge.ts"),
            "export async function charge() {\n  return fetch(\"/v1/payments\", { method: \"POST\" });\n}\n",
        );
        settle_until_read_ahead(&mut worker);
        let read_ahead = worker.counts.read_ahead;
        let answer = worker.respond(&asked);

        assert!(read_ahead >= 1);
        assert_eq!(without_token(&answer), fresh(&asked));
    }

    fn matches_fresh_checks_through(seed: u64, steps: usize) {
        let root = tempfile::tempdir().unwrap();
        let root_path = root.path().canonicalize().unwrap();
        shop_with_hidden_document(&root_path);
        let mut worker = Worker::start(&root_path.join("mod.lm")).unwrap();
        let billing = question(&root_path);
        let mut web = question(&root_path);
        web.args.change = Some("web".to_owned());
        web.raw = ["check", "--change", "web"].map(str::to_owned).to_vec();
        let mut changes = Changes {
            root: root_path.clone(),
            state: seed,
            written: 0,
        };
        let mut done = Vec::new();
        for step in 0..steps {
            done.push(changes.apply());
            if step % 2 == 0 {
                settle(&mut worker);
            }
            let asked = if step % 3 == 0 { &web } else { &billing };
            let answered = without_token(&worker.respond(asked));
            let expected = fresh(asked);
            assert_eq!(answered, expected, "seed {seed}, after {done:#?}");
        }
    }

    #[test]
    fn sentry_answers_as_fresh_checks_do_through_seeded_changes() {
        for seed in [11, 2027] {
            matches_fresh_checks_through(seed, 16);
        }
    }
}
