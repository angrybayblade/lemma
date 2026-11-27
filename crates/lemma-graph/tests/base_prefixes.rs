//! A consumer that keeps the prefix all of a document's paths share in a
//! base URL and writes only the rest at each request: its requests are
//! calls of the document's operations, in a repository that names the
//! base and in no other.

#![allow(clippy::unwrap_used, reason = "test setup should fail loudly")]

use std::path::Path;
use std::process::Command;

use lemma_graph::config::Level;
use lemma_graph::{Change, CheckOptions, Report, Severity, check};
use lemma_logic::ReferenceEngine;
use lemma_verify::Evaluation;

// The base, named once, away from the requests.
const CONFIG: &str = "export const BILLING = 'https://billing.example/api'\n";
// Reads `status` off the body of a request written under the base.
const BASED: &str = "import { BILLING } from './config'\n\nexport async function invoiceStatus(id: string) {\n  const res = await fetch(`${BILLING}/invoices/${id}`)\n  const body = await res.json()\n  return body.status\n}\n";
// The same request in a repository that never names the base: the path
// may be any service's.
const UNBASED: &str = "const LEDGER = process.env.LEDGER_URL\n\nexport async function entryStatus(id: string) {\n  const res = await fetch(`${LEDGER}/invoices/${id}`)\n  const body = await res.json()\n  return body.status\n}\n";

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

/// A document whose every path starts with `/api`, and whose invoice has
/// an `id` and, if `with_status`, a `status`.
fn document(with_status: bool) -> String {
    let status = if with_status {
        r#", "status": { "type": "string" }"#
    } else {
        ""
    };
    format!(
        r#"{{ "openapi": "3.0.0", "info": {{ "title": "B", "version": "1" }}, "paths": {{
  "/api/customers": {{ "get": {{ "operationId": "listCustomers",
    "responses": {{ "200": {{ "description": "ok" }} }} }} }},
  "/api/invoices/{{id}}": {{ "get": {{ "operationId": "getInvoice",
    "parameters": [{{ "name": "id", "in": "path", "required": true, "schema": {{ "type": "string" }} }}],
    "responses": {{ "200": {{ "description": "ok", "content": {{ "application/json": {{ "schema": {{
      "type": "object", "properties": {{ "id": {{ "type": "string" }}{status} }} }} }} }} }} }} }} }} }} }}"#
    )
}

fn consumer(workspace: &Path, name: &str, files: &[(&str, &str)]) {
    let repository = workspace.join(name);
    write(
        &repository.join("mod.lm"),
        &format!(
            "module {name}\n\nexport service {name} {{\n    index: {{ include: [\"src\"] }}\n}}\n"
        ),
    );
    for (file, text) in files {
        write(&repository.join(file), text);
    }
    commit_all(&repository);
}

/// A service whose last commit removes the invoice's `status`, a consumer
/// naming its base, and one that does not.
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

    consumer(
        workspace.path(),
        "web",
        &[("src/config.ts", CONFIG), ("src/based.ts", BASED)],
    );
    consumer(workspace.path(), "ledger", &[("src/unbased.ts", UNBASED)]);

    write(
        &workspace.path().join("mod.lm"),
        "module system\n\nworkspace {\n    repo billing = local(\"billing\")\n    repo web = local(\"web\")\n    repo ledger = local(\"ledger\")\n}\n",
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

/// The consumer files with an error about the removed field, the same in
/// change-driven and full evaluation.
fn files_reading_the_removed_field(workspace: &Path) -> Vec<&'static str> {
    let read = |evaluation| {
        let report = run(workspace, evaluation);
        let mut files: Vec<&'static str> = ["based.ts", "unbased.ts"]
            .into_iter()
            .filter(|file| {
                report.findings.iter().any(|finding| {
                    finding.severity == Severity::Error
                        && finding.check == "consumer_reads_removed_response_field"
                        && finding.message.contains(file)
                })
            })
            .collect();
        files.sort_unstable();
        files
    };
    let change_driven = read(Evaluation::Demand);
    assert_eq!(change_driven, read(Evaluation::Full));
    change_driven
}

#[test]
fn a_request_under_a_base_is_a_call_only_where_the_base_is_named() {
    let workspace = workspace();

    let found = files_reading_the_removed_field(workspace.path());

    assert_eq!(found, ["based.ts"]);
}
