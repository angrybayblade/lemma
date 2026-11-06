//! Checking declarations other than rules and statements: types, entities,
//! relations, services and operations, graphs, checks, allowances and the
//! workspace.

use std::collections::BTreeMap;

use super::expressions::Context;
use super::{Checker, DeclarationId, Definition, Entry, NodeRef, Target};
use crate::load::Module;
use crate::model::{self, Column, Run, Severity};
use crate::stdlib::{self, FACTS, OPERATIONS};
use crate::syntax::ast::ExpressionKind;
use crate::syntax::ast::{self, CheckMember, Declaration as Node, GraphBody, Item, TypeExpression};
use crate::types::{Address, RecordField, Type, Value};

impl<'a> Checker<'a> {
    pub(super) fn check_declaration(&mut self, id: DeclarationId) {
        let declared = self.declarations[id].clone();
        let module = declared.module;
        let detail = match declared.node {
            NodeRef::Top(Node::Binding(_)) | NodeRef::ServiceBinding(_) => {
                self.binding_value(id).0.to_string()
            }
            NodeRef::Top(Node::Type(declaration)) => {
                self.resolve_type(module, &declaration.value).to_string()
            }
            NodeRef::Top(Node::Entity(entity)) => self.check_entity(module, entity),
            NodeRef::Top(Node::Relation(_)) => self.check_relation(id),
            NodeRef::Top(Node::Service(service)) | NodeRef::Service(service) => {
                self.check_service(id, service);
                "Service".to_owned()
            }
            NodeRef::Operation(operation) => {
                self.check_operation(id, operation);
                if operation.is_client {
                    "ClientOperation".to_owned()
                } else {
                    "Operation".to_owned()
                }
            }
            NodeRef::Top(Node::Graph(graph)) => self.check_graph(id, graph),
            NodeRef::Top(Node::Rule(rule)) => self.check_rule(id, rule),
            NodeRef::Top(Node::Require(require)) => self.check_require(id, require),
            NodeRef::Top(Node::Behavior(behavior)) => self.check_behavior(id, behavior),
            NodeRef::Top(Node::Function(function)) => self.check_function(id, function),
            NodeRef::Top(Node::Checker(checker)) => self.check_checker(id, checker),
            NodeRef::Top(Node::Check(check)) => {
                self.check_entry_point(id, check);
                "check".to_owned()
            }
            NodeRef::Top(Node::Allow(_)) => return,
        };
        self.index.definitions.insert(
            declared.address.clone(),
            Definition {
                module: module.to_owned(),
                span: declared.name_span,
                kind: declared.node.kind(),
                detail,
            },
        );
    }

    /// A written type, resolved.
    pub(crate) fn resolve_type(&mut self, module: &str, written: &TypeExpression) -> Type {
        match written {
            TypeExpression::Record { fields, .. } => Type::Record(
                fields
                    .iter()
                    .map(|field| RecordField {
                        name: field.name.text.clone(),
                        field_type: self.resolve_type(module, &field.field_type),
                        is_optional: false,
                    })
                    .collect(),
            ),
            TypeExpression::Named {
                name, arguments, ..
            } => self.resolve_named_type(module, name, arguments),
        }
    }

    fn resolve_named_type(
        &mut self,
        module: &str,
        name: &ast::Name,
        arguments: &[TypeExpression],
    ) -> Type {
        let generic = match name.text.as_str() {
            "List" | "Evidence" | "Resolution" | "Fact" | "Graph" => Some(name.text.as_str()),
            _ => None,
        };
        if let Some(generic) = generic {
            return self.resolve_generic(module, name, generic, arguments);
        }
        if !arguments.is_empty() {
            self.error(
                module,
                name.span,
                format!("`{}` takes no type arguments", name.text),
            );
        }
        if let Some(builtin) = stdlib::type_named(&name.text) {
            return builtin;
        }
        let Some(entry) = self.lookup(module, &name.text) else {
            self.error(
                module,
                name.span,
                format!("there is no type `{}`", name.text),
            );
            return Type::Unknown;
        };
        self.refer_entry(module, name.span, &entry);
        let Entry::Declaration(id) = entry else {
            self.error(module, name.span, format!("`{}` is not a type", name.text));
            return Type::Unknown;
        };
        let declared = self.declarations[id].clone();
        match declared.node {
            NodeRef::Top(Node::Entity(_)) => Type::Entity(declared.address),
            NodeRef::Top(Node::Relation(_)) => Type::Relation(declared.address),
            NodeRef::Top(Node::Type(alias)) => self.resolve_type(declared.module, &alias.value),
            _ => {
                self.error(module, name.span, format!("`{}` is not a type", name.text));
                Type::Unknown
            }
        }
    }

