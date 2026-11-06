//! The syntax tree of one `.lm` module, as written.
//!
//! Every node keeps its span, so diagnostics, hovers and go-to-definition
//! can point at source. Nothing here is resolved: a name is just text until
//! [`crate::check`] binds it. See `.specs/13-lm-grammar.md` §14 for the
//! grammar these types follow.

use crate::span::Span;
use crate::syntax::lexer::Comment;

/// A name as written, with where it is.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Name {
    /// Its text, without backticks.
    pub text: String,
    /// Where it is.
    pub span: Span,
}

/// A dotted module name, such as `platform.services.worker`; also a
/// requirement's address in `run`, `skip` and `allow`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ModuleName {
    /// Its segments.
    pub segments: Vec<Name>,
    /// Where it is.
    pub span: Span,
}

impl ModuleName {
    /// The segments joined with dots.
    pub fn dotted(&self) -> String {
        let texts: Vec<&str> = self
            .segments
            .iter()
            .map(|segment| segment.text.as_str())
            .collect();
        texts.join(".")
    }
}

/// One parsed module.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct File {
    /// Its `module` line's name, if it has one.
    pub module: Option<ModuleName>,
    /// Its items, in order.
    pub items: Vec<Item>,
    /// Its comments, for formatting.
    pub comments: Vec<Comment>,
}

/// A top-level item.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Item {
    /// `import ...`
    Import(Import),
    /// `export { ... } from module`
    Reexport(Reexport),
    /// `workspace { ... }`
    Workspace(Workspace),
    /// `repo name = source`
    Repo(Repo),
    /// A declaration, exported or not.
    Declaration(Declared),
}

/// `import module`, `import { a, b as c } from module` or
/// `import * as name from module`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Import {
    /// What it brings in.
    pub selection: ImportSelection,
    /// The module.
    pub module: ModuleName,
    /// Where it is.
    pub span: Span,
}

/// What an import brings into scope.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ImportSelection {
    /// Nothing: the module is loaded only.
    Module,
    /// The named declarations.
    Names(Vec<ImportName>),
    /// The module itself, under a name.
    Namespace(Name),
}

/// `name` or `name as alias` in an import.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ImportName {
    /// The declaration's name in its module.
    pub name: Name,
    /// The name it gets here, if renamed.
    pub alias: Option<Name>,
}

impl ImportName {
    /// The name it is known by in the importing module.
    pub fn local(&self) -> &Name {
        self.alias.as_ref().unwrap_or(&self.name)
    }
}

/// `export { a, b } from module`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Reexport {
    /// The names.
    pub names: Vec<ImportName>,
    /// The module they come from.
    pub module: ModuleName,
    /// Where it is.
    pub span: Span,
}

/// `workspace { ... }`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Workspace {
    /// Its repositories.
    pub repos: Vec<Repo>,
    /// Its fields, such as `language` and `config`.
    pub fields: Vec<Field>,
    /// Where it is.
    pub span: Span,
}

/// `repo name = source`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Repo {
    /// The repository's name, its root module's.
    pub name: Name,
    /// Where it comes from: `local(...)` or `git(...)`.
    pub source: Expression,
    /// Where it is.
    pub span: Span,
}

/// A declaration and whether it is exported.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Declared {
    /// Whether `export` precedes it.
    pub is_exported: bool,
    /// The declaration.
    pub declaration: Declaration,
    /// Where it is, `export` included.
    pub span: Span,
}

/// Something a module declares.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Declaration {
    /// `let name = value`
    Binding(Binding),
    /// `type Name = type`
    Type(TypeDeclaration),
    /// `entity Name { fields }`
    Entity(EntityDeclaration),
    /// `relation Name(columns)`
    Relation(RelationDeclaration),
    /// `service Name { ... }`
    Service(ServiceDeclaration),
    /// `graph name : Relation ...`
    Graph(GraphDeclaration),
    /// `rule name ... { clauses }`
    Rule(RuleDeclaration),
    /// `require name { ... }`
    Require(RequireDeclaration),
    /// `allow requirement { ... }`
    Allow(AllowDeclaration),
    /// `behavior name { ... }`
    Behavior(BehaviorDeclaration),
    /// `fn name(...): Type { ... }`
    Function(FunctionDeclaration),
    /// `checker name(...): Type { ... }`
    Checker(CheckerDeclaration),
    /// `check name { ... }`
    Check(CheckDeclaration),
}

