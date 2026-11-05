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

/// The consumer facts a call is read from: a call-like relation reads at
/// least one.
const CALL_FACTS: [&str; 3] = ["calls_path", "calls_client", "client_operation"];

/// Consumer facts written with each call, for the path it calls: complete
/// for every call in a file read.
const WRITTEN_WITH_CALLS: [&str; 1] = ["addressed_to"];

/// Consumer facts about one file, complete for every file read and for the
/// files it imports.
const SCOPED_INPUTS: [&str; 4] = ["mentions", "in_file", "named", "imports"];

/// Inputs written only from services' route and schema code, which is
/// always read whole. `imports` rows keyed by such a file are complete too.
const SERVICE_INPUTS: [&str; 5] = [
    "route",
    "route_schema",
    "inline_strict_route",
    "strict_schema",
    "union_member",
];

/// What change-driven evaluation of a grammar needs.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct Plan {
    /// Relations, computed without consumer facts, whose rows hold the
    /// operations findings can be about.
    pub anchors: BTreeSet<String>,
    /// Whether a requirement can hold for code a change edits, so files
    /// that differ between the base and the head are read.
    pub reads_changed_files: bool,
    /// How many `imports` joins a rule chains from a file: the levels of
    /// imported files to read beyond the files selected.
    pub import_depth: usize,
}

/// How a grammar can be evaluated.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum Evaluability {
    /// Change-driven, by this plan.
    ChangeDriven(Plan),
    /// Only in full, for this reason.
    Full(String),
}

/// How `grammar` can be evaluated: change-driven, with its plan, or only in
/// full, and why.
pub(crate) fn plan(grammar: &CompiledGrammar) -> Evaluability {
    match change_driven_plan(grammar) {
        Ok(plan) => Evaluability::ChangeDriven(plan),
        Err(reason) => Evaluability::Full(reason),
    }
}

