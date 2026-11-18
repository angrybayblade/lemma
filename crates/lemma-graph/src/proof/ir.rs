//! The small, shared source representation consumed by symbolic execution.

use std::collections::BTreeMap;

use lemma_lang::syntax::ast::{self, BinaryOperator, UnaryOperator};
use serde::Serialize;

/// A source language: what decides the few meanings the languages do not
/// share, such as how a value is spelled inside a formatted string.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum Language {
    TypeScript,
    Python,
    Go,
}

impl Language {
    pub(super) fn name(self) -> &'static str {
        match self {
            Self::TypeScript => "typescript",
            Self::Python => "python",
            Self::Go => "go",
        }
    }
}

#[derive(Debug, Clone)]
pub(super) struct Function {
    pub language: Language,
    pub parameters: Vec<Parameter>,
    /// An `async` function: a call it makes to an async function must be
    /// awaited, or the value it reads is a pending promise or coroutine.
    pub is_async: bool,
    /// Decorators of the function and its classes, as written; each needs a
    /// decorator model unless the language defines it to return the function
    /// unchanged.
    pub decorators: Vec<String>,
    /// A JavaScript `function` expression, whose `this` is its own: undefined
    /// when called plainly. An arrow function's `this` is the one around it.
    pub has_own_this: bool,
    pub body: Vec<Statement>,
}

/// A parameter, and the value it takes when a caller leaves it out.
#[derive(Debug, Clone)]
pub(super) struct Parameter {
    pub name: String,
    /// The default, or why it could not be read: that matters only to a
    /// contract that leaves the parameter out.
    pub default: Option<Result<Expression, String>>,
}

/// An argument of a call: positional, or named as Python keyword arguments are.
#[derive(Debug, Clone)]
pub(super) struct Argument {
    pub name: Option<String>,
    pub value: Expression,
}

#[derive(Debug, Clone)]
pub(super) struct Statement {
    pub line: usize,
    pub kind: StatementKind,
}

#[derive(Debug, Clone)]
pub(super) enum StatementKind {
    /// A declaration, or any Python assignment: TypeScript and Go scope it
    /// to its block, Python to the function.
    Bind(Pattern, Expression),
    /// A TypeScript or Go assignment to variables already declared.
    Assign(Pattern, Expression),
    /// `name op= value` on a variable.
    Update(String, BinaryOperator, Expression),
    /// `name.field = value`, on a variable whose value nothing else can see;
    /// in JavaScript also `name["field"] = value`, which means the same.
    SetField(String, String, Expression),
    /// Python's `name["key"] = value`: an entry of a dict, which is not an
    /// attribute of it.
    SetEntry(String, String, Expression),
    If(Expression, Vec<Statement>, Vec<Statement>),
    Return(Expression),
    Evaluate(Expression),
    /// `raise`, `throw` or Go's `panic(...)`: the path ends without a normal
    /// return, whatever the raised value would be.
    Raise(Raised),
    /// `try` with its handlers, `else` and `finally`.
    Try(Box<Try>),
    /// Python's `with`: each context manager, then the body.
    With(Vec<Context>, Vec<Statement>),
    /// Python's `assert`: the path goes on only where the condition holds.
    Assert(Expression),
    /// A loop over the items of a sequence, in order.
    ForEach(Box<ForEach>),
    /// `break`: leaves the innermost loop.
    Break,
    /// `continue`: goes on with the next pass of the innermost loop.
    Continue,
}

/// A loop over a sequence: Python's `for`, JavaScript's `for … of` and Go's
/// `for … range`.
#[derive(Debug, Clone)]
pub(super) struct ForEach {
    /// What each position is bound to, as the first name of a Go `range` is.
    pub index: Option<Pattern>,
    /// What each item is bound to.
    pub item: Option<Pattern>,
    pub iterable: Expression,
    pub body: Vec<Statement>,
    /// Python's `else`: run where the loop ends without a `break`.
    pub otherwise: Vec<Statement>,
}

