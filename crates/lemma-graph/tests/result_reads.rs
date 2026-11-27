//! A response field a change removes, read by consumers in two ways: off
//! the body of the call's result, which fails at run time and is an
//! error, and as a name one step off the result, which may be the response
//! object's own property (`res.status`) and stays a warning.

#![allow(clippy::unwrap_used, reason = "test setup should fail loudly")]

use std::path::Path;
use std::process::Command;

use lemma_graph::config::Level;
use lemma_graph::{Change, CheckOptions, Report, Severity, check};
use lemma_logic::ReferenceEngine;
use lemma_verify::Evaluation;

// Reads `status` off the body `res.json()` returns.
const FETCHED: &str = "export async function invoiceStatus(id: string) {\n  const res = await fetch(`/v1/invoices/${id}`)\n  const body = await res.json()\n  return body.status\n}\n";
// Reads `status` off the response itself: the HTTP status, not the field.
const RESPONDED: &str = "export async function invoiceFound(id: string) {\n  const res = await fetch(`/v1/invoices/${id}`)\n  return res.status === 200\n}\n";

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

fn write(path: &Path, text: &str) {
    std::fs::create_dir_all(path.parent().unwrap()).unwrap();
    std::fs::write(path, text).unwrap();
}

fn commit_all(repository: &Path) {
    git(repository, &["init", "-q"]);
    git(repository, &["add", "."]);
    git(repository, &["commit", "-q", "-m", "base"]);
}

/// A document whose invoice has an `id` and, if `with_status`, a `status`.
fn document(with_status: bool) -> String {
    let status = if with_status {
        r#", "status": { "type": "string" }"#
    } else {
        ""
    };
    format!(
        r#"{{ "openapi": "3.0.0", "info": {{ "title": "B", "version": "1" }}, "paths": {{
  "/v1/invoices/{{id}}": {{ "get": {{ "operationId": "getInvoice",
    "parameters": [{{ "name": "id", "in": "path", "required": true, "schema": {{ "type": "string" }} }}],
    "responses": {{ "200": {{ "description": "ok", "content": {{ "application/json": {{ "schema": {{
      "type": "object", "properties": {{ "id": {{ "type": "string" }}{status} }} }} }} }} }} }} }} }} }} }}"#
    )
}

/// A service whose last commit removes the invoice's `status`, and a
/// consumer reading it in both ways.
fn workspace() -> tempfile::TempDir {
    let workspace = tempfile::tempdir().unwrap();
    let billing = workspace.path().join("billing");
    write(
        &billing.join("mod.lm"),
        "module billing\n\nimport * as http from lemma.http\n\nexport service api {\n    operations: http.openapi(\"openapi.json\")\n}\n",
    );
    write(&billing.join("openapi.json"), &document(true));
    commit_all(&billing);
    write(&billing.join("openapi.json"), &document(false));
    git(&billing, &["commit", "-q", "-am", "remove status"]);

    let web = workspace.path().join("web");
    write(
        &web.join("mod.lm"),
        "module web\n\nexport service web {\n    index: { include: [\"src\"] }\n}\n",
    );
    write(&web.join("src/fetched.ts"), FETCHED);
    write(&web.join("src/responded.ts"), RESPONDED);
    commit_all(&web);

    write(
        &workspace.path().join("mod.lm"),
        "module system\n\nworkspace {\n    repo billing = local(\"billing\")\n    repo web = local(\"web\")\n}\n",
    );
    workspace
}

fn run(workspace: &Path, evaluation: Evaluation) -> Report {
    let options = CheckOptions {
        pins: std::collections::BTreeMap::new(),
        change: Some(Change {
            repository: "#billing".to_owned(),
            base: Some("HEAD~1".to_owned()),
            head: Some("HEAD".to_owned()),
        }),
        level: Some(Level::Check),
        cache: workspace.join("cache"),
        cloning: lemma_graph::repos::Cloning::Allowed,
        explain: false,
        relations: Vec::new(),
        facts_cache: None,
        evaluation,
        list_links: false,
        run_checkers: true,
        runs_base_checkers: false,
        select: None,
        warm: None,
        environment: None,
        runtime: None,
    };
    check(&workspace.join("mod.lm"), &options, &ReferenceEngine, None).unwrap()
}

/// Each finding about the removed field, as severity, check and the
/// consumer's file, the same in change-driven and full evaluation.
fn findings(workspace: &Path) -> Vec<(Severity, String, String)> {
    let read = |evaluation| {
        let mut found: Vec<(Severity, String, String)> = run(workspace, evaluation)
            .findings
            .iter()
            .filter(|finding| finding.message.contains("`status`"))
            .map(|finding| {
                let file = ["fetched.ts", "responded.ts"]
                    .into_iter()
                    .find(|file| finding.message.contains(file))
                    .unwrap_or("?");
                (finding.severity, finding.check.clone(), file.to_owned())
            })
            .collect();
        found.sort();
        found
    };
    let change_driven = read(Evaluation::Demand);
    assert_eq!(change_driven, read(Evaluation::Full));
    change_driven
}

#[test]
fn a_removed_field_read_off_the_calls_body_is_an_error() {
    let workspace = workspace();

    let found = findings(workspace.path());

    assert!(
        found.contains(&(
            Severity::Error,
            "consumer_reads_removed_response_field".to_owned(),
            "fetched.ts".to_owned()
        )),
        "{found:#?}"
    );
    assert!(
        !found.iter().any(|(_, check, file)| file == "fetched.ts"
            && check == "consumer_uses_removed_response_field"),
        "{found:#?}"
    );
}

#[test]
fn a_name_read_off_the_response_itself_stays_a_warning() {
    let workspace = workspace();

    let found = findings(workspace.path());

    let responded: Vec<&(Severity, String, String)> = found
        .iter()
        .filter(|(_, _, file)| file == "responded.ts")
        .collect();
    assert_eq!(
        responded,
        [&(
            Severity::Warning,
            "consumer_uses_removed_response_field".to_owned(),
            "responded.ts".to_owned()
        )],
        "{found:#?}"
    );
}
