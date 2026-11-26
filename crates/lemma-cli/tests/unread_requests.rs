//! Requests Lemma sees and cannot read as calls of a path, through the
//! real binary: `lemma verify` lists every one under what it looked at,
//! and `lemma check` notes those a change adds.

#![allow(clippy::unwrap_used, reason = "test setup should fail loudly")]

use std::path::Path;
use std::process::{Command, Output};

fn write(path: &Path, text: &str) {
    std::fs::create_dir_all(path.parent().unwrap()).unwrap();
    std::fs::write(path, text).unwrap();
}

fn git(directory: &Path, args: &[&str]) {
    let status = Command::new("git")
        .arg("-C")
        .arg(directory)
        .args(["-c", "user.name=t", "-c", "user.email=t@t"])
        .args(["-c", "commit.gpgsign=false"])
        .args(args)
        .status()
        .unwrap();
    assert!(status.success(), "git {args:?}");
}

fn commit_all(directory: &Path) {
    git(directory, &["init", "-q"]);
    git(directory, &["add", "."]);
    git(directory, &["commit", "-q", "-m", "first"]);
}

fn lemma(directory: &Path, args: &[&str]) -> Output {
    Command::new(env!("CARGO_BIN_EXE_lemma"))
        .args(args)
        .current_dir(directory)
        .env("HOME", directory)
        .env("XDG_CACHE_HOME", directory.join(".cache"))
        .env("LEMMA_NO_DAEMON", "1")
        .output()
        .unwrap()
}

fn text(output: &Output) -> String {
    format!(
        "{}{}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    )
}

const OPENAPI: &str = r#"{ "openapi": "3.0.0", "info": { "title": "B", "version": "1" }, "paths": { "/v1/invoices": { "get": { "operationId": "listInvoices", "responses": { "200": { "description": "ok" } } } } } }"#;

// One request of a written path, and one of a path computed elsewhere.
const API: &str = "export const invoices = () => fetch('/v1/invoices')\nexport const load = (url: string) => fetch(url)\n";
// A second computed request, as a change adds it.
const SAVE: &str =
    "export const save = (target: string, body: unknown) => axios.post(target, body)\n";

/// Billing's document, and web requesting one written path and one
/// computed one, composed in `root`.
fn system(root: &Path) {
    let billing = root.join("billing");
    write(&billing.join("openapi.json"), OPENAPI);
    write(
        &billing.join("mod.lm"),
        "module billing\n\nimport * as http from lemma.http\n\nexport service api {\n    operations: http.openapi(\"openapi.json\")\n}\n",
    );
    commit_all(&billing);
    let web = root.join("web");
    write(&web.join("src/api.ts"), API);
    write(
        &web.join("mod.lm"),
        "module web\n\nexport service web {\n    index: { include: [\"src\"] }\n}\n",
    );
    commit_all(&web);
    write(
        &root.join("mod.lm"),
        "module shop\n\nworkspace {\n    repo billing = local(\"billing\")\n    repo web = local(\"web\")\n    config: { level: check }\n}\n",
    );
}

#[test]
fn verify_says_what_it_read_and_lists_the_requests_it_did_not() {
    let scratch = tempfile::tempdir().unwrap();
    system(scratch.path());

    let output = lemma(scratch.path(), &["verify", "--no-daemon"]);

    let printed = text(&output);
    assert_eq!(output.status.code(), Some(0), "{printed}");
    assert!(
        printed.contains("1 operations, 1 calls of them read from code"),
        "{printed}"
    );
    assert!(
        printed.contains("1 requests not read: 1 computed"),
        "{printed}"
    );
    assert!(printed.contains("src/api.ts:2  computed  url"), "{printed}");
    assert!(
        printed.contains("1 of 1 operations have a call read, 1 requests not read"),
        "{printed}"
    );
}

#[test]
fn verify_gives_the_same_in_json() {
    let scratch = tempfile::tempdir().unwrap();
    system(scratch.path());

    let output = lemma(scratch.path(), &["verify", "--no-daemon", "--json"]);

    let report: serde_json::Value = serde_json::from_slice(&output.stdout).unwrap();
    let looked_at = &report["looked_at"];
    assert_eq!(looked_at["operations"], 1, "{report:#}");
    assert_eq!(looked_at["calls"], 1, "{report:#}");
    assert_eq!(looked_at["called_operations"], 1, "{report:#}");
    let unread = looked_at["unread_requests"].as_array().unwrap();
    assert_eq!(unread.len(), 1, "{report:#}");
    assert_eq!(unread[0]["reason"], "computed", "{report:#}");
    assert_eq!(unread[0]["line"], 2, "{report:#}");
}

#[test]
fn check_notes_a_request_a_change_adds_and_not_those_already_there() {
    let scratch = tempfile::tempdir().unwrap();
    system(scratch.path());
    let web = scratch.path().join("web");
    write(&web.join("src/api.ts"), &format!("{API}{SAVE}"));

    let output = lemma(
        scratch.path(),
        &[
            "check",
            "--change",
            "web",
            "--base",
            "HEAD",
            "--no-daemon",
            "--evaluation",
            "both",
            "--all",
        ],
    );

    let printed = text(&output);
    assert_eq!(output.status.code(), Some(0), "{printed}");
    assert!(
        printed.contains("adds a request that was not read as a call of a path (computed): target"),
        "{printed}"
    );
    assert!(printed.contains("1 informational"), "{printed}");
    assert!(!printed.contains("(computed): url"), "{printed}");
}
