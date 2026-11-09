use std::collections::BTreeMap;

use lemma_lang::{Repository, SourceError, Sources, check_program};

use super::*;

/// Repository files in memory: the entry repository is "".
struct Memory(BTreeMap<(String, String), String>);

impl Sources for Memory {
    fn read(&mut self, repository: &Repository, path: &str) -> Result<Option<String>, SourceError> {
        let key = if repository.source.is_none() {
            String::new()
        } else {
            repository.name.clone()
        };
        Ok(self.0.get(&(key, path.to_owned())).cloned())
    }
}

/// `module`'s repository as lowered from `text`, with `others` (repository,
/// root module) beside it.
fn lowered(text: &str, module: &str, others: &[(&str, &str)]) -> LemmaFile {
    let mut files = BTreeMap::from([((String::new(), "mod.lm".to_owned()), text.to_owned())]);
    for (repository, other) in others {
        files.insert(
            ((*repository).to_owned(), "mod.lm".to_owned()),
            (*other).to_owned(),
        );
    }
    let checked = check_program(&mut Memory(files), "mod.lm");
    assert_eq!(checked.error_lines(), Vec::<String>::new(), "{text}");
    repository_file(&checked, module).unwrap()
}

const BILLING: &str = r#"module billing

import * as http from lemma.http
import * as json from lemma.json
import * as python from lemma.python

let handlers = python.file("billing/handlers.py")
let spec = json.command("{python} scripts/spec.py", cache: false)
let document = json.document("spec.json", format: "openapi")

export service api {
    runtime: { python: ".venv/bin/python", env: { PYTHONPATH: "src" } }
    index: { include: ["billing"], exclude: ["**/tests/**"] }
    operations: http.openapi("openapi.json")
    rejects_unknown_fields: true

    export operation create_charge {
        implementation: handlers.symbol("create_charge")
        request: handlers.type("CreateChargeRequest")
        response: handlers.type("CreateChargeResponse")
    }

    export operation refund {
        implementation: handlers.symbol("refund")
        request: spec.at("$.refund.request")
        response: spec.at("$.refund.response")
    }

    export operation void {
        implementation: handlers.symbol("void")
        request: document.at("$.void.request")
        previously: ["billing::api.cancel"]
    }
}
"#;

const WEB: &str = r#"module web

import * as go from lemma.go
import { Link } from lemma.links
import { api as billing_api } from billing

repo billing = git(url: "git@github.com:example/billing.git", rev: "3f2a1c9e04")

let charges = go.file("client/charges.go")

export service web {
    index: { include: ["src", "client"] }

    export client operation create_charge {
        implementation: charges.symbol("Charges.Create")
        sends: charges.type("CreateChargeRequest")
        accepts: charges.type("CreateChargeResponse")
    }
}

export graph links : Link {
    web.create_charge -> billing_api.create_charge
}
"#;

#[test]
fn a_services_settings_lower_to_its_fields() {
    let file = lowered(BILLING, "billing", &[]);

    let api = &file.services["api"];
    assert_eq!(api.index.include, ["billing"]);
    assert_eq!(api.index.exclude, ["**/tests/**"]);
    assert_eq!(api.exports.openapi, ["openapi.json"]);
    assert_eq!(api.runtime.python.as_deref(), Some(".venv/bin/python"));
    assert_eq!(api.runtime.env["PYTHONPATH"], "src");
    assert!(api.rejects_unknown_fields);
}

#[test]
fn an_operations_code_lowers_to_where_it_is() {
    let file = lowered(BILLING, "billing", &[]);

    let operations = &file.services["api"].exports.operations;
    assert_eq!(
        operations["create_charge"].path,
        "billing/handlers.py:create_charge"
    );
    assert_eq!(operations["void"].previously, ["api.cancel"]);
}

#[test]
fn a_link_to_another_repository_lowers_to_its_address() {
    let file = lowered(WEB, "web", &[("billing", BILLING)]);

    assert_eq!(
        file.graph.links,
        ["services.web.create_charge -> #billing:services.api.create_charge"]
    );
    assert_eq!(
        file.services["#billing"].git.as_deref(),
        Some("git@github.com:example/billing.git")
    );
    assert_eq!(
        file.services["#billing"].reference.as_deref(),
        Some("3f2a1c9e04")
    );
}

