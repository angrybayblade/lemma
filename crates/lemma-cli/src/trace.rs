//! Following a graph's callers out of their repository, through the
//! workspace: the operations their handlers serve and the code elsewhere
//! calling those (`lemma_graph::trace_through_system`).
//!
//! A process that stays keeps a [`TracedWorkspace`] per workspace: its
//! repositories watched, what traces read of them, and the system as
//! traces last read it, which holds until a file changes.

use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::{Duration, Instant};

use lemma_graph::adapters::States;
use lemma_graph::{GraphError, Reached, SystemTrace, TraceCaches, TracedSystem};
use lemma_logic::Engine;

use crate::watched::WatchedWorkspace;

/// Where one trace starts.
pub(crate) struct Start<'a> {
    /// The workspace's description.
    pub workspace: &'a Path,
    /// The root of the index the reached functions are in.
    pub index_root: &'a Path,
    /// The functions already known to reach the traced symbol.
    pub reached: &'a [Reached],
}

/// The trace from `start`, reading everything it needs: what a process
/// that does not stay, or cannot watch the workspace, does. What one
/// round of the trace reads is kept for its later rounds, and no longer.
pub(crate) fn trace_once(start: &Start<'_>) -> Result<SystemTrace, GraphError> {
    let read: Arc<lemma_graph::Warm> = Arc::default();
    with_engine(Some(&read), |caches, engine| match engine {
        Evaluating::Souffle(souffle) => trace_reading_all(start, caches, souffle),
        Evaluating::Reference(reference) => trace_reading_all(start, caches, reference),
    })
}

fn trace_reading_all<E: Engine>(
    start: &Start<'_>,
    caches: &TraceCaches<'_>,
    engine: &E,
) -> Result<SystemTrace, GraphError> {
    lemma_graph::trace_through_system(
        start.workspace,
        start.index_root,
        start.reached,
        caches,
        engine,
    )
}

/// A workspace traces go through, watched: what they read of its
/// repositories is kept until it changes, and so is the system they read.
pub(crate) struct TracedWorkspace {
    watched: WatchedWorkspace,
    /// The system as traces last read it, and the workspace's state then.
    system: Option<(u64, TracedSystem)>,
    /// When a trace last went through it, or it started being watched.
    last_traced: Instant,
    /// Whether the ask being answered has already had the changes
    /// collected: Git is asked about unwatched repositories once per ask.
    is_collected: bool,
}

impl TracedWorkspace {
    /// Starts watching the workspace whose description is `manifest`,
    /// before any trace reads it, taking at most `watches` watches (`None`
    /// for no limit).
    pub(crate) fn start(manifest: &Path, watches: Option<usize>) -> anyhow::Result<Self> {
        Ok(Self {
            watched: WatchedWorkspace::start_within(manifest, watches)?,
            system: None,
            last_traced: Instant::now(),
            is_collected: false,
        })
    }

    /// How many watches it holds, where each directory takes one.
    pub(crate) fn watches(&self) -> usize {
        self.watched.watches()
    }

    /// Tells it another ask is being answered: what changed is collected
    /// anew for it.
    pub(crate) fn begin_ask(&mut self) {
        self.is_collected = false;
    }

    /// What is kept, as of the start of the ask being answered or later.
    fn current(&mut self) -> Arc<lemma_graph::Warm> {
        if std::mem::replace(&mut self.is_collected, true) {
            return self.watched.current_again();
        }
        self.watched.current()
    }

    /// When a trace last went through it.
    pub(crate) fn traced_at(&self) -> Instant {
        self.last_traced
    }

    /// Sets when a trace last went through it: a trace nobody asked for,
    /// run to be ready for the next, does not count as one.
    pub(crate) fn set_traced_at(&mut self, at: Instant) {
        self.last_traced = at;
    }

    /// Whether files changed, as reported so far, and have settled as of
    /// `now` with nobody having asked since: what there is to get ready
    /// for. Drops what is kept of what changed, as an ask would.
    pub(crate) fn has_settled_changes(&mut self, now: Instant) -> bool {
        self.watched.collect(false);
        !self.watched.take_settled(now).is_empty()
    }

    /// How long ago, at `now`, a trace last went through it.
    pub(crate) fn idle_for(&self, now: Instant) -> Duration {
        now.saturating_duration_since(self.last_traced)
    }

