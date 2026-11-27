//! What a request's call is given that is not written out at the call.
//!
//! `axios.post('/v1/users', { name, email })` writes its request where it
//! sends it: every field it sends is a key in the call. `axios.post(
//! '/v1/users', data)` is given a value built elsewhere, in a caller or a
//! form library, and the calling function says nothing about what is in
//! it. A requirement that rests on a name being absent from a request can
//! only hold of the first kind.
//!
//! So for each request, the values among its call's arguments that are
//! not written there are kept, each with the key it sits under: `body`
//! for `{ body: data }`, nothing for a whole argument or a spread. A
//! request no call was read for is taken as passed whole.
//! A rule then tells an envelope (`body: data`, which may hold anything)
//! from a field given a value (`name: form.name`, which holds `name`).
//!
//! Written out means an object, array or literal at the call, or a
//! variable of the same function assigned one. A parameter whose written
//! type is a primitive holds no field; neither does a function, nor a
//! value under a key that only steers the transport (`headers`,
//! `signal`). Everything else may: a parameter, a variable assigned a
//! call's result, a member of another value, a spread.

use std::collections::BTreeSet;

use tree_sitter::Node;

use super::{Walk, is_string_kind};

/// A request, and the values it is passed that are not written out where
/// it is sent.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, serde::Serialize, serde::Deserialize)]
pub struct RequestArguments {
    /// The line of the request, from 1: the line its path or its client
    /// call is recorded at.
    pub line: u32,
    /// The keys under which the call is given a value built elsewhere;
    /// the empty key for a whole argument or a spread, and for a request
    /// no call was read for. Sorted, unique.
    pub built_elsewhere: Vec<String>,
}

/// How many of each fact a call can add have been recorded, so the ones a
/// call adds are those past these counts.
pub(super) struct Recorded {
    path_references: usize,
    method_paths: usize,
    client_calls: usize,
    member_calls: usize,
    waiting: usize,
    consumed: usize,
}

/// Keys of a request's options that steer how it is sent and carry none
/// of its fields or parameters, in the clients code commonly uses
/// (`fetch`, axios, requests, httpx): a value under one is not part of the
/// request's contents.
const TRANSPORT_KEYS: [&str; 18] = [
    "headers",
    "signal",
    "timeout",
    "method",
    "credentials",
    "mode",
    "cache",
    "redirect",
    "keepalive",
    "responseType",
    "withCredentials",
    "cancelToken",
    "validateStatus",
    "onUploadProgress",
    "onDownloadProgress",
    "allow_redirects",
    "verify",
    "stream",
];

/// Types a parameter is written with that hold no field of a request: the
/// primitives of the languages read, and the values that only cancel or
/// scope a request.
const FIELDLESS_TYPES: [&str; 24] = [
    "string",
    "number",
    "boolean",
    "bigint",
    "str",
    "int",
    "float",
    "bool",
    "int8",
    "int16",
    "int32",
    "int64",
    "uint",
    "uint8",
    "uint16",
    "uint32",
    "uint64",
    "float32",
    "float64",
    "byte",
    "rune",
    "error",
    "AbortSignal",
    "context.Context",
];

/// Nodes that are a function with parameters of its own.
const FUNCTIONS: [&str; 10] = [
    "function_declaration",
    "generator_function_declaration",
    "method_definition",
    "arrow_function",
    "function_expression",
    "function_definition",
    "lambda",
    "func_literal",
    "method_declaration",
    "function_item",
];

/// Nodes declaring one parameter with, where the language writes one, its
/// type.
const PARAMETERS: [&str; 6] = [
    "required_parameter",
    "optional_parameter",
    "typed_parameter",
    "typed_default_parameter",
    "parameter_declaration",
    "parameter",
];

