//! Change-driven evaluation: whether a grammar's findings can come only
//! from the operations a change touches and the files it edits, and so
//! which consumer files need reading at all
//! (`.specs/11-change-driven-evaluation.md`).
//!
//! Consumer files are most of a large system and almost none of a change.
//! Their facts are needed only where a finding can rest on them, which the
//! grammar's requirements say. A requirement qualifies when it is one of:
//!
//! - **complete**: it reads no fact from consumer files (routes against
//!   specs, a client's declared parameters);
//! - **anchored**: it rests on calls of an operation (a relation built only
//!   from the extractor's call facts) joined, through shared variables, to a
//!   relation computed without consumer facts, such as the operations the
//!   change removed. Its other consumer facts are about the calling code's
//!   own file and the files it imports;
//! - **change-local**: it rests on consumer facts guarded by a negation of
//!   the same facts at the base (`!called_before(c, m, p)`), so it can only
//!   hold for code the change edits.
//!
//! A grammar with any other requirement is evaluated in full.

use std::collections::{BTreeMap, BTreeSet};

use lemma_grammar::CompiledGrammar;
use lemma_logic::{Atom, Engine, Literal, Request, Rule, Term, Value};

/// Input relations written from consumer files: read, in change-driven
/// evaluation, only for the files a finding can rest on.
pub(crate) const SELECTED_INPUTS: [&str; 9] = [
    "calls_path",
    "calls_client",
    "client_operation",
    "mentions",
    "in_file",
    "named",
    "imports",
    "addressed_to",
    "part_of_service",
];

/// Consumer facts a selection keeps complete for an operation: every file
/// that could write a call of it, and the service each such file is part
/// of.
const CALL_INPUTS: [&str; 4] = [
    "calls_path",
    "calls_client",
    "client_operation",
    "part_of_service",
];

/// Consumer facts about one file, complete for every file read and for the
/// files it imports.
const SCOPED_INPUTS: [&str; 4] = ["mentions", "in_file", "named", "imports"];

/// What change-driven evaluation of a grammar needs.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct Plan {
    /// Relations, computed without consumer facts, whose rows hold the
    /// operations findings can be about.
    pub anchors: BTreeSet<String>,
    /// Whether a requirement can hold for code a change edits, so files
    /// that differ between the base and the head are read.
    pub reads_changed_files: bool,
}

/// The plan for evaluating `grammar` change-driven, or why it must be
/// evaluated in full.
pub(crate) fn plan(grammar: &CompiledGrammar) -> Result<Plan, String> {
    let rules = grammar.program.rules();
    let reads = inputs_read(rules);
    if let Some(rule) = rules
        .iter()
        .find(|rule| follows_imports_recursively(rule, &reads))
    {
        return Err(format!(
            "`{}` follows imports recursively, so the files a finding rests on cannot be bounded",
            rule.head.relation
        ));
    }
    let mut kinds = Kinds {
        rules,
        reads,
        change_local: BTreeSet::new(),
    };
    kinds.change_local = kinds.change_local_relations();
    let outputs: BTreeSet<&str> = grammar
        .requirements
        .iter()
        .flat_map(|requirement| {
            std::iter::once(requirement.relation.as_str())
                .chain(requirement.base_relation.as_deref())
        })
        .collect();
    let mut plan = Plan {
        anchors: BTreeSet::new(),
        reads_changed_files: false,
    };
    for rule in rules
        .iter()
        .filter(|rule| outputs.contains(rule.head.relation.as_str()))
    {
        match kinds.shape(&rule.body) {
            Shape::Complete => {}
            Shape::Anchored(anchors) => plan.anchors.extend(anchors),
            Shape::ChangeLocal => plan.reads_changed_files = true,
            Shape::Other => {
                return Err(format!(
                    "requirement `{}` rests on consumer facts in a way the change does not bound",
                    rule.head.relation.trim_start_matches("requirement:")
                ));
            }
        }
    }
    Ok(plan)
}

