//! Verifying a change: facts from every repository in a manifest, a
//! grammar's rules and requirements over them, and findings with
//! explanations.
//!
//! Engines are a type parameter, so callers pick one at compile time (see
//! `lemma_logic::Engine`).

mod clients;
mod demand;
mod extract;
mod manifest;
mod system;
mod terms;

use std::collections::{BTreeMap, BTreeSet};
use std::fmt::Write as _;
use std::path::PathBuf;
use std::sync::Arc;

use lemma_grammar::{CompiledGrammar, Severity};
use lemma_logic::{Derivation, Engine, EvaluationError, Fact, Request, Symbols, Value};

pub use extract::operation_key;
pub use manifest::{Change, GeneratedDocument, Manifest, Repository, Role};

/// The grammar used when a manifest names none.
pub const HTTP_CONTRACTS: &str = include_str!("../../../grammars/http-contracts.yaml");

/// Why a change could not be verified.
#[derive(Debug, thiserror::Error)]
pub enum VerifyError {
    /// A file could not be read.
    #[error("could not read {}", path.display())]
    Read {
        /// The file.
        path: PathBuf,
        /// Why.
        #[source]
        source: std::io::Error,
    },
    /// The manifest is malformed.
    #[error("invalid manifest {}", path.display())]
    Manifest {
        /// The manifest.
        path: PathBuf,
        /// Why.
        #[source]
        source: yaml_serde::Error,
    },
    /// The change names a repository the manifest does not list.
    #[error("the change names repository `{name}`, which the manifest does not list")]
    UnknownRepository {
        /// The name.
        name: String,
    },
    /// The grammar does not compile.
    #[error("invalid grammar")]
    Grammar(#[from] lemma_grammar::GrammarError),
    /// Source files could not be read from a repository.
    #[error(transparent)]
    Source(#[from] lemma_http::SourceError),
    /// A spec or operation table the manifest names is not in the repository
    /// at any commit it is read at.
    #[error("{repository} has no file {file} at {commits}")]
    MissingFile {
        /// The repository's name.
        repository: String,
        /// The file.
        file: String,
        /// The commits it was looked for at.
        commits: String,
    },
    /// A source file could not be parsed.
    #[error(transparent)]
    Code(#[from] lemma_http::CodeError),
    /// The definition extractor could not be set up.
    #[error("could not set up the syntax extractor")]
    Syntax {
        /// Why.
        #[source]
        source: lemma_syntax::SyntaxError,
    },
    /// A spec is not valid JSON.
    #[error("{file} is not valid JSON")]
    Spec {
        /// The spec.
        file: String,
        /// Why.
        #[source]
        source: serde_json::Error,
    },
    /// A YAML spec is not valid YAML.
    #[error("{file} is not valid YAML")]
    YamlSpec {
        /// The spec.
        file: String,
        /// Why.
        #[source]
        source: yaml_serde::Error,
    },
    /// A document an adapter generated could not be read.
    #[error("could not read the document generated at {file}")]
    Generated {
        /// The document.
        file: String,
        /// Why.
        #[source]
        source: std::io::Error,
    },
    /// A relation was asked for that the grammar does not declare or derive.
    #[error("the grammar has no relation `{name}`")]
    UnknownRelation {
        /// The name.
        name: String,
    },
    /// The engine could not evaluate the grammar.
    #[error("evaluation failed: {message}")]
    Evaluation {
        /// What went wrong, with its causes.
        message: String,
    },
}

/// One requirement match.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize)]
pub struct Finding {
    /// The requirement's name.
    pub requirement: String,
    /// How serious it is.
    pub severity: &'static str,
    /// The requirement's message with values filled in.
    pub message: String,
    /// The value of each variable.
    pub values: BTreeMap<String, String>,
    /// Whether the change brought it about. A requirement comparing the
    /// snapshots only holds for what changed; one over a single snapshot,
    /// such as a route its spec does not declare, may already have held at
    /// the base, and then it was not introduced.
    pub introduced: bool,
    /// Why it holds, as an indented proof, when explanations were asked for.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub derivation: Option<String>,
}

/// The result of verifying a change.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize)]
pub struct Report {
    /// The change.
    pub change: String,
    /// The engine that evaluated the rules.
    pub engine: String,
    /// How much consumer code was read: `full`, `demand`, or `full: <why>`
    /// when demand was asked for and the grammar does not allow it.
    pub evaluation: String,
    /// Facts extracted, by relation.
    pub facts: BTreeMap<String, usize>,
    /// Findings, most severe first, then by requirement and message.
    pub findings: Vec<Finding>,
    /// The rows of each relation asked for in [`Options::relations`], sorted.
    #[serde(skip_serializing_if = "BTreeMap::is_empty")]
    pub relations: BTreeMap<String, Vec<Vec<String>>>,
}

/// What to include in a report beyond its findings.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Options {
    /// Attach each finding's derivation.
    pub explain: bool,
    /// Relations to report in full, such as `calls` (derived) or `route`
    /// (read from code), for inspecting or measuring what the rules saw and
    /// concluded.
    pub relations: Vec<String>,
    /// Where the facts read from code are kept between runs; `None` keeps
    /// nothing.
    pub facts_cache: Option<FactsCache>,
    /// How much consumer code to read.
    pub evaluation: Evaluation,
}

/// How much consumer code a verification reads.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum Evaluation {
    /// Every consumer file.
    #[default]
    Full,
    /// Only the consumer files a finding can rest on: those that may call
    /// an operation the change touches, the files they import, and the
    /// files the change edits. A grammar whose requirements are not bounded
    /// by the change is evaluated in full, and the report says why.
    /// Relations asked for are complete only for those files.
    Demand,
}

/// Facts read from code, kept between runs by what they were read from.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FactsCache {
    /// The directory they are kept in.
    pub directory: PathBuf,
    /// The build of Lemma that reads them, such as a digest of its
    /// executable: facts one build read are never given to another, whose
    /// extraction may differ.
    pub build: String,
}

