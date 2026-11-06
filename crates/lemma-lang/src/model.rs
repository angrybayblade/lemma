//! A checked program's meaning, for the code that lowers it into what
//! Lemma evaluates: relations, rules, requirements, graphs, services and
//! checks, with every name resolved to an address.

use std::collections::BTreeMap;

use crate::span::Span;
use crate::syntax::ast::{BinaryOperator, Snapshot, UnaryOperator};
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
    /// Behaviors, in module then declaration order.
    pub behaviors: Vec<Behavior>,
    /// Pure functions behaviors call.
    pub functions: Vec<Function>,
    /// Checkers behaviors demand evidence from.
    pub checkers: Vec<Checker>,
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
    /// A parameterised behavior's arguments, evaluated in the check's
    /// module; empty for anything else.
    pub arguments: Vec<CallArgument>,
}

/// The entry module's workspace.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct Workspace {
    /// The language version it pins.
    pub language: Option<String>,
    /// `audit`, `check` or `strict`.
    pub level: Option<String>,
}

/// A behavior: a requirement checked by running statements that may demand
/// evidence (spec 13, section 9).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Behavior {
    /// Its address.
    pub address: Address,
    /// Its parameters; a behavior with any runs only from a check that
    /// gives arguments.
    pub parameters: Vec<Column>,
    /// How serious a violation is.
    pub severity: Severity,
    /// Whether it may pass without running an assertion.
    pub allows_empty: bool,
    /// Its statements, after its leading fields.
    pub body: Vec<Statement>,
    /// Its declaration as written, which a change compares to tell whether
    /// it changed.
    pub text: String,
}

/// A pure function.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Function {
    /// Its address.
    pub address: Address,
    /// Its parameters.
    pub parameters: Vec<Column>,
    /// Its statements.
    pub body: Vec<Statement>,
}

/// A checker: the command that produces evidence for a behavior (spec 13,
/// section 10).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Checker {
    /// Its address.
    pub address: Address,
    /// The repository its module is in, where its command runs.
    pub repository: String,
    /// Its parameters.
    pub parameters: Vec<Column>,
    /// The type of what it observes: the `T` of its `Evidence<T>`.
    pub observes: Type,
    /// The argument list that runs it, without a shell.
    pub command: Vec<String>,
    /// Where it runs, relative to its repository's root.
    pub directory: Option<String>,
    /// A command run once per tree before the first run.
    pub setup: Option<Vec<String>>,
    /// The repositories whose trees its result depends on; its own when
    /// empty.
    pub reads: Vec<String>,
    /// Environment variables, part of its evidence's key.
    pub env: BTreeMap<String, String>,
    /// After this, its evidence is unavailable.
    pub timeout_ms: Option<u64>,
    /// Whether stored evidence may be reused.
    pub is_cached: bool,
    /// Its declaration as written, part of its evidence's key.
    pub text: String,
}

/// A statement of a behavior or function.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Statement {
    /// `let name = value`
    Let {
        /// The name.
        name: String,
        /// Its value.
        value: Expression,
    },
    /// `expect condition [because reason]`
    Expect {
        /// The condition.
        condition: Expression,
        /// Why it should hold.
        because: Option<Expression>,
        /// Where it is.
        span: Span,
        /// The condition as written, for reports.
        text: String,
    },
    /// `return value`
    Return(Expression),
    /// `if condition { then } else { otherwise }`; `else if` is an `If`
    /// alone in `otherwise`.
    If {
        /// The condition.
        condition: Expression,
        /// What runs when it holds.
        then: Vec<Statement>,
        /// What runs when it does not.
        otherwise: Vec<Statement>,
        /// Where it is.
        span: Span,
    },
    /// `for pattern in domain { body }`
    For {
        /// The names each iteration binds.
        pattern: Vec<String>,
        /// A list, or a relation's facts.
        domain: Expression,
        /// The loop body; each iteration is a case.
        body: Vec<Statement>,
        /// Where it is.
        span: Span,
    },
}

/// An expression of a behavior or function, with its names resolved.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Expression {
    /// What it is.
    pub kind: ExpressionKind,
    /// Where it is, in its declaration's module.
    pub span: Span,
}

/// What an expression is.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ExpressionKind {
    /// A value known without running anything.
    Constant(Value),
    /// A local variable or parameter.
    Local(String),
    /// A relation, graph or rule, read as its facts.
    Facts(Predicate),
    /// A function, checker or entity, to be called.
    Callable(Callable),
    /// `[a, b]`
    List(Vec<Expression>),
    /// `{ name: value }`
    Record(Vec<(String, Expression)>),
    /// `target.name`
    Member {
        /// What it is read from.
        target: Box<Expression>,
        /// The member.
        name: String,
    },
    /// `target[keys]`: a fact lookup, or a list or JSON element.
    Index {
        /// What is indexed.
        target: Box<Expression>,
        /// The keys.
        keys: Vec<Expression>,
    },
    /// `callee(arguments)`
    Call {
        /// What is called.
        callee: Box<Expression>,
        /// The arguments, as written.
        arguments: Vec<CallArgument>,
    },
    /// A method evaluated when the behavior runs, such as `x.has("f")`.
    Method {
        /// What it is called on.
        receiver: Box<Expression>,
        /// The method.
        name: String,
        /// The arguments, in order.
        arguments: Vec<Expression>,
    },
    /// `!x`, `-x`, `need x`
    Unary {
        /// The operator.
        operator: UnaryOperator,
        /// Its operand.
        operand: Box<Expression>,
    },
    /// `left op right`
    Binary {
        /// The operator.
        operator: BinaryOperator,
        /// The left operand.
        left: Box<Expression>,
        /// The right operand.
        right: Box<Expression>,
    },
    /// `target@base`
    Snapshot {
        /// What is read.
        target: Box<Expression>,
        /// In which snapshot.
        snapshot: Snapshot,
    },
    /// Something checking reported; it evaluates to unknown.
    Invalid,
}

/// Something a behavior can call.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Callable {
    /// A pure function.
    Function(Address),
    /// A checker, which gives evidence.
    Checker(Address),
    /// An entity type, which builds a value.
    Entity(Address),
}

/// `[name:] value` in a call.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CallArgument {
    /// The parameter's name, for a named argument.
    pub name: Option<String>,
    /// The value.
    pub value: Expression,
}