#[derive(Debug, Clone)]
pub(super) enum Expression {
    Bool(bool),
    Text(String),
    Integer(i64),
    /// Python `None`, JavaScript `null`, Go `nil`.
    Null,
    /// JavaScript `undefined`, and the value of a bare JavaScript `return`.
    Undefined,
    Name(String),
    /// `target.name`; in JavaScript also `target["name"]`, which means the
    /// same.
    Member(Box<Self>, String),
    /// `target[key]`: with a constant text key in Python and Go, where an
    /// entry under a key is not an attribute or a field, and with a computed
    /// key in every language.
    Subscript(Box<Self>, Box<Self>),
    /// JavaScript `a?.b`: undefined where `a` is null or undefined.
    OptionalMember(Box<Self>, String),
    Record(BTreeMap<String, Self>),
    Not(Box<Self>),
    Binary(BinaryOperator, Box<Self>, Box<Self>),
    /// JavaScript `a ?? b`: `b` where `a` is null or undefined.
    Coalesce(Box<Self>, Box<Self>),
    /// Integer negation, `-a`.
    Negate(Box<Self>),
    /// A formatted string: Python's f-string, a JavaScript template literal.
    Template(Vec<Part>),
    /// A tuple, list or array of known length.
    Sequence(Vec<Self>),
    /// A Python set literal: only membership is read from it, since its
    /// order is not the written one.
    Set(Vec<Self>),
    /// An element of a sequence at a constant position.
    Index(Box<Self>, i64),
    /// Python's `needle in haystack`, over a sequence or a text.
    Contains(Box<Self>, Box<Self>),
    /// A function defined where it is used: an arrow function, a function
    /// expression, a lambda, a Go function literal or a nested definition.
    Closure(Box<Function>),
    /// An object literal with spreads: `{...a, b: 1}`, `{**a, "b": 1}`.
    Merge(Vec<Entry>),
    /// `...items` or `*items` as an element of a sequence literal.
    Spread(Box<Self>),
    /// `new Constructor(arguments)`, read under the call model `new Constructor`.
    New(String, Vec<Argument>),
    Conditional(Box<Self>, Box<Self>, Box<Self>),
    Call(Box<Self>, Vec<Argument>),
    /// `await` of a call; any other awaited value is refused.
    Await(Box<Self>),
    /// A Python comprehension: a collection built by `for` and `if` clauses.
    Comprehension(Box<Comprehension>),
}

/// A Python comprehension, such as `[element for item in iterable if test]`.
#[derive(Debug, Clone)]
pub(super) struct Comprehension {
    pub collects: Collects,
    /// What each pass adds: an item, or the value of a dict's entry.
    pub element: Expression,
    /// The `for` and `if` clauses in order. The first is a `for`.
    pub clauses: Vec<Clause>,
}

/// What a comprehension builds from the items its passes add.
#[derive(Debug, Clone)]
pub(super) enum Collects {
    /// A list, in the order of the passes.
    Sequence,
    Set,
    /// A dict, with the key of each entry.
    Record(Expression),
    /// A generator expression: its items are made only when something reads
    /// them, which may be later than where it is written, or never.
    Generator,
}

/// One clause of a comprehension.
#[derive(Debug, Clone)]
pub(super) enum Clause {
    /// `for pattern in iterable`.
    For(Pattern, Expression),
    /// `if test`: the passes go on only where the test holds.
    If(Expression),
}

impl Comprehension {
    /// The expressions of the comprehension, in the order they are written.
    pub(super) fn parts(&self) -> Vec<&Expression> {
        let mut parts = Vec::new();
        if let Collects::Record(key) = &self.collects {
            parts.push(key);
        }
        parts.push(&self.element);
        for clause in &self.clauses {
            match clause {
                Clause::For(_, expression) | Clause::If(expression) => parts.push(expression),
            }
        }
        parts
    }
}

