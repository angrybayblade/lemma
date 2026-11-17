//! Source-backed contracts, shared by TypeScript, Python and Go.
//!
//! Only the explicitly supported immutable-value subset is interpreted.
//! Unsupported code and failed solver obligations produce unavailable evidence.

mod contract;
mod ir;
mod solver;
mod source;
mod symbolic;

use std::path::{Component, Path};

use serde_json::{Value as Json, json};

use crate::repos::Environment;
use contract::Contract;
use solver::{Answer, Solver};
use symbolic::Term;

pub(crate) fn identity(environment: Environment<'_>) -> Option<String> {
    solver::identity(environment)
}

/// Produce the existing checker protocol, including on an incomplete check.
pub(crate) fn run(
    given: &Json,
    directory: &Path,
    environment: Environment<'_>,
    timeout_ms: u64,
) -> Json {
    match verify(given, directory, environment, timeout_ms) {
        Ok(observed) => json!({"observed": observed}),
        Err(reason) => json!({"unavailable": format!("proof unavailable: {reason}")}),
    }
}

fn verify(
    given: &Json,
    directory: &Path,
    environment: Environment<'_>,
    timeout_ms: u64,
) -> Result<Json, String> {
    let toolchain = identity(environment);
    if given["engine_key"]
        .as_str()
        .is_some_and(|expected| Some(expected) != toolchain.as_deref())
    {
        return Err("proof toolchain changed while evidence was being prepared".to_owned());
    }
    let arguments = &given["arguments"];
    let handler = &arguments["handler"];
    let contract: Contract = serde_json::from_value(arguments["contract"].clone())
        .map_err(|e| format!("invalid proof contract: {e}"))?;
    contract.validate()?;
    let (source, function) = read_handler(given, handler)?;
    let execution = symbolic::execute(&function, &contract)?;
    let mut solver = Solver {
        directory,
        environment,
        symbols: &execution.symbols,
        deadline: Solver::deadline(timeout_ms)?,
        queries: 0,
    };
    if let Answer::Unsat = solver.query("preconditions", &execution.requires)? {
        return Err("preconditions are inconsistent; refusing a vacuous proof".to_owned());
    }
    let reachable = disjunction(execution.paths.iter().map(|path| path.guard.clone()));
    if let Answer::Unsat = solver.query("returns", &reachable)? {
        return Err("no reachable normal return; refusing a vacuous proof".to_owned());
    }
    let (holds, assertions) = check_postconditions(&execution, &contract, &mut solver)?;
    if toolchain != identity(environment) {
        return Err("proof toolchain changed during verification".to_owned());
    }
    let status = if !holds {
        "violated"
    } else if execution.assumptions.is_empty() {
        "proved"
    } else {
        "conditional"
    };
    Ok(json!({
        "status": status,
        "holds": holds,
        "assumptions": execution.assumptions,
        "properties": assertions,
        "source": source,
        "contract_hash": lemma_model::content_hash(arguments["contract"].to_string().as_bytes()),
        "toolchain": toolchain,
        "symbols": execution.symbols.iter().map(|(path, (symbol, sort))| (path.clone(), json!({"symbol": symbol, "sort": sort.smt()}))).collect::<serde_json::Map<_, _>>(),
        "coverage": {
            "semantics": "immutable Bool/Text records; exact integer constants; every normal return",
            "paths": execution.paths.len(),
            "queries": solver.queries,
            "excludes": ["exceptions", "termination", "heap mutation", "concurrency", "dependency implementations", "language typechecking"],
        },
        "artifacts": directory.display().to_string(),
    }))
}

/// Bind evidence directly to source bytes, including Git-ignored source files.
pub(crate) fn bind_source(root: &Path, handler: &mut Json) -> Result<(), String> {
    let file = handler["implementation"]["file"]
        .as_str()
        .ok_or("handler needs an implementation file")?;
    let text = std::fs::read(inside(root, file)?).map_err(|error| error.to_string())?;
    handler["implementation"]["source_hash"] = json!(lemma_model::content_hash(&text));
    Ok(())
}

fn read_handler(given: &Json, handler: &Json) -> Result<(Json, ir::Function), String> {
    let repository = handler["repository"]
        .as_str()
        .ok_or("handler must be an Operation")?;
    let root = given["roots"][repository]
        .as_str()
        .ok_or("handler repository has no selected checkout")?;
    let file = handler["implementation"]["file"]
        .as_str()
        .ok_or("handler needs a file-bound implementation")?;
    let symbol = handler["implementation"]["symbol"]
        .as_str()
        .ok_or("handler needs a symbol-bound implementation")?;
    let path = inside(Path::new(root), file)?;
    let text = std::fs::read_to_string(&path).map_err(|e| format!("{}: {e}", path.display()))?;
    if let Some(expected) = handler["implementation"]["source_hash"].as_str() {
        if expected != lemma_model::content_hash(text.as_bytes()) {
            return Err("implementation changed while its proof was being prepared".to_owned());
        }
    }
    let (language, function) = source::read(file, &text, symbol)?;
    Ok((
        json!({"repository": repository, "file": file, "symbol": symbol, "hash": lemma_model::content_hash(text.as_bytes()), "language": language.name()}),
        function,
    ))
}

fn check_postconditions(
    execution: &symbolic::Execution,
    contract: &Contract,
    solver: &mut Solver<'_>,
) -> Result<(bool, Vec<Json>), String> {
    let mut assertions = Vec::new();
    let mut holds = true;
    for (index, assertion) in contract.ensures.iter().enumerate() {
        let mut witnesses = Vec::new();
        for (path_index, path) in execution.paths.iter().enumerate() {
            let condition = execution.postcondition(path, &assertion.condition, contract)?;
            let violation = path.guard.and(&condition.not());
            if let Answer::Sat(model) =
                solver.query(&format!("post-{index}-path-{path_index}"), &violation)?
            {
                witnesses.push(json!({"branches": path.branches, "model": model}));
            }
        }
        let satisfied = witnesses.is_empty();
        holds &= satisfied;
        assertions.push(
            json!({"name": assertion.name, "holds": satisfied, "counterexamples": witnesses}),
        );
    }
    Ok((holds, assertions))
}

fn disjunction(terms: impl Iterator<Item = Term>) -> Term {
    terms.fold(Term::boolean(false), |left, right| {
        left.not().and(&right.not()).not()
    })
}

fn inside(root: &Path, file: &str) -> Result<std::path::PathBuf, String> {
    if Path::new(file)
        .components()
        .any(|c| !matches!(c, Component::Normal(_) | Component::CurDir))
    {
        return Err("implementation path leaves its repository".to_owned());
    }
    let root = root.canonicalize().map_err(|e| e.to_string())?;
    let path = root.join(file).canonicalize().map_err(|e| e.to_string())?;
    if !path.starts_with(root) {
        return Err("implementation symlink leaves its repository".to_owned());
    }
    Ok(path)
}

#[cfg(test)]
mod corpus_tests;
#[cfg(test)]
mod tests;
