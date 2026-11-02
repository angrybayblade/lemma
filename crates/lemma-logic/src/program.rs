//! Lemma's Datalog: relations, rules, and the checks every engine relies on.
//!
//! A [`Program`] is engine-neutral. The grammar compiler builds it from YAML
//! or `.ls` files; engines translate it into their own language. Checking it
//! here, once, means no engine can run a program that is unsafe, unstratified
//! or uses a feature it does not support.

use std::collections::{BTreeMap, BTreeSet};
use std::fmt;

use crate::LogicError;

/// The type of a column. Symbols are interned strings, compared only for
/// equality; numbers support arithmetic comparison.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub enum ColumnType {
    /// An interned string, such as a path or an identity.
    Symbol,
    /// A signed integer, such as a line or a rate.
    Number,
}

/// A named, typed column of a relation.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Column {
    /// The column's name, for output and explanations.
    pub name: String,
    /// What it holds.
    pub kind: ColumnType,
}

/// Whether a relation's facts are supplied or derived by rules.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RelationRole {
    /// Facts come from extractors, specs, declarations or observations.
    Input,
    /// Facts are derived by the program's rules.
    Derived,
}

/// A relation: a named table with typed columns.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Relation {
    /// Unique name within the program.
    pub name: String,
    /// The columns, in order.
    pub columns: Vec<Column>,
    /// Where its facts come from.
    pub role: RelationRole,
}

/// A value in a fact or a rule: an interned symbol or a number.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub enum Value {
    /// An interned symbol; see [`crate::Symbols`].
    Symbol(u32),
    /// A number.
    Number(i64),
}

/// A term in a rule: a variable, a constant, or `_`.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub enum Term {
    /// A named variable, bound by the rule's positive atoms.
    Variable(String),
    /// A fixed value.
    Constant(Value),
    /// Matches anything and binds nothing.
    Wildcard,
}

/// A relation applied to terms, such as `reads(c, op, f)`.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct Atom {
    /// The relation's name.
    pub relation: String,
    /// One term per column.
    pub terms: Vec<Term>,
}

/// A comparison between two terms.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Comparison {
    /// Equal.
    Equal,
    /// Not equal.
    NotEqual,
    /// Less than, for numbers.
    Less,
    /// Less than or equal, for numbers.
    LessOrEqual,
    /// Greater than, for numbers.
    Greater,
    /// Greater than or equal, for numbers.
    GreaterOrEqual,
}

impl Comparison {
    /// Whether this comparison orders values rather than testing equality.
    pub fn is_ordering(self) -> bool {
        !matches!(self, Self::Equal | Self::NotEqual)
    }

    /// The operator as written in rules.
    pub fn symbol(self) -> &'static str {
        match self {
            Self::Equal => "=",
            Self::NotEqual => "!=",
            Self::Less => "<",
            Self::LessOrEqual => "<=",
            Self::Greater => ">",
            Self::GreaterOrEqual => ">=",
        }
    }
}

/// One condition in a rule's body.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub enum Literal {
    /// The atom holds.
    Positive(Atom),
    /// The atom does not hold. Its relation must be fully computed first.
    Negative(Atom),
    /// A comparison between bound terms.
    Compare {
        /// Left side.
        left: Term,
        /// The operator.
        op: Comparison,
        /// Right side.
        right: Term,
    },
}

/// A rule: the head holds whenever every literal in the body holds.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct Rule {
    /// What the rule derives.
    pub head: Atom,
    /// The conditions, all of which must hold.
    pub body: Vec<Literal>,
}

/// Optional language features beyond core Datalog, which engines declare.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
#[expect(
    clippy::struct_excessive_bools,
    reason = "independent capability flags"
)]
pub struct Features {
    /// Ordering comparisons (`<`, `>`, ...) between numbers.
    pub ordering: bool,
    /// Derivation trees for derived facts.
    pub derivations: bool,
    /// Negation (stratified). Core, but listed so engines can refuse it.
    pub negation: bool,
    /// Recursive rules. Core, but listed so engines can refuse it.
    pub recursion: bool,
}

/// A checked program: relations, rules, and the order to evaluate them in.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Program {
    relations: BTreeMap<String, Relation>,
    rules: Vec<Rule>,
    strata: Vec<Vec<usize>>,
}

impl Program {
    /// Checks relations and rules and computes the evaluation order.
    ///
    /// Every rule must be *safe* (each variable in the head, a negated atom
    /// or a comparison appears in a positive atom), use declared relations
    /// with the right arity and column types, derive only derived relations,
    /// and not depend on itself through negation.
    pub fn new(relations: Vec<Relation>, rules: Vec<Rule>) -> Result<Self, LogicError> {
        let relations = index_relations(relations)?;
        for (index, rule) in rules.iter().enumerate() {
            check_rule(&relations, index, rule)?;
        }
        let strata = stratify(&relations, &rules)?;
        Ok(Self {
            relations,
            rules,
            strata,
        })
    }

