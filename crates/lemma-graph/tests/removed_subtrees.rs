//! A field removed with the fields under it is one change: removing or
//! renaming `customer` takes `customer.email` and `customer.address.city`
//! with it. Each call is told once, about the outermost removed field, at
//! the severity the strongest evidence gives, in change-driven and full
//! evaluation alike.

#![allow(clippy::unwrap_used, reason = "test setup should fail loudly")]

use std::path::Path;
use std::process::Command;

use lemma_graph::config::Level;
use lemma_graph::{Change, CheckOptions, Severity, check};
use lemma_logic::ReferenceEngine;
use lemma_verify::Evaluation;

const CUSTOMER: &str = r#""customer": { "type": "object", "properties": {
    "id": { "type": "string" },
    "email": { "type": "string" },
    "address": { "type": "object", "properties": { "city": { "type": "string" } } } } }"#;
const CUSTOMER_WITHOUT_EMAIL: &str = r#""customer": { "type": "object", "properties": {
    "id": { "type": "string" },
    "address": { "type": "object", "properties": { "city": { "type": "string" } } } } }"#;
const RENAMED: &str = r#""buyer": { "type": "object", "properties": {
    "id": { "type": "string" },
    "email": { "type": "string" },
    "address": { "type": "object", "properties": { "city": { "type": "string" } } } } }"#;

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

/// A document whose invoice is read and created with `fields` beside its
/// `id`, in the response and in the request alike.
fn document(fields: &str) -> String {
    let schema = format!(
        r#"{{ "type": "object", "properties": {{ "id": {{ "type": "string" }}, {fields} }} }}"#
    );
    format!(
        r#"{{ "openapi": "3.0.0", "info": {{ "title": "B", "version": "1" }}, "paths": {{
  "/v1/invoices": {{ "get": {{ "operationId": "getInvoice",
    "responses": {{ "200": {{ "description": "ok", "content": {{ "application/json": {{ "schema": {schema} }} }} }} }} }},
  "post": {{ "operationId": "createInvoice",
    "requestBody": {{ "content": {{ "application/json": {{ "schema": {schema} }} }} }},
    "responses": {{ "200": {{ "description": "ok" }} }} }} }} }} }}"#
    )
}

/// A service whose last commit turns the invoice's `before` into `after`,
/// and a consumer whose one file is `consumer`.
fn workspace(before: &str, after: &str, consumer: &str) -> tempfile::TempDir {
    let workspace = tempfile::tempdir().unwrap();
    let billing = workspace.path().join("billing");
    write(
        &billing.join("mod.lm"),
        "module billing\n\nimport * as http from lemma.http\n\nexport service api {\n    operations: http.openapi(\"openapi.json\")\n}\n",
    );
    write(&billing.join("openapi.json"), &document(before));
    commit_all(&billing);
    write(&billing.join("openapi.json"), &document(after));
    git(&billing, &["commit", "-q", "-am", "change the invoice"]);

    let web = workspace.path().join("web");
    write(
        &web.join("mod.lm"),
        "module web\n\nexport service web {\n    index: { include: [\"src\"] }\n}\n",
    );
    write(&web.join("src/invoice.ts"), consumer);
    commit_all(&web);

    write(
        &workspace.path().join("mod.lm"),
        "module system\n\nworkspace {\n    repo billing = local(\"billing\")\n    repo web = local(\"web\")\n}\n",
    );
    workspace
}

