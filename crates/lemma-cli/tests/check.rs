//! `lemma check` on `.lm` descriptions, through the real binary.

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

const BILLING: &str = "module billing\n\nimport * as typescript from lemma.typescript\n\nlet charges = typescript.file(\"src/charges.ts\")\n\nexport service api {\n    index: { include: [\"src\"] }\n\n    operation create_charge {\n        implementation: charges.symbol(\"createCharge\")\n    }\n\n    operation refund {\n        implementation: charges.symbol(\"refund\")\n    }\n}\n";

fn caller(name: &str, operation: &str) -> String {
    format!(
        "module {name}\n\nimport * as typescript from lemma.typescript\nimport {{ Link }} from lemma.links\nimport {{ api }} from billing\n\nrepo billing = local(\"../billing\")\n\nlet calls = typescript.file(\"src/calls.ts\")\n\nexport service client {{\n    index: {{ include: [\"src\"] }}\n\n    client operation call {{\n        implementation: calls.symbol(\"call\")\n    }}\n}}\n\nexport graph links : Link {{\n    client.call -> api.{operation}\n}}\n"
    )
}

/// Billing, and two repositories linked to one of its operations each,
/// composed in `root`.
fn system(root: &Path) {
    let billing = root.join("billing");
    write(
        &billing.join("src/charges.ts"),
        "export function createCharge() {}\nexport function refund() {}\n",
    );
    write(&billing.join("mod.lm"), BILLING);
    commit_all(&billing);
    for (name, operation) in [("web", "create_charge"), ("ledger", "refund")] {
        let directory = root.join(name);
        write(
            &directory.join("src/calls.ts"),
            "export function call() {}\n",
        );
        write(&directory.join("mod.lm"), &caller(name, operation));
        commit_all(&directory);
    }
    write(
        &root.join("mod.lm"),
        "module shop\n\nworkspace {\n    repo billing = local(\"billing\")\n    repo web = local(\"web\")\n    repo ledger = local(\"ledger\")\n    config: { level: check }\n}\n",
    );
}

fn remove_create_charge(root: &Path) {
    let removed = BILLING.replace(
        "    operation create_charge {\n        implementation: charges.symbol(\"createCharge\")\n    }\n\n",
        "",
    );
    write(&root.join("billing/mod.lm"), &removed);
}

#[test]
fn removing_a_linked_operation_is_a_finding_the_change_introduced() {
    let scratch = tempfile::tempdir().unwrap();
    system(scratch.path());
    remove_create_charge(scratch.path());

    let output = lemma(
        scratch.path(),
        &[
            "check",
            "--change",
            "billing",
            "--base",
            "HEAD",
            "--no-daemon",
        ],
    );

    assert_eq!(output.status.code(), Some(4), "{}", text(&output));
    assert!(
        text(&output).contains("no longer exists"),
        "{}",
        text(&output)
    );
}

#[test]
fn removing_a_linked_operation_leaves_every_other_link_checked() {
    let scratch = tempfile::tempdir().unwrap();
    system(scratch.path());
    remove_create_charge(scratch.path());

    let output = lemma(
        scratch.path(),
        &[
            "check",
            "--change",
            "billing",
            "--base",
            "HEAD",
            "--no-daemon",
            "--json",
        ],
    );

    let report: serde_json::Value = serde_json::from_slice(&output.stdout).unwrap();
    assert_eq!(report["links"], 2, "{}", text(&output));
}

#[test]
fn a_link_to_a_missing_operation_is_still_an_error_to_lm_check() {
    let scratch = tempfile::tempdir().unwrap();
    system(scratch.path());
    remove_create_charge(scratch.path());

    let output = Command::new(env!("CARGO_BIN_EXE_lm"))
        .args(["check", "mod.lm"])
        .current_dir(scratch.path().join("web"))
        .env("HOME", scratch.path())
        .output()
        .unwrap();

    assert!(!output.status.success(), "{}", text(&output));
}

#[test]
fn change_finds_a_repository_by_its_directory() {
    let scratch = tempfile::tempdir().unwrap();
    let directory = scratch.path().join("pay-web");
    write(&directory.join("src/a.ts"), "export const a = 1;\n");
    write(
        &directory.join("mod.lm"),
        "module pay_web\n\nexport service web {\n    index: { include: [\"src\"] }\n}\n",
    );
    commit_all(&directory);
    write(
        &scratch.path().join("mod.lm"),
        "module shop\n\nworkspace {\n    repo pay_web = local(\"pay-web\")\n}\n",
    );

    let output = lemma(
        scratch.path(),
        &[
            "check",
            "--change",
            "pay-web",
            "--base",
            "HEAD",
            "--no-daemon",
        ],
    );

    assert_eq!(output.status.code(), Some(0), "{}", text(&output));
}

