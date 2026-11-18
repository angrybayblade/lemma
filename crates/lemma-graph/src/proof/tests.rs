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
fn ambiguous_symbols_and_rebinding_are_refused() {
    assert!(
        source::read(
            "x.py",
            "def f(x):\n    return x\ndef f(y):\n    return y\n",
            "f"
        )
        .is_err()
    );
    let (_, function) = source::read(
        "x.py",
        "def f(request):\n    request = False\n    return request\n",
        "f",
    )
    .unwrap();
    assert!(symbolic::execute(&function, &contract(source::Language::Python)).is_err());
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
