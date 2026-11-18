//! Strict source readers. Unhandled syntax is an error, never an empty statement.

use std::collections::BTreeMap;

use lemma_lang::syntax::ast::BinaryOperator as Op;
use tree_sitter::{Node, Parser};

use super::ir::{
    Argument, Expression as Expr, Function, Parameter, Statement, StatementKind as Stmt,
};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum Language {
    TypeScript,
    Python,
    Go,
}

impl Language {
    pub(super) fn name(self) -> &'static str {
        match self {
            Self::TypeScript => "typescript",
            Self::Python => "python",
            Self::Go => "go",
        }
    }
}

pub(super) fn read(file: &str, source: &str, symbol: &str) -> Result<(Language, Function), String> {
    let language = match std::path::Path::new(file)
        .extension()
        .and_then(|s| s.to_str())
    {
        Some("ts") => Language::TypeScript,
        Some("py") => Language::Python,
        Some("go") => Language::Go,
        _ => return Err("proof supports .ts, .py and .go implementations".to_owned()),
    };
    let grammar = match language {
        Language::TypeScript => tree_sitter_typescript::LANGUAGE_TYPESCRIPT.into(),
        Language::Python => tree_sitter_python::LANGUAGE.into(),
        Language::Go => tree_sitter_go::LANGUAGE.into(),
    };
    let mut parser = Parser::new();
    parser.set_language(&grammar).map_err(|e| e.to_string())?;
    let tree = parser
        .parse(source, None)
        .ok_or("could not parse implementation")?;
    if tree.root_node().has_error() {
        return Err(
            "implementation contains a syntax error; proof does not repair source".to_owned(),
        );
    }
    let reader = Reader { source, language };
    let definition = reader.lookup(tree.root_node(), symbol)?;
    Ok((language, reader.function(&definition)?))
}

/// A function body found for a symbol, and the name its receiver has inside
/// it: `this` for a TypeScript method, the receiver of a Go method. Python
/// methods name their receiver as an ordinary first parameter.
struct Definition<'t> {
    node: Node<'t>,
    receiver: Option<String>,
    /// Decorators of the function and of the classes around it, as written.
    decorators: Vec<String>,
}

struct Reader<'a> {
    source: &'a str,
    language: Language,
}

fn children(node: Node<'_>) -> Vec<Node<'_>> {
    let mut cursor = node.walk();
    node.named_children(&mut cursor)
        .filter(|n| !matches!(n.kind(), "comment" | "type_annotation"))
        .collect()
}

/// Declarations at the top of a file, looking through `export` and variable
/// declarations to what they declare.
fn top_level(node: Node<'_>) -> Vec<Node<'_>> {
    let mut found = Vec::new();
    for child in children(node) {
        if matches!(
            child.kind(),
            "export_statement" | "lexical_declaration" | "variable_declaration"
        ) {
            found.extend(top_level(child));
        } else {
            found.push(child);
        }
    }
    found
}

fn field<'a>(node: Node<'a>, name: &str) -> Result<Node<'a>, String> {
    node.child_by_field_name(name).ok_or_else(|| {
        format!(
            "missing {name} in {} at line {}",
            node.kind(),
            node.start_position().row + 1
        )
    })
}

