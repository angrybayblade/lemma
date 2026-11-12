//! A small, obviously correct evaluator: semi-naive evaluation stratum by
//! stratum, with stratified negation and a recorded derivation for every
//! derived fact.
//!
//! It favours clarity over speed (joins are nested loops) and serves as the
//! oracle other engines are checked against, and for small programs.

use std::collections::{BTreeMap, BTreeSet};

use rustc_hash::FxHashMap;
use std::convert::Infallible;

use crate::engine::{Derivation, Derived, Engine, Fact, Request};
use crate::facts::{FactSet, Tuple};
use crate::program::{
    Atom, Comparison, Features, Literal, Program, RelationRole, Rule, Term, Value,
};

/// The reference engine. It supports every feature of Lemma's Datalog.
#[derive(Debug, Default, Clone, Copy)]
pub struct ReferenceEngine;

impl Engine for ReferenceEngine {
    type Prepared = Program;
    type Error = Infallible;

    fn name(&self) -> &'static str {
        "reference"
    }

    fn capabilities(&self) -> Features {
        Features {
            ordering: true,
            derivations: true,
            negation: true,
            recursion: true,
        }
    }

    fn prepare(&self, program: &Program) -> Result<Program, Infallible> {
        Ok(program.clone())
    }

    fn evaluate(
        &self,
        program: &Program,
        facts: &FactSet,
        request: &Request,
    ) -> Result<Derived, Infallible> {
        // Asked for some relations and no explanations, only the rules
        // those relations depend on run, over the facts they read, and no
        // derivation is recorded: the asked-for rows are the same.
        let needed = (request.explain.is_empty() && !request.outputs.is_empty())
            .then(|| needed_relations(program, &request.outputs));
        let mut state = State::new(facts, needed.as_ref());
        for stratum in program.strata() {
            let rules: Vec<(usize, &Rule)> = stratum
                .iter()
                .map(|&index| (index, &program.rules()[index]))
                .filter(|(_, rule)| {
                    needed
                        .as_ref()
                        .is_none_or(|needed| needed.contains(rule.head.relation.as_str()))
                })
                .collect();
            if !rules.is_empty() {
                state.run_stratum(&rules);
            }
        }
        Ok(state.into_derived(program, request))
    }
}

/// A condition a derivation relied on.
#[derive(Debug, Clone)]
enum Premise {
    Holds(Fact),
    Absent(Fact),
}

/// Why a derived fact was first derived.
#[derive(Debug, Clone)]
struct Reason {
    rule: usize,
    premises: Vec<Premise>,
}

struct State {
    known: BTreeMap<String, BTreeSet<Tuple>>,
    reasons: FxHashMap<Fact, Reason>,
    /// Whether derivations are recorded, for explaining facts.
    keeps_reasons: bool,
}

/// The relations a rule's body reads, positively or negated.
fn read_relations(rule: &Rule) -> impl Iterator<Item = &str> {
    rule.body.iter().filter_map(|literal| match literal {
        Literal::Positive(atom) | Literal::Negative(atom) => Some(atom.relation.as_str()),
        Literal::Compare { .. } => None,
    })
}

/// The relations `outputs` depend on, themselves included: every relation a
/// rule deriving a needed relation reads, positively or negated.
fn needed_relations(program: &Program, outputs: &BTreeSet<String>) -> BTreeSet<String> {
    let mut needed: BTreeSet<String> = outputs.clone();
    loop {
        let before = needed.len();
        for rule in program.rules() {
            if needed.contains(&rule.head.relation) {
                needed.extend(read_relations(rule).map(str::to_owned));
            }
        }
        if needed.len() == before {
            return needed;
        }
    }
}

type Bindings<'a> = FxHashMap<&'a str, Value>;
type Delta = BTreeMap<String, BTreeSet<Tuple>>;

/// One rule to solve, optionally with one positive literal restricted to
/// the facts new in the previous round.
/// A join in progress: the rule being solved and the round's indexes.
struct Walk<'j, 'r, 'i> {
    job: &'j Job<'r>,
    indexes: &'j Indexes<'i>,
}

/// Known rows of a relation by the value in one column, built the first time
/// a join asks in a round: without one, a join scans every row of each atom
/// for every binding of the atoms before it. Known facts do not change
/// within a round, so an index holds for all of the round's rules.
#[derive(Default)]
struct Indexes<'i> {
    built: std::cell::RefCell<FxHashMap<(&'i str, usize), std::rc::Rc<ColumnIndex<'i>>>>,
}

