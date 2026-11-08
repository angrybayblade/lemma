//! A whole check, offline: a provider whose schema comes from a resolver, a
//! Go caller, a parent composing them, and a change that breaks the link.

#![allow(clippy::unwrap_used, reason = "test setup should fail loudly")]

use std::path::Path;
use std::process::Command;

use lemma_graph::config::Level;
use lemma_graph::{Change, CheckOptions, References, Severity, Uses, check};
use lemma_logic::ReferenceEngine;
use lemma_model::{Language, Location, Position, Span};

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

const PROVIDER_LEMMA: &str = r#"
services:
  api:
    index: { include: [charges.py] }
    exports:
      operations:
        create_charge:
          path: charges.py:create_charge
          schema:
            resolver: { command: "cat spec.json" }
            request:  { path: "$.create_charge.request" }
            response: { path: "$.create_charge.response" }
"#;

const SPEC_BASE: &str = r#"{ "create_charge": {
  "request":  { "type": "object", "properties": { "customer": { "type": "object" } }, "required": ["customer"] },
  "response": { "type": "object", "properties": { "result": { "type": "object" }, "receipt_url": { "type": "string" } },
                "required": ["result", "receipt_url"] } } }"#;

/// The change: `receipt_url` is dropped from the response and a
/// required `currency` is added to the request.
const SPEC_HEAD: &str = r#"{ "create_charge": {
  "request":  { "type": "object", "properties": { "customer": { "type": "object" }, "currency": { "type": "string" } },
                "required": ["customer", "currency"] },
  "response": { "type": "object", "properties": { "result": { "type": "object" } }, "required": ["result"] } } }"#;

const CALLER_LEMMA: &str = r##"
services:
  web:
    index: { include: [client] }
    exports:
      operations:
        create_charge:
          path: client/charges.go:Charges.Create
          schema:
            request:  client/charges.go#CreateChargeRequest
            response: client/charges.go#CreateChargeResponse
graph:
  links:
    - services.web.create_charge -> "#provider:services.api.create_charge"
"##;

const CALLER_GO: &str = r#"package client

type CreateChargeRequest struct {
	Customer map[string]interface{} `json:"customer"`
}

type CreateChargeResponse struct {
	Result     map[string]interface{} `json:"result"`
	ReceiptURL string                 `json:"receipt_url"`
}

type Charges struct{}

func (c *Charges) Create() {}
"#;

/// The provider, the caller with `caller_go` as its client, and the parent
/// composing them.
fn system(caller_go: &str) -> tempfile::TempDir {
    let workspace = tempfile::tempdir().unwrap();
    let provider = workspace.path().join("provider");
    let caller = workspace.path().join("caller");
    write(&provider.join("lemma.yaml"), PROVIDER_LEMMA);
    write(
        &provider.join("charges.py"),
        "def create_charge():\n    pass\n",
    );
    write(&provider.join("spec.json"), SPEC_BASE);
    git(&provider, &["init", "-q"]);
    git(&provider, &["add", "."]);
    git(&provider, &["commit", "-q", "-m", "base"]);
    write(&provider.join("spec.json"), SPEC_HEAD);
    git(&provider, &["commit", "-q", "-am", "head"]);
    write(&caller.join("lemma.yaml"), CALLER_LEMMA);
    write(&caller.join("client/charges.go"), caller_go);
    git(&caller, &["init", "-q"]);
    write(
        &workspace.path().join("lemma.yaml"),
        "services:\n  \"#provider\": { path: provider }\n  \"#caller\": { path: caller }\n",
    );
    workspace
}

#[test]
fn a_provider_change_that_breaks_a_go_caller_is_reported() {
    let workspace = system(CALLER_GO);
    let options = CheckOptions {
        change: Some(Change {
            repository: "#provider".to_owned(),
            base: Some("HEAD~1".to_owned()),
            head: Some("HEAD".to_owned()),
        }),
        level: Some(Level::Check),
        cache: workspace.path().join("cache"),
        facts_cache: None,
        evaluation: lemma_verify::Evaluation::Demand,
        list_links: false,
        run_checkers: true,
        runs_base_checkers: false,
        select: None,
        warm: None,
    };

    let report = check(
        &workspace.path().join("lemma.yaml"),
        &options,
        &ReferenceEngine,
        None,
    )
    .unwrap();

    let errors: Vec<&str> = report
        .findings
        .iter()
        .filter(|finding| finding.severity == Severity::Error)
        .map(|finding| finding.message.as_str())
        .collect();
    assert_eq!(report.links, 1);
    assert_eq!(
        errors,
        [
            "request: the receiver requires `currency` at the root; the sender may leave it out",
            "response: the receiver expects `receipt_url` at the root; the sender does not send it",
        ]
    );
}

