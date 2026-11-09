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
        &provider.join("mod.lm"),
        "module billing\n\nimport * as http from lemma.http\n\nexport service api {\n    operations: http.openapi(\"openapi.json\")\n}\n",
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

#[test]
fn changes_are_checked_against_code_that_calls_without_declaring() {
    let workspace = tempfile::tempdir().unwrap();
    system(workspace.path());
    let options = CheckOptions {
        change: Some(Change {
            repository: "#billing".to_owned(),
            base: Some("HEAD~1".to_owned()),
            head: Some("HEAD".to_owned()),
        }),
        level: Some(Level::Check),
        cache: workspace.path().join("cache"),
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
    };

    let report = check(
        &workspace.path().join("mod.lm"),
        &options,
        &ReferenceEngine,
        None,
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

/// The checks of the problems the system's change raises on its
/// link, with `select`.
fn selected_problems(workspace: &Path, select: &str) -> Result<Vec<String>, String> {
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
        evaluation: lemma_verify::Evaluation::Demand,
        list_links: false,
        run_checkers: true,
        runs_base_checkers: false,
        select: Some(select.to_owned()),
        warm: None,
        environment: None,
    };
    let report = check(&workspace.join("mod.lm"), &options, &ReferenceEngine, None)
        .map_err(|error| error.to_string())?;
    Ok(report
        .findings
        .iter()
        .filter(|finding| finding.link.ends_with(" -> POST /v1/charges"))
        .map(|finding| finding.check.clone())
        .collect())
}

#[test]
fn a_description_without_a_program_selects_one_built_in_requirement_by_name() {
    let workspace = tempfile::tempdir().unwrap();
    system(workspace.path());

    let bare = selected_problems(workspace.path(), "consumer_uses_removed_response_field");
    let qualified = selected_problems(
        workspace.path(),
        "lemma.http.consumer_misses_new_required_field",
    );
    let unknown = selected_problems(workspace.path(), "consumer_forgets_everything");

    assert_eq!(bare.unwrap(), ["consumer_uses_removed_response_field"]);
    assert_eq!(qualified.unwrap(), ["consumer_misses_new_required_field"]);
    assert!(
        unknown
            .unwrap_err()
            .contains("no check `consumer_forgets_everything`")
    );
}

/// The findings on the link of the system's change, checked
/// with `warm`.
fn link_findings(
    workspace: &Path,
    warm: Option<&std::sync::Arc<lemma_graph::Warm>>,
) -> Vec<String> {
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
        evaluation: lemma_verify::Evaluation::Demand,
        list_links: false,
        run_checkers: true,
        runs_base_checkers: false,
        select: None,
        warm: warm.cloned(),
        environment: None,
    };
    let report = check(&workspace.join("mod.lm"), &options, &ReferenceEngine, None).unwrap();
    report
        .findings
        .iter()
        .filter(|finding| finding.link.ends_with(" -> POST /v1/charges"))
        .map(|finding| {
            format!(
                "{:?} {} {}",
                finding.severity, finding.check, finding.message
            )
        })
        .collect()
}

#[test]
fn a_warm_check_answers_as_a_fresh_one_once_an_edited_repository_is_forgotten() {
    let workspace = tempfile::tempdir().unwrap();
    system(workspace.path());
    // Committed, as a real repository is: its files are listed through Git.
    let web = workspace.path().join("web");
    git(&web, &["add", "."]);
    git(&web, &["commit", "-q", "-m", "web"]);
    let warm = std::sync::Arc::new(lemma_graph::Warm::default());

    let warm_before = link_findings(workspace.path(), Some(&warm));
    let fresh_before = link_findings(workspace.path(), None);
    // The consumer now sends the newly required field.
    let fixed = CONSUMER_TS.replace("{ customer }", "{ customer, currency: \"usd\" }");
    write(&workspace.path().join("web/src/charge.ts"), &fixed);
    warm.forget_working(&web);
    let warm_after = link_findings(workspace.path(), Some(&warm));
    let fresh_after = link_findings(workspace.path(), None);

    assert_eq!(warm_before, fresh_before);
    assert_eq!(warm_after, fresh_after);
    assert_ne!(warm_after, warm_before);
}