#[test]
fn an_adapter_and_a_generated_client_lower_to_their_settings() {
    let text = r#"module api

import * as hono from lemma.hono
import * as http from lemma.http

export service api {
    index: { include: ["src"] }
    operations: hono.app(entry: ["src/index.ts#app", "src/routes"], directory: "apps/api", setup: "pnpm install --frozen-lockfile", env: { SKIP_VALIDATION: "true" }, timeout_ms: 60000)
    routes: { register: ["openapi"], define: ["createRoute"] }
}

export service sdk {
    index: { include: ["sdk"] }
    client: http.generated_client(generated_from: ["openapi.json"], call_names: ["{camel}"])
}
"#;

    let file = lowered(text, "api", &[]);

    let from = file.services["api"]
        .exports
        .operations_from
        .as_ref()
        .unwrap();
    assert_eq!(from.adapter.as_deref(), Some("hono-zod-openapi"));
    assert_eq!(from.directory.as_deref(), Some("apps/api"));
    assert_eq!(from.timeout_ms, Some(60000));
    let client = file.services["sdk"].exports.client.as_ref().unwrap();
    assert_eq!(client.call_names, ["{camel}"]);
}

#[test]
fn the_languages_adapters_are_the_registrys() {
    let mut registry: Vec<&str> = crate::adapters::built_in_names().collect();
    let mut language = lemma_lang::stdlib::ADAPTERS.to_vec();
    registry.sort_unstable();
    language.sort_unstable();

    assert_eq!(registry, language);
}

#[test]
fn a_workspace_pins_its_repositories_and_level() {
    let workspace = "module example\nworkspace {\n    repo billing = local(\"../billing\")\n    config: { level: check }\n}\n";
    let files = BTreeMap::from([
        ((String::new(), "mod.lm".to_owned()), workspace.to_owned()),
        (
            ("billing".to_owned(), "mod.lm".to_owned()),
            BILLING.to_owned(),
        ),
    ]);
    let checked = check_program(&mut Memory(files), "mod.lm");

    let lowered = repository_file(&checked, "example").unwrap();

    assert_eq!(
        lowered.services["#billing"].path.as_deref(),
        Some("../billing")
    );
    assert_eq!(lowered.graph.level, Some(Level::Check));
}

#[test]
fn nested_services_inherit_their_containers_fields() {
    let text = r#"module functions
import * as python from lemma.python
let tools = python.file("functions/tools.py")
export service Functions {
    index: { include: ["functions"] }
    runtime: { python: ".venv/bin/python" }
    export service serverless {
        export operation setup_webhook {
            implementation: tools.symbol("setup_webhook")
        }
    }
}
"#;
    let files = BTreeMap::from([((String::new(), "mod.lm".to_owned()), text.to_owned())]);
    let checked = check_program(&mut Memory(files), "mod.lm");

    let lowered = repository_file(&checked, "functions").unwrap();

    let names: Vec<&String> = lowered.services.keys().collect();
    assert_eq!(names, ["Functions/serverless"]);
    let serverless = &lowered.services["Functions/serverless"];
    assert_eq!(serverless.index.include, ["functions"]);
    assert_eq!(
        serverless.runtime.python.as_deref(),
        Some(".venv/bin/python")
    );
    assert_eq!(
        serverless.exports.operations["setup_webhook"].path,
        "functions/tools.py:setup_webhook"
    );
}

#[test]
fn a_discovered_operation_named_by_method_and_path_lowers_to_a_route() {
    let text = "module example\nimport * as http from lemma.http\nimport { Link } from lemma.links\nservice Gateway {\n    operations: http.openapi(\"openapi.json\")\n}\nservice Web {\n    client operation list {\n        implementation: x\n    }\n}\ngraph links : Link {\n    Web.list -> Gateway[\"GET /users\"]\n}\n";
    let files = BTreeMap::from([((String::new(), "mod.lm".to_owned()), text.to_owned())]);
    let checked = check_program(&mut Memory(files), "mod.lm");

    let lowered = links(&checked, "example").unwrap();

    assert_eq!(
        lowered,
        ["services.Web.list -> services.Gateway.GET /users"]
    );
}

#[test]
fn a_local_repository_pinned_to_a_commit_keeps_its_pin() {
    let text = "module example\nworkspace {\n    repo billing = local(\"../billing\", rev: \"abc123\")\n}\n";

    let file = lowered(text, "example", &[("billing", "module billing\n")]);

    assert_eq!(
        file.services["#billing"].reference.as_deref(),
        Some("abc123")
    );
}
