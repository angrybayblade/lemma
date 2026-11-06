//! Checking behaviors, functions and checkers: their parameters, fields and
//! statements.

use super::expressions::{Context, Local};
use super::{Checker, DeclarationId, signature_text};
use crate::stdlib::{self, COMMAND};
use crate::syntax::ast::{self, Block, Else, Statement};
use crate::types::{RecordField, Type};

impl Checker<'_> {
    pub(super) fn check_behavior(
        &mut self,
        id: DeclarationId,
        behavior: &ast::BehaviorDeclaration,
    ) -> String {
        let module = self.declarations[id].module;
        let mut context = Context::new(module);
        context.is_behavior = true;
        let parameters = behavior.parameters.as_deref().unwrap_or_default();
        self.bind_parameters(&mut context, parameters);
        let fields = leading_fields(&behavior.body);
        let schema = [
            optional("severity", stdlib::SEVERITY),
            optional("allow_empty", Type::Bool),
        ];
        let values = self.check_fields(&context, &fields, &schema, "a behavior");
        let allows_empty = matches!(
            values.get("allow_empty"),
            Some(crate::types::Value::Bool(true))
        );
        let expects = self.block(&mut context, &behavior.body, None);
        if expects == 0 && !allows_empty {
            self.warning(
                module,
                self.declarations[id].name_span,
                "this behavior asserts nothing, so it is always unknown; add `expect` or `allow_empty: true`",
            );
        }
        "behavior".to_owned()
    }

    pub(super) fn check_function(
        &mut self,
        id: DeclarationId,
        function: &ast::FunctionDeclaration,
    ) -> String {
        let module = self.declarations[id].module;
        let mut context = Context::new(module);
        self.bind_parameters(&mut context, &function.parameters);
        let returns = self.resolve_type(module, &function.returns);
        self.block(&mut context, &function.body, Some(&returns));
        let signature = self.function_signature(module, &function.parameters, &function.returns);
        signature_text(&Type::Function(Box::new(signature)))
    }

    pub(super) fn check_checker(
        &mut self,
        id: DeclarationId,
        checker: &ast::CheckerDeclaration,
    ) -> String {
        let module = self.declarations[id].module;
        let mut context = Context::new(module);
        self.bind_parameters(&mut context, &checker.parameters);
        let returns = self.resolve_type(module, &checker.returns);
        if !matches!(returns, Type::Evidence(_) | Type::Unknown) {
            self.error(
                module,
                checker.returns.span(),
                format!("a checker produces `Evidence<...>`, not `{returns}`"),
            );
        }
        let mut command = optional("command", COMMAND);
        command.is_optional = false;
        let schema = [
            command,
            optional("directory", Type::Text),
            optional("setup", COMMAND),
            optional("reads", Type::List(Box::new(Type::Text))),
            optional("env", Type::Json),
            optional("timeout_ms", Type::Int),
            optional("cache", Type::Bool),
        ];
        let fields: Vec<&ast::Field> = checker.fields.iter().collect();
        self.check_fields(&context, &fields, &schema, "a checker");
        if !checker
            .fields
            .iter()
            .any(|field| field.name.text == "command")
        {
            self.error(
                module,
                self.declarations[id].name_span,
                "a checker needs `command`, the argument list that runs it",
            );
        }
        let signature = self.function_signature(module, &checker.parameters, &checker.returns);
        signature_text(&Type::Function(Box::new(signature)))
    }

    fn bind_parameters(&mut self, context: &mut Context, parameters: &[ast::TypedField]) {
        for parameter in parameters {
            let parameter_type = self.resolve_type(context.module, &parameter.field_type);
            context.locals.push(Local {
                name: parameter.name.text.clone(),
                value_type: parameter_type,
                value: None,
                span: parameter.name.span,
            });
        }
    }

    /// Checks a block's statements; returns how many `expect`s it holds.
    fn block(&mut self, context: &mut Context, block: &Block, returns: Option<&Type>) -> usize {
        let depth = context.locals.len();
        let mut expects = 0;
        let mut past_fields = false;
        for statement in &block.statements {
            let is_misplaced = past_fields || !context.is_behavior;
            match statement {
                Statement::Field(field) if is_misplaced => self.error(
                    context.module,
                    field.name.span,
                    "fields come first in a behavior's body, and only there",
                ),
                Statement::Field(_) => {}
                _ => past_fields = true,
            }
            expects += self.statement(context, statement, returns);
        }
        context.locals.truncate(depth);
        expects
    }

    fn statement(
        &mut self,
        context: &mut Context,
        statement: &Statement,
        returns: Option<&Type>,
    ) -> usize {
        match statement {
            Statement::Field(_) => 0,
            Statement::Binding(binding) => {
                let evaluated = match &binding.declared_type {
                    Some(written) => {
                        let expected = self.resolve_type(context.module, written);
                        let value = self.expect_value(context, &binding.value, &expected);
                        (expected, value)
                    }
                    None => self.evaluate(context, &binding.value),
                };
                context.locals.push(Local {
                    name: binding.name.text.clone(),
                    value_type: evaluated.0,
                    value: evaluated.1,
                    span: binding.name.span,
                });
                0
            }
            Statement::Expect {
                condition,
                because,
                span,
            } => {
                if !context.is_behavior {
                    self.error(
                        context.module,
                        *span,
                        "`expect` asserts an obligation, which only a behavior has",
                    );
                }
                self.condition(context, condition);
                if let Some(reason) = because {
                    self.expect_value(context, reason, &Type::Text);
                }
                1
            }
            Statement::Return { value, span } => {
                let Some(expected) = returns else {
                    self.error(context.module, *span, "only a function returns a value");
                    return 0;
                };
                self.expect_value(context, value, expected);
                0
            }
            Statement::If(statement) => self.if_statement(context, statement, returns),
            Statement::For {
                pattern,
                domain,
                body,
                ..
            } => {
                let written = Loop {
                    pattern,
                    domain,
                    body,
                };
                self.for_statement(context, written, returns)
            }
        }
    }

    fn condition(&mut self, context: &Context, condition: &ast::Expression) {
        let (found, _) = self.evaluate(context, condition);
        if !matches!(found, Type::Bool | Type::Json | Type::Unknown) {
            self.error(
                context.module,
                condition.span,
                format!("a condition is `Bool`, not `{found}`"),
            );
        }
    }

    fn if_statement(
        &mut self,
        context: &mut Context,
        statement: &ast::IfStatement,
        returns: Option<&Type>,
    ) -> usize {
        self.condition(context, &statement.condition);
        let mut expects = self.block(context, &statement.then, returns);
        match &statement.otherwise {
            Some(Else::Block(block)) => expects += self.block(context, block, returns),
            Some(Else::If(nested)) => expects += self.if_statement(context, nested, returns),
            None => {}
        }
        expects
    }

    fn for_statement(
        &mut self,
        context: &mut Context,
        written: Loop,
        returns: Option<&Type>,
    ) -> usize {
        let Loop {
            pattern,
            domain,
            body,
        } = written;
        let (domain_type, _) = self.evaluate(context, domain);
        let item_types = self.item_types(context, &domain_type, pattern.len(), domain.span);
        let depth = context.locals.len();
        for (name, item_type) in pattern.iter().zip(item_types) {
            context.locals.push(Local {
                name: name.text.clone(),
                value_type: item_type,
                value: None,
                span: name.span,
            });
        }
        let expects = self.block(context, body, returns);
        context.locals.truncate(depth);
        expects
    }

    /// The types a `for` pattern binds over `domain_type`: a list's items,
    /// or a relation's columns.
    fn item_types(
        &mut self,
        context: &Context,
        domain_type: &Type,
        width: usize,
        span: crate::Span,
    ) -> Vec<Type> {
        match domain_type {
            Type::List(item) if width == 1 => vec![(**item).clone()],
            Type::Relation(relation) | Type::Graph(relation) => {
                let columns = self
                    .by_address
                    .get(relation)
                    .copied()
                    .and_then(|id| self.columns(id))
                    .unwrap_or_default();
                if columns.len() != width {
                    self.error(
                        context.module,
                        span,
                        format!(
                            "`{}` has {} columns; the pattern names {width}",
                            relation.name(),
                            columns.len()
                        ),
                    );
                }
                columns
                    .into_iter()
                    .map(|column| column.column_type)
                    .collect()
            }
            Type::Json | Type::Unknown => vec![Type::Unknown; width],
            other => {
                self.error(
                    context.module,
                    span,
                    format!("`for` iterates a list or a relation, not `{other}`"),
                );
                vec![Type::Unknown; width]
            }
        }
    }
}

/// A `for` loop's parts.
#[derive(Clone, Copy)]
struct Loop<'s> {
    pattern: &'s [ast::Name],
    domain: &'s ast::Expression,
    body: &'s Block,
}

/// The fields a behavior's body starts with.
fn leading_fields(body: &Block) -> Vec<&ast::Field> {
    body.statements
        .iter()
        .map_while(|statement| match statement {
            Statement::Field(field) => Some(field),
            _ => None,
        })
        .collect()
}

fn optional(name: &str, field_type: Type) -> RecordField {
    RecordField {
        name: name.to_owned(),
        field_type,
        is_optional: true,
    }
}
