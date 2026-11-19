//! Python comprehensions and generator expressions.

use tree_sitter::Node;

use super::super::ir::{Clause, Collects, Comprehension, Expression as Expr};
use super::{Reader, children, field, is_async, only};

impl Reader<'_> {
    /// `[element for item in iterable if test]`, and its set, dict and
    /// generator forms.
    pub(super) fn comprehension(&self, node: Node<'_>) -> Result<Expr, String> {
        let body = field(node, "body")?;
        let (collects, element) = match node.kind() {
            "list_comprehension" => (Collects::Sequence, self.expression(body)?),
            "set_comprehension" => (Collects::Set, self.expression(body)?),
            "generator_expression" => (Collects::Generator, self.expression(body)?),
            "dictionary_comprehension" if body.kind() == "pair" => (
                Collects::Record(self.expression(field(body, "key")?)?),
                self.expression(field(body, "value")?)?,
            ),
            _ => return self.unsupported(node),
        };
        let mut clauses = Vec::new();
        for clause in children(node) {
            if clause.id() == body.id() {
                continue;
            }
            clauses.push(match clause.kind() {
                "for_in_clause" => self.for_clause(clause)?,
                "if_clause" => Clause::If(self.expression(only(clause)?)?),
                _ => return self.unsupported(clause),
            });
        }
        if !matches!(clauses.first(), Some(Clause::For(..))) {
            return self.unsupported(node);
        }
        Ok(Expr::Comprehension(Box::new(Comprehension {
            collects,
            element,
            clauses,
        })))
    }

    /// `for pattern in iterable`. `async for` awaits each item of an
    /// asynchronous iterator, and `for x in a, b` runs over a tuple written
    /// without its parentheses; neither is read.
    fn for_clause(&self, node: Node<'_>) -> Result<Clause, String> {
        let mut cursor = node.walk();
        let iterables: Vec<Node<'_>> = node.children_by_field_name("right", &mut cursor).collect();
        let [iterable] = iterables.as_slice() else {
            return self.unsupported(node);
        };
        if is_async(node) {
            return self.unsupported(node);
        }
        Ok(Clause::For(
            self.pattern(field(node, "left")?)?,
            self.expression(*iterable)?,
        ))
    }
}