    /// Every relation, by name, sorted.
    pub fn relations(&self) -> impl Iterator<Item = &Relation> {
        self.relations.values()
    }

    /// A relation by name.
    pub fn relation(&self, name: &str) -> Option<&Relation> {
        self.relations.get(name)
    }

    /// The rules, in the order given.
    pub fn rules(&self) -> &[Rule] {
        &self.rules
    }

    /// Rule indexes grouped into strata, lowest first. Every relation a
    /// stratum negates is complete before the stratum runs.
    pub fn strata(&self) -> &[Vec<usize>] {
        &self.strata
    }

    /// The optional features the program uses.
    pub fn features(&self) -> Features {
        let literals = || self.rules.iter().flat_map(|rule| &rule.body);
        Features {
            ordering: literals()
                .any(|literal| matches!(literal, Literal::Compare { op, .. } if op.is_ordering())),
            derivations: false,
            negation: literals().any(|literal| matches!(literal, Literal::Negative(_))),
            recursion: self
                .strata
                .iter()
                .any(|stratum| is_recursive(&self.rules, stratum)),
        }
    }
}

fn index_relations(relations: Vec<Relation>) -> Result<BTreeMap<String, Relation>, LogicError> {
    let mut indexed = BTreeMap::new();
    for relation in relations {
        if relation.columns.is_empty() {
            return Err(LogicError::EmptyRelation {
                relation: relation.name,
            });
        }
        let name = relation.name.clone();
        if indexed.insert(name.clone(), relation).is_some() {
            return Err(LogicError::DuplicateRelation { relation: name });
        }
    }
    Ok(indexed)
}

fn check_rule(
    relations: &BTreeMap<String, Relation>,
    index: usize,
    rule: &Rule,
) -> Result<(), LogicError> {
    let head = declared(relations, index, &rule.head)?;
    if head.role != RelationRole::Derived {
        return Err(LogicError::DerivesInput {
            rule: index,
            relation: head.name.clone(),
        });
    }
    let mut kinds = BTreeMap::new();
    let mut bound = BTreeSet::new();
    for literal in &rule.body {
        if let Literal::Positive(atom) = literal {
            record_variables(relations, index, atom, &mut kinds)?;
            bound.extend(variables(atom));
        }
    }
    for literal in &rule.body {
        check_literal(relations, index, literal, &bound, &mut kinds)?;
    }
    record_variables(relations, index, &rule.head, &mut kinds)?;
    require_bound(index, variables(&rule.head), &bound)
}

fn check_literal(
    relations: &BTreeMap<String, Relation>,
    index: usize,
    literal: &Literal,
    bound: &BTreeSet<&str>,
    kinds: &mut BTreeMap<String, ColumnType>,
) -> Result<(), LogicError> {
    match literal {
        Literal::Positive(_) => Ok(()),
        Literal::Negative(atom) => {
            if atom.terms.contains(&Term::Wildcard) {
                return Err(LogicError::WildcardInNegation {
                    rule: index,
                    relation: atom.relation.clone(),
                });
            }
            record_variables(relations, index, atom, kinds)?;
            require_bound(index, variables(atom), bound)
        }
        Literal::Compare { left, op, right } => {
            let terms = [left, right];
            let names = terms.iter().filter_map(|term| variable_name(term));
            require_bound(index, names, bound)?;
            let is_symbolic = terms
                .iter()
                .any(|term| term_kind(term, kinds) == Some(ColumnType::Symbol));
            if op.is_ordering() && is_symbolic {
                return Err(LogicError::OrderedSymbols { rule: index });
            }
            Ok(())
        }
    }
}

fn declared<'a>(
    relations: &'a BTreeMap<String, Relation>,
    index: usize,
    atom: &Atom,
) -> Result<&'a Relation, LogicError> {
    let relation = relations
        .get(&atom.relation)
        .ok_or_else(|| LogicError::UnknownRelation {
            rule: index,
            relation: atom.relation.clone(),
        })?;
    if relation.columns.len() != atom.terms.len() {
        return Err(LogicError::Arity {
            rule: index,
            relation: atom.relation.clone(),
            expected: relation.columns.len(),
            found: atom.terms.len(),
        });
    }
    Ok(relation)
}

