//! Solver-independent checks of source coverage and generated obligations.
#![allow(clippy::unwrap_used, reason = "test setup should fail loudly")]

use super::*;

const TYPESCRIPT: &str = include_str!("../../../../examples/source-contracts/handler.ts");
const PYTHON: &str = include_str!("../../../../examples/source-contracts/handler.py");
const GO: &str = include_str!("../../../../examples/source-contracts/handler.go");
const CONTRACT: &str = include_str!("../../tests/fixtures/proof/contract.json");

fn contract(language: source::Language) -> Contract {
    let mut contract: Contract = serde_json::from_str(CONTRACT).unwrap();
    if language == source::Language::Python {
        let model = contract.models.remove("commitStatus").unwrap();
        contract.models.insert("commit_status".to_owned(), model);
    }
    contract
}

#[test]
fn three_source_languages_feed_the_same_contract_and_symbolic_core() {
    for (file, text, symbol) in [
        ("handler.ts", TYPESCRIPT, "setStatus"),
        ("handler.py", PYTHON, "set_status"),
        ("handler.go", GO, "setStatus"),
    ] {
        let (language, function) = source::read(file, text, symbol).unwrap();
        let contract = contract(language);
        contract.validate().unwrap();
        let execution = symbolic::execute(&function, &contract).unwrap();
        assert_eq!(execution.paths.len(), 3, "{file}");
        assert_eq!(execution.symbols.len(), 5, "{file}");
        assert_eq!(execution.assumptions.len(), 1, "{file}");
        for assertion in &contract.ensures {
            for path in &execution.paths {
                execution
                    .postcondition(path, &assertion.condition, &contract)
                    .unwrap();
            }
        }
    }
}

#[test]
fn unsupported_source_is_refused_instead_of_skipped() {
    for (file, text, symbol) in [
        (
            "x.ts",
            "function f(x) { while (x) { x = false; } return x; }",
            "f",
        ),
        (
            "x.py",
            "def f(x):\n    x['flag'] = False\n    return x\n",
            "f",
        ),
        (
            "x.go",
            "package p\nfunc f(x bool) bool { defer cleanup(); return x }",
            "f",
        ),
        ("x.ts", "function f(x) { return x == false; }", "f"),
        ("x.go", "package p\nfunc f(x bool) int { return 012 }", "f"),
    ] {
        assert!(source::read(file, text, symbol).is_err(), "{file}: {text}");
    }
}

#[test]
fn unmodeled_calls_and_ghost_inputs_are_not_source_implementations() {
    let (_, function) = source::read("handler.ts", TYPESCRIPT, "setStatus").unwrap();
    let mut contract = contract(source::Language::TypeScript);
    contract.models.clear();
    assert!(symbolic::execute(&function, &contract).is_err());
    let (_, function) = source::read(
        "x.ts",
        "function f(request) { return lease.rejected; }",
        "f",
    )
    .unwrap();
    assert!(symbolic::execute(&function, &contract).is_err());
}

#[test]
fn ambiguous_symbols_are_refused() {
    assert!(
        source::read(
            "x.py",
            "def f(x):\n    return x\ndef f(y):\n    return y\n",
            "f"
        )
        .is_err()
    );
}

#[test]
fn reassigning_a_parameter_never_changes_what_a_contract_means_by_an_input() {
    let (_, function) = source::read(
        "x.py",
        "def f(request):\n    request = False\n    return request\n",
        "f",
    )
    .unwrap();
    let contract = contract(source::Language::Python);
    let execution = symbolic::execute(&function, &contract).unwrap();
    let path = &execution.paths[0];
    assert!(matches!(&path.result, Some(symbolic::Value::Scalar(term)) if term.code == "false"));
    let reads_input = execution
        .postcondition(path, "request.allowed || !request.allowed", &contract)
        .unwrap();
    assert_ne!(reads_input.code, "false");
}

#[test]
fn path_limits_and_fallthrough_cannot_produce_success() {
    let (_, function) = source::read("handler.ts", TYPESCRIPT, "setStatus").unwrap();
    let mut contract = contract(source::Language::TypeScript);
    contract.max_paths = 1;
    assert!(symbolic::execute(&function, &contract).is_err());
    let (_, function) = source::read(
        "x.ts",
        "function f(request) { if (request.allowed) { return true; } }",
        "f",
    )
    .unwrap();
    assert!(symbolic::execute(&function, &contract).is_err());
}

fn check_source(file: &str, source_text: &str, symbol: &str, contract: &Json) -> Json {
    let workspace = tempfile::tempdir().unwrap();
    std::fs::write(workspace.path().join(file), source_text).unwrap();
    let artifacts = workspace.path().join("artifacts");
    std::fs::create_dir(&artifacts).unwrap();
    let given = json!({"roots": {"test": workspace.path().display().to_string()}, "arguments": {"handler": {"repository": "test", "implementation": {"file": file, "symbol": symbol}}, "contract": contract}});
    run(
        &given,
        &artifacts,
        crate::repos::Environment {
            base: None,
            added: &std::collections::BTreeMap::default(),
        },
        &Digests::default(),
        30000,
    )
}

/// This lane uses a real optional solver; ordinary tests remain offline and do
/// not infer a proof from a mocked unsatisfiable answer.
#[test]
#[ignore = "requires native Z3; set LEMMA_SMT_SOLVER and run with --ignored"]
fn real_solver_proves_three_languages_and_rejects_three_mutations_each() {
    for (file, text, symbol) in [
        ("handler.ts", TYPESCRIPT, "setStatus"),
        ("handler.py", PYTHON, "set_status"),
        ("handler.go", GO, "setStatus"),
    ] {
        let (language, _) = source::read(file, text, symbol).unwrap();
        let contract = serde_json::from_str::<Json>(CONTRACT).unwrap();
        let mut contract = contract;
        if language == source::Language::Python {
            contract["models"]["commit_status"] = contract["models"]["commitStatus"].clone();
            contract["models"]
                .as_object_mut()
                .unwrap()
                .remove("commitStatus");
        }
        let check = |text: &str, contract: &Json| check_source(file, text, symbol, contract);
        let original = check(text, &contract);
        assert_eq!(
            original["observed"]["status"], "conditional",
            "{file}: {original}"
        );
        assert_eq!(original["observed"]["holds"], true);
        let mutations = mutations(language, text);
        for mutated in mutations {
            assert_ne!(mutated, text);
            let observation = check(&mutated, &contract);
            assert_eq!(
                observation["observed"]["status"], "violated",
                "{file}: {observation}"
            );
            assert_eq!(observation["observed"]["holds"], false);
            assert!(
                observation["observed"]["properties"]
                    .as_array()
                    .unwrap()
                    .iter()
                    .any(|p| !p["counterexamples"].as_array().unwrap().is_empty())
            );
        }
        contract["requires"] = json!(["request.allowed && !request.allowed"]);
        let infeasible = check(text, &contract);
        assert!(
            infeasible["unavailable"]
                .as_str()
                .unwrap()
                .contains("vacuous")
        );
    }
}

#[test]
fn python_elif_and_else_are_all_checked() {
    let source = "def f(request):\n    if request['allowed']:\n        return True\n    elif request['enabled']:\n        return False\n    else:\n        return True\n";
    let (_, function) = source::read("x.py", source, "f").unwrap();
    let execution = symbolic::execute(&function, &contract(source::Language::Python)).unwrap();
    assert_eq!(execution.paths.len(), 3);
}

fn mutations(language: source::Language, text: &str) -> Vec<String> {
    match language {
        source::Language::TypeScript | source::Language::Go => vec![
            text.replace("!request.allowed", "false"),
            text.replace(
                "request.project, request.enabled",
                "request.project, !request.enabled",
            ),
            text.replace("write.rejected", "false"),
        ],
        source::Language::Python => vec![
            text.replace("not request[\"allowed\"]", "False"),
            text.replace(
                "request[\"project\"], request[\"enabled\"]",
                "request[\"project\"], not request[\"enabled\"]",
            ),
            text.replace("write[\"rejected\"]", "False"),
        ],
    }
}

#[test]
#[ignore = "requires native Z3; set LEMMA_SMT_SOLVER and run with --ignored"]
fn pure_implementations_are_proved_without_call_model_assumptions() {
    let contract = json!({
        "inputs": {"request": {"enabled": "Bool"}},
        "ensures": [{"name": "returns_requested_value", "condition": "result == request.enabled"}],
    });
    for (file, text) in [
        (
            "x.ts",
            "function f(request: {enabled: boolean}) { return request.enabled; }",
        ),
        ("x.py", "def f(request):\n    return request['enabled']\n"),
        (
            "x.go",
            "package p\ntype Request struct {enabled bool}\nfunc f(request Request) bool { return request.enabled }",
        ),
    ] {
        let result = check_source(file, text, "f", &contract);
        assert_eq!(result["observed"]["status"], "proved", "{result}");
        assert_eq!(result["observed"]["assumptions"], json!([]));
    }
}

#[test]
fn source_digests_detect_changes_and_parent_paths_are_refused() {
    let workspace = tempfile::tempdir().unwrap();
    let file = workspace.path().join("handler.ts");
    std::fs::write(&file, TYPESCRIPT).unwrap();
    let mut handler = json!({"repository": "test", "implementation": {"file": "handler.ts", "symbol": "setStatus"}});
    bind_source(workspace.path(), &mut handler).unwrap();
    std::fs::write(&file, TYPESCRIPT.replace("write.rejected", "false")).unwrap();
    let given = json!({"roots": {"test": workspace.path().display().to_string()}});
    assert!(
        read_handler(&given, &handler)
            .unwrap_err()
            .contains("changed")
    );
    assert!(inside(workspace.path(), "../outside.ts").is_err());
}

#[test]
#[cfg(unix)]
fn implementation_symlinks_cannot_read_outside_the_checkout() {
    let workspace = tempfile::tempdir().unwrap();
    let outside = tempfile::tempdir().unwrap();
    std::fs::write(outside.path().join("outside.ts"), TYPESCRIPT).unwrap();
    std::os::unix::fs::symlink(
        outside.path().join("outside.ts"),
        workspace.path().join("handler.ts"),
    )
    .unwrap();
    assert!(
        inside(workspace.path(), "handler.ts")
            .unwrap_err()
            .contains("symlink")
    );
}

