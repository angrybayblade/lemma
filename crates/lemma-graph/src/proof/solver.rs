//! SMT-LIB artifacts and a bounded invocation of the external Z3 executable.

use std::collections::BTreeMap;
use std::fmt::Write as _;
use std::path::{Path, PathBuf};
use std::sync::{Mutex, PoisonError};
use std::time::{Duration, Instant, SystemTime};

use crate::repos::{self, Environment};

use super::symbolic::{Sort, Term};

pub(super) struct Solver<'a> {
    pub directory: &'a Path,
    pub environment: Environment<'a>,
    pub symbols: &'a BTreeMap<String, (String, Sort)>,
    pub deadline: Instant,
    pub queries: usize,
}

pub(super) enum Answer {
    Unsat,
    Sat(String),
}

fn requested_executable(environment: Environment<'_>) -> String {
    environment
        .added
        .get("LEMMA_SMT_SOLVER")
        .cloned()
        .or_else(|| environment.inherited("LEMMA_SMT_SOLVER"))
        .unwrap_or_else(|| "z3".to_owned())
}

fn executable(environment: Environment<'_>) -> Result<PathBuf, String> {
    let program = requested_executable(environment);
    if program.contains(std::path::MAIN_SEPARATOR) {
        let path = PathBuf::from(&program);
        return if path.is_absolute() {
            Ok(path)
        } else {
            Err("LEMMA_SMT_SOLVER must be absolute, or a name on PATH".to_owned())
        };
    }
    let path = environment
        .added
        .get("PATH")
        .cloned()
        .or_else(|| environment.inherited("PATH"))
        .unwrap_or_default();
    std::env::split_paths(&path)
        .filter(|part| part.is_absolute())
        .map(|part| part.join(&program))
        .find(|path| path.is_file())
        .ok_or_else(|| {
            format!("SMT solver failed: `{program}` was not found on the absolute entries of PATH")
        })
}

// A daemon can keep running while its executable is replaced on disk. Include
// the compiled semantics too, so it cannot impersonate the replacement build.
const BUILD_INPUTS: &[&str] = &[
    include_str!("contract.rs"),
    include_str!("ir.rs"),
    include_str!("mod.rs"),
    include_str!("solver.rs"),
    include_str!("source/expressions.rs"),
    include_str!("source/lookup.rs"),
    include_str!("source/mod.rs"),
    include_str!("source/statements.rs"),
    include_str!("source/strings.rs"),
    include_str!("symbolic/exceptions.rs"),
    include_str!("symbolic/mod.rs"),
    include_str!("symbolic/models.rs"),
    include_str!("symbolic/value.rs"),
    include_str!("../behaviors/mod.rs"),
    include_str!("../behaviors/evidence.rs"),
    include_str!("../behaviors/encode.rs"),
    include_str!("../../../lemma-lang/src/syntax/ast.rs"),
    include_str!("../../../lemma-lang/src/syntax/lexer.rs"),
    include_str!("../../../lemma-lang/src/syntax/parser/mod.rs"),
    include_str!("../../../lemma-lang/src/syntax/parser/expressions.rs"),
    include_str!("../../../lemma-lang/src/syntax/parser/items.rs"),
    include_str!("../../../lemma-lang/src/syntax/parser/statements.rs"),
    include_str!("../../../lemma-lang/src/syntax/parser/rules.rs"),
    include_str!("../../../../Cargo.lock"),
];

/// Include both the running verifier and the selected solver in evidence keys.
/// Missing identities disable reuse rather than falling back to a weaker key.
pub(super) fn identity(environment: Environment<'_>, digests: &Digests) -> Option<String> {
    let verifier = digests.of(&std::env::current_exe().ok()?)?;
    let solver = digests.of(&executable(environment).ok()?)?;
    Some(lemma_model::content_hash(
        format!(
            "{verifier}:{solver}:{}",
            BUILD_INPUTS
                .iter()
                .map(|source| lemma_model::content_hash(source.as_bytes()))
                .collect::<Vec<_>>()
                .join(":")
        )
        .as_bytes(),
    ))
}

/// Digests of executables, kept while their size and modification time stay
/// the same: the verifier and solver are large, and a check asks for their
/// identity once per case.
#[derive(Debug, Default)]
pub(crate) struct Digests {
    known: Mutex<BTreeMap<PathBuf, (u64, SystemTime, String)>>,
}

impl Digests {
    fn of(&self, path: &Path) -> Option<String> {
        let metadata = std::fs::metadata(path).ok()?;
        let stamp = (metadata.len(), metadata.modified().ok()?);
        let mut known = self.known.lock().unwrap_or_else(PoisonError::into_inner);
        if let Some((size_bytes, modified, digest)) = known.get(path) {
            if (*size_bytes, *modified) == stamp {
                return Some(digest.clone());
            }
        }
        let digest = lemma_model::content_hash(&std::fs::read(path).ok()?);
        known.insert(path.to_path_buf(), (stamp.0, stamp.1, digest.clone()));
        Some(digest)
    }
}

