//! Typing and evaluating expressions.
//!
//! Every expression gets a type. Those that can be known without running
//! anything (literals, declarations, provider and resolution requests) also
//! get a [`Value`], which lowering reads.

use super::{Checker, DeclarationId, Entry, NodeRef, Target, builtin_target, signature_text};
use crate::span::Span;
use crate::stdlib::{self, Builtin};
use crate::syntax::ast::{
    Argument, BinaryOperator, Declaration as Node, Expression, ExpressionKind, UnaryOperator,
};
use crate::types::{Address, Parameter, RecordField, Signature, Type, Value};

/// A local variable or parameter.
#[derive(Debug, Clone)]
pub(crate) struct Local {
    pub name: String,
    pub value_type: Type,
    pub value: Option<Value>,
    pub span: Span,
}

/// Where an expression is: its module, service, and the locals in scope.
#[derive(Debug, Clone)]
pub(crate) struct Context<'c> {
    pub module: &'c str,
    pub service: Option<DeclarationId>,
    pub locals: Vec<Local>,
    pub is_behavior: bool,
    /// Whether known values are recorded for lowering, as in behaviors and
    /// functions.
    pub records: bool,
    /// Whether this is an end of a link, which may name an operation its
    /// service no longer has.
    pub is_link_end: bool,
}

impl<'c> Context<'c> {
    pub fn new(module: &'c str) -> Self {
        Self {
            module,
            service: None,
            locals: Vec::new(),
            is_behavior: false,
            records: false,
            is_link_end: false,
        }
    }

    pub fn in_service(mut self, service: DeclarationId) -> Self {
        self.service = Some(service);
        self
    }

    fn local(&self, name: &str) -> Option<&Local> {
        self.locals.iter().rev().find(|local| local.name == name)
    }
}

/// A call's arguments and where it is.
#[derive(Clone, Copy)]
pub(crate) struct Call<'e> {
    arguments: &'e [Argument],
    span: Span,
}

/// Which parameter an argument may fill: by its name, by its position, or
/// none, for a positional argument after named ones.
#[derive(Clone, Copy)]
enum ArgumentSlot {
    Named,
    Position(usize),
    AfterNamed,
}

/// A method call's receiver: none for a function, or a value that is known
/// or not.
enum Receiver {
    None,
    Known(Value),
    Unknown,
}

/// A type and, when known, a value.
pub(crate) type Evaluated = (Type, Option<Value>);

