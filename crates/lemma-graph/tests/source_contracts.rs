//! Native proofs through the same check, evidence and no-run path as commands.
#![allow(clippy::unwrap_used, reason = "test setup should fail loudly")]

use std::collections::BTreeMap;
use std::path::Path;
use std::process::Command;
use std::sync::Arc;

use lemma_graph::{CheckOptions, Report, check, config::Level};
use lemma_logic::ReferenceEngine;

fn workspace() -> tempfile::TempDir {
    let workspace = tempfile::tempdir().unwrap();
    let root = workspace.path().join("repo");
    std::fs::create_dir(&root).unwrap();
    for file in ["mod.lm", "handler.ts", "handler.py", "handler.go"] {
        std::fs::copy(
            Path::new(env!("CARGO_MANIFEST_DIR"))
                .join("../../examples/source-contracts")
                .join(file),
            root.join(file),
        )
        .unwrap();
    }
    for arguments in [
        vec!["init", "-q"],
        vec!["add", "."],
        vec![
            "-c",
            "user.name=test",
            "-c",
            "user.email=test@example.com",
            "-c",
            "commit.gpgsign=false",
            "commit",
            "-qm",
            "initial",
        ],
    ] {
        assert!(
            Command::new("git")
                .current_dir(&root)
                .args(arguments)
                .status()
                .unwrap()
                .success()
        );
    }
    workspace
}

fn run(workspace: &Path, solver: &str, may_run: bool) -> Report {
    let environment = BTreeMap::from([
        ("PATH".to_owned(), std::env::var("PATH").unwrap_or_default()),
        ("LEMMA_SMT_SOLVER".to_owned(), solver.to_owned()),
    ]);
    let options = CheckOptions {
        change: None,
        level: Some(Level::Strict),
        cache: workspace.join("cache"),
        cloning: lemma_graph::repos::Cloning::Allowed,
        explain: false,
        relations: Vec::new(),
        facts_cache: None,
        evaluation: lemma_verify::Evaluation::Full,
        list_links: false,
        run_checkers: may_run,
        runs_base_checkers: false,
        select: None,
        warm: None,
        environment: Some(Arc::new(environment)),
        runtime: None,
    };
    check(
        &workspace.join("repo/mod.lm"),
        &options,
        &ReferenceEngine,
        None,
    )
    .unwrap()
}

#[test]
fn a_missing_solver_is_unknown_through_the_normal_check_path() {
    let workspace = workspace();
    let report = run(workspace.path(), "/missing/lemma-test-z3", true);
    assert_eq!(report.behaviors.len(), 1);
    assert_eq!(report.behaviors[0].outcome, "unknown");
    assert!(report.behaviors[0].evidence.iter().all(|used| {
        used.unavailable
            .as_ref()
            .is_some_and(|why| why.contains("SMT solver failed"))
    }));
    assert!(
        report
            .findings
            .iter()
            .any(|finding| finding.severity == lemma_graph::Severity::Error)
    );
}

#[test]
#[ignore = "requires native Z3; set LEMMA_SMT_SOLVER and run with --ignored"]
fn native_proofs_reuse_evidence_and_invalidate_it_on_source_or_solver_changes() {
    let workspace = workspace();
    let solver = std::env::var("LEMMA_SMT_SOLVER").unwrap_or_else(|_| "z3".to_owned());
    let first = run(workspace.path(), &solver, true);
    assert_eq!(first.behaviors[0].outcome, "satisfied", "{first:#?}");
    assert_eq!(first.behaviors[0].evidence.len(), 3);
    let reused = run(workspace.path(), &solver, false);
    assert_eq!(reused.behaviors[0].outcome, "satisfied");
    assert!(reused.behaviors[0].evidence.iter().all(|used| used.reused));
    let changed_solver = run(workspace.path(), "/missing/lemma-test-z3", false);
    assert!(
        changed_solver.behaviors[0]
            .cases
            .iter()
            .skip(1)
            .all(|case| case.outcome == "stale")
    );
    let source = workspace.path().join("repo/handler.ts");
    let text = std::fs::read_to_string(&source).unwrap();
    std::fs::write(&source, text.replace("write.rejected", "false")).unwrap();
    let stale = run(workspace.path(), &solver, false);
    assert!(
        stale.behaviors[0]
            .cases
            .iter()
            .skip(1)
            .all(|case| case.outcome == "stale")
    );
    let violated = run(workspace.path(), &solver, true);
    assert_eq!(violated.behaviors[0].outcome, "violated");
}

#[test]
#[ignore = "requires native Z3; set LEMMA_SMT_SOLVER and run with --ignored"]
fn ignored_source_bytes_also_invalidate_kept_proofs() {
    let workspace = workspace();
    let root = workspace.path().join("repo");
    std::fs::write(root.join(".gitignore"), "handler.ts\n").unwrap();
    assert!(
        Command::new("git")
            .current_dir(&root)
            .args(["rm", "--cached", "-q", "handler.ts"])
            .status()
            .unwrap()
            .success()
    );
    let solver = std::env::var("LEMMA_SMT_SOLVER").unwrap_or_else(|_| "z3".to_owned());
    let first = run(workspace.path(), &solver, true);
    assert_eq!(first.behaviors[0].outcome, "satisfied");
    let source = root.join("handler.ts");
    let text = std::fs::read_to_string(&source).unwrap();
    std::fs::write(&source, text.replace("write.rejected", "false")).unwrap();
    let report = run(workspace.path(), &solver, false);
    assert!(report.behaviors[0].cases.iter().any(|case| {
        case.identity.ends_with("handler=Accounts.typescript]") && case.outcome == "stale"
    }));
}
