//! Routes registered by calling a router with a path and its handler:
//! Express, Fastify, Hono and Koa routers in TypeScript and JavaScript
//! (`app.post("/charges", handler)`), and `FastAPI` and Flask decorators in
//! Python (`@router.post("/charges")`, `@app.route("/charges", methods=["POST"])`).
//!
//! Such a call looks like a request (`client.post("/charges", body)`), so
//! it counts as a route only with a sign of serving: a decorator, a
//! receiver made or typed as a router (see [`super::routers`]) or named
//! like one, or a function among its arguments.

use tree_sitter::Node;

use super::{Dialect, HTTP_CLIENTS, VERBS, Walk};
use crate::paths::canonical_method;

/// Receivers that register routes, compared in lower case: `app`, and any
/// name ending in `router` (`router`, `v1Router`, `api_router`).
const ROUTER_RECEIVERS: [&str; 4] = ["app", "server", "fastify", "routes"];

/// Nodes that are a function written in place: a handler.
const FUNCTION_LITERALS: [&str; 4] = [
    "arrow_function",
    "function_expression",
    "function",
    "lambda",
];

impl Walk<'_> {
    /// The methods and path node of a route registration, or `None` when
    /// the call is not one.
    pub(super) fn route_registration<'tree>(
        &self,
        call: Node<'tree>,
        parts: &[String],
        arguments: Node<'tree>,
    ) -> Option<(Vec<String>, Node<'tree>)> {
        if !matches!(self.dialect, Dialect::Script | Dialect::Python) {
            return None;
        }
        let called = parts.last()?.to_ascii_lowercase();
        let written_receiver = parts
            .len()
            .checked_sub(2)
            .and_then(|position| parts.get(position))?;
        // A router made or typed as one (`const api = express()`) is one
        // whatever its name.
        let is_made_router = self.routers.contains_key(written_receiver);
        let receiver = written_receiver
            .trim_start_matches('_')
            .to_ascii_lowercase();
        if HTTP_CLIENTS.contains(&receiver.as_str()) && !is_made_router {
            return None;
        }
        let mut cursor = arguments.walk();
        let listed: Vec<Node<'tree>> = arguments.named_children(&mut cursor).collect();
        let path = *listed.first()?;
        let is_route_path = self
            .string_value(path)
            .is_some_and(|text| text.starts_with('/'));
        if !is_route_path {
            return None;
        }
        let is_decorator = call
            .parent()
            .is_some_and(|parent| parent.kind() == "decorator");
        if is_decorator && called == "route" {
            let methods = self.listed_methods(&listed);
            let methods = if methods.is_empty() {
                vec!["GET".to_owned()]
            } else {
                methods
            };
            return Some((methods, path));
        }
        if !VERBS.contains(&called.as_str()) {
            return None;
        }
        let is_router = is_made_router
            || ROUTER_RECEIVERS.contains(&receiver.as_str())
            || receiver.ends_with("router");
        let has_handler = listed[1..]
            .iter()
            .any(|argument| FUNCTION_LITERALS.contains(&argument.kind()));
        (is_decorator || is_router || has_handler).then(|| (vec![canonical_method(&called)], path))
    }

    /// The methods in a Flask `methods=[...]` argument, empty without one.
    fn listed_methods(&self, listed: &[Node<'_>]) -> Vec<String> {
        let keyword = listed.iter().find(|argument| {
            argument.kind() == "keyword_argument"
                && argument
                    .child_by_field_name("name")
                    .is_some_and(|name| self.text(name) == "methods")
        });
        let Some(value) = keyword.and_then(|keyword| keyword.child_by_field_name("value")) else {
            return Vec::new();
        };
        let mut cursor = value.walk();
        value
            .named_children(&mut cursor)
            .filter_map(|item| self.string_value(item))
            .map(|method| canonical_method(&method))
            .collect()
    }
}
