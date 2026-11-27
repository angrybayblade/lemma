//! Checking a repository's history: its past contract changes are listed
//! with the other repositories' commits of the time, a past change is
//! checked against callers as they stood then, and the code a finding rests
//! on is followed to the commit that changed the lines naming what was
//! found.

#![allow(clippy::unwrap_used, reason = "test setup should fail loudly")]

use std::collections::BTreeMap;
use std::path::Path;
use std::process::Command;

use lemma_graph::config::Level;
use lemma_graph::history::{is_named_at, later_change, past_changes};
use lemma_graph::{Change, CheckOptions, Severity, check};
use lemma_logic::ReferenceEngine;

/// 2026-01-01 00:00:00 UTC; each commit of a fixture is some days after.
const NEW_YEAR_SECONDS: i64 = 1_767_225_600;
const DAY_SECONDS: i64 = 86_400;

fn git_on_day(directory: &Path, day: i64, args: &[&str]) -> String {
    let date = format!("{} +0000", NEW_YEAR_SECONDS + day * DAY_SECONDS);
    let output = Command::new("git")
        .arg("-C")
        .arg(directory)
        .args(["-c", "user.name=t", "-c", "user.email=t@t"])
        .args(["-c", "commit.gpgsign=false"])
        .args(args)
        .env("GIT_AUTHOR_DATE", &date)
        .env("GIT_COMMITTER_DATE", &date)
        .output()
        .unwrap();
    assert!(output.status.success(), "git {args:?}");
    String::from_utf8(output.stdout).unwrap().trim().to_owned()
}

fn write(path: &Path, text: &str) {
    std::fs::create_dir_all(path.parent().unwrap()).unwrap();
    std::fs::write(path, text).unwrap();
}

/// Commits everything in `repository` on `day` and returns the commit.
fn commit_on_day(repository: &Path, day: i64, subject: &str) -> String {
    if !repository.join(".git").exists() {
        git_on_day(repository, day, &["init", "-q"]);
    }
    git_on_day(repository, day, &["add", "."]);
    git_on_day(repository, day, &["commit", "-q", "-m", subject]);
    git_on_day(repository, day, &["rev-parse", "HEAD"])
}

fn document(fields: &str) -> String {
    format!(
        r#"{{ "openapi": "3.0.0", "info": {{ "title": "B", "version": "1" }}, "paths": {{
  "/v1/invoices": {{ "get": {{ "operationId": "getInvoice",
    "responses": {{ "200": {{ "description": "ok", "content": {{ "application/json": {{ "schema":
      {{ "type": "object", "properties": {{ "id": {{ "type": "string" }}, {fields} }} }} }} }} }} }} }} }} }} }}"#
    )
}

const TOTAL_AND_NOTE: &str = r#""total": { "type": "number" }, "note": { "type": "string" }"#;
const TOTAL: &str = r#""total": { "type": "number" }"#;
const NOTHING_ELSE: &str = r#""paid": { "type": "boolean" }"#;

const READS_TOTAL: &str = "export async function invoiceTotal() {\n  const res = await fetch('/v1/invoices')\n  const body = await res.json()\n  return body.total\n}\n";
const READS_PAID: &str = "export async function invoiceTotal() {\n  const res = await fetch('/v1/invoices')\n  const body = await res.json()\n  return body.paid\n}\n";

/// The commits of a shop whose history is, by day:
///
/// 1. billing starts, its invoice holding `total` and `note`;
/// 2. billing drops `note`, before any caller exists;
/// 3. web starts, reading `total`;
/// 4. billing changes its README only;
/// 5. billing drops `total`, which web still reads;
/// 6. web edits a file that reads nothing;
/// 9. web reads `paid` instead.
struct Shop {
    directory: tempfile::TempDir,
    dropped_note: String,
    dropped_total: String,
    web_reading_total: String,
    web_reading_paid: String,
}

