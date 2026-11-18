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
    Bind(Pattern, Expression),
    If(Expression, Vec<Statement>, Vec<Statement>),
    Return(Expression),
    Evaluate(Expression),
    /// `raise`, `throw` or Go's `panic(...)`: the path ends without a normal
    /// return, whatever the raised value would be.
    Raise,
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
    /// An element of a sequence at a constant position.
    Index(Box<Self>, i64),
    /// Python's `needle in haystack`, over a sequence or a text.
    Contains(Box<Self>, Box<Self>),
    Conditional(Box<Self>, Box<Self>, Box<Self>),
    Call(Box<Self>, Vec<Argument>),
    /// `await` of a call; any other awaited value is refused.
    Await(Box<Self>),
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
