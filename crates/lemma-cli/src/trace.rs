//! Following a graph's callers out of their repository, through the
//! workspace: the operations their handlers serve and the code elsewhere
//! calling those (`lemma_graph::trace_through_system`).
//!
//! A process that stays keeps a [`TracedWorkspace`] per workspace: its
//! repositories watched, what traces read of them, and the system as
//! traces last read it, which holds until a file changes.

use std::path::Path;

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
/// that does not stay, or cannot watch the workspace, does.
pub(crate) fn trace_once(start: &Start<'_>) -> Result<SystemTrace, GraphError> {
    with_engine(None, |caches, engine| match engine {
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
}

impl TracedWorkspace {
    /// Starts watching the workspace whose description is `manifest`,
    /// before any trace reads it.
    pub(crate) fn start(manifest: &Path) -> anyhow::Result<Self> {
        Ok(Self {
            watched: WatchedWorkspace::start(manifest)?,
            system: None,
        })
    }

    /// The trace from `start`, through the system as last read if no file
    /// has changed since, and otherwise read again from what is kept.
    pub(crate) fn trace(&mut self, start: &Start<'_>) -> Result<SystemTrace, GraphError> {
        let warm = self.watched.current();
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
    warm: Option<&std::sync::Arc<lemma_graph::Warm>>,
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
        let mut traced = TracedWorkspace::start(&manifest).unwrap();

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
        let mut traced = TracedWorkspace::start(&manifest).unwrap();

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
}
