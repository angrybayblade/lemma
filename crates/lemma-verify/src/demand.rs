//! Change-driven evaluation: whether a grammar's findings can come only
//! from the operations a change touches and the files it edits, and so
//! which consumer files need reading at all
//! (`.specs/11-change-driven-evaluation.md`).
//!
//! Consumer files are most of a large system and almost none of a change.
//! Their facts are needed only where a finding can rest on them, which the
//! grammar's requirements say. Each requirement must be one of these, each
//! checked from the rules' structure rather than from relation names:
//!
//! - **complete**: it reads no fact from consumer files (routes against
//!   specs, a client's declared parameters);
//! - **anchored**: every call it rests on is a call of an operation held by
//!   a relation computed without consumer facts (the operations the change
//!   removed, say), and its other consumer facts are about the calling code:
//!   its own file, the functions in it, and the files it imports;
//! - **change-local**: each consumer fact it rests on is guarded by the
//!   negation of the same row at the base (`!called_before(c, m, p)`), so it
//!   can only hold for code the change edits;
//! - **anchored absence**: it says an operation held by such a relation is
//!   called by nothing (`!called_at_head(op)`, a projection of calls onto
//!   their operation). Every caller of an anchored operation is read, so
//!   the absence is as complete as in full evaluation.
//!
//! A grammar with any other requirement is evaluated in full.

use std::collections::{BTreeMap, BTreeSet};

use lemma_grammar::CompiledGrammar;
use lemma_logic::{Atom, Literal, ReferenceEngine, Request, Rule, Term, Value};

