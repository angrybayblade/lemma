use std::collections::BTreeMap;

use lemma_lang::{Repository, SourceError, Sources, check_program};

use super::*;
use crate::config::parse;

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

/// Converts `yaml` to the root module `module`, checks it with `others`
/// (repository, `lemma.yaml`) converted beside it, and lowers it back.
fn round_trip(yaml: &str, module: &str, others: &[(&str, &str)]) -> (LemmaFile, LemmaFile, String) {
    let original = parse(yaml).unwrap();
    let text = convert(&original, module).unwrap();
    let mut files = BTreeMap::from([((String::new(), "mod.lm".to_owned()), text.clone())]);
    for (repository, other) in others {
        let converted = convert(&parse(other).unwrap(), repository).unwrap();
        files.insert(((*repository).to_owned(), "mod.lm".to_owned()), converted);
    }
    let checked = check_program(&mut Memory(files), "mod.lm");
    assert!(
        checked.diagnostics.is_empty(),
        "{text}\n{:#?}",
        checked.diagnostics
    );
    let lowered = repository_file(&checked, module).unwrap();
    (original, lowered, text)
}

/// External services keep their source; lowering adds which file is their
/// manifest, and writes links without quotes, which read the same.
fn without_manifests(mut file: LemmaFile) -> LemmaFile {
    for service in file.services.values_mut() {
        service.manifest = None;
    }
    file.graph.links = file
        .graph
        .links
        .iter()
        .map(|text| {
            crate::address::parse_link(text).map_or_else(
                |_| text.clone(),
                |link| format!("{} -> {}", link.caller, link.provider),
            )
        })
        .collect();
    file
}

const BILLING: &str = r#"
services:
  api:
    runtime: { python: .venv/bin/python, env: { PYTHONPATH: src } }
    index:
      include: [billing]
      exclude: ["**/tests/**"]
    exports:
      openapi: [openapi.json]
      operations:
        create_charge:
          path: billing/handlers.py:create_charge
          schema:
            request: billing/handlers.py#CreateChargeRequest
            response: billing/handlers.py#CreateChargeResponse
        refund:
          path: billing/handlers.py:refund
          schema:
            resolver: { command: "{python} scripts/spec.py", cache: false }
            request: { path: "$.refund.request" }
            response: { path: "$.refund.response" }
        void:
          path: billing/handlers.py:void
          schema:
            request: { file: spec.json, format: openapi, path: "$.void.request" }
    rejects_unknown_fields: true
"#;

const WEB: &str = r##"
services:
  web:
    index:
      include: [src, client]
    exports:
      operations:
        create_charge:
          path: client/charges.go:Charges.Create
          schema:
            request: client/charges.go#CreateChargeRequest
            response: client/charges.go#CreateChargeResponse
  "#billing":
    git: git@github.com:example/billing.git
    ref: 3f2a1c9e04
graph:
  links:
    - services.web.create_charge -> "#billing:services.api.create_charge"
"##;

#[test]
fn a_service_with_code_command_and_document_schemas_round_trips() {
    let (original, lowered, _) = round_trip(BILLING, "billing", &[]);

    assert_eq!(lowered, original);
}

#[test]
fn a_caller_with_a_link_to_another_repository_round_trips() {
    let (original, lowered, text) = round_trip(WEB, "web", &[("billing", BILLING)]);

    assert_eq!(without_manifests(lowered), without_manifests(original));
    assert!(text.contains("client operation create_charge"), "{text}");
    assert!(
        text.contains("web.create_charge -> billing_api.create_charge"),
        "{text}"
    );
}