    /// Each repository it watches, a line each, for status.
    pub(crate) fn status_lines(&self) -> Vec<String> {
        let repositories = self.watched.repositories().iter();
        repositories
            .map(crate::watched::Watched::status_line)
            .collect()
    }

    /// The state of each watched repository's working directory, as Git
    /// gave it when last asked since the repository changed.
    pub(crate) fn working_states(&mut self) -> WorkingStates {
        WorkingStates {
            states: self.current().states(),
            watched: self.watched.reported_roots(),
        }
    }

    /// The trace from `start`, through the system as last read if no file
    /// has changed since, and otherwise read again from what is kept.
    pub(crate) fn trace(&mut self, start: &Start<'_>) -> Result<SystemTrace, GraphError> {
        self.last_traced = Instant::now();
        let warm = self.current();
        let state = self.watched.state();
        let kept = self
            .system
            .take()
            .filter(|(read_at, _)| *read_at == state)
            .map(|(_, system)| system);
        let (traced, system) = with_engine(Some(&warm), |caches, engine| match engine {
            Evaluating::Souffle(souffle) => trace_keeping(kept, start, caches, souffle),
            Evaluating::Reference(reference) => trace_keeping(kept, start, caches, reference),
        })?;
        self.system = Some((state, system));
        Ok(traced)
    }
}

/// The working states a watched workspace keeps, for the repositories whose
/// changes drop them.
pub(crate) struct WorkingStates {
    states: Arc<States>,
    /// The watched repositories' directories, canonical.
    watched: Vec<PathBuf>,
}

impl WorkingStates {
    /// The kept states, if the directory at `root` is a watched
    /// repository's: one that is not, such as a worktree at a pin, changes
    /// unseen, so its state is asked for every time.
    pub(crate) fn of(&self, root: &Path) -> Option<&States> {
        let root = root.canonicalize().ok()?;
        self.watched.contains(&root).then_some(&*self.states)
    }
}

/// The trace from `start` through `kept`, or through the system read now
/// when nothing is kept; with the system, for the next trace. An error
/// drops it: a failed read leaves it short of what it should hold.
fn trace_keeping<E: Engine>(
    kept: Option<TracedSystem>,
    start: &Start<'_>,
    caches: &TraceCaches<'_>,
    engine: &E,
) -> Result<(SystemTrace, TracedSystem), GraphError> {
    let mut system = match kept {
        Some(system) => system,
        None => TracedSystem::read(start.workspace, caches, engine)?,
    };
    let traced = system.trace(start.index_root, start.reached, caches, engine)?;
    Ok((traced, system))
}

