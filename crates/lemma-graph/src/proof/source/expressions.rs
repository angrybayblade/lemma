//! Expressions, read into the shared representation.

use std::collections::BTreeMap;

use lemma_lang::syntax::ast::BinaryOperator as Op;
use tree_sitter::Node;

use super::super::ir::Expression as Expr;
use super::{Language, Reader, children, field, has_optional_chain, only};

impl Reader<'_> {
    pub(super) fn expression(&self, node: Node<'_>) -> Result<Expr, String> {
        if let Some(expression) = self.special_form(node)? {
            return Ok(expression);
        }
        Ok(match node.kind() {
            "true" => Expr::Bool(true),
            "false" => Expr::Bool(false),
            "none" | "null" | "nil" => Expr::Null,
            "undefined" => Expr::Undefined,
            "identifier" | "this" => Expr::Name(self.text(node).to_owned()),
            "number" | "integer" | "int_literal" => self.integer(node)?,
            "string"
            | "interpreted_string_literal"
            | "raw_string_literal"
            | "template_string"
            | "concatenated_string" => self.text_expression(node)?,
            "parenthesized_expression" | "literal_element" => self.expression(only(node)?)?,
            "await_expression" | "await" => {
                let inner = only(node)?;
                if !matches!(inner.kind(), "call" | "call_expression") {
                    return self.unsupported(node);
                }
                Expr::Await(Box::new(self.expression(inner)?))
            }
            // A field read, in every language, under the precondition that the
            // value is plain data: no getter, descriptor or `__getattr__` runs.
            "member_expression" | "selector_expression" | "attribute" => self.member(node)?,
            "subscript" | "subscript_expression" | "index_expression" => self.index(node)?,
            "unary_expression" | "not_operator" | "unary_operator" => self.unary(node)?,
            "binary_expression"
            | "boolean_operator"
            | "comparison_operator"
            | "binary_operator" => self.binary(node)?,
            "ternary_expression" => Expr::Conditional(
                Box::new(self.expression(field(node, "condition")?)?),
                Box::new(self.expression(field(node, "consequence")?)?),
                Box::new(self.expression(field(node, "alternative")?)?),
            ),
            "object" | "dictionary" | "composite_literal" => self.record(node)?,
            "expression_list" if children(node).len() == 1 => self.expression(only(node)?)?,

            "list" | "tuple" | "array" | "expression_list" => Expr::Sequence(
                children(node)
                    .into_iter()
                    .map(|element| self.expression(element))
                    .collect::<Result<_, _>>()?,
            ),
            "call_expression" | "call" if has_optional_chain(node) => {
                return self.unsupported(node);
            }
            "call_expression" | "call" => {
                let function = self.expression(field(node, "function")?)?;
                let arguments = children(field(node, "arguments")?)
                    .into_iter()
                    .map(|n| self.argument(n))
                    .collect::<Result<_, _>>()?;
                Expr::Call(Box::new(function), arguments)
            }
            _ => return self.unsupported(node),
        })
    }

    /// Type syntax that does nothing at runtime, Python's conditional
    /// expression and set literals.
    pub(super) fn special_form(&self, node: Node<'_>) -> Result<Option<Expr>, String> {
        let first = || children(node).first().copied().ok_or("empty expression");
        Ok(Some(match node.kind() {
            "as_expression" | "satisfies_expression" => self.expression(first()?)?,
            // The grammar groups `a && b!` as `(a && b)!`, where TypeScript's
            // postfix `!` binds tighter than any operator; accept only the
            // operands TypeScript itself could give it.
            "non_null_expression" => {
                let operand = first()?;
                if !matches!(
                    operand.kind(),
                    "identifier"
                        | "this"
                        | "member_expression"
                        | "subscript_expression"
                        | "call_expression"
                        | "parenthesized_expression"
                        | "non_null_expression"
                ) {
                    return self.unsupported(node);
                }
                self.expression(operand)?
            }
            "type_assertion" if self.language == Language::TypeScript => {
                self.expression(*children(node).last().ok_or("empty type assertion")?)?
            }
            "conditional_expression" => {
                let [then, test, otherwise] = children(node)[..] else {
                    return self.unsupported(node);
                };
                Expr::Conditional(
                    Box::new(self.expression(test)?),
                    Box::new(self.expression(then)?),
                    Box::new(self.expression(otherwise)?),
                )
            }
            "set" => Expr::Set(
                children(node)
                    .into_iter()
                    .map(|element| self.expression(element))
                    .collect::<Result<_, _>>()?,
            ),
            _ => return Ok(None),
        }))
    }

    pub(super) fn integer(&self, node: Node<'_>) -> Result<Expr, String> {
        let text = self.text(node);
        if !text.bytes().all(|c| c.is_ascii_digit()) || (text.len() > 1 && text.starts_with('0')) {
            return self.unsupported(node);
        }
        let integer = text.parse::<i64>().map_err(|e| e.to_string())?;
        if integer > 9_007_199_254_740_991 {
            return self.unsupported(node);
        }
        Ok(Expr::Integer(integer))
    }

    pub(super) fn member(&self, node: Node<'_>) -> Result<Expr, String> {
        let target = node
            .child_by_field_name("object")
            .or_else(|| node.child_by_field_name("operand"))
            .ok_or("missing member target")?;
        let property = node
            .child_by_field_name("property")
            .or_else(|| node.child_by_field_name("field"))
            .or_else(|| node.child_by_field_name("attribute"))
            .ok_or("missing member name")?;
        let target = Box::new(self.expression(target)?);
        let name = self.identifier(property)?;
        if has_optional_chain(node) {
            return Ok(Expr::OptionalMember(target, name));
        }
        Ok(Expr::Member(target, name))
    }

    pub(super) fn index(&self, node: Node<'_>) -> Result<Expr, String> {
        let target = node
            .child_by_field_name("value")
            .or_else(|| node.child_by_field_name("object"))
            .or_else(|| node.child_by_field_name("operand"))
            .ok_or("missing index target")?;
        let key = node
            .child_by_field_name("subscript")
            .or_else(|| node.child_by_field_name("index"))
            .ok_or("missing index")?;
        let target = Box::new(self.expression(target)?);
        // Only constant keys: a variable key may name a field the contract
        // never declared, whose value nothing gives.
        if let Some(position) = self.constant_integer(key) {
            if has_optional_chain(node) {
                return self.unsupported(node);
            }
            return Ok(Expr::Index(target, position));
        }
        let key = self.string(key)?;
        if has_optional_chain(node) {
            return Ok(Expr::OptionalMember(target, key));
        }
        Ok(Expr::Member(target, key))
    }

    /// An integer literal, possibly negated.
    pub(super) fn constant_integer(&self, node: Node<'_>) -> Option<i64> {
        match node.kind() {
            "integer" | "number" | "int_literal" => match self.integer(node).ok()? {
                Expr::Integer(value) => Some(value),
                _ => None,
            },
            "unary_operator" | "unary_expression"
                if self.source[node.byte_range()].trim_start().starts_with('-') =>
            {
                let operand = children(node).last().copied()?;
                self.constant_integer(operand).map(i64::wrapping_neg)
            }
            _ => None,
        }
    }

    pub(super) fn unary(&self, node: Node<'_>) -> Result<Expr, String> {
        let operand = node
            .child_by_field_name("argument")
            .or_else(|| node.child_by_field_name("operand"))
            .or_else(|| children(node).last().copied())
            .ok_or("missing unary operand")?;
        let operator = self.source[node.start_byte()..operand.start_byte()].trim();
        match operator {
            "!" | "not" => Ok(Expr::Not(Box::new(self.expression(operand)?))),
            "-" => Ok(Expr::Negate(Box::new(self.expression(operand)?))),
            // A pointer to a fresh struct reads as the struct: nothing here
            // compares pointers or writes through them.
            "&" if operand.kind() == "composite_literal" => self.expression(operand),
            _ => self.unsupported(node),
        }
    }

    pub(super) fn binary(&self, node: Node<'_>) -> Result<Expr, String> {
        let operands = children(node);
        let left = node
            .child_by_field_name("left")
            .or_else(|| operands.first().copied())
            .ok_or("missing left operand")?;
        let right = node
            .child_by_field_name("right")
            .or_else(|| operands.last().copied())
            .ok_or("missing right operand")?;
        if operands.len() > 2 && node.kind() == "comparison_operator" {
            return self.chained_comparison(node, &operands);
        }
        if operands.len() != 2 {
            return self.unsupported(node);
        }
        let operator_text = self.source[left.end_byte()..right.start_byte()].trim();
        if operator_text == "??" {
            return Ok(Expr::Coalesce(
                Box::new(self.expression(left)?),
                Box::new(self.expression(right)?),
            ));
        }
        // Python's `is` compares identity, which is equality only for the
        // singletons `None`, `True` and `False`.
        if matches!(operator_text, "is" | "is not")
            && ![left, right]
                .iter()
                .any(|side| matches!(side.kind(), "none" | "true" | "false"))
        {
            return self.unsupported(node);
        }
        if let Some(membership) = self.membership(operator_text, left, right)? {
            return Ok(membership);
        }
        let operator = match operator_text {
            "&&" | "and" => Op::And,
            "||" | "or" => Op::Or,
            "===" | "is" => Op::Equal,
            "!==" | "is not" => Op::NotEqual,
            "==" if self.language != Language::TypeScript => Op::Equal,
            "!=" if self.language != Language::TypeScript => Op::NotEqual,
            "<" => Op::Less,
            "<=" => Op::LessOrEqual,
            ">" => Op::Greater,
            ">=" => Op::GreaterOrEqual,
            "+" => Op::Add,
            "-" => Op::Subtract,
            "*" => Op::Multiply,
            _ => return self.unsupported(node),
        };
        Ok(Expr::Binary(
            operator,
            Box::new(self.expression(left)?),
            Box::new(self.expression(right)?),
        ))
    }

    /// Python's `a < b <= c`: each neighbouring pair compared, each operand
    /// evaluated once. Read only when no operand calls a function, so that
    /// comparing a middle operand twice changes nothing.
    pub(super) fn chained_comparison(
        &self,
        node: Node<'_>,
        operands: &[Node<'_>],
    ) -> Result<Expr, String> {
        let mut chain: Option<Expr> = None;
        for pair in operands.windows(2) {
            let [left, right] = pair else {
                return self.unsupported(node);
            };
            let operator = self.source[left.end_byte()..right.start_byte()].trim();
            let comparison = if let Some(membership) = self.membership(operator, *left, *right)? {
                membership
            } else {
                let operator = match operator {
                    "==" => Op::Equal,
                    "!=" => Op::NotEqual,
                    "<" => Op::Less,
                    "<=" => Op::LessOrEqual,
                    ">" => Op::Greater,
                    ">=" => Op::GreaterOrEqual,
                    _ => return self.unsupported(node),
                };
                Expr::Binary(
                    operator,
                    Box::new(self.expression(*left)?),
                    Box::new(self.expression(*right)?),
                )
            };
            if comparison.has_call() {
                return self.unsupported(node);
            }
            chain = Some(match chain {
                Some(earlier) => Expr::Binary(Op::And, Box::new(earlier), Box::new(comparison)),
                None => comparison,
            });
        }
        chain.ok_or_else(|| self.refusal(node))
    }

    /// Python's `in` and `not in`.
    pub(super) fn membership(
        &self,
        operator: &str,
        left: Node<'_>,
        right: Node<'_>,
    ) -> Result<Option<Expr>, String> {
        if self.language != Language::Python || !matches!(operator, "in" | "not in") {
            return Ok(None);
        }
        let contains = Expr::Contains(
            Box::new(self.expression(left)?),
            Box::new(self.expression(right)?),
        );
        if operator == "in" {
            return Ok(Some(contains));
        }
        Ok(Some(Expr::Not(Box::new(contains))))
    }

    pub(super) fn record(&self, node: Node<'_>) -> Result<Expr, String> {
        let body = if node.kind() == "composite_literal" {
            match field(node, "type")?.kind() {
                "type_identifier" | "qualified_type" => {}
                "slice_type" | "array_type" => return self.go_sequence(field(node, "body")?),
                _ => return self.unsupported(node),
            }
            field(node, "body")?
        } else {
            node
        };
        let mut record = BTreeMap::new();
        for entry in children(body) {
            let (key, value) = self.record_entry(entry)?;
            if record.insert(key, value).is_some() {
                return Err("duplicate record field".to_owned());
            }
        }
        Ok(Expr::Record(record))
    }

    /// The elements of a Go slice or array literal, without keys.
    pub(super) fn go_sequence(&self, body: Node<'_>) -> Result<Expr, String> {
        let mut items = Vec::new();
        for element in children(body) {
            if element.kind() != "literal_element" {
                return self.unsupported(element);
            }
            items.push(self.expression(only(element)?)?);
        }
        Ok(Expr::Sequence(items))
    }

    pub(super) fn record_entry(&self, entry: Node<'_>) -> Result<(String, Expr), String> {
        if entry.kind() == "shorthand_property_identifier" {
            let name = self.text(entry).to_owned();
            return Ok((name.clone(), Expr::Name(name)));
        }
        let (key, value) = match entry.kind() {
            "pair" => (field(entry, "key")?, field(entry, "value")?),
            "keyed_element" => (only(field(entry, "key")?)?, field(entry, "value")?),
            _ => return self.unsupported(entry),
        };
        let key = if matches!(key.kind(), "string" | "interpreted_string_literal") {
            self.string(key)?
        } else {
            self.identifier(key)?
        };
        if self.language == Language::TypeScript
            && matches!(key.as_str(), "__proto__" | "constructor" | "prototype")
        {
            return Err("prototype-sensitive object fields are unsupported".to_owned());
        }
        Ok((key, self.expression(value)?))
    }
}
