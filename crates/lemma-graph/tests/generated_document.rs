//! Generated documents, offline: a provider whose committed `OpenAPI`
//! document is written by a command from its code, a consumer that calls
//! it, and changes to the code that nobody regenerated the document for.
//! The command is a shell script, so nothing here depends on a framework.

#![allow(clippy::unwrap_used, reason = "test setup should fail loudly")]

use std::path::Path;
use std::process::Command;

use lemma_graph::config::Level;
use lemma_graph::{Change, CheckOptions, Report, Severity, check};
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

/// The provider's generator: the request's fields come from the code's
/// field list, one per line, `name` or `name required`. Each run is
/// counted in `$RUNS`.
const GENERATOR: &str = r#"#!/bin/sh
echo run >> "$RUNS"
properties=$(awk '{ printf "%s\"%s\": { \"type\": \"string\" }", sep, $1; sep = ", " }' src/charges.fields)
required=$(awk '$2 == "required" { printf "%s\"%s\"", sep, $1; sep = ", " }' src/charges.fields)
cat > "$LEMMA_OUTPUT" <<EOF
{ "openapi": "3.0.0", "paths": { "/v1/charges": { "post": {
  "operationId": "createCharge",
  "requestBody": { "content": { "application/json": { "schema": {
    "type": "object", "properties": { $properties }, "required": [ $required ] } } } },
  "responses": { "200": { "description": "OK" } } } } } }
EOF
"#;

/// What the generator writes for a `customer`-only field list, committed.
const COMMITTED: &str = r#"{ "openapi": "3.0.0", "paths": { "/v1/charges": { "post": {
  "operationId": "createCharge",
  "requestBody": { "content": { "application/json": { "schema": {
    "type": "object", "properties": { "customer": { "type": "string" } }, "required": [ ] } } } },
  "responses": { "200": { "description": "OK" } } } } } }
"#;

const CONSUMER_TS: &str = r#"export async function charge(customer: string) {
  const response = await fetch("/v1/charges", {
    method: "POST",
    body: JSON.stringify({ customer }),
  });
  return response.json();
}
"#;

/// A provider generating its document with a command, a consumer, and a
/// parent composing them, under `workspace`; the provider's code at one
/// commit asks for `customer` alone.
fn system(workspace: &Path) {
    let provider = workspace.join("billing");
    let consumer = workspace.join("web");
    let runs = workspace.join("runs.log");
    write(
        &provider.join("mod.lm"),
        &format!(
            "module billing\n\nimport * as http from lemma.http\n\n\
             let spec = command(\"sh tools/openapi.sh\", env: {{ RUNS: \"{}\" }}, inputs: [\"src\", \"tools\"])\n\n\
             export service api {{\n    operations: http.openapi(\"openapi.json\", command: spec)\n}}\n",
            runs.display()
        ),
    );
    write(&provider.join("tools/openapi.sh"), GENERATOR);
    write(&provider.join("src/charges.fields"), "customer\n");
    write(&provider.join("openapi.json"), COMMITTED);
    write(&provider.join("README.md"), "Billing.\n");
    git(&provider, &["init", "-q"]);
    git(&provider, &["add", "."]);
    git(&provider, &["commit", "-q", "-m", "base"]);
    write(
        &consumer.join("mod.lm"),
        "module web\n\nexport service web {}\n",
    );
    write(&consumer.join("src/charge.ts"), CONSUMER_TS);
    git(&consumer, &["init", "-q"]);
    write(
        &workspace.join("mod.lm"),
        "module system\n\nworkspace {\n    repo billing = local(\"billing\")\n    repo web = local(\"web\")\n}\n",
    );
}

/// The handler change nobody regenerated the document for: a required
/// `currency`.
fn require_currency(workspace: &Path) {
    write(
        &workspace.join("billing/src/charges.fields"),
        "customer\ncurrency required\n",
    );
}

fn run_check(workspace: &Path, base: &str, head: Option<&str>) -> Report {
    let options = CheckOptions {
        change: Some(Change {
            repository: "#billing".to_owned(),
            base: Some(base.to_owned()),
            head: head.map(str::to_owned),
        }),
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
    };
    check(&workspace.join("mod.lm"), &options, &ReferenceEngine, None).unwrap()
}

/// The checks of the errors on the consumer's call.
fn errors_on_the_call(report: &Report) -> Vec<&str> {
    report
        .findings
        .iter()
        .filter(|finding| finding.link.ends_with(" -> POST /v1/charges"))
        .filter(|finding| finding.severity == Severity::Error)
        .map(|finding| finding.check.as_str())
        .collect()
}

fn runs(workspace: &Path) -> usize {
    std::fs::read_to_string(workspace.join("runs.log"))
        .unwrap_or_default()
        .lines()
        .count()
}

#[test]
fn an_uncommitted_code_change_is_checked_before_the_document_is_regenerated() {
    let workspace = tempfile::tempdir().unwrap();
    system(workspace.path());
    require_currency(workspace.path());

    let report = run_check(workspace.path(), "HEAD", None);

    assert_eq!(
        errors_on_the_call(&report),
        ["consumer_misses_new_required_field"]
    );
}

#[test]
fn a_committed_code_change_with_a_stale_document_is_checked() {
    let workspace = tempfile::tempdir().unwrap();
    system(workspace.path());
    require_currency(workspace.path());
    git(
        &workspace.path().join("billing"),
        &["commit", "-q", "-am", "head"],
    );

    let report = run_check(workspace.path(), "HEAD~1", Some("HEAD"));

    assert_eq!(
        errors_on_the_call(&report),
        ["consumer_misses_new_required_field"]
    );
}

#[test]
fn the_command_runs_again_only_when_its_inputs_change() {
    let workspace = tempfile::tempdir().unwrap();
    system(workspace.path());
    let first = run_check(workspace.path(), "HEAD", None);
    let after_first = runs(workspace.path());

    write(
        &workspace.path().join("billing/README.md"),
        "Billing, edited.\n",
    );
    let outside = run_check(workspace.path(), "HEAD", None);
    let after_outside = runs(workspace.path());
    require_currency(workspace.path());
    let inside = run_check(workspace.path(), "HEAD", None);
    let after_inside = runs(workspace.path());

    assert!(after_first > 0, "the command never ran");
    assert_eq!(errors_on_the_call(&first), Vec::<&str>::new());
    assert_eq!(
        after_outside, after_first,
        "an edit outside `inputs` ran it"
    );
    assert_eq!(errors_on_the_call(&outside), Vec::<&str>::new());
    assert_eq!(
        after_inside,
        after_first + 1,
        "an edit inside `inputs` did not"
    );
    assert_eq!(
        errors_on_the_call(&inside),
        ["consumer_misses_new_required_field"]
    );
}