impl Declaration {
    /// The declared name. An allowance declares none: it names the
    /// requirement it covers.
    pub fn name(&self) -> Option<&Name> {
        let name = match self {
            Declaration::Binding(binding) => &binding.name,
            Declaration::Type(declaration) => &declaration.name,
            Declaration::Entity(declaration) => &declaration.name,
            Declaration::Relation(declaration) => &declaration.name,
            Declaration::Service(declaration) => &declaration.name,
            Declaration::Graph(declaration) => &declaration.name,
            Declaration::Rule(declaration) => &declaration.name,
            Declaration::Require(declaration) => &declaration.name,
            Declaration::Allow(_) => return None,
            Declaration::Behavior(declaration) => &declaration.name,
            Declaration::Function(declaration) => &declaration.name,
            Declaration::Checker(declaration) => &declaration.name,
            Declaration::Check(declaration) => &declaration.name,
        };
        Some(name)
    }
}

/// `let name[: Type] = value`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Binding {
    /// The name.
    pub name: Name,
    /// Its written type, if any.
    pub declared_type: Option<TypeExpression>,
    /// Its value.
    pub value: Expression,
    /// Where it is.
    pub span: Span,
}

/// `type Name = type`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TypeDeclaration {
    /// The name.
    pub name: Name,
    /// What it stands for.
    pub value: TypeExpression,
    /// Where it is.
    pub span: Span,
}

/// `entity Name { field: Type ... }`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct EntityDeclaration {
    /// The name.
    pub name: Name,
    /// Its fields.
    pub fields: Vec<TypedField>,
    /// Where it is.
    pub span: Span,
}

/// `relation Name(column: Type, ...)`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RelationDeclaration {
    /// The name.
    pub name: Name,
    /// Its columns, in order.
    pub columns: Vec<TypedField>,
    /// Where it is.
    pub span: Span,
}

/// `service Name { ... }`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ServiceDeclaration {
    /// The name.
    pub name: Name,
    /// Its members, in order.
    pub members: Vec<ServiceMember>,
    /// Where it is.
    pub span: Span,
}

/// Something inside a service.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ServiceMember {
    /// A nested service.
    Service {
        /// Whether it is exported.
        is_exported: bool,
        /// The service.
        service: ServiceDeclaration,
    },
    /// An operation.
    Operation {
        /// Whether it is exported.
        is_exported: bool,
        /// The operation.
        operation: OperationDeclaration,
    },
    /// A `let` binding local to the service.
    Binding(Binding),
    /// A field, such as `index` or `operations`.
    Field(Field),
}

/// `[client] operation name { fields }`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct OperationDeclaration {
    /// Whether it is a client operation.
    pub is_client: bool,
    /// The name.
    pub name: Name,
    /// Its properties.
    pub fields: Vec<Field>,
    /// Where it is.
    pub span: Span,
}

/// `graph name : Relation { rows }` or `graph name : Relation = provider`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct GraphDeclaration {
    /// The name.
    pub name: Name,
    /// Its relation.
    pub relation: TypeExpression,
    /// Its facts.
    pub body: GraphBody,
    /// Where it is.
    pub span: Span,
}

/// Where a graph's facts come from.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum GraphBody {
    /// Written rows.
    Rows(Vec<GraphRow>),
    /// A provider call.
    Provider(Expression),
}

/// One row: `a -> b`, `(a, b, c)`, or a single value for a one-column
/// relation.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct GraphRow {
    /// Its values, one per column.
    pub values: Vec<Expression>,
    /// Where it is.
    pub span: Span,
}

/// `rule name : Relation { clauses }` or `rule name(columns) { clauses }`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RuleDeclaration {
    /// The name.
    pub name: Name,
    /// Its columns.
    pub columns: RuleColumns,
    /// Its clauses.
    pub clauses: Vec<RuleClause>,
    /// Where it is.
    pub span: Span,
}

/// How a rule says what its columns are.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum RuleColumns {
    /// Borrowed from a relation.
    Relation(TypeExpression),
    /// Written in place.
    Inline(Vec<TypedField>),
}

/// `(head) :- body`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RuleClause {
    /// The derived tuple's terms.
    pub head: Vec<Term>,
    /// The conditions.
    pub body: Body,
    /// Where it is.
    pub span: Span,
}

/// A conjunction of literals.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Body {
    /// The literals, in order.
    pub literals: Vec<Literal>,
    /// Where it is.
    pub span: Span,
}

/// One condition of a body.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Literal {
    /// `[!]predicate[@snapshot](terms)`
    Atom(Atom),
    /// `term op term`
    Comparison(Comparison),
}

/// `[!]predicate[@snapshot](terms)`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Atom {
    /// Whether it is negated.
    pub is_negated: bool,
    /// The graph, rule or relation.
    pub predicate: ModuleName,
    /// The snapshot it reads, if written.
    pub snapshot: Option<Snapshot>,
    /// Its arguments.
    pub arguments: AtomArguments,
    /// Where it is.
    pub span: Span,
}

/// An atom's arguments: positional, or all named.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum AtomArguments {
    /// One term per column, in order.
    Positional(Vec<Term>),
    /// Terms for some columns, by name.
    Named(Vec<NamedTerm>),
}