/// One relation's rows by one column's value, each list in the relation's
/// order.
type ColumnIndex<'i> = FxHashMap<Value, Vec<&'i Tuple>>;

impl<'i> Indexes<'i> {
    /// The rows of `relation` (all of them in `rows`) whose `column` holds
    /// `value`.
    fn rows(
        &self,
        relation: &'i str,
        rows: &'i BTreeSet<Tuple>,
        column: usize,
        value: Value,
    ) -> Vec<&'i Tuple> {
        let index = {
            let mut built = self.built.borrow_mut();
            let index = built
                .entry((relation, column))
                .or_insert_with(|| std::rc::Rc::new(column_index(rows, column)));
            std::rc::Rc::clone(index)
        };
        index.get(&value).cloned().unwrap_or_default()
    }
}

/// `rows` by the value in `column`, each list in the rows' order.
fn column_index(rows: &BTreeSet<Tuple>, column: usize) -> ColumnIndex<'_> {
    let mut index: ColumnIndex<'_> = FxHashMap::default();
    for row in rows {
        if let Some(found) = row.get(column) {
            index.entry(*found).or_default().push(row);
        }
    }
    index
}

struct Job<'r> {
    index: usize,
    rule: &'r Rule,
    positives: &'r [usize],
    restrict: Option<(usize, &'r Delta)>,
}

impl State {
    fn new(facts: &FactSet, needed: Option<&BTreeSet<String>>) -> Self {
        let mut known: BTreeMap<String, BTreeSet<Tuple>> = BTreeMap::new();
        for name in facts.relation_names() {
            if needed.is_some_and(|needed| !needed.contains(name)) {
                continue;
            }
            known.insert(name.to_owned(), facts.rows(name).cloned().collect());
        }
        Self {
            known,
            reasons: FxHashMap::default(),
            keeps_reasons: needed.is_none(),
        }
    }

    /// Evaluates a stratum to its fixpoint. The first round uses every
    /// fact; later rounds only consider derivations using at least one fact
    /// that was new in the previous round.
    fn run_stratum(&mut self, rules: &[(usize, &Rule)]) {
        let heads: BTreeSet<&str> = rules
            .iter()
            .map(|(_, rule)| rule.head.relation.as_str())
            .collect();
        let mut delta = self.round(rules, &heads, None);
        while !delta.is_empty() {
            delta = self.round(rules, &heads, Some(&delta));
        }
    }

    /// One round; returns the facts derived for the first time.
    fn round(
        &mut self,
        rules: &[(usize, &Rule)],
        heads: &BTreeSet<&str>,
        delta: Option<&Delta>,
    ) -> Delta {
        let found = self.derive(rules, heads, delta);
        self.record(found)
    }

    /// Every fact the rules derive in one round, with its reason.
    fn derive(
        &self,
        rules: &[(usize, &Rule)],
        heads: &BTreeSet<&str>,
        delta: Option<&Delta>,
    ) -> Vec<(Fact, Reason)> {
        let indexes = Indexes::default();
        let mut found = Vec::new();
        for &(index, rule) in rules {
            let positives = positive_positions(rule);
            let restrictions: Vec<Option<(usize, &Delta)>> = match delta {
                None => vec![None],
                Some(delta) => recursive_positions(rule, &positives, heads)
                    .into_iter()
                    .map(|position| Some((position, delta)))
                    .collect(),
            };
            for restrict in restrictions {
                let job = Job {
                    index,
                    rule,
                    positives: &positives,
                    restrict,
                };
                self.solve(&job, &indexes, &mut found);
            }
        }
        found
    }

    fn record(&mut self, found: Vec<(Fact, Reason)>) -> Delta {
        let mut fresh = Delta::new();
        for (fact, reason) in found {
            let rows = self.known.entry(fact.relation.clone()).or_default();
            if !rows.insert(fact.row.clone()) {
                continue;
            }
            fresh
                .entry(fact.relation.clone())
                .or_default()
                .insert(fact.row.clone());
            if self.keeps_reasons {
                self.reasons.insert(fact, reason);
            }
        }
        fresh
    }

