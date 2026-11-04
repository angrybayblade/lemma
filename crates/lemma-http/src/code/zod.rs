//! Zod schemas in TypeScript services: which ones reject keys they do not
//! list (`.strict()`), and which schema each route's request uses. A
//! request with an unknown key fails against a strict schema and has the
//! key dropped by any other, which decides how serious it is when a
//! consumer still sends a field the service removed.

use tree_sitter::Node;

use super::{Dialect, Walk};
use crate::paths::{canonical_method, canonical_route};

/// The schema a route uses for one part of its request.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, serde::Serialize, serde::Deserialize)]
pub struct SchemaUse {
    /// Upper-case method of the route.
    pub method: String,
    /// Canonical path of the route.
    pub path: String,
    /// `body` or `query`.
    pub part: String,
    /// The schema's name, or `None` for a schema written in place.
    pub schema: Option<String>,
    /// For a schema written in place, whether it is strict.
    pub is_inline_strict: bool,
}

/// A union of schemas, strict only if every member is.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, serde::Serialize, serde::Deserialize)]
pub struct SchemaUnion {
    /// The union's name.
    pub name: String,
    /// Its members by name; `*` stands for a member written in place that
    /// is not strict.
    pub members: Vec<String>,
}

/// How a schema treats keys it does not list, as far as its outer calls say.
enum Strictness {
    Strict,
    Lenient,
    Union(Vec<String>),
}

/// Where following a schema expression inward has got to.
enum Step<'tree> {
    Inner(Node<'tree>),
    Found(Strictness),
}

/// Methods that decide what happens to unknown keys; the outermost wins.
const KEY_POLICIES: [&str; 4] = ["strict", "passthrough", "strip", "nonstrict"];

impl Walk<'_> {
    /// `const X = z.object({..}).strict()` or a union of schemas.
    pub(super) fn zod_definition(&mut self, declarator: Node<'_>) {
        if self.dialect != Dialect::Script {
            return;
        }
        let (Some(name), Some(value)) = (
            declarator.child_by_field_name("name"),
            declarator.child_by_field_name("value"),
        ) else {
            return;
        };
        let name = self.text(name).to_owned();
        match self.strictness(value) {
            Strictness::Strict => self.facts.strict_schemas.push(name),
            Strictness::Union(members) => {
                self.facts.schema_unions.push(SchemaUnion { name, members });
            }
            Strictness::Lenient => {}
        }
    }

    /// `request: { body: { content: { 'application/json': { schema: X } } }, query: Y }`
    /// beside a route's method and path.
    pub(super) fn zod_route_request(&mut self, method: &str, template: &str, request: Node<'_>) {
        let method = canonical_method(method);
        let Some(path) = canonical_route(template).filter(|_| method != "*") else {
            return;
        };
        for (part, value) in self.request_parts(request) {
            let schema_use = match value.kind() {
                "identifier" => SchemaUse {
                    method: method.clone(),
                    path: path.clone(),
                    part,
                    schema: Some(self.text(value).to_owned()),
                    is_inline_strict: false,
                },
                _ => SchemaUse {
                    method: method.clone(),
                    path: path.clone(),
                    part,
                    schema: None,
                    is_inline_strict: matches!(self.strictness(value), Strictness::Strict),
                },
            };
            self.facts.schema_uses.push(schema_use);
        }
    }

    /// The body's schema and the query's, as written.
    fn request_parts<'tree>(&self, request: Node<'tree>) -> Vec<(String, Node<'tree>)> {
        let mut parts = Vec::new();
        for (key, value) in self.pairs(request) {
            match key.as_str() {
                "query" => parts.push(("query".to_owned(), value)),
                "body" => parts.extend(
                    self.find_pair(value, "schema")
                        .map(|schema| ("body".to_owned(), schema)),
                ),
                _ => {}
            }
        }
        parts
    }

    fn strictness(&self, node: Node<'_>) -> Strictness {
        let mut current = node;
        loop {
            match self.strictness_step(current) {
                Step::Inner(inner) => current = inner,
                Step::Found(strictness) => return strictness,
            }
        }
    }

    /// One step inward along a schema expression's outer calls.
    fn strictness_step<'tree>(&self, node: Node<'tree>) -> Step<'tree> {
        match node.kind() {
            "satisfies_expression" | "as_expression" | "parenthesized_expression" => node
                .named_child(0)
                .map_or(Step::Found(Strictness::Lenient), Step::Inner),
            "call_expression" => self.call_step(node),
            _ => Step::Found(Strictness::Lenient),
        }
    }

    /// `x.strict()` decides; `z.union(..)` is a union; `x.openapi(..)` and
    /// other calls lead on to `x`.
    fn call_step<'tree>(&self, call: Node<'tree>) -> Step<'tree> {
        let Some(function) = call.child_by_field_name("function") else {
            return Step::Found(Strictness::Lenient);
        };
        let called = self.chain_parts(function).pop().unwrap_or_default();
        if called == "strict" {
            return Step::Found(Strictness::Strict);
        }
        if KEY_POLICIES.contains(&called.as_str()) {
            return Step::Found(Strictness::Lenient);
        }
        if matches!(called.as_str(), "union" | "discriminatedUnion") {
            return Step::Found(self.union_members(call));
        }
        function
            .child_by_field_name("object")
            .filter(|_| function.kind() == "member_expression")
            .map_or(Step::Found(Strictness::Lenient), Step::Inner)
    }

    /// The members of `z.union([..])` or `z.discriminatedUnion(key, [..])`.
    fn union_members(&self, call: Node<'_>) -> Strictness {
        let Some(arguments) = call.child_by_field_name("arguments") else {
            return Strictness::Lenient;
        };
        let mut cursor = arguments.walk();
        let Some(array) = arguments
            .named_children(&mut cursor)
            .find(|child| child.kind() == "array")
        else {
            return Strictness::Lenient;
        };
        let mut members = Vec::new();
        let mut cursor = array.walk();
        for member in array.named_children(&mut cursor) {
            if member.kind() == "identifier" {
                members.push(self.text(member).to_owned());
                continue;
            }
            if !matches!(self.strictness(member), Strictness::Strict) {
                members.push("*".to_owned());
            }
        }
        Strictness::Union(members)
    }

    /// An object's key-value pairs, keys unquoted.
    fn pairs<'tree>(&self, object: Node<'tree>) -> Vec<(String, Node<'tree>)> {
        if object.kind() != "object" {
            return Vec::new();
        }
        let mut cursor = object.walk();
        object
            .named_children(&mut cursor)
            .filter(|child| child.kind() == "pair")
            .filter_map(|pair| {
                let key = pair.child_by_field_name("key")?;
                let value = pair.child_by_field_name("value")?;
                Some((self.text(key).trim_matches(['"', '\'']).to_owned(), value))
            })
            .collect()
    }

    /// The value of the first pair named `key` at any depth.
    fn find_pair<'tree>(&self, node: Node<'tree>, key: &str) -> Option<Node<'tree>> {
        let mut stack = vec![node];
        while let Some(current) = stack.pop() {
            if let Some((_, value)) = self
                .pairs(current)
                .into_iter()
                .find(|(name, _)| name == key)
            {
                return Some(value);
            }
            let mut cursor = current.walk();
            stack.extend(current.named_children(&mut cursor));
        }
        None
    }
}