/// Uses by the line of the name asked about: the declaration only for
/// `Create` (line 13), and a call from elsewhere for `CreateWithRetry`
/// (line 14).
struct FakeReferences;

impl References for FakeReferences {
    fn uses(&mut self, _: &Path, file: &str, _: Language, position: Position) -> Uses {
        let at = |path: &str, line: u32| {
            let start = Position { line, column: 0 };
            Location {
                path: path.to_owned(),
                span: Span { start, end: start },
                is_external: false,
            }
        };
        let mut uses = vec![at(file, position.line)];
        if position.line == 14 {
            uses.push(at("cmd/main.go", 3));
        }
        Ok(Some(uses))
    }
}

#[test]
fn a_caller_anchored_at_a_method_nothing_calls_is_warned_about() {
    let caller_go = format!("{CALLER_GO}func (c *Charges) CreateWithRetry() {{}}\n");
    let workspace = system(&caller_go);
    let options = CheckOptions {
        change: None,
        level: Some(Level::Check),
        cache: workspace.path().join("cache"),
        facts_cache: None,
        evaluation: lemma_verify::Evaluation::Demand,
        list_links: false,
        run_checkers: true,
        runs_base_checkers: false,
        select: None,
        warm: None,
    };

    let report = check(
        &workspace.path().join("lemma.yaml"),
        &options,
        &ReferenceEngine,
        Some(&mut FakeReferences),
    )
    .unwrap();

    let anchors: Vec<(Severity, &str)> = report
        .findings
        .iter()
        .filter(|finding| finding.check == "anchor")
        .map(|finding| (finding.severity, finding.message.as_str()))
        .collect();
    assert_eq!(
        anchors,
        [(
            Severity::Warning,
            "the caller operation #caller:services.web.create_charge's anchor `client/charges.go:Charges.Create` is never called, while `CreateWithRetry` (called 1 time) of the same type is: the anchor may name the wrong method, and traces from it find nothing"
        )]
    );
}

/// A provider documenting its operations without `operationId`s, and a
/// caller whose links name them by method and path, `route` among them.
fn routed_system(route: &str) -> tempfile::TempDir {
    let workspace = tempfile::tempdir().unwrap();
    let billing = workspace.path().join("billing");
    write(
        &billing.join("mod.lm"),
        "module billing\nimport * as http from lemma.http\nexport service api {\n    operations: http.openapi(\"openapi.json\")\n}\n",
    );
    write(
        &billing.join("openapi.json"),
        r#"{ "openapi": "3.0.0", "paths": { "/v1/charges/{id}": { "get": {
  "responses": { "200": { "content": { "application/json": { "schema": { "type": "object" } } } } } } } } }"#,
    );
    git(&billing, &["init", "-q"]);
    git(&billing, &["add", "."]);
    git(&billing, &["commit", "-q", "-m", "billing"]);
    let web = workspace.path().join("web");
    write(
        &web.join("mod.lm"),
        &format!(
            r#"module web

import * as python from lemma.python
import {{ Link }} from lemma.links
import {{ api }} from billing

repo billing = local("../billing")

let client = python.file("client.py")

export service web {{
    client operation get_charge {{
        implementation: client.symbol("get_charge")
    }}
}}

export graph links : Link {{
    web.get_charge -> api["{route}"]
}}
"#
        ),
    );
    write(
        &web.join("client.py"),
        "def get_charge(identifier):\n    pass\n",
    );
    git(&web, &["init", "-q"]);
    git(&web, &["add", "."]);
    git(&web, &["commit", "-q", "-m", "web"]);
    workspace
}

fn link_problems(workspace: &Path) -> Vec<String> {
    let options = CheckOptions {
        change: None,
        level: Some(Level::Check),
        cache: workspace.join("cache"),
        facts_cache: None,
        evaluation: lemma_verify::Evaluation::Demand,
        list_links: false,
        run_checkers: true,
        runs_base_checkers: false,
        select: None,
        warm: None,
    };
    let report = check(
        &workspace.join("web/mod.lm"),
        &options,
        &ReferenceEngine,
        None,
    )
    .unwrap();
    assert_eq!(report.links, 1);
    report
        .findings
        .iter()
        .filter(|finding| finding.check == "link")
        .map(|finding| finding.message.clone())
        .collect()
}

#[test]
fn a_link_may_name_a_documented_operation_by_method_and_path() {
    let workspace = routed_system("GET /v1/charges/{id}");

    assert_eq!(link_problems(workspace.path()), Vec::<String>::new());
}