    fn resolve_generic(
        &mut self,
        module: &str,
        name: &ast::Name,
        generic: &str,
        arguments: &[TypeExpression],
    ) -> Type {
        let [argument] = arguments else {
            self.error(
                module,
                name.span,
                format!("`{generic}` takes one type argument"),
            );
            return Type::Unknown;
        };
        let inner = self.resolve_type(module, argument);
        match (generic, inner) {
            ("List", inner) => Type::List(Box::new(inner)),
            ("Evidence", inner) => Type::Evidence(Box::new(inner)),
            ("Resolution", inner) => Type::Resolution(Box::new(inner)),
            ("Fact", Type::Relation(relation)) => Type::Fact(relation),
            ("Graph", Type::Relation(relation)) => Type::Graph(relation),
            (_, Type::Unknown) => Type::Unknown,
            (generic, other) => {
                self.error(
                    module,
                    argument.span(),
                    format!("`{generic}` takes a relation, not `{other}`"),
                );
                Type::Unknown
            }
        }
    }

    fn check_entity(&mut self, module: &str, entity: &ast::EntityDeclaration) -> String {
        let mut names = Vec::new();
        for field in &entity.fields {
            let field_type = self.resolve_type(module, &field.field_type);
            names.push(format!("{}: {field_type}", field.name.text));
        }
        format!("{{ {} }}", names.join(", "))
    }

    fn check_relation(&mut self, id: DeclarationId) -> String {
        let columns = self.columns(id).unwrap_or_default();
        let address = self.declarations[id].address.clone();
        let detail = columns_text(&columns);
        self.model
            .relations
            .push(model::Relation { address, columns });
        detail
    }

    /// The columns of a relation, rule or graph.
    pub(crate) fn columns(&mut self, id: DeclarationId) -> Option<Vec<Column>> {
        if let Some(columns) = self.columns.get(&id) {
            return Some(columns.clone());
        }
        let declared = self.declarations[id].clone();
        let module = declared.module;
        let columns = match declared.node {
            NodeRef::Top(Node::Relation(relation)) => self.typed_columns(module, &relation.columns),
            NodeRef::Top(Node::Rule(rule)) => match &rule.columns {
                ast::RuleColumns::Inline(columns) => self.typed_columns(module, columns),
                ast::RuleColumns::Relation(relation) => self.relation_columns(module, relation)?,
            },
            NodeRef::Top(Node::Graph(graph)) => self.relation_columns(module, &graph.relation)?,
            _ => return None,
        };
        self.columns.insert(id, columns.clone());
        Some(columns)
    }

    fn typed_columns(&mut self, module: &str, fields: &[ast::TypedField]) -> Vec<Column> {
        let mut columns: Vec<Column> = Vec::new();
        for field in fields {
            let column_type = self.resolve_type(module, &field.field_type);
            if !column_type.is_column_type() {
                self.error(
                    module,
                    field.field_type.span(),
                    format!("a column cannot be `{column_type}`; use Text, Int, Bool, Service, Operation, Symbol, Site or an entity"),
                );
            }
            if columns.iter().any(|column| column.name == field.name.text) {
                self.error(
                    module,
                    field.name.span,
                    format!("there are two columns named `{}`", field.name.text),
                );
            }
            columns.push(Column {
                name: field.name.text.clone(),
                column_type,
            });
        }
        columns
    }

    /// The columns of the relation a type expression names.
    fn relation_columns(&mut self, module: &str, written: &TypeExpression) -> Option<Vec<Column>> {
        let relation = self.relation_named(module, written)?;
        let id = *self.by_address.get(&relation)?;
        self.columns(id)
    }

    /// The relation a type expression names, reported if it names none.
    pub(crate) fn relation_named(
        &mut self,
        module: &str,
        written: &TypeExpression,
    ) -> Option<Address> {
        match self.resolve_type(module, written) {
            Type::Relation(address) => Some(address),
            Type::Unknown => None,
            other => {
                self.error(
                    module,
                    written.span(),
                    format!("expected a relation, found `{other}`"),
                );
                None
            }
        }
    }