impl Walk<'_> {
    /// How much has been recorded so far.
    pub(super) fn recorded(&self) -> Recorded {
        Recorded {
            path_references: self.facts.path_references.len(),
            method_paths: self.facts.method_paths.len(),
            client_calls: self.facts.client_calls.len(),
            member_calls: self.facts.member_calls.len(),
            waiting: self.relative.waiting_count(),
            consumed: self.consumed.len(),
        }
    }

    /// Keeps what the call `node` is given for each request recorded since
    /// `before`, which reading the call recorded: the requests it makes.
    pub(super) fn note_call_arguments(&mut self, node: Node<'_>, before: &Recorded) {
        let lines = self.request_lines(before);
        if lines.is_empty() {
            return;
        }
        let Some(arguments) = node.child_by_field_name("arguments") else {
            return;
        };
        let paths = self.consumed[before.consumed..].to_vec();
        let mut given = BTreeSet::new();
        for argument in Self::children(arguments) {
            self.given_argument(argument, &paths, &mut given);
        }
        self.keep_arguments(&lines, &given);
    }

    /// Keeps what the object `node` holds for each request recorded since
    /// `before`: `{ method: 'POST', url: '/v1/x', body: data }` is the
    /// request, whatever it is then passed to.
    pub(super) fn note_object_arguments(&mut self, node: Node<'_>, before: &Recorded) {
        let lines = self.request_lines(before);
        if lines.is_empty() {
            return;
        }
        let paths = self.consumed[before.consumed..].to_vec();
        let mut given = BTreeSet::new();
        self.given_argument(node, &paths, &mut given);
        self.keep_arguments(&lines, &given);
    }

    /// One argument of a request's call. The argument holding the path
    /// (`paths`, or a variable a path was assigned to) gives the request
    /// nothing but a query, if it puts one in; an object holding it
    /// (`{ url: '/v1/x', body: data }`) gives its other members.
    fn given_argument(&self, argument: Node<'_>, paths: &[usize], out: &mut BTreeSet<String>) {
        if !holds_any(argument, paths) && !self.names_bound_path(argument) {
            self.given_value(argument, "", out);
            return;
        }
        if !matches!(argument.kind(), "object" | "dictionary") {
            if self.puts_in_a_query(argument) {
                out.insert(String::new());
            }
            return;
        }
        for member in Self::children(argument) {
            if !holds_any(member, paths) {
                self.given_member(member, "", out);
            } else if self.puts_in_a_query(member) {
                out.insert(String::new());
            }
        }
    }

    /// The lines of the requests recorded since `before` whose method is
    /// known, or that are calls through a client: a path of any method
    /// (`buildUrl('/v1/x')`) is not known to be requested by the call it
    /// is given to.
    fn request_lines(&mut self, before: &Recorded) -> Vec<u32> {
        let facts = &self.facts;
        let references = facts.path_references[before.path_references..]
            .iter()
            .filter(|reference| reference.method != "*")
            .map(|reference| reference.location.line);
        let descriptors = facts.method_paths[before.method_paths..]
            .iter()
            .filter(|item| item.method != "*" && !item.is_registration)
            .map(|item| item.location.line);
        let of_paths: Vec<u32> = references
            .chain(descriptors)
            .chain(self.relative.waiting_lines(before.waiting))
            .collect();
        self.read_path_lines.extend(of_paths.iter().copied());
        let facts = &self.facts;
        let through_clients = facts.client_calls[before.client_calls..]
            .iter()
            .map(|call| call.location.line);
        let through_members = facts.member_calls[before.member_calls..]
            .iter()
            .map(|call| call.location.line);
        of_paths
            .into_iter()
            .chain(through_clients)
            .chain(through_members)
            .collect()
    }

    /// Two calls recorded at one line are both the request there, so what
    /// either is given is kept.
    fn keep_arguments(&mut self, lines: &[u32], given: &BTreeSet<String>) {
        for line in lines {
            self.request_arguments
                .entry(*line)
                .or_default()
                .extend(given.iter().cloned());
        }
    }

    /// What each request is passed that is built elsewhere, by line. A
    /// path no call was read for (one of any method, or one found as a
    /// string alone) is taken as passed its whole request: whatever sends
    /// it is not in sight, and another call on its line, such as the
    /// helper giving the path its base, is not the request.
    pub(super) fn finish_arguments(&mut self) {
        let mut kept = std::mem::take(&mut self.request_arguments);
        let facts = &self.facts;
        let references = facts
            .path_references
            .iter()
            .map(|reference| reference.location.line);
        let descriptors = facts
            .method_paths
            .iter()
            .filter(|item| !item.is_registration)
            .map(|item| item.location.line);
        for line in references.chain(descriptors) {
            if !self.read_path_lines.contains(&line) {
                kept.entry(line).or_default().insert(String::new());
            }
        }
        self.facts.request_arguments = kept
            .into_iter()
            .map(|(line, built_elsewhere)| RequestArguments {
                line,
                built_elsewhere: built_elsewhere.into_iter().collect(),
            })
            .collect();
    }

    /// `const body = { name }`: keeps what the object is given, so a call
    /// given `body` is given that.
    pub(super) fn bind_written(&mut self, node: Node<'_>) {
        let (name, value) = match node.kind() {
            "assignment" => (
                node.child_by_field_name("left"),
                node.child_by_field_name("right"),
            ),
            "short_var_declaration" => (
                node.child_by_field_name("left")
                    .and_then(|left| left.named_child(0)),
                node.child_by_field_name("right")
                    .and_then(|right| right.named_child(0)),
            ),
            _ => (
                node.child_by_field_name("name"),
                node.child_by_field_name("value"),
            ),
        };
        let (Some(name), Some(value)) = (name, value) else {
            return;
        };
        if name.kind() != "identifier" || !is_written_out(value) {
            return;
        }
        let mut given = BTreeSet::new();
        self.given_value(value, "", &mut given);
        let key = (scope_of(node), self.text(name).to_owned());
        self.written.insert(key, given);
    }

    /// Adds to `out` the keys under which `node`, a value under `key`,
    /// holds something built elsewhere.
    fn given_value(&self, node: Node<'_>, key: &str, out: &mut BTreeSet<String>) {
        match node.kind() {
            kind if is_string_kind(kind) || is_literal(kind) || FUNCTIONS.contains(&kind) => {}
            "await_expression"
            | "await"
            | "parenthesized_expression"
            | "non_null_expression"
            | "as_expression"
            | "satisfies_expression"
            | "unary_expression" => {
                if let Some(inner) = node.named_child(0) {
                    self.given_value(inner, key, out);
                }
            }
            "object" | "dictionary" | "array" | "list" | "literal_value" => {
                for member in Self::children(node) {
                    self.given_member(member, key, out);
                }
            }
            "composite_literal" => {
                if let Some(body) = node.child_by_field_name("body") {
                    self.given_value(body, key, out);
                }
            }
            "keyword_argument" => self.given_member(node, key, out),
            "call_expression" | "call" | "new_expression" => self.given_call(node, key, out),
            "identifier" => self.given_name(node, key, out),
            "selector_expression" if self.is_of_standard_package(node) => {}
            _ => {
                out.insert(key.to_owned());
            }
        }
    }

    /// One member of an object, a dictionary, an array or an argument
    /// list, itself under `key`.
    fn given_member(&self, member: Node<'_>, key: &str, out: &mut BTreeSet<String>) {
        match member.kind() {
            "pair" | "keyword_argument" | "keyed_element" => {
                let name = member
                    .child_by_field_name("key")
                    .or_else(|| member.child_by_field_name("name"))
                    .or_else(|| member.named_child(0));
                let value = member
                    .child_by_field_name("value")
                    .or_else(|| member.named_child(1));
                let (Some(name), Some(value)) = (name, value) else {
                    return;
                };
                // `[field]: value` may set any name.
                if name.kind() == "computed_property_name" {
                    out.insert(key.to_owned());
                    return;
                }
                let name = self.key_text(name);
                if !TRANSPORT_KEYS.contains(&name.as_str()) {
                    self.given_value(value, &name, out);
                }
            }
            "literal_element" => {
                if let Some(inner) = member.named_child(0) {
                    self.given_value(inner, key, out);
                }
            }
            "shorthand_property_identifier" => {
                let name = self.text(member);
                if !TRANSPORT_KEYS.contains(&name) {
                    self.given_name(member, name, out);
                }
            }
            "method_definition" | "comment" => {}
            _ => self.given_value(member, key, out),
        }
    }

    /// A key as a request would carry it: a quoted key without its quotes.
    fn key_text(&self, key: Node<'_>) -> String {
        self.string_value(key)
            .unwrap_or_else(|| self.text(key).to_owned())
    }

    /// A call in place of a value: one given only what is written out
    /// (`JSON.stringify({ name })`, `JSON.stringify(body)` with `body`
    /// assigned an object) passes that on; any other returns a value
    /// built elsewhere.
    fn given_call(&self, node: Node<'_>, key: &str, out: &mut BTreeSet<String>) {
        let arguments = node
            .child_by_field_name("arguments")
            .map(Self::children)
            .unwrap_or_default();
        let wraps_what_is_written = !arguments.is_empty()
            && arguments.iter().all(|argument| {
                is_written_out(*argument)
                    || is_string_kind(argument.kind())
                    || matches!(self.named(*argument), Some(Named::Written(_)))
            });
        if !wraps_what_is_written {
            out.insert(key.to_owned());
            return;
        }
        for argument in arguments {
            self.given_value(argument, key, out);
        }
    }

    /// A name in place of a value: a variable assigned something written
    /// out stands for it, a parameter of a fieldless type holds no field,
    /// and any other name is of a value built elsewhere.
    fn given_name(&self, node: Node<'_>, key: &str, out: &mut BTreeSet<String>) {
        if matches!(
            self.text(node),
            "undefined" | "None" | "nil" | "True" | "False"
        ) {
            return;
        }
        let under = match self.named(node) {
            Some(Named::Written(written)) => written
                .iter()
                .map(|inner| if inner.is_empty() { key } else { inner })
                .collect(),
            Some(Named::Parameter {
                holds_fields: false,
            }) => Vec::new(),
            _ => vec![key],
        };
        out.extend(under.into_iter().map(str::to_owned));
    }

    /// What the name `node` is of, looked up from the innermost function
    /// outward, so a parameter hides an outer variable of its name; `None`
    /// for a node that is no name, or a name of neither.
    fn named(&self, node: Node<'_>) -> Option<Named<'_>> {
        if !matches!(node.kind(), "identifier" | "shorthand_property_identifier") {
            return None;
        }
        let name = self.text(node);
        let mut inside = node;
        loop {
            let function = function_around(inside);
            let scope = function.map(|function| function.start_byte());
            if let Some(written) = self.written.get(&(scope, name.to_owned())) {
                return Some(Named::Written(written));
            }
            let function = function?;
            if let Some(holds_fields) = self.parameter_holds_fields(function, name) {
                return Some(Named::Parameter { holds_fields });
            }
            inside = function;
        }
    }

    /// Whether the parameter `name` of `function` may hold a field of a
    /// request: not when its type, written whole for it, is a fieldless
    /// one. `None` when `function` has no such parameter.
    fn parameter_holds_fields(&self, function: Node<'_>, name: &str) -> Option<bool> {
        let parameters = function
            .child_by_field_name("parameters")
            .or_else(|| function.child_by_field_name("parameter"))?;
        if parameters.kind() == "identifier" {
            return (self.text(parameters) == name).then_some(true);
        }
        let mut pending = vec![(parameters, None::<Node<'_>>)];
        while let Some((node, declaration)) = pending.pop() {
            let declaration = if PARAMETERS.contains(&node.kind()) {
                Some(node)
            } else {
                declaration
            };
            let written_type =
                declaration.and_then(|declaration| declaration.child_by_field_name("type"));
            if written_type.is_some_and(|written| written.id() == node.id()) {
                continue;
            }
            let is_name = matches!(
                node.kind(),
                "identifier" | "shorthand_property_identifier_pattern"
            );
            if is_name && self.text(node) == name {
                // A name taken out of a pattern has the pattern's type, not
                // one of its own.
                let is_fieldless = written_type
                    .filter(|_| is_child_of(node, declaration))
                    .is_some_and(|written| is_fieldless(self.text(written)));
                return Some(!is_fieldless);
            }
            let children = Self::children(node);
            pending.extend(children.into_iter().map(|child| (child, declaration)));
        }
        None
    }

    /// Whether `node` is a name a variable holding a path was given,
    /// whose request the call makes.
    fn names_bound_path(&self, node: Node<'_>) -> bool {
        let Some(variable) = Self::handed_variable(node) else {
            return false;
        };
        let key = (
            self.enclosing_function(node),
            self.text(variable).to_owned(),
        );
        self.bound_paths.contains_key(&key)
    }

    /// Whether `node`, which holds a request's path, puts a value in after
    /// the path's `?`: a query built elsewhere, whose parameters are not
    /// written at the call.
    fn puts_in_a_query(&self, node: Node<'_>) -> bool {
        let mut pending = vec![node];
        while let Some(current) = pending.pop() {
            let is_alone = current.id() == node.id();
            if is_string_kind(current.kind()) && starts_a_query(self.text(current), is_alone) {
                return true;
            }
            pending.extend(Self::children(current));
        }
        false
    }

    /// Whether `node` is a member of a package of Go's standard library
    /// (`http.MethodPost`): a constant, not a value built for the call.
    fn is_of_standard_package(&self, node: Node<'_>) -> bool {
        node.child_by_field_name("operand").is_some_and(|operand| {
            self.standard_packages
                .iter()
                .any(|name| name == self.text(operand))
        })
    }
}

/// What a name in a function is of.
enum Named<'a> {
    /// A variable assigned something written out: the keys under which
    /// that holds a value built elsewhere.
    Written(&'a BTreeSet<String>),
    /// A parameter, and whether it may hold a field of a request.
    Parameter { holds_fields: bool },
}

/// Whether `text`, a string in a path's argument, starts a query that a
/// value is put into: `?${query}`, `?{query}` or `?%s` in the string, or
/// a `?` in a string that is one piece of the argument among others.
fn starts_a_query(text: &str, is_alone: bool) -> bool {
    text.split_once('?')
        .is_some_and(|(_, query)| !is_alone || query.contains('{') || query.contains('%'))
}

/// Whether `node` is a child of `parent`.
fn is_child_of(node: Node<'_>, parent: Option<Node<'_>>) -> bool {
    let above = node.parent().map(|above| above.id());
    above.is_some() && above == parent.map(|parent| parent.id())
}

/// Whether `node` or a node inside it is one of `ids`.
fn holds_any(node: Node<'_>, ids: &[usize]) -> bool {
    if ids.is_empty() {
        return false;
    }
    let mut pending = vec![node];
    while let Some(current) = pending.pop() {
        if ids.contains(&current.id()) {
            return true;
        }
        let mut cursor = current.walk();
        pending.extend(current.named_children(&mut cursor));
    }
    false
}

/// The innermost function around `node`, not `node` itself.
fn function_around(node: Node<'_>) -> Option<Node<'_>> {
    let mut around = node.parent();
    while let Some(candidate) = around {
        if FUNCTIONS.contains(&candidate.kind()) {
            return Some(candidate);
        }
        around = candidate.parent();
    }
    None
}

/// What names the scope a variable of `node`'s place is declared in:
/// where the innermost function around it starts, or nothing outside
/// functions.
fn scope_of(node: Node<'_>) -> Option<usize> {
    function_around(node).map(|function| function.start_byte())
}

fn is_literal(kind: &str) -> bool {
    matches!(
        kind,
        "number"
            | "integer"
            | "float"
            | "true"
            | "false"
            | "null"
            | "none"
            | "nil"
            | "undefined"
            | "int_literal"
            | "float_literal"
            | "regex"
            | "comment"
    )
}

/// Whether `node` is written out where it stands: an object, an array or
/// a literal, rather than a name for something made elsewhere.
fn is_written_out(node: Node<'_>) -> bool {
    match node.kind() {
        "object" | "dictionary" | "array" | "list" | "composite_literal" => true,
        "unary_expression"
        | "as_expression"
        | "satisfies_expression"
        | "parenthesized_expression" => node.named_child(0).is_some_and(is_written_out),
        kind => is_literal(kind),
    }
}

/// Whether a parameter's type, as written, holds no field: a fieldless
/// type, alone or allowed to be absent (`string | undefined`,
/// `Optional[str]`).
fn is_fieldless(written: &str) -> bool {
    let written = written.trim_start_matches(':').trim();
    let plain = written
        .strip_prefix("Optional[")
        .and_then(|inner| inner.strip_suffix(']'))
        .unwrap_or(written);
    plain
        .split('|')
        .map(str::trim)
        .filter(|part| !matches!(*part, "undefined" | "null" | "None"))
        .all(|part| FIELDLESS_TYPES.contains(&part))
}

#[cfg(test)]
mod tests {
    use lemma_syntax::Extractor;

    use crate::code::{CodeOptions, extract};

    /// The requests of `source`, each with the keys it is passed a value
    /// built elsewhere under.
    fn arguments(file: &str, source: &str) -> Vec<(u32, Vec<String>)> {
        let mut extractor = Extractor::new().unwrap();
        let options = CodeOptions {
            path_markers: vec!["/v1/".to_owned()],
            ..CodeOptions::default()
        };
        let facts = extract(file, source, &options, &mut extractor).unwrap();
        facts
            .request_arguments
            .into_iter()
            .map(|request| (request.line, request.built_elsewhere))
            .collect()
    }

    fn written(line: u32) -> (u32, Vec<String>) {
        (line, Vec::new())
    }

    fn given(line: u32, keys: &[&str]) -> (u32, Vec<String>) {
        (line, keys.iter().map(|key| (*key).to_owned()).collect())
    }

    #[test]
    fn a_field_given_a_value_is_kept_with_its_key() {
        let source = "export async function create(name: string, form: Form) {\n  return axios.post('/v1/users', { name, email: form.email })\n}\n";

        let found = arguments("src/users.ts", source);

        assert_eq!(found, vec![given(2, &["email"])]);
    }

    #[test]
    fn a_parameter_passed_whole_is_built_elsewhere() {
        let source = "export async function create(data: CreateUser) {\n  return axios.post('/v1/users', data)\n}\n";

        let found = arguments("src/users.ts", source);

        assert_eq!(found, vec![given(2, &[""])]);
    }

    #[test]
    fn a_value_under_a_key_is_kept_with_the_key() {
        let source = "export async function create(data: CreateUser) {\n  return request({ method: 'POST', url: '/v1/users', body: data })\n}\n";

        let found = arguments("src/users.ts", source);

        assert_eq!(found, vec![given(2, &["body"])]);
    }

    #[test]
    fn a_spread_is_built_elsewhere() {
        let source = "export async function create(name: string, rest: Rest) {\n  return axios.post('/v1/users', { name, ...rest })\n}\n";

        let found = arguments("src/users.ts", source);

        assert_eq!(found, vec![given(2, &[""])]);
    }

    #[test]
    fn a_parameter_of_a_primitive_type_holds_no_field() {
        let source = "export async function show(id: string, signal?: AbortSignal) {\n  return axios.get(`/v1/users/${id}`, { signal })\n}\n";

        let found = arguments("src/users.ts", source);

        assert_eq!(found, vec![written(2)]);
    }

    #[test]
    fn a_variable_assigned_an_object_stands_for_the_object() {
        let source = "export async function create(name: string, rest: Rest) {\n  const body = { name, tags: rest.tags }\n  return fetch('/v1/users', { method: 'POST', body: JSON.stringify(body) })\n}\n";

        let found = arguments("src/users.ts", source);

        assert_eq!(found, vec![given(3, &["tags"])]);
    }

    #[test]
    fn a_variable_assigned_a_calls_result_is_built_elsewhere() {
        let source = "export async function create(form: Form) {\n  const body = toRequest(form)\n  return axios.post('/v1/users', body)\n}\n";

        let found = arguments("src/users.ts", source);

        assert_eq!(found, vec![given(3, &[""])]);
    }

    #[test]
    fn a_value_under_a_transport_key_is_no_part_of_the_request() {
        let source = "export async function list(options: Options) {\n  return axios.get('/v1/users', { headers: options.headers, timeout: options.timeout })\n}\n";

        let found = arguments("src/users.ts", source);

        assert_eq!(found, vec![written(2)]);
    }

    #[test]
    fn a_query_put_into_the_path_is_built_elsewhere() {
        let source = "export async function list(filter: Filter) {\n  const query = new URLSearchParams(filter)\n  return axios.get(`/v1/users?${query}`)\n}\n";

        let found = arguments("src/users.ts", source);

        assert_eq!(found, vec![given(3, &[""])]);
    }

    #[test]
    fn a_path_given_to_a_call_of_no_known_method_is_passed_its_whole_request() {
        let source =
            "export function usersUrl(id: string) {\n  return buildUrl(`/v1/users/${id}`)\n}\n";

        let found = arguments("src/users.ts", source);

        assert_eq!(found, vec![given(2, &[""])]);
    }

    #[test]
    fn python_keyword_arguments_are_keys() {
        let source = "def create(session, payload, name: str):\n    session.post(\"/v1/users\", json={\"name\": name, \"profile\": payload})\n    return requests.post(\"/v1/users\", json=payload, timeout=5)\n";

        let found = arguments("app/users.py", source);

        assert_eq!(found, vec![given(2, &["profile"]), given(3, &["json"])]);
    }

    #[test]
    fn a_parameter_hides_an_outer_variable_of_its_name() {
        let source = "const data = { name: 'fixed' }\nexport async function create(data: CreateUser) {\n  return axios.post('/v1/users', data)\n}\nexport async function seed() {\n  return axios.post('/v1/users', data)\n}\n";

        let found = arguments("src/users.ts", source);

        assert_eq!(found, vec![given(3, &[""]), written(6)]);
    }

    #[test]
    fn another_call_on_the_line_of_a_path_of_no_known_method_is_not_its_request() {
        let source = "import { base } from './base'\nexport function open(name: string) {\n  location.assign(base() + '/v1/files/' + name)\n}\n";

        let found = arguments("src/files.ts", source);

        assert_eq!(found, vec![given(3, &[""])]);
    }
}
