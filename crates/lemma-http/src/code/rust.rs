//! Rust: routes declared with axum, actix-web or Rocket, requests made
//! with reqwest, field names from structs, serde renames and `json!`, and
//! `use` paths within a crate.

use tree_sitter::Node;

use super::{VERBS, Walk};
use crate::paths::canonical_method;

impl Walk<'_> {
    /// Route declarations. Returns whether the call was one, so the general
    /// rules do not read it again as a request.
    pub(super) fn rust_call(&mut self, parts: &[String], arguments: Node<'_>) -> bool {
        if parts.last().map(String::as_str) != Some("route") {
            return false;
        }
        // axum: `.route("/x", get(list).post(create))`.
        let arguments = Self::children(arguments);
        let [path_node, handlers, ..] = arguments.as_slice() else {
            return false;
        };
        let Some(template) = self.string_value(*path_node) else {
            return false;
        };
        for method in self.rust_method_router(*handlers) {
            self.add_method_path(Some(&method), &template, *path_node);
        }
        self.consumed.push(path_node.id());
        true
    }

    /// The verbs of an axum method router: `get(a).post(b)` or
    /// `routing::delete(c)`.
    fn rust_method_router(&self, node: Node<'_>) -> Vec<String> {
        let mut methods = Vec::new();
        let mut stack = vec![node];
        while let Some(current) = stack.pop() {
            let verb = self
                .called_name(current)
                .filter(|name| VERBS.contains(&name.as_str()));
            methods.extend(verb.map(|verb| canonical_method(&verb)));
            if matches!(current.kind(), "call_expression" | "field_expression") {
                stack.extend(Self::children(current));
            }
        }
        methods.sort();
        methods
    }

    /// actix-web and Rocket: `#[get("/x")]` on a handler.
    pub(super) fn rust_route_attribute(&mut self, node: Node<'_>) {
        let Some(attribute) = node
            .named_child(0)
            .filter(|child| child.kind() == "attribute")
        else {
            return;
        };
        let Some(name) = attribute.named_child(0) else {
            return;
        };
        let verb = self.text(name).to_owned();
        if !VERBS.contains(&verb.as_str()) {
            return;
        }
        let Some(arguments) = attribute.child_by_field_name("arguments") else {
            return;
        };
        let Some(path_node) = Self::children(arguments)
            .into_iter()
            .find(|child| self.string_value(*child).is_some())
        else {
            return;
        };
        let template = self.string_value(path_node).unwrap_or_default();
        self.add_method_path(Some(&verb), &template, path_node);
    }

    /// `Method::POST` among a call's arguments, as `POST`.
    pub(super) fn rust_method_constant(&self, arguments: Node<'_>) -> Option<String> {
        Self::children(arguments).into_iter().find_map(|argument| {
            if argument.kind() != "scoped_identifier" {
                return None;
            }
            let parts = self.chain_parts(argument);
            let [.., owner, constant] = parts.as_slice() else {
                return None;
            };
            let method = canonical_method(constant);
            (owner == "Method" && method != "*").then_some(method)
        })
    }

    /// A field's name on the wire: a struct field (serde's default), a
    /// `#[serde(rename = "x")]`, a field in a struct literal or access, or a
    /// key in `json!({ "x": .. })`.
    pub(super) fn rust_field_name(&self, node: Node<'_>) -> Option<String> {
        match node.kind() {
            "field_declaration" => node
                .child_by_field_name("name")
                .map(|name| self.text(name).to_owned()),
            "field_initializer" => node
                .child_by_field_name("field")
                .map(|field| self.text(field).to_owned()),
            "shorthand_field_initializer" => {
                node.named_child(0).map(|name| self.text(name).to_owned())
            }
            "field_expression" => node
                .child_by_field_name("field")
                .map(|field| self.text(field).to_owned()),
            "attribute_item" => {
                let text = self.text(node);
                let start = text.find("rename")?;
                let rest = &text[start..];
                let quote = rest.find('"')? + 1;
                let end = rest[quote..].find('"')?;
                text.contains("serde")
                    .then(|| rest[quote..quote + end].to_owned())
            }
            "string_literal" => {
                let mut enclosing = node.parent();
                while enclosing.is_some_and(|parent| parent.kind() == "token_tree") {
                    enclosing = enclosing.and_then(|parent| parent.parent());
                }
                let in_json = enclosing
                    .filter(|parent| parent.kind() == "macro_invocation")
                    .and_then(|invocation| invocation.child_by_field_name("macro"))
                    .is_some_and(|name| self.text(name).ends_with("json"));
                let is_key = node
                    .next_sibling()
                    .is_some_and(|next| self.text(next) == ":");
                (in_json && is_key)
                    .then(|| self.string_value(node))
                    .flatten()
            }
            _ => None,
        }
    }

    /// The module a `use` names within its crate: `crate::models::Tool`
    /// becomes `crate::models::Tool`, `super::types::{A, B}` becomes
    /// `super::types`. Paths into other crates are left out.
    pub(super) fn rust_use(&mut self, node: Node<'_>) {
        let Some(argument) = node.child_by_field_name("argument") else {
            return;
        };
        let text = self.text(argument);
        let path = text.split("::{").next().unwrap_or(text).trim();
        let is_local = ["crate::", "super::", "self::"]
            .iter()
            .any(|prefix| path.starts_with(prefix));
        if is_local {
            self.facts.imports.push(path.to_owned());
        }
    }
}