impl Checker<'_> {
    /// Evaluates `expression` where a value of type `expected` belongs,
    /// reporting a mismatch. Enumerated fields read a bare name as itself.
    pub(crate) fn expect_value(
        &mut self,
        context: &Context,
        expression: &Expression,
        expected: &Type,
    ) -> Option<Value> {
        if let Some(names) = stdlib::enumeration(expected) {
            return self.enumerated(context, expression, names);
        }
        if let (Type::Record(fields), ExpressionKind::Record(written)) =
            (expected, &expression.kind)
        {
            return Some(self.expect_record(context, written, fields));
        }
        if let (Type::List(item), ExpressionKind::List(items)) = (expected, &expression.kind) {
            let values: Vec<Option<Value>> = items
                .iter()
                .map(|element| self.expect_value(context, element, item))
                .collect();
            return values
                .into_iter()
                .collect::<Option<Vec<Value>>>()
                .map(Value::List);
        }
        let (found, value) = self.evaluate(context, expression);
        if !found.fits(expected) {
            self.error(
                context.module,
                expression.span,
                format!("expected `{expected}`, found `{found}`"),
            );
        }
        value
    }

    fn enumerated(
        &mut self,
        context: &Context,
        expression: &Expression,
        names: &[&str],
    ) -> Option<Value> {
        let written = match &expression.kind {
            ExpressionKind::Name(name) => Some(name.as_str()),
            ExpressionKind::Text(text) => Some(text.as_str()),
            _ => None,
        };
        if let Some(name) = written.filter(|name| names.contains(name)) {
            return Some(Value::Text(name.to_owned()));
        }
        let listed: Vec<String> = names.iter().map(|name| format!("`{name}`")).collect();
        self.error(
            context.module,
            expression.span,
            format!("expected one of {}", listed.join(", ")),
        );
        None
    }

    fn expect_record(
        &mut self,
        context: &Context,
        written: &[crate::syntax::ast::Field],
        fields: &[RecordField],
    ) -> Value {
        let written: Vec<&crate::syntax::ast::Field> = written.iter().collect();
        let values = self.check_fields(context, &written, fields, "this record");
        let ordered = written
            .iter()
            .filter_map(|field| {
                values
                    .get(&field.name.text)
                    .map(|value| (field.name.text.clone(), value.clone()))
            })
            .collect();
        Value::Record(ordered)
    }

    /// The type of `expression` and, when it can be known, its value.
    pub(crate) fn evaluate(&mut self, context: &Context, expression: &Expression) -> Evaluated {
        let evaluated = self.evaluate_kind(context, expression);
        if let (true, Some(value)) = (context.records, &evaluated.1) {
            self.constants
                .insert((context.module.to_owned(), expression.span), value.clone());
        }
        evaluated
    }

    fn evaluate_kind(&mut self, context: &Context, expression: &Expression) -> Evaluated {
        match &expression.kind {
            ExpressionKind::Integer(value) => (Type::Int, Some(Value::Int(*value))),
            ExpressionKind::Text(text) => (Type::Text, Some(Value::Text(text.clone()))),
            ExpressionKind::Boolean(value) => (Type::Bool, Some(Value::Bool(*value))),
            ExpressionKind::Null => (Type::Null, Some(Value::Null)),
            ExpressionKind::SelfModule => (Type::Namespace(context.module.to_owned()), None),
            ExpressionKind::Name(name) => self.name_value(context, name, expression.span),
            ExpressionKind::List(items) => self.list(context, items),
            ExpressionKind::Record(fields) => self.record(context, fields),
            ExpressionKind::Member { target, name } => {
                let evaluated = self.evaluate(context, target);
                self.member(context, evaluated, &name.text, name.span)
            }
            ExpressionKind::Snapshot { target, .. } => self.evaluate(context, target),
            ExpressionKind::Index { target, keys } => {
                self.index(context, target, keys, expression.span)
            }
            ExpressionKind::Call { callee, arguments } => {
                self.call(context, callee, arguments, expression.span)
            }
            ExpressionKind::Unary { operator, operand } => {
                self.unary(context, *operator, operand, expression.span)
            }
            ExpressionKind::Binary {
                operator,
                left,
                right,
            } => self.binary(context, *operator, left, right),
            ExpressionKind::Missing => (Type::Unknown, None),
        }
    }

    fn list(&mut self, context: &Context, items: &[Expression]) -> Evaluated {
        let mut element: Option<Type> = None;
        let mut values = Some(Vec::new());
        for item in items {
            let (item_type, value) = self.evaluate(context, item);
            match &element {
                None => element = Some(item_type),
                Some(first) if !item_type.fits(first) && !first.fits(&item_type) => {
                    self.error(
                        context.module,
                        item.span,
                        format!(
                            "a list holds one type: this is `{item_type}`, the first item `{first}`"
                        ),
                    );
                }
                Some(_) => {}
            }
            match (&mut values, value) {
                (Some(known), Some(value)) => known.push(value),
                _ => values = None,
            }
        }
        let element = element.unwrap_or(Type::Unknown);
        (Type::List(Box::new(element)), values.map(Value::List))
    }

    fn record(&mut self, context: &Context, fields: &[crate::syntax::ast::Field]) -> Evaluated {
        let mut types = Vec::new();
        let mut values = Some(Vec::new());
        for field in fields {
            let (field_type, value) = self.evaluate(context, &field.value);
            types.push(RecordField {
                name: field.name.text.clone(),
                field_type,
                is_optional: false,
            });
            match (&mut values, value) {
                (Some(known), Some(value)) => known.push((field.name.text.clone(), value)),
                _ => values = None,
            }
        }
        (Type::Record(types), values.map(Value::Record))
    }

    fn name_value(&mut self, context: &Context, name: &str, span: Span) -> Evaluated {
        if let Some(local) = context.local(name).cloned() {
            self.refer(
                context.module,
                span,
                Target::Local {
                    definition: local.span,
                    detail: local.value_type.to_string(),
                },
            );
            return (local.value_type, local.value);
        }
        if let Some(id) = self.service_binding(context, name) {
            self.refer(
                context.module,
                span,
                Target::Declaration(self.declarations[id].address.clone()),
            );
            return self.binding_value(id);
        }
        let Some(entry) = self.lookup(context.module, name) else {
            self.error(context.module, span, format!("there is no `{name}` here"));
            return (Type::Unknown, None);
        };
        self.refer_entry(context.module, span, &entry);
        self.entry_value(context.module, &entry, span)
    }

    /// A `let` inside the expression's service or one around it.
    fn service_binding(&self, context: &Context, name: &str) -> Option<DeclarationId> {
        let mut service = context.service;
        while let Some(id) = service {
            let found = self
                .members
                .get(&id)
                .and_then(|members| members.get(name))
                .copied()
                .filter(|member| {
                    matches!(self.declarations[*member].node, NodeRef::ServiceBinding(_))
                });
            if found.is_some() {
                return found;
            }
            service = self.parent_service(&self.declarations[id].address);
        }
        None
    }

    fn entry_value(&mut self, module: &str, entry: &Entry, span: Span) -> Evaluated {
        match entry {
            Entry::Namespace(name) => (Type::Namespace(name.clone()), None),
            Entry::Builtin(index) => (
                Type::Function(Box::new(self.builtins[*index].signature.clone())),
                None,
            ),
            Entry::Declaration(id) => self.declaration_value(module, *id, span),
        }
    }

    fn declaration_value(&mut self, module: &str, id: DeclarationId, span: Span) -> Evaluated {
        let declared = self.declarations[id].clone();
        let address = declared.address.clone();
        match declared.node {
            NodeRef::Top(Node::Binding(_)) | NodeRef::ServiceBinding(_) => self.binding_value(id),
            NodeRef::Top(Node::Service(_)) | NodeRef::Service(_) => {
                (Type::Service, Some(Value::Service(address)))
            }
            NodeRef::Operation(operation) => {
                let operation_type = if operation.is_client {
                    Type::ClientOperation
                } else {
                    Type::Operation
                };
                (operation_type, Some(Value::Operation(address)))
            }
            NodeRef::Top(Node::Relation(_)) => (Type::Relation(address), None),
            NodeRef::Top(Node::Graph(_) | Node::Rule(_)) => {
                let relation = self.graph_relation(id).unwrap_or(address);
                (Type::Graph(relation), None)
            }
            NodeRef::Top(Node::Entity(entity)) => {
                let signature = self.entity_signature(declared.module, entity, &address);
                (Type::Function(Box::new(signature)), None)
            }
            NodeRef::Top(Node::Function(function)) => {
                let signature = self.function_signature(
                    declared.module,
                    &function.parameters,
                    &function.returns,
                );
                (Type::Function(Box::new(signature)), None)
            }
            NodeRef::Top(Node::Checker(checker)) => {
                let signature =
                    self.function_signature(declared.module, &checker.parameters, &checker.returns);
                (Type::Function(Box::new(signature)), None)
            }
            NodeRef::Top(Node::Type(_)) => {
                self.error(
                    module,
                    span,
                    format!("`{}` is a type, not a value", address.name()),
                );
                (Type::Unknown, None)
            }
            NodeRef::Top(
                Node::Require(_) | Node::Behavior(_) | Node::Check(_) | Node::Allow(_),
            ) => {
                self.error(
                    module,
                    span,
                    format!(
                        "`{}` is a requirement or check, not a value",
                        address.name()
                    ),
                );
                (Type::Unknown, None)
            }
        }
    }

    /// The relation a graph or rule holds facts of, for its type.
    fn graph_relation(&mut self, id: DeclarationId) -> Option<Address> {
        let declared = self.declarations[id].clone();
        match declared.node {
            NodeRef::Top(Node::Graph(graph)) => {
                let resolved = self.resolve_type(declared.module, &graph.relation);
                match resolved {
                    Type::Relation(address) => Some(address),
                    _ => None,
                }
            }
            _ => Some(declared.address),
        }
    }

    pub(crate) fn function_signature(
        &mut self,
        module: &str,
        parameters: &[crate::syntax::ast::TypedField],
        returns: &crate::syntax::ast::TypeExpression,
    ) -> Signature {
        Signature {
            parameters: parameters
                .iter()
                .map(|parameter| Parameter {
                    name: parameter.name.text.clone(),
                    parameter_type: self.resolve_type(module, &parameter.field_type),
                    is_optional: false,
                })
                .collect(),
            returns: self.resolve_type(module, returns),
        }
    }

    fn entity_signature(
        &mut self,
        module: &str,
        entity: &crate::syntax::ast::EntityDeclaration,
        address: &Address,
    ) -> Signature {
        Signature {
            parameters: entity
                .fields
                .iter()
                .map(|field| Parameter {
                    name: field.name.text.clone(),
                    parameter_type: self.resolve_type(module, &field.field_type),
                    is_optional: false,
                })
                .collect(),
            returns: Type::Entity(address.clone()),
        }
    }

    /// A `let` binding's type and value, evaluated once; a binding that
    /// depends on itself is reported.
    pub(crate) fn binding_value(&mut self, id: DeclarationId) -> Evaluated {
        if let Some(evaluated) = self.values.get(&id) {
            return evaluated.clone();
        }
        let declared = self.declarations[id].clone();
        let (NodeRef::Top(Node::Binding(binding)) | NodeRef::ServiceBinding(binding)) =
            declared.node
        else {
            return (Type::Unknown, None);
        };
        if !self.evaluating.insert(id) {
            self.error(
                declared.module,
                declared.name_span,
                format!("`{}` depends on itself", binding.name.text),
            );
            return (Type::Unknown, None);
        }
        let mut context = Context::new(declared.module);
        if let NodeRef::ServiceBinding(_) = declared.node {
            context.service = self.parent_service(&declared.address);
        }
        let evaluated = match &binding.declared_type {
            Some(written) => {
                let expected = self.resolve_type(declared.module, written);
                let value = self.expect_value(&context, &binding.value, &expected);
                (expected, value)
            }
            None => self.evaluate(&context, &binding.value),
        };
        self.evaluating.remove(&id);
        self.values.insert(id, evaluated.clone());
        evaluated
    }

    fn member(
        &mut self,
        context: &Context,
        (target_type, target_value): Evaluated,
        name: &str,
        span: Span,
    ) -> Evaluated {
        if let Type::Namespace(module) = &target_type {
            return self.namespace_member(context, module, name, span);
        }
        if let Some(Value::Service(service)) = &target_value {
            return self.service_member(context, service, name, span);
        }
        match &target_type {
            Type::Unknown => (Type::Unknown, None),
            Type::Json => (Type::Json, None),
            Type::Record(fields) => {
                let Some(field) = fields.iter().find(|field| field.name == name) else {
                    self.error(
                        context.module,
                        span,
                        format!("this record has no field `{name}`"),
                    );
                    return (Type::Unknown, None);
                };
                let value = target_value
                    .as_ref()
                    .and_then(|value| value.field(name))
                    .cloned();
                (field.field_type.clone(), value)
            }
            Type::Fact(relation) => self.fact_member(context, relation, name, span),
            Type::Symbol | Type::Site => {
                let field_type = match name {
                    "line" | "column" => Type::Int,
                    _ => Type::Text,
                };
                (field_type, None)
            }
            Type::Builtin(_) if stdlib::method(&target_type, name).is_some() => {
                self.error(
                    context.module,
                    span,
                    format!("`{name}` is a method; call it"),
                );
                (Type::Unknown, None)
            }
            other => {
                self.error(
                    context.module,
                    span,
                    format!("`{other}` has no member `{name}`"),
                );
                (Type::Unknown, None)
            }
        }
    }

    fn namespace_member(
        &mut self,
        context: &Context,
        module: &str,
        name: &str,
        span: Span,
    ) -> Evaluated {
        let entry = if module == context.module {
            self.own
                .get(module)
                .and_then(|names| names.get(name))
                .copied()
                .map(Entry::Declaration)
        } else {
            self.export_of(module, name, &mut std::collections::BTreeSet::new())
        };
        let Some(entry) = entry else {
            self.error(
                context.module,
                span,
                format!("module `{module}` exports no `{name}`"),
            );
            return (Type::Unknown, None);
        };
        self.refer_entry(context.module, span, &entry);
        self.entry_value(context.module, &entry, span)
    }

    fn service_member(
        &mut self,
        context: &Context,
        service: &Address,
        name: &str,
        span: Span,
    ) -> Evaluated {
        let Some(&service_id) = self.by_address.get(service) else {
            return (Type::Unknown, None);
        };
        let member = self
            .members
            .get(&service_id)
            .and_then(|members| members.get(name))
            .copied();
        if let Some(member) = member {
            self.refer(
                context.module,
                span,
                Target::Declaration(self.declarations[member].address.clone()),
            );
            return self.declaration_value(context.module, member, span);
        }
        if self.discovers(service_id) {
            let key = Value::Discovered {
                service: service.clone(),
                key: name.to_owned(),
            };
            return (Type::Operation, Some(key));
        }
        let message = format!(
            "service `{}` has no `{name}`; it discovers no operations either",
            service.name()
        );
        if context.is_link_end {
            self.diagnostics.push(
                crate::diagnostic::Diagnostic::error(span, message)
                    .in_module(context.module)
                    .as_removed_link_end(),
            );
            return operation_of(service, name);
        }
        self.error(context.module, span, message);
        (Type::Unknown, None)
    }

    fn fact_member(
        &mut self,
        context: &Context,
        relation: &Address,
        name: &str,
        span: Span,
    ) -> Evaluated {
        match name {
            "exists" => return (Type::Bool, None),
            "supports" => return (Type::List(Box::new(Type::Json)), None),
            _ => {}
        }
        let columns = self
            .by_address
            .get(relation)
            .copied()
            .and_then(|id| self.columns(id))
            .unwrap_or_default();
        let Some(column) = columns.iter().find(|column| column.name == name) else {
            self.error(
                context.module,
                span,
                format!("a `{}` fact has no `{name}`", relation.name()),
            );
            return (Type::Unknown, None);
        };
        (column.column_type.clone(), None)
    }

    fn index(
        &mut self,
        context: &Context,
        target: &Expression,
        keys: &[Expression],
        span: Span,
    ) -> Evaluated {
        let (target_type, target_value) = self.evaluate(context, target);
        if let Some(Value::Service(service)) = &target_value {
            return self.discovered_key(context, service, keys, span);
        }
        match &target_type {
            Type::Graph(relation) | Type::Relation(relation) => {
                self.lookup_key(context, relation, keys, span)
            }
            Type::List(item) => {
                for key in keys {
                    self.expect_value(context, key, &Type::Int);
                }
                ((**item).clone(), None)
            }
            Type::Json | Type::Unknown => {
                for key in keys {
                    self.evaluate(context, key);
                }
                (target_type.clone(), None)
            }
            other => {
                self.error(context.module, span, format!("`{other}` cannot be indexed"));
                (Type::Unknown, None)
            }
        }
    }

    fn discovered_key(
        &mut self,
        context: &Context,
        service: &Address,
        keys: &[Expression],
        span: Span,
    ) -> Evaluated {
        let [key] = keys else {
            self.error(
                context.module,
                span,
                "a service is indexed by one key, such as `[\"GET /users/{}\"]`",
            );
            return (Type::Unknown, None);
        };
        let key_value = self.expect_value(context, key, &Type::Text);
        let Some(Value::Text(text)) = key_value else {
            return (Type::Operation, None);
        };
        let value = Value::Discovered {
            service: service.clone(),
            key: normalize_key(&text),
        };
        (Type::Operation, Some(value))
    }

    fn lookup_key(
        &mut self,
        context: &Context,
        relation: &Address,
        keys: &[Expression],
        span: Span,
    ) -> Evaluated {
        let columns = self
            .by_address
            .get(relation)
            .copied()
            .and_then(|id| self.columns(id))
            .unwrap_or_default();
        if keys.len() != columns.len() {
            self.error(
                context.module,
                span,
                format!(
                    "a `{}` lookup takes {} values, one per column",
                    relation.name(),
                    columns.len()
                ),
            );
        }
        for (key, column) in keys.iter().zip(&columns) {
            self.expect_value(context, key, &column.column_type);
        }
        (Type::Fact(relation.clone()), None)
    }

    fn call(
        &mut self,
        context: &Context,
        callee: &Expression,
        arguments: &[Argument],
        span: Span,
    ) -> Evaluated {
        if let ExpressionKind::Member { target, name } = &callee.kind {
            let receiver = self.evaluate(context, target);
            if let Some(method) = stdlib::method(&receiver.0, &name.text) {
                self.refer(context.module, name.span, builtin_target(&method));
                let receiver = receiver.1.map_or(Receiver::Unknown, Receiver::Known);
                return self.call_builtin(context, &method, receiver, Call { arguments, span });
            }
            let member = self.member(context, receiver, &name.text, name.span);
            return self.call_value(context, callee, member, Call { arguments, span });
        }
        let evaluated = self.evaluate(context, callee);
        self.call_value(context, callee, evaluated, Call { arguments, span })
    }

    fn call_value(
        &mut self,
        context: &Context,
        callee: &Expression,
        (callee_type, _): Evaluated,
        call: Call,
    ) -> Evaluated {
        if let Some(builtin) = self.builtin_named(context, callee) {
            return self.call_builtin(context, &builtin, Receiver::None, call);
        }
        let Type::Function(signature) = callee_type else {
            if callee_type != Type::Unknown {
                self.error(
                    context.module,
                    callee.span,
                    format!("`{callee_type}` cannot be called"),
                );
            }
            return (Type::Unknown, None);
        };
        let values = self.check_arguments(context, &signature, call);
        let value = match (&signature.returns, values) {
            (Type::Entity(entity), Some(fields)) => Some(Value::Entity {
                entity: entity.clone(),
                fields,
            }),
            _ => None,
        };
        (signature.returns.clone(), value)
    }

    /// The built-in function a callee names, if it names one.
    fn builtin_named(&self, context: &Context, callee: &Expression) -> Option<Builtin> {
        let entry = match &callee.kind {
            ExpressionKind::Name(name) if context.local(name).is_none() => {
                self.lookup(context.module, name)?
            }
            ExpressionKind::Member { target, name } => {
                let ExpressionKind::Name(namespace) = &target.kind else {
                    return None;
                };
                let Entry::Namespace(module) = self.lookup(context.module, namespace)? else {
                    return None;
                };
                self.export_of(&module, &name.text, &mut std::collections::BTreeSet::new())?
            }
            _ => return None,
        };
        let Entry::Builtin(index) = entry else {
            return None;
        };
        Some(self.builtins[index].clone())
    }

    fn call_builtin(
        &mut self,
        context: &Context,
        builtin: &Builtin,
        receiver: Receiver,
        call: Call,
    ) -> Evaluated {
        let values = self.check_arguments(context, &builtin.signature, call);
        let function = if builtin.module.is_empty() {
            builtin.name.to_owned()
        } else {
            format!("{}.{}", builtin.module, builtin.name)
        };
        if let Some(problem) = values
            .as_ref()
            .and_then(|known| stdlib::argument_problem(&function, known))
        {
            self.error(context.module, call.span, problem);
        }
        let value = values.and_then(|mut arguments| {
            match receiver {
                Receiver::None => {}
                Receiver::Known(value) => arguments.insert(0, ("receiver".to_owned(), value)),
                Receiver::Unknown => return None,
            }
            Some(Value::Request {
                function,
                arguments,
            })
        });
        (builtin.signature.returns.clone(), value)
    }

    /// Checks the arguments a check gives a parameterised behavior.
    pub(crate) fn check_run_arguments(
        &mut self,
        context: &Context,
        signature: &Signature,
        arguments: &[Argument],
        span: Span,
    ) {
        self.check_arguments(context, signature, Call { arguments, span });
    }

    /// Checks a call's arguments against a signature: positional ones first,
    /// then named ones. Returns their values by parameter name, when all
    /// are known.
    fn check_arguments(
        &mut self,
        context: &Context,
        signature: &Signature,
        call: Call,
    ) -> Option<Vec<(String, Value)>> {
        let mut given: Vec<(String, Option<Value>)> = Vec::new();
        let mut seen_named = false;
        for (position, argument) in call.arguments.iter().enumerate() {
            seen_named |= argument.name.is_some();
            let slot = match (&argument.name, seen_named) {
                (Some(_), _) => ArgumentSlot::Named,
                (None, true) => ArgumentSlot::AfterNamed,
                (None, false) => ArgumentSlot::Position(position),
            };
            let Some(parameter) = self.parameter_for(context, signature, slot, argument) else {
                self.evaluate(context, &argument.value);
                continue;
            };
            if given.iter().any(|(name, _)| *name == parameter.name) {
                self.error(
                    context.module,
                    argument.value.span,
                    format!("`{}` is given twice", parameter.name),
                );
                continue;
            }
            let value = self.expect_value(context, &argument.value, &parameter.parameter_type);
            given.push((parameter.name.clone(), value));
        }
        let missing = signature
            .parameters
            .iter()
            .filter(|parameter| !parameter.is_optional)
            .filter(|parameter| !given.iter().any(|(name, _)| *name == parameter.name));
        for parameter in missing {
            self.error(
                context.module,
                call.span,
                format!("`{}` is required", parameter.name),
            );
        }
        given
            .into_iter()
            .map(|(name, value)| value.map(|value| (name, value)))
            .collect()
    }

    /// The parameter an argument fills, reported when there is none.
    fn parameter_for<'s>(
        &mut self,
        context: &Context,
        signature: &'s Signature,
        slot: ArgumentSlot,
        argument: &Argument,
    ) -> Option<&'s Parameter> {
        let found = match (slot, &argument.name) {
            (_, Some(name)) => signature
                .parameters
                .iter()
                .find(|parameter| parameter.name == name.text),
            (ArgumentSlot::AfterNamed, None) => {
                self.error(
                    context.module,
                    argument.value.span,
                    "positional arguments come before named ones",
                );
                return None;
            }
            (ArgumentSlot::Position(position), None) => signature.parameters.get(position),
            (ArgumentSlot::Named, None) => None,
        };
        if found.is_none() {
            let what = argument.name.as_ref().map_or_else(
                || "this argument".to_owned(),
                |name| format!("`{}`", name.text),
            );
            let takes = signature_text(&Type::Function(Box::new(signature.clone())));
            self.error(
                context.module,
                argument.value.span,
                format!("there is no parameter for {what}; it takes {takes}"),
            );
        }
        found
    }

    fn unary(
        &mut self,
        context: &Context,
        operator: UnaryOperator,
        operand: &Expression,
        span: Span,
    ) -> Evaluated {
        let (operand_type, value) = self.evaluate(context, operand);
        match operator {
            UnaryOperator::Not => {
                self.require_type(context, operand.span, &operand_type, &Type::Bool);
                let value = match value {
                    Some(Value::Bool(known)) => Some(Value::Bool(!known)),
                    _ => None,
                };
                (Type::Bool, value)
            }
            UnaryOperator::Negate => {
                self.require_type(context, operand.span, &operand_type, &Type::Int);
                let value = match value {
                    Some(Value::Int(known)) => known.checked_neg().map(Value::Int),
                    _ => None,
                };
                (Type::Int, value)
            }
            UnaryOperator::Need => {
                if !context.is_behavior {
                    self.error(
                        context.module,
                        span,
                        "`need` asks a checker for evidence, which only a behavior can do",
                    );
                }
                match operand_type {
                    Type::Evidence(inner) => (*inner, None),
                    Type::Unknown => (Type::Unknown, None),
                    other => {
                        self.error(
                            context.module,
                            operand.span,
                            format!("`need` takes evidence from a checker, not `{other}`"),
                        );
                        (Type::Unknown, None)
                    }
                }
            }
        }
    }

    fn binary(
        &mut self,
        context: &Context,
        operator: BinaryOperator,
        left: &Expression,
        right: &Expression,
    ) -> Evaluated {
        let (left_type, _) = self.evaluate(context, left);
        let (right_type, _) = self.evaluate(context, right);
        match operator {
            BinaryOperator::And | BinaryOperator::Or => {
                self.require_type(context, left.span, &left_type, &Type::Bool);
                self.require_type(context, right.span, &right_type, &Type::Bool);
                (Type::Bool, None)
            }
            BinaryOperator::Equal | BinaryOperator::NotEqual => {
                let comparable = left_type.fits(&right_type) || right_type.fits(&left_type);
                if !comparable {
                    self.error(
                        context.module,
                        left.span.to(right.span),
                        format!("`{left_type}` and `{right_type}` are never equal; equality is type-strict"),
                    );
                }
                (Type::Bool, None)
            }
            BinaryOperator::Less
            | BinaryOperator::LessOrEqual
            | BinaryOperator::Greater
            | BinaryOperator::GreaterOrEqual => {
                self.require_type(context, left.span, &left_type, &Type::Int);
                self.require_type(context, right.span, &right_type, &Type::Int);
                (Type::Bool, None)
            }
            BinaryOperator::Add if left_type == Type::Text => {
                self.require_type(context, right.span, &right_type, &Type::Text);
                (Type::Text, None)
            }
            _ => {
                self.require_type(context, left.span, &left_type, &Type::Int);
                self.require_type(context, right.span, &right_type, &Type::Int);
                (Type::Int, None)
            }
        }
    }

    fn require_type(&mut self, context: &Context, span: Span, found: &Type, expected: &Type) {
        // Json values are checked when the evidence arrives, not here.
        if found.fits(expected) || *found == Type::Json {
            return;
        }
        self.error(
            context.module,
            span,
            format!("expected `{expected}`, found `{found}`"),
        );
    }
}

/// An HTTP key with path parameters written `{}`: `GET /users/{id}` and
/// `GET /users/{}` name one operation.
pub fn normalize_key(key: &str) -> String {
    let mut normalized = String::with_capacity(key.len());
    let mut in_parameter = false;
    for character in key.chars() {
        match character {
            '{' => {
                in_parameter = true;
                normalized.push('{');
            }
            '}' => {
                in_parameter = false;
                normalized.push('}');
            }
            _ if in_parameter => {}
            _ => normalized.push(character),
        }
    }
    normalized
}

/// The operation `name` of `service` as a link writes it, known only by its
/// address because no declaration of it can be read.
fn operation_of(service: &Address, name: &str) -> Evaluated {
    let mut path = service.path.clone();
    path.push(name.to_owned());
    let operation = Address {
        module: service.module.clone(),
        path,
    };
    (Type::Unknown, Some(Value::Operation(operation)))
}