    fn check_service(&mut self, id: DeclarationId, service: &ast::ServiceDeclaration) {
        let declared = self.declarations[id].clone();
        let context = Context::new(declared.module).in_service(id);
        let fields: Vec<&ast::Field> = service
            .members
            .iter()
            .filter_map(|member| match member {
                ast::ServiceMember::Field(field) if field.name.text != "operations" => Some(field),
                _ => None,
            })
            .collect();
        let mut values =
            self.check_fields(&context, &fields, &stdlib::service_fields(), "a service");
        let operations = service.members.iter().find_map(|member| match member {
            ast::ServiceMember::Field(field) if field.name.text == "operations" => Some(field),
            _ => None,
        });
        if let Some(field) = operations {
            let providers = self.operations_providers(&context, &field.value);
            values.insert("operations".to_owned(), Value::List(providers));
        }
        values.retain(|_, value| !matches!(value, Value::Null));
        let repository = self.loaded.modules[declared.module].repository.clone();
        let existing = self
            .model
            .services
            .iter_mut()
            .find(|entry| entry.address == declared.address);
        if let Some(entry) = existing {
            entry.repository = repository;
            entry.fields = values;
            return;
        }
        self.model.services.push(model::Service {
            address: declared.address,
            repository,
            fields: values,
            operations: Vec::new(),
        });
    }

    /// A service's `operations`: one provider, or a list of them.
    fn operations_providers(
        &mut self,
        context: &Context,
        expression: &ast::Expression,
    ) -> Vec<Value> {
        let items: Vec<&ast::Expression> = match &expression.kind {
            ExpressionKind::List(items) => items.iter().collect(),
            _ => vec![expression],
        };
        items
            .into_iter()
            .filter_map(|item| self.expect_value(context, item, &OPERATIONS))
            .collect()
    }

    /// Whether a service discovers operations through providers, so a
    /// name its declarations do not have may still be one of them.
    pub(crate) fn discovers(&self, service: DeclarationId) -> bool {
        let node = self.declarations[service].node;
        let members = match node {
            NodeRef::Top(Node::Service(service)) | NodeRef::Service(service) => &service.members,
            _ => return false,
        };
        members.iter().any(|member| {
            matches!(member, ast::ServiceMember::Field(field) if field.name.text == "operations")
        })
    }

    fn check_operation(&mut self, id: DeclarationId, operation: &ast::OperationDeclaration) {
        let declared = self.declarations[id].clone();
        let service = self.parent_service(&declared.address);
        let mut context = Context::new(declared.module);
        if let Some(service) = service {
            context = context.in_service(service);
        }
        let fields: Vec<&ast::Field> = operation.fields.iter().collect();
        let schema = stdlib::operation_fields(operation.is_client);
        let other_role = stdlib::operation_fields(!operation.is_client);
        let misplaced = fields.iter().filter(|field| {
            !schema.iter().any(|known| known.name == field.name.text)
                && other_role.iter().any(|known| known.name == field.name.text)
        });
        for field in misplaced {
            self.report_other_role(declared.module, field, operation.is_client);
        }
        let known: Vec<&ast::Field> = fields
            .into_iter()
            .filter(|field| {
                !other_role.iter().any(|known| known.name == field.name.text)
                    || schema.iter().any(|known| known.name == field.name.text)
            })
            .collect();
        let values = self.check_fields(&context, &known, &schema, "an operation");
        self.add_operation(service, declared.address, operation.is_client, values);
    }

    /// Reports a schema property of the other role, such as `request` on a
    /// client operation.
    fn report_other_role(&mut self, module: &str, field: &ast::Field, is_client: bool) {
        let (role, instead) = if is_client {
            ("a client operation", "`sends` and `accepts`")
        } else {
            ("an operation", "`request` and `response`")
        };
        self.error(
            module,
            field.name.span,
            format!("{role} has {instead}, not `{}`", field.name.text),
        );
    }

    fn add_operation(
        &mut self,
        service: Option<DeclarationId>,
        address: Address,
        is_client: bool,
        fields: BTreeMap<String, Value>,
    ) {
        let Some(service) = service else {
            return;
        };
        let service_address = self.declarations[service].address.clone();
        let operation = model::Operation {
            address,
            is_client,
            fields,
        };
        if let Some(entry) = self
            .model
            .services
            .iter_mut()
            .find(|entry| entry.address == service_address)
        {
            entry.operations.push(operation);
        } else {
            // The service is checked after its operations when declared
            // later; keep the operation for it.
            self.model.services.push(model::Service {
                address: service_address,
                repository: String::new(),
                fields: BTreeMap::new(),
                operations: vec![operation],
            });
        }
    }