/// Verifies a manifest's change with `engine`.
pub fn verify<E: Engine>(
    manifest: &Manifest,
    engine: &E,
    options: &Options,
) -> Result<Report, VerifyError> {
    verify_with_listings(manifest, engine, options, &Arc::default())
}

/// [`verify`], listing repositories through `listings`, which a caller
/// reading the same repositories again in one run can share.
pub fn verify_with_listings<E: Engine>(
    manifest: &Manifest,
    engine: &E,
    options: &Options,
    listings: &Arc<lemma_http::Listings>,
) -> Result<Report, VerifyError> {
    let grammar_text = match &manifest.grammar {
        Some(path) => std::fs::read_to_string(path).map_err(|source| VerifyError::Read {
            path: path.clone(),
            source,
        })?,
        None => HTTP_CONTRACTS.to_owned(),
    };
    let mut grammar = lemma_grammar::compile(&lemma_grammar::parse_grammar(&grammar_text)?)?;
    check_relations(&grammar, &options.relations)?;
    let plan = match options.evaluation {
        Evaluation::Full => None,
        Evaluation::Demand => Some(demand::plan(&grammar)),
    };
    let evaluation = match &plan {
        None => "full".to_owned(),
        Some(Ok(_)) => "demand".to_owned(),
        Some(Err(reason)) => format!("full: {reason}"),
    };
    let demand = match &plan {
        Some(Ok(plan)) => Some((plan, engine)),
        _ => None,
    };
    let mut writer = extract::FactWriter::new(&mut grammar);
    extract::extract_all(
        manifest,
        &mut writer,
        options.facts_cache.as_ref(),
        listings,
        demand,
    )?;
    let facts = writer.counts;
    let derived = evaluate(&grammar, engine, &wanted(&grammar, &options.relations))?;
    let mut findings = findings(&grammar, &derived);
    let relations = relation_rows(&grammar, &derived, &options.relations);
    if options.explain {
        add_derivations(&grammar, engine, &mut findings)?;
    }
    let change = &manifest.change;
    Ok(Report {
        change: format!(
            "{} {}..{}",
            change.repository,
            change.base,
            change.head_label()
        ),
        engine: engine.name().to_owned(),
        evaluation,
        facts,
        findings: findings.into_iter().map(|(finding, _)| finding).collect(),
        relations,
    })
}

fn check_relations(grammar: &CompiledGrammar, names: &[String]) -> Result<(), VerifyError> {
    for name in names {
        if grammar.program.relation(name).is_none() {
            return Err(VerifyError::UnknownRelation { name: name.clone() });
        }
    }
    Ok(())
}

/// What a run reads of the derived relations: its requirements' findings
/// and the relations asked for. Asking for nothing else keeps an engine
/// from writing out every intermediate relation, some of millions of rows.
fn wanted(grammar: &CompiledGrammar, relations: &[String]) -> Request {
    let outputs = grammar
        .requirements
        .iter()
        .flat_map(|requirement| {
            std::iter::once(requirement.relation.clone()).chain(requirement.base_relation.clone())
        })
        .chain(relations.iter().cloned())
        .collect();
    Request {
        outputs,
        ..Request::default()
    }
}

fn relation_rows(
    grammar: &CompiledGrammar,
    derived: &lemma_logic::Derived,
    names: &[String],
) -> BTreeMap<String, Vec<Vec<String>>> {
    let mut relations = BTreeMap::new();
    for name in names {
        // Input relations (facts read from code) are not among the derived
        // ones; their rows are the facts themselves.
        let mut rows: Vec<Vec<String>> = derived
            .rows(name)
            .chain(grammar.facts.rows(name))
            .map(|row| {
                row.iter()
                    .map(|value| grammar.symbols.display(*value))
                    .collect()
            })
            .collect();
        rows.sort();
        rows.dedup();
        relations.insert(name.clone(), rows);
    }
    relations
}

fn evaluate<E: Engine>(
    grammar: &CompiledGrammar,
    engine: &E,
    request: &Request,
) -> Result<lemma_logic::Derived, VerifyError> {
    lemma_logic::evaluate(engine, &grammar.program, &grammar.facts, request).map_err(|error| {
        VerifyError::Evaluation {
            message: describe(&error),
        }
    })
}