#[test]
fn python_docstrings_are_metadata_but_formatted_expressions_are_not() {
    let (_, function) = source::read("x.py", "def f(request):\n    \"\"\"A constant function description.\"\"\"\n    return request['allowed']\n", "f").unwrap();
    assert!(symbolic::execute(&function, &contract(source::Language::Python)).is_ok());
    for source in [
        "def f(request):\n    f'call {side_effect()}'\n    return True\n",
        "def f(request):\n    value = True\n    'later expression'\n    return value\n",
    ] {
        assert!(source::read("x.py", source, "f").is_err());
    }
}

fn async_contract() -> Contract {
    let mut contract = contract(source::Language::TypeScript);
    contract.models.get_mut("commitStatus").unwrap().is_async = true;
    contract
}

#[test]
fn an_unawaited_call_to_an_async_model_is_refused() {
    let awaited = TYPESCRIPT
        .replace("export function", "export async function")
        .replace(
            "const write = commitStatus",
            "const write = await commitStatus",
        );
    let (_, function) = source::read("x.ts", &awaited, "setStatus").unwrap();
    assert!(symbolic::execute(&function, &async_contract()).is_ok());
    let forgotten = TYPESCRIPT.replace("export function", "export async function");
    let (_, function) = source::read("x.ts", &forgotten, "setStatus").unwrap();
    let error = symbolic::execute(&function, &async_contract())
        .err()
        .unwrap();
    assert!(error.contains("not awaited"), "{error}");
}

#[test]
fn awaiting_a_model_that_is_not_async_is_refused() {
    let source = "async def f(request):\n    write = await commit_status(request['project'], request['enabled'], request['epoch'])\n    return {'success': not write['rejected']}\n";
    let (_, function) = source::read("x.py", source, "f").unwrap();
    let error = symbolic::execute(&function, &contract(source::Language::Python))
        .err()
        .unwrap();
    assert!(error.contains("not `async`"), "{error}");
}

const TWO_WRITES: &str = "def set_status(request):\n    if not request[\"allowed\"]:\n        return {\"success\": False}\n    probe = commit_status(request[\"project\"], request[\"enabled\"], request[\"epoch\"])\n    commit_status(request[\"project\"], request[\"enabled\"], request[\"epoch\"])\n    if probe[\"rejected\"]:\n        return {\"success\": False}\n    return {\"success\": True}\n";

#[test]
fn a_second_call_to_a_model_reading_specification_inputs_is_refused() {
    let (_, function) = source::read("x.py", TWO_WRITES, "set_status").unwrap();
    let error = symbolic::execute(&function, &contract(source::Language::Python))
        .err()
        .unwrap();
    assert!(error.contains("more than once"), "{error}");
}

fn per_call_contract() -> Json {
    json!({
        "inputs": {"request": {"allowed": "Bool", "enabled": "Bool", "project": "Text", "epoch": "Text"}},
        "parameters": ["request"],
        "models": {"commit_status": {
            "assumption": "commit_status answers whether this write was rejected; rejected writes change nothing",
            "parameters": ["project", "enabled", "epoch"],
            "outcomes": {"rejected": "Bool"},
            "returns": {"rejected": {"expr": "outcome.rejected"}},
            "effect": {"name": "commit", "fields": {"rejected": {"expr": "outcome.rejected"}}}
        }},
        "ensures": [{"name": "success_requires_an_accepted_last_write", "condition": "!result.success || (effects.commit.called && !effects.commit.last.rejected)"}]
    })
}

#[test]
fn outcomes_give_every_call_its_own_answer() {
    let contract: Contract = serde_json::from_value(per_call_contract()).unwrap();
    contract.validate().unwrap();
    let (_, function) = source::read("x.py", TWO_WRITES, "set_status").unwrap();
    let execution = symbolic::execute(&function, &contract).unwrap();
    assert!(
        execution
            .symbols
            .contains_key("calls.commit_status.1.rejected")
    );
    assert!(
        execution
            .symbols
            .contains_key("calls.commit_status.2.rejected")
    );
}

#[test]
#[ignore = "requires native Z3; set LEMMA_SMT_SOLVER and run with --ignored"]
fn checking_one_call_and_succeeding_after_another_is_violated() {
    let contract = per_call_contract();
    let violated = check_source("x.py", TWO_WRITES, "set_status", &contract);
    assert_eq!(violated["observed"]["status"], "violated", "{violated}");
    let checked = check_source("x.py", PYTHON, "set_status", &contract);
    assert_eq!(checked["observed"]["status"], "conditional", "{checked}");
}

#[test]
fn methods_are_read_by_qualified_name_with_their_receiver_first() {
    for (file, text, symbol, receiver) in [
        (
            "x.ts",
            "export class Gate {\n  allows(request: Request) {\n    return this.open && request.allowed;\n  }\n}\n",
            "Gate.allows",
            "this",
        ),
        (
            "x.py",
            "class Gate:\n    def allows(self, request):\n        return request['allowed']\n",
            "Gate.allows",
            "self",
        ),
        (
            "x.go",
            "package p\ntype Gate struct{ open bool }\nfunc (g *Gate) allows(request Request) bool { return g.open && request.allowed }\n",
            "Gate.allows",
            "g",
        ),
    ] {
        let (_, function) = source::read(file, text, symbol).unwrap();
        let names: Vec<&str> = function
            .parameters
            .iter()
            .map(|p| p.name.as_str())
            .collect();
        assert_eq!(names, [receiver, "request"], "{file}");
    }
}

#[test]
fn a_nested_function_reads_its_captures_only_from_the_contract() {
    let nested = "def outer(request):\n    def inner():\n        return request['allowed']\n    return inner()\n";
    let (_, function) = source::read("x.py", nested, "outer.inner").unwrap();
    let mut contract: Contract = serde_json::from_value(json!({
        "inputs": {"request": {"allowed": "Bool"}},
        "parameters": [],
        "ensures": [{"name": "allowed", "condition": "result == request.allowed"}],
    }))
    .unwrap();
    let error = symbolic::execute(&function, &contract).err().unwrap();
    assert!(error.contains("unmodeled name `request`"), "{error}");
    contract
        .captures
        .insert("request".to_owned(), "request".to_owned());
    assert!(symbolic::execute(&function, &contract).is_ok());
}

#[test]
fn unknown_containers_are_refused() {
    assert!(
        source::read(
            "x.py",
            "class A:\n    def m(self):\n        return True\n",
            "B.m"
        )
        .is_err()
    );
}

#[test]
#[ignore = "requires native Z3; set LEMMA_SMT_SOLVER and run with --ignored"]
fn a_method_is_proved_with_its_receiver_bound_to_an_input() {
    let contract = json!({
        "inputs": {"gate": {"open": "Bool"}, "request": {"allowed": "Bool"}},
        "parameters": ["gate", "request"],
        "ensures": [{"name": "open_and_allowed", "condition": "result == (gate.open && request.allowed)"}],
    });
    for (file, text) in [
        (
            "x.ts",
            "export class Gate {\n  allows(request: Request) {\n    return this.open && request.allowed;\n  }\n}\n",
        ),
        (
            "x.py",
            "class Gate:\n    def allows(self, request):\n        return self['open'] and request['allowed']\n",
        ),
        (
            "x.go",
            "package p\ntype Gate struct{ open bool }\nfunc (g *Gate) allows(request Request) bool { return g.open && request.allowed }\n",
        ),
    ] {
        let result = check_source(file, text, "Gate.allows", &contract);
        assert_eq!(result["observed"]["status"], "proved", "{file}: {result}");
    }
}

#[test]
fn decorators_are_recorded_for_functions_methods_and_classes() {
    for (file, text, symbol, expected) in [
        (
            "x.py",
            "@app.get('/s')\ndef f(request):\n    return True\n",
            "f",
            vec!["app.get('/s')"],
        ),
        (
            "x.py",
            "@register\nclass A:\n    @staticmethod\n    def m(request):\n        return True\n",
            "A.m",
            vec!["staticmethod", "register"],
        ),
        (
            "x.ts",
            "@Controller()\nexport class A {\n  @Get('/s')\n  m(request) {\n    return true;\n  }\n}\n",
            "A.m",
            vec!["Get('/s')", "Controller()"],
        ),
    ] {
        let (_, function) = source::read(file, text, symbol).unwrap();
        assert_eq!(function.decorators, expected, "{file}");
    }
}

#[test]
fn a_decorator_needs_a_model_unless_the_language_defines_it() {
    let text = "class A:\n    @property\n    def ready(self):\n        return True\n    @cached\n    def other(self):\n        return True\n";
    let mut contract: Contract = serde_json::from_value(json!({
        "inputs": {"a": {"open": "Bool"}},
        "parameters": ["a"],
        "ensures": [{"name": "returns_true", "condition": "result"}],
    }))
    .unwrap();
    let (_, property) = source::read("x.py", text, "A.ready").unwrap();
    let execution = symbolic::execute(&property, &contract).unwrap();
    assert!(execution.assumptions.is_empty());
    let (_, cached) = source::read("x.py", text, "A.other").unwrap();
    let error = symbolic::execute(&cached, &contract).err().unwrap();
    assert!(error.contains("decorator model"), "{error}");
    contract.decorators.insert(
        "cached".to_owned(),
        "cached returns what the method returns for the same receiver".to_owned(),
    );
    let execution = symbolic::execute(&cached, &contract).unwrap();
    assert_eq!(execution.assumptions.len(), 1);
}

#[test]
fn overload_signatures_are_not_the_implementation() {
    let text = "from typing import overload\n@overload\ndef f(x: int) -> int: ...\n@overload\ndef f(x: str) -> str: ...\ndef f(x):\n    return x\n";
    let (_, function) = source::read("x.py", text, "f").unwrap();
    assert_eq!(function.decorators, Vec::<String>::new());
}

#[test]
fn keyword_arguments_bind_model_parameters_by_name() {
    let text = "def set_status(request):\n    write = commit_status(request['project'], epoch=request['epoch'], enabled=request['enabled'])\n    return {'success': not write['rejected']}\n";
    let (_, function) = source::read("x.py", text, "set_status").unwrap();
    assert!(symbolic::execute(&function, &contract(source::Language::Python)).is_ok());
    for wrong in [
        "commit_status(request['project'], request['enabled'], request['epoch'], enabled=True)",
        "commit_status(request['project'], colour=True)",
    ] {
        let text = format!(
            "def set_status(request):\n    write = {wrong}\n    return {{'success': True}}\n"
        );
        let (_, function) = source::read("x.py", &text, "set_status").unwrap();
        assert!(
            symbolic::execute(&function, &contract(source::Language::Python)).is_err(),
            "{wrong}"
        );
    }
}