    /// Finds every way to satisfy the rule's body.
    fn solve<'i>(&'i self, job: &Job<'_>, indexes: &Indexes<'i>, found: &mut Vec<(Fact, Reason)>) {
        let rule = job.rule;
        let mut emit = |bindings: &Bindings<'_>, chosen: &[Fact]| {
            let Some(premises) = self.check_rest(rule, bindings, chosen) else {
                return;
            };
            let fact = Fact {
                relation: rule.head.relation.clone(),
                row: instantiate(&rule.head, bindings),
            };
            let reason = Reason {
                rule: job.index,
                premises,
            };
            found.push((fact, reason));
        };
        let walk = Walk { job, indexes };
        self.join(
            &walk,
            0,
            &mut Bindings::default(),
            &mut Vec::new(),
            &mut emit,
        );
    }

    #[expect(clippy::too_many_arguments, reason = "the recursive join's state")]
    fn join<'r, 'i>(
        &'i self,
        walk: &Walk<'_, 'r, 'i>,
        depth: usize,
        bindings: &mut Bindings<'r>,
        chosen: &mut Vec<Fact>,
        emit: &mut dyn FnMut(&Bindings<'r>, &[Fact]),
    ) {
        let job = walk.job;
        let Some(&position) = job.positives.get(depth) else {
            emit(bindings, chosen);
            return;
        };
        let Literal::Positive(atom) = &job.rule.body[position] else {
            return;
        };
        let rows: Vec<&Tuple> = match job.restrict {
            Some((restricted, delta)) if restricted == position => {
                delta.get(&atom.relation).into_iter().flatten().collect()
            }
            _ => self.known_rows(walk.indexes, atom, bindings),
        };
        for row in rows {
            let mut added = Vec::new();
            if unify(atom, row, bindings, &mut added) {
                chosen.push(Fact {
                    relation: atom.relation.clone(),
                    row: row.clone(),
                });
                self.join(walk, depth + 1, bindings, chosen, emit);
                chosen.pop();
            }
            for name in added {
                bindings.remove(name);
            }
        }
    }

    /// The known rows of `atom`'s relation that may unify under `bindings`:
    /// those holding the value of its first bound column, found through an
    /// index, or every row when no column is bound. In the relation's order,
    /// so derivations come out in the same order either way.
    fn known_rows<'i>(
        &'i self,
        indexes: &Indexes<'i>,
        atom: &Atom,
        bindings: &Bindings<'_>,
    ) -> Vec<&'i Tuple> {
        let Some((name, rows)) = self.known.get_key_value(&atom.relation) else {
            return Vec::new();
        };
        let bound = atom
            .terms
            .iter()
            .enumerate()
            .find_map(|(column, term)| match term {
                Term::Constant(value) => Some((column, *value)),
                Term::Variable(variable) => bindings
                    .get(variable.as_str())
                    .map(|value| (column, *value)),
                Term::Wildcard => None,
            });
        let Some((column, value)) = bound else {
            return rows.iter().collect();
        };
        indexes.rows(name, rows, column, value)
    }

    /// Checks negations and comparisons under complete bindings, returning
    /// the premises in body order if they hold.
    fn check_rest(
        &self,
        rule: &Rule,
        bindings: &Bindings<'_>,
        chosen: &[Fact],
    ) -> Option<Vec<Premise>> {
        let mut positive = chosen.iter();
        let mut premises = Vec::new();
        for literal in &rule.body {
            match literal {
                Literal::Positive(_) => premises.push(Premise::Holds(positive.next()?.clone())),
                Literal::Negative(atom) => {
                    premises.push(Premise::Absent(self.absent(atom, bindings)?));
                }
                Literal::Compare { left, op, right } => {
                    holds(resolve(left, bindings)?, *op, resolve(right, bindings)?).then_some(())?;
                }
            }
        }
        Some(premises)
    }

    /// The fact a negated atom denotes, if it does not hold.
    fn absent(&self, atom: &Atom, bindings: &Bindings<'_>) -> Option<Fact> {
        let row = instantiate(atom, bindings);
        let holds = self
            .known
            .get(&atom.relation)
            .is_some_and(|rows| rows.contains(&row));
        (!holds).then(|| Fact {
            relation: atom.relation.clone(),
            row,
        })
    }

    fn into_derived(self, program: &Program, request: &Request) -> Derived {
        let wanted = |name: &str| request.outputs.is_empty() || request.outputs.contains(name);
        let relations = program
            .relations()
            .filter(|relation| relation.role == RelationRole::Derived && wanted(&relation.name))
            .map(|relation| {
                let rows = self.known.get(&relation.name).cloned().unwrap_or_default();
                (relation.name.clone(), rows)
            })
            .collect();
        let derivations = request
            .explain
            .iter()
            .filter(|fact| self.reasons.contains_key(*fact))
            .map(|fact| self.explain(fact))
            .collect();
        Derived {
            relations,
            derivations,
        }
    }

    fn explain(&self, fact: &Fact) -> Derivation {
        let Some(reason) = self.reasons.get(fact) else {
            return Derivation::Given(fact.clone());
        };
        let premises = reason
            .premises
            .iter()
            .map(|premise| match premise {
                Premise::Holds(fact) => self.explain(fact),
                Premise::Absent(fact) => Derivation::Absent(fact.clone()),
            })
            .collect();
        Derivation::Derived {
            fact: fact.clone(),
            rule: reason.rule,
            premises,
        }
    }
}

