//! Strict source readers. Unhandled syntax is an error, never an empty statement.

use std::collections::BTreeMap;

use lemma_lang::syntax::ast::BinaryOperator as Op;
use tree_sitter::{Node, Parser};

use super::ir::{
    Argument, Expression as Expr, Function, Parameter, Part, Pattern, Statement,
    StatementKind as Stmt,
};

pub(super) use super::ir::Language;

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
        Err(self.refusal(node))
    }

    /// Why `node` is refused: its kind, line and text.
    fn refusal(&self, node: Node<'_>) -> String {
        format!(
            "unsupported {} at line {}: {}",
            node.kind(),
            node.start_position().row + 1,
            self.text(node)
        )
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
            language: self.language,
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
                self.pattern(field(node, "name")?)?,
                self.expression(field(node, "value")?)?,
            ),
            "short_var_declaration" | "assignment_statement" | "assignment" => {
                self.assignment(node)?
            }
            // Nothing in a raise can make the path return normally, so what it
            // raises is not read.
            "raise_statement" | "throw_statement" => Stmt::Raise,
            "assert_statement" => {
                let condition = children(node)
                    .into_iter()
                    .next()
                    .ok_or("empty assert statement")?;
                Stmt::Assert(self.expression(condition)?)
            }
            "expression_statement" if self.is_go_panic(node) => Stmt::Raise,
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

    /// Whether a statement is a call of Go's built-in `panic`.
    fn is_go_panic(&self, node: Node<'_>) -> bool {
        self.language == Language::Go
            && only(node).is_ok_and(|call| {
                call.kind() == "call_expression"
                    && call
                        .child_by_field_name("function")
                        .is_some_and(|function| self.text(function) == "panic")
            })
    }

    fn assignment(&self, node: Node<'_>) -> Result<Stmt, String> {
        if node.kind() == "assignment_statement"
            && field(node, "operator").is_ok_and(|op| self.text(op) != "=")
        {
            return self.unsupported(node);
        }
        let left = field(node, "left")?;
        let right = field(node, "right")?;
        Ok(Stmt::Bind(self.pattern(left)?, self.expression(right)?))
    }

    /// What a binding's left side names.
    fn pattern(&self, node: Node<'_>) -> Result<Pattern, String> {
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
            "object_pattern" => Pattern::Record(
                children(node)
                    .into_iter()
                    .map(|part| self.record_pattern(part))
                    .collect::<Result<_, _>>()?,
            ),
            _ => return self.unsupported(node),
        })
    }

    /// One field of a TypeScript object pattern.
    fn record_pattern(&self, node: Node<'_>) -> Result<(String, Pattern), String> {
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

    fn return_value(&self, node: Node<'_>) -> Result<Expr, String> {
        match children(node).as_slice() {
            // A Go bare return yields the named results, which are not read.
            [] if self.language == Language::Go => self.unsupported(node),
            [] if self.language == Language::Python => Ok(Expr::Null),
            [] => Ok(Expr::Undefined),
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
        let target = Box::new(self.expression(target)?);
        let name = self.identifier(property)?;
        if has_optional_chain(node) {
            return Ok(Expr::OptionalMember(target, name));
        }
        Ok(Expr::Member(target, name))
    }

    fn index(&self, node: Node<'_>) -> Result<Expr, String> {
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
    fn constant_integer(&self, node: Node<'_>) -> Option<i64> {
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

    fn unary(&self, node: Node<'_>) -> Result<Expr, String> {
        let operand = node
            .child_by_field_name("argument")
            .or_else(|| node.child_by_field_name("operand"))
            .or_else(|| children(node).last().copied())
            .ok_or("missing unary operand")?;
        let operator = self.source[node.start_byte()..operand.start_byte()].trim();
        match operator {
            "!" | "not" => Ok(Expr::Not(Box::new(self.expression(operand)?))),
            "-" => Ok(Expr::Negate(Box::new(self.expression(operand)?))),
            _ => self.unsupported(node),
        }
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

    /// Python's `in` and `not in`.
    fn membership(
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

    /// A string literal without interpolation, such as a record key.
    fn string(&self, node: Node<'_>) -> Result<String, String> {
        let mut text = String::new();
        for part in self.text_parts(node)? {
            let Part::Text(piece) = part else {
                return self.unsupported(node);
            };
            text.push_str(&piece);
        }
        Ok(text)
    }

    /// A string literal, or a formatted string where values are interpolated.
    fn text_expression(&self, node: Node<'_>) -> Result<Expr, String> {
        let parts = self.text_parts(node)?;
        if parts.iter().all(|part| matches!(part, Part::Text(_))) {
            return self.string(node).map(Expr::Text);
        }
        Ok(Expr::Template(parts))
    }

    /// The pieces of a string literal, decoded from the grammar's fragments,
    /// escapes and interpolations rather than from its source text.
    fn text_parts(&self, node: Node<'_>) -> Result<Vec<Part>, String> {
        if !matches!(
            node.kind(),
            "string"
                | "concatenated_string"
                | "template_string"
                | "interpreted_string_literal"
                | "raw_string_literal"
        ) {
            return self.unsupported(node);
        }
        if node.kind() == "concatenated_string" {
            let mut parts = Vec::new();
            for piece in children(node) {
                parts.extend(self.text_parts(piece)?);
            }
            return Ok(parts);
        }
        if self.language == Language::Python {
            return self.python_string(node);
        }
        let mut parts = Vec::new();
        for piece in children(node) {
            let part = match piece.kind() {
                "string_fragment" | "interpreted_string_literal_content" => {
                    Part::Text(self.text(piece).to_owned())
                }
                // Go discards carriage returns inside raw strings.
                "raw_string_literal_content" if !self.text(piece).contains('\r') => {
                    Part::Text(self.text(piece).to_owned())
                }
                "escape_sequence" => Part::Text(
                    decode_escapes(self.text(piece), self.language)
                        .ok_or_else(|| self.refusal(piece))?,
                ),
                "template_substitution" => Part::Value(self.expression(only(piece)?)?),
                _ => return self.unsupported(piece),
            };
            parts.push(part);
        }
        Ok(parts)
    }

    fn python_string(&self, node: Node<'_>) -> Result<Vec<Part>, String> {
        let start = node
            .named_child(0)
            .filter(|start| start.kind() == "string_start")
            .ok_or("missing string start")?;
        let prefix = self
            .text(start)
            .trim_end_matches(['\'', '"'])
            .to_ascii_lowercase();
        // Bytes are not text.
        if prefix.contains('b') {
            return self.unsupported(node);
        }
        let is_raw = prefix.contains('r');
        let is_formatted = prefix.contains('f');
        let mut parts = Vec::new();
        for piece in children(node) {
            match piece.kind() {
                "string_start" | "string_end" => {}
                "string_content" => {
                    parts.push(Part::Text(self.python_content(
                        piece,
                        is_raw,
                        is_formatted,
                    )?));
                }
                // Conversions, format specifications and `{x=}` spell values
                // in ways each needs its own reading.
                "interpolation"
                    if piece.child_by_field_name("type_conversion").is_none()
                        && piece.child_by_field_name("format_specifier").is_none()
                        && !self
                            .text(piece)
                            .trim_end_matches('}')
                            .trim_end()
                            .ends_with('=') =>
                {
                    parts.push(Part::Value(self.expression(field(piece, "expression")?)?));
                }
                _ => return self.unsupported(piece),
            }
        }
        Ok(parts)
    }
}

impl Reader<'_> {
    /// A piece of Python string literal between interpolations.
    fn python_content(
        &self,
        piece: Node<'_>,
        is_raw: bool,
        is_formatted: bool,
    ) -> Result<String, String> {
        let text = if is_raw {
            self.text(piece).to_owned()
        } else {
            decode_escapes(self.text(piece), Language::Python).ok_or_else(|| self.refusal(piece))?
        };
        if !is_formatted {
            return Ok(text);
        }
        Ok(text.replace("{{", "{").replace("}}", "}"))
    }
}

/// Decodes the escapes in a piece of string literal, or `None` for one this
/// reader does not decode, rather than guessing what the language does.
fn decode_escapes(text: &str, language: Language) -> Option<String> {
    let mut decoded = String::new();
    let mut characters = text.chars().peekable();
    while let Some(character) = characters.next() {
        if character != '\\' {
            decoded.push(character);
            continue;
        }
        let escape = characters.next()?;
        let simple = match escape {
            'n' => Some('\n'),
            't' => Some('\t'),
            'r' => Some('\r'),
            '\\' | '\'' | '"' => Some(escape),
            'b' => Some('\u{8}'),
            'f' => Some('\u{c}'),
            'v' => Some('\u{b}'),
            'a' if language != Language::TypeScript => Some('\u{7}'),
            '0' if language == Language::TypeScript
                && !characters.peek().is_some_and(char::is_ascii_digit) =>
            {
                Some('\0')
            }
            _ => None,
        };
        if let Some(simple) = simple {
            decoded.push(simple);
            continue;
        }
        let digits = match (escape, language) {
            ('x', _) => 2,
            ('u', Language::TypeScript) if characters.peek() == Some(&'{') => {
                characters.next();
                let hex: String = characters.by_ref().take_while(|c| *c != '}').collect();
                decoded.push(char::from_u32(u32::from_str_radix(&hex, 16).ok()?)?);
                continue;
            }
            ('u', _) => 4,
            ('U', Language::Python | Language::Go) => 8,
            // A backslash before a line break continues the line.
            ('\n', Language::TypeScript | Language::Python) => continue,
            _ => return None,
        };
        let hex: String = characters.by_ref().take(digits).collect();
        if hex.len() != digits {
            return None;
        }
        decoded.push(char::from_u32(u32::from_str_radix(&hex, 16).ok()?)?);
    }
    Some(decoded)
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

/// Whether a member read, subscript or call is part of an optional chain,
/// written `?.`.
fn has_optional_chain(node: Node<'_>) -> bool {
    let mut cursor = node.walk();
    node.children(&mut cursor)
        .any(|child| matches!(child.kind(), "optional_chain" | "?."))
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