/// Input relations written from consumer files: read, in change-driven
/// evaluation, only for the files a finding can rest on.
const SELECTED_INPUTS: [&str; 9] = [
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

/// The consumer facts a call is read from.
const CALL_FACTS: [&str; 3] = ["calls_path", "calls_client", "client_operation"];

/// The consumer fact saying which service a file belongs to: complete for
/// every file read.
const MEMBERSHIP: &str = "part_of_service";

/// Consumer facts written with each call, for the path it calls: complete
/// for every call in a file read.
const WRITTEN_WITH_CALLS: [&str; 1] = ["addressed_to"];

/// Consumer facts about one scope (a file or a function in it): complete
/// for every file read.
const SCOPED_INPUTS: [&str; 3] = ["mentions", "in_file", "named"];

/// Inputs written only from services' route and schema code, which is
/// always read whole. `imports` rows keyed by such a file are complete too.
const SERVICE_INPUTS: [&str; 6] = [
    "route",
    "code_names_path",
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
    /// How many levels of imports anchored requirements follow from the
    /// calling code: the imported files to read beyond those selected.
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
    let kinds = Kinds::of(grammar.program.rules());
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
    for rule in kinds
        .rules
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
/// operation named in a row of `plan`'s anchors, evaluated in-process over
/// the facts written so far, which hold no consumer facts. Only values that
/// are operations of the specs count.
pub(crate) fn operations(
    grammar: &CompiledGrammar,
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
    log::debug!("finding the operations the change touches, with the reference engine");
    let derived = crate::evaluate(grammar, &ReferenceEngine, &request)?;
    let mut operations = BTreeSet::new();
    for anchor in &plan.anchors {
        for row in derived.rows(anchor) {
            let named = row.iter().filter(|value| known.contains(value));
            operations.extend(named.map(|value| grammar.symbols.display(*value)));
        }
    }
    Ok(operations)
}

/// How a requirement depends on consumer facts.
enum Shape {
    Complete,
    /// The relations holding the operations its calls are of, and how many
    /// levels of imports its other consumer facts reach.
    Anchored(Vec<String>, usize),
    ChangeLocal,
    Other,
}

/// What the rules say of each derived relation.
struct Kinds<'a> {
    rules: &'a [Rule],
    /// The consumer inputs each derived relation reads, counting relations
    /// about service code as reading none.
    consumer: BTreeMap<String, BTreeSet<String>>,
    /// Relations whose rows are calls: column 0 the calling code, column 1
    /// the operation called, column 2 the calling file.
    calls: BTreeSet<String>,
    /// Relations about one scope, with the columns whose values stay within
    /// the scope's file (and its imports) and how many imports they reach.
    scoped: BTreeMap<String, Scope>,
    /// Relations holding just the operation of calls: one column, every
    /// rule copying it from column 1 of a single call.
    called_operations: BTreeSet<String>,
    /// Relations holding only for code a change edits.
    change_local: BTreeSet<String>,
}

/// What a relation about one scope reaches: its columns within the scope's
/// file and imports, and how many levels of imports.
#[derive(Debug, Clone, PartialEq, Eq)]
struct Scope {
    columns: BTreeSet<usize>,
    depth: usize,
}

impl<'a> Kinds<'a> {
    fn of(rules: &'a [Rule]) -> Self {
        let raw = inputs_read(rules, &BTreeSet::new());
        let consumer = inputs_read(rules, &service_keyed(rules, &raw))
            .into_iter()
            .map(|(relation, inputs)| {
                let consumer = inputs
                    .into_iter()
                    .filter(|input| SELECTED_INPUTS.contains(&input.as_str()))
                    .collect();
                (relation, consumer)
            })
            .collect();
        let mut kinds = Self {
            rules,
            consumer,
            calls: BTreeSet::new(),
            scoped: BTreeMap::new(),
            called_operations: BTreeSet::new(),
            change_local: BTreeSet::new(),
        };
        kinds.calls = kinds.call_relations();
        kinds.called_operations = kinds.called_operation_relations();
        kinds.scoped = kinds.scoped_relations();
        kinds.change_local = kinds.change_local_relations();
        kinds
    }

    /// Whether `relation` is, or reads, a consumer input.
    fn is_selected(&self, relation: &str) -> bool {
        SELECTED_INPUTS.contains(&input_name(relation))
            || self
                .consumer
                .get(relation)
                .is_some_and(|inputs| !inputs.is_empty())
    }

    fn is_derived(&self, relation: &str) -> bool {
        self.consumer.contains_key(relation)
    }

    fn defining(&self, relation: &str) -> impl Iterator<Item = &'a Rule> + '_ {
        let relation = relation.to_owned();
        self.rules
            .iter()
            .filter(move |rule| rule.head.relation == relation)
    }

    /// Relations every rule of which derives a call: its operation column
    /// from an `operation` atom or another call, its calling code from a
    /// call fact or another call, reading no consumer fact but calls and the
    /// calling file's service.
    fn call_relations(&self) -> BTreeSet<String> {
        let mut calls: BTreeSet<String> = BTreeSet::new();
        loop {
            let found: BTreeSet<String> = self
                .consumer
                .keys()
                .filter(|relation| {
                    let mut defining = self.defining(relation).peekable();
                    defining.peek().is_some()
                        && defining.all(|rule| derives_call(rule, &calls, |r| self.is_selected(r)))
                })
                .cloned()
                .collect();
            if found == calls {
                return calls;
            }
            calls = found;
        }
    }

    /// Relations every rule of which is `called(op) :- call(_, op, _, ...)`:
    /// the operations something calls, and nothing else.
    fn called_operation_relations(&self) -> BTreeSet<String> {
        self.consumer
            .keys()
            .filter(|relation| {
                let mut defining = self.defining(relation).peekable();
                defining.peek().is_some()
                    && defining.all(|rule| copies_called_operation(rule, |r| self.is_call(r)))
            })
            .cloned()
            .collect()
    }

    /// Whether an atom's rows are calls.
    fn is_call(&self, relation: &str) -> bool {
        input_name(relation) == "client_operation" || self.calls.contains(relation)
    }

    /// Relations about one scope: every consumer fact in their rules
    /// reachable from the head's first column through `in_file` and
    /// `imports`. Relations whose reach keeps growing (imports followed
    /// through themselves) are not scoped.
    fn scoped_relations(&self) -> BTreeMap<String, Scope> {
        let mut scoped: BTreeMap<String, Scope> = BTreeMap::new();
        for _ in 0..=self.rules.len() {
            let found: BTreeMap<String, Scope> = self
                .consumer
                .keys()
                .filter_map(|relation| {
                    let scope = self.relation_scope(relation, &scoped)?;
                    Some((relation.clone(), scope))
                })
                .collect();
            if found == scoped {
                return scoped;
            }
            scoped = found;
        }
        // Still changing: some reach is unbounded. Keep only what is stable.
        BTreeMap::new()
    }

    /// What `relation` reaches when every rule defining it is about one
    /// scope: the columns all of them keep within it, at the deepest reach.
    fn relation_scope(&self, relation: &str, scoped: &BTreeMap<String, Scope>) -> Option<Scope> {
        let rule_scopes: Option<Vec<Scope>> = self
            .defining(relation)
            .map(|rule| rule_scope(rule, scoped, |r| self.is_selected(r)))
            .collect();
        let rule_scopes = rule_scopes?;
        let columns = rule_scopes
            .iter()
            .map(|scope| scope.columns.clone())
            .reduce(|left, right| left.intersection(&right).copied().collect())?;
        let depth = rule_scopes.iter().map(|scope| scope.depth).max()?;
        Some(Scope { columns, depth })
    }

    fn change_local_relations(&self) -> BTreeSet<String> {
        self.consumer
            .keys()
            .filter(|relation| {
                let mut defining = self.defining(relation).peekable();
                defining.peek().is_some() && defining.all(|rule| self.is_change_local(&rule.body))
            })
            .cloned()
            .collect()
    }

    fn shape(&self, body: &[Literal]) -> Shape {
        let selected = body
            .iter()
            .filter_map(atom)
            .any(|atom| self.is_selected(&atom.relation));
        if !selected {
            return Shape::Complete;
        }
        if let Some((anchors, depth)) = self.anchors(body) {
            return Shape::Anchored(anchors, depth);
        }
        if self.is_change_local(body) {
            return Shape::ChangeLocal;
        }
        Shape::Other
    }

    /// The relations holding the operations of every call the body rests
    /// on, and how far into imports its other consumer facts reach, when
    /// those are all about the calling code.
    fn anchors(&self, body: &[Literal]) -> Option<(Vec<String>, usize)> {
        let positive: Vec<&Atom> = body.iter().filter_map(positive).collect();
        let calls: Vec<&Atom> = positive
            .iter()
            .copied()
            .filter(|atom| self.is_call(&atom.relation))
            .collect();
        let absences: Vec<&Atom> = body
            .iter()
            .filter_map(negative)
            .filter(|atom| self.called_operations.contains(&atom.relation))
            .collect();
        if (calls.is_empty() && absences.is_empty())
            || body
                .iter()
                .filter_map(negative)
                .any(|atom| self.is_call(&atom.relation))
        {
            return None;
        }
        let mut anchors = self.absence_anchors(&absences, &positive)?;
        if calls.is_empty() {
            // Nothing is read about the calling code of an absent call, so
            // no other consumer fact may be.
            let reads_other = body
                .iter()
                .filter_map(atom)
                .any(|atom| self.is_selected(&atom.relation) && !absences.contains(&atom));
            return (!reads_other).then_some((anchors, 0));
        }
        for call in &calls {
            let operation = call_operation(call, &positive)?;
            let holding: Vec<String> = positive
                .iter()
                .filter(|atom| {
                    self.is_derived(&atom.relation)
                        && !self.is_selected(&atom.relation)
                        && variables(atom).any(|variable| variable == operation)
                })
                .map(|atom| atom.relation.clone())
                .collect();
            if holding.is_empty() {
                return None;
            }
            anchors.extend(holding);
        }
        let depth = self.calling_code_depth(body, &calls, &absences)?;
        Some((anchors, depth))
    }

    /// The relations holding the operation of each absent call: derived
    /// without consumer facts and sharing its variable. `None` when one is
    /// held by none, since its callers would not all be read.
    fn absence_anchors(&self, absences: &[&Atom], positive: &[&Atom]) -> Option<Vec<String>> {
        let mut anchors = Vec::new();
        for absence in absences {
            let operation = variable_at(absence, 0)?;
            let holding: Vec<String> = positive
                .iter()
                .filter(|atom| {
                    self.is_derived(&atom.relation)
                        && !self.is_selected(&atom.relation)
                        && variables(atom).any(|variable| variable == operation)
                })
                .map(|atom| atom.relation.clone())
                .collect();
            if holding.is_empty() {
                return None;
            }
            anchors.extend(holding);
        }
        Some(anchors)
    }

    /// How many levels of imports the body's consumer facts other than its
    /// calls reach from the calling code, when all of them are about it:
    /// its consumer, its file, or what `in_file` and `imports` reach from
    /// those.
    fn calling_code_depth(
        &self,
        body: &[Literal],
        calls: &[&Atom],
        absences: &[&Atom],
    ) -> Option<usize> {
        let mut reach = Reach::default();
        let calling_code = calls
            .iter()
            .flat_map(|call| [variable_at(call, 0), variable_at(call, 2)])
            .flatten();
        for variable in calling_code {
            reach.add(variable, 0);
        }
        let others: Vec<&Atom> = body
            .iter()
            .filter_map(atom)
            .filter(|atom| {
                self.is_selected(&atom.relation)
                    && !self.is_call(&atom.relation)
                    && !absences.contains(atom)
            })
            .collect();
        reach.spread(&others, &self.scoped);
        let mut depth = 0;
        for atom in others {
            let subject = variable_at(atom, 0)?;
            let reached = reach.depth(subject)?;
            let own = self
                .scoped
                .get(&atom.relation)
                .map_or(0, |scope| scope.depth);
            let is_membership = input_name(&atom.relation) == MEMBERSHIP;
            let is_scoped = is_membership
                || SCOPED_INPUTS.contains(&input_name(&atom.relation))
                || input_name(&atom.relation) == "imports"
                || self.scoped.contains_key(&atom.relation);
            if !is_scoped {
                return None;
            }
            depth = depth.max(reached + own);
        }
        Some(depth)
    }

    /// Whether every consumer fact the body rests on holds only for edited
    /// code: a relation that does, a consumer input whose row is guarded by
    /// the negation of the same row at the base, or a fact written with
    /// such a call. Its negated consumer facts must be those guards, or the
    /// service membership of a guarded call's file.
    fn is_change_local(&self, body: &[Literal]) -> bool {
        let positive: Vec<&Atom> = body
            .iter()
            .filter_map(positive)
            .filter(|atom| self.is_selected(&atom.relation))
            .collect();
        let negative: Vec<&Atom> = body
            .iter()
            .filter_map(negative)
            .filter(|atom| self.is_selected(&atom.relation))
            .collect();
        if positive.is_empty() {
            return false;
        }
        let guarded: Vec<&Atom> = positive
            .iter()
            .copied()
            .filter(|atom| {
                negative
                    .iter()
                    .any(|guard| self.is_same_row_guard(guard, atom))
            })
            .collect();
        let edited: Vec<&Atom> = positive
            .iter()
            .copied()
            .filter(|atom| self.change_local.contains(&atom.relation))
            .collect();
        let rests_on_edits = !guarded.is_empty() || !edited.is_empty();
        let positives_bounded = positive.iter().all(|atom| {
            guarded.contains(atom)
                || edited.contains(atom)
                || (WRITTEN_WITH_CALLS.contains(&input_name(&atom.relation))
                    && guarded.iter().any(|call| shares_variable(atom, call)))
        });
        let negatives_bounded = negative.iter().all(|atom| {
            guarded.iter().any(|row| self.is_same_row_guard(atom, row))
                || (input_name(&atom.relation) == MEMBERSHIP
                    && variable_at(atom, 0).is_some_and(|file| {
                        guarded
                            .iter()
                            .chain(&edited)
                            .any(|row| variables(row).any(|variable| variable == file))
                    }))
        });
        rests_on_edits && positives_bounded && negatives_bounded
    }

    /// Whether `guard` is the negation of `row`'s own row at the base: a
    /// relation with one rule reading only `row`'s input at the base, with
    /// nothing but variables, and used with `row`'s variables in the columns
    /// it copies, its calling code (column 0) among them.
    fn is_same_row_guard(&self, guard: &Atom, row: &Atom) -> bool {
        if !SELECTED_INPUTS.contains(&input_name(&row.relation)) || row.relation.ends_with("@base")
        {
            return false;
        }
        let defining: Vec<&Rule> = self.defining(&guard.relation).collect();
        let [rule] = defining[..] else {
            return false;
        };
        let [Literal::Positive(read)] = &rule.body[..] else {
            return false;
        };
        if read.relation != format!("{}@base", row.relation) {
            return false;
        }
        let mut copies_calling_code = false;
        for (head_column, head_term) in rule.head.terms.iter().enumerate() {
            let Term::Variable(name) = head_term else {
                return false;
            };
            let Some(read_column) = read
                .terms
                .iter()
                .position(|term| matches!(term, Term::Variable(other) if other == name))
            else {
                return false;
            };
            let (Some(used), Some(rowed)) = (
                variable_at(guard, head_column),
                variable_at(row, read_column),
            ) else {
                return false;
            };
            if used != rowed {
                return false;
            }
            copies_calling_code |= read_column == 0;
        }
        copies_calling_code
            && read
                .terms
                .iter()
                .all(|term| matches!(term, Term::Variable(_) | Term::Wildcard))
    }
}

/// Whether `rule` derives a call: column 1 bound to an operation (column 1
/// of an `operation` atom or of a call), column 0 to the calling code
/// (column 0 of a call fact or a call), and no consumer fact read but calls
/// and the service membership of the calling file.
fn derives_call(rule: &Rule, calls: &BTreeSet<String>, is_selected: impl Fn(&str) -> bool) -> bool {
    let positive: Vec<&Atom> = rule.body.iter().filter_map(positive).collect();
    let is_call =
        |relation: &str| CALL_FACTS.contains(&input_name(relation)) || calls.contains(relation);
    let (Some(operation), Some(caller)) = (variable_at(&rule.head, 1), variable_at(&rule.head, 0))
    else {
        return false;
    };
    let operation_bound = positive.iter().any(|atom| {
        (input_name(&atom.relation) == "operation" || calls.contains(&atom.relation))
            && variable_at(atom, 1) == Some(operation)
    });
    let caller_bound = positive
        .iter()
        .any(|atom| is_call(&atom.relation) && variable_at(atom, 0) == Some(caller));
    let files: BTreeSet<&str> = positive
        .iter()
        .filter(|atom| is_call(&atom.relation))
        .filter_map(|atom| file_variable(atom))
        .collect();
    let only_calls = rule.body.iter().filter_map(atom).all(|atom| {
        !is_selected(&atom.relation)
            || is_call(&atom.relation)
            || reads_only_membership_of(atom, &files)
    });
    operation_bound && caller_bound && only_calls
}

/// Whether `rule` is `called(op) :- call(_, op, ...)`: one column, copied
/// from the operation column of its one atom, a call.
fn copies_called_operation(rule: &Rule, is_call: impl Fn(&str) -> bool) -> bool {
    let [Literal::Positive(call)] = &rule.body[..] else {
        return false;
    };
    let [Term::Variable(operation)] = &rule.head.terms[..] else {
        return false;
    };
    is_call(&call.relation) && variable_at(call, 1) == Some(operation.as_str())
}

/// Whether `atom` is service membership (or a relation over it, keyed in
/// column 0) of one of `files`.
fn reads_only_membership_of(atom: &Atom, files: &BTreeSet<&str>) -> bool {
    variable_at(atom, 0).is_some_and(|file| files.contains(file))
        && (input_name(&atom.relation) == MEMBERSHIP
            || !SELECTED_INPUTS.contains(&input_name(&atom.relation)))
}

/// The file column of a call: column 3 of `calls_path` (`consumer, method,
/// path, file, line`), column 2 of `calls_client` and of derived calls,
/// column 4 of `client_operation`.
fn file_variable(atom: &Atom) -> Option<&str> {
    let column = match input_name(&atom.relation) {
        "calls_path" => 3,
        "client_operation" => 4,
        _ => 2,
    };
    variable_at(atom, column)
}

/// The variable holding a call's operation: column 1 of a derived call, or
/// for a client table's operation, column 1 of the `operation` atom joined
/// to it on method and path.
fn call_operation<'a>(call: &'a Atom, positive: &[&'a Atom]) -> Option<&'a str> {
    if input_name(&call.relation) != "client_operation" {
        return variable_at(call, 1);
    }
    let (method, path) = (variable_at(call, 2)?, variable_at(call, 3)?);
    positive
        .iter()
        .filter(|atom| input_name(&atom.relation) == "operation")
        .find(|atom| variable_at(atom, 2) == Some(method) && variable_at(atom, 3) == Some(path))
        .and_then(|atom| variable_at(atom, 1))
}

/// The variables a body reaches from the calling code, each with how many
/// imports away.
#[derive(Default)]
struct Reach<'a> {
    depth: BTreeMap<&'a str, usize>,
}

impl<'a> Reach<'a> {
    fn add(&mut self, variable: &'a str, depth: usize) -> bool {
        let entry = self.depth.entry(variable).or_insert(usize::MAX);
        if depth < *entry {
            *entry = depth;
            return true;
        }
        false
    }

    fn depth(&self, variable: &str) -> Option<usize> {
        self.depth.get(variable).copied()
    }

    /// Spreads reach through `in_file` (a reached scope's file, a reached
    /// file's functions), `imports` (one level further) and relations about
    /// one scope (the columns they keep within it), until it stops growing.
    fn spread(&mut self, atoms: &[&'a Atom], scoped: &BTreeMap<String, Scope>) {
        loop {
            let mut grew = false;
            for atom in atoms {
                grew |= self.step(atom, scoped);
            }
            if !grew {
                return;
            }
        }
    }

    /// Spreads reach through one atom; whether it grew.
    fn step(&mut self, atom: &'a Atom, scoped: &BTreeMap<String, Scope>) -> bool {
        let Some(first) = variable_at(atom, 0) else {
            return false;
        };
        let second = variable_at(atom, 1);
        let mut grew = false;
        match input_name(&atom.relation) {
            "in_file" => {
                if let (Some(depth), Some(file)) = (self.depth(first), second) {
                    grew |= self.add(file, depth);
                }
                if let Some(depth) = second.and_then(|file| self.depth(file)) {
                    grew |= self.add(first, depth);
                }
            }
            "imports" => {
                if let (Some(depth), Some(imported)) = (self.depth(first), second) {
                    grew |= self.add(imported, depth + 1);
                }
            }
            _ => {
                let (Some(scope), Some(depth)) = (scoped.get(&atom.relation), self.depth(first))
                else {
                    return false;
                };
                let kept = scope
                    .columns
                    .iter()
                    .filter_map(|column| variable_at(atom, *column));
                for variable in kept {
                    grew |= self.add(variable, depth + scope.depth);
                }
            }
        }
        grew
    }
}

/// What one rule of a candidate scoped relation reaches from its head's
/// first column, or `None` when a consumer fact in it is out of reach.
fn rule_scope(
    rule: &Rule,
    scoped: &BTreeMap<String, Scope>,
    is_selected: impl Fn(&str) -> bool,
) -> Option<Scope> {
    let start = variable_at(&rule.head, 0)?;
    let selected: Vec<&Atom> = rule
        .body
        .iter()
        .filter_map(atom)
        .filter(|atom| is_selected(&atom.relation))
        .collect();
    let mut reach = Reach::default();
    reach.add(start, 0);
    reach.spread(&selected, scoped);
    let mut depth = 0;
    for atom in &selected {
        let name = input_name(&atom.relation);
        let known = SCOPED_INPUTS.contains(&name)
            || name == "imports"
            || scoped.contains_key(&atom.relation);
        if !known {
            return None;
        }
        let reached = reach.depth(variable_at(atom, 0)?)?;
        let own = scoped.get(&atom.relation).map_or(0, |scope| scope.depth);
        let further = usize::from(name == "imports");
        depth = depth.max(reached + own + further);
    }
    let columns = rule
        .head
        .terms
        .iter()
        .enumerate()
        .filter(|(_, term)| matches!(term, Term::Variable(name) if reach.depth(name).is_some()))
        .map(|(column, _)| column)
        .collect();
    Some(Scope { columns, depth })
}

/// For each derived relation, the input relations its rules read,
/// transitively, by name without a snapshot. A relation in `complete`
/// reads its own rules' `imports` as service imports, which are complete.
fn inputs_read(rules: &[Rule], complete: &BTreeSet<String>) -> BTreeMap<String, BTreeSet<String>> {
    let derived: BTreeSet<&str> = rules
        .iter()
        .map(|rule| rule.head.relation.as_str())
        .collect();
    let mut reads: BTreeMap<String, BTreeSet<String>> = BTreeMap::new();
    loop {
        let mut grew = false;
        for rule in rules {
            let is_complete = complete.contains(&rule.head.relation);
            let found: BTreeSet<String> = rule
                .body
                .iter()
                .filter_map(atom)
                .filter(|atom| !(is_complete && input_name(&atom.relation) == "imports"))
                .flat_map(|atom| atom_inputs(atom, &derived, &reads, complete))
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

/// The inputs reading `atom` reads: its own relation if it is an input, what
/// its relation is known to read so far if derived, and nothing from a
/// relation about service code.
fn atom_inputs(
    atom: &Atom,
    derived: &BTreeSet<&str>,
    reads: &BTreeMap<String, BTreeSet<String>>,
    complete: &BTreeSet<String>,
) -> Vec<String> {
    if !derived.contains(atom.relation.as_str()) {
        return vec![input_name(&atom.relation).to_owned()];
    }
    reads
        .get(&atom.relation)
        .into_iter()
        .flatten()
        .filter(|input| {
            !(complete.contains(&atom.relation) && SELECTED_INPUTS.contains(&input.as_str()))
        })
        .cloned()
        .collect()
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

/// A program relation's input name: `calls_path@base` is `calls_path`.
fn input_name(relation: &str) -> &str {
    relation.strip_suffix("@base").unwrap_or(relation)
}

/// Whether two atoms share a variable.
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

    fn with(extra_rules: &str, requirement: &str) -> CompiledGrammar {
        let base = crate::HTTP_CONTRACTS.trim_end();
        let (head, requirements) = base.split_once("\nrequirements:\n").unwrap();
        compiled(&format!(
            "{head}\n{extra_rules}\nrequirements:\n{requirements}\n  extra:\n    when: {requirement}\n    severity: info\n"
        ))
    }

    fn refused(grammar: &CompiledGrammar) -> String {
        change_driven_plan(grammar).unwrap_err()
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
            "added_operation",
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

    #[test]
    fn an_operation_called_by_nothing_is_anchored_by_the_relation_holding_it() {
        let grammar = with("", "removed_operation(op), !called_at_head(op)");

        let plan = change_driven_plan(&grammar).unwrap();

        assert!(plan.anchors.contains("removed_operation"));
    }

    #[test]
    fn an_absent_call_of_any_operation_is_evaluated_in_full() {
        let grammar = with("", "operation(s, op, m, p), !called_at_head(op)");

        assert!(refused(&grammar).contains("extra"));
    }

    #[test]
    fn an_absent_call_beside_other_consumer_facts_is_evaluated_in_full() {
        let grammar = with(
            "",
            "removed_operation(op), !called_at_head(op), mentions(c, op)",
        );

        assert!(refused(&grammar).contains("extra"));
    }

    #[test]
    fn a_requirement_over_every_call_is_evaluated_in_full() {
        let grammar = with("", "calls(c, op, f, l)");

        assert!(refused(&grammar).contains("extra"));
    }

    #[test]
    fn consumer_facts_about_other_code_than_the_caller_are_evaluated_in_full() {
        let grammar = with(
            "",
            r#"calls(c, op, f, l), removed_operation(op), mentions(g, "legacy")"#,
        );

        assert!(refused(&grammar).contains("extra"));
    }

    #[test]
    fn a_scope_that_strays_from_the_calling_file_is_not_a_scope() {
        let grammar = with(
            "  - uses_anywhere(c, n) :- in_file(c, f), mentions(g, n).",
            "calls(c, op, f, l), removed_response_field(op, fld, leaf), !uses_anywhere(c, leaf)",
        );

        assert!(refused(&grammar).contains("extra"));
    }

    #[test]
    fn an_anchor_on_another_operation_than_the_calls_is_no_anchor() {
        let grammar = with(
            "",
            "calls(c, op, f, l), operation@base(s, op, m, p), operation@base(s, op2, m2, p2), removed_operation(op2)",
        );

        assert!(refused(&grammar).contains("extra"));
    }

    #[test]
    fn a_relation_whose_second_column_is_no_operation_is_no_call() {
        let grammar = with(
            "  - service_of(op, s) :- operation@base(s, op, _, _).\n  - calls_service(c, s, f, l) :- calls(c, op, f, l), service_of(op, s).\n  - retired_service(s) :- removed_operation(op), service_of(op, s).",
            "calls_service(c, s, f, l), retired_service(s)",
        );

        assert!(refused(&grammar).contains("extra"));
    }

    #[test]
    fn a_guard_not_keyed_by_the_calling_code_is_no_guard() {
        let grammar = with(
            "  - called_anywhere_before(m, p) :- calls_path@base(_, m, p, _, _).\n  - newly_called(c, m, p) :- calls_path(c, m, p, f, l), !called_anywhere_before(m, p).",
            "newly_called(c, m, p)",
        );

        assert!(refused(&grammar).contains("extra"));
    }

    #[test]
    fn a_guard_narrower_than_the_row_is_no_guard() {
        let grammar = with(
            "  - called_before_get(c, m, p) :- calls_path@base(c, m, p, _, _), m = \"GET\".\n  - newly(c, m, p) :- calls_path(c, m, p, f, l), !called_before_get(c, m, p).",
            "newly(c, m, p)",
        );

        assert!(refused(&grammar).contains("extra"));
    }

    #[test]
    fn a_second_negation_of_other_code_spoils_a_guard() {
        let grammar = with(
            "  - first_ever(c, m, p) :- calls_path(c, m, p, f, l), !called_before(c, m, p), !called_by_anyone(m, p).\n  - called_by_anyone(m, p) :- calls_path@base(_, m, p, _, _).",
            "first_ever(c, m, p)",
        );

        assert!(refused(&grammar).contains("extra"));
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
    fn imports_followed_through_themselves_are_evaluated_in_full() {
        let grammar = with(
            "  - reaches(f, g) :- imports(f, g).\n  - reaches(f, h) :- reaches(f, g), imports(g, h).\n  - uses_far(c, n) :- in_file(c, f), reaches(f, g), mentions(g, n).",
            "calls(c, op, f, l), removed_response_field(op, fld, leaf), uses_far(c, leaf)",
        );

        assert!(refused(&grammar).contains("extra"));
    }
}