/// What a raise raises, as far as a handler needs to know.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) enum Raised {
    /// An exception class, as written: `raise ValueError(...)`.
    Class(String),
    /// What the language raises itself, such as reading a field of `None`;
    /// its class depends on how the field was read.
    Implicit,
    /// A raised value whose class is not written, such as `raise error`.
    Unknown,
    /// Python's bare `raise`, raising again what a handler caught.
    Again,
}

/// One context manager of a `with`: its expression as written, which names
/// its context model, and what `as` binds.
#[derive(Debug, Clone)]
pub(super) struct Context {
    pub text: String,
    pub value: Expression,
    pub target: Option<Pattern>,
    pub is_async: bool,
}

#[derive(Debug, Clone)]
pub(super) struct Try {
    pub body: Vec<Statement>,
    pub handlers: Vec<Handler>,
    /// Python's `else`: run when the body raised nothing.
    pub otherwise: Vec<Statement>,
    pub finally: Vec<Statement>,
}

impl StatementKind {
    /// The blocks nested in a statement.
    pub(super) fn blocks(&self) -> Vec<&[Statement]> {
        match self {
            Self::If(_, then, otherwise) => vec![then, otherwise],
            Self::Try(statement) => statement.blocks().collect(),
            Self::With(_, body) => vec![body],
            Self::ForEach(statement) => vec![&statement.body, &statement.otherwise],
            _ => Vec::new(),
        }
    }
}

impl Try {
    /// Every block of the statement, in order.
    pub(super) fn blocks(&self) -> impl Iterator<Item = &[Statement]> {
        std::iter::once(self.body.as_slice())
            .chain(self.handlers.iter().map(|handler| handler.body.as_slice()))
            .chain([self.otherwise.as_slice(), self.finally.as_slice()])
    }
}

/// A handler, and the classes it catches; `None` catches everything, as a
/// bare `except:` and a TypeScript `catch` do.
#[derive(Debug, Clone)]
pub(super) struct Handler {
    pub classes: Option<Vec<String>>,
    pub body: Vec<Statement>,
}

/// What a binding names: one variable, or the parts of a sequence or record.
#[derive(Debug, Clone)]
pub(super) enum Pattern {
    Name(String),
    /// Go's blank identifier `_`, which binds nothing.
    Ignore,
    Sequence(Vec<Pattern>),
    /// Fields read from a record, each bound to its own pattern.
    Record(Vec<(String, Pattern)>),
    /// Fields read from a record, and the rest of its fields bound together.
    RecordRest(Vec<(String, Pattern)>, Box<Pattern>),
    /// `...rest` or `*rest` in a sequence pattern: the items the others leave.
    Rest(Box<Pattern>),
}

/// A part of an object literal written with spreads, in order: later parts
/// replace the fields of earlier ones.
#[derive(Debug, Clone)]
pub(super) enum Entry {
    Field(String, Expression),
    Spread(Expression),
}

/// A piece of a formatted string.
#[derive(Debug, Clone)]
pub(super) enum Part {
    Text(String),
    Value(Expression),
}

/// One source-level decision in a witness path.
#[derive(Debug, Clone, Serialize)]
pub(super) struct Branch {
    pub line: usize,
    pub taken: bool,
}

/// Parse contract expressions with the existing `.lm` parser.
pub(super) fn condition(text: &str) -> Result<Expression, String> {
    let source = format!("module contract\nlet condition = ({text})\n");
    let parsed = lemma_lang::syntax::parser::parse(&source);
    if let Some(problem) = parsed.diagnostics.first() {
        return Err(format!(
            "invalid contract expression `{text}`: {}",
            problem.message
        ));
    }
    let [ast::Item::Declaration(declared)] = parsed.file.items.as_slice() else {
        return Err("expected one contract expression".to_owned());
    };
    let ast::Declaration::Binding(binding) = &declared.declaration else {
        return Err("expected one contract expression".to_owned());
    };
    lower(&binding.value)
}

