//! Lowering the statements of behaviors and functions into the model, once
//! checking has resolved their names: each expression becomes a value known
//! without running anything, a local, the facts of a relation, or something
//! to call, so evaluating them needs no scopes.

use super::{Checker, NodeRef};
use crate::model::{self, CallArgument, Callable, Predicate};
use crate::span::Span;
use crate::syntax::ast::{self, Block, Declaration as Node, Else, ExpressionKind, Statement};
use crate::types::Value;

/// Methods evaluated when a behavior runs, on values only evidence gives.
const RUNTIME_METHODS: [&str; 1] = ["has"];

impl Checker<'_> {
    /// A block's statements, its leading fields left out.
    pub(super) fn lower_block(&self, module: &str, block: &Block) -> Vec<model::Statement> {
        block
            .statements
            .iter()
            .filter_map(|statement| self.lower_statement(module, statement))
            .collect()
    }

    fn lower_statement(&self, module: &str, statement: &Statement) -> Option<model::Statement> {
        let lowered = match statement {
            Statement::Field(_) => return None,
            Statement::Binding(binding) => model::Statement::Let {
                name: binding.name.text.clone(),
                value: self.lower_expression(module, &binding.value),
            },
            Statement::Expect {
                condition,
                because,
                span,
            } => model::Statement::Expect {
                condition: self.lower_expression(module, condition),
                because: because
                    .as_ref()
                    .map(|reason| self.lower_expression(module, reason)),
                span: *span,
                text: self.source_text(module, condition.span),
            },
            Statement::Return { value, .. } => {
                model::Statement::Return(self.lower_expression(module, value))
            }
            Statement::If(statement) => self.lower_if(module, statement),
            Statement::For {
                pattern,
                domain,
                body,
                span,
            } => model::Statement::For {
                pattern: pattern.iter().map(|name| name.text.clone()).collect(),
                domain: self.lower_expression(module, domain),
                body: self.lower_block(module, body),
                span: *span,
            },
        };
        Some(lowered)
    }

    fn lower_if(&self, module: &str, statement: &ast::IfStatement) -> model::Statement {
        let otherwise = match &statement.otherwise {
            Some(Else::Block(block)) => self.lower_block(module, block),
            Some(Else::If(nested)) => vec![self.lower_if(module, nested)],
            None => Vec::new(),
        };
        model::Statement::If {
            condition: self.lower_expression(module, &statement.condition),
            then: self.lower_block(module, &statement.then),
            otherwise,
            span: statement.span,
        }
    }

    /// An expression with its names resolved.
    pub(super) fn lower_expression(
        &self,
        module: &str,
        expression: &ast::Expression,
    ) -> model::Expression {
        let span = expression.span;
        if let Some(value) = self.constants.get(&(module.to_owned(), span)) {
            return model::Expression {
                kind: model::ExpressionKind::Constant(value.clone()),
                span,
            };
        }
        if let Some(value) = literal(&expression.kind) {
            return model::Expression {
                kind: model::ExpressionKind::Constant(value),
                span,
            };
        }
        let kind = self.lower_kind(module, expression);
        model::Expression { kind, span }
    }

    fn lower_kind(&self, module: &str, expression: &ast::Expression) -> model::ExpressionKind {
        let span = expression.span;
        match &expression.kind {
            ExpressionKind::Name(name) => self
                .declared_kind(module, span)
                .unwrap_or_else(|| model::ExpressionKind::Local(name.clone())),
            ExpressionKind::List(items) => model::ExpressionKind::List(
                items
                    .iter()
                    .map(|item| self.lower_expression(module, item))
                    .collect(),
            ),
            ExpressionKind::Record(fields) => model::ExpressionKind::Record(
                fields
                    .iter()
                    .map(|field| {
                        let value = self.lower_expression(module, &field.value);
                        (field.name.text.clone(), value)
                    })
                    .collect(),
            ),
            ExpressionKind::Member { target, name } => self
                .declared_kind(module, name.span)
                .unwrap_or_else(|| model::ExpressionKind::Member {
                    target: Box::new(self.lower_expression(module, target)),
                    name: name.text.clone(),
                }),
            ExpressionKind::Snapshot { target, snapshot } => model::ExpressionKind::Snapshot {
                target: Box::new(self.lower_expression(module, target)),
                snapshot: *snapshot,
            },
            ExpressionKind::Index { target, keys } => model::ExpressionKind::Index {
                target: Box::new(self.lower_expression(module, target)),
                keys: keys
                    .iter()
                    .map(|key| self.lower_expression(module, key))
                    .collect(),
            },
            ExpressionKind::Call { callee, arguments } => {
                self.lower_call(module, callee, arguments)
            }
            ExpressionKind::Unary { operator, operand } => model::ExpressionKind::Unary {
                operator: *operator,
                operand: Box::new(self.lower_expression(module, operand)),
            },
            ExpressionKind::Binary {
                operator,
                left,
                right,
            } => model::ExpressionKind::Binary {
                operator: *operator,
                left: Box::new(self.lower_expression(module, left)),
                right: Box::new(self.lower_expression(module, right)),
            },
            ExpressionKind::Integer(_)
            | ExpressionKind::Text(_)
            | ExpressionKind::Boolean(_)
            | ExpressionKind::Null
            | ExpressionKind::SelfModule
            | ExpressionKind::Missing => model::ExpressionKind::Invalid,
        }
    }

    fn lower_call(
        &self,
        module: &str,
        callee: &ast::Expression,
        arguments: &[ast::Argument],
    ) -> model::ExpressionKind {
        if let ExpressionKind::Member { target, name } = &callee.kind {
            let is_declared = self
                .declared_at
                .contains_key(&(module.to_owned(), name.span));
            if !is_declared && RUNTIME_METHODS.contains(&name.text.as_str()) {
                return model::ExpressionKind::Method {
                    receiver: Box::new(self.lower_expression(module, target)),
                    name: name.text.clone(),
                    arguments: arguments
                        .iter()
                        .map(|argument| self.lower_expression(module, &argument.value))
                        .collect(),
                };
            }
        }
        model::ExpressionKind::Call {
            callee: Box::new(self.lower_expression(module, callee)),
            arguments: self.lower_arguments(module, arguments),
        }
    }

    /// A call's arguments as written, resolved.
    pub(super) fn lower_arguments(
        &self,
        module: &str,
        arguments: &[ast::Argument],
    ) -> Vec<CallArgument> {
        arguments
            .iter()
            .map(|argument| CallArgument {
                name: argument.name.as_ref().map(|name| name.text.clone()),
                value: self.lower_expression(module, &argument.value),
            })
            .collect()
    }

    /// What a name that names a declaration is when a behavior runs; `None`
    /// for a name of a local.
    fn declared_kind(&self, module: &str, span: Span) -> Option<model::ExpressionKind> {
        let address = self.declared_at.get(&(module.to_owned(), span))?;
        let id = *self.by_address.get(address)?;
        let address = address.clone();
        let kind = match self.declarations[id].node {
            NodeRef::Top(Node::Relation(_)) => {
                model::ExpressionKind::Facts(Predicate::Relation(address))
            }
            NodeRef::Top(Node::Graph(_)) => model::ExpressionKind::Facts(Predicate::Graph(address)),
            NodeRef::Top(Node::Rule(_)) => model::ExpressionKind::Facts(Predicate::Rule(address)),
            NodeRef::Top(Node::Function(_)) => {
                model::ExpressionKind::Callable(Callable::Function(address))
            }
            NodeRef::Top(Node::Checker(_)) => {
                model::ExpressionKind::Callable(Callable::Checker(address))
            }
            NodeRef::Top(Node::Entity(_)) => {
                model::ExpressionKind::Callable(Callable::Entity(address))
            }
            // Services, operations and bindings have values, recorded as
            // constants; one without a value was an error.
            _ => model::ExpressionKind::Invalid,
        };
        Some(kind)
    }

    /// The text of `span` in `module`.
    pub(super) fn source_text(&self, module: &str, span: Span) -> String {
        self.loaded
            .modules
            .get(module)
            .and_then(|loaded| loaded.text.get(span.start as usize..span.end as usize))
            .unwrap_or_default()
            .to_owned()
    }
}

