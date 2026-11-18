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
fn nested_functions_and_unknown_containers_are_refused() {
    let nested = "def outer(request):\n    def inner():\n        return request['allowed']\n    return inner()\n";
    let error = source::read("x.py", nested, "outer.inner").err().unwrap();
    assert!(error.contains("enclosing"), "{error}");
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
    let variable_key = "def f(r, key):\n    return r[key]\n";
    assert!(source::read("x.py", variable_key, "f").is_err());
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
