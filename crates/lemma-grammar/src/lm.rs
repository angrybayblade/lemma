//! Lowering a checked `.lm` program into a grammar file, so `.lm` relations,
//! rules, graphs and requirements run on the same compiler and engines as
//! YAML grammars.
//!
//! Names stay as written where they are unique across the program, so the
//! standard library's `lemma.http` lowers to the relation and requirement
//! names extractors and `lemma check` already use. A name two modules both
//! declare is qualified with its module.

use std::collections::{BTreeMap, BTreeSet};
use std::fmt::Write as _;

use lemma_lang::model::{Literal, Model, Run, Severity as LmSeverity, Term};
use lemma_lang::syntax::ast::Snapshot;
use lemma_lang::types::{Address, Type, Value};

use lemma_logic::ColumnType;

use crate::{DeclaredFact, FactValue, GrammarFile, RelationSpec, RequirementSpec, Severity};

/// Why a checked program cannot be lowered into a grammar.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum LowerError {
    /// The program uses something the grammar compiler cannot express yet.
    #[error("{what} cannot run in `lemma check` yet")]
    Unsupported {
        /// What, in a phrase naming the declaration.
        what: String,
    },
}

/// The grammar a checked program's model describes. `name` becomes the
/// grammar's name; `check` selects requirements as a `check` declaration
/// does, every requirement when `None`.
pub fn grammar_file(
    model: &Model,
    name: &str,
    check: Option<&[Run]>,
) -> Result<GrammarFile, LowerError> {
    let names = Names::new(model);
    let mut file = GrammarFile {
        grammar: name.to_owned(),
        relations: Vec::new(),
        rules: Vec::new(),
        requirements: BTreeMap::new(),
        facts: Vec::new(),
    };
    for relation in &model.relations {
        let columns = relation
            .columns
            .iter()
            .map(|column| (column.name.as_str(), &column.column_type));
        file.relations
            .push((names.get(&relation.address), relation_spec(columns)));
    }
    for graph in &model.graphs {
        lower_graph(&mut file, &names, model, graph)?;
    }
    for rule in &model.rules {
        for clause in &rule.clauses {
            let head = terms_text(&clause.head, &rule.address)?;
            let body = body_text(&clause.body, &names, &rule.address)?;
            file.rules
                .push(format!("{}({head}) :- {body}.", names.get(&rule.address)));
        }
    }
    for requirement in &model.requirements {
        let Some(severity) = selected(check, &requirement.address, requirement.severity) else {
            continue;
        };
        let spec = RequirementSpec {
            when: body_text(&requirement.body, &names, &requirement.address)?,
            severity,
            explain: requirement.message.clone(),
        };
        file.requirements
            .insert(names.get(&requirement.address), spec);
    }
    Ok(file)
}

/// The severity a requirement runs with under `check`, or `None` when the
/// check does not select it.
fn selected(check: Option<&[Run]>, address: &Address, own: LmSeverity) -> Option<Severity> {
    let Some(runs) = check else {
        return Some(severity(own));
    };
    let run = runs.iter().find(|run| &run.requirement == address)?;
    Some(severity(run.severity.unwrap_or(own)))
}

fn severity(severity: LmSeverity) -> Severity {
    match severity {
        LmSeverity::Error => Severity::Error,
        LmSeverity::Warning => Severity::Warning,
        LmSeverity::Info => Severity::Info,
    }
}

fn relation_spec<'c>(columns: impl Iterator<Item = (&'c str, &'c Type)>) -> RelationSpec {
    let column = |(name, column_type): (&str, &Type)| {
        let kind = if *column_type == Type::Int {
            ColumnType::Number
        } else {
            ColumnType::Symbol
        };
        (name.to_owned(), kind)
    };
    RelationSpec {
        columns: columns.map(column).collect(),
        sources: ["syntax", "semantic", "spec", "declared", "observed"]
            .map(str::to_owned)
            .to_vec(),
        description: None,
    }
}