/// The operations, as `METHOD /path`, findings can be about: every
/// operation named in a row of `plan`'s anchors, evaluated over the facts
/// written so far, which hold no consumer facts. Every operation of the
/// specs is a candidate, so a value that happens to look like one but is
/// not one is left out.
pub(crate) fn operations<E: Engine>(
    grammar: &CompiledGrammar,
    engine: &E,
    plan: &Plan,
) -> Result<BTreeSet<String>, crate::VerifyError> {
    let known: BTreeSet<Value> = ["operation", "operation@base"]
        .iter()
        .flat_map(|relation| grammar.facts.rows(relation))
        .filter_map(|row| row.get(1).copied())
        .collect();
    let request = Request {
        outputs: plan.anchors.iter().cloned().collect(),
        ..Request::default()
    };
    let derived = lemma_logic::evaluate(engine, &grammar.program, &grammar.facts, &request)
        .map_err(|error| crate::VerifyError::Evaluation {
            message: crate::describe(&error),
        })?;
    let mut operations = BTreeSet::new();
    for anchor in &plan.anchors {
        for row in derived.rows(anchor) {
            let named = row.iter().filter(|value| known.contains(value));
            operations.extend(named.map(|value| grammar.symbols.display(*value)));
        }
    }
    Ok(operations)
}

/// What is known of a grammar's relations: the inputs each derived one
/// reads, and which hold only for edited code.
struct Kinds<'a> {
    rules: &'a [Rule],
    reads: BTreeMap<String, BTreeSet<String>>,
    /// Derived relations every rule of which holds only for edited code,
    /// such as calls a change adds.
    change_local: BTreeSet<String>,
}

/// How a requirement depends on consumer facts.
enum Shape {
    Complete,
    /// The relations computed without consumer facts it is anchored on.
    Anchored(Vec<String>),
    ChangeLocal,
    Other,
}