/// The engine inference rules are evaluated with.
enum Evaluating<'a> {
    Souffle(&'a lemma_souffle::SouffleEngine),
    Reference(&'a lemma_logic::ReferenceEngine),
}

/// Runs `trace` with where traces keep what they read (`warm`, in a
/// process that stays) and the engine to evaluate with: Soufflé when it is
/// installed.
fn with_engine<T>(
    warm: Option<&Arc<lemma_graph::Warm>>,
    trace: impl FnOnce(&TraceCaches<'_>, Evaluating<'_>) -> Result<T, GraphError>,
) -> Result<T, GraphError> {
    let clones = lemma_graph::repos::cache_directory();
    let facts = crate::verify::facts_cache();
    let caches = TraceCaches {
        clones: &clones,
        facts: facts.as_ref(),
        warm,
    };
    match lemma_souffle::SouffleEngine::find() {
        Ok(souffle) => trace(&caches, Evaluating::Souffle(&souffle)),
        Err(_) => trace(
            &caches,
            Evaluating::Reference(&lemma_logic::ReferenceEngine),
        ),
    }
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

    const SERVER: &str = "import express from \"express\";\nconst app = express();\nfunction createCharge(req, res) {\n  res.json({ id: \"ch_1\" });\n}\napp.post(\"/v1/charges\", createCharge);\n";

    /// A file whose function `name` calls `POST /v1/charges`.
    fn caller(name: &str) -> String {
        format!(
            "export async function {name}() {{\n  return fetch(\"/v1/charges\", {{ method: \"POST\" }});\n}}\n"
        )
    }

    /// The shop under `root`: billing's `createCharge` serves
    /// `POST /v1/charges`, which web's `charge` calls. Its description.
    fn shop(root: &Path) -> std::path::PathBuf {
        let billing = root.join("billing");
        write(&billing.join("openapi.json"), SPEC);
        write(&billing.join("src/server.ts"), SERVER);
        write(
            &billing.join("mod.lm"),
            "module billing\n\nimport * as http from lemma.http\n\nexport service api {\n    index: { include: [\"src\"] }\n    operations: http.openapi(\"openapi.json\")\n}\n",
        );
        let web = root.join("web");
        write(&web.join("src/charge.ts"), &caller("charge"));
        write(
            &web.join("mod.lm"),
            "module web\n\nexport service web {\n    index: { include: [\"src\"] }\n}\n",
        );
        for repository in [&billing, &web] {
            git(repository, &["init", "-q"]);
            git(repository, &["add", "."]);
            git(repository, &["commit", "-q", "-m", "first"]);
        }
        write(
            &root.join("mod.lm"),
            "module shop\n\nworkspace {\n    repo billing = local(\"billing\")\n    repo web = local(\"web\")\n    config: { level: check }\n}\n",
        );
        root.join("mod.lm").canonicalize().unwrap()
    }

    fn labels(trace: &SystemTrace) -> Vec<&str> {
        trace.nodes.iter().map(|node| node.label.as_str()).collect()
    }

    #[test]
    fn a_kept_system_is_read_again_once_a_file_changes() {
        let root = tempfile::tempdir().unwrap();
        let manifest = shop(root.path());
        let billing = root.path().join("billing");
        let reached = [Reached {
            node: "symbol:handler".to_owned(),
            file: "src/server.ts".to_owned(),
            line: 3,
        }];
        let start = Start {
            workspace: &manifest,
            index_root: &billing,
            reached: &reached,
        };
        let mut traced = TracedWorkspace::start(&manifest, None).unwrap();

        let before = traced.trace(&start).unwrap();
        let again = traced.trace(&start).unwrap();
        write(&root.path().join("web/src/refund.ts"), &caller("refund"));
        let after = traced.trace(&start).unwrap();

        assert_eq!(labels(&before), ["POST /v1/charges", "charge"]);
        assert_eq!(again, before);
        assert_eq!(labels(&after), ["POST /v1/charges", "charge", "refund"]);
        assert_eq!(after, trace_once(&start).unwrap());
    }

    #[test]
    fn a_kept_system_is_read_again_once_a_description_changes() {
        let root = tempfile::tempdir().unwrap();
        let manifest = shop(root.path());
        let billing = root.path().join("billing");
        let reached = [Reached {
            node: "symbol:handler".to_owned(),
            file: "src/server.ts".to_owned(),
            line: 3,
        }];
        let start = Start {
            workspace: &manifest,
            index_root: &billing,
            reached: &reached,
        };
        let mut traced = TracedWorkspace::start(&manifest, None).unwrap();

        let before = traced.trace(&start).unwrap();
        write(
            &manifest,
            "module shop\n\nworkspace {\n    repo billing = local(\"billing\")\n    config: { level: check }\n}\n",
        );
        let after = traced.trace(&start).unwrap();

        assert_eq!(labels(&before), ["POST /v1/charges", "charge"]);
        assert_eq!(labels(&after), ["POST /v1/charges"]);
        assert_eq!(after, trace_once(&start).unwrap());
    }

    /// Where a trace from billing's handler starts.
    fn from_handler<'a>(
        manifest: &'a Path,
        billing: &'a Path,
        reached: &'a [Reached],
    ) -> Start<'a> {
        Start {
            workspace: manifest,
            index_root: billing,
            reached,
        }
    }

    fn handler() -> [Reached; 1] {
        [Reached {
            node: "symbol:handler".to_owned(),
            file: "src/server.ts".to_owned(),
            line: 3,
        }]
    }

    #[test]
    fn with_no_watches_the_system_is_kept_until_git_says_a_file_changed() {
        let root = tempfile::tempdir().unwrap();
        let manifest = shop(root.path());
        let billing = root.path().join("billing");
        let reached = handler();
        let start = from_handler(&manifest, &billing, &reached);
        let mut traced = TracedWorkspace::start(&manifest, Some(0)).unwrap();

        let before = traced.trace(&start).unwrap();
        traced.begin_ask();
        traced.trace(&start).unwrap();
        let read_at = traced.watched.state();
        traced.begin_ask();
        let again = traced.trace(&start).unwrap();
        let still_read_at = traced.watched.state();
        write(&root.path().join("web/src/refund.ts"), &caller("refund"));
        traced.begin_ask();
        let after = traced.trace(&start).unwrap();

        assert_eq!(traced.watches(), 0);
        assert_eq!(labels(&before), ["POST /v1/charges", "charge"]);
        assert_eq!(again, before);
        assert_eq!(still_read_at, read_at);
        assert_eq!(labels(&after), ["POST /v1/charges", "charge", "refund"]);
        assert_eq!(after, trace_once(&start).unwrap());
    }

    /// Waits, for a few seconds at most, until changes made to the
    /// workspace are reported and count as settled: a watcher reports in
    /// its own time, and only an ask waits for it.
    fn settles(traced: &mut TracedWorkspace) -> bool {
        let long_after = Instant::now() + Duration::from_secs(60);
        (0..500).any(|_| {
            std::thread::sleep(Duration::from_millis(10));
            traced.has_settled_changes(long_after)
        })
    }

    #[test]
    fn a_change_nobody_asked_about_is_there_to_get_ready_for_once() {
        let root = tempfile::tempdir().unwrap();
        let manifest = shop(root.path());
        let billing = root.path().join("billing");
        let reached = handler();
        let start = from_handler(&manifest, &billing, &reached);
        let mut traced = TracedWorkspace::start(&manifest, None).unwrap();
        traced.trace(&start).unwrap();

        write(&root.path().join("web/src/refund.ts"), &caller("refund"));
        let has_settled = settles(&mut traced);
        let long_after = Instant::now() + Duration::from_secs(60);
        let is_left = traced.has_settled_changes(long_after);
        traced.begin_ask();
        let after = traced.trace(&start).unwrap();

        assert!(has_settled);
        assert!(!is_left);
        assert_eq!(labels(&after), ["POST /v1/charges", "charge", "refund"]);
        assert_eq!(after, trace_once(&start).unwrap());
    }

    #[test]
    fn a_commit_is_a_change_to_get_ready_for() {
        let root = tempfile::tempdir().unwrap();
        let manifest = shop(root.path());
        let web = root.path().join("web");
        let mut traced = TracedWorkspace::start(&manifest, None).unwrap();
        write(&web.join("src/refund.ts"), &caller("refund"));
        let has_settled_after_the_edit = settles(&mut traced);
        // Whatever the edit still had to report is taken before the commit.
        std::thread::sleep(Duration::from_millis(300));
        traced.has_settled_changes(Instant::now() + Duration::from_secs(60));

        git(&web, &["add", "."]);
        git(&web, &["commit", "-q", "-m", "refunds"]);

        assert!(has_settled_after_the_edit);
        assert!(settles(&mut traced));
    }

    #[test]
    fn a_change_an_ask_has_read_leaves_nothing_to_get_ready_for() {
        let root = tempfile::tempdir().unwrap();
        let manifest = shop(root.path());
        let billing = root.path().join("billing");
        let reached = handler();
        let start = from_handler(&manifest, &billing, &reached);
        let mut traced = TracedWorkspace::start(&manifest, None).unwrap();
        traced.trace(&start).unwrap();

        write(&root.path().join("web/src/refund.ts"), &caller("refund"));
        traced.begin_ask();
        traced.trace(&start).unwrap();
        let long_after = Instant::now() + Duration::from_secs(60);

        assert!(!traced.has_settled_changes(long_after));
    }

    #[test]
    fn working_states_are_kept_for_watched_repositories_only() {
        let root = tempfile::tempdir().unwrap();
        let manifest = shop(root.path());
        let elsewhere = tempfile::tempdir().unwrap();
        let mut traced = TracedWorkspace::start(&manifest, None).unwrap();

        let states = traced.working_states();

        assert!(states.of(&root.path().join("web")).is_some());
        assert!(states.of(elsewhere.path()).is_none());
    }
}