fn lower(expression: &ast::Expression) -> Result<Expression, String> {
    use ast::ExpressionKind as Kind;
    Ok(match &expression.kind {
        Kind::Boolean(value) => Expression::Bool(*value),
        Kind::Text(value) => Expression::Text(value.clone()),
        Kind::Integer(value) if *value < 0 => {
            Expression::Negate(Box::new(Expression::Integer(value.saturating_neg())))
        }
        Kind::Integer(value) => Expression::Integer(*value),
        Kind::Null => Expression::Null,
        Kind::Name(name) => Expression::Name(name.clone()),
        Kind::Member { target, name } => {
            Expression::Member(Box::new(lower(target)?), name.text.clone())
        }
        Kind::Unary {
            operator: UnaryOperator::Not,
            operand,
        } => Expression::Not(Box::new(lower(operand)?)),
        Kind::Unary {
            operator: UnaryOperator::Negate,
            operand,
        } => Expression::Negate(Box::new(lower(operand)?)),
        Kind::Binary {
            operator,
            left,
            right,
        } => Expression::Binary(*operator, Box::new(lower(left)?), Box::new(lower(right)?)),
        Kind::List(items) => {
            Expression::Sequence(items.iter().map(lower).collect::<Result<_, _>>()?)
        }
        Kind::Index { target, keys } => {
            let [index] = keys.as_slice() else {
                return Err("a contract indexes a sequence by one constant".to_owned());
            };
            let position = match &index.kind {
                Kind::Integer(value) => *value,
                Kind::Unary {
                    operator: UnaryOperator::Negate,
                    operand,
                } => match operand.kind {
                    Kind::Integer(value) => -value,
                    _ => return Err("a contract indexes a sequence by a constant".to_owned()),
                },
                _ => return Err("a contract indexes a sequence by a constant".to_owned()),
            };
            Expression::Index(Box::new(lower(target)?), position)
        }
        Kind::Record(fields) => Expression::Record(
            fields
                .iter()
                .map(|field| Ok((field.name.text.clone(), lower(&field.value)?)))
                .collect::<Result<_, String>>()?,
        ),
        _ => return Err(format!("unsupported contract expression `{expression:?}`")),
    })
}

impl Expression {
    /// The expressions directly inside this one, not counting the body of a
    /// closure, which runs only when called.
    pub(super) fn parts(&self) -> Vec<&Self> {
        match self {
            Self::Member(inner, _)
            | Self::OptionalMember(inner, _)
            | Self::Not(inner)
            | Self::Negate(inner)
            | Self::Spread(inner)
            | Self::Await(inner)
            | Self::Index(inner, _) => vec![inner],
            Self::Binary(_, left, right)
            | Self::Coalesce(left, right)
            | Self::Subscript(left, right)
            | Self::Contains(left, right) => {
                vec![left, right]
            }
            Self::Conditional(test, then, otherwise) => vec![test, then, otherwise],
            Self::Record(fields) => fields.values().collect(),
            Self::Sequence(items) | Self::Set(items) => items.iter().collect(),
            Self::Template(parts) => parts
                .iter()
                .filter_map(|part| match part {
                    Part::Text(_) => None,
                    Part::Value(value) => Some(value),
                })
                .collect(),
            Self::Merge(entries) => entries
                .iter()
                .map(|entry| match entry {
                    Entry::Field(_, value) | Entry::Spread(value) => value,
                })
                .collect(),
            Self::Call(callee, arguments) => std::iter::once(callee.as_ref())
                .chain(arguments.iter().map(|argument| &argument.value))
                .collect(),
            Self::New(_, arguments) => arguments.iter().map(|argument| &argument.value).collect(),
            Self::Comprehension(comprehension) => comprehension.parts(),
            Self::Bool(_)
            | Self::Text(_)
            | Self::Integer(_)
            | Self::Null
            | Self::Undefined
            | Self::Name(_)
            | Self::Closure(_) => Vec::new(),
        }
    }

    /// Whether evaluating the expression may call a function.
    pub(super) fn has_call(&self) -> bool {
        matches!(self, Self::Call(..) | Self::Await(_) | Self::New(..))
            || self.parts().into_iter().any(Self::has_call)
    }
}
