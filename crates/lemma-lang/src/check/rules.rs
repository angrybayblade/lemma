//! Checking rules, requirement bodies and their terms: the Datalog layer.
//!
//! Each clause is range-restricted (spec 13 §7): a variable in the head, in
//! a negated literal or in a comparison must be bound by a positive literal
//! of the same clause. A variable's type comes from the columns it fills,
//! and must agree wherever it appears.

use std::collections::BTreeMap;

use super::expressions::Context;
use super::{Checker, DeclarationId, Entry, NodeRef, Target};
use crate::model::{self, Column, Predicate, Term};
use crate::span::Span;
use crate::stdlib;
use crate::syntax::ast::{
    self, AtomArguments, Declaration as Node, Expression, ExpressionKind, Literal, PathStep,
    TermKind,
};
use crate::types::{Address, RecordField, Type, Value};

/// A requirement's checked fields.
struct RequireFields {
    severity: model::Severity,
    message: Option<String>,
    at: Option<String>,
}

/// The column an atom's term fills, and whether the atom is negated.
#[derive(Clone, Copy)]
struct Slot<'c> {
    column: &'c Column,
    is_negated: bool,
}

/// The variables of one clause: their types, and whether a positive
/// literal binds them.
#[derive(Debug, Default)]
struct Variables {
    entries: BTreeMap<String, Variable>,
    /// Variables in the order they first appear.
    order: Vec<String>,
}

#[derive(Debug)]
struct Variable {
    variable_type: Type,
    is_bound: bool,
}

impl Variables {
    fn note(
        &mut self,
        name: &str,
        variable_type: Type,
        is_bound: bool,
    ) -> Result<(), (Type, Type)> {
        if let Some(existing) = self.entries.get_mut(name) {
            existing.is_bound |= is_bound;
            let agrees = variable_type.fits(&existing.variable_type)
                || existing.variable_type.fits(&variable_type);
            if !agrees {
                return Err((existing.variable_type.clone(), variable_type));
            }
            if existing.variable_type == Type::Unknown {
                existing.variable_type = variable_type;
            }
            return Ok(());
        }
        self.order.push(name.to_owned());
        self.entries.insert(
            name.to_owned(),
            Variable {
                variable_type,
                is_bound,
            },
        );
        Ok(())
    }

    fn bound(&self, name: &str) -> Option<&Type> {
        self.entries
            .get(name)
            .filter(|variable| variable.is_bound)
            .map(|variable| &variable.variable_type)
    }
}