/// A repository holding one operation, cloned nowhere yet; its URL.
fn remote_billing(root: &Path) -> String {
    let billing = root.join("remote/billing");
    write(
        &billing.join("src/charges.ts"),
        "export function createCharge() {}\nexport function refund() {}\n",
    );
    write(&billing.join("mod.lm"), BILLING);
    commit_all(&billing);
    format!("file://{}", billing.display())
}

/// `web` linked to billing, named by `source` in its `repo` line.
fn web_importing(root: &Path, source: &str) -> std::path::PathBuf {
    let web = root.join("web");
    write(&web.join("src/calls.ts"), "export function call() {}\n");
    let text = caller("web", "create_charge").replace("local(\"../billing\")", source);
    write(&web.join("mod.lm"), &text);
    commit_all(&web);
    web
}

fn git_source(url: &str) -> String {
    format!("git(url: \"{url}\", rev: \"HEAD\")")
}

#[test]
fn a_missing_local_repository_is_an_error() {
    let scratch = tempfile::tempdir().unwrap();
    let web = web_importing(scratch.path(), "local(\"../nowhere\")");

    let output = lemma(&web, &["check", "mod.lm", "--no-daemon"]);

    assert_eq!(output.status.code(), Some(1), "{}", text(&output));
}

#[test]
fn without_a_terminal_a_git_repository_is_cloned_by_default() {
    let scratch = tempfile::tempdir().unwrap();
    let url = remote_billing(scratch.path());
    let web = web_importing(scratch.path(), &git_source(&url));

    let output = lemma(&web, &["check", "mod.lm", "--no-daemon"]);

    assert_eq!(output.status.code(), Some(0), "{}", text(&output));
}

#[test]
fn never_cloning_checks_what_is_here_and_exits_incomplete() {
    let scratch = tempfile::tempdir().unwrap();
    let url = remote_billing(scratch.path());
    let web = web_importing(scratch.path(), &git_source(&url));
    write(
        &web.join(".lemma/config.toml"),
        "[repositories]\nclone = \"never\"\n",
    );

    let output = lemma(&web, &["check", "mod.lm", "--no-daemon"]);

    assert_eq!(output.status.code(), Some(5), "{}", text(&output));
    assert!(
        text(&output).contains("not checked: 1 links to #billing"),
        "{}",
        text(&output)
    );
}

#[test]
fn no_clone_stops_at_what_is_here_too() {
    let scratch = tempfile::tempdir().unwrap();
    let url = remote_billing(scratch.path());
    let web = web_importing(scratch.path(), &git_source(&url));

    let output = lemma(&web, &["check", "mod.lm", "--no-daemon", "--no-clone"]);

    assert_eq!(output.status.code(), Some(5), "{}", text(&output));
}

#[test]
fn a_change_naming_a_repository_that_is_not_here_is_an_error() {
    let scratch = tempfile::tempdir().unwrap();
    let url = remote_billing(scratch.path());
    let web = web_importing(scratch.path(), &git_source(&url));

    let output = lemma(
        &web,
        &[
            "check",
            "mod.lm",
            "--no-daemon",
            "--no-clone",
            "--change",
            "billing",
            "--base",
            "HEAD",
        ],
    );

    assert_eq!(output.status.code(), Some(1), "{}", text(&output));
    assert!(text(&output).contains("not cloned"), "{}", text(&output));
}

#[test]
fn an_index_that_may_not_clone_keeps_what_the_description_says() {
    let scratch = tempfile::tempdir().unwrap();
    let url = remote_billing(scratch.path());
    let web = web_importing(scratch.path(), &git_source(&url));
    write(&web.join("scripts/seed.ts"), "export const seed = 1;\n");

    let output = lemma(&web, &["index", "--no-clone"]);

    assert_eq!(output.status.code(), Some(0), "{}", text(&output));
    assert!(
        text(&output).contains("#billing is not cloned"),
        "{}",
        text(&output)
    );
    assert!(text(&output).contains("1 files"), "{}", text(&output));
}

#[test]
fn a_query_does_not_clone() {
    let scratch = tempfile::tempdir().unwrap();
    let url = remote_billing(scratch.path());
    let web = web_importing(scratch.path(), &git_source(&url));
    let first = lemma(&web, &["index", "--no-clone"]);
    assert_eq!(first.status.code(), Some(0), "{}", text(&first));

    let output = lemma(&web, &["symbols", "call"]);

    assert_eq!(output.status.code(), Some(0), "{}", text(&output));
    assert!(!scratch.path().join("web/.cache/lemma/repos").exists());
}
