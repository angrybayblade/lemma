//! The facts behaviors read: a program's stated graphs, and the rules
//! derived from them, evaluated bottom-up stratum by stratum.
//!
//! Facts Lemma reads from code or from providers are not stated, so a
//! relation that has any is incompletely covered here, and an absent fact of
//! it is unknown rather than false (spec 13, section 6).

use std::collections::{BTreeMap, BTreeSet, HashMap};

use super::datum::Datum;
use crate::model::{Clause, Literal, Model, Predicate, Rule, Term};
use crate::syntax::ast::{BinaryOperator, Snapshot};
use crate::types::Address;

/// The facts of a relation, graph or rule.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct Facts {
    /// Its columns' names.
    pub columns: Vec<String>,
    /// Its rows, sorted by their text.
    pub rows: Vec<Row>,
    /// Why some of its facts may be absent; `None` when it is completely
    /// covered.
    pub incomplete: Option<String>,
}

/// One fact.
#[derive(Debug, Clone, PartialEq)]
pub struct Row {
    /// One value per column.
    pub values: Vec<Datum>,
    /// Where it came from, such as `stated in graph platform::links`.
    pub supports: Vec<String>,
}

/// The stated facts of a program at the head and, when there is one, at
/// the base, with derived rules kept once computed.
pub struct StatedFacts<'m> {
    head: &'m Model,
    base: Option<&'m Model>,
    /// Derived rules, by address and whether at the base.
    derived: HashMap<(Address, bool), Facts>,
}

impl<'m> StatedFacts<'m> {
    /// The facts `head` states, and `base`'s for `@base`.
    pub fn new(head: &'m Model, base: Option<&'m Model>) -> Self {
        Self {
            head,
            base,
            derived: HashMap::new(),
        }
    }

    /// The facts of `predicate` at `snapshot`.
    pub fn facts(&mut self, predicate: &Predicate, snapshot: Snapshot) -> Facts {
        let Some(model) = self.model(snapshot) else {
            return Facts {
                incomplete: Some("there is no base snapshot to read".to_owned()),
                ..Facts::default()
            };
        };
        match predicate {
            Predicate::Graph(address) => graph_facts(model, address),
            Predicate::Relation(address) => relation_facts(model, address),
            Predicate::Rule(address) => self.rule_facts(address, snapshot),
        }
    }

    /// Whether `body` holds with `values` bound: an allowance's condition
    /// over one finding's variables. Atoms read stated facts at the head.
    pub fn holds(&mut self, body: &[Literal], values: &BTreeMap<String, Datum>) -> bool {
        let clause = Clause {
            head: Vec::new(),
            body: body.to_vec(),
        };
        let current = BTreeMap::new();
        let mut reader = Reader {
            facts: self,
            snapshot: Snapshot::Head,
            current: &current,
            incomplete: None,
        };
        !reader.solve_from(&clause, values.clone()).is_empty()
    }

    fn model(&self, snapshot: Snapshot) -> Option<&'m Model> {
        match snapshot {
            Snapshot::Head => Some(self.head),
            Snapshot::Base => self.base,
        }
    }

    fn rule_facts(&mut self, address: &Address, snapshot: Snapshot) -> Facts {
        let key = (address.clone(), snapshot == Snapshot::Base);
        if let Some(found) = self.derived.get(&key) {
            return found.clone();
        }
        let Some(model) = self.model(snapshot) else {
            return Facts::default();
        };
        for component in strata(model, address) {
            self.derive_component(&component, snapshot);
        }
        self.derived.get(&key).cloned().unwrap_or_default()
    }

    /// Derives the rules of one stratum, which may call each other, until
    /// nothing new is derived. Rules they negate are in earlier strata.
    // TODO(release): each round derives every row again and finds new ones
    // by scanning, quadratic in a rule's facts. Fine for stated graphs;
    // evaluate semi-naively, with hashed rows, before behaviors read facts
    // from code.
    fn derive_component(&mut self, component: &[&Rule], snapshot: Snapshot) {
        let mut rows: BTreeMap<Address, Vec<Vec<Datum>>> = BTreeMap::new();
        let mut incomplete: BTreeMap<Address, Option<String>> = BTreeMap::new();
        loop {
            let mut grew = false;
            for rule in component {
                grew |= self.derive_rule(rule, snapshot, &mut rows, &mut incomplete);
            }
            if !grew {
                break;
            }
        }
        for rule in component {
            let mut found: Vec<Row> = rows
                .remove(&rule.address)
                .unwrap_or_default()
                .into_iter()
                .map(|values| Row {
                    values,
                    supports: vec![format!("derived by rule {}", rule.address)],
                })
                .collect();
            sort_rows(&mut found);
            let facts = Facts {
                columns: rule
                    .columns
                    .iter()
                    .map(|column| column.name.clone())
                    .collect(),
                rows: found,
                incomplete: incomplete.remove(&rule.address).flatten(),
            };
            self.derived
                .insert((rule.address.clone(), snapshot == Snapshot::Base), facts);
        }
    }
}