/// The same system composed by modules, whose parent adds a requirement of
/// its own and, given `main`, a `check main`.
fn system_of_modules(workspace: &Path, main: &str) {
    system(workspace);
    write(
        &workspace.join("billing/mod.lm"),
        "module billing\nimport * as http from lemma.http\nexport service api {\n    operations: http.openapi(\"openapi.json\")\n}\n",
    );
    git(&workspace.join("billing"), &["add", "-A"]);
    git(
        &workspace.join("billing"),
        &["commit", "-q", "--amend", "--no-edit"],
    );
    write(
        &workspace.join("web/mod.lm"),
        "module web\nexport service web {}\n",
    );
    let parent = format!(
        r#"module system

import {{ calls }} from lemma.http

workspace {{
    repo billing = local("billing")
    repo web = local("web")
}}

require charges_are_called {{
    severity: warning
    when: calls(c, op, f, l), op == "POST /v1/charges"
    message: "{{c}} calls {{op}}"
}}
{main}"#
    );
    write(&workspace.join("mod.lm"), &parent);
}

fn checks_of(workspace: &Path) -> Vec<(Severity, String)> {
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
        evaluation: lemma_verify::Evaluation::Demand,
        list_links: false,
        run_checkers: true,
        runs_base_checkers: false,
        select: None,
        warm: None,
        environment: None,
    };
    let report = check(&workspace.join("mod.lm"), &options, &ReferenceEngine, None).unwrap();
    report
        .findings
        .iter()
        .filter(|finding| finding.link.ends_with(" -> POST /v1/charges"))
        .map(|finding| (finding.severity, finding.check.clone()))
        .collect()
}

#[test]
fn a_parent_module_runs_its_own_requirements_beside_the_http_contracts() {
    let workspace = tempfile::tempdir().unwrap();
    system_of_modules(workspace.path(), "");

    let found = checks_of(workspace.path());

    assert_eq!(
        found,
        [
            (
                Severity::Error,
                "consumer_misses_new_required_field".to_owned()
            ),
            (Severity::Warning, "charges_are_called".to_owned()),
            (
                Severity::Warning,
                "consumer_uses_removed_response_field".to_owned()
            ),
        ]
    );
}

#[test]
fn a_check_main_selects_what_runs() {
    let workspace = tempfile::tempdir().unwrap();
    system_of_modules(
        workspace.path(),
        "\ncheck main {\n    run charges_are_called\n    run lemma.http.consumer_misses_new_required_field { severity: warning }\n}\n",
    );

    let found = checks_of(workspace.path());

    assert_eq!(
        found,
        [
            (Severity::Warning, "charges_are_called".to_owned()),
            (
                Severity::Warning,
                "consumer_misses_new_required_field".to_owned()
            ),
        ]
    );
}

#[test]
fn select_runs_another_check_and_an_unknown_one_is_refused() {
    let workspace = tempfile::tempdir().unwrap();
    system_of_modules(
        workspace.path(),
        "\ncheck main {\n    run charges_are_called\n}\n\ncheck strict_http {\n    run lemma.http.*\n}\n",
    );
    let options = |select: &str| CheckOptions {
        change: Some(Change {
            repository: "#billing".to_owned(),
            base: Some("HEAD~1".to_owned()),
            head: Some("HEAD".to_owned()),
        }),
        level: Some(Level::Check),
        cache: workspace.path().join("cache"),
        cloning: lemma_graph::repos::Cloning::Allowed,
        explain: false,
        relations: Vec::new(),
        facts_cache: None,
        evaluation: lemma_verify::Evaluation::Demand,
        list_links: false,
        run_checkers: true,
        runs_base_checkers: false,
        select: Some(select.to_owned()),
        warm: None,
        environment: None,
    };
    let entry = workspace.path().join("mod.lm");

    let report = check(&entry, &options("strict_http"), &ReferenceEngine, None).unwrap();
    let unknown = check(&entry, &options("nightly"), &ReferenceEngine, None);

    let checks: Vec<&str> = report
        .findings
        .iter()
        .filter(|finding| finding.link.ends_with(" -> POST /v1/charges"))
        .map(|finding| finding.check.as_str())
        .collect();
    assert_eq!(
        checks,
        [
            "consumer_misses_new_required_field",
            "consumer_uses_removed_response_field"
        ]
    );
    assert!(
        unknown
            .unwrap_err()
            .to_string()
            .contains("no check `nightly`")
    );
}

#[test]
fn a_change_may_name_its_repository_as_a_module_does() {
    let workspace = tempfile::tempdir().unwrap();
    system_of_modules(workspace.path(), "");
    let options = CheckOptions {
        change: Some(Change {
            repository: "billing".to_owned(),
            base: Some("HEAD~1".to_owned()),
            head: Some("HEAD".to_owned()),
        }),
        level: Some(Level::Check),
        cache: workspace.path().join("cache"),
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
    };

    let report = check(
        &workspace.path().join("mod.lm"),
        &options,
        &ReferenceEngine,
        None,
    )
    .unwrap();

    assert_eq!(report.inferred_links, Some(1));
}
