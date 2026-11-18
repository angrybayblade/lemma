//! Loops over sequences, and leaving one early.

use tree_sitter::Node;

use super::super::ir::{ForEach, Statement, StatementKind as Stmt};
use super::{Language, Reader, children, field, is_async};

impl Reader<'_> {
    /// Python's `for`, or Go's `for … range`. Go's other `for` loops run
    /// until a condition fails, which is not a loop over a sequence.
    pub(super) fn for_statement(&self, node: Node<'_>) -> Result<Stmt, String> {
        match self.language {
            Language::Python => self.python_for(node),
            Language::Go => self.go_range(node),
            Language::TypeScript => self.unsupported(node),
        }
    }

    fn python_for(&self, node: Node<'_>) -> Result<Stmt, String> {
        // `async for` awaits each item of an asynchronous iterator.
        if is_async(node) {
            return self.unsupported(node);
        }
        let otherwise = match node.child_by_field_name("alternative") {
            Some(clause) => self.block(field(clause, "body")?)?,
            None => Vec::new(),
        };
        Ok(Stmt::ForEach(Box::new(ForEach {
            index: None,
            item: Some(self.pattern(field(node, "left")?)?),
            iterable: self.expression(field(node, "right")?)?,
            body: self.block(field(node, "body")?)?,
            otherwise,
        })))
    }

    /// `for index, item := range sequence`, with either name left out.
    fn go_range(&self, node: Node<'_>) -> Result<Stmt, String> {
        let Some(clause) = children(node)
            .into_iter()
            .find(|part| part.kind() == "range_clause")
        else {
            return self.unsupported(node);
        };
        let names = match clause.child_by_field_name("left") {
            // `=` assigns variables declared before the loop, which then
            // outlive it; only `:=`, which declares the loop's own, is read.
            Some(_) if !self.has_token(clause, ":=") => return self.unsupported(clause),
            Some(left) => children(left),
            None => Vec::new(),
        };
        let (index, item) = match names.as_slice() {
            [] => (None, None),
            [index] => (Some(self.pattern(*index)?), None),
            [index, item] => (Some(self.pattern(*index)?), Some(self.pattern(*item)?)),
            _ => return self.unsupported(clause),
        };
        Ok(Stmt::ForEach(Box::new(ForEach {
            index,
            item,
            iterable: self.expression(field(clause, "right")?)?,
            body: self.block(field(node, "body")?)?,
            otherwise: Vec::new(),
        })))
    }

    /// JavaScript's `for (const item of sequence)`. `for … in` reads an
    /// object's keys in an order objects here do not keep.
    pub(super) fn for_of(&self, node: Node<'_>) -> Result<Stmt, String> {
        let is_of = node
            .child_by_field_name("operator")
            .is_some_and(|operator| self.text(operator) == "of");
        // `for await` awaits each item; `var` and a bare name outlive the loop.
        let declares_its_own = node
            .child_by_field_name("kind")
            .is_some_and(|kind| matches!(self.text(kind), "const" | "let"));
        if !is_of || !declares_its_own || self.has_token(node, "await") {
            return self.unsupported(node);
        }
        Ok(Stmt::ForEach(Box::new(ForEach {
            index: None,
            item: Some(self.pattern(field(node, "left")?)?),
            iterable: self.expression(field(node, "right")?)?,
            body: self.loop_body(field(node, "body")?)?,
            otherwise: Vec::new(),
        })))
    }

    /// `break` or `continue`. With a label it names another loop than the
    /// innermost, which is not followed.
    pub(super) fn jump(&self, node: Node<'_>) -> Result<Stmt, String> {
        if !children(node).is_empty() {
            return self.unsupported(node);
        }
        Ok(if node.kind() == "break_statement" {
            Stmt::Break
        } else {
            Stmt::Continue
        })
    }

    /// A loop's body: a block, or in JavaScript one statement.
    fn loop_body(&self, node: Node<'_>) -> Result<Vec<Statement>, String> {
        if matches!(node.kind(), "statement_block" | "block") {
            return self.block(node);
        }
        self.statement(node)
    }

    /// Whether `node` has the keyword or operator `token` as a child.
    fn has_token(&self, node: Node<'_>, token: &str) -> bool {
        let mut cursor = node.walk();
        node.children(&mut cursor)
            .any(|child| !child.is_named() && self.text(child) == token)
    }
}
