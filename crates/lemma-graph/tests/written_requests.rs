//! A removed response field read one step off a call's result is read
//! off the body.

#![allow(clippy::unwrap_used, reason = "test setup should fail loudly")]

use std::collections::BTreeMap;
use std::path::Path;
use std::process::Command;

use lemma_graph::config::Level;
use lemma_graph::{Change, CheckOptions, Severity, check};
use lemma_logic::ReferenceEngine;

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

fn commit(repository: &Path, subject: &str) {
    if !repository.join(".git").exists() {
        git(repository, &["init", "-q"]);
    }
    git(repository, &["add", "."]);
    git(repository, &["commit", "-q", "-m", subject]);
}

/// Billing's document. `POST /v1/users` takes `name` and a `profile` with
/// a `bio`, and requires what `required` and `required_in_profile` list;
/// `GET /v1/users` takes `limit`, and requires `parameters` too;
/// `GET /v1/invoices` returns `id`, `customer.email` and `returned`.
fn document(required: &str, required_in_profile: &str, parameters: &str, returned: &str) -> String {
    format!(
        r#"{{ "openapi": "3.0.0", "info": {{ "title": "B", "version": "1" }}, "paths": {{
  "/v1/users": {{
    "post": {{ "operationId": "createUser",
      "requestBody": {{ "content": {{ "application/json": {{ "schema": {{ "type": "object", "required": ["name", "profile"{required}],
        "properties": {{ "name": {{ "type": "string" }}, "email": {{ "type": "string" }},
          "profile": {{ "type": "object", "required": [{required_in_profile}],
            "properties": {{ "bio": {{ "type": "string" }}, "city": {{ "type": "string" }} }} }} }} }} }} }} }},
      "responses": {{ "200": {{ "description": "ok" }} }} }},
    "get": {{ "operationId": "listUsers",
      "parameters": [ {{ "name": "limit", "in": "query", "schema": {{ "type": "integer" }} }}{parameters} ],
      "responses": {{ "200": {{ "description": "ok" }} }} }} }},
  "/v1/invoices": {{
    "get": {{ "operationId": "getInvoice",
      "responses": {{ "200": {{ "description": "ok", "content": {{ "application/json": {{ "schema": {{ "type": "object",
        "properties": {{ "id": {{ "type": "string" }}, "customer": {{ "type": "object", "properties": {{ "email": {{ "type": "string" }} }} }}{returned} }} }} }} }} }} }} }} }} }} }}"#
    )
}

const AMOUNT_AND_STATUS: &str =
    r#", "amount": { "type": "number" }, "status": { "type": "string" }"#;
/// A workspace of billing, whose last commit changes its document from
/// `before` to `after`, and web, whose `src/calls.ts` is `calls`.
fn workspace(before: &str, after: &str, calls: &str) -> tempfile::TempDir {
    let directory = tempfile::tempdir().unwrap();
    let billing = directory.path().join("billing");
    let web = directory.path().join("web");
    write(
        &billing.join("mod.lm"),
        "module billing\n\nimport * as http from lemma.http\n\nexport service api {\n    operations: http.openapi(\"openapi.json\")\n}\n",
    );
    write(&billing.join("openapi.json"), before);
    commit(&billing, "start billing");
    write(&billing.join("openapi.json"), after);
    commit(&billing, "change the contract");
    write(
        &web.join("mod.lm"),
        "module web\n\nexport service web {\n    index: { include: [\"src\"] }\n}\n",
    );
    write(&web.join("src/calls.ts"), calls);
    commit(&web, "start web");
    write(
        &directory.path().join("mod.lm"),
        "module system\n\nworkspace {\n    repo billing = local(\"billing\")\n    repo web = local(\"web\")\n}\n",
    );
    directory
}

/// The findings of billing's last commit, as `(severity, requirement,
/// calling function)`, read whole and read change-driven.
fn found(workspace: &Path) -> Vec<(Severity, String, String)> {
    let asked = |evaluation| CheckOptions {
        change: Some(Change {
            repository: "billing".to_owned(),
            base: Some("HEAD~1".to_owned()),
            head: Some("HEAD".to_owned()),
        }),
        pins: BTreeMap::new(),
        level: Some(Level::Check),
        cache: workspace.join("cache"),
        cloning: lemma_graph::repos::Cloning::Allowed,
        explain: false,
        relations: Vec::new(),
        facts_cache: None,
        evaluation,
        list_links: false,
        run_checkers: false,
        runs_base_checkers: false,
        select: None,
        warm: None,
        environment: None,
        runtime: None,
    };
    let findings = |evaluation| -> Vec<(Severity, String, String)> {
        let report = check(
            &workspace.join("mod.lm"),
            &asked(evaluation),
            &ReferenceEngine,
            None,
        )
        .unwrap();
        report
            .findings
            .into_iter()
            .filter(|finding| finding.severity != Severity::Info)
            .map(|finding| {
                let function = finding
                    .message
                    .split_whitespace()
                    .next()
                    .and_then(|consumer| consumer.rsplit("::").next())
                    .unwrap_or_default()
                    .to_owned();
                (finding.severity, finding.check, function)
            })
            .collect()
    };
    let whole = findings(lemma_verify::Evaluation::Full);
    let change_driven = findings(lemma_verify::Evaluation::Demand);
    assert_eq!(whole, change_driven);
    whole
}

fn finding(severity: Severity, check: &str, function: &str) -> (Severity, String, String) {
    (severity, check.to_owned(), function.to_owned())
}

#[test]
fn a_removed_field_read_one_step_off_a_result_is_an_error_at_the_top_of_the_response() {
    let calls = "export async function amount(http: Http) {\n  const invoice = await http.get('/v1/invoices')\n  return invoice.amount\n}\n\nexport async function taken(http: Http) {\n  const { amount } = await http.get('/v1/invoices')\n  return amount\n}\n\nexport async function state(http: Http) {\n  const invoice = await http.get('/v1/invoices')\n  return invoice.status\n}\n";
    let shop = workspace(
        &document("", "", "", AMOUNT_AND_STATUS),
        &document("", "", "", ""),
        calls,
    );

    let findings = found(shop.path());

    assert_eq!(
        findings,
        [
            finding(
                Severity::Error,
                "consumer_reads_removed_response_field",
                "amount"
            ),
            finding(
                Severity::Error,
                "consumer_reads_removed_response_field",
                "taken"
            ),
            finding(
                Severity::Warning,
                "consumer_uses_removed_response_field",
                "state"
            ),
        ]
    );
}

#[test]
fn a_name_from_deeper_in_the_response_read_one_step_off_a_result_stays_a_warning() {
    let calls = "export async function email(http: Http) {\n  const invoice = await http.get('/v1/invoices')\n  return invoice.email\n}\n";
    let before = document("", "", "", "");
    let after = before.replace(
        r#""customer": { "type": "object", "properties": { "email": { "type": "string" } } }"#,
        r#""customer": { "type": "object", "properties": { "name": { "type": "string" } } }"#,
    );
    let shop = workspace(&before, &after, calls);

    let findings = found(shop.path());

    assert_eq!(
        findings,
        [finding(
            Severity::Warning,
            "consumer_uses_removed_response_field",
            "email"
        )]
    );
}
