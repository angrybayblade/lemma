//! Short calls of a client whose code is committed: a function imported
//! from the client and called itself (`createCharge()`), or a method called
//! on an imported class (`ChargesService.create()`). The operation the
//! service removes is reported at each such caller, whether the import
//! names the client's package or one of its files in the same repository,
//! in full and change-driven evaluation alike.

#![allow(clippy::unwrap_used, reason = "test setup should fail loudly")]

use std::path::Path;
use std::process::Command;

use lemma_graph::config::Level;
use lemma_graph::{Change, CheckOptions, Severity, check};
use lemma_logic::ReferenceEngine;
use lemma_verify::Evaluation;

const LIST: &str =
    r#""get": { "operationId": "listCharges", "responses": { "200": { "description": "ok" } } }"#;
const CREATE: &str =
    r#""post": { "operationId": "createCharge", "responses": { "200": { "description": "ok" } } }"#;

/// What a generator writes: one function per operation, and the same
/// operations as methods of a class.
const GENERATED: &str = "import { client } from './client.gen';\n\
export const listCharges = (options) => (options.client ?? client).get({ url: '/v1/charges', ...options });\n\
export const createCharge = (options) => (options.client ?? client).post({ url: '/v1/charges', ...options });\n\
export class ChargesService {\n\
  public static create(options) {\n\
    return (options.client ?? client).post({ url: '/v1/charges', ...options });\n\
  }\n\
}\n";

const SERVICE: &str = "module billing\n\nimport * as http from lemma.http\n\nexport service api {\n    operations: http.openapi(\"openapi.json\")\n}\n";

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

fn document(operations: &str) -> String {
    format!(
        r#"{{ "openapi": "3.0.0", "info": {{ "title": "B", "version": "1" }}, "paths": {{ "/v1/charges": {{ {operations} }} }} }}"#
    )
}

/// A service whose last commit removes `POST /v1/charges`.
fn service(workspace: &Path) {
    let service = workspace.join("billing");
    write(&service.join("mod.lm"), SERVICE);
    write(
        &service.join("openapi.json"),
        &document(&format!("{LIST}, {CREATE}")),
    );
    git(&service, &["init", "-q"]);
    git(&service, &["add", "."]);
    git(&service, &["commit", "-q", "-m", "base"]);
    write(&service.join("openapi.json"), &document(LIST));
    git(&service, &["commit", "-q", "-am", "remove create"]);
}

/// The service, its client published as a package in a repository of its
/// own, and a consumer whose one file is `consumer`.
fn with_a_client_package(consumer: &str) -> tempfile::TempDir {
    let workspace = tempfile::tempdir().unwrap();
    service(workspace.path());
    let client = workspace.path().join("client");
    write(
        &client.join("mod.lm"),
        "module client\n\nimport * as http from lemma.http\n\nexport service client {\n    index: { include: [\"src\"] }\n    client: http.generated_client(generated_from: [], specs: [\"spec/openapi.json\"])\n}\n",
    );
    write(
        &client.join("package.json"),
        r#"{ "name": "@acme/billing" }"#,
    );
    write(
        &client.join("spec/openapi.json"),
        &document(&format!("{LIST}, {CREATE}")),
    );
    write(&client.join("src/sdk.gen.ts"), GENERATED);
    git(&client, &["init", "-q"]);
    let web = workspace.path().join("web");
    write(
        &web.join("mod.lm"),
        "module web\n\nexport service web {\n    index: { include: [\"src\"] }\n}\n",
    );
    write(
        &web.join("package.json"),
        r#"{ "name": "web", "dependencies": { "@acme/billing": "1.0.0" } }"#,
    );
    write(&web.join("src/pay.ts"), consumer);
    git(&web, &["init", "-q"]);
    write(
        &workspace.path().join("mod.lm"),
        "module system\n\nworkspace {\n    repo billing = local(\"billing\")\n    repo client = local(\"client\")\n    repo web = local(\"web\")\n}\n",
    );
    workspace
}