impl StatedFacts<'_> {
    /// Applies each of a rule's clauses once; returns whether it derived
    /// anything new.
    fn derive_rule(
        &mut self,
        rule: &Rule,
        snapshot: Snapshot,
        rows: &mut BTreeMap<Address, Vec<Vec<Datum>>>,
        incomplete: &mut BTreeMap<Address, Option<String>>,
    ) -> bool {
        let mut grew = false;
        for clause in &rule.clauses {
            let mut reader = Reader {
                facts: self,
                snapshot,
                current: rows,
                incomplete: None,
            };
            let found = reader.solve_clause(clause);
            if let Some(why) = reader.incomplete {
                incomplete.insert(rule.address.clone(), Some(why));
            }
            let known = rows.entry(rule.address.clone()).or_default();
            grew |= add_new(known, found);
        }
        grew
    }
}

/// Reads the facts a clause's body needs while its stratum is derived.
struct Reader<'r, 'm> {
    facts: &'r mut StatedFacts<'m>,
    snapshot: Snapshot,
    /// What the stratum's rules have derived so far.
    current: &'r BTreeMap<Address, Vec<Vec<Datum>>>,
    incomplete: Option<String>,
}

/// Variables bound so far.
type Binding = BTreeMap<String, Datum>;

impl Reader<'_, '_> {
    fn solve_clause(&mut self, clause: &Clause) -> Vec<Vec<Datum>> {
        self.solve_from(clause, Binding::new())
    }

    /// The clause's head for each way its body holds, starting from
    /// `start`.
    fn solve_from(&mut self, clause: &Clause, start: Binding) -> Vec<Vec<Datum>> {
        let mut bindings = vec![start];
        // Positive atoms bind variables; comparisons and negations test
        // bound ones.
        let ordered = clause
            .body
            .iter()
            .filter(|literal| is_positive(literal))
            .chain(clause.body.iter().filter(|literal| !is_positive(literal)));
        for literal in ordered {
            bindings = bindings
                .into_iter()
                .flat_map(|binding| self.extend(&binding, literal))
                .collect();
            if bindings.is_empty() {
                break;
            }
        }
        bindings
            .iter()
            .filter_map(|binding| {
                clause
                    .head
                    .iter()
                    .map(|term| term_value(term, binding))
                    .collect()
            })
            .collect()
    }

    fn extend(&mut self, binding: &Binding, literal: &Literal) -> Vec<Binding> {
        match literal {
            Literal::Atom {
                is_negated,
                predicate,
                snapshot,
                terms,
            } => {
                let rows = self.rows(predicate, snapshot.unwrap_or(self.snapshot));
                let matches: Vec<Binding> = rows
                    .iter()
                    .filter_map(|row| unify(binding, terms, row))
                    .collect();
                match (is_negated, matches.is_empty()) {
                    (false, _) => matches,
                    (true, true) => vec![binding.clone()],
                    (true, false) => Vec::new(),
                }
            }
            Literal::Comparison {
                left,
                operator,
                right,
            } => {
                let (Some(left), Some(right)) =
                    (term_value(left, binding), term_value(right, binding))
                else {
                    return Vec::new();
                };
                if compares(&left, *operator, &right) {
                    vec![binding.clone()]
                } else {
                    Vec::new()
                }
            }
        }
    }