/// `column: term`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct NamedTerm {
    /// The column.
    pub name: Name,
    /// Its term.
    pub term: Term,
}

/// `left op right`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Comparison {
    /// The left term.
    pub left: Term,
    /// The comparison.
    pub operator: BinaryOperator,
    /// The right term.
    pub right: Term,
    /// Where it is.
    pub span: Span,
}

/// A rule term.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Term {
    /// What it is.
    pub kind: TermKind,
    /// Where it is.
    pub span: Span,
}

/// What a rule term is. Spelling decides: a lowercase bare name is a
/// variable, an uppercase or `self` path a constant.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum TermKind {
    /// A variable.
    Variable(String),
    /// `variable.field`.
    Field {
        /// The variable.
        variable: Name,
        /// The field.
        field: Name,
    },
    /// `_`
    Wildcard,
    /// A string.
    Text(String),
    /// An integer.
    Integer(i64),
    /// `true` or `false`.
    Boolean(bool),
    /// `null`
    Null,
    /// A declared constant.
    Constant(ConstantPath),
}

/// `Name.member["key"]` or `self.name`: a constant in a rule term.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ConstantPath {
    /// The first name; `None` for `self`.
    pub root: Option<Name>,
    /// What follows it.
    pub steps: Vec<PathStep>,
}

/// One step of a path.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum PathStep {
    /// `.name`
    Member(Name),
    /// `["key"]`
    Key(Name),
}

/// `@base` or `@head`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Snapshot {
    /// The change's base.
    Base,
    /// The change's head.
    Head,
}

impl Snapshot {
    /// Its name, without `@`.
    pub fn as_str(self) -> &'static str {
        match self {
            Snapshot::Base => "base",
            Snapshot::Head => "head",
        }
    }
}

/// `require name { when: body, ... }`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RequireDeclaration {
    /// The name.
    pub name: Name,
    /// Its `when` body.
    pub when: Option<Body>,
    /// Its other fields: `at`, `message`, `severity`.
    pub fields: Vec<Field>,
    /// Where it is.
    pub span: Span,
}

/// `allow requirement { when: body, ... }`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AllowDeclaration {
    /// The requirement it covers.
    pub target: ModuleName,
    /// Which findings it covers.
    pub when: Option<Body>,
    /// Its other fields: `reason`, `until`.
    pub fields: Vec<Field>,
    /// Where it is.
    pub span: Span,
}

/// `behavior name[(parameters)] { statements }`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct BehaviorDeclaration {
    /// The name.
    pub name: Name,
    /// Its parameters, if it takes any.
    pub parameters: Option<Vec<TypedField>>,
    /// Its body.
    pub body: Block,
    /// Where it is.
    pub span: Span,
}

/// `fn name(parameters): Type { statements }`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FunctionDeclaration {
    /// The name.
    pub name: Name,
    /// Its parameters.
    pub parameters: Vec<TypedField>,
    /// What it returns.
    pub returns: TypeExpression,
    /// Its body.
    pub body: Block,
    /// Where it is.
    pub span: Span,
}

/// `checker name(parameters): Type { fields }`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CheckerDeclaration {
    /// The name.
    pub name: Name,
    /// Its parameters.
    pub parameters: Vec<TypedField>,
    /// The evidence it produces.
    pub returns: TypeExpression,
    /// Its fields: `command`, `directory`, `reads`, ...
    pub fields: Vec<Field>,
    /// Where it is.
    pub span: Span,
}

/// `check name { run ...; skip ... }`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CheckDeclaration {
    /// The name.
    pub name: Name,
    /// Its selections, in order.
    pub members: Vec<CheckMember>,
    /// Where it is.
    pub span: Span,
}

/// One line of a check.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum CheckMember {
    /// `run target[.*][(arguments)][{ overrides }]`
    Run(RunSelection),
    /// `skip target`
    Skip(ModuleName),
}

/// `run target[.*][(arguments)][{ overrides }]`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RunSelection {
    /// The requirement or module.
    pub target: ModuleName,
    /// Whether it ends in `.*`, selecting a module's requirements.
    pub is_wildcard: bool,
    /// Arguments for a parameterised behavior.
    pub arguments: Option<Vec<Argument>>,
    /// Fields overriding the requirement's, such as `severity`.
    pub overrides: Vec<Field>,
    /// Where it is.
    pub span: Span,
}

/// `{ statements }`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Block {
    /// The statements.
    pub statements: Vec<Statement>,
    /// Where it is.
    pub span: Span,
}

