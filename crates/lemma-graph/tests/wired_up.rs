//! Whether a change's additions are wired up: an operation it adds to a
//! spec has a route and a caller, and a route it adds is in the spec.
//! Each case runs change-driven and in full, which must agree.

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

fn commit(repository: &Path, message: &str) {
    git(repository, &["add", "."]);
    git(repository, &["commit", "-q", "-m", message]);
}

const CHARGES: &str = r#""/v1/charges": { "post": { "operationId": "createCharge",
    "responses": { "200": { "description": "ok" } } } }"#;

const GET_CHARGE: &str = r#""/v1/charges/{id}": { "get": { "operationId": "getCharge",
    "responses": { "200": { "description": "ok" } } } }"#;

fn spec(paths: &[&str]) -> String {
    format!(
        r#"{{ "openapi": "3.0.0", "info": {{ "title": "Billing", "version": "1" }}, "paths": {{ {} }} }}"#,
        paths.join(", ")
    )
}

const SERVER: &str = "import express from \"express\";\nconst app = express();\napp.post(\"/v1/charges\", (req, res) => res.json({ id: \"ch_1\" }));\n";

const BILLING: &str = "module billing\n\nimport * as http from lemma.http\n\nexport service api {\n    index: { include: [\"src\"] }\n    operations: http.openapi(\"openapi.json\")\n}\n";

const WEB_CHARGE: &str = "export async function charge(customer: string) {\n  return fetch(\"/v1/charges\", { method: \"POST\", body: JSON.stringify({ customer }) });\n}\n";

/// The shop: billing serves `POST /v1/charges`, web calls it. Each
/// repository has one commit.
fn shop() -> tempfile::TempDir {
    let workspace = tempfile::tempdir().unwrap();
    let billing = workspace.path().join("billing");
    write(&billing.join("openapi.json"), &spec(&[CHARGES]));
    write(&billing.join("src/server.ts"), SERVER);
    write(&billing.join("mod.lm"), BILLING);
    git(&billing, &["init", "-q"]);
    commit(&billing, "billing");
    let web = workspace.path().join("web");
    write(&web.join("src/charge.ts"), WEB_CHARGE);
    write(
        &web.join("mod.lm"),
        "module web\n\nexport service web {\n    index: { include: [\"src\"] }\n}\n",
    );
    git(&web, &["init", "-q"]);
    commit(&web, "web");
    write(
        &workspace.path().join("mod.lm"),
        "module shop\n\nworkspace {\n    repo billing = local(\"billing\")\n    repo web = local(\"web\")\n    config: { level: check }\n}\n",
    );
    workspace
}

/// The warnings of billing's last commit, the same either way it is
/// evaluated.
fn warnings(workspace: &Path) -> Vec<String> {
    let run = |evaluation| {
        let options = CheckOptions {
            change: Some(Change {
                repository: "billing".to_owned(),
                base: Some("HEAD~1".to_owned()),
                head: Some("HEAD".to_owned()),
            }),
            level: Some(Level::Check),
            cache: workspace.join("cache"),
            facts_cache: None,
            evaluation,
            list_links: false,
            run_checkers: true,
            runs_base_checkers: false,
            select: None,
        };
        let report = check(&workspace.join("mod.lm"), &options, &ReferenceEngine, None).unwrap();
        let mut found: Vec<String> = report
            .findings
            .iter()
            .map(|finding| format!("{}: {}", finding.check, finding.message))
            .collect();
        found.sort();
        found
    };
    let change_driven = run(lemma_verify::Evaluation::Demand);
    assert_eq!(change_driven, run(lemma_verify::Evaluation::Full));
    change_driven
}

fn adds_get_charge(workspace: &Path, route: bool) {
    let billing = workspace.join("billing");
    write(&billing.join("openapi.json"), &spec(&[CHARGES, GET_CHARGE]));
    if route {
        let served =
            format!("{SERVER}app.get(\"/v1/charges/:id\", (req, res) => res.json({{}}));\n");
        write(&billing.join("src/server.ts"), &served);
    }
    commit(&billing, "get a charge");
}

fn web_looks_charges_up(workspace: &Path) {
    let lookup = format!(
        "{WEB_CHARGE}export async function lookup(id: string) {{\n  return fetch(`/v1/charges/${{id}}`);\n}}\n"
    );
    write(&workspace.join("web/src/charge.ts"), &lookup);
}

