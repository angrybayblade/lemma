//! Strict source readers. Unhandled syntax is an error, never an empty statement.

use tree_sitter::{Node, Parser};

use super::ir::{
    Argument, Expression as Expr, Function, Parameter, Pattern, Statement, StatementKind as Stmt,
};

mod expressions;
mod lookup;
mod statements;
mod strings;

pub(super) use super::ir::Language;

pub(super) fn read(file: &str, source: &str, symbol: &str) -> Result<(Language, Function), String> {
    let extension = std::path::Path::new(file)
        .extension()
        .and_then(|s| s.to_str());
    let (language, grammar): (Language, tree_sitter::Language) = match extension {
        Some("ts" | "mts" | "cts") => (
            Language::TypeScript,
            tree_sitter_typescript::LANGUAGE_TYPESCRIPT.into(),
        ),
        // TypeScript with JSX; JSX itself is refused like any other syntax
        // the reader does not know.
        Some("tsx") => (
            Language::TypeScript,
            tree_sitter_typescript::LANGUAGE_TSX.into(),
        ),
        Some("py") => (Language::Python, tree_sitter_python::LANGUAGE.into()),
        Some("go") => (Language::Go, tree_sitter_go::LANGUAGE.into()),
        _ => return Err("proof supports .ts, .tsx, .py and .go implementations".to_owned()),
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
        check_field_assignments(&body)?;
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
}

/// Refuses assigning a field of a variable the function never binds to a
/// record literal, such as a parameter: no contract could show such a write
/// invisible to the caller.
fn check_field_assignments(body: &[Statement]) -> Result<(), String> {
    let mut built = std::collections::BTreeSet::new();
    literal_bindings(body, &mut built);
    let mut targets = Vec::new();
    field_assignments(body, &mut targets);
    for (line, name) in targets {
        if !built.contains(name) {
            return Err(format!(
                "assigning a field of `{name}` at line {line}, an object the function did not build, is unsupported"
            ));
        }
    }
    Ok(())
}

fn literal_bindings<'b>(body: &'b [Statement], built: &mut std::collections::BTreeSet<&'b str>) {
    for statement in body {
        match &statement.kind {
            Stmt::Bind(Pattern::Name(name), Expr::Record(_) | Expr::Sequence(_))
            | Stmt::Assign(Pattern::Name(name), Expr::Record(_) | Expr::Sequence(_)) => {
                built.insert(name);
            }
            kind => {
                for block in kind.blocks() {
                    literal_bindings(block, built);
                }
            }
        }
    }
}

fn field_assignments<'b>(body: &'b [Statement], targets: &mut Vec<(usize, &'b str)>) {
    for statement in body {
        match &statement.kind {
            Stmt::SetField(name, _, _) => targets.push((statement.line, name)),
            kind => {
                for block in kind.blocks() {
                    field_assignments(block, targets);
                }
            }
        }
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