#[test]
fn a_left_out_parameter_takes_its_default() {
    let text = "def f(request, *, strict=True):\n    return request['allowed'] and strict\n";
    let (_, function) = source::read("x.py", text, "f").unwrap();
    let mut contract: Contract = serde_json::from_value(json!({
        "inputs": {"request": {"allowed": "Bool"}},
        "parameters": ["request", null],
        "ensures": [{"name": "allowed", "condition": "result == request.allowed"}],
    }))
    .unwrap();
    let execution = symbolic::execute(&function, &contract).unwrap();
    assert_eq!(execution.paths.len(), 1);
    contract.parameters = vec![None, None];
    let error = symbolic::execute(&function, &contract).err().unwrap();
    assert!(error.contains("no default"), "{error}");
}

fn nullable_contract(ensures: &str) -> Json {
    json!({
        "inputs": {"request": {"user": ["Null", {"admin": "Bool"}], "note": ["Text", "Null"]}},
        "parameters": ["request"],
        "ensures": [{"name": "property", "condition": ensures}],
    })
}

#[test]
fn null_literals_and_identity_with_none_are_read() {
    for (file, text) in [
        (
            "x.py",
            "def f(request):\n    if request.note is None:\n        return None\n    return request.note\n",
        ),
        (
            "x.ts",
            "function f(request) {\n  if (request.note === null) {\n    return undefined;\n  }\n  return request.user?.admin ?? false;\n}\n",
        ),
        (
            "x.go",
            "package p\nfunc f(request Request) *bool {\n  if request.user == nil {\n    return nil\n  }\n  return request.user\n}\n",
        ),
    ] {
        assert!(source::read(file, text, "f").is_ok(), "{file}");
    }
    let identity = "def f(request):\n    return request.note is request.user\n";
    assert!(source::read("x.py", identity, "f").is_err());
    let optional_call = "function f(request) { return request.check?.(); }";
    assert!(source::read("x.ts", optional_call, "f").is_err());
}

#[test]
fn a_field_read_of_null_ends_the_path_but_a_guarded_read_does_not() {
    let contract: Contract =
        serde_json::from_value(nullable_contract("result == true || result == false")).unwrap();
    let unguarded = "function f(request) {\n  return request.user.admin;\n}\n";
    let (_, function) = source::read("x.ts", unguarded, "f").unwrap();
    let execution = symbolic::execute(&function, &contract).unwrap();
    assert_eq!(execution.paths.len(), 1);
    assert_ne!(execution.paths[0].guard.code, execution.requires.code);
    let guarded = "function f(request) {\n  if (request.user !== null && request.user.admin) {\n    return true;\n  }\n  return false;\n}\n";
    let (_, function) = source::read("x.ts", guarded, "f").unwrap();
    let execution = symbolic::execute(&function, &contract).unwrap();
    assert_eq!(execution.paths.len(), 2);
    assert!(
        execution
            .paths
            .iter()
            .all(|path| path.guard.code != "false")
    );
}

#[test]
#[ignore = "requires native Z3; set LEMMA_SMT_SOLVER and run with --ignored"]
fn null_semantics_agree_with_the_languages() {
    // A guarded read proves; an unguarded read of a field of null raises,
    // so its normal returns still satisfy the property.
    let admin_only = nullable_contract("!result || (request.user != null && request.user.admin)");
    for (file, text) in [
        (
            "x.ts",
            "function f(request) {\n  return request.user?.admin ?? false;\n}\n",
        ),
        (
            "x.py",
            "def f(request):\n    if request.user is None:\n        return False\n    return request.user.admin\n",
        ),
        (
            "x.go",
            "package p\nfunc f(request Request) bool {\n  if request.user == nil {\n    return false\n  }\n  return request.user.admin\n}\n",
        ),
        (
            "x.ts",
            "function f(request) {\n  return request.user.admin;\n}\n",
        ),
    ] {
        let result = check_source(file, text, "f", &admin_only);
        assert_eq!(result["observed"]["status"], "proved", "{file}: {result}");
    }
    // Treating a missing user as an admin is caught.
    let wrong = "def f(request):\n    if request.user is None:\n        return True\n    return request.user.admin\n";
    let result = check_source("x.py", wrong, "f", &admin_only);
    assert_eq!(result["observed"]["status"], "violated", "{result}");
    // A postcondition that reads a field of null is never satisfied by it.
    let careless = nullable_contract("request.user.admin || !request.user.admin");
    let result = check_source("x.py", "def f(request):\n    return True\n", "f", &careless);
    assert_eq!(result["observed"]["status"], "violated", "{result}");
    // A precondition must be defined for every input.
    let mut undefined_precondition = nullable_contract("result");
    undefined_precondition["requires"] = json!(["request.user.admin"]);
    let result = check_source(
        "x.py",
        "def f(request):\n    return True\n",
        "f",
        &undefined_precondition,
    );
    assert!(
        result["unavailable"]
            .as_str()
            .unwrap()
            .contains("precondition"),
        "{result}"
    );
}

#[test]
fn strings_are_decoded_from_their_escapes_and_fragments() {
    for (file, text, expected) in [
        (
            "x.py",
            "def f():\n    return 'a\\tb\\u00e9' 'c'\n",
            "a\tbéc",
        ),
        ("x.py", "def f():\n    return r'\\d'\n", "\\d"),
        (
            "x.py",
            "def f():\n    return \"\"\"line\nnext\"\"\"\n",
            "line\nnext",
        ),
        (
            "x.ts",
            "function f() { return 'it\\'s\\u{1F600}'; }",
            "it's😀",
        ),
        (
            "x.go",
            "package p\nfunc f() string { return \"a\\x41\" }",
            "aA",
        ),
        (
            "x.go",
            "package p\nfunc f() string { return `raw\\n` }",
            "raw\\n",
        ),
    ] {
        let (_, function) = source::read(file, text, "f").unwrap();
        let ir::StatementKind::Return(ir::Expression::Text(value)) = &function.body[0].kind else {
            panic!("{file}: {:?}", function.body);
        };
        assert_eq!(value, expected, "{file}");
    }
    for (file, text) in [
        ("x.py", "def f(x):\n    return b'bytes'\n"),
        ("x.py", "def f(x):\n    return f'{x!r}'\n"),
        ("x.py", "def f(x):\n    return f'{x:>3}'\n"),
        ("x.py", "def f(x):\n    return f'{x=}'\n"),
        ("x.py", "def f(x):\n    return '\\d'\n"),
        ("x.py", "def f(x):\n    return x / 2\n"),
    ] {
        assert!(source::read(file, text, "f").is_err(), "{text}");
    }
}

#[test]
#[ignore = "requires native Z3; set LEMMA_SMT_SOLVER and run with --ignored"]
fn formatted_strings_spell_values_as_each_language_does() {
    let contract = |ensures: &str| {
        json!({
            "inputs": {"r": {"name": "Text", "flag": "Bool"}},
            "parameters": ["r"],
            "ensures": [{"name": "property", "condition": ensures}],
        })
    };
    for (file, text, ensures) in [
        (
            "x.py",
            "def f(r):\n    return f'{r.name}!'\n",
            "result == r.name + \"!\"",
        ),
        (
            "x.ts",
            "function f(r) { return `${r.name}!`; }",
            "result == r.name + \"!\"",
        ),
        (
            "x.py",
            "def f(r):\n    return f'{r.flag}'\n",
            "result == \"True\" || result == \"False\"",
        ),
        (
            "x.ts",
            "function f(r) { return `${r.flag}`; }",
            "result == \"true\" || result == \"false\"",
        ),
        (
            "x.go",
            "package p\nfunc f(r R) string { return r.name + \"!\" }",
            "result == r.name + \"!\"",
        ),
    ] {
        let result = check_source(file, text, "f", &contract(ensures));
        assert_eq!(result["observed"]["status"], "proved", "{file}: {result}");
    }
    let wrong = check_source(
        "x.ts",
        "function f(r) { return `${r.flag}`; }",
        "f",
        &contract("result == \"True\" || result == \"False\""),
    );
    assert_eq!(wrong["observed"]["status"], "violated", "{wrong}");
}

#[test]
#[ignore = "requires native Z3; set LEMMA_SMT_SOLVER and run with --ignored"]
fn integer_arithmetic_is_exact_in_python_and_assumed_elsewhere() {
    let contract = json!({
        "inputs": {"r": {"count": "Int"}},
        "parameters": ["r"],
        "ensures": [{"name": "grows", "condition": "result == r.count + 1 && result > r.count"}],
    });
    for (file, text, status) in [
        ("x.py", "def f(r):\n    return r.count + 1\n", "proved"),
        (
            "x.ts",
            "function f(r) { return r.count + 1; }",
            "conditional",
        ),
        (
            "x.go",
            "package p\nfunc f(r R) int { return r.count + 1 }",
            "conditional",
        ),
    ] {
        let result = check_source(file, text, "f", &contract);
        assert_eq!(result["observed"]["status"], status, "{file}: {result}");
    }
    let wrong = check_source(
        "x.py",
        "def f(r):\n    return r.count - 1\n",
        "f",
        &contract,
    );
    assert_eq!(wrong["observed"]["status"], "violated", "{wrong}");
}

