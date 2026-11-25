//! Calls of a generated Go client: its methods are called on a value, so
//! a call is tied to the client by the package the file imports and the
//! method's name. The operation the service removes is reported at the
//! caller, in full and change-driven evaluation alike; a file that does
//! not import the client's package, or defines a method of that name
//! itself, is no caller.

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

/// What a generator writes for one operation: the function building the
/// request, the method sending it, and the method reading its response.
const GENERATED: &str = "package rest\n\nfunc NewCreateChargeRequest(server string) (*http.Request, error) {\n\toperationPath := fmt.Sprintf(\"/v1/charges\")\n\tqueryURL, err := serverURL.Parse(operationPath)\n\tif err != nil {\n\t\treturn nil, err\n\t}\n\treturn http.NewRequest(\"POST\", queryURL.String(), nil)\n}\n\nfunc (c *Client) CreateCharge(ctx context.Context) (*http.Response, error) {\n\treq, err := NewCreateChargeRequest(c.Server)\n\tif err != nil {\n\t\treturn nil, err\n\t}\n\treturn c.Client.Do(req)\n}\n\nfunc (c *ClientWithResponses) CreateChargeWithResponse(ctx context.Context) (*CreateChargeResponse, error) {\n\trsp, err := c.CreateCharge(ctx)\n\tif err != nil {\n\t\treturn nil, err\n\t}\n\treturn ParseCreateChargeResponse(rsp)\n}\n";

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

/// A service whose last commit removes `POST /v1/charges`, and one Go
/// module holding its generated client and a command whose one file is
/// `caller`.
fn workspace(caller: &str) -> tempfile::TempDir {
    let workspace = tempfile::tempdir().unwrap();
    let service = workspace.path().join("billing");
    write(
        &service.join("mod.lm"),
        "module billing\n\nimport * as http from lemma.http\n\nexport service api {\n    operations: http.openapi(\"openapi.json\")\n}\n",
    );
    write(
        &service.join("openapi.json"),
        &document(&format!("{LIST}, {CREATE}")),
    );
    git(&service, &["init", "-q"]);
    git(&service, &["add", "."]);
    git(&service, &["commit", "-q", "-m", "base"]);
    write(&service.join("openapi.json"), &document(LIST));
    git(&service, &["commit", "-q", "-am", "remove create"]);
    let tools = workspace.path().join("tools");
    write(
        &tools.join("mod.lm"),
        "module tools\n\nimport * as http from lemma.http\n\nexport service client {\n    index: { include: [\"pkg/rest\"] }\n    client: http.generated_client(generated_from: [], specs: [\"spec/openapi.json\"])\n}\n\nexport service cli {\n    index: { include: [\"cmd\"] }\n}\n",
    );
    write(
        &tools.join("go.mod"),
        "module example.com/acme/tools\n\ngo 1.24\n",
    );
    write(
        &tools.join("spec/openapi.json"),
        &document(&format!("{LIST}, {CREATE}")),
    );
    write(&tools.join("pkg/rest/gen.go"), GENERATED);
    write(&tools.join("cmd/pay.go"), caller);
    git(&tools, &["init", "-q"]);
    write(
        &workspace.path().join("mod.lm"),
        "module system\n\nworkspace {\n    repo billing = local(\"billing\")\n    repo tools = local(\"tools\")\n}\n",
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

/// The errors naming the caller's file, the same in both evaluations.
fn errors_at_the_caller(workspace: &Path) -> Vec<String> {
    let change_driven = errors(workspace, Evaluation::Demand);
    assert_eq!(change_driven, errors(workspace, Evaluation::Full));
    change_driven
        .into_iter()
        .filter(|message| message.contains("cmd/pay.go"))
        .collect()
}

#[test]
fn a_method_called_on_a_value_in_a_file_importing_the_client_is_a_call() {
    let caller = "package main\n\nimport \"example.com/acme/tools/pkg/rest\"\n\nfunc pay(api *rest.ClientWithResponses) error {\n\t_, err := api.CreateChargeWithResponse(ctx)\n\treturn err\n}\n";
    let workspace = workspace(caller);

    let errors = errors_at_the_caller(workspace.path());

    assert_eq!(errors.len(), 1, "{errors:?}");
    assert!(errors[0].contains("POST /v1/charges"), "{errors:?}");
}

#[test]
fn a_method_of_that_name_in_a_file_not_importing_the_client_is_no_call() {
    let caller = "package main\n\nfunc pay(ledger *Ledger) error {\n\t_, err := ledger.CreateChargeWithResponse(ctx)\n\treturn err\n}\n";
    let workspace = workspace(caller);

    assert_eq!(errors_at_the_caller(workspace.path()), Vec::<String>::new());
}

#[test]
fn a_file_defining_a_method_of_that_name_calls_its_own() {
    let caller = "package main\n\nimport \"example.com/acme/tools/pkg/rest\"\n\nfunc (l *Ledger) CreateChargeWithResponse(ctx context.Context) error {\n\treturn l.write(rest.Charge{})\n}\n\nfunc (l *Ledger) pay() error {\n\treturn l.CreateChargeWithResponse(ctx)\n}\n";
    let workspace = workspace(caller);

    assert_eq!(errors_at_the_caller(workspace.path()), Vec::<String>::new());
}
