//! Finding the definition a symbol names: a top-level function, a method of
//! a class, or a Go method by its receiver.

use tree_sitter::Node;

use super::{Definition, Language, Reader, children, field, is_overload, only, top_level};

/// What kind of scope a container opens, which decides how a definition in
/// it is found and what its receiver is.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Kind {
    File,
    Class,
    Function,
    Object,
}

/// The declarations of a scope, each with the decorators written before it,
/// and the decorators that reach every definition in it.
struct Scope<'t> {
    kind: Kind,
    nodes: Vec<(Node<'t>, Vec<String>)>,
    decorators: Vec<String>,
}

impl<'a> Reader<'a> {
    /// The one definition `symbol` names: found by its containers, or, where
    /// they pass through anonymous functions and objects, by the name Lemma's
    /// symbol index gives it, so that both name a definition alike.
    pub(super) fn find<'t>(
        &self,
        root: Node<'t>,
        symbol: &str,
        indexed: lemma_model::Language,
    ) -> Result<Definition<'t>, String> {
        let found = self.lookup(root, symbol);
        if matches!(&found, Ok(definitions) if definitions.len() == 1) {
            return found.map(|mut definitions| definitions.remove(0));
        }
        let mut extracted = self.indexed(root, symbol, indexed)?;
        if extracted.len() == 1 {
            return Ok(extracted.remove(0));
        }
        let count = found?.len();
        Err(format!(
            "expected one function or method `{symbol}`, found {count}"
        ))
    }

    /// Definitions Lemma's symbol index names `symbol`, found by the place of
    /// their names.
    fn indexed<'t>(
        &self,
        root: Node<'t>,
        symbol: &str,
        language: lemma_model::Language,
    ) -> Result<Vec<Definition<'t>>, String> {
        let mut extractor = lemma_syntax::Extractor::new().map_err(|e| e.to_string())?;
        let extraction = extractor
            .extract(language, self.source)
            .map_err(|e| e.to_string())?;
        let mut found = Vec::new();
        for indexed in &extraction.symbols {
            let is_function = matches!(
                indexed.kind,
                lemma_model::SymbolKind::Function | lemma_model::SymbolKind::Method
            );
            if !is_function || indexed.qualified_name != symbol {
                continue;
            }
            let point = |position: lemma_model::Position| tree_sitter::Point {
                row: position.line as usize,
                column: position.column as usize,
            };
            let name = root.descendant_for_point_range(
                point(indexed.name_span.start),
                point(indexed.name_span.end),
            );
            let definition = name.and_then(|name| self.definition_at(name));
            found.extend(
                definition
                    .filter(|definition| !definition.decorators.iter().any(|d| is_overload(d))),
            );
        }
        Ok(found)
    }

    /// The definition whose name is `name`, its receiver as its place gives
    /// it, and the decorators on it and on the classes around it.
    fn definition_at<'t>(&self, name: Node<'t>) -> Option<Definition<'t>> {
        let owner = name.parent()?;
        let (node, receiver) = match owner.kind() {
            "function_declaration" | "function_definition" => (owner, None),
            "method_definition" => (owner, Some("this".to_owned())),
            "method_declaration" => {
                let receiver = only(owner.child_by_field_name("receiver")?).ok()?;
                let receiver = receiver
                    .child_by_field_name("name")
                    .map_or("_", |n| self.text(n));
                (owner, Some(receiver.to_owned()))
            }
            "variable_declarator" | "pair" | "public_field_definition" => {
                let value = owner.child_by_field_name("value")?;
                // `this` in a function value is the object it is called on,
                // and an arrow function's `this` is the class instance only
                // as a class field.
                let receiver = match (value.kind(), owner.kind()) {
                    ("function_expression", "pair")
                    | ("arrow_function", "public_field_definition") => Some("this".to_owned()),
                    ("arrow_function" | "function_expression", _) => None,
                    _ => return None,
                };
                (value, receiver)
            }
            _ => return None,
        };
        let mut decorators = self.attached_decorators(owner);
        let mut ancestor = owner.parent();
        while let Some(current) = ancestor {
            if matches!(
                current.kind(),
                "class_declaration" | "abstract_class_declaration" | "class_definition"
            ) {
                decorators.extend(self.attached_decorators(current));
            }
            ancestor = current.parent();
        }
        Some(Definition {
            node,
            receiver,
            decorators,
        })
    }

    /// The decorators of a declaration: on it, on a Python wrapper or a
    /// TypeScript `export`, and written before it as siblings.
    fn attached_decorators(&self, node: Node<'_>) -> Vec<String> {
        let mut decorators = self.decorators(node);
        if let Some(parent) = node.parent()
            && matches!(parent.kind(), "decorated_definition" | "export_statement")
        {
            decorators.extend(self.decorators(parent));
        }
        let mut sibling = node.prev_named_sibling();
        while let Some(previous) = sibling.filter(|previous| previous.kind() == "decorator") {
            decorators.push(self.decorator_text(previous));
            sibling = previous.prev_named_sibling();
        }
        decorators
    }

    /// The definitions `symbol` names through its containers.
    fn lookup<'t>(&self, root: Node<'t>, symbol: &str) -> Result<Vec<Definition<'t>>, String> {
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
            [containers @ .., name] => self.scoped(root, containers, name)?,
            [] => Vec::new(),
        };
        let found: Vec<Definition<'t>> = found;
        Ok(found
            .into_iter()
            .filter(|definition| !definition.decorators.iter().any(|d| is_overload(d)))
            .collect())
    }

    pub(super) fn function_named<'t>(
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
    pub(super) fn decorators(&self, node: Node<'_>) -> Vec<String> {
        let mut cursor = node.walk();
        node.children(&mut cursor)
            .filter(|child| child.kind() == "decorator")
            .map(|decorator| self.decorator_text(decorator))
            .collect()
    }

    pub(super) fn decorator_text(&self, decorator: Node<'_>) -> String {
        let text = self.text(decorator).trim_start_matches('@');
        text.split_whitespace().collect::<Vec<_>>().join(" ")
    }

    /// The name of the function or class a Python decorator list wraps.
    pub(super) fn decorated_name(&self, node: Node<'_>) -> Option<&'a str> {
        let definition = node.child_by_field_name("definition")?;
        Some(self.text(definition.child_by_field_name("name")?))
    }

    /// Go methods `name` whose receiver's type is `container`.
    pub(super) fn go_methods<'t>(
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

    /// Definitions `name` in the scope reached through `containers`: classes,
    /// functions and object literals, nested in any order.
    pub(super) fn scoped<'t>(
        &self,
        root: Node<'t>,
        containers: &[&str],
        name: &str,
    ) -> Result<Vec<Definition<'t>>, String> {
        let mut scope = Scope {
            kind: Kind::File,
            nodes: self.with_decorators(top_level(root)),
            decorators: Vec::new(),
        };
        for container in containers {
            let mut found = self.containers(&scope, container)?;
            if found.len() != 1 {
                return Err(format!(
                    "expected one class, function or object `{container}`, found {}",
                    found.len()
                ));
            }
            let mut inner = found.remove(0);
            if inner.kind == Kind::Class {
                inner.decorators.extend(scope.decorators.iter().cloned());
            }
            scope = inner;
        }
        let mut found = Vec::new();
        for (node, written) in scope.nodes {
            let definition = match scope.kind {
                Kind::File | Kind::Function => self.function_named(node, name)?,
                Kind::Class => self.method_named(node, name)?,
                Kind::Object => self.object_member_named(node, name)?,
            };
            if let Some(mut definition) = definition {
                definition.decorators.extend(written);
                definition
                    .decorators
                    .extend(scope.decorators.iter().cloned());
                found.push(definition);
            }
        }
        Ok(found)
    }

    /// The scopes named `name` among the declarations of `scope`.
    fn containers<'t>(&self, scope: &Scope<'t>, name: &str) -> Result<Vec<Scope<'t>>, String> {
        let mut found = Vec::new();
        for (node, written) in &scope.nodes {
            found.extend(self.container(*node, written, name, scope.kind)?);
        }
        Ok(found)
    }

    /// The scope `node` opens if it is a class, function or object literal
    /// named `name`. Class decorators reach the methods; a function's
    /// decorators do not reach what it defines inside.
    fn container<'t>(
        &self,
        node: Node<'t>,
        written: &[String],
        name: &str,
        kind: Kind,
    ) -> Result<Option<Scope<'t>>, String> {
        if let Some((class, mut decorators)) = self.class_named(node, name)? {
            decorators.extend(written.iter().cloned());
            return Ok(Some(Scope {
                kind: Kind::Class,
                nodes: self.with_decorators(children(field(class, "body")?)),
                decorators,
            }));
        }
        if let Some(object) = self.object_named(node, name, kind)? {
            return Ok(Some(Scope {
                kind: Kind::Object,
                nodes: self.with_decorators(children(object)),
                decorators: Vec::new(),
            }));
        }
        let function = match kind {
            Kind::File | Kind::Function => self.function_named(node, name)?,
            Kind::Class => self.method_named(node, name)?,
            Kind::Object => self.object_member_named(node, name)?,
        };
        let Some(function) = function else {
            return Ok(None);
        };
        let nodes = match function.node.child_by_field_name("body") {
            Some(body) if matches!(body.kind(), "block" | "statement_block") => top_level(body),
            _ => Vec::new(),
        };
        Ok(Some(Scope {
            kind: Kind::Function,
            nodes: self.with_decorators(nodes),
            decorators: Vec::new(),
        }))
    }

    /// A TypeScript object literal named `name`: a variable's value, or a
    /// property's value inside another object.
    fn object_named<'t>(
        &self,
        node: Node<'t>,
        name: &str,
        kind: Kind,
    ) -> Result<Option<Node<'t>>, String> {
        let (key, value) = match (node.kind(), kind) {
            ("variable_declarator", Kind::File | Kind::Function) => (
                node.child_by_field_name("name"),
                node.child_by_field_name("value"),
            ),
            ("pair", Kind::Object) => (
                node.child_by_field_name("key"),
                node.child_by_field_name("value"),
            ),
            _ => return Ok(None),
        };
        let (Some(key), Some(value)) = (key, value) else {
            return Ok(None);
        };
        if self.text(key) != name {
            return Ok(None);
        }
        let value = if value.kind() == "parenthesized_expression" {
            only(value)?
        } else {
            value
        };
        Ok((value.kind() == "object").then_some(value))
    }

    /// A function of an object literal: a method, whose `this` is the object,
    /// or a property holding an arrow function, whose `this` is not.
    fn object_member_named<'t>(
        &self,
        node: Node<'t>,
        name: &str,
    ) -> Result<Option<Definition<'t>>, String> {
        let named = |n: Node<'_>| self.text(n) == name;
        match node.kind() {
            "method_definition" if node.child_by_field_name("name").is_some_and(named) => {
                Ok(Some(Definition {
                    node,
                    receiver: Some("this".to_owned()),
                    decorators: Vec::new(),
                }))
            }
            "pair" if node.child_by_field_name("key").is_some_and(named) => {
                let value = field(node, "value")?;
                let receiver = match value.kind() {
                    "arrow_function" => None,
                    "function_expression" => Some("this".to_owned()),
                    _ => return Ok(None),
                };
                Ok(Some(Definition {
                    node: value,
                    receiver,
                    decorators: Vec::new(),
                }))
            }
            _ => Ok(None),
        }
    }

    /// Pairs each declaration with the TypeScript decorators written before
    /// it as siblings, as a class body and an `export` hold them.
    pub(super) fn with_decorators<'t>(&self, nodes: Vec<Node<'t>>) -> Vec<(Node<'t>, Vec<String>)> {
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
    pub(super) fn class_named<'t>(
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
        Ok(None)
    }

    pub(super) fn method_named<'t>(
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
}