#[test]
#[ignore = "requires native Z3; set LEMMA_SMT_SOLVER and run with --ignored"]
fn a_path_that_raises_has_no_normal_return_to_check() {
    let contract = json!({
        "inputs": {"r": {"allowed": "Bool"}},
        "parameters": ["r"],
        "ensures": [{"name": "only_when_allowed", "condition": "r.allowed && result"}],
    });
    for (file, text, status) in [
        (
            "x.py",
            "def f(r):\n    if not r.allowed:\n        raise PermissionError(f'denied {r}')\n    return True\n",
            "proved",
        ),
        (
            "x.ts",
            "function f(r) {\n  if (!r.allowed) {\n    throw new Error('denied');\n  }\n  return true;\n}\n",
            "proved",
        ),
        (
            "x.go",
            "package p\nfunc f(r R) bool {\n  if !r.allowed {\n    panic(\"denied\")\n  }\n  return true\n}\n",
            "proved",
        ),
        (
            "x.py",
            "def f(r):\n    assert r.allowed, 'denied'\n    return True\n",
            "conditional",
        ),
    ] {
        let result = check_source(file, text, "f", &contract);
        assert_eq!(result["observed"]["status"], status, "{file}: {result}");
    }
    let wrong = check_source(
        "x.py",
        "def f(r):\n    if r.allowed:\n        raise ValueError()\n    return True\n",
        "f",
        &contract,
    );
    assert_eq!(wrong["observed"]["status"], "violated", "{wrong}");
    let always = check_source(
        "x.py",
        "def f(r):\n    raise ValueError()\n",
        "f",
        &contract,
    );
    assert!(
        always["unavailable"]
            .as_str()
            .unwrap()
            .contains("no reachable normal return"),
        "{always}"
    );
}

#[test]
#[ignore = "requires native Z3; set LEMMA_SMT_SOLVER and run with --ignored"]
fn sequences_unpack_index_and_contain_as_each_language_does() {
    let lookup = json!({
        "inputs": {"r": {"id": "Text", "role": "Text"}},
        "parameters": ["r"],
        "models": {"find": {
            "assumption": "find answers whether the account exists, or an error",
            "parameters": ["id"],
            "outcomes": {"found": "Bool", "error": ["Null", "Text"]},
            "returns": [{"expr": "outcome.found"}, {"expr": "outcome.error"}],
        }},
        "ensures": [{"name": "errors_are_reported", "condition": "result[1] == null || !result[0]"}],
    });
    let go = "package p\nfunc f(r R) (bool, error) {\n  found, err := find(r.id)\n  if err != nil {\n    return false, err\n  }\n  return found, nil\n}\n";
    let py = "def f(r):\n    found, error = find(r.id)\n    if error is not None:\n        return (False, error)\n    return found, None\n";
    for (file, text) in [("x.go", go), ("x.py", py)] {
        let result = check_source(file, text, "f", &lookup);
        assert_eq!(
            result["observed"]["status"], "conditional",
            "{file}: {result}"
        );
    }
    let leaky = py.replace("return (False, error)", "return (True, error)");
    let result = check_source("x.py", &leaky, "f", &lookup);
    assert_eq!(result["observed"]["status"], "violated", "{result}");
    let roles = json!({
        "inputs": {"r": {"role": "Text"}},
        "parameters": ["r"],
        "ensures": [{"name": "staff", "condition": "result == (r.role == \"admin\" || r.role == \"owner\")"}],
    });
    for (file, text) in [
        (
            "x.py",
            "def f(r):\n    return r.role in ('admin', 'owner')\n",
        ),
        (
            "x.py",
            "def f(r):\n    staff = ['admin', 'owner']\n    return staff[0] == r.role or staff[-1] == r.role\n",
        ),
        (
            "x.ts",
            "function f(r) {\n  const [first, second] = ['admin', 'owner'];\n  return r.role === first || r.role === second;\n}\n",
        ),
    ] {
        let result = check_source(file, text, "f", &roles);
        assert_eq!(result["observed"]["status"], "proved", "{file}: {result}");
    }
}

#[test]
fn unpacking_a_sequence_of_another_length_follows_the_language() {
    let contract: Contract = serde_json::from_value(json!({
        "inputs": {"r": {"ok": "Bool"}},
        "parameters": ["r"],
        "ensures": [{"name": "any", "condition": "true"}],
    }))
    .unwrap();
    let python = "def f(r):\n    a, b = (r.ok,)\n    return a\n";
    let (_, function) = source::read("x.py", python, "f").unwrap();
    assert!(
        symbolic::execute(&function, &contract)
            .unwrap()
            .paths
            .is_empty()
    );
    let typescript = "function f(r) {\n  const [a, b] = [r.ok];\n  return b;\n}\n";
    let (_, function) = source::read("x.ts", typescript, "f").unwrap();
    let execution = symbolic::execute(&function, &contract).unwrap();
    assert!(matches!(
        execution.paths[0].result,
        Some(symbolic::Value::Undefined)
    ));
}

#[test]
#[ignore = "requires native Z3; set LEMMA_SMT_SOLVER and run with --ignored"]
fn type_syntax_conditional_forms_and_chains_read_as_their_runtime_meaning() {
    let contract = json!({
        "inputs": {"r": {"level": "Int", "role": "Text"}},
        "parameters": ["r"],
        "ensures": [{"name": "middle", "condition": "result == (1 < r.level && r.level <= 3)"}],
    });
    for (file, text) in [
        ("x.py", "def f(r):\n    return 1 < r.level <= 3\n"),
        (
            "x.py",
            "def f(r):\n    return True if 1 < r.level and r.level <= 3 else False\n",
        ),
        (
            "x.ts",
            "function f(r) {\n  return ((1 < (r as Request).level) satisfies boolean) && (r!.level <= 3);\n}\n",
        ),
        (
            "x.go",
            "package p\nfunc f(r R) bool {\n  var low = 1\n  bounds := []int{low, 3}\n  return bounds[0] < r.level && r.level <= bounds[1]\n}\n",
        ),
    ] {
        let result = check_source(file, text, "f", &contract);
        assert!(
            matches!(
                result["observed"]["status"].as_str(),
                Some("proved" | "conditional")
            ),
            "{file}: {result}"
        );
        assert_eq!(result["observed"]["holds"], true, "{file}: {result}");
    }
    let staff = json!({
        "inputs": {"r": {"role": "Text"}},
        "parameters": ["r"],
        "ensures": [{"name": "staff", "condition": "result == (r.role == \"admin\" || r.role == \"owner\")"}],
    });
    let result = check_source(
        "x.py",
        "def f(r):\n    return r.role in {'admin', 'owner'}\n",
        "f",
        &staff,
    );
    assert_eq!(result["observed"]["status"], "proved", "{result}");
}

#[test]
fn a_set_is_read_for_membership_only_and_chains_with_calls_are_refused() {
    let contract: Contract = serde_json::from_value(json!({
        "inputs": {"r": {"role": "Text"}},
        "parameters": ["r"],
        "ensures": [{"name": "any", "condition": "true"}],
    }))
    .unwrap();
    let (_, function) = source::read(
        "x.py",
        "def f(r):\n    a, b = {'x', 'y'}\n    return a\n",
        "f",
    )
    .unwrap();
    assert!(symbolic::execute(&function, &contract).is_err());
    assert!(source::read("x.py", "def f(r):\n    return 1 < g(r) < 3\n", "f").is_err());
    // The grammar would group this as `(r.a && r)!.b`.
    assert!(source::read("x.ts", "function f(r) { return r.a && r!.b; }", "f").is_err());
    assert!(
        source::read(
            "x.go",
            "package p\nfunc f(r R) bool { var ok bool; return ok }",
            "f"
        )
        .is_err()
    );
}

#[test]
fn a_set_holding_a_list_is_refused() {
    let contract: Contract = serde_json::from_value(json!({
        "inputs": {"r": {"role": "Text"}},
        "parameters": ["r"],
        "ensures": [{"name": "any", "condition": "true"}],
    }))
    .unwrap();
    let execute = |body: &str| {
        let (_, function) = source::read("x.py", body, "f").unwrap();
        symbolic::execute(&function, &contract)
    };

    // Python raises TypeError here; saying nothing would let a proof hold
    // for code that cannot run.
    assert!(execute("def f(r):\n    seen = {[1, 2]}\n    return 1\n").is_err());
    assert!(execute("def f(r):\n    return r.role in {'admin', 'owner'}\n").is_ok());
}

#[test]
#[ignore = "requires native Z3; set LEMMA_SMT_SOLVER and run with --ignored"]
fn locals_are_reassigned_and_fresh_objects_are_built_field_by_field() {
    let contract = json!({
        "inputs": {"r": {"limit": "Int", "cursor": ["Null", "Text"]}},
        "parameters": ["r"],
        "ensures": [{"name": "query", "condition": "result.limit == r.limit + 1 && (r.cursor == null || result.after == r.cursor)"}],
    });
    for (file, text) in [
        (
            "x.py",
            "def f(r):\n    params = {'limit': r.limit}\n    params['limit'] += 1\n    if r.cursor is not None:\n        params['after'] = r.cursor\n    return params\n",
        ),
        (
            "x.ts",
            "function f(r) {\n  const params = { limit: r.limit };\n  params.limit += 1;\n  if (r.cursor !== null) {\n    params.after = r.cursor;\n  }\n  return params;\n}\n",
        ),
        (
            "x.go",
            "package p\nfunc f(r R) Q {\n  limit := r.limit\n  limit = limit + 1\n  q := Q{limit: limit}\n  if r.cursor != nil {\n    q.after = r.cursor\n  }\n  return q\n}\n",
        ),
    ] {
        let result = check_source(file, text, "f", &contract);
        assert_eq!(result["observed"]["holds"], true, "{file}: {result}");
    }
}

#[test]
fn objects_others_can_see_are_never_assigned_and_blocks_scope_declarations() {
    let contract: Contract = serde_json::from_value(json!({
        "inputs": {"r": {"ok": "Bool"}},
        "parameters": ["r"],
        "models": {"send": {"assumption": "send records its argument", "parameters": ["body"], "returns": true}},
        "ensures": [{"name": "any", "condition": "true"}],
    }))
    .unwrap();
    let mutates_input = "def f(r):\n    r.ok = True\n    return r\n";
    assert!(source::read("x.py", mutates_input, "f").is_err());
    for text in [
        "def f(r):\n    body = {}\n    send(body)\n    body['late'] = True\n    return body\n",
        "def f(r):\n    body = {}\n    alias = body\n    body['late'] = True\n    return alias\n",
    ] {
        let (_, function) = source::read("x.py", text, "f").unwrap();
        let error = symbolic::execute(&function, &contract).err().unwrap();
        assert!(error.contains("seen elsewhere"), "{text}: {error}");
    }
    let shadowed =
        "function f(r) {\n  const x = 1;\n  if (r.ok) {\n    const x = 2;\n  }\n  return x;\n}\n";
    let (_, function) = source::read("x.ts", shadowed, "f").unwrap();
    let execution = symbolic::execute(&function, &contract).unwrap();
    for path in &execution.paths {
        assert!(matches!(&path.result, Some(symbolic::Value::Scalar(term)) if term.code == "1"));
    }
    let undeclared = "function f(r) {\n  y = 1;\n  return y;\n}\n";
    let (_, function) = source::read("x.ts", undeclared, "f").unwrap();
    assert!(symbolic::execute(&function, &contract).is_err());
}

