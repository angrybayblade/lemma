//! The small, shared source representation consumed by symbolic execution.

use std::collections::BTreeMap;

use lemma_lang::syntax::ast::{self, BinaryOperator, UnaryOperator};
use serde::Serialize;

#[derive(Debug, Clone)]
pub(super) struct Function {
    pub parameters: Vec<String>,
    pub body: Vec<Statement>,
}

#[derive(Debug, Clone)]
pub(super) struct Statement {
    pub line: usize,
    pub kind: StatementKind,
}

#[derive(Debug, Clone)]
pub(super) enum StatementKind {
    Bind(String, Expression),
    If(Expression, Vec<Statement>, Vec<Statement>),
    Return(Expression),
    Evaluate(Expression),
}

#[derive(Debug, Clone)]
pub(super) enum Expression {
    Bool(bool),
    Text(String),
    Integer(i64),
    Undefined,
    Name(String),
    Member(Box<Self>, String),
    Record(BTreeMap<String, Self>),
    Not(Box<Self>),
    Binary(BinaryOperator, Box<Self>, Box<Self>),
    Conditional(Box<Self>, Box<Self>, Box<Self>),
    Call(Box<Self>, Vec<Self>),
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
        Kind::Integer(value) => Expression::Integer(*value),
        Kind::Name(name) => Expression::Name(name.clone()),
        Kind::Member { target, name } => {
            Expression::Member(Box::new(lower(target)?), name.text.clone())
        }
        Kind::Unary {
            operator: UnaryOperator::Not,
            operand,
        } => Expression::Not(Box::new(lower(operand)?)),
        Kind::Binary {
            operator,
            left,
            right,
        } => Expression::Binary(*operator, Box::new(lower(left)?), Box::new(lower(right)?)),
        Kind::Record(fields) => Expression::Record(
            fields
                .iter()
                .map(|field| Ok((field.name.text.clone(), lower(&field.value)?)))
                .collect::<Result<_, String>>()?,
        ),
        _ => return Err(format!("unsupported contract expression `{expression:?}`")),
    })
}