/// Positive literals over relations the stratum derives: the only places a
/// new fact can come from in later rounds.
fn recursive_positions(rule: &Rule, positives: &[usize], heads: &BTreeSet<&str>) -> Vec<usize> {
    positives
        .iter()
        .copied()
        .filter(|&position| {
            matches!(&rule.body[position], Literal::Positive(atom) if heads.contains(atom.relation.as_str()))
        })
        .collect()
}

fn positive_positions(rule: &Rule) -> Vec<usize> {
    rule.body
        .iter()
        .enumerate()
        .filter(|(_, literal)| matches!(literal, Literal::Positive(_)))
        .map(|(position, _)| position)
        .collect()
}

/// Matches a row against an atom, extending `bindings`. Variables bound
/// here are pushed to `added` so the caller can undo them.
fn unify<'r>(
    atom: &'r Atom,
    row: &Tuple,
    bindings: &mut Bindings<'r>,
    added: &mut Vec<&'r str>,
) -> bool {
    for (term, value) in atom.terms.iter().zip(row.iter()) {
        match term {
            Term::Constant(constant) if constant != value => return false,
            Term::Constant(_) | Term::Wildcard => {}
            Term::Variable(name) => match bindings.get(name.as_str()) {
                Some(bound) if bound != value => return false,
                Some(_) => {}
                None => {
                    bindings.insert(name, *value);
                    added.push(name);
                }
            },
        }
    }
    true
}

/// The row an atom denotes under complete bindings. Safe rules bind every
/// variable in heads and negations, and negations have no wildcards.
fn instantiate(atom: &Atom, bindings: &Bindings<'_>) -> Tuple {
    atom.terms
        .iter()
        .map(|term| resolve(term, bindings).unwrap_or(Value::Number(0)))
        .collect()
}

fn resolve(term: &Term, bindings: &Bindings<'_>) -> Option<Value> {
    match term {
        Term::Variable(name) => bindings.get(name.as_str()).copied(),
        Term::Constant(value) => Some(*value),
        Term::Wildcard => None,
    }
}