    pub(crate) fn parent_service(&self, address: &Address) -> Option<DeclarationId> {
        let mut parent = address.clone();
        parent.path.pop();
        if parent.path.is_empty() {
            return None;
        }
        self.by_address.get(&parent).copied()
    }

    /// Checks `fields` against `schema`, returning their values by name.
    pub(crate) fn check_fields(
        &mut self,
        context: &Context,
        fields: &[&ast::Field],
        schema: &[RecordField],
        what: &str,
    ) -> BTreeMap<String, Value> {
        let mut values = BTreeMap::new();
        for field in fields {
            let Some(expected) = schema.iter().find(|known| known.name == field.name.text) else {
                let known: Vec<String> = schema
                    .iter()
                    .map(|known| format!("`{}`", known.name))
                    .collect();
                self.error(
                    context.module,
                    field.name.span,
                    format!(
                        "{what} has no field `{}`; it has {}",
                        field.name.text,
                        known.join(", ")
                    ),
                );
                continue;
            };
            if values.contains_key(&field.name.text) {
                self.error(
                    context.module,
                    field.name.span,
                    format!("`{}` is given twice", field.name.text),
                );
                continue;
            }
            let value = self.expect_value(context, &field.value, &expected.field_type);
            values.insert(field.name.text.clone(), value.unwrap_or(Value::Null));
        }
        values
    }

    fn check_graph(&mut self, id: DeclarationId, graph: &ast::GraphDeclaration) -> String {
        let declared = self.declarations[id].clone();
        let module = declared.module;
        let relation = self.relation_named(module, &graph.relation);
        let columns = self.columns(id).unwrap_or_default();
        let context = Context::new(module);
        let mut rows = Vec::new();
        let mut provider = None;
        match &graph.body {
            GraphBody::Rows(written) => {
                for row in written {
                    rows.push(self.check_row(&context, row, &columns));
                }
            }
            GraphBody::Provider(expression) => {
                provider = self.expect_value(&context, expression, &FACTS);
            }
        }
        let detail = relation.as_ref().map_or_else(String::new, |relation| {
            format!("Graph<{}>", relation.name())
        });
        if let Some(relation) = relation {
            self.model.graphs.push(model::Graph {
                address: declared.address,
                relation,
                rows,
                provider,
            });
        }
        detail
    }

    fn check_row(
        &mut self,
        context: &Context,
        row: &ast::GraphRow,
        columns: &[Column],
    ) -> Vec<Value> {
        if row.values.len() != columns.len() && !columns.is_empty() {
            self.error(
                context.module,
                row.span,
                format!(
                    "this relation has {} columns; the row gives {}",
                    columns.len(),
                    row.values.len()
                ),
            );
            return Vec::new();
        }
        row.values
            .iter()
            .zip(columns)
            .filter_map(|(expression, column)| self.row_value(context, expression, column))
            .collect()
    }

    /// One value of a row, which must be known without running anything.
    fn row_value(
        &mut self,
        context: &Context,
        expression: &ast::Expression,
        column: &Column,
    ) -> Option<Value> {
        let reported = self.diagnostics.len();
        let value = self.expect_value(context, expression, &column.column_type);
        // A value that failed to check was reported already.
        if value.is_none() && self.diagnostics.len() == reported {
            self.error(
                context.module,
                expression.span,
                "a row's values must be known without running anything",
            );
        }
        value
    }

    fn check_entry_point(&mut self, id: DeclarationId, check: &ast::CheckDeclaration) {
        let declared = self.declarations[id].clone();
        let module = declared.module;
        let mut runs: Vec<Run> = Vec::new();
        let mut skipped = Vec::new();
        for member in &check.members {
            match member {
                CheckMember::Run(run) => runs.extend(self.check_run(module, run)),
                CheckMember::Skip(target) => skipped.extend(self.requirement_named(module, target)),
            }
        }
        runs.retain(|run| !skipped.contains(&run.requirement));
        self.model.checks.push(model::Check {
            address: declared.address,
            runs,
        });
    }