pub(crate) fn describe<E: std::error::Error + 'static>(error: &EvaluationError<E>) -> String {
    let mut message = error.to_string();
    let mut source = std::error::Error::source(error);
    while let Some(cause) = source {
        let _ = write!(message, ": {cause}");
        source = cause.source();
    }
    message
}

/// Findings with the fact each came from, sorted.
/// What makes a finding the same one at the base and the head: its text
/// values. Numbers are positions (a line moves when code above it changes),
/// so a route that only moved down its file is not new.
fn identity(row: &[Value]) -> Vec<Value> {
    row.iter()
        .filter(|value| !matches!(value, Value::Number(_)))
        .copied()
        .collect()
}

fn findings(grammar: &CompiledGrammar, derived: &lemma_logic::Derived) -> Vec<(Finding, Fact)> {
    let mut found = Vec::new();
    for requirement in &grammar.requirements {
        let at_base: BTreeSet<Vec<Value>> = requirement
            .base_relation
            .iter()
            .flat_map(|relation| derived.rows(relation))
            .map(|row| identity(row))
            .collect();
        for row in derived.rows(&requirement.relation) {
            let values: BTreeMap<String, String> = requirement
                .variables
                .iter()
                .zip(row.iter())
                .map(|(name, value)| (name.clone(), grammar.symbols.display(*value)))
                .collect();
            let template = requirement
                .explain
                .clone()
                .unwrap_or_else(|| requirement.name.clone());
            let finding = Finding {
                requirement: requirement.name.clone(),
                severity: severity_name(requirement.severity),
                message: fill(&template, &values),
                values,
                introduced: !at_base.contains(&identity(row)),
                derivation: None,
            };
            found.push((
                finding,
                Fact {
                    relation: requirement.relation.clone(),
                    row: row.clone(),
                },
            ));
        }
    }
    found.sort_by(|(left, _), (right, _)| {
        (
            severity_rank(left.severity),
            &left.requirement,
            &left.message,
        )
            .cmp(&(
                severity_rank(right.severity),
                &right.requirement,
                &right.message,
            ))
    });
    found
}

fn add_derivations<E: Engine>(
    grammar: &CompiledGrammar,
    engine: &E,
    findings: &mut [(Finding, Fact)],
) -> Result<(), VerifyError> {
    let request = Request {
        explain: findings.iter().map(|(_, fact)| fact.clone()).collect(),
        ..Request::default()
    };
    let derived = evaluate(grammar, engine, &request)?;
    let by_fact: BTreeMap<&Fact, &Derivation> = derived
        .derivations
        .iter()
        .map(|derivation| (derivation.fact(), derivation))
        .collect();
    for (finding, fact) in findings.iter_mut() {
        if let Some(derivation) = by_fact.get(fact) {
            let mut text = String::new();
            render(derivation, &grammar.symbols, 0, &mut text);
            finding.derivation = Some(text);
        }
    }
    Ok(())
}

/// A derivation as indented lines, one fact per line.
fn render(derivation: &Derivation, symbols: &Symbols, depth: usize, out: &mut String) {
    let indent = "  ".repeat(depth);
    let marker = if matches!(derivation, Derivation::Absent(_)) {
        "not "
    } else {
        ""
    };
    let fact = derivation.fact();
    let values: Vec<String> = fact
        .row
        .iter()
        .map(|value| symbols.display(*value))
        .collect();
    let _ = writeln!(
        out,
        "{indent}{marker}{}({})",
        fact.relation,
        values.join(", ")
    );
    if let Derivation::Derived { premises, .. } = derivation {
        for premise in premises {
            render(premise, symbols, depth + 1, out);
        }
    }
}

/// Replaces `{name}` with each variable's value.
fn fill(template: &str, values: &BTreeMap<String, String>) -> String {
    let mut message = template.to_owned();
    for (name, value) in values {
        message = message.replace(&format!("{{{name}}}"), value);
    }
    message
}

fn severity_name(severity: Severity) -> &'static str {
    match severity {
        Severity::Error => "error",
        Severity::Warning => "warning",
        Severity::Info => "info",
    }
}

fn severity_rank(name: &str) -> u8 {
    match name {
        "error" => 0,
        "warning" => 1,
        _ => 2,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_built_in_grammar_compiles() {
        let grammar =
            lemma_grammar::compile(&lemma_grammar::parse_grammar(HTTP_CONTRACTS).unwrap()).unwrap();

        assert!(
            grammar
                .requirements
                .iter()
                .any(|requirement| requirement.name == "client_declares_removed_parameter")
        );
        assert!(grammar.input_relation("operation", "base").is_some());
    }

    #[test]
    fn messages_fill_in_variables() {
        let values = BTreeMap::from([
            ("op".to_owned(), "GET /api/x".to_owned()),
            ("n".to_owned(), "search".to_owned()),
        ]);

        assert_eq!(fill("{op} lost `{n}`", &values), "GET /api/x lost `search`");
    }
}
