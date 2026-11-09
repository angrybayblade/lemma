//! A consumer that reads a response through a helper, as SDKs do: the
//! helper's use of a field counts when the consumer calls it, in its own
//! file or one it imports, and not otherwise. Each case runs
//! change-driven and in full, which must agree.

#![allow(clippy::unwrap_used, reason = "test setup should fail loudly")]

use std::path::Path;
use std::process::Command;

use lemma_graph::config::Level;
use lemma_graph::{Change, CheckOptions, check};
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

fn spec(response_fields: &str) -> String {
    format!(
        r#"{{ "openapi": "3.0.0", "info": {{ "title": "B", "version": "1" }}, "paths": {{ "/v1/charges": {{ "post": {{ "operationId": "createCharge",
  "responses": {{ "200": {{ "content": {{ "application/json": {{ "schema": {{ "type": "object", "properties": {{ {response_fields} }} }} }} }} }} }} }} }} }} }}"#
    )
}

const BOTH: &str = r#""id": { "type": "string" }, "next_cursor": { "type": "string" }"#;
const ID_ONLY: &str = r#""id": { "type": "string" }"#;

const CHARGE: &str = "import { readPage } from \"./page\";\n\nexport async function charge() {\n  const response = await fetch(\"/v1/charges\", { method: \"POST\" });\n  return readPage(await response.json());\n}\n";

/// Billing drops `next_cursor` from its response in its last commit; web
/// has `charge.ts` and the helper file `page.ts`.
fn shop(page: &str) -> tempfile::TempDir {
    let workspace = tempfile::tempdir().unwrap();
    let billing = workspace.path().join("billing");
    write(&billing.join("openapi.json"), &spec(BOTH));
    write(
        &billing.join("mod.lm"),
        "module billing\n\nimport * as http from lemma.http\n\nexport service api {\n    operations: http.openapi(\"openapi.json\")\n}\n",
    );
    git(&billing, &["init", "-q"]);
    git(&billing, &["add", "."]);
    git(&billing, &["commit", "-q", "-m", "billing"]);
    write(&billing.join("openapi.json"), &spec(ID_ONLY));
    git(&billing, &["commit", "-q", "-am", "drop next_cursor"]);
    let web = workspace.path().join("web");
    write(&web.join("src/charge.ts"), CHARGE);
    write(&web.join("src/page.ts"), page);
    write(
        &web.join("mod.lm"),
        "module web\n\nexport service web {\n    index: { include: [\"src\"] }\n}\n",
    );
    git(&web, &["init", "-q"]);
    git(&web, &["add", "."]);
    git(&web, &["commit", "-q", "-m", "web"]);
    write(
        &workspace.path().join("mod.lm"),
        "module shop\n\nworkspace {\n    repo billing = local(\"billing\")\n    repo web = local(\"web\")\n    config: { level: check }\n}\n",
    );
    workspace
}

/// The requirements that found something, the same either way.
fn found(workspace: &Path) -> Vec<String> {
    let run = |evaluation| {
        let options = CheckOptions {
            change: Some(Change {
                repository: "billing".to_owned(),
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
        let mut checks: Vec<String> = report
            .findings
            .iter()
            .map(|finding| finding.check.clone())
            .collect();
        checks.sort();
        checks
    };
    let change_driven = run(lemma_verify::Evaluation::Demand);
    assert_eq!(change_driven, run(lemma_verify::Evaluation::Full));
    change_driven
}

#[test]
fn a_field_read_in_an_imported_helper_the_consumer_calls_is_a_use() {
    let workspace = shop(
        "export function readPage(body: { next_cursor?: string }) {\n  return { cursor: body.next_cursor };\n}\n",
    );

    let checks = found(workspace.path());

    assert!(
        checks.contains(&"consumer_uses_removed_response_field".to_owned()),
        "{checks:?}"
    );
}

#[test]
fn a_field_read_in_a_helper_the_consumer_never_calls_is_no_use() {
    let workspace = shop(
        "export function readPage(body: unknown) {\n  return body;\n}\n\nexport function other(row: { next_cursor?: string }) {\n  return row.next_cursor;\n}\n",
    );

    let checks = found(workspace.path());

    assert!(
        !checks.contains(&"consumer_uses_removed_response_field".to_owned()),
        "{checks:?}"
    );
}

#[test]
fn a_library_method_of_the_same_name_is_no_call_of_a_helper() {
    let workspace = shop(
        "export function parse(row: { next_cursor?: string }) {\n  return row.next_cursor;\n}\n",
    );
    let charge = "import { parse } from \"./page\";\n\nexport async function charge() {\n  const response = await fetch(\"/v1/charges\", { method: \"POST\" });\n  return JSON.parse(await response.text());\n}\n";
    write(&workspace.path().join("web/src/charge.ts"), charge);

    let checks = found(workspace.path());

    assert!(
        !checks.contains(&"consumer_uses_removed_response_field".to_owned()),
        "{checks:?}"
    );
}