fn shop() -> Shop {
    let directory = tempfile::tempdir().unwrap();
    let billing = directory.path().join("billing");
    let web = directory.path().join("web");
    write(
        &billing.join("mod.lm"),
        "module billing\n\nimport * as http from lemma.http\n\nexport service api {\n    operations: http.openapi(\"openapi.json\")\n}\n",
    );
    write(&billing.join("openapi.json"), &document(TOTAL_AND_NOTE));
    commit_on_day(&billing, 1, "start billing");
    write(&billing.join("openapi.json"), &document(TOTAL));
    let dropped_note = commit_on_day(&billing, 2, "drop note");

    write(
        &web.join("mod.lm"),
        "module web\n\nexport service web {\n    index: { include: [\"src\"] }\n}\n",
    );
    write(&web.join("src/invoice.ts"), READS_TOTAL);
    write(&web.join("src/title.ts"), "export const title = 'Shop'\n");
    let web_reading_total = commit_on_day(&web, 3, "start web");

    write(&billing.join("README.md"), "Billing.\n");
    commit_on_day(&billing, 4, "describe billing");
    write(&billing.join("openapi.json"), &document(NOTHING_ELSE));
    let dropped_total = commit_on_day(&billing, 5, "drop total");

    write(
        &web.join("src/title.ts"),
        "export const title = 'The shop'\n",
    );
    commit_on_day(&web, 6, "rename the shop");
    write(&web.join("src/invoice.ts"), READS_PAID);
    let web_reading_paid = commit_on_day(&web, 9, "read paid");

    write(
        &directory.path().join("mod.lm"),
        "module system\n\nworkspace {\n    repo billing = local(\"billing\")\n    repo web = local(\"web\")\n}\n",
    );
    Shop {
        directory,
        dropped_note,
        dropped_total,
        web_reading_total,
        web_reading_paid,
    }
}

fn options(workspace: &Path, change: Change, pins: BTreeMap<String, String>) -> CheckOptions {
    CheckOptions {
        change: Some(change),
        pins,
        level: Some(Level::Check),
        cache: workspace.join("cache"),
        cloning: lemma_graph::repos::Cloning::Allowed,
        explain: false,
        relations: Vec::new(),
        facts_cache: None,
        evaluation: lemma_verify::Evaluation::Demand,
        list_links: false,
        run_checkers: true,
        runs_base_checkers: false,
        select: None,
        warm: None,
        environment: None,
        runtime: None,
    }
}

fn billing_changed(base: Option<String>, head: Option<String>) -> Change {
    Change {
        repository: "billing".to_owned(),
        base,
        head,
    }
}

#[test]
fn past_changes_are_the_commits_that_touched_a_contract_newest_first() {
    let shop = shop();
    let workspace = shop.directory.path();
    let asked = options(workspace, billing_changed(None, None), BTreeMap::new());

    let changes = past_changes(&workspace.join("mod.lm"), &asked, 10).unwrap();

    let subjects: Vec<&str> = changes
        .iter()
        .map(|change| change.commit.subject.as_str())
        .collect();
    assert_eq!(subjects, ["drop total", "drop note"]);
    assert_eq!(changes[0].commit.hash, shop.dropped_total);
    assert_eq!(
        changes[0].commit.committed_at_seconds,
        NEW_YEAR_SECONDS + 5 * DAY_SECONDS
    );
}

#[test]
fn only_as_many_changes_as_asked_for_are_listed() {
    let shop = shop();
    let workspace = shop.directory.path();
    let asked = options(workspace, billing_changed(None, None), BTreeMap::new());

    let changes = past_changes(&workspace.join("mod.lm"), &asked, 1).unwrap();

    assert_eq!(changes.len(), 1);
    assert_eq!(changes[0].commit.hash, shop.dropped_total);
}