#[test]
fn a_link_to_a_route_no_document_declares_is_reported() {
    let workspace = routed_system("GET /v1/refunds/{id}");

    let problems = link_problems(workspace.path());

    assert_eq!(problems.len(), 1, "{problems:#?}");
    assert!(
        problems[0].contains("GET /v1/refunds/{}"),
        "{}",
        problems[0]
    );
}

/// The provider renames `create_charge` to `make_charge` in its last
/// commit, its document unchanged and already disagreeing with the
/// caller, saying so with `previously` when `says_previously`.
fn renamed_system(says_previously: bool) -> tempfile::TempDir {
    let workspace = system(CALLER_GO);
    let provider = workspace.path().join("provider");
    write(&provider.join("spec.json"), SPEC_HEAD);
    git(&provider, &["commit", "-q", "--amend", "-am", "head"]);
    let previously = if says_previously {
        "\n          previously: [api.create_charge]"
    } else {
        ""
    };
    let renamed = PROVIDER_LEMMA.replace(
        "        create_charge:\n          path: charges.py:create_charge",
        &format!("        make_charge:\n          path: charges.py:create_charge{previously}"),
    );
    write(&provider.join("lemma.yaml"), &renamed);
    git(&provider, &["commit", "-q", "-am", "rename"]);
    let caller = workspace.path().join("caller");
    write(
        &caller.join("lemma.yaml"),
        &CALLER_LEMMA.replace("api.create_charge", "api.make_charge"),
    );
    workspace
}

fn errors_after_rename(says_previously: bool) -> usize {
    let workspace = renamed_system(says_previously);
    let options = CheckOptions {
        change: Some(Change {
            repository: "#provider".to_owned(),
            base: Some("HEAD~1".to_owned()),
            head: Some("HEAD".to_owned()),
        }),
        level: Some(Level::Check),
        cache: workspace.path().join("cache"),
        facts_cache: None,
        evaluation: lemma_verify::Evaluation::Demand,
        list_links: false,
        run_checkers: true,
        runs_base_checkers: false,
        select: None,
        warm: None,
    };
    let report = check(
        &workspace.path().join("lemma.yaml"),
        &options,
        &ReferenceEngine,
        None,
    )
    .unwrap();
    report
        .findings
        .iter()
        .filter(|finding| finding.severity == Severity::Error)
        .count()
}

#[test]
fn a_renamed_operation_is_compared_with_where_it_was() {
    assert_eq!(
        errors_after_rename(false),
        2,
        "the rename looks like a new operation"
    );
    assert_eq!(
        errors_after_rename(true),
        0,
        "its disagreements were already there"
    );
}

#[test]
fn select_is_refused_for_a_parent_lemma_yaml() {
    let workspace = system(CALLER_GO);
    let options = CheckOptions {
        change: None,
        level: Some(Level::Check),
        cache: workspace.path().join("cache"),
        facts_cache: None,
        evaluation: lemma_verify::Evaluation::Demand,
        list_links: false,
        run_checkers: true,
        runs_base_checkers: false,
        select: Some("nightly".to_owned()),
        warm: None,
    };

    let refused = check(
        &workspace.path().join("lemma.yaml"),
        &options,
        &ReferenceEngine,
        None,
    );

    assert!(
        refused
            .unwrap_err()
            .to_string()
            .contains("declares no check `nightly`")
    );
}

/// A check of the system as it is, at `level`, with the provider's resolver
/// replaced by a command that cannot run here.
fn checked_without_resolver(level: Level) -> Vec<(Severity, String)> {
    let workspace = system(CALLER_GO);
    let provider = workspace.path().join("provider/lemma.yaml");
    let missing = PROVIDER_LEMMA.replace("cat spec.json", "./.venv/bin/python -m spec");
    write(&provider, &missing);
    let options = CheckOptions {
        change: None,
        level: Some(level),
        cache: workspace.path().join("cache"),
        facts_cache: None,
        evaluation: lemma_verify::Evaluation::Demand,
        list_links: false,
        run_checkers: true,
        runs_base_checkers: false,
        select: None,
        warm: None,
    };
    let report = check(
        &workspace.path().join("lemma.yaml"),
        &options,
        &ReferenceEngine,
        None,
    )
    .unwrap();
    report
        .findings
        .iter()
        .filter(|finding| finding.check == "resolver" || finding.check == "schema")
        .map(|finding| (finding.severity, finding.check.clone()))
        .collect()
}

#[test]
fn a_resolver_that_cannot_run_here_warns_once_and_fails_only_a_strict_check() {
    let checked = checked_without_resolver(Level::Check);
    let strict = checked_without_resolver(Level::Strict);

    assert_eq!(checked, [(Severity::Warning, "resolver".to_owned())]);
    assert_eq!(strict, [(Severity::Error, "resolver".to_owned())]);
}