/// A graph's rows hold in its relation and in a relation of its own, which
/// rules naming the graph read.
fn lower_graph(
    file: &mut GrammarFile,
    names: &Names,
    model: &Model,
    graph: &lemma_lang::model::Graph,
) -> Result<(), LowerError> {
    let Some(relation) = model
        .relations
        .iter()
        .find(|relation| relation.address == graph.relation)
    else {
        return Ok(());
    };
    let columns = relation
        .columns
        .iter()
        .map(|column| (column.name.as_str(), &column.column_type));
    file.relations
        .push((names.get(&graph.address), relation_spec(columns)));
    for row in &graph.rows {
        let values: Vec<FactValue> = row
            .iter()
            .map(|value| fact_value(value, &graph.address))
            .collect::<Result<_, _>>()?;
        for relation_name in [names.get(&graph.relation), names.get(&graph.address)] {
            file.facts.push(DeclaredFact {
                relation: relation_name,
                values: values.clone(),
                snapshot: None,
            });
        }
    }
    Ok(())
}

fn fact_value(value: &Value, owner: &Address) -> Result<FactValue, LowerError> {
    match value {
        Value::Int(number) => Ok(FactValue::Number(*number)),
        _ => Ok(FactValue::Text(constant_symbol(value, owner)?)),
    }
}

/// The symbol a constant becomes in Lemma's Datalog.
fn constant_symbol(value: &Value, owner: &Address) -> Result<String, LowerError> {
    let symbol = match value {
        Value::Text(text) => text.clone(),
        Value::Bool(flag) => flag.to_string(),
        Value::Null => "null".to_owned(),
        Value::Service(address) | Value::Operation(address) => address.to_string(),
        Value::Discovered { service, key } => format!("{service}[{key}]"),
        Value::Entity { entity, fields } => entity_symbol(entity, fields, owner)?,
        Value::Int(number) => number.to_string(),
        Value::List(_) | Value::Record(_) | Value::Request { .. } => {
            return Err(unsupported(owner, "a list, record or request as a fact"));
        }
    };
    Ok(symbol)
}

fn entity_symbol(
    entity: &Address,
    fields: &[(String, Value)],
    owner: &Address,
) -> Result<String, LowerError> {
    let mut symbol = format!("{entity}(");
    for (index, (name, value)) in fields.iter().enumerate() {
        if index > 0 {
            symbol.push_str(", ");
        }
        let _ = write!(symbol, "{name}: {}", constant_symbol(value, owner)?);
    }
    symbol.push(')');
    Ok(symbol)
}

fn body_text(literals: &[Literal], names: &Names, owner: &Address) -> Result<String, LowerError> {
    let written: Vec<String> = literals
        .iter()
        .map(|literal| literal_text(literal, names, owner))
        .collect::<Result<_, _>>()?;
    Ok(written.join(", "))
}

fn literal_text(literal: &Literal, names: &Names, owner: &Address) -> Result<String, LowerError> {
    match literal {
        Literal::Atom {
            is_negated,
            predicate,
            snapshot,
            terms,
        } => {
            let negation = if *is_negated { "!" } else { "" };
            let snapshot = match snapshot {
                Some(Snapshot::Base) => "@base",
                Some(Snapshot::Head) => "@head",
                None => "",
            };
            let name = names.get(predicate.address());
            Ok(format!(
                "{negation}{name}{snapshot}({})",
                terms_text(terms, owner)?
            ))
        }
        Literal::Comparison {
            left,
            operator,
            right,
        } => {
            let operator = match operator.as_str() {
                "==" => "=",
                other => other,
            };
            Ok(format!(
                "{} {operator} {}",
                term_text(left, owner)?,
                term_text(right, owner)?
            ))
        }
    }
}

fn terms_text(terms: &[Term], owner: &Address) -> Result<String, LowerError> {
    let written: Vec<String> = terms
        .iter()
        .map(|term| term_text(term, owner))
        .collect::<Result<_, _>>()?;
    Ok(written.join(", "))
}

fn term_text(term: &Term, owner: &Address) -> Result<String, LowerError> {
    match term {
        Term::Variable(name) => Ok(name.clone()),
        Term::Wildcard => Ok("_".to_owned()),
        Term::Constant(Value::Int(number)) => Ok(number.to_string()),
        Term::Constant(value) => {
            let symbol = constant_symbol(value, owner)?;
            if symbol.contains('"') {
                return Err(unsupported(owner, "a constant holding `\"`"));
            }
            Ok(format!("\"{symbol}\""))
        }
        Term::Field { .. } => Err(unsupported(owner, "comparing a variable's field")),
    }
}

fn unsupported(owner: &Address, what: &str) -> LowerError {
    LowerError::Unsupported {
        what: format!("{what} (in `{owner}`)"),
    }
}