impl Kinds<'_> {
    /// Whether `relation` is, or reads, a consumer input.
    fn is_selected(&self, relation: &str) -> bool {
        SELECTED_INPUTS.contains(&input_name(relation))
            || self.consumer_inputs(relation).next().is_some()
    }

    /// The consumer inputs a derived relation reads.
    fn consumer_inputs<'s>(&'s self, relation: &str) -> impl Iterator<Item = &'s str> {
        self.reads
            .get(relation)
            .into_iter()
            .flatten()
            .map(String::as_str)
            .filter(|input| SELECTED_INPUTS.contains(input))
    }

    /// Whether `relation` is derived and reads no consumer input but
    /// `allowed`.
    fn reads_only(&self, relation: &str, allowed: &[&str]) -> bool {
        self.reads.contains_key(relation)
            && self
                .consumer_inputs(relation)
                .all(|input| allowed.contains(&input))
    }

    /// Whether `relation`'s rows are calls of an operation that a selection
    /// keeps complete.
    fn is_call(&self, relation: &str) -> bool {
        input_name(relation) == "client_operation" || self.reads_only(relation, &CALL_INPUTS)
    }

    /// Whether `relation` holds facts about one file only.
    fn is_scoped(&self, relation: &str) -> bool {
        SCOPED_INPUTS.contains(&input_name(relation)) || self.reads_only(relation, &SCOPED_INPUTS)
    }

    fn change_local_relations(&self) -> BTreeSet<String> {
        self.reads
            .keys()
            .filter(|relation| {
                let mut defining = self
                    .rules
                    .iter()
                    .filter(|rule| &rule.head.relation == *relation)
                    .peekable();
                defining.peek().is_some()
                    && defining.all(|rule| matches!(self.shape(&rule.body), Shape::ChangeLocal))
            })
            .cloned()
            .collect()
    }

    fn shape(&self, body: &[Literal]) -> Shape {
        let positive: Vec<&Atom> = body.iter().filter_map(positive).collect();
        let negative: Vec<&Atom> = body.iter().filter_map(negative).collect();
        let selected: Vec<&Atom> = positive
            .iter()
            .chain(&negative)
            .copied()
            .filter(|atom| self.is_selected(&atom.relation))
            .collect();
        if selected.is_empty() {
            return Shape::Complete;
        }
        if let Some(anchors) = self.anchors(&positive, &selected) {
            return Shape::Anchored(anchors);
        }
        if self.is_change_local(&positive, &negative) {
            return Shape::ChangeLocal;
        }
        Shape::Other
    }

    /// The relations computed without consumer facts that a call in the
    /// body is joined to, when the body's other consumer facts are scoped
    /// to the calling file.
    fn anchors(&self, positive: &[&Atom], selected: &[&Atom]) -> Option<Vec<String>> {
        let calls: Vec<&Atom> = positive
            .iter()
            .copied()
            .filter(|atom| self.is_call(&atom.relation))
            .collect();
        let rest_scoped = selected
            .iter()
            .filter(|atom| !self.is_call(&atom.relation))
            .all(|atom| self.is_scoped(&atom.relation));
        if calls.is_empty() || !rest_scoped {
            return None;
        }
        let complete: Vec<&Atom> = positive
            .iter()
            .copied()
            .filter(|atom| !self.is_selected(&atom.relation))
            .collect();
        let joined = joined_to(&calls, &complete);
        let anchors: Vec<String> = complete
            .iter()
            .filter(|atom| joined.contains(&atom.relation.as_str()))
            .map(|atom| atom.relation.clone())
            .collect();
        (!anchors.is_empty()).then_some(anchors)
    }

    /// Whether every consumer fact the body rests on is either a relation
    /// holding only for edited code, or guarded by a negation of consumer
    /// facts at the base sharing a variable with it.
    fn is_change_local(&self, positive: &[&Atom], negative: &[&Atom]) -> bool {
        let selected: Vec<&Atom> = positive
            .iter()
            .copied()
            .filter(|atom| self.is_selected(&atom.relation))
            .collect();
        let guards: Vec<&Atom> = negative
            .iter()
            .copied()
            .filter(|guard| reads_selected_at_base(&guard.relation, self.rules))
            .collect();
        let is_edited = |atom: &&Atom| self.change_local.contains(&atom.relation);
        let is_guarded = |atom: &&Atom| {
            is_edited(atom) || guards.iter().any(|guard| shares_variable(atom, guard))
        };
        let rests_on_edits = selected.iter().any(is_edited) || !guards.is_empty();
        !selected.is_empty() && selected.iter().all(is_guarded) && rests_on_edits
    }
}

/// The relations among `complete` sharing a variable with `calls`,
/// directly or through one another.
fn joined_to<'a>(calls: &[&Atom], complete: &[&'a Atom]) -> BTreeSet<&'a str> {
    let mut variables: BTreeSet<&str> = calls.iter().flat_map(|atom| variables(atom)).collect();
    let mut joined = BTreeSet::new();
    loop {
        let before = joined.len();
        for atom in complete {
            if variables_of(atom).any(|variable| variables.contains(variable)) {
                joined.insert(atom.relation.as_str());
                variables.extend(variables_of(atom));
            }
        }
        if joined.len() == before {
            return joined;
        }
    }
}

/// For each derived relation, the input relations its rules read,
/// transitively, by name without a snapshot.
fn inputs_read(rules: &[Rule]) -> BTreeMap<String, BTreeSet<String>> {
    let derived: BTreeSet<&str> = rules
        .iter()
        .map(|rule| rule.head.relation.as_str())
        .collect();
    let mut reads: BTreeMap<String, BTreeSet<String>> = BTreeMap::new();
    loop {
        let mut grew = false;
        for rule in rules {
            let found: BTreeSet<String> = rule
                .body
                .iter()
                .filter_map(atom)
                .flat_map(|atom| atom_inputs(atom, &derived, &reads))
                .collect();
            let entry = reads.entry(rule.head.relation.clone()).or_default();
            let size = entry.len();
            entry.extend(found);
            grew |= entry.len() != size;
        }
        if !grew {
            return reads;
        }
    }
}