#[test]
#[ignore = "requires native Z3; set LEMMA_SMT_SOLVER and run with --ignored"]
fn handlers_catch_what_the_body_raises_and_finally_runs_on_every_way_out() {
    let contract = json!({
        "inputs": {"r": {"allowed": "Bool", "user": ["Null", {"name": "Text"}]}},
        "parameters": ["r"],
        "ensures": [{"name": "outcome", "condition": "(!r.allowed && result == \"denied\") || (r.allowed && r.user == null && result == \"error\") || (r.allowed && r.user != null && result == r.user.name)"}],
    });
    let python = "def f(r):\n    try:\n        if not r.allowed:\n            raise PermissionError('no')\n        name = r.user.name\n    except PermissionError:\n        return 'denied'\n    except Exception:\n        return 'error'\n    else:\n        return name\n    finally:\n        pass\n";
    let result = check_source("x.py", python, "f", &contract);
    assert_eq!(result["observed"]["status"], "proved", "{result}");
    let swapped = python.replace("return 'denied'", "return 'error'");
    let result = check_source("x.py", &swapped, "f", &contract);
    assert_eq!(result["observed"]["status"], "violated", "{result}");
    let typescript = "function f(r) {\n  try {\n    if (!r.allowed) {\n      return 'denied';\n    }\n    return r.user.name;\n  } catch (error) {\n    return 'error';\n  }\n}\n";
    let result = check_source("x.ts", typescript, "f", &contract);
    assert_eq!(result["observed"]["status"], "proved", "{result}");
    let overriding = json!({
        "inputs": {"r": {"allowed": "Bool"}},
        "parameters": ["r"],
        "ensures": [{"name": "finally_wins", "condition": "result == 2"}],
    });
    let result = check_source(
        "x.py",
        "def f(r):\n    try:\n        return 1\n    finally:\n        return 2\n",
        "f",
        &overriding,
    );
    assert_eq!(result["observed"]["status"], "proved", "{result}");
}

#[test]
fn an_unknown_relation_between_exception_classes_is_refused() {
    let contract: Contract = serde_json::from_value(json!({
        "inputs": {"r": {"ok": "Bool"}},
        "parameters": ["r"],
        "ensures": [{"name": "any", "condition": "true"}],
    }))
    .unwrap();
    let text = "def f(r):\n    try:\n        raise NotFound('x')\n    except ValueError:\n        return False\n";
    let (_, function) = source::read("x.py", text, "f").unwrap();
    let error = symbolic::execute(&function, &contract).err().unwrap();
    assert!(error.contains("not known"), "{error}");
    let reraised =
        "def f(r):\n    try:\n        raise KeyError('x')\n    except KeyError:\n        raise\n";
    let (_, function) = source::read("x.py", reraised, "f").unwrap();
    assert!(
        symbolic::execute(&function, &contract)
            .unwrap()
            .paths
            .is_empty()
    );
}

#[test]
#[ignore = "requires native Z3; set LEMMA_SMT_SOLVER and run with --ignored"]
fn with_enters_each_context_manager_under_its_context_model() {
    let mut contract = json!({
        "inputs": {"r": {"id": "Text"}},
        "parameters": ["r"],
        "models": {"open_account": {"assumption": "open_account returns the account it opens", "parameters": ["id"], "returns": {"id": {"expr": "id"}}}},
        "ensures": [{"name": "same_account", "condition": "result == r.id"}],
    });
    let text = "def f(r):\n    with lock:\n        with open_account(r.id) as account:\n            return account.id\n";
    let refused = check_source("x.py", text, "f", &contract);
    assert!(
        refused["unavailable"]
            .as_str()
            .unwrap()
            .contains("context model"),
        "{refused}"
    );
    contract["contexts"] = json!({
        "lock": "the lock is released on leaving and suppresses nothing",
        "open_account": "an account entered is the account opened, and leaving closes it without other effects",
    });
    let result = check_source("x.py", text, "f", &contract);
    assert_eq!(result["observed"]["status"], "conditional", "{result}");
    assert_eq!(
        result["observed"]["assumptions"].as_array().unwrap().len(),
        3,
        "{result}"
    );
}

#[test]
fn tsx_files_read_their_typescript_and_refuse_jsx() {
    let helper = "export function label(user: User): string {\n  return user.admin ? 'Admin' : 'Member';\n}\nexport function Badge(user: User) {\n  return <span>{label(user)}</span>;\n}\n";
    assert!(source::read("x.tsx", helper, "label").is_ok());
    assert!(source::read("x.tsx", helper, "Badge").is_err());
}

#[test]
#[ignore = "requires native Z3; set LEMMA_SMT_SOLVER and run with --ignored"]
fn nested_functions_and_object_methods_are_read_with_their_captures() {
    let contract = json!({
        "inputs": {"limit": "Int", "r": {"count": "Int"}},
        "parameters": ["r"],
        "captures": {"limit": "limit"},
        "ensures": [{"name": "within", "condition": "result == (r.count <= limit)"}],
    });
    for (file, text, symbol) in [
        (
            "x.py",
            "def make(limit):\n    def within(r):\n        return r.count <= limit\n    return within\n",
            "make.within",
        ),
        (
            "x.ts",
            "export function make(limit: number) {\n  const within = (r) => r.count <= limit;\n  return within;\n}\n",
            "make.within",
        ),
        (
            "x.py",
            "class Policy:\n    def check(self, limit):\n        def within(r):\n            return r.count <= limit\n        return within\n",
            "Policy.check.within",
        ),
    ] {
        let result = check_source(file, text, symbol, &contract);
        assert!(
            matches!(
                result["observed"]["status"].as_str(),
                Some("proved" | "conditional")
            ),
            "{file} {symbol}: {result}"
        );
        assert_eq!(result["observed"]["holds"], true, "{result}");
    }
    let object = json!({
        "inputs": {"api": {"base": "Text"}, "r": {"path": "Text"}},
        "parameters": ["api", "r"],
        "ensures": [{"name": "joined", "condition": "result == api.base + r.path"}],
    });
    let text = "export const client = {\n  url(r) {\n    return this.base + r.path;\n  },\n};\n";
    let result = check_source("x.ts", text, "client.url", &object);
    assert_eq!(result["observed"]["holds"], true, "{result}");
}

#[test]
fn definitions_inside_anonymous_functions_are_found_by_their_indexed_name() {
    let callback =
        "describe('router', () => {\n  function Home(props) {\n    return props.ready;\n  }\n});\n";
    let (_, function) = source::read("x.ts", callback, "Home").unwrap();
    assert_eq!(function.parameters.len(), 1);
    let getter = "export function createRouter(routes) {\n  return {\n    get routes() {\n      return this.list;\n    },\n  };\n}\n";
    let (_, function) = source::read("x.ts", getter, "createRouter.routes").unwrap();
    assert_eq!(function.parameters[0].name, "this");
    let twice =
        "a(() => { function f() { return 1; } });\nb(() => { function f() { return 2; } });\n";
    assert!(source::read("x.ts", twice, "f").is_err());
}

#[test]
#[ignore = "requires native Z3; set LEMMA_SMT_SOLVER and run with --ignored"]
fn local_functions_are_called_by_running_their_bodies() {
    let contract = |ensures: &str| {
        json!({
            "inputs": {"r": {"count": "Int"}},
            "parameters": ["r"],
            "ensures": [{"name": "property", "condition": ensures}],
        })
    };
    let plus_one = contract("result == r.count + 1");
    for (file, text) in [
        (
            "x.py",
            "def f(r):\n    g = lambda y, z=1: y + z\n    def h(a):\n        return a\n    return h(g(r.count))\n",
        ),
        (
            "x.ts",
            "function f(r) {\n  const g = (y) => y + 1;\n  const k = function (a) { return a; };\n  return h(k(g(r.count)));\n  function h(a) { return a; }\n}\n",
        ),
        (
            "x.go",
            "package p\nfunc f(r R) int {\n  g := func(y int) int { return y + 1 }\n  return g(r.count)\n}\n",
        ),
    ] {
        let result = check_source(file, text, "f", &plus_one);
        assert_eq!(result["observed"]["holds"], true, "{file}: {result}");
    }
    let sign = contract("(r.count < 0 && result == -1) || (r.count >= 0 && result == 1)");
    let branching = "function f(r) {\n  const sign = (n) => {\n    if (n < 0) {\n      return -1;\n    }\n    return 1;\n  };\n  return sign(r.count);\n}\n";
    let result = check_source("x.ts", branching, "f", &sign);
    assert_eq!(result["observed"]["holds"], true, "{result}");
    let wrong = branching.replace("return -1", "return 1");
    let result = check_source("x.ts", &wrong, "f", &sign);
    assert_eq!(result["observed"]["status"], "violated", "{result}");
    let marks = contract("result == true");
    let outer = "function f(r) {\n  let seen = false;\n  const mark = () => {\n    seen = true;\n  };\n  mark();\n  return seen;\n}\n";
    let result = check_source("x.ts", outer, "f", &marks);
    assert_eq!(result["observed"]["holds"], true, "{result}");
}

#[test]
fn functions_passed_to_models_and_branching_effects_are_refused() {
    let mut contract: Contract = serde_json::from_value(json!({
        "inputs": {"r": {"ok": "Bool"}},
        "parameters": ["r"],
        "models": {"each": {"assumption": "each runs nothing it is given", "parameters": ["items", "callback"], "returns": true},
                   "log": {"assumption": "log records", "parameters": ["message"], "returns": true}},
        "ensures": [{"name": "any", "condition": "true"}],
    }))
    .unwrap();
    let callback = "function f(r) {\n  return each([], (item) => item);\n}\n";
    let (_, function) = source::read("x.ts", callback, "f").unwrap();
    let error = symbolic::execute(&function, &contract).err().unwrap();
    assert!(error.contains("ignores_callbacks"), "{error}");
    contract.models.get_mut("each").unwrap().ignores_callbacks = true;
    assert!(symbolic::execute(&function, &contract).is_ok());
    let effects = "function f(r) {\n  const g = () => {\n    if (r.ok) {\n      log('a');\n    }\n    return 1;\n  };\n  return g() + 1;\n}\n";
    let (_, function) = source::read("x.ts", effects, "f").unwrap();
    let error = symbolic::execute(&function, &contract).err().unwrap();
    assert!(error.contains("different effects"), "{error}");
}