    fn rows(&mut self, predicate: &Predicate, snapshot: Snapshot) -> Vec<Vec<Datum>> {
        if let (Predicate::Rule(address), true) = (predicate, snapshot == self.snapshot) {
            if let Some(rows) = self.current.get(address) {
                return rows.clone();
            }
        }
        let facts = self.facts.facts(predicate, snapshot);
        if let Some(why) = facts.incomplete {
            self.incomplete.get_or_insert(why);
        }
        facts.rows.into_iter().map(|row| row.values).collect()
    }
}

fn is_positive(literal: &Literal) -> bool {
    matches!(
        literal,
        Literal::Atom {
            is_negated: false,
            ..
        }
    )
}

/// `binding` extended so `terms` match `row`, if they can.
fn unify(binding: &Binding, terms: &[Term], row: &[Datum]) -> Option<Binding> {
    if terms.len() != row.len() {
        return None;
    }
    let mut extended = binding.clone();
    for (term, value) in terms.iter().zip(row) {
        match term {
            Term::Wildcard => {}
            Term::Variable(name) => match extended.get(name) {
                Some(bound) if !bound.same_as(value) => return None,
                Some(_) => {}
                None => {
                    extended.insert(name.clone(), value.clone());
                }
            },
            Term::Constant(constant) => {
                if !Datum::from_value(constant).same_as(value) {
                    return None;
                }
            }
            Term::Field { .. } => return None,
        }
    }
    Some(extended)
}

/// A term's value under `binding`, if it has one.
pub(crate) fn term_value(term: &Term, binding: &Binding) -> Option<Datum> {
    match term {
        Term::Variable(name) => binding.get(name).cloned(),
        Term::Constant(value) => Some(Datum::from_value(value)),
        Term::Field { variable, field } => binding.get(variable)?.field(field).cloned(),
        Term::Wildcard => None,
    }
}

/// Whether `left operator right` holds, comparing type-strictly.
pub(crate) fn compares(left: &Datum, operator: BinaryOperator, right: &Datum) -> bool {
    match operator {
        BinaryOperator::Equal => left.same_as(right),
        BinaryOperator::NotEqual => !left.same_as(right),
        _ => {
            let (Datum::Int(left), Datum::Int(right)) = (left, right) else {
                return false;
            };
            match operator {
                BinaryOperator::Less => left < right,
                BinaryOperator::LessOrEqual => left <= right,
                BinaryOperator::Greater => left > right,
                BinaryOperator::GreaterOrEqual => left >= right,
                _ => false,
            }
        }
    }
}

/// Adds the rows of `found` that `known` lacks; returns whether there were
/// any.
fn add_new(known: &mut Vec<Vec<Datum>>, found: Vec<Vec<Datum>>) -> bool {
    let mut grew = false;
    for row in found {
        if !known.iter().any(|existing| same_row(existing, &row)) {
            known.push(row);
            grew = true;
        }
    }
    grew
}

fn same_row(left: &[Datum], right: &[Datum]) -> bool {
    left.len() == right.len()
        && left
            .iter()
            .zip(right)
            .all(|(left, right)| left.same_as(right))
}

/// Sorts rows by their text, so iteration is the same every run.
pub(crate) fn sort_rows(rows: &mut [Row]) {
    rows.sort_by_cached_key(|row| {
        row.values
            .iter()
            .map(ToString::to_string)
            .collect::<Vec<String>>()
    });
}

fn graph_facts(model: &Model, address: &Address) -> Facts {
    let Some(graph) = model.graphs.iter().find(|graph| graph.address == *address) else {
        return Facts {
            incomplete: Some(format!("there is no graph `{address}`")),
            ..Facts::default()
        };
    };
    let mut rows: Vec<Row> = graph
        .rows
        .iter()
        .map(|row| Row {
            values: row.iter().map(Datum::from_value).collect(),
            supports: vec![format!("stated in graph {address}")],
        })
        .collect();
    sort_rows(&mut rows);
    Facts {
        columns: relation_columns(model, &graph.relation),
        rows,
        incomplete: graph.provider.as_ref().map(|_| {
            format!(
                "graph `{}` gets its facts from a provider, which behaviors do not run",
                address.name()
            )
        }),
    }
}