impl Solver<'_> {
    pub(super) fn query(&mut self, label: &str, formula: &Term) -> Result<Answer, String> {
        let stem = format!("{:04}-{label}", self.queries);
        self.queries += 1;
        let input = self.directory.join(format!("{stem}.smt2"));
        let output = self.directory.join(format!("{stem}.stdout"));
        let mut script = String::from("(set-logic QF_SLIA)\n(set-option :produce-models true)\n");
        for (symbol, sort) in self.symbols.values() {
            let _ = writeln!(script, "(declare-fun {symbol} () {})", sort.smt());
        }
        let _ = writeln!(script, "(assert {})\n(check-sat)", formula.code);
        std::fs::write(&input, &script).map_err(|e| e.to_string())?;
        let answer = self.run(&input)?;
        std::fs::write(&output, &answer).map_err(|e| e.to_string())?;
        if !matches!(answer.trim(), "sat" | "unsat" | "unknown") {
            return Err(format!("invalid solver check-sat output: {answer}"));
        }
        match answer_of(&answer)? {
            "unsat" => Ok(Answer::Unsat),
            "sat" => {
                // Retain a separately replayable query that includes the model.
                script.push_str("(get-model)\n");
                let model_input = self.directory.join(format!("{stem}-witness.smt2"));
                std::fs::write(&model_input, script).map_err(|e| e.to_string())?;
                let model = self.run(&model_input)?;
                std::fs::write(
                    self.directory.join(format!("{stem}-witness.stdout")),
                    &model,
                )
                .map_err(|e| e.to_string())?;
                if answer_of(&model)? != "sat" {
                    return Err("solver changed its answer while requesting a witness".to_owned());
                }
                if !model
                    .lines()
                    .skip(1)
                    .any(|line| line.trim_start().starts_with('('))
                {
                    return Err("solver supplied no witness model".to_owned());
                }
                Ok(Answer::Sat(model))
            }
            _ => Err("solver returned unknown".to_owned()),
        }
    }

    fn run(&self, input: &Path) -> Result<String, String> {
        let remaining = self
            .deadline
            .checked_duration_since(Instant::now())
            .ok_or("proof timeout exceeded")?;
        let timeout = u64::try_from(remaining.as_millis())
            .unwrap_or(u64::MAX)
            .max(1);
        repos::run_program(
            self.directory,
            &[
                executable(self.environment)?.display().to_string(),
                "-smt2".to_owned(),
                input.display().to_string(),
            ],
            self.environment,
            Some(timeout),
        )
        .map_err(|error| format!("SMT solver failed: {error}"))
    }

    pub(super) fn deadline(timeout_ms: u64) -> Result<Instant, String> {
        Instant::now()
            .checked_add(Duration::from_millis(timeout_ms))
            .ok_or_else(|| "proof timeout is too large".to_owned())
    }
}

fn answer_of(output: &str) -> Result<&str, String> {
    if output.contains("(error") {
        return Err(format!("solver reported an error: {output}"));
    }
    match output
        .lines()
        .find(|line| !line.trim().is_empty())
        .map(str::trim)
    {
        Some(answer @ ("sat" | "unsat" | "unknown")) => Ok(answer),
        _ => Err(format!("invalid solver output: {output}")),
    }
}

#[cfg(all(test, unix))]
mod tests {
    #![allow(clippy::unwrap_used, reason = "test setup should fail loudly")]

    use std::os::unix::fs::PermissionsExt;

    use super::*;

    #[test]
    fn unknown_malformed_and_error_answers_never_establish_a_proof() {
        for output in [
            "unknown",
            "not-a-verdict",
            "unsat\\nsat",
            "(error solver-failed)",
        ] {
            let workspace = tempfile::tempdir().unwrap();
            let program = workspace.path().join("solver");
            std::fs::write(&program, format!("#!/bin/sh\nprintf '%b\\n' '{output}'\n")).unwrap();
            std::fs::set_permissions(&program, std::fs::Permissions::from_mode(0o755)).unwrap();
            let environment =
                BTreeMap::from([("LEMMA_SMT_SOLVER".to_owned(), program.display().to_string())]);
            let mut solver = Solver {
                directory: workspace.path(),
                environment: Environment {
                    base: None,
                    added: &environment,
                },
                symbols: &BTreeMap::new(),
                deadline: Solver::deadline(1000).unwrap(),
                queries: 0,
            };
            assert!(
                solver.query("failure", &Term::boolean(true)).is_err(),
                "{output}"
            );
            assert!(workspace.path().join("0000-failure.smt2").is_file());
        }
    }

    #[test]
    fn replacing_a_solver_at_the_same_path_changes_its_identity() {
        let workspace = tempfile::tempdir().unwrap();
        let program = workspace.path().join("solver");
        let added =
            BTreeMap::from([("LEMMA_SMT_SOLVER".to_owned(), program.display().to_string())]);
        let environment = Environment {
            base: None,
            added: &added,
        };
        std::fs::write(&program, "first solver").unwrap();
        let before = identity(environment, &Digests::default()).unwrap();
        std::fs::write(&program, "different solver").unwrap();
        assert_ne!(before, identity(environment, &Digests::default()).unwrap());
    }
}
