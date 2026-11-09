use std::collections::BTreeMap;

use crate::load::{Repository, SourceError, Sources};
use crate::model::{Literal, Predicate, Term};
use crate::types::{Address, Value};
use crate::{Checked, Severity, check_program};

/// Repositories in memory, by name; "" is the entry repository.
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

fn checked(files: &[(&str, &str, &str)]) -> Checked {
    let mut memory = Memory(
        files
            .iter()
            .map(|(repository, path, text)| {
                (
                    ((*repository).to_owned(), (*path).to_owned()),
                    (*text).to_owned(),
                )
            })
            .collect(),
    );
    check_program(&mut memory, "mod.lm")
}

fn single(text: &str) -> Checked {
    checked(&[("", "mod.lm", text)])
}

fn errors(checked: &Checked) -> Vec<String> {
    checked
        .diagnostics
        .iter()
        .filter(|diagnostic| diagnostic.severity == Severity::Error)
        .map(|diagnostic| format!("{}: {}", diagnostic.module, diagnostic.message))
        .collect()
}

fn assert_clean(checked: &Checked) {
    assert!(checked.diagnostics.is_empty(), "{:#?}", checked.diagnostics);
}

fn assert_error(checked: &Checked, fragment: &str) {
    let found = errors(checked);
    assert!(
        found.iter().any(|message| message.contains(fragment)),
        "no error containing {fragment:?} in {found:#?}"
    );
}

#[test]
fn the_standard_http_module_checks_cleanly() {
    let result = single("module example\nimport lemma.http\n");

    assert_clean(&result);
    assert!(result.model.requirements.len() >= 9);
    assert!(
        result
            .model
            .rules
            .iter()
            .any(|rule| rule.address.name() == "calls")
    );
}

const FUNCTIONS: &str = r#"module functions

import * as python from lemma.python
import * as json from lemma.json

let tools = python.file("functions/serverless/task_functions.py")
let spec = json.command("{python} -m functions.serverless.spec")

export service Functions {
    index: { include: ["functions"] }
    runtime: { python: ".venv/bin/python" }

    export service serverless {
        export operation setup_webhook {
            implementation: tools.symbol("setup_webhook")
            dispatch: "setup_webhook"
            request: spec.at("$.setup_webhook.request")
            response: spec.at("$.setup_webhook.response")
        }
    }
}
"#;

const WORKER: &str = r#"module platform.services.worker

import * as go from lemma.go
import { Link } from lemma.links
import { Functions } from functions
import { replay_setup_error } from platform.checks.setup_replay

let client = go.file("apps/worker/generated/client.go")

export service Worker {
    index: { include: ["apps/worker"] }

    client operation call_setup_webhook {
        implementation: client.method("SetupWebhook", "Invoke")
        sends: client.type("SetupWebhookRequest")
        accepts: client.type("SetupWebhookResponse")
    }

    export operation setup_monitor {
        implementation: client.method("SetupMonitorHandler", "ServeHTTP")
    }
}

export graph serverless_links : Link {
    Worker.call_setup_webhook -> Functions.serverless.setup_webhook
}

export behavior forwards_upstream_retriable {
    severity: error

    let link = serverless_links[
        Worker.call_setup_webhook,
        Functions.serverless.setup_webhook
    ]
    expect link.exists

    for flag in [true, false] {
        let trace = need replay_setup_error(
            handler: Worker.setup_monitor,
            through: link,
            upstream_retriable: flag
        )
        expect trace.upstream.body.result.retriable == flag
        expect trace.http.body.retriable == trace.upstream.body.result.retriable
            because "the worker passes on the flag"
    }
}
"#;

const REPLAY: &str = r#"module platform.checks.setup_replay

import { Link } from lemma.links

export checker replay_setup_error(
    handler: Operation,
    through: Fact<Link>,
    upstream_retriable: Bool
): Evidence<Json> {
    command: ["go", "run", "./tools/setup-replay"]
    directory: "apps/worker"
    reads: ["platform", "functions"]
    timeout_ms: 120000
}
"#;

