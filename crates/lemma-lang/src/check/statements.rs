//! Checking behaviors, functions and checkers: their parameters, fields and
//! statements.

use std::collections::BTreeMap;

use super::declarations::severity_of;
use super::expressions::{Context, Local};
use super::lower::callables;
use super::{Checker, DeclarationId, signature_text};
use crate::model::{self, Callable, Column};
use crate::stdlib::{self, COMMAND};
use crate::syntax::ast::{self, Block, Else, Statement};
use crate::types::{Address, RecordField, Type, Value};

impl Checker<'_> {
    pub(super) fn check_behavior(
        &mut self,
        id: DeclarationId,
        behavior: &ast::BehaviorDeclaration,
    ) -> String {
        let module = self.declarations[id].module;
        let address = self.declarations[id].address.clone();
        let mut context = Context::new(module);
        context.is_behavior = true;
        context.records = true;
        let parameters = behavior.parameters.as_deref().unwrap_or_default();
        let columns = self.bind_parameters(&mut context, parameters);
        let fields = leading_fields(&behavior.body);
        let schema = [
            optional("severity", stdlib::SEVERITY),
            optional("allow_empty", Type::Bool),
        ];
        let values = self.check_fields(&context, &fields, &schema, "a behavior");
        let allows_empty = matches!(values.get("allow_empty"), Some(Value::Bool(true)));
        self.case_variables.clear();
        let expects = self.block(&mut context, &behavior.body, None);
        if expects == 0 && !allows_empty {
            self.warning(
                module,
                self.declarations[id].name_span,
                "this behavior asserts nothing, so it is always unknown; add `expect` or `allow_empty: true`",
            );
        }
        let mut variables: Vec<(String, Type)> = columns
            .iter()
            .map(|column| (column.name.clone(), column.column_type.clone()))
            .collect();
        for (name, variable_type) in std::mem::take(&mut self.case_variables) {
            if !variables.iter().any(|(known, _)| *known == name) {
                variables.push((name, variable_type));
            }
        }
        self.requirement_variables
            .insert(address.clone(), variables);
        let body = self.lower_block(module, &behavior.body);
        self.model.behaviors.push(model::Behavior {
            address,
            parameters: columns,
            severity: values
                .get("severity")
                .and_then(severity_of)
                .unwrap_or_default(),
            allows_empty,
            body,
            text: self.source_text(module, behavior.span),
        });
        "behavior".to_owned()
    }

    pub(super) fn check_function(
        &mut self,
        id: DeclarationId,
        function: &ast::FunctionDeclaration,
    ) -> String {
        let module = self.declarations[id].module;
        let mut context = Context::new(module);
        context.records = true;
        let columns = self.bind_parameters(&mut context, &function.parameters);
        let returns = self.resolve_type(module, &function.returns);
        self.block(&mut context, &function.body, Some(&returns));
        let body = self.lower_block(module, &function.body);
        self.model.functions.push(model::Function {
            address: self.declarations[id].address.clone(),
            parameters: columns,
            body,
        });
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
        let values = self.check_fields(&context, &fields, &schema, "a checker");
        self.add_checker(id, checker, &values, &returns);
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

    /// Puts `parameters` in scope; returns them as columns.
    fn bind_parameters(
        &mut self,
        context: &mut Context,
        parameters: &[ast::TypedField],
    ) -> Vec<Column> {
        let mut columns = Vec::new();
        for parameter in parameters {
            let parameter_type = self.resolve_type(context.module, &parameter.field_type);
            context.locals.push(Local {
                name: parameter.name.text.clone(),
                value_type: parameter_type.clone(),
                value: None,
                span: parameter.name.span,
            });
            columns.push(Column {
                name: parameter.name.text.clone(),
                column_type: parameter_type,
            });
        }
        columns
    }

    /// Adds a checker whose fields are known to the model.
    fn add_checker(
        &mut self,
        id: DeclarationId,
        checker: &ast::CheckerDeclaration,
        values: &BTreeMap<String, Value>,
        returns: &Type,
    ) {
        let module = self.declarations[id].module;
        let parameters = self.bind_parameters(&mut Context::new(module), &checker.parameters);
        let observes = match returns {
            Type::Evidence(inner) => (**inner).clone(),
            _ => Type::Unknown,
        };
        let command = match values.get("command") {
            Some(Value::List(items)) => texts(items),
            Some(Value::Text(_)) => {
                let span = field_span(checker, "command");
                self.error(
                    module,
                    span,
                    "a checker's command is an argument list, run without a shell: `[\"go\", \"run\", \"./tool\"]`",
                );
                return;
            }
            _ => return,
        };
        let repository = self
            .loaded
            .modules
            .get(module)
            .map(|loaded| loaded.repository.clone())
            .unwrap_or_default();
        let env = environment(values.get("env"));
        self.model.checkers.push(model::Checker {
            address: self.declarations[id].address.clone(),
            repository,
            parameters,
            observes,
            command,
            directory: values
                .get("directory")
                .and_then(Value::as_text)
                .map(str::to_owned),
            setup: match values.get("setup") {
                Some(Value::List(items)) => Some(texts(items)),
                Some(Value::Text(text)) => {
                    Some(vec!["sh".to_owned(), "-c".to_owned(), text.clone()])
                }
                _ => None,
            },
            reads: match values.get("reads") {
                Some(Value::List(items)) => texts(items),
                _ => Vec::new(),
            },
            env,
            timeout_ms: match values.get("timeout_ms") {
                Some(Value::Int(milliseconds)) => u64::try_from(*milliseconds).ok(),
                _ => None,
            },
            is_cached: !matches!(values.get("cache"), Some(Value::Bool(false))),
            text: self.source_text(module, checker.span),
        });
    }

    /// Reports functions that call themselves, directly or not, and
    /// functions that call checkers: a function is pure and finite.
    pub(super) fn check_function_calls(&mut self) {
        let calls: BTreeMap<Address, Vec<(Callable, crate::Span)>> = self
            .model
            .functions
            .iter()
            .map(|function| (function.address.clone(), callables(&function.body)))
            .collect();
        let problems: Vec<(String, crate::Span, String)> = calls
            .iter()
            .flat_map(|(function, called)| {
                called.iter().filter_map(|(callable, span)| {
                    let message = call_problem(&calls, function, callable)?;
                    Some((function.module.clone(), *span, message))
                })
            })
            .collect();
        for (module, span, message) in problems {
            self.error(&module, span, message);
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
            if context.is_behavior {
                self.case_variables
                    .push((name.text.clone(), item_type.clone()));
            }
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

/// A checker's `env` record as variables.
fn environment(written: Option<&Value>) -> BTreeMap<String, String> {
    let Some(Value::Record(fields)) = written else {
        return BTreeMap::new();
    };
    fields
        .iter()
        .filter_map(|(name, value)| Some((name.clone(), value.as_text()?.to_owned())))
        .collect()
}

fn texts(items: &[Value]) -> Vec<String> {
    items
        .iter()
        .filter_map(|item| item.as_text().map(str::to_owned))
        .collect()
}

fn field_span(checker: &ast::CheckerDeclaration, name: &str) -> crate::Span {
    checker
        .fields
        .iter()
        .find(|field| field.name.text == name)
        .map_or(checker.name.span, |field| field.value.span)
}

/// What is wrong with `function` calling `callable`, if anything.
fn call_problem(
    calls: &BTreeMap<Address, Vec<(Callable, crate::Span)>>,
    function: &Address,
    callable: &Callable,
) -> Option<String> {
    match callable {
        Callable::Checker(_) => {
            Some("a function cannot demand evidence; call checkers from a behavior".to_owned())
        }
        Callable::Function(target) if calls_back(calls, target, function) => Some(format!(
            "`{}` calls itself, directly or not; recursion belongs in a `rule`",
            function.name()
        )),
        _ => None,
    }
}

/// Whether `from` calls `target`, directly or through other functions.
fn calls_back(
    calls: &BTreeMap<Address, Vec<(Callable, crate::Span)>>,
    from: &Address,
    target: &Address,
) -> bool {
    let mut stack = vec![from.clone()];
    let mut seen = std::collections::BTreeSet::new();
    while let Some(function) = stack.pop() {
        if function == *target {
            return true;
        }
        if !seen.insert(function.clone()) {
            continue;
        }
        let called = calls.get(&function).into_iter().flatten();
        stack.extend(called.filter_map(|(callable, _)| match callable {
            Callable::Function(next) => Some(next.clone()),
            _ => None,
        }));
    }
    false
}