/// A literal's value.
fn literal(kind: &ExpressionKind) -> Option<Value> {
    let value = match kind {
        ExpressionKind::Integer(value) => Value::Int(*value),
        ExpressionKind::Text(text) => Value::Text(text.clone()),
        ExpressionKind::Boolean(value) => Value::Bool(*value),
        ExpressionKind::Null => Value::Null,
        _ => return None,
    };
    Some(value)
}

/// The functions and checkers `statements` call, for checking that
/// functions do not call themselves and demand no evidence.
pub(super) fn callables(statements: &[model::Statement]) -> Vec<(Callable, Span)> {
    let mut found = Vec::new();
    for statement in statements {
        statement_callables(statement, &mut found);
    }
    found
}

fn statement_callables(statement: &model::Statement, found: &mut Vec<(Callable, Span)>) {
    match statement {
        model::Statement::Let { value, .. } | model::Statement::Return(value) => {
            expression_callables(value, found);
        }
        model::Statement::Expect {
            condition, because, ..
        } => {
            expression_callables(condition, found);
            if let Some(reason) = because {
                expression_callables(reason, found);
            }
        }
        model::Statement::If {
            condition,
            then,
            otherwise,
            ..
        } => {
            expression_callables(condition, found);
            for nested in then.iter().chain(otherwise) {
                statement_callables(nested, found);
            }
        }
        model::Statement::For { domain, body, .. } => {
            expression_callables(domain, found);
            for nested in body {
                statement_callables(nested, found);
            }
        }
    }
}

fn expression_callables(expression: &model::Expression, found: &mut Vec<(Callable, Span)>) {
    use model::ExpressionKind as Kind;
    match &expression.kind {
        Kind::Callable(callable) => found.push((callable.clone(), expression.span)),
        Kind::List(items) => {
            for item in items {
                expression_callables(item, found);
            }
        }
        Kind::Record(fields) => {
            for (_, value) in fields {
                expression_callables(value, found);
            }
        }
        Kind::Member { target, .. } | Kind::Snapshot { target, .. } => {
            expression_callables(target, found);
        }
        Kind::Index { target, keys } => {
            expression_callables(target, found);
            for key in keys {
                expression_callables(key, found);
            }
        }
        Kind::Call { callee, arguments } => {
            expression_callables(callee, found);
            for argument in arguments {
                expression_callables(&argument.value, found);
            }
        }
        Kind::Method {
            receiver,
            arguments,
            ..
        } => {
            expression_callables(receiver, found);
            for argument in arguments {
                expression_callables(argument, found);
            }
        }
        Kind::Unary { operand, .. } => expression_callables(operand, found),
        Kind::Binary { left, right, .. } => {
            expression_callables(left, found);
            expression_callables(right, found);
        }
        Kind::Constant(_) | Kind::Local(_) | Kind::Facts(_) | Kind::Invalid => {}
    }
}