#[test]
fn an_operation_added_with_its_route_and_a_caller_is_wired_up() {
    let workspace = shop();
    adds_get_charge(workspace.path(), true);
    web_looks_charges_up(workspace.path());

    let found = warnings(workspace.path());

    assert_eq!(found, Vec::<String>::new());
}

#[test]
fn an_operation_added_to_the_spec_alone_has_no_route_and_no_caller() {
    let workspace = shop();
    adds_get_charge(workspace.path(), false);

    let found = warnings(workspace.path());

    assert_eq!(
        found,
        [
            "operation_without_consumer: #billing:api adds GET /v1/charges/{}, which no code in the workspace calls; if code outside it does, add \"GET /v1/charges/{}\" to a graph of lemma.http.public_operation",
            "operation_without_route: #billing:api adds GET /v1/charges/{} to its spec, but no route in its code serves it",
        ]
    );
}

#[test]
fn a_public_operation_needs_no_caller_in_the_workspace() {
    let workspace = shop();
    let billing = workspace.path().join("billing");
    let public = format!(
        "{BILLING}\nimport {{ public_operation }} from lemma.http\n\ngraph public : public_operation {{\n    \"GET /v1/charges/{{}}\"\n}}\n"
    );
    write(&billing.join("mod.lm"), &public);
    adds_get_charge(workspace.path(), true);

    let found = warnings(workspace.path());

    assert_eq!(found, Vec::<String>::new());
}

#[test]
fn a_route_added_without_its_spec_entry_is_reported_at_its_line() {
    let workspace = shop();
    let billing = workspace.path().join("billing");
    let served =
        format!("{SERVER}app.delete(\"/v1/charges/:id\", (req, res) => res.sendStatus(204));\n");
    write(&billing.join("src/server.ts"), &served);
    commit(&billing, "delete a charge");

    let found = warnings(workspace.path());

    assert_eq!(
        found,
        [
            "route_without_operation: #billing:api adds a route for DELETE /v1/charges/{} (src/server.ts:4) that its spec does not declare"
        ]
    );
}

#[test]
fn a_called_operation_whose_route_was_removed_is_an_error() {
    let workspace = shop();
    let billing = workspace.path().join("billing");
    let kept = "import express from \"express\";\nconst app = express();\napp.get(\"/v1/health\", (req, res) => res.sendStatus(200));\n";
    write(&billing.join("src/server.ts"), kept);
    commit(&billing, "drop the charge handler");

    let found = warnings(workspace.path());

    assert!(
        found.iter().any(|finding| finding.starts_with(
            "consumer_calls_unrouted_operation: #web:web:src/charge.ts::charge calls POST /v1/charges, whose route the change removed"
        )),
        "{found:#?}"
    );
}

#[test]
fn a_route_moved_where_no_route_is_read_from_it_is_not_taken_for_removed() {
    let workspace = shop();
    let billing = workspace.path().join("billing");
    let mounted = "import express from \"express\";\nimport { create } from \"./handlers\";\nconst app = express();\napp.get(\"/v1/health\", (req, res) => res.sendStatus(200));\nconst charges = express.Router();\nmount(charges);\napp.use(\"/v1/charges\", charges);\nfunction mount(r) {\n  r.post(\"/\", create);\n}\n";
    write(&billing.join("src/server.ts"), mounted);
    commit(&billing, "serve charges from a router");

    let found = warnings(workspace.path());

    assert!(
        !found
            .iter()
            .any(|finding| finding.starts_with("consumer_calls_unrouted_operation")),
        "{found:#?}"
    );
}

#[test]
fn a_generated_client_inside_a_service_adds_no_routes() {
    let workspace = shop();
    let billing = workspace.path().join("billing");
    let client = "// This file is auto-generated by @hey-api/openapi-ts\nexport const listRefunds = () => __request(OpenAPI, { method: 'GET', url: '/v1/refunds' });\nexport const getPayout = () => ({ method: 'GET', path: '/v1/payouts' });\n";
    write(&billing.join("src/generated/other.gen.ts"), client);
    commit(&billing, "commit a generated client");

    let found = warnings(workspace.path());

    assert!(
        !found
            .iter()
            .any(|finding| finding.starts_with("route_without_operation")),
        "{found:#?}"
    );
}
