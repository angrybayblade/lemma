//! Statements: bindings, assignments, branches, raises, `try` and `with`.

use lemma_lang::syntax::ast::BinaryOperator as Op;
use tree_sitter::Node;

use super::super::ir::{
    Context, Expression as Expr, Handler, Pattern, Raised, Statement, StatementKind as Stmt, Try,
};
use super::{Definition, Language, Reader, children, field, is_async, only};

impl Reader<'_> {
    pub(super) fn block(&self, node: Node<'_>) -> Result<Vec<Statement>, String> {
        self.block_nodes(&children(node))
    }

    pub(super) fn block_nodes(&self, nodes: &[Node<'_>]) -> Result<Vec<Statement>, String> {
        let mut statements = Vec::new();
        // JavaScript hoists a function declaration to the top of its block,
        // so it can be called above where it is written.
        let (hoisted, nodes): (Vec<Node<'_>>, Vec<Node<'_>>) = nodes.iter().partition(|node| {
            self.language == Language::TypeScript && node.kind() == "function_declaration"
        });
        for child in hoisted.into_iter().chain(nodes) {
            if child.kind() == "statement_list" {
                statements.extend(self.block(child)?);
            } else {
                statements.extend(self.statement(child)?);
            }
        }
        Ok(statements)
    }

    pub(super) fn statement(&self, node: Node<'_>) -> Result<Vec<Statement>, String> {
        let kind = match node.kind() {
            "lexical_declaration" | "variable_declaration" | "var_declaration" => {
                return self.declarations(node);
            }
            // `let x;` declares `x` as undefined.
            "variable_declarator" => Stmt::Bind(
                self.pattern(field(node, "name")?)?,
                node.child_by_field_name("value")
                    .map_or(Ok(Expr::Undefined), |value| self.expression(value))?,
            ),
            "short_var_declaration"
            | "assignment_statement"
            | "assignment"
            | "assignment_expression"
            | "augmented_assignment"
            | "augmented_assignment_expression" => self.assignment(node)?,
            "raise_statement" | "throw_statement" | "assert_statement" => self.exit(node)?,
            "expression_statement" if self.is_go_panic(node) => Stmt::Raise(Raised::Unknown),
            "try_statement" => Stmt::Try(Box::new(self.try_statement(node)?)),
            "pass_statement" | "empty_statement" => return Ok(Vec::new()),
            "function_definition" | "function_declaration" | "decorated_definition" => {
                self.nested_definition(node)?
            }
            "with_statement" => self.with_statement(node)?,
            "for_statement" => self.for_statement(node)?,
            "for_in_statement" => self.for_of(node)?,
            // A `switch` is not read, so a `break` here always leaves a loop.
            "break_statement" | "continue_statement" => self.jump(node)?,
            "expression_statement" => {
                let expression = only(node)?;
                if matches!(
                    expression.kind(),
                    "assignment"
                        | "augmented_assignment"
                        | "assignment_expression"
                        | "augmented_assignment_expression"
                ) {
                    return self.statement(expression);
                }
                if !matches!(
                    expression.kind(),
                    "call" | "call_expression" | "await_expression" | "await"
                ) {
                    return self.unsupported(expression);
                }
                Stmt::Evaluate(self.expression(expression)?)
            }
            "return_statement" => Stmt::Return(self.return_value(node)?),
            "if_statement" | "elif_clause" => self.if_statement(node)?,
            _ => return self.unsupported(node),
        };
        Ok(vec![Statement {
            line: node.start_position().row + 1,
            kind,
        }])
    }

    /// A statement that may leave the function by raising. Nothing in a raise
    /// can make the path return normally, so what it raises is not read.
    pub(super) fn exit(&self, node: Node<'_>) -> Result<Stmt, String> {
        if node.kind() == "throw_statement" {
            return Ok(Stmt::Raise(Raised::Unknown));
        }
        if node.kind() == "raise_statement" {
            return Ok(Stmt::Raise(self.raised(node)));
        }
        let condition = children(node)
            .into_iter()
            .next()
            .ok_or("empty assert statement")?;
        Ok(Stmt::Assert(self.expression(condition)?))
    }

    /// A TypeScript `const` or `let`, or a Go `var`: one binding per name.
    fn declarations(&self, node: Node<'_>) -> Result<Vec<Statement>, String> {
        if node.kind() == "var_declaration" {
            let mut bindings = Vec::new();
            for spec in children(node) {
                bindings.extend(self.go_var(spec)?);
            }
            return Ok(bindings);
        }
        // New declaration forms can carry runtime behavior (for example
        // resource disposal); only ordinary lexical bindings belong here.
        if !matches!(
            self.text(node).split_whitespace().next(),
            Some("const" | "let")
        ) {
            return self.unsupported(node);
        }
        self.block(node)
    }

    /// A function defined inside another, bound to its name as a closure.
    fn nested_definition(&self, node: Node<'_>) -> Result<Stmt, String> {
        let (definition, decorators) = if node.kind() == "decorated_definition" {
            (field(node, "definition")?, self.decorators(node))
        } else {
            (node, Vec::new())
        };
        if !matches!(
            definition.kind(),
            "function_definition" | "function_declaration"
        ) {
            return self.unsupported(definition);
        }
        let name = self.identifier(field(definition, "name")?)?;
        let function = self.function(&Definition {
            node: definition,
            receiver: None,
            decorators,
        })?;
        Ok(Stmt::Bind(
            Pattern::Name(name),
            Expr::Closure(Box::new(function)),
        ))
    }

    /// Python's `with` and its context managers.
    pub(super) fn with_statement(&self, node: Node<'_>) -> Result<Stmt, String> {
        let is_async = is_async(node);
        let mut contexts = Vec::new();
        for clause in children(node) {
            if clause.kind() != "with_clause" {
                continue;
            }
            for item in children(clause) {
                contexts.push(self.context(item, is_async)?);
            }
        }
        Ok(Stmt::With(contexts, self.block(field(node, "body")?)?))
    }

    /// One `with` item: a context manager, and what `as` binds.
    pub(super) fn context(&self, item: Node<'_>, is_async: bool) -> Result<Context, String> {
        let value = field(item, "value")?;
        let (value, target) = if value.kind() == "as_pattern" {
            let alias = only(field(value, "alias")?)?;
            let manager = *children(value).first().ok_or("empty with item")?;
            (manager, Some(self.pattern(alias)?))
        } else {
            (value, None)
        };
        Ok(Context {
            text: self
                .text(value)
                .split_whitespace()
                .collect::<Vec<_>>()
                .join(" "),
            value: self.expression(value)?,
            target,
            is_async,
        })
    }

    /// The class a Python `raise` names, as written.
    pub(super) fn raised(&self, node: Node<'_>) -> Raised {
        let Some(value) = node.named_child(0).filter(|value| {
            node.child_by_field_name("cause")
                .is_none_or(|cause| cause.id() != value.id())
        }) else {
            return Raised::Again;
        };
        let class = if value.kind() == "call" {
            value.child_by_field_name("function")
        } else {
            Some(value)
        };
        match class {
            Some(class) if matches!(class.kind(), "identifier" | "attribute") => {
                Raised::Class(self.text(class).to_owned())
            }
            _ => Raised::Unknown,
        }
    }

    pub(super) fn try_statement(&self, node: Node<'_>) -> Result<Try, String> {
        let mut statement = Try {
            body: self.block(field(node, "body")?)?,
            handlers: Vec::new(),
            otherwise: Vec::new(),
            finally: Vec::new(),
        };
        for part in children(node) {
            match part.kind() {
                "block" | "statement_block" => {}
                "except_clause" => statement.handlers.push(self.python_handler(part)?),
                "catch_clause" => statement.handlers.push(Handler {
                    classes: None,
                    body: self.block(field(part, "body")?)?,
                }),
                "else_clause" => statement.otherwise = self.block(field(part, "body")?)?,
                "finally_clause" => {
                    let body = part
                        .child_by_field_name("body")
                        .or_else(|| children(part).last().copied())
                        .ok_or("empty finally clause")?;
                    statement.finally = self.block(body)?;
                }
                _ => return self.unsupported(part),
            }
        }
        Ok(statement)
    }

    /// A Python `except` clause and the classes it names.
    pub(super) fn python_handler(&self, clause: Node<'_>) -> Result<Handler, String> {
        let body = children(clause)
            .into_iter()
            .find(|part| part.kind() == "block")
            .ok_or("empty except clause")?;
        let Some(value) = clause.child_by_field_name("value") else {
            return Ok(Handler {
                classes: None,
                body: self.block(body)?,
            });
        };
        let value = if value.kind() == "as_pattern" {
            *children(value).first().ok_or("empty except pattern")?
        } else {
            value
        };
        let names = if value.kind() == "tuple" {
            children(value)
        } else {
            vec![value]
        };
        let mut classes = Vec::new();
        for name in names {
            if !matches!(name.kind(), "identifier" | "attribute") {
                return self.unsupported(name);
            }
            classes.push(self.text(name).to_owned());
        }
        Ok(Handler {
            classes: Some(classes),
            body: self.block(body)?,
        })
    }

    /// A Go `var` with a value; one without would bind a zero value.
    pub(super) fn go_var(&self, spec: Node<'_>) -> Result<Vec<Statement>, String> {
        if spec.kind() != "var_spec" {
            return self.unsupported(spec);
        }
        let Some(value) = spec.child_by_field_name("value") else {
            return self.unsupported(spec);
        };
        let mut cursor = spec.walk();
        let names: Vec<Pattern> = spec
            .children_by_field_name("name", &mut cursor)
            .map(|name| self.pattern(name))
            .collect::<Result<_, _>>()?;
        let pattern = match <[Pattern; 1]>::try_from(names) {
            Ok([name]) => name,
            Err(names) => Pattern::Sequence(names),
        };
        Ok(vec![Statement {
            line: spec.start_position().row + 1,
            kind: Stmt::Bind(pattern, self.expression(value)?),
        }])
    }

    /// Whether a statement is a call of Go's built-in `panic`.
    pub(super) fn is_go_panic(&self, node: Node<'_>) -> bool {
        self.language == Language::Go
            && only(node).is_ok_and(|call| {
                call.kind() == "call_expression"
                    && call
                        .child_by_field_name("function")
                        .is_some_and(|function| self.text(function) == "panic")
            })
    }

    /// An assignment, declaration with `:=`, or `op=` update.
    pub(super) fn assignment(&self, node: Node<'_>) -> Result<Stmt, String> {
        let left = field(node, "left")?;
        let right = field(node, "right")?;
        let operator = node
            .child_by_field_name("operator")
            .map_or("=", |operator| self.text(operator));
        let operator = match operator {
            "=" | ":=" => None,
            "+=" => Some(Op::Add),
            "-=" => Some(Op::Subtract),
            "*=" => Some(Op::Multiply),
            _ => return self.unsupported(node),
        };
        let value = self.expression(right)?;
        let left = if left.kind() == "expression_list" && children(left).len() == 1 {
            only(left)?
        } else {
            left
        };
        if let Some((name, key)) = self.field_target(left)? {
            let is_entry = self.language == Language::Python && left.kind() == "subscript";
            let current = if is_entry {
                Expr::Subscript(
                    Box::new(Expr::Name(name.clone())),
                    Box::new(Expr::Text(key.clone())),
                )
            } else {
                Expr::Member(Box::new(Expr::Name(name.clone())), key.clone())
            };
            let value = match operator {
                Some(operator) => Expr::Binary(operator, Box::new(current), Box::new(value)),
                None => value,
            };
            if is_entry {
                return Ok(Stmt::SetEntry(name, key, value));
            }
            return Ok(Stmt::SetField(name, key, value));
        }
        if let Some(operator) = operator {
            return Ok(Stmt::Update(self.identifier(left)?, operator, value));
        }
        let pattern = self.pattern(left)?;
        // Python binds wherever it assigns; TypeScript and Go declare with
        // `let`, `const`, `var` or `:=` and otherwise assign.
        let is_declaration =
            self.language == Language::Python || node.kind() == "short_var_declaration";
        if is_declaration {
            return Ok(Stmt::Bind(pattern, value));
        }
        Ok(Stmt::Assign(pattern, value))
    }

    /// `name.field` or `name["field"]` as an assignment target.
    pub(super) fn field_target(&self, node: Node<'_>) -> Result<Option<(String, String)>, String> {
        let is_member = matches!(
            node.kind(),
            "attribute" | "member_expression" | "selector_expression"
        );
        let is_item = matches!(
            node.kind(),
            "subscript" | "subscript_expression" | "index_expression"
        );
        if !is_member && !is_item {
            return Ok(None);
        }
        let (target, key) = match self.expression(node)? {
            Expr::Member(target, key) => (target, key),
            Expr::Subscript(target, key) => match *key {
                Expr::Text(key) => (target, key),
                _ => return self.unsupported(node),
            },
            _ => return self.unsupported(node),
        };
        let Expr::Name(name) = *target else {
            return self.unsupported(node);
        };
        if self.language == Language::TypeScript
            && matches!(key.as_str(), "__proto__" | "constructor" | "prototype")
        {
            return self.unsupported(node);
        }
        Ok(Some((name, key)))
    }

    /// What a binding's left side names.
    pub(super) fn pattern(&self, node: Node<'_>) -> Result<Pattern, String> {
        Ok(match node.kind() {
            "identifier" if self.language == Language::Go && self.text(node) == "_" => {
                Pattern::Ignore
            }
            "identifier" => Pattern::Name(self.text(node).to_owned()),
            "expression_list" if children(node).len() == 1 => self.pattern(only(node)?)?,
            "expression_list" | "pattern_list" | "tuple_pattern" | "list_pattern"
            | "array_pattern" => Pattern::Sequence(
                children(node)
                    .into_iter()
                    .map(|part| self.pattern(part))
                    .collect::<Result<_, _>>()?,
            ),
            "object_pattern" => self.object_pattern(node)?,
            "rest_pattern" | "list_splat_pattern" => {
                Pattern::Rest(Box::new(self.pattern(only(node)?)?))
            }
            _ => return self.unsupported(node),
        })
    }

    /// A TypeScript object pattern, with a `...rest` of the fields it leaves.
    fn object_pattern(&self, node: Node<'_>) -> Result<Pattern, String> {
        let mut fields = Vec::new();
        let mut rest = None;
        for part in children(node) {
            if part.kind() == "rest_pattern" {
                rest = Some(Box::new(self.pattern(only(part)?)?));
                continue;
            }
            fields.push(self.record_pattern(part)?);
        }
        Ok(match rest {
            Some(rest) => Pattern::RecordRest(fields, rest),
            None => Pattern::Record(fields),
        })
    }

    /// One field of a TypeScript object pattern.
    pub(super) fn record_pattern(&self, node: Node<'_>) -> Result<(String, Pattern), String> {
        match node.kind() {
            "shorthand_property_identifier_pattern" => {
                let name = self.text(node).to_owned();
                Ok((name.clone(), Pattern::Name(name)))
            }
            "pair_pattern" => {
                let key = field(node, "key")?;
                let key = if key.kind() == "string" {
                    self.string(key)?
                } else {
                    self.identifier(key)?
                };
                Ok((key, self.pattern(field(node, "value")?)?))
            }
            _ => self.unsupported(node),
        }
    }

    pub(super) fn return_value(&self, node: Node<'_>) -> Result<Expr, String> {
        match children(node).as_slice() {
            // A Go bare return yields the named results, which are not read.
            [] if self.language == Language::Go => self.unsupported(node),
            [] if self.language == Language::Python => Ok(Expr::Null),
            [] => Ok(Expr::Undefined),
            [value] => self.expression(*value),
            _ => self.unsupported(node),
        }
    }

    pub(super) fn if_statement(&self, node: Node<'_>) -> Result<Stmt, String> {
        if node.child_by_field_name("initializer").is_some() {
            return self.unsupported(node);
        }
        let condition = self.expression(field(node, "condition")?)?;
        let then = self.block(field(node, "consequence")?)?;
        let mut cursor = node.walk();
        let alternatives: Vec<_> = node
            .children_by_field_name("alternative", &mut cursor)
            .collect();
        let mut otherwise = Vec::new();
        for other in alternatives.into_iter().rev() {
            otherwise = self.alternative(other, otherwise)?;
        }
        Ok(Stmt::If(condition, then, otherwise))
    }

    pub(super) fn alternative(
        &self,
        node: Node<'_>,
        tail: Vec<Statement>,
    ) -> Result<Vec<Statement>, String> {
        match node.kind() {
            "elif_clause" => Ok(vec![Statement {
                line: node.start_position().row + 1,
                kind: Stmt::If(
                    self.expression(field(node, "condition")?)?,
                    self.block(field(node, "consequence")?)?,
                    tail,
                ),
            }]),
            "if_statement" => self.statement(node),
            "else_clause" => {
                let body = node
                    .child_by_field_name("body")
                    .or_else(|| children(node).first().copied())
                    .ok_or("empty else clause")?;
                if body.kind() == "if_statement" {
                    self.statement(body)
                } else {
                    self.block(body)
                }
            }
            _ => self.block(node),
        }
    }
}
