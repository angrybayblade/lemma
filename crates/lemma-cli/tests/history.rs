//! `lemma check --change <repository> --last N`, through the real binary:
//! a service's past contract changes are checked one by one against the
//! caller as it stood then, each finding says whether the lines naming
//! what was found changed later, and no finding fails the command.

#![allow(clippy::unwrap_used, reason = "test setup should fail loudly")]

use std::path::Path;
use std::process::{Command, Output};

/// 2026-01-01 00:00:00 UTC; each commit of the fixture is some days after.
const NEW_YEAR_SECONDS: i64 = 1_767_225_600;
const DAY_SECONDS: i64 = 86_400;

fn write(path: &Path, text: &str) {
    std::fs::create_dir_all(path.parent().unwrap()).unwrap();
    std::fs::write(path, text).unwrap();
}

fn git_on_day(directory: &Path, day: i64, args: &[&str]) {
    let date = format!("{} +0000", NEW_YEAR_SECONDS + day * DAY_SECONDS);
    let status = Command::new("git")
        .arg("-C")
        .arg(directory)
        .args(["-c", "user.name=t", "-c", "user.email=t@t"])
        .args(["-c", "commit.gpgsign=false"])
        .args(args)
        .env("GIT_AUTHOR_DATE", &date)
        .env("GIT_COMMITTER_DATE", &date)
        .status()
        .unwrap();
    assert!(status.success(), "git {args:?}");
}

fn commit_on_day(repository: &Path, day: i64, subject: &str) {
    if !repository.join(".git").exists() {
        git_on_day(repository, day, &["init", "-q"]);
    }
    git_on_day(repository, day, &["add", "."]);
    git_on_day(repository, day, &["commit", "-q", "-m", subject]);
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

fn document(fields: &str) -> String {
    format!(
        r#"{{ "openapi": "3.0.0", "info": {{ "title": "B", "version": "1" }}, "paths": {{
  "/v1/invoices": {{ "get": {{ "operationId": "getInvoice",
    "responses": {{ "200": {{ "description": "ok", "content": {{ "application/json": {{ "schema":
      {{ "type": "object", "properties": {{ "id": {{ "type": "string" }}, {fields} }} }} }} }} }} }} }} }} }} }}"#
    )
}

const READS_TOTAL: &str = "export async function invoiceTotal() {\n  const res = await fetch('/v1/invoices')\n  const body = await res.json()\n  return body.total\n}\n";
const READS_PAID: &str = "export async function invoiceTotal() {\n  const res = await fetch('/v1/invoices')\n  const body = await res.json()\n  return body.paid\n}\n";

/// A shop whose billing service drops `note` on day 2, before its caller
/// exists, adds `paid` on day 4, and drops `total` on day 5, which web
/// reads from day 3 until it reads `paid` instead on day 9.
fn shop() -> tempfile::TempDir {
    let directory = tempfile::tempdir().unwrap();
    let billing = directory.path().join("billing");
    let web = directory.path().join("web");
    write(
        &billing.join("mod.lm"),
        "module billing\n\nimport * as http from lemma.http\n\nexport service api {\n    operations: http.openapi(\"openapi.json\")\n}\n",
    );
    let total_and_note = r#""total": { "type": "number" }, "note": { "type": "string" }"#;
    write(&billing.join("openapi.json"), &document(total_and_note));
    commit_on_day(&billing, 1, "start billing");
    let total = r#""total": { "type": "number" }"#;
    write(&billing.join("openapi.json"), &document(total));
    commit_on_day(&billing, 2, "drop note");

    write(
        &web.join("mod.lm"),
        "module web\n\nexport service web {\n    index: { include: [\"src\"] }\n}\n",
    );
    write(&web.join("src/invoice.ts"), READS_TOTAL);
    commit_on_day(&web, 3, "start web");

    let total_and_paid = r#""total": { "type": "number" }, "paid": { "type": "boolean" }"#;
    write(&billing.join("openapi.json"), &document(total_and_paid));
    commit_on_day(&billing, 4, "add paid");
    let paid = r#""paid": { "type": "boolean" }"#;
    write(&billing.join("openapi.json"), &document(paid));
    commit_on_day(&billing, 5, "drop total");

    write(&web.join("src/invoice.ts"), READS_PAID);
    commit_on_day(&web, 9, "read paid");

    write(
        &directory.path().join("mod.lm"),
        "module system\n\nworkspace {\n    repo billing = local(\"billing\")\n    repo web = local(\"web\")\n}\n",
    );
    directory
}

#[test]
fn past_changes_are_checked_against_the_caller_of_their_time() {
    let shop = shop();

    let output = lemma(
        shop.path(),
        &["check", "mod.lm", "--change", "billing", "--last", "10"],
    );

    let printed = String::from_utf8_lossy(&output.stdout);
    let lines: Vec<&str> = printed.lines().collect();
    assert!(output.status.success(), "{printed}");
    assert!(lines[0].ends_with(" 2026-01-06 drop total"), "{printed}");
    assert!(
        lines[1].starts_with("  error   ") && lines[1].contains("`total`"),
        "{printed}"
    );
    assert!(
        lines[3].contains(
            "changed the lines naming `total` in src/invoice.ts (1 removed, 0 added), 4 days later (read paid)"
        ),
        "{printed}"
    );
    assert!(lines[4].ends_with(" 2026-01-05 add paid"), "{printed}");
    assert_eq!(lines[5], "  introduced no error or warning");
    assert!(lines[6].ends_with(" 2026-01-03 drop note"), "{printed}");
    assert_eq!(lines[7], "  not checked: #web had no commit yet");
    assert_eq!(
        lines[9],
        "3 changes: 1 introduced errors, 0 introduced warnings only, 1 introduced neither, 1 not checked"
    );
}

#[test]
fn a_caller_that_still_names_the_field_is_said_to() {
    let shop = shop();
    let web = shop.path().join("web");
    git_on_day(&web, 9, &["reset", "-q", "--hard", "HEAD~1"]);

    let output = lemma(
        shop.path(),
        &["check", "mod.lm", "--change", "billing", "--last", "1"],
    );

    let printed = String::from_utf8_lossy(&output.stdout);
    assert!(
        printed.contains(
            "afterwards: no later commit changed a line naming `total` in src/invoice.ts"
        ),
        "{printed}"
    );
}

#[test]
fn as_json_each_change_holds_its_commit_its_pins_and_its_findings() {
    let shop = shop();

    let output = lemma(
        shop.path(),
        &[
            "check", "mod.lm", "--change", "billing", "--last", "1", "--json",
        ],
    );

    let changes: serde_json::Value = serde_json::from_slice(&output.stdout).unwrap();
    let change = &changes[0];
    assert_eq!(change["commit"]["subject"], "drop total");
    assert_eq!(change["commit"]["committed_on"], "2026-01-06");
    assert!(change["pins"]["#web"].is_string(), "{change}");
    assert_eq!(change["findings"][0]["severity"], "error");
    assert_eq!(change["findings"][0]["afterwards"]["days_after"], 4);
    assert_eq!(
        change["findings"][0]["afterwards"]["changed_in"]["commit"]["subject"],
        "read paid"
    );
}

#[test]
fn the_last_changes_cannot_be_asked_with_a_base() {
    let shop = shop();

    let output = lemma(
        shop.path(),
        &[
            "check", "mod.lm", "--change", "billing", "--last", "2", "--base", "HEAD~1",
        ],
    );

    assert!(!output.status.success());
    assert!(String::from_utf8_lossy(&output.stderr).contains("--base"));
}