const GATEWAY: &str = r#"module platform.services.gateway

import * as http from lemma.http
import * as hono from lemma.hono

export service Gateway {
    index: { include: ["apps/gateway/src"] }
    operations: [
        http.openapi("apps/gateway/openapi.json"),
        hono.app(directory: "apps/gateway", entry: ["src/index.ts#app", "src/routes"], setup: "pnpm install"),
    ]
}
"#;

fn platform_example() -> Checked {
    checked(&[
        (
            "",
            "mod.lm",
            "module example\nworkspace {\n    language: \"0.2\"\n    repo functions = local(\"../functions\")\n    repo platform = local(\"../platform\")\n    config: { level: check }\n}\ncheck main {\n    run lemma.links.*\n    run platform.services.worker.forwards_upstream_retriable { severity: warning }\n}\nimport lemma.links\n",
        ),
        ("functions", "mod.lm", FUNCTIONS),
        (
            "platform",
            "mod.lm",
            "module platform\nimport platform.services.gateway\nimport platform.services.worker\nrepo functions = git(url: \"https://git.example.com/functions.git\", rev: \"3f2a1c9e04\")\n",
        ),
        ("platform", "services/worker.lm", WORKER),
        ("platform", "services/gateway.lm", GATEWAY),
        ("platform", "checks/setup_replay.lm", REPLAY),
    ])
}

#[test]
fn the_platform_example_checks_cleanly() {
    let result = platform_example();

    assert_clean(&result);
}

#[test]
fn a_graph_row_holds_the_operations_it_names() {
    let result = platform_example();

    let graph = &result.model.graphs[0];
    assert_eq!(graph.relation, Address::top("lemma.links", "Link"));
    assert_eq!(
        graph.rows,
        [vec![
            Value::Operation(
                Address::top("platform.services.worker", "Worker").child("call_setup_webhook")
            ),
            Value::Operation(
                Address::top("functions", "Functions")
                    .child("serverless")
                    .child("setup_webhook")
            ),
        ]]
    );
}

#[test]
fn operations_keep_their_resolution_requests() {
    let result = platform_example();

    let serverless = result
        .model
        .services
        .iter()
        .find(|service| service.address.name() == "serverless")
        .unwrap();
    let operation = &serverless.operations[0];
    let Value::Request {
        function,
        arguments,
    } = &operation.fields["implementation"]
    else {
        panic!("expected a request");
    };
    assert_eq!(function, "symbol");
    assert_eq!(
        arguments[1],
        ("name".to_owned(), Value::Text("setup_webhook".to_owned()))
    );
}

#[test]
fn a_check_selects_requirements_and_overrides_severity() {
    let result = platform_example();

    let main = &result.model.checks[0];
    assert_eq!(main.runs.len(), 1);
    assert_eq!(main.runs[0].severity, Some(crate::model::Severity::Warning));
    assert_eq!(
        result
            .model
            .workspace
            .as_ref()
            .and_then(|workspace| workspace.level.as_deref()),
        Some("check")
    );
}

#[test]
fn a_service_that_discovers_operations_accepts_their_names() {
    let result = single(
        "module example\nimport * as http from lemma.http\nimport { Link } from lemma.links\nservice Gateway {\n    operations: http.openapi(\"openapi.json\")\n}\nservice Web {\n    client operation list { }\n}\ngraph links : Link {\n    Web.list -> Gateway[\"GET /users/{id}\"]\n    Web.list -> Gateway.listUsers\n}\n",
    );

    assert_clean(&result);
    let rows = &result.model.graphs[0].rows;
    assert_eq!(
        rows[0][1],
        Value::Discovered {
            service: Address::top("example", "Gateway"),
            key: "GET /users/{}".to_owned()
        }
    );
}

