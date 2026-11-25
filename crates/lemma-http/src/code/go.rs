//! Go: routes registered with `net/http`, gorilla/mux, chi, gin or echo,
//! requests built with `net/http`, and field names from JSON struct tags.

use tree_sitter::Node;

use super::{BoundPath, Dialect, ImportBinding, VERBS, Walk, has_marker};
use crate::paths::canonical_method;

/// Calls that register a handler for a path.
const HANDLERS: [&str; 3] = ["Handle", "HandleFunc", "Path"];

impl Walk<'_> {
    /// Route registrations, and calls that only format a string. Returns
    /// whether the call was one, so the general rules do not read it as a
    /// request.
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
            "PathPrefix" | "Group" => {
                self.consume_strings(arguments);
                true
            }
            // A formatted path requests nothing itself: it is read with the
            // request it is given to, or as a string of unknown method.
            "Sprintf" => true,
            name if HANDLERS.contains(&name) => {
                self.go_handler(parts, arguments, None);
                true
            }
            name if VERBS.contains(&name.to_ascii_lowercase().as_str()) => {
                self.go_verb_route(parts, name, arguments)
            }
            _ => {
                self.go_router_argument(parts, arguments);
                false
            }
        }
    }

    /// `users.Register(v1.Group("/users"))`: a router passed to a function
    /// that registers routes on it, as gin and echo services split their
    /// routes by package. The function's routes are served under the
    /// router's prefix, wherever it is defined.
    fn go_router_argument(&mut self, parts: &[String], arguments: Node<'_>) {
        if parts.is_empty() {
            return;
        }
        for argument in Self::children(arguments) {
            let Some(prefix) = self.go_router_expression_prefix(argument) else {
                continue;
            };
            self.facts.mounts.push(super::Mount {
                prefix,
                target: parts.join("."),
            });
        }
    }

    /// The prefix of a router expression: `v1.Group("/users")` under `v1`'s
    /// prefix, or a router variable whose prefix is known.
    fn go_router_expression_prefix(&self, node: Node<'_>) -> Option<String> {
        if node.kind() == "identifier" {
            return self.prefixes.get(self.text(node)).cloned();
        }
        if !matches!(
            self.called_name(node).as_deref(),
            Some("Group" | "PathPrefix")
        ) {
            return None;
        }
        let function = node.child_by_field_name("function")?;
        let arguments = node.child_by_field_name("arguments")?;
        let prefix = Self::children(arguments)
            .into_iter()
            .find_map(|argument| self.go_literal_suffix(argument))?;
        let parts = self.chain_parts(function);
        Some(format!("{}{prefix}", self.router_prefix(&parts)))
    }

    /// Tags the route just added, if any, with the function registering it
    /// when its router is not one this file gives a prefix: the router is
    /// then the function's parameter, and a mount naming the function in
    /// another file supplies the prefix.
    fn go_tag_router(&mut self, parts: &[String], added_from: usize, node: Node<'_>) {
        let receiver = parts
            .len()
            .checked_sub(2)
            .and_then(|index| parts.get(index));
        if receiver.is_none_or(|name| self.prefixes.contains_key(name)) {
            return;
        }
        let Some(function) = self.enclosing_function(node) else {
            return;
        };
        for route in self.facts.method_paths.iter_mut().skip(added_from) {
            route.router = Some(function.clone());
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
        let added_from = self.facts.method_paths.len();
        self.add_method_path(method.as_deref(), &template, path_node);
        self.go_tag_router(parts, added_from, path_node);
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
        let added_from = self.facts.method_paths.len();
        self.add_method_path(Some(verb), &template, *path_node);
        self.go_tag_router(parts, added_from, *path_node);
        true
    }

    /// `import rest "example.com/acme/pkg/client/rest"`: the package a name
    /// stands for in this file. An import path is never a request's path.
    /// The standard library, whose paths start with no domain, is not
    /// recorded: nothing Lemma checks is reached through it by name.
    pub(super) fn go_import(&mut self, node: Node<'_>) {
        if self.dialect != Dialect::Go {
            return;
        }
        let Some(written) = node.child_by_field_name("path") else {
            return;
        };
        self.consumed.push(written.id());
        let Some(path) = self.string_value(written) else {
            return;
        };
        let mut segments = path.rsplit('/');
        // `example.com/acme/client/v2` is package `client`.
        let last = segments.next().unwrap_or_default();
        let is_version = last.strip_prefix('v').is_some_and(|digits| {
            !digits.is_empty() && digits.chars().all(|digit| digit.is_ascii_digit())
        });
        let package = if is_version {
            segments.next().unwrap_or(last)
        } else {
            last
        };
        let local = node
            .child_by_field_name("name")
            .map_or(package, |name| self.text(name))
            .to_owned();
        let is_standard = !path.split('/').next().unwrap_or_default().contains('.');
        if is_standard {
            self.standard_packages.push(local);
            return;
        }
        if local == "_" || local == "." {
            return;
        }
        self.facts.imported_modules.push(path.clone());
        self.facts.import_bindings.push(ImportBinding {
            local,
            imported: None,
            module: path,
        });
    }

    /// Whether a call of two names is of a method a Go package exports
    /// (`api.GetTaskWithResponse(...)`): a generated client's methods are
    /// called on a value, which names neither a resource nor an import.
    pub(super) fn is_go_exported_method(&self, chain: &[String]) -> bool {
        self.dialect == Dialect::Go
            && chain.len() == 2
            && !self.standard_packages.contains(&chain[0])
            && chain[1].starts_with(char::is_uppercase)
    }

    /// The method `c.GetTask` names where `c` is the receiver of the method
    /// the call is in: a call of the type's own method, as `this.getTask()`
    /// is elsewhere.
    pub(super) fn go_own_method<'t>(&self, function: Node<'t>) -> Option<Node<'t>> {
        let operand = function
            .child_by_field_name("operand")
            .filter(|operand| operand.kind() == "identifier")?;
        let mut enclosing = function.parent();
        while let Some(current) = enclosing {
            if current.kind() == "method_declaration" {
                break;
            }
            enclosing = current.parent();
        }
        let receiver = enclosing?.child_by_field_name("receiver")?;
        let declared = Self::children(receiver).into_iter().next()?;
        let name = declared.child_by_field_name("name")?;
        (self.text(name) == self.text(operand))
            .then(|| function.child_by_field_name("field"))
            .flatten()
    }

    /// `path := fmt.Sprintf("/v1/users/%s", id)` or `path := "/v1/users"`:
    /// remembers the path a variable holds, by the function it is assigned
    /// in. A variable made from one by a call (`target, err :=
    /// base.Parse(path)`) holds the same path, so a request given either is
    /// read with its method.
    pub(super) fn go_bind_path(&mut self, node: Node<'_>) {
        if self.dialect != Dialect::Go {
            return;
        }
        let (Some(left), Some(right)) = (
            node.child_by_field_name("left"),
            node.child_by_field_name("right"),
        ) else {
            return;
        };
        let (Some(name), Some(value)) = (
            Self::children(left).into_iter().next(),
            Self::children(right).into_iter().next(),
        ) else {
            return;
        };
        if name.kind() != "identifier" {
            return;
        }
        let function = self.location(value).function;
        let is_extended = node
            .child_by_field_name("operator")
            .is_some_and(|operator| self.text(operator) == "+=");
        if is_extended {
            // `path += "/archive"` makes it another path, which is not
            // followed; a query string added to it leaves it the same.
            let adds_a_query = self
                .go_leading_string(value)
                .is_some_and(|added| added.starts_with('?'));
            if !adds_a_query {
                self.bound_paths
                    .remove(&(function, self.text(name).to_owned()));
            }
            return;
        }
        let Some(bound) = self
            .go_written_path(value)
            .or_else(|| self.go_path_handed_on(value, function.as_ref()))
        else {
            return;
        };
        self.bound_paths
            .insert((function, self.text(name).to_owned()), bound);
    }

    /// The string an expression starts with: itself, or the leftmost one
    /// of a concatenation (`"?" + query.Encode()`).
    fn go_leading_string(&self, value: Node<'_>) -> Option<String> {
        let mut leading = value;
        while leading.kind() == "binary_expression" {
            leading = leading.child_by_field_name("left")?;
        }
        self.string_value(leading)
    }

    /// The path `value` writes: a string, or the format of a `Sprintf`.
    fn go_written_path(&self, value: Node<'_>) -> Option<BoundPath> {
        let written = if value.kind() == "call_expression" {
            let function = value.child_by_field_name("function")?;
            let is_format = self
                .chain_parts(function)
                .last()
                .is_some_and(|name| name == "Sprintf");
            let arguments = value.child_by_field_name("arguments")?;
            Self::children(arguments)
                .into_iter()
                .next()
                .filter(|_| is_format)?
        } else {
            value
        };
        let template = self.string_value(written).filter(|text| {
            has_marker(text, &self.options.path_markers) || self.is_under_a_base(text)
        })?;
        Some(BoundPath {
            node: written.id(),
            template,
            location: self.location(written),
        })
    }

    /// The path a call's result holds because the call was given a
    /// variable holding one, in the same function.
    fn go_path_handed_on(&self, value: Node<'_>, function: Option<&String>) -> Option<BoundPath> {
        if value.kind() != "call_expression" {
            return None;
        }
        let arguments = value.child_by_field_name("arguments")?;
        Self::children(arguments)
            .into_iter()
            .filter(|argument| argument.kind() == "identifier")
            .find_map(|argument| {
                let key = (function.cloned(), self.text(argument).to_owned());
                self.bound_paths.get(&key).cloned()
            })
    }

    /// `api := r.PathPrefix("/api").Subrouter()` (gorilla/mux) or
    /// `v1 := r.Group("/api")` (gin, echo): remembers the router's prefix.
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
        let Some(prefix_call) = self
            .find_call(right, "PathPrefix")
            .or_else(|| self.find_call(right, "Group"))
        else {
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
            .find_map(|argument| self.go_literal_suffix(argument))
        else {
            return;
        };
        let parts = self.chain_parts(function);
        let full = format!("{}{prefix}", self.router_prefix(&parts));
        self.prefixes.insert(self.text(name).to_owned(), full);
    }

    /// The constant end of a path expression: all of `"/api"`, and the
    /// `"/api/v1"` of `cfg.BasePath + "/api/v1"`.
    ///
    /// A prefix read from configuration is unknown here, but the literal
    /// after it is what the provider's spec usually spells, so keeping it
    /// beats dropping the whole prefix and recording bare handler paths.
    fn go_literal_suffix(&self, node: Node<'_>) -> Option<String> {
        if let Some(text) = self.string_value(node) {
            return Some(text);
        }
        if node.kind() != "binary_expression" {
            return None;
        }
        let operator = node.child_by_field_name("operator")?;
        if self.text(operator) != "+" {
            return None;
        }
        let suffix = self.string_value(node.child_by_field_name("right")?)?;
        let left = node.child_by_field_name("left")?;
        let Some(before) = self.go_literal_suffix(left) else {
            return Some(suffix);
        };
        Some(format!("{before}{suffix}"))
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
    /// one side of `base + "/v1/x"`, or a path a helper builds: a tagged
    /// template passed to `client.get`, or `request("patch", build_path("/v1/x"))`.
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
