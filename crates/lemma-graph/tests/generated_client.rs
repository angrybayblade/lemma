//! A generated client whose copy of a service's document can be updated
//! apart from the code generated from it: a parameter the service removes is
//! reported while either of them still has it, and once, in full and
//! change-driven evaluation alike.

#![allow(clippy::unwrap_used, reason = "test setup should fail loudly")]

use std::path::Path;
use std::process::Command;

use lemma_graph::config::Level;
use lemma_graph::{Change, CheckOptions, Severity, check};
use lemma_logic::ReferenceEngine;
use lemma_verify::Evaluation;

const SEARCH: &str = r#"{ "name": "search", "in": "query", "schema": { "type": "string" } }"#;
const LIMIT: &str = r#"{ "name": "limit", "in": "query", "schema": { "type": "integer" } }"#;

const SENDS_SEARCH: &str = "class ChargesResource:\n    def list(self, search=None, limit=None):\n        query = {\"search\": search, \"limit\": limit}\n        return self._client._request(\"get\", \"/v1/charges\", query=query)\n";
const REGENERATED: &str = "class ChargesResource:\n    def list(self, limit=None):\n        query = {\"limit\": limit}\n        return self._client._request(\"get\", \"/v1/charges\", query=query)\n";

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

fn document(parameters: &str) -> String {
    format!(
        r#"{{ "openapi": "3.0.0", "info": {{ "title": "B", "version": "1" }}, "paths": {{ "/v1/charges": {{ "get": {{
  "operationId": "listCharges", "parameters": [{parameters}],
  "responses": {{ "200": {{ "description": "ok" }} }} }} }} }} }}"#
    )
}

/// A service whose last commit removes `search`, and a client of it with
/// the given copy of the document and the given code.
fn workspace(client_document: &str, client_code: &str) -> tempfile::TempDir {
    let workspace = tempfile::tempdir().unwrap();
    let service = workspace.path().join("billing");
    write(
        &service.join("mod.lm"),
        "module billing\n\nimport * as http from lemma.http\n\nexport service api {\n    operations: http.openapi(\"openapi.json\")\n}\n",
    );
    write(
        &service.join("openapi.json"),
        &document(&format!("{SEARCH}, {LIMIT}")),
    );
    git(&service, &["init", "-q"]);
    git(&service, &["add", "."]);
    git(&service, &["commit", "-q", "-m", "base"]);
    write(&service.join("openapi.json"), &document(LIMIT));
    git(&service, &["commit", "-q", "-am", "remove search"]);
    let client = workspace.path().join("client");
    write(
        &client.join("mod.lm"),
        "module client\n\nimport * as http from lemma.http\n\nexport service client {\n    index: { include: [\"python\"] }\n    client: http.generated_client(generated_from: [], specs: [\"spec/openapi.json\"])\n}\n",
    );
    write(&client.join("spec/openapi.json"), client_document);
    write(&client.join("python/charges.py"), client_code);
    git(&client, &["init", "-q"]);
    write(
        &workspace.path().join("mod.lm"),
        "module system\n\nworkspace {\n    repo billing = local(\"billing\")\n    repo client = local(\"client\")\n}\n",
    );
    workspace
}

fn errors(workspace: &Path, evaluation: Evaluation) -> Vec<String> {
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
    };
    let report = check(&workspace.join("mod.lm"), &options, &ReferenceEngine, None).unwrap();
    report
        .findings
        .iter()
        .filter(|finding| finding.severity == Severity::Error)
        .map(|finding| finding.message.clone())
        .collect()
}

fn errors_agreeing(workspace: &Path) -> Vec<String> {
    let change_driven = errors(workspace, Evaluation::Demand);
    assert_eq!(change_driven, errors(workspace, Evaluation::Full));
    change_driven
}

#[test]
fn code_still_sending_a_parameter_its_updated_copy_dropped_is_reported() {
    let workspace = workspace(&document(LIMIT), SENDS_SEARCH);

    let errors = errors_agreeing(workspace.path());

    assert_eq!(errors.len(), 1, "{errors:?}");
    assert!(
        errors[0].contains("code still sends query parameter `search`"),
        "{errors:?}"
    );
}

#[test]
fn a_client_generated_again_has_nothing_to_report() {
    let workspace = workspace(&document(LIMIT), REGENERATED);

    assert_eq!(errors_agreeing(workspace.path()), Vec::<String>::new());
}

#[test]
fn a_copy_still_declaring_the_parameter_is_reported_once() {
    let copy = document(&format!("{SEARCH}, {LIMIT}"));
    let workspace = workspace(&copy, SENDS_SEARCH);

    let errors = errors_agreeing(workspace.path());

    assert_eq!(errors.len(), 1, "{errors:?}");
    assert!(
        errors[0].contains("still declares query parameter `search`"),
        "{errors:?}"
    );
}
