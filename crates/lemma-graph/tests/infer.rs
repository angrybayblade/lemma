//! Inferred links, offline: a provider documented by `OpenAPI`, a consumer
//! whose code calls it without declaring anything, and a change that drops
//! a response field the consumer reads and requires a field it never sends.

#![allow(clippy::unwrap_used, reason = "test setup should fail loudly")]

use std::path::Path;
use std::process::Command;

use lemma_graph::config::Level;
use lemma_graph::{Change, CheckOptions, Severity, check};
use lemma_logic::ReferenceEngine;

fn git(directory: &Path, args: &[&str]) {
    let status = Command::new("git")
        .arg("-C")
        .arg(directory)
        .args([
            "-c",
            "user.name=t",
            "-c",
            "user.email=t@t",
            "-c",
            "commit.gpgsign=false",
        ])
        .args(args)
        .status()
        .unwrap();
    assert!(status.success(), "git {args:?}");
}

fn write(path: &Path, text: &str) {
    std::fs::create_dir_all(path.parent().unwrap()).unwrap();
    std::fs::write(path, text).unwrap();
}

fn spec(request_schema: &str, response_properties: &str) -> String {
    format!(
        r#"{{ "openapi": "3.0.0", "paths": {{ "/v1/charges": {{ "post": {{
  "operationId": "createCharge",
  "requestBody": {{ "content": {{ "application/json": {{ "schema": {{
    "type": "object", {request_schema} }} }} }} }},
  "responses": {{ "200": {{ "content": {{ "application/json": {{ "schema": {{
    "type": "object", "properties": {{ {response_properties} }} }} }} }} }} }}
}} }} }} }}"#
    )
}

const CONSUMER_TS: &str = r#"export async function charge(customer: string) {
  const response = await fetch("/v1/charges", {
    method: "POST",
    body: JSON.stringify({ customer }),
  });
  const body = await response.json();
  return body.receipt_url;
}
"#;

/// A provider whose last commit changes its document, a consumer, and a
/// parent composing them, under `workspace`.
fn system(workspace: &Path) {
    let provider = workspace.join("billing");
    let consumer = workspace.join("web");
    write(
        &provider.join("lemma.yaml"),
        "services:\n  api:\n    exports: { openapi: [openapi.json] }\n",
    );
    write(
        &provider.join("openapi.json"),
        &spec(
            r#""properties": { "customer": { "type": "string" } }"#,
            r#""id": { "type": "string" }, "receipt_url": { "type": "string" }"#,
        ),
    );
    git(&provider, &["init", "-q"]);
    git(&provider, &["add", "."]);
    git(&provider, &["commit", "-q", "-m", "base"]);
    write(
        &provider.join("openapi.json"),
        &spec(
            r#""properties": { "customer": { "type": "string" }, "currency": { "type": "string" } },
                "required": ["currency"]"#,
            r#""id": { "type": "string" }"#,
        ),
    );
    git(&provider, &["commit", "-q", "-am", "head"]);
    write(&consumer.join("lemma.yaml"), "services:\n  web: {}\n");
    write(&consumer.join("src/charge.ts"), CONSUMER_TS);
    git(&consumer, &["init", "-q"]);
    write(
        &workspace.join("lemma.yaml"),
        "services:\n  \"#billing\": { path: billing }\n  \"#web\": { path: web }\n",
    );
}

#[test]
fn changes_are_checked_against_code_that_calls_without_declaring() {
    let workspace = tempfile::tempdir().unwrap();
    system(workspace.path());
    let options = CheckOptions {
        change: Some(Change {
            repository: "#billing".to_owned(),
            base: "HEAD~1".to_owned(),
            head: Some("HEAD".to_owned()),
        }),
        level: Some(Level::Check),
        cache: workspace.path().join("cache"),
        facts_cache: None,
        evaluation: lemma_verify::Evaluation::Demand,
    };

    let report = check(
        &workspace.path().join("lemma.yaml"),
        &options,
        &ReferenceEngine,
    )
    .unwrap();

    let problems: Vec<(Severity, &str)> = report
        .findings
        .iter()
        .filter(|finding| finding.link.ends_with(" -> POST /v1/charges"))
        .map(|finding| (finding.severity, finding.check.as_str()))
        .collect();
    assert_eq!(report.inferred_links, Some(1));
    assert_eq!(
        problems,
        [
            (Severity::Error, "consumer_misses_new_required_field"),
            (Severity::Warning, "consumer_uses_removed_response_field"),
        ]
    );
}
