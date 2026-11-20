//! The `lm` binary, run as a user runs it.

#![allow(clippy::unwrap_used, reason = "test setup should fail loudly")]

use std::path::Path;
use std::process::{Command, Output};

fn lm(directory: &Path, args: &[&str]) -> Output {
    Command::new(env!("CARGO_BIN_EXE_lm"))
        .args(args)
        .current_dir(directory)
        .env("XDG_CACHE_HOME", directory.join("cache"))
        .output()
        .unwrap()
}

fn text(bytes: &[u8]) -> String {
    String::from_utf8_lossy(bytes).into_owned()
}

#[test]
fn check_reports_each_problem_with_its_place_and_fails() {
    let directory = tempfile::tempdir().unwrap();
    std::fs::write(
        directory.path().join("mod.lm"),
        "module shop\nrelation Owns(team: Text)\nrequire broken {\n    when: missing(x)\n}\n",
    )
    .unwrap();

    let output = lm(directory.path(), &["check"]);

    assert_eq!(output.status.code(), Some(1));
    let stdout = text(&output.stdout);
    assert!(stdout.contains("mod.lm:4:11: error:"), "{stdout}");
    assert!(stdout.contains("1 errors"), "{stdout}");
}

#[test]
fn check_reports_rules_that_cannot_run() {
    let directory = tempfile::tempdir().unwrap();
    // A wildcard in a negation: the module type-checks, but its rules do
    // not compile, and before they did only when `lemma check` inferred
    // links.
    std::fs::write(
        directory.path().join("mod.lm"),
        "module shop\n\nimport lemma.http\n\nrule without_query(op: Text) {\n    (op) :- lemma.http.operation@head(_, op, _, _), !lemma.http.parameter@head(op, _, \"query\")\n}\n",
    )
    .unwrap();

    let output = lm(directory.path(), &["check"]);

    let stdout = text(&output.stdout);
    assert_eq!(output.status.code(), Some(1), "{stdout}");
    assert!(stdout.contains("the rules cannot run"), "{stdout}");
    assert!(stdout.contains("negates"), "{stdout}");
}

#[test]
fn a_clean_module_checks_and_formats_quietly() {
    let directory = tempfile::tempdir().unwrap();
    std::fs::write(
        directory.path().join("mod.lm"),
        "module shop\n\nrelation Owns(team: Text)\n",
    )
    .unwrap();

    let checked = lm(directory.path(), &["check", "--json"]);
    let formatted = lm(directory.path(), &["fmt", "--check"]);

    assert!(checked.status.success(), "{}", text(&checked.stdout));
    let report: serde_json::Value = serde_json::from_slice(&checked.stdout).unwrap();
    assert_eq!(report["entry"], "shop");
    assert!(formatted.status.success(), "{}", text(&formatted.stdout));
}