#[test]
fn rules_resolve_predicates_and_named_columns() {
    let result = single(
        "module example\nimport { operation } from lemma.http\nrule removed(target: Text) {\n    (target) :-\n        operation@base(operation: target),\n        !operation@head(operation: target)\n}\nrequire gone {\n    severity: warning\n    when: removed(op)\n    message: \"{op} was removed\"\n}\n",
    );

    assert_clean(&result);
    let rule = &result.model.rules[0];
    let Literal::Atom {
        predicate, terms, ..
    } = &rule.clauses[0].body[0]
    else {
        panic!("expected an atom");
    };
    assert_eq!(
        predicate,
        &Predicate::Relation(Address::top("lemma.http", "operation"))
    );
    assert_eq!(terms[1], Term::Variable("target".to_owned()));
    assert_eq!(terms[0], Term::Wildcard);
    assert_eq!(
        result.model.requirements[0].severity,
        crate::model::Severity::Warning
    );
}

#[test]
fn an_unbound_variable_in_a_negation_is_reported() {
    let result = single(
        "module example\nrelation R(a: Text)\nrule r(a: Text) {\n    (a) :- R(a), !R(b)\n}\n",
    );

    assert_error(&result, "`b` must be bound");
}

#[test]
fn a_head_variable_bound_nowhere_is_reported() {
    let result = single(
        "module example\nrelation R(a: Text)\nrule r(a: Text, b: Text) {\n    (a, b) :- R(a)\n}\n",
    );

    assert_error(&result, "`b` must be bound by a positive literal");
}

#[test]
fn a_variable_of_two_types_is_reported() {
    let result = single(
        "module example\nrelation R(a: Text)\nrelation S(n: Int)\nrule r(a: Text) {\n    (a) :- R(a), S(a)\n}\n",
    );

    assert_error(&result, "`a` is `Text` elsewhere");
}

#[test]
fn the_wrong_number_of_terms_is_reported() {
    let result = single(
        "module example\nrelation R(a: Text, b: Text)\nrule r(a: Text) {\n    (a) :- R(a)\n}\n",
    );

    assert_error(&result, "`R` has 2 columns; this gives 1");
}

#[test]
fn a_snapshot_on_a_rule_is_reported() {
    let result = single(
        "module example\nrelation R(a: Text)\nrule r(a: Text) {\n    (a) :- R(a)\n}\nrule s(a: Text) {\n    (a) :- r@base(a)\n}\n",
    );

    assert_error(&result, "a rule's facts have no snapshot");
}

#[test]
fn a_placeholder_naming_no_variable_is_reported() {
    let result = single(
        "module example\nrelation R(a: Text)\nrequire x {\n    when: R(a)\n    message: \"{a} and {b}\"\n}\n",
    );

    assert_error(&result, "`{b}` names no variable");
}

#[test]
fn an_unknown_severity_is_reported() {
    let result = single(
        "module example\nrelation R(a: Text)\nrequire x {\n    severity: fatal\n    when: R(a)\n}\n",
    );

    assert_error(&result, "expected one of `error`, `warning`");
}

#[test]
fn a_client_operation_with_provider_schemas_is_reported() {
    let result = single(
        "module example\nservice Web {\n    client operation call {\n        request: x\n    }\n}\n",
    );

    assert_error(
        &result,
        "a client operation has `sends` and `accepts`, not `request`",
    );
}

#[test]
fn an_unknown_service_field_lists_the_known_ones() {
    let result = single("module example\nservice Web {\n    indexes: { include: [\"src\"] }\n}\n");

    assert_error(&result, "a service has no field `indexes`; it has `index`");
}

#[test]
fn a_row_of_the_wrong_width_is_reported() {
    let result =
        single("module example\nrelation R(a: Text, b: Text)\ngraph g : R {\n    \"x\"\n}\n");

    assert_error(&result, "this relation has 2 columns; the row gives 1");
}