#[test]
#[ignore = "requires native Z3; set LEMMA_SMT_SOLVER and run with --ignored"]
fn spreads_rest_patterns_and_constructors_follow_their_languages() {
    let contract = json!({
        "inputs": {"r": {"id": "Text", "tags": "Text"}},
        "parameters": ["r"],
        "models": {"new URL": {"assumption": "a URL keeps the address it is given", "parameters": ["address"], "returns": {"href": {"expr": "address"}}}},
        "ensures": [{"name": "kept", "condition": "result.id == r.id && result.kind == \"user\""}],
    });
    for (file, text) in [
        (
            "x.ts",
            "function f(r) {\n  const base = { kind: 'guest', id: 'none' };\n  const { tags, ...rest } = { ...base, ...r, kind: 'user' };\n  return rest;\n}\n",
        ),
        (
            "x.py",
            "def f(r):\n    base = {'kind': 'guest'}\n    return {**base, 'id': r.id, 'kind': 'user'}\n",
        ),
    ] {
        let result = check_source(file, text, "f", &contract);
        assert_eq!(result["observed"]["holds"], true, "{file}: {result}");
    }
    let sequences = json!({
        "inputs": {"r": {"id": "Text"}},
        "parameters": ["r"],
        "ensures": [{"name": "middle", "condition": "result == r.id"}],
    });
    for (file, text) in [
        (
            "x.ts",
            "function f(r) {\n  const [first, ...others] = ['a', ...[r.id, 'c']];\n  return others[0];\n}\n",
        ),
        (
            "x.py",
            "def f(r):\n    first, *others, last = ['a', *[r.id], 'c']\n    return others[0]\n",
        ),
    ] {
        let result = check_source(file, text, "f", &sequences);
        assert_eq!(result["observed"]["holds"], true, "{file}: {result}");
    }
    let url = json!({
        "inputs": {"r": {"id": "Text"}},
        "parameters": ["r"],
        "models": {"new URL": {"assumption": "a URL keeps the address it is given", "parameters": ["address"], "returns": {"href": {"expr": "address"}}}},
        "ensures": [{"name": "address", "condition": "result == r.id"}],
    });
    let result = check_source(
        "x.ts",
        "function f(r) {\n  return new URL(r.id).href;\n}\n",
        "f",
        &url,
    );
    assert_eq!(result["observed"]["status"], "conditional", "{result}");
}

/// The value every path of `f` returns, or why the function was refused.
fn returned(file: &str, text: &str, contract: &Json) -> Result<Vec<String>, String> {
    let contract: Contract = serde_json::from_value(contract.clone()).unwrap();
    let (_, function) = source::read(file, text, "f")?;
    let execution = symbolic::execute(&function, &contract)?;
    Ok(execution
        .paths
        .iter()
        .map(|path| match &path.result {
            Some(symbolic::Value::Scalar(term)) => term.code.clone(),
            other => format!("{other:?}"),
        })
        .collect())
}

fn scope_contract() -> Json {
    json!({
        "inputs": {"r": {"ok": "Bool"}},
        "parameters": ["r"],
        "contexts": {"manager": "the manager is entered and left without another effect"},
        "models": {"manager": {"assumption": "manager returns a context manager", "returns": true}},
        "ensures": [{"name": "any", "condition": "true"}],
    })
}

#[test]
fn a_declaration_in_a_try_body_or_a_handler_ends_with_its_block() {
    let contract = scope_contract();
    for text in [
        "function f(r) {\n  const x = 1;\n  try {\n    const x = 2;\n  } catch (e) {\n  }\n  return x;\n}\n",
        "function f(r) {\n  const x = 1;\n  try {\n    throw new Error();\n  } catch (e) {\n    const x = 2;\n  } finally {\n    const x = 3;\n  }\n  return x;\n}\n",
    ] {
        assert_eq!(returned("x.ts", text, &contract).unwrap(), ["1"], "{text}");
    }
}

#[test]
fn a_called_function_reads_only_the_variables_it_was_defined_with() {
    let contract = scope_contract();
    // Each of these calls a function where a name it reads means another
    // variable: one of an inner block, of a function that has returned, or
    // of the caller it runs in.
    for (file, text) in [
        (
            "x.ts",
            "function f(r) {\n  const x = 1;\n  let g = () => 0;\n  if (r.ok) {\n    const x = 2;\n    g = () => x;\n  }\n  return g();\n}\n",
        ),
        (
            "x.py",
            "def f(r):\n    def make():\n        x = 1\n        def inner():\n            return x\n        return inner\n    x = 5\n    g = make()\n    return g()\n",
        ),
        (
            "x.py",
            "def f(r):\n    x = 1\n    def get():\n        return x\n    def call(x):\n        return get()\n    return call(2)\n",
        ),
        (
            "x.go",
            "package p\nfunc f(r R) int {\n  x := 1\n  g := func() int { return x }\n  if r.ok {\n    x := 2\n    _ = x\n    return g()\n  }\n  return g()\n}\n",
        ),
    ] {
        let error = returned(file, text, &contract).unwrap_err();
        assert!(error.contains("another variable"), "{text}: {error}");
    }
    // Functions defined in any order still find each other.
    for (file, text) in [
        (
            "x.ts",
            "function f(r) {\n  const a = () => b();\n  const b = () => 1;\n  return a();\n}\n",
        ),
        (
            "x.py",
            "def f(r):\n    def a():\n        return b()\n    def b():\n        return 1\n    return a()\n",
        ),
    ] {
        assert_eq!(returned(file, text, &contract).unwrap(), ["1"], "{text}");
    }
}

#[test]
fn a_name_a_python_function_assigns_is_its_own_from_the_start() {
    let contract = scope_contract();
    // Python raises here: `x` is local to `g`, and unbound where it is read.
    let unbound = "def f(r):\n    x = 1\n    def g():\n        y = x\n        x = 2\n        return y\n    return g()\n";
    assert!(returned("x.py", unbound, &contract).is_err());
    // What a called function's `with` binds stays in that function.
    let with_target = "def f(r):\n    def g():\n        with manager() as x:\n            pass\n        return 1\n    x = 5\n    g()\n    return x\n";
    assert_eq!(returned("x.py", with_target, &contract).unwrap(), ["5"]);
}

#[test]
fn a_python_default_is_evaluated_where_the_function_is_defined() {
    let contract = scope_contract();
    let python =
        "def f(r):\n    x = 1\n    def g(a=x):\n        return a\n    x = 2\n    return g()\n";
    assert_eq!(returned("x.py", python, &contract).unwrap(), ["1"]);
    // TypeScript evaluates a default at the call that leaves the parameter out.
    let typescript =
        "function f(r) {\n  let x = 1;\n  const g = (a = x) => a;\n  x = 2;\n  return g();\n}\n";
    assert_eq!(returned("x.ts", typescript, &contract).unwrap(), ["2"]);
}

fn loop_contract() -> Json {
    json!({
        "inputs": {"r": {"ok": "Bool"}, "xs": {"sequence": "Int", "max_length": 2}},
        "parameters": ["r", "xs"],
        "ensures": [{"name": "any", "condition": "true"}],
    })
}

#[test]
fn a_loop_runs_its_body_once_for_each_item_of_a_sequence() {
    let contract = loop_contract();
    for (file, text, expected) in [
        (
            "x.py",
            "def f(r, xs):\n    total = 0\n    for x in [1, 2, 3]:\n        if x == 2:\n            continue\n        total += x\n    return total\n",
            vec!["4"],
        ),
        (
            "x.ts",
            "function f(r, xs) {\n  let total = 0;\n  for (const x of [1, 2, 3]) {\n    total += x;\n  }\n  return total;\n}\n",
            vec!["6"],
        ),
        // A Go `range` names the position first, then the item.
        (
            "x.go",
            "package p\nfunc f(r R, xs []int) int {\n  total := 0\n  for i, x := range []int{5, 6} {\n    total = total + i + x\n  }\n  return total\n}\n",
            vec!["12"],
        ),
        // One path for each length a contract allows an input.
        (
            "x.py",
            "def f(r, xs):\n    total = 0\n    for x in xs:\n        total += x\n    return total\n",
            vec!["0", "(+ 0 input_2)", "(+ (+ 0 input_2) input_3)"],
        ),
    ] {
        assert_eq!(returned(file, text, &contract).unwrap(), expected, "{text}");
    }
}

#[test]
fn break_else_and_finally_leave_a_loop_as_the_languages_do() {
    let contract = loop_contract();
    for (file, text, expected) in [
        // Python's `else` runs only where the loop was not broken.
        (
            "x.py",
            "def f(r, xs):\n    for x in [1, 2, 3]:\n        if x == 2:\n            break\n    else:\n        return 0\n    return x\n",
            "2",
        ),
        (
            "x.py",
            "def f(r, xs):\n    for x in [1, 3]:\n        if x == 2:\n            break\n    else:\n        return 0\n    return x\n",
            "0",
        ),
        // `finally` runs on `continue`, which then goes on with the next pass.
        (
            "x.py",
            "def f(r, xs):\n    seen = 0\n    for x in [1, 2]:\n        try:\n            if x == 1:\n                continue\n            return x + seen\n        finally:\n            seen += 10\n    return 0\n",
            "12",
        ),
        // A `try` in a loop does not run its `else` where the body broke.
        (
            "x.py",
            "def f(r, xs):\n    for x in [1]:\n        try:\n            break\n        except ValueError:\n            pass\n        else:\n            return 7\n    return 0\n",
            "0",
        ),
        // What a TypeScript loop declares ends with the loop.
        (
            "x.ts",
            "function f(r, xs) {\n  const x = 9;\n  for (const x of [1]) {\n  }\n  return x;\n}\n",
            "9",
        ),
    ] {
        assert_eq!(
            returned(file, text, &contract).unwrap(),
            [expected],
            "{text}"
        );
    }
}