/// Records each variable's column type, rejecting a variable used as both a
/// symbol and a number, and constants of the wrong type.
fn record_variables(
    relations: &BTreeMap<String, Relation>,
    index: usize,
    atom: &Atom,
    kinds: &mut BTreeMap<String, ColumnType>,
) -> Result<(), LogicError> {
    let relation = declared(relations, index, atom)?;
    for (term, column) in atom.terms.iter().zip(&relation.columns) {
        let mismatch = || LogicError::TypeMismatch {
            rule: index,
            relation: relation.name.clone(),
            column: column.name.clone(),
        };
        match term {
            Term::Variable(name) => {
                let known = *kinds.entry(name.clone()).or_insert(column.kind);
                if known != column.kind {
                    return Err(mismatch());
                }
            }
            Term::Constant(value) if value_kind(*value) != column.kind => return Err(mismatch()),
            Term::Constant(_) | Term::Wildcard => {}
        }
    }
    Ok(())
}

fn require_bound<'a>(
    index: usize,
    names: impl IntoIterator<Item = &'a str>,
    bound: &BTreeSet<&str>,
) -> Result<(), LogicError> {
    for name in names {
        if !bound.contains(name) {
            return Err(LogicError::Unbound {
                rule: index,
                variable: name.to_owned(),
            });
        }
    }
    Ok(())
}

fn variables(atom: &Atom) -> impl Iterator<Item = &str> {
    atom.terms.iter().filter_map(variable_name)
}

fn variable_name(term: &Term) -> Option<&str> {
    match term {
        Term::Variable(name) => Some(name),
        Term::Constant(_) | Term::Wildcard => None,
    }
}

fn term_kind(term: &Term, kinds: &BTreeMap<String, ColumnType>) -> Option<ColumnType> {
    match term {
        Term::Variable(name) => kinds.get(name).copied(),
        Term::Constant(value) => Some(value_kind(*value)),
        Term::Wildcard => None,
    }
}

fn value_kind(value: Value) -> ColumnType {
    match value {
        Value::Symbol(_) => ColumnType::Symbol,
        Value::Number(_) => ColumnType::Number,
    }
}

/// Groups rules into strata by the relations they derive, so that every
/// relation used under negation is complete before it is negated.
fn stratify(
    relations: &BTreeMap<String, Relation>,
    rules: &[Rule],
) -> Result<Vec<Vec<usize>>, LogicError> {
    let derived: Vec<&str> = relations
        .values()
        .filter(|relation| relation.role == RelationRole::Derived)
        .map(|relation| relation.name.as_str())
        .collect();
    let mut level: BTreeMap<&str, usize> = derived.iter().map(|name| (*name, 0)).collect();
    // A stratum level can rise at most once per derived relation; more
    // means a cycle through negation.
    for _ in 0..=derived.len() {
        if !raise_levels(rules, &mut level) {
            return Ok(group_by_level(rules, &level));
        }
    }
    Err(LogicError::NegationCycle {
        relations: cycle_members(rules, &level, derived.len()),
    })
}

/// One pass: a head's level is at least each positive dependency's level and
/// above each negated dependency's level. Returns whether anything rose.
fn raise_levels(rules: &[Rule], level: &mut BTreeMap<&str, usize>) -> bool {
    let mut changed = false;
    for rule in rules {
        let mut needed = level[rule.head.relation.as_str()];
        for literal in &rule.body {
            let (atom, step) = match literal {
                Literal::Positive(atom) => (atom, 0),
                Literal::Negative(atom) => (atom, 1),
                Literal::Compare { .. } => continue,
            };
            if let Some(&dependency) = level.get(atom.relation.as_str()) {
                needed = needed.max(dependency + step);
            }
        }
        let head = level.get_mut(rule.head.relation.as_str());
        if let Some(head) = head.filter(|head| needed > **head) {
            *head = needed;
            changed = true;
        }
    }
    changed
}

fn group_by_level(rules: &[Rule], level: &BTreeMap<&str, usize>) -> Vec<Vec<usize>> {
    let top = level.values().copied().max().unwrap_or(0);
    let mut strata = vec![Vec::new(); top + 1];
    for (index, rule) in rules.iter().enumerate() {
        strata[level[rule.head.relation.as_str()]].push(index);
    }
    strata.retain(|stratum| !stratum.is_empty());
    strata
}

fn cycle_members(rules: &[Rule], level: &BTreeMap<&str, usize>, limit: usize) -> Vec<String> {
    let mut members: Vec<String> = rules
        .iter()
        .map(|rule| rule.head.relation.as_str())
        .filter(|name| level[name] >= limit)
        .map(str::to_owned)
        .collect();
    members.sort();
    members.dedup();
    members
}