#[test]
fn where_a_renamed_operation_was_round_trips() {
    let yaml = BILLING.replace(
        "        void:\n          path: billing/handlers.py:void",
        "        void:\n          path: billing/handlers.py:void\n          previously: [api.cancel]",
    );

    let (original, lowered, text) = round_trip(&yaml, "billing", &[]);

    assert_eq!(lowered, original);
    assert!(
        text.contains(r#"previously: ["billing::api.cancel"]"#),
        "{text}"
    );
}

#[test]
fn a_link_naming_an_operation_by_method_and_path_round_trips() {
    let yaml = WEB.replace(
        "\"#billing:services.api.create_charge\"",
        "\"#billing:services.api.POST /v1/charges.create\"",
    );

    let (original, lowered, text) = round_trip(&yaml, "web", &[("billing", BILLING)]);

    assert_eq!(without_manifests(lowered), without_manifests(original));
    assert!(
        text.contains(r#"web.create_charge -> billing_api["POST /v1/charges.create"]"#),
        "{text}"
    );
}

#[test]
fn an_adapter_and_a_generated_client_round_trip() {
    let yaml = r#"
services:
  api:
    index: { include: [src] }
    exports:
      operations_from:
        adapter: hono-zod-openapi
        directory: apps/api
        entry: [src/index.ts#app, src/routes]
        setup: pnpm install --frozen-lockfile
        env: { SKIP_VALIDATION: "true" }
        timeout_ms: 60000
      routes:
        register: [openapi]
        define: [createRoute]
  sdk:
    index: { include: [sdk] }
    exports:
      client:
        generated_from: [openapi.json]
        call_names: ["{camel}"]
"#;

    let (original, lowered, _) = round_trip(yaml, "api", &[]);

    assert_eq!(lowered, original);
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
fn every_built_in_adapter_round_trips() {
    for adapter in lemma_lang::stdlib::ADAPTERS {
        let yaml = format!(
            "services:\n  api:\n    exports:\n      operations_from:\n        adapter: {adapter}\n        entry: [src]\n"
        );

        let (original, lowered, _) = round_trip(&yaml, "api", &[]);

        assert_eq!(lowered, original, "{adapter}");
    }
}

#[test]
fn a_file_of_external_services_becomes_a_workspace() {
    let yaml = "services:\n  \"#billing\": { path: ../billing }\n  \"#web\": { path: ../web }\ngraph:\n  level: check\n";

    let text = convert(&parse(yaml).unwrap(), "example").unwrap();

    assert!(text.contains("workspace {"), "{text}");
    assert!(
        text.contains("repo billing = local(\"../billing\")"),
        "{text}"
    );
    assert!(text.contains("config: { level: check }"), "{text}");
}

#[test]
fn a_workspace_pins_its_repositories_and_level() {
    let workspace = "module example\nworkspace {\n    repo billing = local(\"../billing\")\n    config: { level: check }\n}\n";
    let files = BTreeMap::from([
        ((String::new(), "mod.lm".to_owned()), workspace.to_owned()),
        (
            ("billing".to_owned(), "mod.lm".to_owned()),
            convert(&parse(BILLING).unwrap(), "billing").unwrap(),
        ),
    ]);
    let checked = check_program(&mut Memory(files), "mod.lm");

    let lowered = repository_file(&checked, "example").unwrap();

    assert_eq!(
        lowered.services["#billing"].path.as_deref(),
        Some("../billing")
    );
    assert_eq!(
        lowered.services["#billing"].manifest.as_deref(),
        Some("mod.lm")
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
fn a_link_that_cannot_be_read_stops_the_conversion() {
    let yaml = "services:\n  web: {}\ngraph:\n  links:\n    - not a link\n";

    let converted = convert(&parse(yaml).unwrap(), "example");

    assert!(converted.is_err(), "{converted:?}");
}

#[test]
fn a_source_file_named_after_a_service_gets_a_binding_of_its_own() {
    let yaml = "services:\n  api:\n    exports:\n      operations:\n        create:\n          path: src/api.ts:create\n";

    let text = convert(&parse(yaml).unwrap(), "example").unwrap();

    assert!(text.contains("let api2 = typescript.file"), "{text}");
    let mut files = BTreeMap::from([((String::new(), "mod.lm".to_owned()), text.clone())]);
    let checked = check_program(
        &mut Memory(files.split_off(&(String::new(), String::new()))),
        "mod.lm",
    );
    assert_eq!(checked.error_lines(), Vec::<String>::new(), "{text}");
}