/// The plan, or why there is none.
fn change_driven_plan(grammar: &CompiledGrammar) -> Result<Plan, String> {
    let rules = grammar.program.rules();
    let raw = inputs_read(rules, &BTreeSet::new());
    // Relations about service code read imports only of service files,
    // which are complete; they count as reading no consumer fact.
    let reads = inputs_read(rules, &service_keyed(rules, &raw));
    let depth = imports_depth(rules)?;
    let mut kinds = Kinds {
        rules,
        reads,
        depth,
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
        import_depth: 0,
    };
    for rule in rules
        .iter()
        .filter(|rule| outputs.contains(rule.head.relation.as_str()))
    {
        match kinds.shape(&rule.body) {
            Shape::Complete => {}
            Shape::Anchored(anchors, depth) => {
                plan.anchors.extend(anchors);
                plan.import_depth = plan.import_depth.max(depth);
            }
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
    let derived = crate::evaluate(grammar, engine, &request)?;
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
    /// How many `imports` joins each derived relation chains.
    depth: BTreeMap<String, usize>,
    /// Derived relations every rule of which holds only for edited code,
    /// such as calls a change adds.
    change_local: BTreeSet<String>,
}

/// How a requirement depends on consumer facts.
enum Shape {
    Complete,
    /// The relations computed without consumer facts it is anchored on, and
    /// how many levels of imports its other consumer facts reach.
    Anchored(Vec<String>, usize),
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
        let mut inputs = self.consumer_inputs(relation).peekable();
        inputs.peek().is_some() && inputs.all(|input| allowed.contains(&input))
    }

    /// Whether `relation`'s rows are calls of an operation that a selection
    /// keeps complete.
    fn is_call(&self, relation: &str) -> bool {
        if input_name(relation) == "client_operation" {
            return true;
        }
        self.reads_only(relation, &CALL_INPUTS)
            && self
                .consumer_inputs(relation)
                .any(|input| CALL_FACTS.contains(&input))
            && binds_operation_column(relation, self.rules)
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
        if let Some((anchors, depth)) = self.anchors(&positive, &selected) {
            return Shape::Anchored(anchors, depth);
        }
        if self.is_change_local(&positive, &negative) {
            return Shape::ChangeLocal;
        }
        Shape::Other
    }

    /// The relations computed without consumer facts that a call in the
    /// body is joined to, when the body's other consumer facts are scoped
    /// to the calling file.
    fn anchors(&self, positive: &[&Atom], selected: &[&Atom]) -> Option<(Vec<String>, usize)> {
        let calls: Vec<&Atom> = positive
            .iter()
            .copied()
            .filter(|atom| self.is_call(&atom.relation))
            .collect();
        // The calling code: each call's consumer and file.
        let calling: BTreeSet<&str> = calls
            .iter()
            .flat_map(|atom| {
                [0, 2]
                    .into_iter()
                    .filter_map(|column| variable_at(atom, column))
            })
            .collect();
        // Facts about the calling code start from it: it is their subject.
        let rest_scoped = selected
            .iter()
            .filter(|atom| !self.is_call(&atom.relation))
            .all(|atom| {
                self.is_scoped(&atom.relation)
                    && variable_at(atom, 0).is_some_and(|subject| calling.contains(subject))
            });
        if calls.is_empty() || !rest_scoped {
            return None;
        }
        // Any relation without consumer facts joins the call to an anchor,
        // but only derived ones are anchors: their rows are what phase one
        // evaluates.
        let complete: Vec<&Atom> = positive
            .iter()
            .copied()
            .filter(|atom| !self.is_selected(&atom.relation))
            .collect();
        let joined = joined_to(&calls, &complete);
        // An anchor's rows must name operations: it shares a variable the
        // body binds to an operation.
        let operation_variables = operation_variables(positive, |relation| self.is_call(relation));
        let anchors: Vec<String> = complete
            .iter()
            .filter(|atom| {
                joined.contains(&atom.relation.as_str())
                    && self.reads.contains_key(&atom.relation)
                    && variables(atom).any(|variable| operation_variables.contains(variable))
            })
            .map(|atom| atom.relation.clone())
            .collect();
        let depth = selected
            .iter()
            .filter(|atom| !self.is_call(&atom.relation))
            .map(|atom| {
                usize::from(input_name(&atom.relation) == "imports")
                    + self.depth.get(&atom.relation).copied().unwrap_or(0)
            })
            // Each starts from the calling code: siblings, not a chain.
            .max()
            .unwrap_or(0);
        (!anchors.is_empty()).then_some((anchors, depth))
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
        let guards: Vec<(&Atom, BTreeSet<String>)> = negative
            .iter()
            .map(|guard| (*guard, selected_at_base(&guard.relation, self.rules)))
            .filter(|(_, inputs)| !inputs.is_empty())
            .collect();
        let is_edited = |atom: &&Atom| self.change_local.contains(&atom.relation);
        // A guard negates, at the base, the very facts it guards, keyed by the
        // same code (its consumer): then only code the change edits can hold.
        let is_guarded = |atom: &&Atom| {
            let Some(consumer) = variable_at(atom, 0) else {
                return false;
            };
            let facts: BTreeSet<String> = self.atom_consumer_inputs(atom);
            guards.iter().any(|(guard, inputs)| {
                variables(guard).any(|variable| variable == consumer) && facts.is_subset(inputs)
            })
        };
        let guarded: Vec<&Atom> = selected
            .iter()
            .copied()
            .filter(|atom| is_edited(atom) || is_guarded(atom))
            .collect();
        // Facts written with each call come with the guarded call's file.
        let is_written_with_call = |atom: &&Atom| {
            WRITTEN_WITH_CALLS.contains(&input_name(&atom.relation))
                && guarded.iter().any(|call| shares_variable(atom, call))
        };
        let rests_on_edits = selected.iter().any(is_edited) || !guards.is_empty();
        !selected.is_empty()
            && rests_on_edits
            && selected
                .iter()
                .all(|atom| is_edited(atom) || is_guarded(atom) || is_written_with_call(atom))
    }

    /// The consumer inputs an atom reads: its own relation if it is one,
    /// otherwise those its relation reads.
    fn atom_consumer_inputs(&self, atom: &Atom) -> BTreeSet<String> {
        let name = input_name(&atom.relation);
        if SELECTED_INPUTS.contains(&name) {
            return BTreeSet::from([name.to_owned()]);
        }
        self.consumer_inputs(&atom.relation)
            .map(str::to_owned)
            .collect()
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
fn inputs_read(rules: &[Rule], complete: &BTreeSet<String>) -> BTreeMap<String, BTreeSet<String>> {
    let derived: BTreeSet<&str> = rules
        .iter()
        .map(|rule| rule.head.relation.as_str())
        .collect();
    let mut reads: BTreeMap<String, BTreeSet<String>> = BTreeMap::new();
    loop {
        let mut grew = false;
        for rule in rules {
            // A complete relation's own rules read service imports only.
            let is_complete = complete.contains(&rule.head.relation);
            let found: BTreeSet<String> = rule
                .body
                .iter()
                .filter_map(atom)
                .filter(|atom| !(is_complete && input_name(&atom.relation) == "imports"))
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

/// The derived relations about service code: those whose only consumer
/// input is `imports`, where every `imports` join in their rules, and every
/// use of them anywhere, is keyed by a service file (a variable a
/// service-only input binds, or the relation's own key, which its uses key
/// in turn).
fn service_keyed(rules: &[Rule], reads: &BTreeMap<String, BTreeSet<String>>) -> BTreeSet<String> {
    let mut keyed: BTreeSet<String> = reads
        .iter()
        .filter(|(_, inputs)| {
            let consumer: Vec<&String> = inputs
                .iter()
                .filter(|input| SELECTED_INPUTS.contains(&input.as_str()))
                .collect();
            !consumer.is_empty() && consumer.iter().all(|input| *input == "imports")
        })
        .map(|(relation, _)| relation.clone())
        .collect();
    loop {
        let unkeyed: Vec<String> = keyed
            .iter()
            .filter(|relation| !is_keyed_everywhere(relation, rules, &keyed))
            .cloned()
            .collect();
        if unkeyed.is_empty() {
            return keyed;
        }
        for relation in unkeyed {
            keyed.remove(&relation);
        }
    }
}

/// Whether `relation`'s own `imports` joins, and every atom over it in any
/// rule, are keyed by a service file.
fn is_keyed_everywhere(relation: &str, rules: &[Rule], keyed: &BTreeSet<String>) -> bool {
    rules.iter().all(|rule| {
        let service: BTreeSet<&str> = rule
            .body
            .iter()
            .filter_map(atom)
            .filter(|atom| SERVICE_INPUTS.contains(&input_name(&atom.relation)))
            .flat_map(variables)
            .collect();
        let own_key = keyed
            .contains(&rule.head.relation)
            .then(|| variable_at(&rule.head, 0))
            .flatten();
        let is_service_file = |variable: Option<&str>| {
            variable.is_some_and(|variable| service.contains(variable) || Some(variable) == own_key)
        };
        rule.body.iter().filter_map(atom).all(|atom| {
            let is_own_import =
                rule.head.relation == relation && input_name(&atom.relation) == "imports";
            let is_use = atom.relation == relation;
            !(is_own_import || is_use) || is_service_file(variable_at(atom, 0))
        })
    })
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

/// The consumer inputs `relation`, or a relation it is derived from, reads
/// at the base.
fn selected_at_base(relation: &str, rules: &[Rule]) -> BTreeSet<String> {
    let mut found = BTreeSet::new();
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
        for atom in &read {
            let name = input_name(&atom.relation);
            if atom.relation.ends_with("@base") && SELECTED_INPUTS.contains(&name) {
                found.insert(name.to_owned());
            }
        }
        pending.extend(read.iter().map(|atom| atom.relation.clone()));
    }
    found
}

/// The variables `positive` binds to an operation: column 1 of an
/// `operation` atom, or of a call.
fn operation_variables<'a>(
    positive: &[&'a Atom],
    is_call: impl Fn(&str) -> bool,
) -> BTreeSet<&'a str> {
    positive
        .iter()
        .filter(|atom| input_name(&atom.relation) == "operation" || is_call(&atom.relation))
        .filter_map(|atom| variable_at(atom, 1))
        .collect()
}

/// Whether every rule deriving `relation` binds its column 1 to an
/// operation: column 1 of an `operation` atom, or of the relation itself or
/// another such relation.
fn binds_operation_column(relation: &str, rules: &[Rule]) -> bool {
    let defining: Vec<&Rule> = rules
        .iter()
        .filter(|rule| rule.head.relation == relation)
        .collect();
    !defining.is_empty()
        && defining.iter().all(|rule| {
            let Some(column) = variable_at(&rule.head, 1) else {
                return false;
            };
            rule.body.iter().filter_map(positive).any(|atom| {
                let names_operation = input_name(&atom.relation) == "operation"
                    || (atom.relation != relation
                        && rules
                            .iter()
                            .any(|other| other.head.relation == atom.relation));
                names_operation && variable_at(atom, 1) == Some(column)
            })
        })
}

/// For each derived relation, at most how many `imports` joins one of its
/// derivations chains (counted generously: every join in a rule, plus the
/// chains of the relations it reads), or why that has no bound: a rule
/// following imports through itself.
fn imports_depth(rules: &[Rule]) -> Result<BTreeMap<String, usize>, String> {
    let mut depth: BTreeMap<String, usize> = BTreeMap::new();
    // Each pass can lengthen a chain by one rule; more passes than rules
    // means a cycle through imports.
    for _ in 0..=rules.len() {
        let mut grew = false;
        for rule in rules {
            let total: usize = rule
                .body
                .iter()
                .filter_map(atom)
                .map(|atom| {
                    usize::from(input_name(&atom.relation) == "imports")
                        + depth.get(atom.relation.as_str()).copied().unwrap_or(0)
                })
                .sum();
            let entry = depth.entry(rule.head.relation.clone()).or_insert(0);
            if total > *entry {
                *entry = total;
                grew = true;
            }
        }
        if !grew {
            return Ok(depth);
        }
    }
    Err(
        "a rule follows imports through itself, so the files a finding rests on cannot be bounded"
            .to_owned(),
    )
}

/// A program relation's input name: `calls_path@base` is `calls_path`.
fn input_name(relation: &str) -> &str {
    relation.strip_suffix("@base").unwrap_or(relation)
}

/// The variable in `column` of `atom`, if one is written there.
fn shares_variable(left: &Atom, right: &Atom) -> bool {
    let right: BTreeSet<&str> = variables(right).collect();
    variables(left).any(|variable| right.contains(variable))
}

/// The variable in `column` of `atom`, if one is written there.
fn variable_at(atom: &Atom, column: usize) -> Option<&str> {
    match atom.terms.get(column)? {
        Term::Variable(name) => Some(name.as_str()),
        _ => None,
    }
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

        let plan = change_driven_plan(&grammar).unwrap();

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
        assert_eq!(plan.import_depth, 1);
    }

    fn with(extra_rules: &str, requirement: &str) -> CompiledGrammar {
        let base = crate::HTTP_CONTRACTS.trim_end();
        let (head, requirements) = base.split_once("\nrequirements:\n").unwrap();
        compiled(&format!(
            "{head}\n{extra_rules}\nrequirements:\n{requirements}\n  extra:\n    when: {requirement}\n    severity: info\n"
        ))
    }

    #[test]
    fn consumer_facts_about_other_code_than_the_caller_are_evaluated_in_full() {
        let grammar = with(
            "",
            r#"calls(c, op, f, l), removed_operation(op), mentions(g, "legacy")"#,
        );

        let reason = change_driven_plan(&grammar).unwrap_err();

        assert!(reason.contains("extra"), "{reason}");
    }

    #[test]
    fn an_anchor_that_is_not_derived_is_no_anchor() {
        let grammar = with("", "calls(c, op, f, l), operation@base(s, op, m, p)");

        let reason = change_driven_plan(&grammar).unwrap_err();

        assert!(reason.contains("extra"), "{reason}");
    }

    #[test]
    fn a_guard_not_keyed_by_the_calling_code_is_no_guard() {
        let grammar = with(
            "  - called_anywhere_before(m, p) :- calls_path@base(_, m, p, _, _).\n  - newly_called(c, m, p) :- calls_path(c, m, p, f, l), !called_anywhere_before(m, p).",
            "newly_called(c, m, p)",
        );

        let reason = change_driven_plan(&grammar).unwrap_err();

        assert!(reason.contains("extra"), "{reason}");
    }

    #[test]
    fn imports_followed_twice_are_read_two_levels_deep() {
        let grammar = with(
            "  - uses_deep(c, n) :- in_file(c, f), imports(f, g), imports(g, h), mentions(h, n).",
            "calls(c, op, f, l), removed_response_field(op, fld, leaf), uses_deep(c, leaf)",
        );

        let plan = change_driven_plan(&grammar).unwrap();

        assert_eq!(plan.import_depth, 2);
    }

    #[test]
    fn a_requirement_over_every_call_is_evaluated_in_full() {
        let text = format!(
            "{}\n  every_call:\n    when: calls(c, op, f, l)\n    severity: info\n",
            crate::HTTP_CONTRACTS.trim_end()
        );
        let grammar = compiled(&text);

        let reason = change_driven_plan(&grammar).unwrap_err();

        assert!(reason.contains("every_call"), "{reason}");
    }
}