/// The name each requirement's findings carry once lowered: its own, or
/// qualified with its module where two requirements, relations, rules or
/// graphs share it.
pub fn requirement_names(model: &Model) -> BTreeMap<Address, String> {
    let names = Names::new(model);
    model
        .requirements
        .iter()
        .map(|requirement| (requirement.address.clone(), names.get(&requirement.address)))
        .collect()
}

/// Datalog names for declarations: their own where unique, qualified with
/// their module where two declare the same one.
struct Names {
    taken: BTreeSet<String>,
    clashing: BTreeSet<String>,
}

impl Names {
    fn new(model: &Model) -> Self {
        let addresses = model
            .relations
            .iter()
            .map(|relation| &relation.address)
            .chain(model.rules.iter().map(|rule| &rule.address))
            .chain(model.graphs.iter().map(|graph| &graph.address))
            .chain(
                model
                    .requirements
                    .iter()
                    .map(|requirement| &requirement.address),
            );
        let mut taken = BTreeSet::new();
        let mut clashing = BTreeSet::new();
        for address in addresses {
            if !taken.insert(address.name().to_owned()) {
                clashing.insert(address.name().to_owned());
            }
        }
        Self { taken, clashing }
    }

    fn get(&self, address: &Address) -> String {
        let name = address.name();
        if !self.clashing.contains(name) && self.taken.contains(name) {
            return name.to_owned();
        }
        format!(
            "{}_{}",
            address.module.replace('.', "_"),
            address.path.join("_")
        )
    }
}

/// Every predicate's name a rule body reads, for tests.
#[cfg(test)]
fn predicates(model: &Model) -> BTreeSet<String> {
    model
        .rules
        .iter()
        .flat_map(|rule| &rule.clauses)
        .flat_map(|clause| &clause.body)
        .filter_map(|literal| match literal {
            Literal::Atom { predicate, .. } => Some(predicate.address().name().to_owned()),
            Literal::Comparison { .. } => None,
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use std::collections::BTreeMap;

    use lemma_lang::{Repository, SourceError, Sources, check_program};

    use super::*;
    use crate::compile;

    struct Memory(BTreeMap<String, String>);

    impl Sources for Memory {
        fn read(&mut self, _: &Repository, path: &str) -> Result<Option<String>, SourceError> {
            Ok(self.0.get(path).cloned())
        }
    }

    fn model(text: &str) -> Model {
        let mut sources = Memory(BTreeMap::from([("mod.lm".to_owned(), text.to_owned())]));
        let checked = check_program(&mut sources, "mod.lm");
        assert!(checked.diagnostics.is_empty(), "{:#?}", checked.diagnostics);
        checked.model
    }

    #[test]
    fn graph_rows_hold_in_their_relation_and_their_graph() {
        let lowered = grammar_file(
            &model("module example\nrelation Owns(team: Text, service: Text)\ngraph owners : Owns {\n    \"platform\" -> \"gateway\"\n}\nrequire owned {\n    when: owners(team, service)\n    message: \"{team} owns {service}\"\n}\n"),
            "example",
            None,
        )
        .unwrap();

        let relations: Vec<&str> = lowered
            .facts
            .iter()
            .map(|fact| fact.relation.as_str())
            .collect();
        assert_eq!(relations, ["Owns", "owners"]);
        assert_eq!(lowered.requirements["owned"].when, "owners(team, service)");
        compile(&lowered).unwrap();
    }

    #[test]
    fn a_check_selects_requirements_and_overrides_their_severity() {
        let model = model(
            "module example\nrelation R(a: Text)\nrequire kept {\n    when: R(a)\n}\nrequire dropped {\n    when: R(a)\n}\n",
        );
        let runs = [Run {
            requirement: Address::top("example", "kept"),
            severity: Some(LmSeverity::Warning),
            arguments: Vec::new(),
        }];

        let lowered = grammar_file(&model, "example", Some(&runs)).unwrap();

        assert_eq!(lowered.requirements.keys().collect::<Vec<_>>(), ["kept"]);
        assert_eq!(lowered.requirements["kept"].severity, Severity::Warning);
    }

    #[test]
    fn rules_read_the_predicates_they_name() {
        let model = model(
            "module example\nrelation R(a: Text)\nrule r(a: Text) {\n    (a) :- R(a), a != \"x\"\n}\n",
        );

        let lowered = grammar_file(&model, "example", None).unwrap();

        assert_eq!(lowered.rules, ["r(a) :- R(a), a != \"x\"."]);
        assert_eq!(predicates(&model), BTreeSet::from(["R".to_owned()]));
    }
}
