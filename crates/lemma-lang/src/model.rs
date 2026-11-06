//! A checked program's meaning, for the code that lowers it into what
//! Lemma evaluates: relations, rules, requirements, graphs, services and
//! checks, with every name resolved to an address.

use std::collections::BTreeMap;

use crate::syntax::ast::{BinaryOperator, Snapshot};
use crate::types::{Address, Type, Value};

/// Everything a program declares, resolved.
#[derive(Debug, Clone, Default)]
pub struct Model {
    /// Relations, in module then declaration order.
    pub relations: Vec<Relation>,
    /// Rules.
    pub rules: Vec<Rule>,
    /// Relational requirements.
    pub requirements: Vec<Requirement>,
    /// Graphs of stated facts.
    pub graphs: Vec<Graph>,
    /// Services, nested ones included, outermost first.
    pub services: Vec<Service>,
    /// Allowances.
    pub allowances: Vec<Allowance>,
    /// Checks.
    pub checks: Vec<Check>,
    /// The entry module's workspace, if it has one.
    pub workspace: Option<Workspace>,
}

/// A column of a relation or rule.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Column {
    /// Its name.
    pub name: String,
    /// Its type.
    pub column_type: Type,
}

/// A declared relation.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Relation {
    /// Its address.
    pub address: Address,
    /// Its columns.
    pub columns: Vec<Column>,
}

/// A rule, deriving a relation of its own.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Rule {
    /// Its address.
    pub address: Address,
    /// Its columns.
    pub columns: Vec<Column>,
    /// Its clauses.
    pub clauses: Vec<Clause>,
}

/// `(head) :- body`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Clause {
    /// The derived tuple.
    pub head: Vec<Term>,
    /// The conditions.
    pub body: Vec<Literal>,
}

/// One condition.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Literal {
    /// A predicate over terms, one per column (named arguments expanded).
    Atom {
        /// Whether it is negated.
        is_negated: bool,
        /// What it reads.
        predicate: Predicate,
        /// The snapshot, if written.
        snapshot: Option<Snapshot>,
        /// One term per column.
        terms: Vec<Term>,
    },
    /// A comparison.
    Comparison {
        /// The left term.
        left: Term,
        /// The comparison.
        operator: BinaryOperator,
        /// The right term.
        right: Term,
    },
}

/// What an atom reads.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Predicate {
    /// A relation: the union of its stated graphs and provided facts.
    Relation(Address),
    /// A rule's derived facts.
    Rule(Address),
    /// One graph's facts.
    Graph(Address),
}

impl Predicate {
    /// The address of what it reads.
    pub fn address(&self) -> &Address {
        match self {
            Predicate::Relation(address) | Predicate::Rule(address) | Predicate::Graph(address) => {
                address
            }
        }
    }
}

/// A rule term.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Term {
    /// A variable.
    Variable(String),
    /// `_`.
    Wildcard,
    /// A constant.
    Constant(Value),
    /// A variable's field, in a comparison.
    Field {
        /// The variable.
        variable: String,
        /// The field.
        field: String,
    },
}

/// How serious a requirement's findings are.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum Severity {
    /// Fails a check at level `check` or `strict`.
    #[default]
    Error,
    /// Reported without failing.
    Warning,
    /// Worth knowing; never a problem.
    Info,
}

/// A relational requirement.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Requirement {
    /// Its address.
    pub address: Address,
    /// How serious its findings are.
    pub severity: Severity,
    /// Its condition.
    pub body: Vec<Literal>,
    /// Its message template, with `{variable}` placeholders.
    pub message: Option<String>,
    /// The variable holding a finding's location.
    pub at: Option<String>,
}

/// A graph's facts.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Graph {
    /// Its address.
    pub address: Address,
    /// Its relation.
    pub relation: Address,
    /// Written rows, one value per column.
    pub rows: Vec<Vec<Value>>,
    /// The provider request, for a provided graph.
    pub provider: Option<Value>,
}

/// A service.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Service {
    /// Its address.
    pub address: Address,
    /// The repository its module is in.
    pub repository: String,
    /// Its fields, by name.
    pub fields: BTreeMap<String, Value>,
    /// Its own operations, in order.
    pub operations: Vec<Operation>,
}

/// A hand-written operation.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Operation {
    /// Its address.
    pub address: Address,
    /// Whether it is a client operation.
    pub is_client: bool,
    /// Its properties, by name.
    pub fields: BTreeMap<String, Value>,
}

/// An allowance.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Allowance {
    /// The requirement it covers.
    pub requirement: Address,
    /// Which findings.
    pub body: Vec<Literal>,
    /// Why.
    pub reason: Option<String>,
    /// Until when, `YYYY-MM-DD`.
    pub until: Option<String>,
}

/// A check entry point.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Check {
    /// Its address.
    pub address: Address,
    /// The requirements and behaviors it runs, after `*` and `skip`.
    pub runs: Vec<Run>,
}

/// One selected requirement.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Run {
    /// The requirement or behavior.
    pub requirement: Address,
    /// A severity overriding its own.
    pub severity: Option<Severity>,
}

/// The entry module's workspace.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct Workspace {
    /// The language version it pins.
    pub language: Option<String>,
    /// `audit`, `check` or `strict`.
    pub level: Option<String>,
}