/// The service, and one repository holding both the committed client and
/// the code that calls it through the client's index file.
fn with_a_client_beside_its_callers(consumer: &str) -> tempfile::TempDir {
    let workspace = tempfile::tempdir().unwrap();
    service(workspace.path());
    let app = workspace.path().join("app");
    write(
        &app.join("mod.lm"),
        "module app\n\nimport * as http from lemma.http\n\nexport service client {\n    index: { include: [\"src/client\"] }\n    client: http.generated_client(generated_from: [], specs: [\"spec/openapi.json\"])\n}\n\nexport service web {\n    index: { include: [\"src/pages\"] }\n}\n",
    );
    write(
        &app.join("spec/openapi.json"),
        &document(&format!("{LIST}, {CREATE}")),
    );
    write(&app.join("src/client/sdk.gen.ts"), GENERATED);
    write(
        &app.join("src/client/index.ts"),
        "export { ChargesService, createCharge, listCharges } from './sdk.gen';\n",
    );
    write(&app.join("src/pages/pay.ts"), consumer);
    git(&app, &["init", "-q"]);
    write(
        &workspace.path().join("mod.lm"),
        "module system\n\nworkspace {\n    repo billing = local(\"billing\")\n    repo app = local(\"app\")\n}\n",
    );
    workspace
}

fn errors(workspace: &Path, evaluation: Evaluation) -> Vec<String> {
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
    let report = check(&workspace.join("mod.lm"), &options, &ReferenceEngine, None).unwrap();
    report
        .findings
        .iter()
        .filter(|finding| finding.severity == Severity::Error)
        .map(|finding| finding.message.clone())
        .collect()
}

/// The errors naming `file`, the same in both evaluations.
fn errors_at(workspace: &Path, file: &str) -> Vec<String> {
    let change_driven = errors(workspace, Evaluation::Demand);
    assert_eq!(change_driven, errors(workspace, Evaluation::Full));
    change_driven
        .into_iter()
        .filter(|message| message.contains(file))
        .collect()
}

#[test]
fn a_function_imported_from_the_clients_package_is_a_call_of_its_operation() {
    let consumer = "import { createCharge as charge } from '@acme/billing';\n\nexport async function pay() {\n  return charge({ body: { amount: 1 } });\n}\n";
    let workspace = with_a_client_package(consumer);

    let errors = errors_at(workspace.path(), "src/pay.ts");

    assert_eq!(errors.len(), 1, "{errors:?}");
    assert!(errors[0].contains("POST /v1/charges"), "{errors:?}");
}

#[test]
fn a_method_called_on_a_class_imported_from_the_client_is_a_call_of_its_operation() {
    let consumer = "import { ChargesService } from '@acme/billing';\n\nexport async function pay() {\n  return ChargesService.create({ body: { amount: 1 } });\n}\n";
    let workspace = with_a_client_package(consumer);

    let errors = errors_at(workspace.path(), "src/pay.ts");

    assert_eq!(errors.len(), 1, "{errors:?}");
    assert!(errors[0].contains("POST /v1/charges"), "{errors:?}");
}

#[test]
fn a_call_of_an_operation_the_service_keeps_is_not_reported() {
    let consumer = "import { listCharges } from '@acme/billing';\n\nexport async function charges() {\n  return listCharges({});\n}\n";
    let workspace = with_a_client_package(consumer);

    assert_eq!(
        errors_at(workspace.path(), "src/pay.ts"),
        Vec::<String>::new()
    );
}

#[test]
fn a_function_of_the_same_name_imported_from_elsewhere_is_no_call() {
    let consumer = "import { createCharge } from './ledger';\n\nexport async function pay() {\n  return createCharge({ body: { amount: 1 } });\n}\n";
    let workspace = with_a_client_package(consumer);
    write(
        &workspace.path().join("web/src/ledger.ts"),
        "export const createCharge = (charge) => charge;\n",
    );

    assert_eq!(
        errors_at(workspace.path(), "src/pay.ts"),
        Vec::<String>::new()
    );
}

#[test]
fn a_client_committed_beside_its_callers_is_reached_through_its_index_file() {
    let consumer = "import { createCharge } from '../client';\n\nexport async function pay() {\n  return createCharge({ body: { amount: 1 } });\n}\n";
    let workspace = with_a_client_beside_its_callers(consumer);

    let errors = errors_at(workspace.path(), "src/pages/pay.ts");

    assert_eq!(errors.len(), 1, "{errors:?}");
    assert!(errors[0].contains("POST /v1/charges"), "{errors:?}");
}
