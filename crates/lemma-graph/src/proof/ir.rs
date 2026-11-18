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
    /// `name.field = value` or `name["field"] = value`, on a variable whose
    /// value nothing else can see.
    SetField(String, String, Expression),
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
    Member(Box<Self>, String),
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
    Conditional(Box<Self>, Box<Self>, Box<Self>),
    Call(Box<Self>, Vec<Argument>),
    /// `await` of a call; any other awaited value is refused.
    Await(Box<Self>),
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
    /// Whether evaluating the expression may call a function.
    pub(super) fn has_call(&self) -> bool {
        match self {
            Self::Call(..) | Self::Await(_) => true,
            Self::Member(inner, _)
            | Self::OptionalMember(inner, _)
            | Self::Not(inner)
            | Self::Negate(inner)
            | Self::Index(inner, _) => inner.has_call(),
            Self::Binary(_, left, right)
            | Self::Coalesce(left, right)
            | Self::Contains(left, right) => left.has_call() || right.has_call(),
            Self::Conditional(test, then, otherwise) => {
                test.has_call() || then.has_call() || otherwise.has_call()
            }
            Self::Record(fields) => fields.values().any(Self::has_call),
            Self::Sequence(items) | Self::Set(items) => items.iter().any(Self::has_call),
            Self::Template(parts) => parts.iter().any(|part| match part {
                Part::Text(_) => false,
                Part::Value(value) => value.has_call(),
            }),
            Self::Bool(_)
            | Self::Text(_)
            | Self::Integer(_)
            | Self::Null
            | Self::Undefined
            | Self::Name(_) => false,
        }
    }
}
