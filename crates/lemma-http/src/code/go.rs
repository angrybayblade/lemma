//! Go: routes registered with `net/http`, gorilla/mux, chi, gin or echo,
//! requests built with `net/http`, and field names from JSON struct tags.

use tree_sitter::Node;

use super::{Dialect, VERBS, Walk};
use crate::paths::canonical_method;

/// Calls that register a handler for a path.
const HANDLERS: [&str; 3] = ["Handle", "HandleFunc", "Path"];

impl Walk<'_> {
    /// Route registrations. Returns whether the call was one, so the
    /// general rules do not read it again as a request.
    pub(super) fn go_call(
        &mut self,
        parts: &[String],
        function: Node<'_>,
        arguments: Node<'_>,
    ) -> bool {
        let Some(called) = parts.last().map(String::as_str) else {
            return false;
        };
        match called {
            "Methods" => self.go_methods(function, arguments),
            "PathPrefix" => {
                self.consume_strings(arguments);
                true
            }
            name if HANDLERS.contains(&name) => {
                self.go_handler(parts, arguments, None);
                true
            }
            name if VERBS.contains(&name.to_ascii_lowercase().as_str()) => {
                self.go_verb_route(parts, name, arguments)
            }
            _ => false,
        }
    }

    /// `r.Handle("/x", h).Methods("GET", http.MethodHead)`.
    fn go_methods(&mut self, function: Node<'_>, arguments: Node<'_>) -> bool {
        let Some(registration) = function
            .child_by_field_name("operand")
            .filter(|operand| operand.kind() == "call_expression")
        else {
            return false;
        };
        let (Some(inner_function), Some(inner_arguments)) = (
            registration.child_by_field_name("function"),
            registration.child_by_field_name("arguments"),
        ) else {
            return false;
        };
        let inner_parts = self.chain_parts(inner_function);
        let is_handler = inner_parts
            .last()
            .is_some_and(|name| HANDLERS.contains(&name.as_str()));
        if !is_handler {
            return false;
        }
        let mut methods: Vec<String> = Self::children(arguments)
            .into_iter()
            .filter_map(|argument| self.string_value(argument))
            .collect();
        methods.extend(self.go_method_constant(arguments));
        for method in methods {
            self.go_handler(&inner_parts, inner_arguments, Some(&method));
        }
        true
    }

    /// The path a handler is registered for, with its router's prefix.
    fn go_handler(&mut self, parts: &[String], arguments: Node<'_>, method: Option<&str>) {
        let Some(path_node) = Self::children(arguments).into_iter().next() else {
            return;
        };
        let Some(pattern) = self.string_value(path_node) else {
            return;
        };
        if method.is_none() && self.consumed.contains(&path_node.id()) {
            return; // already read with its `.Methods(...)`
        }
        // Go 1.22 patterns carry their method: `mux.HandleFunc("GET /x", h)`.
        let (written_method, path) = match pattern.split_once(' ') {
            Some((verb, path)) if canonical_method(verb) != "*" => {
                (Some(canonical_method(verb)), path.trim())
            }
            _ => (None, pattern.as_str()),
        };
        let template = format!("{}{path}", self.router_prefix(parts));
        let method = method.map(str::to_owned).or(written_method);
        self.add_method_path(method.as_deref(), &template, path_node);
    }

    /// `r.Get("/x", handler)` (chi, gin, echo): a verb-named call with a
    /// relative path and a handler. `http.Get(url)` is a request instead.
    fn go_verb_route(&mut self, parts: &[String], verb: &str, arguments: Node<'_>) -> bool {
        let arguments = Self::children(arguments);
        let [path_node, handler, ..] = arguments.as_slice() else {
            return false;
        };
        let Some(path) = self.string_value(*path_node) else {
            return false;
        };
        if !path.starts_with('/') || self.string_value(*handler).is_some() {
            return false;
        }
        let template = format!("{}{path}", self.router_prefix(parts));
        self.add_method_path(Some(verb), &template, *path_node);
        true
    }

    /// `api := r.PathPrefix("/api").Subrouter()`: remembers `api`'s prefix.
    pub(super) fn go_router_prefix(&mut self, node: Node<'_>) {
        if self.dialect != Dialect::Go {
            return;
        }
        let (Some(left), Some(right)) = (
            node.child_by_field_name("left"),
            node.child_by_field_name("right"),
        ) else {
            return;
        };
        let Some(name) = Self::children(left).into_iter().next() else {
            return;
        };
        let Some(prefix_call) = self.find_call(right, "PathPrefix") else {
            return;
        };
        let (Some(function), Some(arguments)) = (
            prefix_call.child_by_field_name("function"),
            prefix_call.child_by_field_name("arguments"),
        ) else {
            return;
        };
        let Some(prefix) = Self::children(arguments)
            .into_iter()
            .find_map(|argument| self.string_value(argument))
        else {
            return;
        };
        let parts = self.chain_parts(function);
        let full = format!("{}{prefix}", self.router_prefix(&parts));
        self.prefixes.insert(self.text(name).to_owned(), full);
    }

    /// `http.MethodPost` among a call's arguments, as `POST`.
    pub(super) fn go_method_constant(&self, arguments: Node<'_>) -> Option<String> {
        Self::children(arguments).into_iter().find_map(|argument| {
            if argument.kind() != "selector_expression" {
                return None;
            }
            let parts = self.chain_parts(argument);
            let [package, constant] = parts.as_slice() else {
                return None;
            };
            let method = constant.strip_prefix("Method")?;
            (package == "http").then(|| canonical_method(method))
        })
    }

    /// A string naming an API path inside an argument, such as the format
    /// of `fmt.Sprintf("%s/v1/x", base)` or Rust's `&format!("{base}/v1/x")`,
    /// one side of `base + "/v1/x"`, or a path built by a helper:
    /// `client.get(path\`/v1/x/${id}\`)`, `request("patch", build_path("/v1/x"))`.
    pub(super) fn nested_path_argument<'tree>(
        &self,
        arguments: Node<'tree>,
    ) -> Option<Node<'tree>> {
        let mut stack = Self::children(arguments);
        while let Some(node) = stack.pop() {
            if self.names_path(node) {
                return Some(node);
            }
            let is_wrapper = matches!(
                node.kind(),
                "call_expression"
                    | "call"
                    | "argument_list"
                    | "arguments"
                    | "binary_expression"
                    | "macro_invocation"
                    | "token_tree"
                    | "reference_expression"
            );
            if is_wrapper {
                stack.extend(Self::children(node));
            }
        }
        None
    }

    fn names_path(&self, node: Node<'_>) -> bool {
        self.string_value(node)
            .is_some_and(|text| crate::paths::has_marker(&text, &self.options.path_markers))
    }

    /// The JSON name in a struct field's tag, or a string key in a keyed
    /// literal: the names on the wire, which Go's own field names are not.
    pub(super) fn go_field_name(&self, node: Node<'_>) -> Option<String> {
        if node.kind() == "keyed_element" {
            let mut key = node.named_child(0)?;
            if key.kind() == "literal_element" {
                key = key.named_child(0)?;
            }
            return self.string_value(key);
        }
        let tag = self.text(node.child_by_field_name("tag")?);
        let start = tag.find("json:\"")? + "json:\"".len();
        let value = &tag[start..];
        let name = value[..value.find('"')?].split(',').next()?;
        (!name.is_empty() && name != "-").then(|| name.to_owned())
    }

    fn router_prefix(&self, parts: &[String]) -> String {
        let receiver = parts
            .len()
            .checked_sub(2)
            .and_then(|index| parts.get(index));
        receiver
            .and_then(|name| self.prefixes.get(name))
            .cloned()
            .unwrap_or_default()
    }

    fn consume_strings(&mut self, arguments: Node<'_>) {
        for argument in Self::children(arguments) {
            if self.string_value(argument).is_some() {
                self.consumed.push(argument.id());
            }
        }
    }

    fn find_call<'tree>(&self, node: Node<'tree>, name: &str) -> Option<Node<'tree>> {
        let mut stack = vec![node];
        while let Some(current) = stack.pop() {
            if self.called_name(current).as_deref() == Some(name) {
                return Some(current);
            }
            stack.extend(Self::children(current));
        }
        None
    }

    /// The last name of the function a call expression calls.
    pub(super) fn called_name(&self, node: Node<'_>) -> Option<String> {
        if node.kind() != "call_expression" {
            return None;
        }
        let function = node.child_by_field_name("function")?;
        self.chain_parts(function).pop()
    }

    pub(super) fn children(node: Node<'_>) -> Vec<Node<'_>> {
        let mut cursor = node.walk();
        node.named_children(&mut cursor).collect()
    }
}
