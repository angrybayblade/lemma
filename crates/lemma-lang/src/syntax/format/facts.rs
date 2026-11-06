//! What the formatter needs from the tree: which `<` and `>` tokens
//! bracket type arguments, and which `:` tokens name a graph's or rule's
//! relation. Tokens alone cannot tell `Fact<Link>` from `a < b`.

use std::collections::BTreeSet;

use crate::syntax::ast::{
    Binding, Block, Declaration, Else, File, IfStatement, Item, RuleColumns, ServiceDeclaration,
    ServiceMember, Statement, TypeExpression, TypedField,
};

/// Token positions the spacing rules treat specially, as byte offsets.
#[derive(Debug, Default)]
pub(super) struct Facts {
    /// Where a type name with arguments ends: the `<` after it opens them.
    pub generic_opens: BTreeSet<u32>,
    /// Where a type with arguments ends: the `>` ending there closes them.
    pub generic_closes: BTreeSet<u32>,
    /// Where a graph's or rule's name ends: the `:` after it names its
    /// relation, and is written with a space on each side.
    pub relation_colons: BTreeSet<u32>,
}

/// Collects the facts of a parsed module.
pub(super) fn collect(file: &File) -> Facts {
    let mut facts = Facts::default();
    for item in &file.items {
        if let Item::Declaration(declared) = item {
            facts.declaration(&declared.declaration);
        }
    }
    facts
}

impl Facts {
    fn declaration(&mut self, declaration: &Declaration) {
        match declaration {
            Declaration::Binding(binding) => self.binding(binding),
            Declaration::Type(declaration) => self.type_expression(&declaration.value),
            Declaration::Entity(declaration) => self.typed_fields(&declaration.fields),
            Declaration::Relation(declaration) => self.typed_fields(&declaration.columns),
            Declaration::Service(service) => self.service(service),
            Declaration::Graph(graph) => {
                self.relation_colons.insert(graph.name.span.end);
                self.type_expression(&graph.relation);
            }
            Declaration::Rule(rule) => match &rule.columns {
                RuleColumns::Relation(relation) => {
                    self.relation_colons.insert(rule.name.span.end);
                    self.type_expression(relation);
                }
                RuleColumns::Inline(columns) => self.typed_fields(columns),
            },
            Declaration::Behavior(behavior) => {
                self.typed_fields(behavior.parameters.as_deref().unwrap_or_default());
                self.block(&behavior.body);
            }
            Declaration::Function(function) => {
                self.typed_fields(&function.parameters);
                self.type_expression(&function.returns);
                self.block(&function.body);
            }
            Declaration::Checker(checker) => {
                self.typed_fields(&checker.parameters);
                self.type_expression(&checker.returns);
            }
            Declaration::Require(_) | Declaration::Allow(_) | Declaration::Check(_) => {}
        }
    }

    fn service(&mut self, service: &ServiceDeclaration) {
        for member in &service.members {
            match member {
                ServiceMember::Service { service, .. } => self.service(service),
                ServiceMember::Binding(binding) => self.binding(binding),
                ServiceMember::Operation { .. } | ServiceMember::Field(_) => {}
            }
        }
    }

    fn binding(&mut self, binding: &Binding) {
        if let Some(declared) = &binding.declared_type {
            self.type_expression(declared);
        }
    }

    fn block(&mut self, block: &Block) {
        for statement in &block.statements {
            match statement {
                Statement::Binding(binding) => self.binding(binding),
                Statement::If(statement) => self.if_statement(statement),
                Statement::For { body, .. } => self.block(body),
                Statement::Field(_) | Statement::Expect { .. } | Statement::Return { .. } => {}
            }
        }
    }

    fn if_statement(&mut self, statement: &IfStatement) {
        self.block(&statement.then);
        match &statement.otherwise {
            Some(Else::Block(block)) => self.block(block),
            Some(Else::If(next)) => self.if_statement(next),
            None => {}
        }
    }

    fn typed_fields(&mut self, fields: &[TypedField]) {
        for field in fields {
            self.type_expression(&field.field_type);
        }
    }

    fn type_expression(&mut self, written: &TypeExpression) {
        match written {
            TypeExpression::Named {
                name,
                arguments,
                span,
            } => {
                if !arguments.is_empty() {
                    self.generic_opens.insert(name.span.end);
                    self.generic_closes.insert(span.end);
                }
                for argument in arguments {
                    self.type_expression(argument);
                }
            }
            TypeExpression::Record { fields, .. } => self.typed_fields(fields),
        }
    }
}
