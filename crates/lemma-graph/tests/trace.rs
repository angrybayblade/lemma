//! A trace through a workspace: from a handler, to the operation it
//! serves, to the code in another repository calling it. A long-lived
//! process keeps what a trace read, and must answer as one that kept
//! nothing once what changed is forgotten.

#![allow(clippy::unwrap_used, reason = "test setup should fail loudly")]

use std::path::Path;
use std::process::Command;
use std::sync::Arc;

use lemma_graph::{Reached, SystemTrace, TraceCaches, TracedSystem, Warm, trace_through_system};
use lemma_logic::ReferenceEngine;

fn git(directory: &Path, args: &[&str]) {
    let status = Command::new("git")
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

/// The shop: billing's `createCharge` serves `POST /v1/charges`, which
/// web's `charge` calls. Each repository has one commit.
fn shop() -> tempfile::TempDir {
    let workspace = tempfile::tempdir().unwrap();
    let billing = workspace.path().join("billing");
    write(&billing.join("openapi.json"), SPEC);
    write(&billing.join("src/server.ts"), SERVER);
    write(
        &billing.join("mod.lm"),
        "module billing\n\nimport * as http from lemma.http\n\nexport service api {\n    index: { include: [\"src\"] }\n    operations: http.openapi(\"openapi.json\")\n}\n",
    );
    let web = workspace.path().join("web");
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
        &workspace.path().join("mod.lm"),
        "module shop\n\nworkspace {\n    repo billing = local(\"billing\")\n    repo web = local(\"web\")\n    config: { level: check }\n}\n",
    );
    workspace
}

/// A file whose function `name` calls `POST /v1/charges`.
fn caller(name: &str) -> String {
    format!(
        "export async function {name}() {{\n  return fetch(\"/v1/charges\", {{ method: \"POST\" }});\n}}\n"
    )
}

/// Billing's handler, as a graph that reached it names it.
fn handler() -> [Reached; 1] {
    [Reached {
        node: "symbol:handler".to_owned(),
        file: "src/server.ts".to_owned(),
        line: 3,
    }]
}

/// The trace from billing's handler, reading through `warm`.
fn trace(workspace: &Path, warm: Option<&Arc<Warm>>) -> SystemTrace {
    let reached = handler();
    let clones = workspace.join("cache");
    let caches = TraceCaches {
        clones: &clones,
        facts: None,
        warm,
    };
    trace_through_system(
        &workspace.join("mod.lm"),
        &workspace.join("billing"),
        &reached,
        &caches,
        &ReferenceEngine,
    )
    .unwrap()
}

fn labels(trace: &SystemTrace) -> Vec<&str> {
    trace.nodes.iter().map(|node| node.label.as_str()).collect()
}

#[test]
fn a_trace_follows_a_handler_to_the_code_calling_its_operation() {
    let workspace = shop();

    let traced = trace(workspace.path(), None);

    assert_eq!(labels(&traced), ["POST /v1/charges", "charge"]);
}

#[test]
fn a_trace_reading_what_is_kept_answers_as_one_that_kept_nothing() {
    let workspace = shop();
    let warm = Arc::new(Warm::default());

    let first = trace(workspace.path(), Some(&warm));
    let again = trace(workspace.path(), Some(&warm));

    assert_eq!(first, trace(workspace.path(), None));
    assert_eq!(again, first);
}

#[test]
fn a_trace_sees_a_change_once_what_is_kept_of_its_repository_is_forgotten() {
    let workspace = shop();
    let warm = Arc::new(Warm::default());
    let before = trace(workspace.path(), Some(&warm));
    let web = workspace.path().join("web");
    write(&web.join("src/refund.ts"), &caller("refund"));

    warm.forget_working(&web);
    let after = trace(workspace.path(), Some(&warm));

    assert_eq!(labels(&before), ["POST /v1/charges", "charge"]);
    assert_eq!(labels(&after), ["POST /v1/charges", "charge", "refund"]);
    assert_eq!(after, trace(workspace.path(), None));
}

#[test]
fn a_system_read_once_gives_every_trace_what_reading_it_again_would() {
    let workspace = shop();
    let clones = workspace.path().join("cache");
    let caches = TraceCaches {
        clones: &clones,
        facts: None,
        warm: None,
    };
    let manifest = workspace.path().join("mod.lm");
    let billing = workspace.path().join("billing");
    let nothing_reached: [Reached; 0] = [];
    let mut system = TracedSystem::read(&manifest, &caches, &ReferenceEngine).unwrap();

    let first = system
        .trace(&billing, &handler(), &caches, &ReferenceEngine)
        .unwrap();
    let from_nothing = system
        .trace(&billing, &nothing_reached, &caches, &ReferenceEngine)
        .unwrap();
    let again = system
        .trace(&billing, &handler(), &caches, &ReferenceEngine)
        .unwrap();

    assert_eq!(first, trace(workspace.path(), None));
    assert_eq!(again, first);
    assert_eq!(from_nothing, SystemTrace::default());
}