    fn check_run(&mut self, module: &str, run: &ast::RunSelection) -> Vec<Run> {
        let context = Context::new(module);
        let override_fields: Vec<&ast::Field> = run.overrides.iter().collect();
        let schema = [RecordField {
            name: "severity".to_owned(),
            field_type: stdlib::SEVERITY,
            is_optional: true,
        }];
        let overrides = self.check_fields(&context, &override_fields, &schema, "a run");
        let severity = overrides.get("severity").and_then(severity_of);
        if run.is_wildcard {
            let name = run.target.dotted();
            if !self.loaded.modules.contains_key(&name) {
                self.error(
                    module,
                    run.target.span,
                    format!("module `{name}` is not loaded; import it"),
                );
                return Vec::new();
            }
            self.refer(module, run.target.span, Target::Module(name.clone()));
            // A parameterised behavior runs only with arguments, which a
            // module's `*` cannot give.
            return self
                .requirements_of(&name)
                .into_iter()
                .filter(|requirement| self.behavior_parameters(requirement).is_none())
                .map(|requirement| Run {
                    requirement,
                    severity,
                    arguments: Vec::new(),
                })
                .collect();
        }
        let Some(requirement) = self.requirement_named(module, &run.target) else {
            return Vec::new();
        };
        let arguments = self.run_arguments(module, run, &requirement);
        vec![Run {
            requirement,
            severity,
            arguments,
        }]
    }

    /// The arguments a run gives its behavior, checked against the
    /// behavior's parameters.
    fn run_arguments(
        &mut self,
        module: &str,
        run: &ast::RunSelection,
        requirement: &Address,
    ) -> Vec<model::CallArgument> {
        let parameters = self.behavior_parameters(requirement);
        let (Some(parameters), Some(arguments)) = (&parameters, &run.arguments) else {
            if parameters.is_some() {
                self.error(
                    module,
                    run.target.span,
                    format!(
                        "`{}` takes parameters; give them, as in `run {}(...)`",
                        requirement.name(),
                        run.target.dotted()
                    ),
                );
            } else if run.arguments.is_some() {
                self.error(
                    module,
                    run.target.span,
                    format!("`{}` takes no arguments", requirement.name()),
                );
            }
            return Vec::new();
        };
        let mut context = Context::new(module);
        context.records = true;
        let signature = crate::types::Signature {
            parameters: parameters
                .iter()
                .map(|column| crate::types::Parameter {
                    name: column.name.clone(),
                    parameter_type: column.column_type.clone(),
                    is_optional: false,
                })
                .collect(),
            returns: Type::Unknown,
        };
        self.check_run_arguments(&context, &signature, arguments, run.span);
        self.lower_arguments(module, arguments)
    }

    /// A behavior's parameters, if it is a behavior that takes any.
    fn behavior_parameters(&mut self, requirement: &Address) -> Option<Vec<Column>> {
        let id = *self.by_address.get(requirement)?;
        let declared = self.declarations[id].clone();
        let NodeRef::Top(Node::Behavior(behavior)) = declared.node else {
            return None;
        };
        let parameters = behavior
            .parameters
            .as_ref()
            .filter(|written| !written.is_empty())?;
        Some(
            parameters
                .iter()
                .map(|parameter| Column {
                    name: parameter.name.text.clone(),
                    column_type: self.resolve_type(declared.module, &parameter.field_type),
                })
                .collect(),
        )
    }

    /// Every requirement and behavior a module declares, in order.
    fn requirements_of(&self, module: &str) -> Vec<Address> {
        self.declarations
            .iter()
            .filter(|declared| declared.module == module)
            .filter(|declared| {
                matches!(
                    declared.node,
                    NodeRef::Top(Node::Require(_) | Node::Behavior(_))
                )
            })
            .map(|declared| declared.address.clone())
            .collect()
    }

    /// The requirement or behavior `target` names: a name in scope, or a
    /// module and a name joined by dots.
    pub(crate) fn requirement_named(
        &mut self,
        module: &str,
        target: &ast::ModuleName,
    ) -> Option<Address> {
        let dotted = target.dotted();
        let found = if let [single] = target.segments.as_slice() {
            match self.lookup(module, &single.text) {
                Some(Entry::Declaration(id)) => Some(id),
                _ => None,
            }
        } else {
            dotted
                .rsplit_once('.')
                .and_then(|(owner, name)| self.by_address.get(&Address::top(owner, name)).copied())
        };
        let is_requirement = found.is_some_and(|id| {
            matches!(
                self.declarations[id].node,
                NodeRef::Top(Node::Require(_) | Node::Behavior(_))
            )
        });
        let Some(id) = found.filter(|_| is_requirement) else {
            self.error(
                module,
                target.span,
                format!("there is no requirement or behavior `{dotted}`"),
            );
            return None;
        };
        let address = self.declarations[id].address.clone();
        self.refer(module, target.span, Target::Declaration(address.clone()));
        Some(address)
    }