impl Checker<'_> {
    pub(super) fn check_rule(&mut self, id: DeclarationId, rule: &ast::RuleDeclaration) -> String {
        let declared = self.declarations[id].clone();
        let module = declared.module;
        let columns = self.columns(id).unwrap_or_default();
        let mut clauses = Vec::new();
        for clause in &rule.clauses {
            let mut variables = Variables::default();
            let body = self.literals(module, &clause.body.literals, &mut variables);
            let head = self.head(module, clause, &columns, &variables);
            clauses.push(model::Clause { head, body });
        }
        self.model.rules.push(model::Rule {
            address: declared.address,
            columns: columns.clone(),
            clauses,
        });
        let written: Vec<String> = columns
            .iter()
            .map(|column| format!("{}: {}", column.name, column.column_type))
            .collect();
        format!("({})", written.join(", "))
    }

    fn head(
        &mut self,
        module: &str,
        clause: &ast::RuleClause,
        columns: &[Column],
        variables: &Variables,
    ) -> Vec<Term> {
        if clause.head.len() != columns.len() {
            self.error(
                module,
                clause.span,
                format!(
                    "this rule has {} columns; the clause derives {}",
                    columns.len(),
                    clause.head.len()
                ),
            );
        }
        clause
            .head
            .iter()
            .zip(columns)
            .map(|(term, column)| self.head_term(module, term, column, variables))
            .collect()
    }

    fn head_term(
        &mut self,
        module: &str,
        term: &ast::Term,
        column: &Column,
        variables: &Variables,
    ) -> Term {
        match &term.kind {
            TermKind::Variable(name) => {
                self.check_head_variable(module, (name, term.span), column, variables);
                Term::Variable(name.clone())
            }
            TermKind::Wildcard | TermKind::Field { .. } => {
                self.error(
                    module,
                    term.span,
                    "a clause's head holds variables and constants",
                );
                Term::Wildcard
            }
            _ => self.constant_term(module, term, &column.column_type),
        }
    }

    fn check_head_variable(
        &mut self,
        module: &str,
        (name, span): (&str, Span),
        column: &Column,
        variables: &Variables,
    ) {
        match variables.bound(name) {
            Some(found) if !found.fits(&column.column_type) => {
                let message = format!(
                    "`{name}` is `{found}`, but column `{}` is `{}`",
                    column.name, column.column_type
                );
                self.error(module, span, message);
            }
            Some(_) => {}
            None => self.error(
                module,
                span,
                format!("`{name}` must be bound by a positive literal of this clause"),
            ),
        }
    }

    /// Checks a body: positive literals first, which bind variables, then
    /// negations and comparisons, which need them bound.
    fn literals(
        &mut self,
        module: &str,
        literals: &[Literal],
        variables: &mut Variables,
    ) -> Vec<model::Literal> {
        let mut checked: Vec<Option<model::Literal>> = vec![None; literals.len()];
        for (index, literal) in literals.iter().enumerate() {
            checked[index] = match literal {
                Literal::Atom(atom) if !atom.is_negated => self.atom(module, atom, variables),
                _ => None,
            };
        }
        for (index, literal) in literals.iter().enumerate() {
            match literal {
                Literal::Atom(atom) if atom.is_negated => {
                    checked[index] = self.atom(module, atom, variables);
                }
                Literal::Comparison(comparison) => {
                    checked[index] = Some(self.comparison(module, comparison, variables));
                }
                Literal::Atom(_) => {}
            }
        }
        checked.into_iter().flatten().collect()
    }

    fn atom(
        &mut self,
        module: &str,
        atom: &ast::Atom,
        variables: &mut Variables,
    ) -> Option<model::Literal> {
        let (predicate, columns) = self.predicate(module, &atom.predicate)?;
        if atom.snapshot.is_some() && matches!(predicate, Predicate::Rule(_)) {
            self.error(
                module,
                atom.span,
                "a rule's facts have no snapshot; `@base` and `@head` read stated relations and graphs",
            );
        }
        let written = self.positional(module, atom, &columns)?;
        let mut terms = Vec::new();
        for (term, column) in written.iter().zip(&columns) {
            let slot = Slot {
                column,
                is_negated: atom.is_negated,
            };
            terms.push(self.atom_term(module, *term, slot, variables));
        }
        Some(model::Literal::Atom {
            is_negated: atom.is_negated,
            predicate,
            snapshot: atom.snapshot,
            terms,
        })
    }

    /// The atom's terms, one per column; columns named arguments leave out
    /// are `None`.
    fn positional<'t>(
        &mut self,
        module: &str,
        atom: &'t ast::Atom,
        columns: &[Column],
    ) -> Option<Vec<Option<&'t ast::Term>>> {
        match &atom.arguments {
            AtomArguments::Positional(terms) => {
                if terms.len() != columns.len() {
                    self.error(
                        module,
                        atom.span,
                        format!(
                            "`{}` has {} columns; this gives {}",
                            atom.predicate.dotted(),
                            columns.len(),
                            terms.len()
                        ),
                    );
                    return None;
                }
                Some(terms.iter().map(Some).collect())
            }
            AtomArguments::Named(named) => Some(self.named_terms(module, atom, named, columns)),
        }
    }

    /// Named arguments placed at their columns.
    fn named_terms<'t>(
        &mut self,
        module: &str,
        atom: &ast::Atom,
        named: &'t [ast::NamedTerm],
        columns: &[Column],
    ) -> Vec<Option<&'t ast::Term>> {
        let mut written: Vec<Option<&ast::Term>> = vec![None; columns.len()];
        for argument in named {
            let Some(position) = columns
                .iter()
                .position(|column| column.name == argument.name.text)
            else {
                self.report_unknown_column(module, atom, &argument.name, columns);
                continue;
            };
            if written[position].is_some() {
                self.error(
                    module,
                    argument.name.span,
                    format!("column `{}` is given twice", argument.name.text),
                );
            }
            written[position] = Some(&argument.term);
        }
        written
    }

    fn report_unknown_column(
        &mut self,
        module: &str,
        atom: &ast::Atom,
        name: &ast::Name,
        columns: &[Column],
    ) {
        let names: Vec<String> = columns
            .iter()
            .map(|column| format!("`{}`", column.name))
            .collect();
        let message = format!(
            "`{}` has no column `{}`; it has {}",
            atom.predicate.dotted(),
            name.text,
            names.join(", ")
        );
        self.error(module, name.span, message);
    }

    fn atom_term(
        &mut self,
        module: &str,
        term: Option<&ast::Term>,
        slot: Slot,
        variables: &mut Variables,
    ) -> Term {
        let Slot { column, is_negated } = slot;
        let Some(term) = term else {
            return Term::Wildcard;
        };
        match &term.kind {
            TermKind::Wildcard => Term::Wildcard,
            TermKind::Variable(name) => {
                if is_negated && variables.bound(name).is_none() {
                    self.error(
                        module,
                        term.span,
                        format!("`{name}` must be bound by a positive literal before a negation can use it"),
                    );
                }
                if let Err((first, here)) =
                    variables.note(name, column.column_type.clone(), !is_negated)
                {
                    self.error(
                        module,
                        term.span,
                        format!(
                            "`{name}` is `{first}` elsewhere in this clause, and `{here}` here"
                        ),
                    );
                }
                Term::Variable(name.clone())
            }
            TermKind::Field { .. } => {
                self.error(
                    module,
                    term.span,
                    "a field of a variable can only be compared, as in `x.name == \"...\"`",
                );
                Term::Wildcard
            }
            _ => self.constant_term(module, term, &column.column_type),
        }
    }

    /// A literal or a constant path, checked against `expected`.
    fn constant_term(&mut self, module: &str, term: &ast::Term, expected: &Type) -> Term {
        let (found, value) = self.term_value(module, term);
        if !found.fits(expected) {
            self.error(
                module,
                term.span,
                format!("expected `{expected}`, found `{found}`"),
            );
        }
        if let Some(value) = value {
            return Term::Constant(value);
        }
        if found != Type::Unknown {
            self.error(
                module,
                term.span,
                "a constant must be known without running anything",
            );
        }
        Term::Wildcard
    }

    fn term_value(&mut self, module: &str, term: &ast::Term) -> (Type, Option<Value>) {
        match &term.kind {
            TermKind::Text(text) => (Type::Text, Some(Value::Text(text.clone()))),
            TermKind::Integer(value) => (Type::Int, Some(Value::Int(*value))),
            TermKind::Boolean(value) => (Type::Bool, Some(Value::Bool(*value))),
            TermKind::Null => (Type::Null, Some(Value::Null)),
            TermKind::Constant(path) => self.constant_path(module, path, term.span),
            TermKind::Variable(_) | TermKind::Wildcard | TermKind::Field { .. } => {
                (Type::Unknown, None)
            }
        }
    }

    /// Evaluates `Name.member["key"]` or `self.name` as an expression.
    fn constant_path(
        &mut self,
        module: &str,
        path: &ast::ConstantPath,
        span: Span,
    ) -> (Type, Option<Value>) {
        let mut expression = match &path.root {
            Some(root) => Expression {
                kind: ExpressionKind::Name(root.text.clone()),
                span: root.span,
            },
            None => Expression {
                kind: ExpressionKind::SelfModule,
                span,
            },
        };
        for step in &path.steps {
            let step_span = match step {
                PathStep::Member(name) | PathStep::Key(name) => name.span,
            };
            let start = expression.span;
            expression = match step {
                PathStep::Member(name) => Expression {
                    kind: ExpressionKind::Member {
                        target: Box::new(expression),
                        name: name.clone(),
                    },
                    span: start.to(step_span),
                },
                PathStep::Key(key) => Expression {
                    kind: ExpressionKind::Index {
                        target: Box::new(expression),
                        keys: vec![Expression {
                            kind: ExpressionKind::Text(key.text.clone()),
                            span: key.span,
                        }],
                    },
                    span: start.to(step_span),
                },
            };
        }
        self.evaluate(&Context::new(module), &expression)
    }

    fn comparison(
        &mut self,
        module: &str,
        comparison: &ast::Comparison,
        variables: &mut Variables,
    ) -> model::Literal {
        let (left, left_type) = self.compared_term(module, &comparison.left, variables);
        let (right, right_type) = self.compared_term(module, &comparison.right, variables);
        let comparable = left_type.fits(&right_type) || right_type.fits(&left_type);
        if !comparable {
            self.error(
                module,
                comparison.span,
                format!("`{left_type}` and `{right_type}` cannot be compared"),
            );
        }
        let is_ordering = !matches!(
            comparison.operator,
            ast::BinaryOperator::Equal | ast::BinaryOperator::NotEqual
        );
        if is_ordering && !left_type.fits(&Type::Int) {
            self.error(
                module,
                comparison.span,
                format!("`{}` compares integers", comparison.operator.as_str()),
            );
        }
        model::Literal::Comparison {
            left,
            operator: comparison.operator,
            right,
        }
    }

    fn compared_term(
        &mut self,
        module: &str,
        term: &ast::Term,
        variables: &Variables,
    ) -> (Term, Type) {
        match &term.kind {
            TermKind::Variable(name) => {
                let Some(found) = variables.bound(name) else {
                    self.error(
                        module,
                        term.span,
                        format!(
                            "`{name}` must be bound by a positive literal before it is compared"
                        ),
                    );
                    return (Term::Variable(name.clone()), Type::Unknown);
                };
                (Term::Variable(name.clone()), found.clone())
            }
            TermKind::Field { variable, field } => {
                let Some(found) = variables.bound(&variable.text).cloned() else {
                    self.error(
                        module,
                        variable.span,
                        format!(
                            "`{}` must be bound by a positive literal before it is compared",
                            variable.text
                        ),
                    );
                    return (Term::Wildcard, Type::Unknown);
                };
                let field_type = self.field_type(module, &found, field);
                let term = Term::Field {
                    variable: variable.text.clone(),
                    field: field.text.clone(),
                };
                (term, field_type)
            }
            TermKind::Wildcard => {
                self.error(module, term.span, "`_` cannot be compared");
                (Term::Wildcard, Type::Unknown)
            }
            _ => {
                let (found, value) = self.term_value(module, term);
                (value.map_or(Term::Wildcard, Term::Constant), found)
            }
        }
    }

    /// The type of a variable's field, for `x.field` in a comparison.
    fn field_type(&mut self, module: &str, variable_type: &Type, field: &ast::Name) -> Type {
        match variable_type {
            Type::Symbol | Type::Site => match field.text.as_str() {
                "line" | "column" => Type::Int,
                "repository" | "file" | "name" | "kind" => Type::Text,
                _ => {
                    self.error(
                        module,
                        field.span,
                        format!("a `{variable_type}` has no field `{}`", field.text),
                    );
                    Type::Unknown
                }
            },
            Type::Entity(entity) => {
                let found = self.entity_field(entity, &field.text);
                if found.is_none() {
                    self.error(
                        module,
                        field.span,
                        format!("`{}` has no field `{}`", entity.name(), field.text),
                    );
                }
                found.unwrap_or(Type::Unknown)
            }
            Type::Unknown => Type::Unknown,
            other => {
                self.error(
                    module,
                    field.span,
                    format!("a `{other}` has no fields to compare"),
                );
                Type::Unknown
            }
        }
    }

    fn entity_field(&mut self, entity: &Address, name: &str) -> Option<Type> {
        let id = *self.by_address.get(entity)?;
        let declared = self.declarations[id].clone();
        let NodeRef::Top(Node::Entity(declaration)) = declared.node else {
            return None;
        };
        let field = declaration
            .fields
            .iter()
            .find(|field| field.name.text == name)?;
        Some(self.resolve_type(declared.module, &field.field_type))
    }

    /// What an atom's predicate names, and its columns.
    fn predicate(
        &mut self,
        module: &str,
        name: &ast::ModuleName,
    ) -> Option<(Predicate, Vec<Column>)> {
        let entry = self.predicate_entry(module, name)?;
        self.refer_entry(module, name.span, &entry);
        let Entry::Declaration(id) = entry else {
            self.error(
                module,
                name.span,
                format!("`{}` is not a relation, rule or graph", name.dotted()),
            );
            return None;
        };
        let address = self.declarations[id].address.clone();
        let predicate = match self.declarations[id].node {
            NodeRef::Top(Node::Relation(_)) => Predicate::Relation(address),
            NodeRef::Top(Node::Rule(_)) => Predicate::Rule(address),
            NodeRef::Top(Node::Graph(_)) => Predicate::Graph(address),
            _ => {
                self.error(
                    module,
                    name.span,
                    format!("`{}` is not a relation, rule or graph", name.dotted()),
                );
                return None;
            }
        };
        let columns = self.columns(id)?;
        Some((predicate, columns))
    }

    fn predicate_entry(&mut self, module: &str, name: &ast::ModuleName) -> Option<Entry> {
        let segments = &name.segments;
        if let [single] = segments.as_slice() {
            let found = self.lookup(module, &single.text);
            if found.is_none() {
                self.error(
                    module,
                    single.span,
                    format!("there is no relation, rule or graph `{}`", single.text),
                );
            }
            return found;
        }
        let (last, owner) = segments.split_last()?;
        let namespace = match owner {
            [namespace] => self.lookup(module, &namespace.text),
            _ => None,
        };
        let owner_module = if let Some(Entry::Namespace(target)) = namespace {
            target
        } else {
            owner
                .iter()
                .map(|segment| segment.text.as_str())
                .collect::<Vec<_>>()
                .join(".")
        };
        let found = self.export_of(
            &owner_module,
            &last.text,
            &mut std::collections::BTreeSet::new(),
        );
        if found.is_none() {
            self.error(
                module,
                name.span,
                format!("there is no relation, rule or graph `{}`", name.dotted()),
            );
        } else {
            self.refer(module, owner[0].span, Target::Module(owner_module));
        }
        found
    }

    pub(super) fn check_require(
        &mut self,
        id: DeclarationId,
        require: &ast::RequireDeclaration,
    ) -> String {
        let declared = self.declarations[id].clone();
        let module = declared.module;
        let mut variables = Variables::default();
        let body = if let Some(when) = &require.when {
            self.literals(module, &when.literals, &mut variables)
        } else {
            self.error(
                module,
                declared.name_span,
                "a requirement needs `when:` and the condition its findings match",
            );
            Vec::new()
        };
        let fields = self.require_fields(module, require, &variables);
        let finding_variables = variables
            .order
            .iter()
            .filter_map(|name| {
                variables
                    .bound(name)
                    .map(|found| (name.clone(), found.clone()))
            })
            .collect();
        self.requirement_variables
            .insert(declared.address.clone(), finding_variables);
        self.model.requirements.push(model::Requirement {
            address: declared.address,
            severity: fields.severity,
            body,
            message: fields.message,
            at: fields.at,
        });
        "requirement".to_owned()
    }

    /// A requirement's `severity`, `message` and `at`, checked against the
    /// variables of its `when`.
    fn require_fields(
        &mut self,
        module: &str,
        require: &ast::RequireDeclaration,
        variables: &Variables,
    ) -> RequireFields {
        let context = Context::new(module);
        let at = require.fields.iter().find(|field| field.name.text == "at");
        let others: Vec<&ast::Field> = require
            .fields
            .iter()
            .filter(|field| field.name.text != "at")
            .collect();
        let schema = [
            RecordField {
                name: "severity".to_owned(),
                field_type: stdlib::SEVERITY,
                is_optional: true,
            },
            RecordField {
                name: "message".to_owned(),
                field_type: Type::Text,
                is_optional: true,
            },
        ];
        let values = self.check_fields(&context, &others, &schema, "a requirement");
        let message = values
            .get("message")
            .and_then(Value::as_text)
            .map(str::to_owned);
        let message_field = others.iter().find(|field| field.name.text == "message");
        if let (Some(text), Some(field)) = (&message, message_field) {
            self.check_placeholders(module, text, field.value.span, variables);
        }
        RequireFields {
            severity: values
                .get("severity")
                .and_then(super::declarations::severity_of)
                .unwrap_or_default(),
            message,
            at: at.and_then(|field| self.at_variable(module, field, variables)),
        }
    }

    fn at_variable(
        &mut self,
        module: &str,
        field: &ast::Field,
        variables: &Variables,
    ) -> Option<String> {
        let ExpressionKind::Name(name) = &field.value.kind else {
            self.error(
                module,
                field.value.span,
                "`at` names a variable of `when` holding the finding's site",
            );
            return None;
        };
        if variables.bound(name).is_none() {
            self.error(
                module,
                field.value.span,
                format!("`{name}` is not a variable of `when`"),
            );
            return None;
        }
        Some(name.clone())
    }

    fn check_placeholders(&mut self, module: &str, text: &str, span: Span, variables: &Variables) {
        for placeholder in placeholders(text) {
            let variable = placeholder.split('.').next().unwrap_or(&placeholder);
            if variables.bound(variable).is_none() {
                self.error(
                    module,
                    span,
                    format!("`{{{placeholder}}}` names no variable of `when`"),
                );
            }
        }
    }

    /// Checks an allowance's body: the requirement's finding variables are
    /// bound, and the body may use them and further literals.
    pub(super) fn check_body(
        &mut self,
        module: &str,
        body: &ast::Body,
        bound: Option<&Vec<(String, Type)>>,
    ) -> Vec<model::Literal> {
        let mut variables = Variables::default();
        for (name, variable_type) in bound.into_iter().flatten() {
            // Names are fresh here, so noting them cannot conflict.
            let _ = variables.note(name, variable_type.clone(), true);
        }
        self.literals(module, &body.literals, &mut variables)
    }
}

/// The `{name}` and `{name.field}` placeholders of a message, `{{` and
/// `}}` being literal braces.
pub fn placeholders(text: &str) -> Vec<String> {
    let mut found = Vec::new();
    let mut characters = text.chars().peekable();
    while let Some(character) = characters.next() {
        if character != '{' {
            continue;
        }
        if characters.peek() == Some(&'{') {
            characters.next();
            continue;
        }
        let name: String = characters
            .by_ref()
            .take_while(|next| *next != '}')
            .collect();
        if !name.is_empty() {
            found.push(name);
        }
    }
    found
}
