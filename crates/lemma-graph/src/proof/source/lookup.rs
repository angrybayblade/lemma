//! Finding the definition a symbol names: a top-level function, a method of
//! a class, or a Go method by its receiver.

use tree_sitter::Node;

use super::{Definition, Language, Reader, children, field, is_overload, only, top_level};

impl<'a> Reader<'a> {
    /// Finds the one definition `symbol` names: a top-level function, or a
    /// method written `Container.method`, where Python classes may nest.
    pub(super) fn lookup<'t>(
        &self,
        root: Node<'t>,
        symbol: &str,
    ) -> Result<Definition<'t>, String> {
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

    /// Methods `name` of the class reached through `containers`.
    pub(super) fn class_methods<'t>(
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
    pub(super) fn classes_named<'t>(
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