fn relation_facts(model: &Model, address: &Address) -> Facts {
    let mut rows = Vec::new();
    let mut incomplete = from_code(address);
    for graph in model
        .graphs
        .iter()
        .filter(|graph| graph.relation == *address)
    {
        let facts = graph_facts(model, &graph.address);
        if incomplete.is_none() {
            incomplete = facts.incomplete;
        }
        rows.extend(facts.rows);
    }
    sort_rows(&mut rows);
    Facts {
        columns: relation_columns(model, address),
        rows,
        incomplete,
    }
}

/// Why a standard-library relation is incomplete: its facts come from code,
/// which behaviors do not read. Links are stated, so `lemma.links` is
/// complete.
fn from_code(address: &Address) -> Option<String> {
    let is_read_from_code = address.module.starts_with("lemma.") && address.module != "lemma.links";
    is_read_from_code.then(|| {
        format!(
            "`{}`'s facts come from code, which behaviors do not read yet",
            address.name()
        )
    })
}

fn relation_columns(model: &Model, address: &Address) -> Vec<String> {
    model
        .relations
        .iter()
        .find(|relation| relation.address == *address)
        .map(|relation| {
            relation
                .columns
                .iter()
                .map(|column| column.name.clone())
                .collect()
        })
        .unwrap_or_default()
}

/// The rules `target` depends on, grouped into strata that call each
/// other, dependencies first (Tarjan's algorithm).
fn strata<'m>(model: &'m Model, target: &Address) -> Vec<Vec<&'m Rule>> {
    let rules: BTreeMap<&Address, &Rule> = model
        .rules
        .iter()
        .map(|rule| (&rule.address, rule))
        .collect();
    let mut search = Tarjan {
        rules: &rules,
        index: BTreeMap::new(),
        low: BTreeMap::new(),
        stack: Vec::new(),
        on_stack: BTreeSet::new(),
        components: Vec::new(),
    };
    if rules.contains_key(target) {
        search.visit(target);
    }
    search
        .components
        .into_iter()
        .map(|component| {
            component
                .into_iter()
                .filter_map(|address| rules.get(address).copied())
                .collect()
        })
        .collect()
}

struct Tarjan<'a, 'm> {
    rules: &'a BTreeMap<&'m Address, &'m Rule>,
    index: BTreeMap<&'m Address, usize>,
    low: BTreeMap<&'m Address, usize>,
    stack: Vec<&'m Address>,
    on_stack: BTreeSet<&'m Address>,
    components: Vec<Vec<&'m Address>>,
}

impl Tarjan<'_, '_> {
    fn visit(&mut self, address: &Address) {
        let Some((&key, rule)) = self.rules.get_key_value(address) else {
            return;
        };
        let number = self.index.len();
        self.index.insert(key, number);
        self.low.insert(key, number);
        self.stack.push(key);
        self.on_stack.insert(key);
        for dependency in rule_dependencies(rule) {
            let Some((&dependency, _)) = self.rules.get_key_value(dependency) else {
                continue;
            };
            if !self.index.contains_key(dependency) {
                self.visit(dependency);
                let lowest = self.low[key].min(self.low[dependency]);
                self.low.insert(key, lowest);
            } else if self.on_stack.contains(dependency) {
                let lowest = self.low[key].min(self.index[dependency]);
                self.low.insert(key, lowest);
            }
        }
        if self.low[key] != self.index[key] {
            return;
        }
        let mut component = Vec::new();
        while let Some(member) = self.stack.pop() {
            self.on_stack.remove(member);
            component.push(member);
            if member == key {
                break;
            }
        }
        self.components.push(component);
    }
}

fn rule_dependencies(rule: &Rule) -> impl Iterator<Item = &Address> {
    rule.clauses
        .iter()
        .flat_map(|clause| &clause.body)
        .filter_map(|literal| match literal {
            Literal::Atom {
                predicate: Predicate::Rule(address),
                ..
            } => Some(address),
            _ => None,
        })
}