fn run(workspace: &Path, evaluation: Evaluation) -> Vec<(Severity, String, String)> {
    let options = CheckOptions {
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
    let report = check(&workspace.join("mod.lm"), &options, &ReferenceEngine, None).unwrap();
    let mut found: Vec<(Severity, String, String)> = report
        .findings
        .iter()
        .filter(|finding| finding.message.contains("src/invoice.ts"))
        .map(|finding| {
            (
                finding.severity,
                finding.check.clone(),
                finding.message.clone(),
            )
        })
        .collect();
    found.sort();
    found
}

/// The findings at the consumer, the same in both evaluations.
fn findings(workspace: &Path) -> Vec<(Severity, String, String)> {
    let change_driven = run(workspace, Evaluation::Demand);
    assert_eq!(change_driven, run(workspace, Evaluation::Full));
    change_driven
}

#[test]
fn a_renamed_field_read_with_fields_under_it_is_one_error() {
    let consumer = "export async function invoiceEmail() {\n  const res = await fetch('/v1/invoices')\n  const body = await res.json()\n  return [body.customer.id, body.customer.email, body.customer.address.city]\n}\n";
    let workspace = workspace(CUSTOMER, RENAMED, consumer);

    let found = findings(workspace.path());

    assert_eq!(found.len(), 1, "{found:#?}");
    let (severity, check, message) = &found[0];
    assert_eq!(*severity, Severity::Error);
    assert_eq!(check, "consumer_reads_removed_response_field");
    assert!(
        message.contains("response field `customer` was removed"),
        "{message}"
    );
}

#[test]
fn a_renamed_field_only_named_near_the_call_is_one_warning() {
    let consumer = "export function invoiceEmail(show: (email: string, city: string) => void) {\n  fetch('/v1/invoices').then((res) => res.json()).then((invoice) => {\n    show(invoice.customer.email, invoice.customer.address.city)\n  })\n}\n";
    let workspace = workspace(CUSTOMER, RENAMED, consumer);

    let found = findings(workspace.path());

    assert_eq!(found.len(), 1, "{found:#?}");
    let (severity, check, message) = &found[0];
    assert_eq!(*severity, Severity::Warning);
    assert_eq!(check, "consumer_uses_removed_response_field");
    assert!(
        message.contains("response field `customer` was removed"),
        "{message}"
    );
}

#[test]
fn a_field_under_the_removed_one_read_off_the_body_makes_it_an_error() {
    // An axios instance whose interceptor returns the body: `customer` is
    // one step off the result, `email` two.
    let consumer = "export async function invoiceEmail() {\n  const invoice = await axios.get('/v1/invoices')\n  return invoice.customer.email\n}\n";
    let workspace = workspace(CUSTOMER, RENAMED, consumer);

    let found = findings(workspace.path());

    assert_eq!(found.len(), 1, "{found:#?}");
    let (severity, check, message) = &found[0];
    assert_eq!(*severity, Severity::Error);
    assert_eq!(check, "consumer_reads_removed_response_field");
    assert!(
        message.contains("response field `customer` was removed"),
        "{message}"
    );
}

#[test]
fn a_field_removed_under_a_kept_one_is_still_reported() {
    let consumer = "export async function invoiceEmail() {\n  const res = await fetch('/v1/invoices')\n  const body = await res.json()\n  return body.customer.email\n}\n";
    let workspace = workspace(CUSTOMER, CUSTOMER_WITHOUT_EMAIL, consumer);

    let found = findings(workspace.path());

    assert_eq!(found.len(), 1, "{found:#?}");
    let (severity, _, message) = &found[0];
    assert_eq!(*severity, Severity::Error);
    assert!(
        message.contains("response field `customer.email` was removed"),
        "{message}"
    );
}

#[test]
fn a_renamed_request_field_sent_with_fields_under_it_is_reported_once() {
    let consumer = "export async function createInvoice(email: string, city: string) {\n  return axios.post('/v1/invoices', { customer: { email, address: { city } } })\n}\n";
    let workspace = workspace(CUSTOMER, RENAMED, consumer);

    let found = findings(workspace.path());

    let about_request: Vec<&(Severity, String, String)> = found
        .iter()
        .filter(|(_, _, message)| message.contains("request field"))
        .collect();
    assert_eq!(about_request.len(), 1, "{found:#?}");
    assert!(
        about_request[0]
            .2
            .contains("request field `customer` was removed"),
        "{found:#?}"
    );
}