#[test]
fn a_loop_is_read_only_over_a_sequence_of_known_length() {
    let contract = loop_contract();
    let unknown = "def f(r, xs):\n    for x in r:\n        return x\n    return 0\n";
    let error = returned("x.py", unknown, &contract).unwrap_err();
    assert!(error.contains("max_length"), "{error}");
    for (file, text) in [
        (
            "x.py",
            "async def f(r, xs):\n    async for x in xs:\n        return x\n",
        ),
        (
            "x.ts",
            "function f(r, xs) {\n  for (const k in r) {\n    return k;\n  }\n}\n",
        ),
        (
            "x.ts",
            "async function f(r, xs) {\n  for await (const x of xs) {\n    return x;\n  }\n}\n",
        ),
        (
            "x.ts",
            "function f(r, xs) {\n  for (var x of xs) {\n    return x;\n  }\n}\n",
        ),
        (
            "x.ts",
            "function f(r, xs) {\n  outer: for (const x of xs) {\n    break outer;\n  }\n}\n",
        ),
        (
            "x.go",
            "package p\nfunc f(r R, xs []int) int {\n  for r.ok {\n    return 1\n  }\n  return 0\n}\n",
        ),
        (
            "x.go",
            "package p\nfunc f(r R, xs []int) int {\n  i := 0\n  for i = range xs {\n  }\n  return i\n}\n",
        ),
    ] {
        assert!(source::read(file, text, "f").is_err(), "{text}");
    }
    let too_long = json!({
        "inputs": {"xs": {"sequence": "Int", "max_length": 33}},
        "ensures": [{"name": "any", "condition": "true"}],
    });
    let error = returned("x.py", "def f(xs):\n    return 0\n", &too_long).unwrap_err();
    assert!(error.contains("max_length"), "{error}");
}

#[test]
fn each_pass_of_a_loop_has_its_own_variables_outside_python() {
    let contract = loop_contract();
    // The function reads the `x` of the pass that defined it, which has
    // ended where it is called.
    let typescript = "function f(r, xs) {\n  let g = () => 0;\n  for (const x of [1, 2]) {\n    if (x === 1) {\n      g = () => x;\n    }\n  }\n  return g();\n}\n";
    let error = returned("x.ts", typescript, &contract).unwrap_err();
    assert!(error.contains("another variable"), "{error}");
    // A Python loop binds one variable of the function, read as it is last.
    let python = "def f(r, xs):\n    g = lambda: 0\n    for x in [1, 2]:\n        if x == 1:\n            g = lambda: x\n    return g()\n";
    let [result] = &returned("x.py", python, &contract).unwrap()[..] else {
        panic!("one path");
    };
    assert!(result.contains("code: \"2\""), "{result}");
}

#[test]
#[ignore = "requires native Z3; set LEMMA_SMT_SOLVER and run with --ignored"]
fn loops_over_an_input_are_proved_for_every_length_it_may_have() {
    let all = json!({
        "inputs": {"xs": {"sequence": "Int", "max_length": 2}},
        "parameters": ["xs"],
        "ensures": [{"name": "all", "condition": "result == ((xs.length < 1 || xs[0] >= 0) && (xs.length < 2 || xs[1] >= 0))"}],
    });
    for (file, text) in [
        (
            "x.py",
            "def f(xs):\n    for x in xs:\n        if x < 0:\n            return False\n    return True\n",
        ),
        (
            "x.ts",
            "function f(xs) {\n  for (const x of xs) {\n    if (x < 0) {\n      return false;\n    }\n  }\n  return true;\n}\n",
        ),
        (
            "x.go",
            "package p\nfunc f(xs []int) bool {\n  for _, x := range xs {\n    if x < 0 {\n      return false\n    }\n  }\n  return true\n}\n",
        ),
    ] {
        let result = check_source(file, text, "f", &all);
        assert_eq!(result["observed"]["status"], "proved", "{file}: {result}");
        let mutated = check_source(file, &text.replace("x < 0", "x <= 0"), "f", &all);
        assert_eq!(mutated["observed"]["holds"], false, "{file}: {mutated}");
    }
    let counted = json!({
        "inputs": {"xs": {"sequence": "Int", "max_length": 3}},
        "parameters": ["xs"],
        "ensures": [{"name": "count", "condition": "result == xs.length"}],
    });
    for (file, text) in [
        ("x.py", "def f(xs):\n    return len(xs)\n"),
        ("x.ts", "function f(xs) {\n  return xs.length;\n}\n"),
        (
            "x.go",
            "package p\nfunc f(xs []int) int {\n  return len(xs)\n}\n",
        ),
        (
            "x.py",
            "def f(xs):\n    count = 0\n    for x in xs:\n        count += 1\n    return count\n",
        ),
    ] {
        let result = check_source(file, text, "f", &counted);
        assert_eq!(result["observed"]["holds"], true, "{file}: {result}");
    }
}

#[test]
fn a_branch_on_constants_follows_only_the_path_it_takes() {
    let contract = scope_contract();
    let text = "def f(r):\n    x = 1 + 2\n    if x == 2:\n        return 0\n    if x < 3:\n        return 1\n    if 'a' == 'b':\n        return 2\n    return x * 2\n";
    assert_eq!(returned("x.py", text, &contract).unwrap(), ["6"]);
}

#[test]
fn a_raise_python_makes_while_unpacking_or_spreading_is_caught_by_a_handler() {
    let contract = scope_contract();
    for text in [
        "def f(r):\n    try:\n        a, b = (1, 2, 3)\n    except ValueError:\n        return 1\n    return 2\n",
        "def f(r):\n    try:\n        a, *b, c = (1,)\n    except Exception:\n        return 1\n    return 2\n",
        "def f(r):\n    try:\n        d = {**None, \"a\": 1}\n    except Exception:\n        return 1\n    return 2\n",
    ] {
        assert_eq!(returned("x.py", text, &contract).unwrap(), ["1"], "{text}");
    }
}

fn comprehension_contract() -> Json {
    json!({
        "inputs": {"r": {"ok": "Bool"}, "xs": {"sequence": "Int", "max_length": 2}},
        "parameters": ["r", "xs"],
        "models": {
            "total": {"assumption": "total counts its values", "parameters": ["values"], "returns": {"expr": "values.length"}},
            "record": {"assumption": "record stores an item", "parameters": ["item"], "effect": {"name": "stored", "fields": {"item": {"expr": "item"}}}, "returns": {"expr": "item"}},
        },
        "ensures": [{"name": "any", "condition": "true"}],
    })
}

#[test]
fn a_comprehension_adds_an_item_for_each_pass_its_tests_let_through() {
    let contract = comprehension_contract();
    for (body, expected) in [
        (
            "ys = [x + 1 for x in [1, 2, 3]]\n    return ys[0] + ys[2]",
            "6",
        ),
        (
            "ys = [x for x in [1, 2, 3] if x != 2]\n    return ys[1]",
            "3",
        ),
        (
            "ys = [a * b for a in [1, 2] for b in [10, 20]]\n    return ys[3] + len(ys)",
            "44",
        ),
        (
            "ys = [a + b for a, b in [(1, 2), (3, 4)]]\n    return ys[1]",
            "7",
        ),
        (
            "ys = [b for a in [[1], [2, 3]] if len(a) == 2 for b in a]\n    return ys[0]",
            "2",
        ),
        (
            "d = {k: 1 for k in [\"a\", \"b\"]}\n    return d[\"b\"]",
            "1",
        ),
        // A later entry of a dict replaces an earlier one of the same key.
        ("d = {\"k\": v for v in [1, 2]}\n    return d[\"k\"]", "2"),
        ("return 2 in {x + 1 for x in [1, 2]}", "true"),
        ("return total(x for x in [1, 2, 3])", "3"),
        // Calls run in order where every input reaches every pass.
        ("ys = [record(x) for x in [4, 5]]\n    return ys[1]", "5"),
    ] {
        let text = format!("def f(r, xs):\n    {body}\n");
        assert_eq!(
            returned("x.py", &text, &contract).unwrap(),
            [expected],
            "{text}"
        );
    }
}

#[test]
fn the_names_of_a_comprehension_are_its_own() {
    let contract = comprehension_contract();
    for (body, expected) in [
        // The name it binds is not the function's.
        ("x = 5\n    ys = [x for x in [1, 2]]\n    return x", "5"),
        // The first iterable is evaluated where the comprehension is written.
        ("x = [1, 2]\n    ys = [x for x in x]\n    return ys[1]", "2"),
        // A handler does not see the name either.
        (
            "x = 5\n    try:\n        ys = [x.a for x in [None]]\n    except Exception:\n        return x\n    return 0",
            "5",
        ),
    ] {
        let text = format!("def f(r, xs):\n    {body}\n");
        assert_eq!(
            returned("x.py", &text, &contract).unwrap(),
            [expected],
            "{text}"
        );
    }
    for (body, why) in [
        // Python raises: `y` is the comprehension's own, and not bound yet.
        (
            "y = [1]\n    return [y for x in [1] for y in y]",
            "unmodeled name `y`",
        ),
        // Python reads the last item: every function shares the one variable.
        (
            "fs = [lambda: x for x in [1, 2]]\n    return fs[0]()",
            "means another variable",
        ),
    ] {
        let text = format!("def f(r, xs):\n    {body}\n");
        let refused = returned("x.py", &text, &contract).unwrap_err();
        assert!(refused.contains(why), "{text}: {refused}");
    }
}