#[test]
fn each_past_change_reads_the_others_at_their_last_commit_before_it() {
    let shop = shop();
    let workspace = shop.directory.path();
    let asked = options(workspace, billing_changed(None, None), BTreeMap::new());

    let changes = past_changes(&workspace.join("mod.lm"), &asked, 10).unwrap();

    assert_eq!(
        changes[0].pins,
        BTreeMap::from([("#web".to_owned(), shop.web_reading_total.clone())])
    );
    assert_eq!(changes[0].not_yet_there, Vec::<String>::new());
}

#[test]
fn a_change_older_than_another_repository_says_that_repository_was_not_there() {
    let shop = shop();
    let workspace = shop.directory.path();
    let asked = options(workspace, billing_changed(None, None), BTreeMap::new());

    let changes = past_changes(&workspace.join("mod.lm"), &asked, 10).unwrap();

    assert_eq!(changes[1].commit.hash, shop.dropped_note);
    assert_eq!(changes[1].pins, BTreeMap::new());
    assert_eq!(changes[1].not_yet_there, ["#web"]);
}

#[test]
fn a_past_change_is_checked_against_the_caller_as_it_stood_then() {
    let shop = shop();
    let workspace = shop.directory.path();
    let change = billing_changed(
        Some(format!("{}^", shop.dropped_total)),
        Some(shop.dropped_total.clone()),
    );
    let as_it_stands = options(workspace, change.clone(), BTreeMap::new());
    let pins = BTreeMap::from([("#web".to_owned(), shop.web_reading_total.clone())]);
    let as_it_stood = options(workspace, change, pins);
    let errors = |asked: &CheckOptions| -> Vec<String> {
        let report = check(&workspace.join("mod.lm"), asked, &ReferenceEngine, None).unwrap();
        report
            .findings
            .into_iter()
            .filter(|finding| finding.severity == Severity::Error)
            .map(|finding| finding.message)
            .collect()
    };

    let today = errors(&as_it_stands);
    let then = errors(&as_it_stood);

    assert_eq!(today, Vec::<String>::new());
    assert_eq!(then.len(), 1, "{then:?}");
    assert!(then[0].contains("`total`"), "{then:?}");
    assert!(then[0].contains("src/invoice.ts:2"), "{then:?}");
}

#[test]
fn a_later_commit_that_changed_the_line_naming_the_field_is_found() {
    let shop = shop();
    let web = shop.directory.path().join("web");

    let later = later_change(&web, &shop.web_reading_total, "src/invoice.ts", &["total"]).unwrap();

    let later = later.unwrap();
    assert_eq!(later.commit.hash, shop.web_reading_paid);
    assert_eq!(later.commit.subject, "read paid");
    assert_eq!(
        later.commit.committed_at_seconds,
        NEW_YEAR_SECONDS + 9 * DAY_SECONDS
    );
    assert_eq!((later.lines_removed, later.lines_added), (1, 0));
}

#[test]
fn lines_naming_the_field_that_stand_have_no_later_change() {
    let shop = shop();
    let web = shop.directory.path().join("web");

    let another_name =
        later_change(&web, &shop.web_reading_total, "src/invoice.ts", &["id"]).unwrap();
    let from_the_last_commit =
        later_change(&web, &shop.web_reading_paid, "src/invoice.ts", &["paid"]).unwrap();

    assert_eq!(another_name, None);
    assert_eq!(from_the_last_commit, None);
}

#[test]
fn only_a_file_that_names_the_field_at_the_commit_has_lines_to_follow() {
    let shop = shop();
    let web = shop.directory.path().join("web");
    let then = shop.web_reading_total.as_str();

    let reads_it = is_named_at(&web, then, "src/invoice.ts", &["total"]);
    let another_file = is_named_at(&web, then, "src/title.ts", &["total"]);
    let no_such_file = is_named_at(&web, then, "src/invoice.ts/src/invoice.ts", &["total"]);
    let after_the_fix = is_named_at(&web, &shop.web_reading_paid, "src/invoice.ts", &["total"]);

    assert!(reads_it);
    assert!(!another_file);
    assert!(!no_such_file);
    assert!(!after_the_fix);
}