impl<'a> Reader<'a> {
    fn text(&self, node: Node<'_>) -> &'a str {
        &self.source[node.byte_range()]
    }

    fn unsupported<T>(&self, node: Node<'_>) -> Result<T, String> {
        Err(format!(
            "unsupported {} at line {}: {}",
            node.kind(),
            node.start_position().row + 1,
            self.text(node)
        ))
    }

    /// Finds the one definition `symbol` names: a top-level function, or a
    /// method written `Container.method`, where Python classes may nest.
    fn lookup<'t>(&self, root: Node<'t>, symbol: &str) -> Result<Definition<'t>, String> {
        let segments: Vec<&str> = symbol.split('.').collect();
        let found = match segments.as_slice() {
            [name] => top_level(root)
                .into_iter()
                .map(|node| self.function_named(node, name))
                .collect::<Result<Vec<_>, _>>()?
                .into_iter()
                .flatten()
                .collect(),
            [container, name] if self.language == Language::Go => {
                self.go_methods(root, container, name)?
            }
            [containers @ .., name] => self.class_methods(root, containers, name)?,
            [] => Vec::new(),
        };
        let mut found: Vec<Definition<'t>> = found
            .into_iter()
            .filter(|definition| !definition.decorators.iter().any(|d| is_overload(d)))
            .collect();
        if found.len() != 1 {
            return Err(format!(
                "expected one function or method `{symbol}`, found {}",
                found.len()
            ));
        }
        Ok(found.remove(0))
    }

    fn function_named<'t>(
        &self,
        node: Node<'t>,
        name: &str,
    ) -> Result<Option<Definition<'t>>, String> {
        let plain = |node| Definition {
            node,
            receiver: None,
            decorators: Vec::new(),
        };
        match node.kind() {
            "function_declaration" | "function_definition"
                if node
                    .child_by_field_name("name")
                    .is_some_and(|n| self.text(n) == name) =>
            {
                Ok(Some(Definition {
                    decorators: self.decorators(node),
                    ..plain(node)
                }))
            }
            "variable_declarator" if self.text(field(node, "name")?) == name => {
                let value = field(node, "value")?;
                if !matches!(value.kind(), "arrow_function" | "function_expression") {
                    return self.unsupported(value);
                }
                Ok(Some(plain(value)))
            }
            "decorated_definition" if self.decorated_name(node) == Some(name) => {
                let definition = field(node, "definition")?;
                if definition.kind() != "function_definition" {
                    return Ok(None);
                }
                Ok(Some(Definition {
                    decorators: self.decorators(node),
                    ..plain(definition)
                }))
            }
            _ => Ok(None),
        }
    }

    /// The decorators written on a declaration, without their `@`. Python
    /// puts them on a wrapping node; TypeScript on the declaration itself.
    fn decorators(&self, node: Node<'_>) -> Vec<String> {
        let mut cursor = node.walk();
        node.children(&mut cursor)
            .filter(|child| child.kind() == "decorator")
            .map(|decorator| self.decorator_text(decorator))
            .collect()
    }

    fn decorator_text(&self, decorator: Node<'_>) -> String {
        let text = self.text(decorator).trim_start_matches('@');
        text.split_whitespace().collect::<Vec<_>>().join(" ")
    }

    /// The name of the function or class a Python decorator list wraps.
    fn decorated_name(&self, node: Node<'_>) -> Option<&'a str> {
        let definition = node.child_by_field_name("definition")?;
        Some(self.text(definition.child_by_field_name("name")?))
    }

    /// Go methods `name` whose receiver's type is `container`.
    fn go_methods<'t>(
        &self,
        root: Node<'t>,
        container: &str,
        name: &str,
    ) -> Result<Vec<Definition<'t>>, String> {
        let mut found = Vec::new();
        for node in children(root) {
            if node.kind() != "method_declaration"
                || node
                    .child_by_field_name("name")
                    .is_none_or(|n| self.text(n) != name)
            {
                continue;
            }
            let receiver = only(field(node, "receiver")?)?;
            let receiver_type = self.text(field(receiver, "type")?);
            let receiver_type = receiver_type.trim_start_matches('*');
            let receiver_type = receiver_type.split('[').next().unwrap_or(receiver_type);
            if receiver_type != container {
                continue;
            }
            // An unnamed receiver cannot be read; it still takes a place.
            let receiver_name = receiver
                .child_by_field_name("name")
                .map_or("_", |n| self.text(n));
            found.push(Definition {
                node,
                receiver: Some(receiver_name.to_owned()),
                decorators: Vec::new(),
            });
        }
        Ok(found)
    }

    /// Methods `name` of the class reached through `containers`.
    fn class_methods<'t>(
        &self,
        root: Node<'t>,
        containers: &[&str],
        name: &str,
    ) -> Result<Vec<Definition<'t>>, String> {
        let mut scope = self.with_decorators(top_level(root));
        let mut class_decorators = Vec::new();
        for container in containers {
            let classes = self.classes_named(scope, container)?;
            let [(class, decorators)] = classes.as_slice() else {
                return Err(format!(
                    "expected one class `{container}`, found {}",
                    classes.len()
                ));
            };
            class_decorators.extend(decorators.iter().cloned());
            scope = self.with_decorators(children(field(*class, "body")?));
        }
        let mut found = Vec::new();
        for (node, written) in scope {
            if let Some(mut method) = self.method_named(node, name)? {
                method.decorators.extend(written);
                method.decorators.extend(class_decorators.iter().cloned());
                found.push(method);
            }
        }
        Ok(found)
    }

    /// The classes named `name` among `scope`, each with its decorators.
    fn classes_named<'t>(
        &self,
        scope: Vec<(Node<'t>, Vec<String>)>,
        name: &str,
    ) -> Result<Vec<(Node<'t>, Vec<String>)>, String> {
        let mut classes = Vec::new();
        for (node, written) in scope {
            let Some((class, mut decorators)) = self.class_named(node, name)? else {
                continue;
            };
            decorators.extend(written);
            classes.push((class, decorators));
        }
        Ok(classes)
    }

    /// Pairs each declaration with the TypeScript decorators written before
    /// it as siblings, as a class body and an `export` hold them.
    fn with_decorators<'t>(&self, nodes: Vec<Node<'t>>) -> Vec<(Node<'t>, Vec<String>)> {
        let mut paired = Vec::new();
        let mut pending = Vec::new();
        for node in nodes {
            if node.kind() == "decorator" {
                pending.push(self.decorator_text(node));
                continue;
            }
            paired.push((node, std::mem::take(&mut pending)));
        }
        paired
    }

    /// A class named `name`, with its decorators.
    fn class_named<'t>(
        &self,
        node: Node<'t>,
        name: &str,
    ) -> Result<Option<(Node<'t>, Vec<String>)>, String> {
        let is_class = |node: Node<'_>| {
            matches!(
                node.kind(),
                "class_declaration" | "abstract_class_declaration" | "class_definition"
            )
        };
        let is_named = |node: Node<'_>| {
            node.child_by_field_name("name")
                .is_some_and(|n| self.text(n) == name)
        };
        if is_class(node) && is_named(node) {
            return Ok(Some((node, self.decorators(node))));
        }
        if node.kind() == "decorated_definition" {
            let definition = field(node, "definition")?;
            if is_class(definition) && is_named(definition) {
                return Ok(Some((definition, self.decorators(node))));
            }
        }
        let wrapped = if node.kind() == "decorated_definition" {
            field(node, "definition")?
        } else {
            node
        };
        if matches!(
            wrapped.kind(),
            "function_declaration" | "function_definition"
        ) && is_named(wrapped)
        {
            return Err(
                "a nested function reads its enclosing function's variables, which proof does not model"
                    .to_owned(),
            );
        }
        Ok(None)
    }

    fn method_named<'t>(
        &self,
        node: Node<'t>,
        name: &str,
    ) -> Result<Option<Definition<'t>>, String> {
        let receiver = match self.language {
            Language::TypeScript => Some("this".to_owned()),
            Language::Python | Language::Go => None,
        };
        let named = |n: Node<'_>| self.text(n) == name;
        match node.kind() {
            "method_definition" | "function_definition"
                if node.child_by_field_name("name").is_some_and(named) =>
            {
                Ok(Some(Definition {
                    node,
                    receiver,
                    decorators: self.decorators(node),
                }))
            }
            // A class field holding an arrow function binds `this` to the
            // instance, as a method does.
            "public_field_definition" if node.child_by_field_name("name").is_some_and(named) => {
                let value = field(node, "value")?;
                if value.kind() != "arrow_function" {
                    return self.unsupported(value);
                }
                Ok(Some(Definition {
                    node: value,
                    receiver,
                    decorators: self.decorators(node),
                }))
            }
            "decorated_definition" if self.decorated_name(node) == Some(name) => {
                let definition = field(node, "definition")?;
                if definition.kind() != "function_definition" {
                    return Ok(None);
                }
                Ok(Some(Definition {
                    node: definition,
                    receiver,
                    decorators: self.decorators(node),
                }))
            }
            _ => Ok(None),
        }
    }

    fn function(&self, definition: &Definition<'_>) -> Result<Function, String> {
        let node = definition.node;
        let parameter_node = node
            .child_by_field_name("parameters")
            .or_else(|| node.child_by_field_name("parameter"))
            .ok_or("function has no parameter list")?;
        // A method's receiver is its first parameter in every language.
        let mut parameters: Vec<Parameter> = definition
            .receiver
            .iter()
            .map(|name| required(name.clone()))
            .collect();
        if parameter_node.kind() == "identifier" {
            parameters.push(required(self.identifier(parameter_node)?));
        } else {
            for parameter in children(parameter_node) {
                parameters.extend(self.parameter(parameter)?);
            }
        }
        let body = field(node, "body")?;
        let body = if matches!(body.kind(), "statement_block" | "block") {
            self.function_body(body)?
        } else {
            vec![Statement {
                line: body.start_position().row + 1,
                kind: Stmt::Return(self.expression(body)?),
            }]
        };
        Ok(Function {
            parameters,
            is_async: is_async(node),
            decorators: definition.decorators.clone(),
            body,
        })
    }

    fn function_body(&self, node: Node<'_>) -> Result<Vec<Statement>, String> {
        let nodes = children(node);
        // A plain Python function docstring is constant metadata, not an
        // evaluated f-string. Only the first statement can be a docstring.
        let has_docstring = self.language == Language::Python
            && nodes.first().is_some_and(|first| {
                first.kind() == "expression_statement"
                    && only(*first).is_ok_and(|value| {
                        value.kind() == "string" && self.text(value).starts_with(['\'', '"'])
                    })
            });
        self.block_nodes(&nodes[usize::from(has_docstring)..])
    }

    fn parameter(&self, node: Node<'_>) -> Result<Vec<Parameter>, String> {
        let default = |name: Node<'_>, value: Option<Node<'_>>| -> Result<Parameter, String> {
            Ok(Parameter {
                name: self.identifier(name)?,
                default: value.map(|value| self.expression(value)),
            })
        };
        Ok(match node.kind() {
            "identifier" => vec![required(self.identifier(node)?)],
            "required_parameter" => vec![default(
                field(node, "pattern")?,
                node.child_by_field_name("value"),
            )?],
            // `x?: T` is `undefined` when left out, unless it has a default.
            "optional_parameter" => {
                let mut parameter =
                    default(field(node, "pattern")?, node.child_by_field_name("value"))?;
                parameter.default.get_or_insert(Ok(Expr::Undefined));
                vec![parameter]
            }
            "typed_parameter" => vec![required(
                self.identifier(*children(node).first().ok_or("empty Python parameter")?)?,
            )],
            "default_parameter" | "typed_default_parameter" => {
                vec![default(field(node, "name")?, Some(field(node, "value")?))?]
            }
            // `*` and `/` separate kinds of parameters; they are not one.
            "keyword_separator" | "positional_separator" => Vec::new(),
            "parameter_declaration" => {
                let mut cursor = node.walk();
                node.children_by_field_name("name", &mut cursor)
                    .map(|name| Ok(required(self.identifier(name)?)))
                    .collect::<Result<_, String>>()?
            }
            _ => return self.unsupported(node),
        })
    }

    fn argument(&self, node: Node<'_>) -> Result<Argument, String> {
        if node.kind() == "keyword_argument" {
            return Ok(Argument {
                name: Some(self.identifier(field(node, "name")?)?),
                value: self.expression(field(node, "value")?)?,
            });
        }
        Ok(Argument {
            name: None,
            value: self.expression(node)?,
        })
    }

    fn identifier(&self, node: Node<'_>) -> Result<String, String> {
        if !matches!(
            node.kind(),
            "identifier" | "field_identifier" | "property_identifier"
        ) {
            return self.unsupported(node);
        }
        Ok(self.text(node).to_owned())
    }

    fn block(&self, node: Node<'_>) -> Result<Vec<Statement>, String> {
        self.block_nodes(&children(node))
    }

    fn block_nodes(&self, nodes: &[Node<'_>]) -> Result<Vec<Statement>, String> {
        let mut statements = Vec::new();
        for &child in nodes {
            if child.kind() == "statement_list" {
                statements.extend(self.block(child)?);
            } else {
                statements.extend(self.statement(child)?);
            }
        }
        Ok(statements)
    }

    fn statement(&self, node: Node<'_>) -> Result<Vec<Statement>, String> {
        let kind = match node.kind() {
            "lexical_declaration" | "variable_declaration" => {
                // New declaration forms can carry runtime behavior (for
                // example resource disposal); only ordinary lexical bindings
                // belong to this subset.
                if !matches!(
                    self.text(node).split_whitespace().next(),
                    Some("const" | "let")
                ) {
                    return self.unsupported(node);
                }
                return self.block(node);
            }
            "variable_declarator" => Stmt::Bind(
                self.identifier(field(node, "name")?)?,
                self.expression(field(node, "value")?)?,
            ),
            "short_var_declaration" | "assignment_statement" | "assignment" => {
                self.assignment(node)?
            }
            "expression_statement" => {
                let expression = only(node)?;
                if expression.kind() == "assignment" {
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

    fn assignment(&self, node: Node<'_>) -> Result<Stmt, String> {
        if node.kind() == "assignment_statement"
            && field(node, "operator").is_ok_and(|op| self.text(op) != "=")
        {
            return self.unsupported(node);
        }
        let mut left = field(node, "left")?;
        let mut right = field(node, "right")?;
        if left.kind() == "expression_list" {
            left = only(left)?;
        }
        if right.kind() == "expression_list" {
            right = only(right)?;
        }
        Ok(Stmt::Bind(self.identifier(left)?, self.expression(right)?))
    }

    fn return_value(&self, node: Node<'_>) -> Result<Expr, String> {
        match children(node).as_slice() {
            [] => Ok(Expr::Undefined),
            [value] if value.kind() == "expression_list" => self.expression(only(*value)?),
            [value] => self.expression(*value),
            _ => self.unsupported(node),
        }
    }

    fn if_statement(&self, node: Node<'_>) -> Result<Stmt, String> {
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

    fn alternative(&self, node: Node<'_>, tail: Vec<Statement>) -> Result<Vec<Statement>, String> {
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

    fn expression(&self, node: Node<'_>) -> Result<Expr, String> {
        Ok(match node.kind() {
            "true" => Expr::Bool(true),
            "false" => Expr::Bool(false),
            "identifier" | "this" => Expr::Name(self.text(node).to_owned()),
            "number" | "integer" | "int_literal" => self.integer(node)?,
            "string" | "interpreted_string_literal" => Expr::Text(self.string(node)?),
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
            "subscript" | "subscript_expression" => self.index(node)?,
            "unary_expression" | "not_operator" => self.unary(node)?,
            "binary_expression" | "boolean_operator" | "comparison_operator" => {
                self.binary(node)?
            }
            "ternary_expression" => Expr::Conditional(
                Box::new(self.expression(field(node, "condition")?)?),
                Box::new(self.expression(field(node, "consequence")?)?),
                Box::new(self.expression(field(node, "alternative")?)?),
            ),
            "object" | "dictionary" | "composite_literal" => self.record(node)?,
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

    fn integer(&self, node: Node<'_>) -> Result<Expr, String> {
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

    fn member(&self, node: Node<'_>) -> Result<Expr, String> {
        let target = node
            .child_by_field_name("object")
            .or_else(|| node.child_by_field_name("operand"))
            .ok_or("missing member target")?;
        let property = node
            .child_by_field_name("property")
            .or_else(|| node.child_by_field_name("field"))
            .or_else(|| node.child_by_field_name("attribute"))
            .ok_or("missing member name")?;
        Ok(Expr::Member(
            Box::new(self.expression(target)?),
            self.identifier(property)?,
        ))
    }

    fn index(&self, node: Node<'_>) -> Result<Expr, String> {
        let target = node
            .child_by_field_name("value")
            .or_else(|| node.child_by_field_name("object"))
            .ok_or("missing index target")?;
        let key = node
            .child_by_field_name("subscript")
            .or_else(|| node.child_by_field_name("index"))
            .ok_or("missing index")?;
        Ok(Expr::Member(
            Box::new(self.expression(target)?),
            self.string(key)?,
        ))
    }

    fn unary(&self, node: Node<'_>) -> Result<Expr, String> {
        let operand = node
            .child_by_field_name("argument")
            .or_else(|| node.child_by_field_name("operand"))
            .or_else(|| children(node).last().copied())
            .ok_or("missing unary operand")?;
        let operator = self.source[node.start_byte()..operand.start_byte()].trim();
        if !matches!(operator, "!" | "not") {
            return self.unsupported(node);
        }
        Ok(Expr::Not(Box::new(self.expression(operand)?)))
    }

    fn binary(&self, node: Node<'_>) -> Result<Expr, String> {
        let operands = children(node);
        let left = node
            .child_by_field_name("left")
            .or_else(|| operands.first().copied())
            .ok_or("missing left operand")?;
        let right = node
            .child_by_field_name("right")
            .or_else(|| operands.last().copied())
            .ok_or("missing right operand")?;
        if operands.len() != 2 {
            return self.unsupported(node);
        }
        let operator = match self.source[left.end_byte()..right.start_byte()].trim() {
            "&&" | "and" => Op::And,
            "||" | "or" => Op::Or,
            "===" => Op::Equal,
            "!==" => Op::NotEqual,
            "==" if self.language != Language::TypeScript => Op::Equal,
            "!=" if self.language != Language::TypeScript => Op::NotEqual,
            _ => return self.unsupported(node),
        };
        Ok(Expr::Binary(
            operator,
            Box::new(self.expression(left)?),
            Box::new(self.expression(right)?),
        ))
    }

    fn record(&self, node: Node<'_>) -> Result<Expr, String> {
        let body = if node.kind() == "composite_literal" {
            if field(node, "type")?.kind() != "type_identifier" {
                return self.unsupported(node);
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

    fn record_entry(&self, entry: Node<'_>) -> Result<(String, Expr), String> {
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

    fn string(&self, node: Node<'_>) -> Result<String, String> {
        let text = self.text(node);
        // Start with plain literals: interpolation, escapes, prefixes and multiline
        // literals need language-specific decoding before they can be trusted.
        if text.len() < 2
            || !matches!(text.as_bytes()[0], b'\'' | b'"')
            || text.as_bytes().last() != text.as_bytes().first()
        {
            return self.unsupported(node);
        }
        let value = &text[1..text.len() - 1];
        if value.contains(['\\', '\n', '\r', '\'', '"']) {
            return self.unsupported(node);
        }
        Ok(value.to_owned())
    }
}

fn required(name: String) -> Parameter {
    Parameter {
        name,
        default: None,
    }
}

/// Whether a decorator marks an `@overload` signature, which the function
/// defined after it replaces at runtime.
fn is_overload(decorator: &str) -> bool {
    matches!(
        decorator,
        "overload" | "typing.overload" | "typing_extensions.overload"
    )
}

/// Whether a function is declared `async`: the keyword is an unnamed child
/// of the declaration in every supported grammar.
fn is_async(node: Node<'_>) -> bool {
    let mut cursor = node.walk();
    node.children(&mut cursor)
        .any(|child| child.kind() == "async")
}

fn only(node: Node<'_>) -> Result<Node<'_>, String> {
    let nodes = children(node);
    match nodes.as_slice() {
        [child] => Ok(*child),
        _ => Err(format!(
            "expected one value in {} at line {}",
            node.kind(),
            node.start_position().row + 1
        )),
    }
}