#[test]
fn a_comprehension_is_refused_where_its_collection_cannot_be_read() {
    let contract = comprehension_contract();
    for (body, why) in [
        ("return [record(x) for x in xs]", "only some inputs reach"),
        (
            "return [record(x) for x in [1, 2] if r.ok]",
            "only some inputs reach",
        ),
        ("return {str(x): 1 for x in xs}", "only some inputs reach"),
        (
            "k = r.ok\n    return {k: 1 for x in [1]}",
            "each key is a constant text",
        ),
        (
            "return [x for x in r]",
            "a comprehension runs over a value whose items are not known",
        ),
        (
            "return total(record(x) for x in [1])",
            "a generator expression that calls a function",
        ),
        ("return total(x.a for x in [None])", "cannot raise"),
        (
            "g = lambda values: 1\n    return g(x for x in [1])",
            "a generator expression passed to a function",
        ),
        (
            "return len(x for x in [1])",
            "`len` of a generator expression",
        ),
        ("return [x async for x in xs]", "unsupported for_in_clause"),
        ("return [x for x in 1, 2]", "unsupported"),
        (
            "g = (x for x in xs)\n    return 1",
            "unsupported generator_expression",
        ),
    ] {
        let text = format!("def f(r, xs):\n    {body}\n");
        let refused = returned("x.py", &text, &contract).unwrap_err();
        assert!(refused.contains(why), "{text}: {refused}");
    }
}

#[test]
#[ignore = "requires native Z3; set LEMMA_SMT_SOLVER and run with --ignored"]
fn comprehensions_over_an_input_are_proved_for_every_length_it_may_have() {
    let contract = |condition: &str| {
        json!({
            "inputs": {"xs": {"sequence": "Int", "max_length": 3}},
            "parameters": ["xs"],
            "models": {"total": {"assumption": "total counts its values", "parameters": ["values"], "returns": {"expr": "values.length"}}},
            "ensures": [{"name": "holds", "condition": condition}],
        })
    };
    for (body, condition, mutation) in [
        (
            "return [x + 1 for x in xs]",
            "result.length == xs.length && (xs.length < 3 || result[2] == xs[2] + 1)",
            ("x + 1", "x + 2"),
        ),
        (
            "return len([x for x in xs if x > 0])",
            "result <= xs.length && (xs.length != 1 || xs[0] != 0 || result == 0)",
            ("x > 0", "x >= 0"),
        ),
        (
            "return total(x for x in xs if x != 0)",
            "xs.length != 2 || xs[0] == 0 || xs[1] == 0 || result == 2",
            ("x != 0", "x == 0"),
        ),
        (
            "return len([a + b for a in xs for b in xs])",
            "(xs.length != 0 || result == 0) && (xs.length != 2 || result == 4) && (xs.length != 3 || result == 9)",
            ("for b in xs", "for b in [1]"),
        ),
    ] {
        let text = format!("def f(xs):\n    {body}\n");
        // A call model's assumption makes the outcome conditional.
        let result = check_source("x.py", &text, "f", &contract(condition));
        assert_eq!(result["observed"]["holds"], true, "{text}: {result}");
        let mutated = check_source(
            "x.py",
            &text.replace(mutation.0, mutation.1),
            "f",
            &contract(condition),
        );
        assert_eq!(mutated["observed"]["holds"], false, "{text}: {mutated}");
    }
}

#[test]
fn an_entry_of_a_dict_is_not_an_attribute_and_an_inherited_property_is_not_undefined() {
    let contract = scope_contract();
    for (file, body, expected) in [
        (
            "x.py",
            "def f(r):\n    d = {\"a\": 1}\n    d[\"b\"] = 2\n    d[\"b\"] += 1\n    return d[\"a\"] + d[\"b\"]\n",
            "4",
        ),
        ("x.py", "def f(r):\n    return r[\"ok\"]\n", "input_0"),
        (
            "x.ts",
            "function f(r) {\n  const o = {a: 1};\n  o.toString = 2;\n  return o.toString + o[\"a\"];\n}\n",
            "3",
        ),
    ] {
        assert_eq!(
            returned(file, body, &contract).unwrap(),
            [expected],
            "{body}"
        );
    }
    for (file, body, why) in [
        // Python returns 2 here: a dict has an attribute `items`.
        (
            "x.py",
            "def f(r):\n    d = {\"a\": 1}\n    x = d.items\n    return 2\n",
            "reads an attribute of a dict",
        ),
        // Python raises in these two: an entry is not an attribute.
        (
            "x.py",
            "def f(r):\n    d = {\"a\": 1}\n    return d.a\n",
            "reads an attribute of a dict",
        ),
        (
            "x.py",
            "def f(r):\n    d = {\"a\": 1}\n    d.b = 2\n    return d[\"b\"]\n",
            "assigning the attribute `b` of `d`",
        ),
        // JavaScript reads the functions every object inherits.
        (
            "x.ts",
            "function f(r) {\n  const o = {a: 1};\n  return o.toString === undefined;\n}\n",
            "every JavaScript object inherits",
        ),
        (
            "x.ts",
            "function f(r) {\n  const o = {a: 1};\n  return o[\"constructor\"] === undefined;\n}\n",
            "every JavaScript object inherits",
        ),
    ] {
        let refused = returned(file, body, &contract).unwrap_err();
        assert!(refused.contains(why), "{body}: {refused}");
    }
}

fn key_contract() -> Json {
    json!({
        "inputs": {"r": {"ok": "Bool"}, "k": "Text", "i": "Int"},
        "parameters": ["r", "k", "i"],
        "ensures": [{"name": "any", "condition": "true"}],
    })
}

#[test]
fn a_computed_key_reads_whichever_entry_or_item_it_names() {
    let contract = key_contract();
    for (file, text, expected) in [
        // A constant the code computed is a constant key.
        (
            "x.py",
            "def f(r, k, i):\n    d = {\"a\": 1, \"b\": 2}\n    name = \"b\"\n    return d[name]\n",
            "2",
        ),
        (
            "x.py",
            "def f(r, k, i):\n    xs = [5, 6, 7]\n    at = 1\n    return xs[at + 1]\n",
            "7",
        ),
        (
            "x.ts",
            "function f(r, k, i) {\n  const xs = [5, 6];\n  const at = 1;\n  return xs[at];\n}\n",
            "6",
        ),
        (
            "x.go",
            "package p\nfunc f(r R, k string, i int) int {\n  xs := []int{5, 6}\n  at := 1\n  return xs[at]\n}\n",
            "6",
        ),
    ] {
        assert_eq!(
            returned(file, text, &contract).unwrap(),
            [expected],
            "{text}"
        );
    }
}

#[test]
fn a_computed_key_is_refused_where_nothing_gives_what_it_reads() {
    let contract = key_contract();
    for (file, text, why) in [
        (
            "x.py",
            "def f(r, k, i):\n    return r[k]\n",
            "may name a field the contract never declared",
        ),
        (
            "x.ts",
            "function f(r, k, i) {\n  return r[k];\n}\n",
            "may name a field the contract never declared",
        ),
        (
            "x.py",
            "def f(r, k, i):\n    d = {\"a\": 1}\n    return d[r.ok]\n",
            "a Boolean key differs between languages",
        ),
        (
            "x.py",
            "def f(r, k, i):\n    xs = [1]\n    return xs[k]\n",
            "only an object has entries under text keys",
        ),
        (
            "x.ts",
            "function f(r, k, i) {\n  const o = {a: 1};\n  return o[i];\n}\n",
            "only a sequence has elements at positions",
        ),
        (
            "x.py",
            "def f(r, k, i):\n    d = {\"a\": 1}\n    return d[k, k]\n",
            "unsupported subscript",
        ),
        (
            "x.ts",
            "function f(r, k, i) {\n  const o = {a: 1};\n  return o?.[k];\n}\n",
            "unsupported",
        ),
    ] {
        let refused = returned(file, text, &contract).unwrap_err();
        assert!(refused.contains(why), "{text}: {refused}");
    }
}

#[test]
#[ignore = "requires native Z3; set LEMMA_SMT_SOLVER and run with --ignored"]
fn a_key_that_depends_on_an_input_is_proved_for_every_entry_it_may_name() {
    let contract = |condition: &str| {
        json!({
            "inputs": {"k": "Text", "i": "Int"},
            "parameters": ["k", "i"],
            "ensures": [{"name": "holds", "condition": condition}],
        })
    };
    for (file, text, condition, mutation) in [
        // Python raises for a key the dict lacks, so every return read one.
        (
            "x.py",
            "def f(k, i):\n    d = {\"a\": 1, \"b\": 2}\n    return d[k]\n",
            "(k == \"a\" && result == 1) || (k == \"b\" && result == 2)",
            ("\"b\": 2", "\"b\": 3"),
        ),
        // Python counts a negative position from the end.
        (
            "x.py",
            "def f(k, i):\n    xs = [5, 6, 7]\n    return xs[i]\n",
            "(i == 0 - 1 && result == 7) || (i == 0 - 3 && result == 5) || (i >= 0 && i <= 2 && result == i + 5) || (i == 0 - 2 && result == 6)",
            ("7]", "8]"),
        ),
        // Go panics outside the slice.
        (
            "x.go",
            "package p\nfunc f(k string, i int) int {\n  xs := []int{5, 6, 7}\n  return xs[i]\n}\n",
            "i >= 0 && i <= 2 && result == i + 5",
            ("7}", "8}"),
        ),
        // JavaScript reads undefined outside the array, and for another key.
        (
            "x.ts",
            "function f(k, i) {\n  const xs = [5, 6, 7];\n  const x = xs[i];\n  if (x === undefined) {\n    return 0 - 1;\n  }\n  return x;\n}\n",
            "(i >= 0 && i <= 2 && result == i + 5) || ((i < 0 || i > 2) && result == 0 - 1)",
            ("7]", "8]"),
        ),
        (
            "x.ts",
            "function f(k, i) {\n  const o = {a: 1, b: 2};\n  const x = o[k];\n  if (x === undefined) {\n    return 0;\n  }\n  return x;\n}\n",
            "(k == \"a\" && result == 1) || (k == \"b\" && result == 2) || (k != \"a\" && k != \"b\" && result == 0)",
            ("b: 2", "b: 3"),
        ),
    ] {
        let result = check_source(file, text, "f", &contract(condition));
        assert_eq!(result["observed"]["holds"], true, "{text}: {result}");
        let mutated = check_source(
            file,
            &text.replace(mutation.0, mutation.1),
            "f",
            &contract(condition),
        );
        assert_eq!(mutated["observed"]["holds"], false, "{text}: {mutated}");
    }
    // A computed key of a JavaScript object is read under an assumption.
    let text = "function f(k, i) {\n  const o = {a: 1};\n  return o[k] === undefined;\n}\n";
    let result = check_source("x.ts", text, "f", &contract("result == (k != \"a\")"));
    assert_eq!(result["observed"]["status"], "conditional", "{result}");
}
