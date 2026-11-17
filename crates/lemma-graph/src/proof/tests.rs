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
fn pure_implementations_are_proved_without_dependency_assumptions() {
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
