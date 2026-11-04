//! A whole check, offline: a provider whose schema comes from a resolver, a
//! Go caller, a parent composing them, and a change that breaks the link.

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

#[test]
fn a_provider_change_that_breaks_a_go_caller_is_reported() {
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
    write(&caller.join("client/charges.go"), CALLER_GO);
    git(&caller, &["init", "-q"]);
    write(
        &workspace.path().join("lemma.yaml"),
        "services:\n  \"#provider\": { path: provider }\n  \"#caller\": { path: caller }\n",
    );
    let options = CheckOptions {
        change: Some(Change {
            repository: "#provider".to_owned(),
            base: "HEAD~1".to_owned(),
            head: Some("HEAD".to_owned()),
        }),
        level: Some(Level::Check),
        cache: workspace.path().join("cache"),
    };

    let report = check(
        &workspace.path().join("lemma.yaml"),
        &options,
        &ReferenceEngine,
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