#[test]
fn a_misspelled_operation_in_a_row_is_reported_once() {
    let result = single(
        "module example\nimport { Link } from lemma.links\nservice Web {\n    client operation list { }\n}\nservice Api {\n    operation list { }\n}\ngraph links : Link {\n    Web.list -> Api.lst\n}\n",
    );

    assert_eq!(
        errors(&result),
        ["example: service `Api` has no `lst`; it discovers no operations either"]
    );
}

#[test]
fn importing_an_unexported_name_is_reported() {
    let result = checked(&[
        (
            "",
            "mod.lm",
            "module example\nimport { hidden } from example.other\n",
        ),
        ("", "other.lm", "module example.other\nlet hidden = 1\n"),
    ]);

    assert_error(&result, "module `example.other` exports no `hidden`");
}

#[test]
fn a_cycle_of_imports_is_reported() {
    let result = checked(&[
        ("", "mod.lm", "module example\nimport example.a\n"),
        ("", "a.lm", "module example.a\nimport example.b\n"),
        ("", "b.lm", "module example.b\nimport example.a\n"),
    ]);

    assert_error(&result, "makes a cycle");
}

#[test]
fn a_name_declared_twice_is_reported() {
    let result = single("module example\nlet a = 1\nlet a = 2\n");

    assert_error(&result, "`a` is declared twice");
}

#[test]
fn need_outside_a_behavior_is_reported() {
    let result = single(
        "module example\nchecker c(): Evidence<Json> {\n    command: [\"true\"]\n}\nfn f(): Json {\n    return need c()\n}\n",
    );

    assert_error(&result, "only a behavior can do");
}

#[test]
fn a_behavior_asserting_nothing_is_warned_about() {
    let result = single("module example\nbehavior empty {\n    let a = 1\n}\n");

    assert!(
        result
            .diagnostics
            .iter()
            .any(|diagnostic| diagnostic.message.contains("asserts nothing"))
    );
}

#[test]
fn hovering_a_use_finds_its_declaration() {
    let result =
        single("module example\nrelation R(a: Text)\nrule r(a: Text) {\n    (a) :- R(a)\n}\n");

    let reference = result
        .index
        .references
        .iter()
        .find(|reference| matches!(&reference.target, crate::Target::Declaration(address) if address.name() == "R"))
        .unwrap();
    let definition = &result.index.definitions[&Address::top("example", "R")];
    assert_eq!(definition.kind, "relation");
    assert_eq!(definition.detail, "(a: Text)");
    assert!(reference.span.start > definition.span.start);
}

#[test]
fn a_workspace_naming_another_language_version_is_refused() {
    let result = single("module system\nworkspace {\n    language: \"0.9\"\n}\n");

    assert_error(&result, "implements language 0.2, not 0.9");
}

const LINK_TO_MISSING_OPERATION: &str = "module example\nimport { Link } from lemma.links\nservice Api {\n}\nservice Web {\n    client operation list {\n        implementation: 1\n    }\n}\ngraph links : Link {\n    Web.list -> Api.gone\n}\n";

#[test]
fn a_link_to_a_missing_operation_is_an_error_that_does_not_stop_a_check() {
    let found = checked(&[("", "mod.lm", LINK_TO_MISSING_OPERATION)]);

    let stopping: Vec<String> = found
        .stopping_error_lines()
        .into_iter()
        .filter(|line| line.contains("gone"))
        .collect();

    assert!(found.error_lines().iter().any(|line| line.contains("gone")));
    assert_eq!(stopping, Vec::<String>::new());
}

#[test]
fn a_missing_member_outside_a_link_still_stops_a_check() {
    let text = "module example\nservice Api {\n}\nlet gone = Api.gone\n";

    let found = checked(&[("", "mod.lm", text)]);

    assert!(
        found
            .stopping_error_lines()
            .iter()
            .any(|line| line.contains("gone"))
    );
}