/// The inputs reading `atom` reads: its own relation if it is an input,
/// otherwise what its relation is known to read so far.
fn atom_inputs(
    atom: &Atom,
    derived: &BTreeSet<&str>,
    reads: &BTreeMap<String, BTreeSet<String>>,
) -> Vec<String> {
    if !derived.contains(atom.relation.as_str()) {
        return vec![input_name(&atom.relation).to_owned()];
    }
    reads
        .get(&atom.relation)
        .into_iter()
        .flatten()
        .cloned()
        .collect()
}

/// Whether `relation` (or a relation it is derived from) reads a consumer
/// input at the base.
fn reads_selected_at_base(relation: &str, rules: &[Rule]) -> bool {
    let is_base_input = |atom: &Atom| {
        atom.relation.ends_with("@base") && SELECTED_INPUTS.contains(&input_name(&atom.relation))
    };
    let mut pending = vec![relation.to_owned()];
    let mut seen = BTreeSet::new();
    while let Some(next) = pending.pop() {
        if !seen.insert(next.clone()) {
            continue;
        }
        let read: Vec<&Atom> = rules
            .iter()
            .filter(|rule| rule.head.relation == next)
            .flat_map(|rule| rule.body.iter().filter_map(atom))
            .collect();
        if read.iter().any(|atom| is_base_input(atom)) {
            return true;
        }
        pending.extend(read.iter().map(|atom| atom.relation.clone()));
    }
    false
}

/// Whether `rule` reads the relation it derives and imports are among what
/// that relation reads: a chain of imports of unbounded length.
fn follows_imports_recursively(rule: &Rule, reads: &BTreeMap<String, BTreeSet<String>>) -> bool {
    let is_recursive = rule
        .body
        .iter()
        .filter_map(atom)
        .any(|atom| atom.relation == rule.head.relation);
    is_recursive
        && reads
            .get(&rule.head.relation)
            .is_some_and(|inputs| inputs.contains("imports"))
}

/// A program relation's input name: `calls_path@base` is `calls_path`.
fn input_name(relation: &str) -> &str {
    relation.strip_suffix("@base").unwrap_or(relation)
}

fn shares_variable(left: &Atom, right: &Atom) -> bool {
    let right: BTreeSet<&str> = variables(right).collect();
    variables(left).any(|variable| right.contains(variable))
}

fn variables(atom: &Atom) -> impl Iterator<Item = &str> {
    variables_of(atom)
}

fn variables_of(atom: &Atom) -> impl Iterator<Item = &str> {
    atom.terms.iter().filter_map(|term| match term {
        Term::Variable(name) => Some(name.as_str()),
        _ => None,
    })
}

fn atom(literal: &Literal) -> Option<&Atom> {
    match literal {
        Literal::Positive(atom) | Literal::Negative(atom) => Some(atom),
        Literal::Compare { .. } => None,
    }
}

fn positive(literal: &Literal) -> Option<&Atom> {
    match literal {
        Literal::Positive(atom) => Some(atom),
        _ => None,
    }
}

fn negative(literal: &Literal) -> Option<&Atom> {
    match literal {
        Literal::Negative(atom) => Some(atom),
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn compiled(text: &str) -> CompiledGrammar {
        lemma_grammar::compile(&lemma_grammar::parse_grammar(text).unwrap()).unwrap()
    }

    #[test]
    fn the_built_in_grammar_is_change_driven() {
        let grammar = compiled(crate::HTTP_CONTRACTS);

        let plan = plan(&grammar).unwrap();

        for anchor in [
            "removed_operation",
            "new_required_parameter",
            "removed_request_field",
            "removed_response_field",
        ] {
            assert!(
                plan.anchors.contains(anchor),
                "{anchor} in {:?}",
                plan.anchors
            );
        }
        assert!(plan.reads_changed_files);
    }

    #[test]
    fn a_requirement_over_every_call_is_evaluated_in_full() {
        let text = format!(
            "{}\n  every_call:\n    when: calls(c, op, f, l)\n    severity: info\n",
            crate::HTTP_CONTRACTS.trim_end()
        );
        let grammar = compiled(&text);

        let reason = plan(&grammar).unwrap_err();

        assert!(reason.contains("every_call"), "{reason}");
    }
}