/// One statement of a behavior or function.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Statement {
    /// `let name = value`
    Binding(Binding),
    /// `name: value`, allowed at the start of a behavior.
    Field(Field),
    /// `expect condition [because "reason"]`
    Expect {
        /// The condition.
        condition: Expression,
        /// The reason, if given.
        because: Option<Expression>,
        /// Where it is.
        span: Span,
    },
    /// `return value`
    Return {
        /// The value.
        value: Expression,
        /// Where it is.
        span: Span,
    },
    /// `if ... { } else { }`
    If(IfStatement),
    /// `for pattern in domain { }`
    For {
        /// The names each iteration binds.
        pattern: Vec<Name>,
        /// What it iterates over.
        domain: Expression,
        /// The loop body.
        body: Block,
        /// Where it is.
        span: Span,
    },
}

/// `if condition { } [else ...]`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct IfStatement {
    /// The condition.
    pub condition: Expression,
    /// What runs when it holds.
    pub then: Block,
    /// What runs otherwise.
    pub otherwise: Option<Else>,
    /// Where it is.
    pub span: Span,
}

/// The `else` of an `if`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Else {
    /// `else { }`
    Block(Block),
    /// `else if ...`
    If(Box<IfStatement>),
}

/// `name: Type`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TypedField {
    /// The name.
    pub name: Name,
    /// Its type.
    pub field_type: TypeExpression,
    /// Where it is.
    pub span: Span,
}

/// `name: value`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Field {
    /// The name.
    pub name: Name,
    /// Its value.
    pub value: Expression,
    /// Where it is.
    pub span: Span,
}

/// A written type.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum TypeExpression {
    /// `Name` or `Name<Arguments>`.
    Named {
        /// The name.
        name: Name,
        /// Its type arguments.
        arguments: Vec<TypeExpression>,
        /// Where it is.
        span: Span,
    },
    /// `{ field: Type ... }`.
    Record {
        /// The fields.
        fields: Vec<TypedField>,
        /// Where it is.
        span: Span,
    },
}

impl TypeExpression {
    /// Where it is.
    pub fn span(&self) -> Span {
        match self {
            TypeExpression::Named { span, .. } | TypeExpression::Record { span, .. } => *span,
        }
    }
}

/// An expression.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Expression {
    /// What it is.
    pub kind: ExpressionKind,
    /// Where it is.
    pub span: Span,
}

/// What an expression is.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ExpressionKind {
    /// An integer.
    Integer(i64),
    /// A string, decoded.
    Text(String),
    /// `true` or `false`.
    Boolean(bool),
    /// `null`
    Null,
    /// `self`
    SelfModule,
    /// A name.
    Name(String),
    /// `[a, b]`
    List(Vec<Expression>),
    /// `{ name: value ... }`
    Record(Vec<Field>),
    /// `target.name`
    Member {
        /// What it is read from.
        target: Box<Expression>,
        /// The member.
        name: Name,
    },
    /// `target@base`
    Snapshot {
        /// What is read.
        target: Box<Expression>,
        /// In which snapshot.
        snapshot: Snapshot,
    },
    /// `target[keys]`
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
        /// The arguments.
        arguments: Vec<Argument>,
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
    /// Where an expression should be but could not be parsed.
    Missing,
}

/// `[name:] value` in a call.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Argument {
    /// The parameter's name, for a named argument.
    pub name: Option<Name>,
    /// The value.
    pub value: Expression,
}

/// A prefix operator.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum UnaryOperator {
    /// `!`
    Not,
    /// `-`
    Negate,
    /// `need`
    Need,
}

/// An infix operator.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum BinaryOperator {
    /// `||`
    Or,
    /// `&&`
    And,
    /// `==`
    Equal,
    /// `!=`
    NotEqual,
    /// `<`
    Less,
    /// `<=`
    LessOrEqual,
    /// `>`
    Greater,
    /// `>=`
    GreaterOrEqual,
    /// `+`
    Add,
    /// `-`
    Subtract,
    /// `*`
    Multiply,
    /// `/`
    Divide,
    /// `%`
    Remainder,
}

impl BinaryOperator {
    /// How it is written.
    pub fn as_str(self) -> &'static str {
        match self {
            BinaryOperator::Or => "||",
            BinaryOperator::And => "&&",
            BinaryOperator::Equal => "==",
            BinaryOperator::NotEqual => "!=",
            BinaryOperator::Less => "<",
            BinaryOperator::LessOrEqual => "<=",
            BinaryOperator::Greater => ">",
            BinaryOperator::GreaterOrEqual => ">=",
            BinaryOperator::Add => "+",
            BinaryOperator::Subtract => "-",
            BinaryOperator::Multiply => "*",
            BinaryOperator::Divide => "/",
            BinaryOperator::Remainder => "%",
        }
    }

    /// Whether it compares two values.
    pub fn is_comparison(self) -> bool {
        matches!(
            self,
            BinaryOperator::Equal
                | BinaryOperator::NotEqual
                | BinaryOperator::Less
                | BinaryOperator::LessOrEqual
                | BinaryOperator::Greater
                | BinaryOperator::GreaterOrEqual
        )
    }
}
