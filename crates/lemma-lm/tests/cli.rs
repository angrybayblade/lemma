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

#[test]
fn convert_prints_a_lemma_yaml_as_a_module() {
    let directory = tempfile::tempdir().unwrap();
    std::fs::write(
        directory.path().join("lemma.yaml"),
        "services:\n  api:\n    exports: { openapi: [openapi.json] }\n",
    )
    .unwrap();

    let output = lm(directory.path(), &["convert", "--module", "billing"]);

    assert!(output.status.success(), "{}", text(&output.stderr));
    let module = text(&output.stdout);
    assert!(module.starts_with("module billing\n"), "{module}");
    assert!(
        module.contains("http.openapi(\"openapi.json\")"),
        "{module}"
    );
}