fn holds(left: Value, op: Comparison, right: Value) -> bool {
    match op {
        Comparison::Equal => left == right,
        Comparison::NotEqual => left != right,
        Comparison::Less => left < right,
        Comparison::LessOrEqual => left <= right,
        Comparison::Greater => left > right,
        Comparison::GreaterOrEqual => left >= right,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::engine::evaluate;
    use crate::program::{Column, ColumnType, Relation};

    fn relation(name: &str, arity: usize, role: RelationRole) -> Relation {
        Relation {
            name: name.to_owned(),
            columns: (0..arity)
                .map(|position| Column {
                    name: format!("c{position}"),
                    kind: ColumnType::Number,
                })
                .collect(),
            role,
        }
    }

    fn atom(relation: &str, variables: &[&str]) -> Atom {
        Atom {
            relation: relation.to_owned(),
            terms: variables
                .iter()
                .map(|name| Term::Variable((*name).to_owned()))
                .collect(),
        }
    }

    fn row(values: &[i64]) -> Tuple {
        values.iter().map(|value| Value::Number(*value)).collect()
    }

    /// Reachability over a chain 1 -> 2 -> 3 -> 4, plus nodes not reached
    /// from 1.
    fn reachability() -> (Program, FactSet) {
        let relations = vec![
            relation("edge", 2, RelationRole::Input),
            relation("node", 1, RelationRole::Input),
            relation("path", 2, RelationRole::Derived),
            relation("from_one", 1, RelationRole::Derived),
            relation("unreached", 1, RelationRole::Derived),
        ];
        let one = Term::Constant(Value::Number(1));
        let rules = vec![
            Rule {
                head: atom("path", &["a", "b"]),
                body: vec![Literal::Positive(atom("edge", &["a", "b"]))],
            },
            Rule {
                head: atom("path", &["a", "c"]),
                body: vec![
                    Literal::Positive(atom("path", &["a", "b"])),
                    Literal::Positive(atom("edge", &["b", "c"])),
                ],
            },
            Rule {
                head: atom("from_one", &["b"]),
                body: vec![Literal::Positive(Atom {
                    relation: "path".to_owned(),
                    terms: vec![one, Term::Variable("b".to_owned())],
                })],
            },
            Rule {
                head: atom("unreached", &["n"]),
                body: vec![
                    Literal::Positive(atom("node", &["n"])),
                    Literal::Negative(atom("from_one", &["n"])),
                ],
            },
        ];
        let program = Program::new(relations, rules).unwrap();
        let mut facts = FactSet::new();
        for (from, to) in [(1, 2), (2, 3), (3, 4)] {
            facts.insert("edge", row(&[from, to]));
        }
        for node in [1, 2, 3, 4, 5] {
            facts.insert("node", row(&[node]));
        }
        (program, facts)
    }

    #[test]
    fn recursion_reaches_a_fixpoint() {
        let (program, facts) = reachability();

        let derived = evaluate(&ReferenceEngine, &program, &facts, &Request::default()).unwrap();

        assert_eq!(derived.rows("path").count(), 6);
        assert!(derived.rows("path").any(|r| r == &row(&[1, 4])));
    }

    #[test]
    fn asking_for_one_relation_gives_the_rows_a_full_evaluation_gives() {
        let (program, facts) = reachability();
        let request = Request {
            outputs: std::iter::once("unreached".to_owned()).collect(),
            ..Request::default()
        };

        let pruned = evaluate(&ReferenceEngine, &program, &facts, &request).unwrap();
        let full = evaluate(&ReferenceEngine, &program, &facts, &Request::default()).unwrap();

        let rows = |derived: &Derived| derived.rows("unreached").cloned().collect::<Vec<_>>();
        assert_eq!(rows(&pruned), rows(&full));
    }

    #[test]
    fn the_relations_an_output_needs_follow_negations_and_recursion() {
        let (program, _) = reachability();

        let needed = needed_relations(&program, &std::iter::once("from_one".to_owned()).collect());

        let expected: BTreeSet<String> = ["from_one", "path", "edge"]
            .into_iter()
            .map(str::to_owned)
            .collect();
        assert_eq!(needed, expected);
    }

    #[test]
    fn negation_sees_the_completed_lower_stratum() {
        let (program, facts) = reachability();

        let derived = evaluate(&ReferenceEngine, &program, &facts, &Request::default()).unwrap();

        let unreached: Vec<_> = derived.rows("unreached").cloned().collect();
        assert_eq!(unreached, [row(&[1]), row(&[5])]);
    }

    #[test]
    fn derivations_lead_back_to_input_facts() {
        let (program, facts) = reachability();
        let wanted = Fact {
            relation: "unreached".to_owned(),
            row: row(&[5]),
        };
        let request = Request {
            explain: vec![wanted.clone()],
            ..Request::default()
        };

        let derived = evaluate(&ReferenceEngine, &program, &facts, &request).unwrap();

        let Derivation::Derived {
            fact,
            rule,
            premises,
        } = &derived.derivations[0]
        else {
            panic!("expected a derived fact");
        };
        assert_eq!(fact, &wanted);
        assert_eq!(*rule, 3);
        assert!(matches!(&premises[0], Derivation::Given(f) if f.relation == "node"));
        assert!(matches!(&premises[1], Derivation::Absent(f) if f.relation == "from_one"));
    }

    #[test]
    fn comparisons_filter_numbers() {
        let relations = vec![
            relation("rate", 2, RelationRole::Input),
            relation("over", 1, RelationRole::Derived),
        ];
        let rules = vec![Rule {
            head: atom("over", &["s"]),
            body: vec![
                Literal::Positive(atom("rate", &["s", "r"])),
                Literal::Compare {
                    left: Term::Variable("r".to_owned()),
                    op: Comparison::Greater,
                    right: Term::Constant(Value::Number(120)),
                },
            ],
        }];
        let program = Program::new(relations, rules).unwrap();
        let mut facts = FactSet::new();
        facts.insert("rate", row(&[1, 188]));
        facts.insert("rate", row(&[2, 90]));

        let derived = evaluate(&ReferenceEngine, &program, &facts, &Request::default()).unwrap();

        let over: Vec<_> = derived.rows("over").cloned().collect();
        assert_eq!(over, [row(&[1])]);
    }
}