    pub(super) fn check_workspace(&mut self, entry: &'a Module) {
        let context = Context::new(&entry.name);
        for item in &entry.file.items {
            match item {
                Item::Workspace(workspace) => self.check_workspace_block(&context, workspace),
                Item::Repo(repo) => {
                    self.expect_value(&context, &repo.source, &stdlib::REPOSITORY_SOURCE);
                }
                _ => {}
            }
        }
        let ignored: Vec<(String, crate::Span)> = self
            .loaded
            .modules
            .values()
            .filter(|module| module.name != entry.name)
            .filter_map(|module| {
                module.file.items.iter().find_map(|item| match item {
                    Item::Workspace(workspace) => Some((module.name.clone(), workspace.span)),
                    _ => None,
                })
            })
            .collect();
        for (module, span) in ignored {
            self.diagnostics.push(
                crate::Diagnostic::note(
                    span,
                    "this workspace is ignored: only the entry module's workspace is read",
                )
                .in_module(&module),
            );
        }
    }

    fn check_workspace_block(&mut self, context: &Context, workspace: &ast::Workspace) {
        let fields: Vec<&ast::Field> = workspace.fields.iter().collect();
        let values =
            self.check_fields(context, &fields, &stdlib::workspace_fields(), "a workspace");
        for repo in &workspace.repos {
            self.expect_value(context, &repo.source, &stdlib::REPOSITORY_SOURCE);
        }
        let level = values
            .get("config")
            .and_then(|config| config.field("level"))
            .and_then(Value::as_text)
            .map(str::to_owned);
        let language = values
            .get("language")
            .and_then(Value::as_text)
            .map(str::to_owned);
        self.model.workspace = Some(model::Workspace { language, level });
    }

    pub(super) fn check_allow(&mut self, module: &'a str, allow: &'a ast::AllowDeclaration) {
        let Some(requirement) = self.requirement_named(module, &allow.target) else {
            return;
        };
        let context = Context::new(module);
        let schema = [
            RecordField {
                name: "reason".to_owned(),
                field_type: Type::Text,
                is_optional: true,
            },
            RecordField {
                name: "until".to_owned(),
                field_type: Type::Text,
                is_optional: true,
            },
        ];
        let fields: Vec<&ast::Field> = allow.fields.iter().collect();
        let values = self.check_fields(&context, &fields, &schema, "an allowance");
        let until = values
            .get("until")
            .and_then(Value::as_text)
            .map(str::to_owned);
        if let (Some(until), Some(field)) = (
            &until,
            fields.iter().find(|field| field.name.text == "until"),
        ) {
            if !is_date(until) {
                self.error(
                    module,
                    field.value.span,
                    "`until` is a date written `YYYY-MM-DD`",
                );
            }
        }
        let variables = self.requirement_variables(&requirement);
        let body = allow
            .when
            .as_ref()
            .map(|body| self.check_body(module, body, Some(&variables)))
            .unwrap_or_default();
        self.model.allowances.push(model::Allowance {
            requirement,
            variables: variables
                .iter()
                .map(|(name, variable_type)| Column {
                    name: name.clone(),
                    column_type: variable_type.clone(),
                })
                .collect(),
            body,
            reason: values
                .get("reason")
                .and_then(Value::as_text)
                .map(str::to_owned),
            until,
        });
    }

    /// A requirement's finding variables and their types: those its message
    /// and `at` use.
    fn requirement_variables(&self, requirement: &Address) -> Vec<(String, Type)> {
        self.requirement_variables
            .get(requirement)
            .cloned()
            .unwrap_or_default()
    }
}

pub(crate) fn severity_of(value: &Value) -> Option<Severity> {
    match value.as_text()? {
        "error" => Some(Severity::Error),
        "warning" => Some(Severity::Warning),
        "info" => Some(Severity::Info),
        _ => None,
    }
}

fn columns_text(columns: &[Column]) -> String {
    let written: Vec<String> = columns
        .iter()
        .map(|column| format!("{}: {}", column.name, column.column_type))
        .collect();
    format!("({})", written.join(", "))
}

fn is_date(text: &str) -> bool {
    let bytes = text.as_bytes();
    bytes.len() == 10
        && bytes[4] == b'-'
        && bytes[7] == b'-'
        && bytes
            .iter()
            .enumerate()
            .all(|(index, byte)| index == 4 || index == 7 || byte.is_ascii_digit())
}