fn is_recursive(rules: &[Rule], stratum: &[usize]) -> bool {
    let heads: BTreeSet<&str> = stratum
        .iter()
        .map(|&index| rules[index].head.relation.as_str())
        .collect();
    stratum.iter().any(|&index| {
        rules[index].body.iter().any(|literal| {
            matches!(literal, Literal::Positive(atom) if heads.contains(atom.relation.as_str()))
        })
    })
}

impl fmt::Display for Atom {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(formatter, "{}(", self.relation)?;
        for (position, term) in self.terms.iter().enumerate() {
            if position > 0 {
                write!(formatter, ", ")?;
            }
            match term {
                Term::Variable(name) => write!(formatter, "{name}")?,
                Term::Constant(Value::Symbol(id)) => write!(formatter, "#{id}")?,
                Term::Constant(Value::Number(number)) => write!(formatter, "{number}")?,
                Term::Wildcard => write!(formatter, "_")?,
            }
        }
        write!(formatter, ")")
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn column(name: &str, kind: ColumnType) -> Column {
        Column {
            name: name.to_owned(),
            kind,
        }
    }

    fn relation(name: &str, kinds: &[ColumnType], role: RelationRole) -> Relation {
        let columns = kinds
            .iter()
            .enumerate()
            .map(|(position, kind)| column(&format!("c{position}"), *kind))
            .collect();
        Relation {
            name: name.to_owned(),
            columns,
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

    fn graph() -> Vec<Relation> {
        use ColumnType::Symbol;
        vec![
            relation("edge", &[Symbol, Symbol], RelationRole::Input),
            relation("node", &[Symbol], RelationRole::Input),
            relation("path", &[Symbol, Symbol], RelationRole::Derived),
            relation("unreached", &[Symbol], RelationRole::Derived),
            relation("reached", &[Symbol], RelationRole::Derived),
        ]
    }

    fn path_rules() -> Vec<Rule> {
        vec![
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
        ]
    }

    #[test]
    fn negation_is_evaluated_after_what_it_negates() {
        let mut rules = path_rules();
        rules.push(Rule {
            head: atom("reached", &["b"]),
            body: vec![Literal::Positive(atom("path", &["_a", "b"]))],
        });
        rules.push(Rule {
            head: atom("unreached", &["n"]),
            body: vec![
                Literal::Positive(atom("node", &["n"])),
                Literal::Negative(atom("reached", &["n"])),
            ],
        });

        let program = Program::new(graph(), rules).unwrap();

        assert_eq!(program.strata(), [vec![0, 1, 2], vec![3]]);
        assert!(program.features().recursion);
        assert!(program.features().negation);
    }

    #[test]
    fn variables_must_be_bound_by_a_positive_atom() {
        let rules = vec![Rule {
            head: atom("unreached", &["n"]),
            body: vec![Literal::Negative(atom("node", &["n"]))],
        }];

        let error = Program::new(graph(), rules).unwrap_err();

        assert!(matches!(error, LogicError::Unbound { .. }));
    }

    #[test]
    fn recursion_through_negation_is_rejected() {
        let rules = vec![
            Rule {
                head: atom("reached", &["n"]),
                body: vec![
                    Literal::Positive(atom("node", &["n"])),
                    Literal::Negative(atom("unreached", &["n"])),
                ],
            },
            Rule {
                head: atom("unreached", &["n"]),
                body: vec![
                    Literal::Positive(atom("node", &["n"])),
                    Literal::Negative(atom("reached", &["n"])),
                ],
            },
        ];

        let error = Program::new(graph(), rules).unwrap_err();

        assert!(matches!(error, LogicError::NegationCycle { .. }));
    }

    #[test]
    fn inputs_cannot_be_derived() {
        let rules = vec![Rule {
            head: atom("edge", &["a", "b"]),
            body: vec![Literal::Positive(atom("path", &["a", "b"]))],
        }];

        let error = Program::new(graph(), rules).unwrap_err();

        assert!(matches!(error, LogicError::DerivesInput { .. }));
    }

    #[test]
    fn symbols_cannot_be_ordered() {
        let rules = vec![Rule {
            head: atom("path", &["a", "b"]),
            body: vec![
                Literal::Positive(atom("edge", &["a", "b"])),
                Literal::Compare {
                    left: Term::Variable("a".to_owned()),
                    op: Comparison::Less,
                    right: Term::Variable("b".to_owned()),
                },
            ],
        }];

        let error = Program::new(graph(), rules).unwrap_err();

        assert!(matches!(error, LogicError::OrderedSymbols { .. }));
    }

    #[test]
    fn arity_is_checked() {
        let rules = vec![Rule {
            head: atom("path", &["a"]),
            body: vec![Literal::Positive(atom("edge", &["a", "b"]))],
        }];

        let error = Program::new(graph(), rules).unwrap_err();

        assert!(matches!(error, LogicError::Arity { .. }));
    }
}
