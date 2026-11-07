//! Routers in TypeScript, JavaScript and Python, and the prefixes they add
//! to the routes registered on them: a router made with a prefix
//! (`APIRouter(prefix="/v1")`, `Blueprint(..., url_prefix="/v1")`,
//! `new Router({ prefix: "/v1" })`, `new Hono().basePath("/v1")`), and one
//! mounted under a prefix (`app.use("/v1", charges)`, `app.route("/v1",
//! charges)`, `app.register(charges, { prefix: "/v1" })`,
//! `app.include_router(charges, prefix="/v1")`,
//! `app.register_blueprint(charges, url_prefix="/v1")`).
//!
//! A router is known by how it is made or typed, not by its name, so
//! `const api = express()` registers routes though `api` usually names a
//! client. A mount whose router this file does not make is kept for the
//! file that does ([`super::Mount`]).

use std::collections::BTreeMap;

use tree_sitter::Node;

use super::{Dialect, Mount, Walk};
use crate::paths::canonical_route;

/// Calls and constructors that make a router or an app routes are
/// registered on, by their last name.
const ROUTER_FACTORIES: [&str; 14] = [
    "express",
    "Router",
    "fastify",
    "Fastify",
    "Hono",
    "OpenAPIHono",
    "KoaRouter",
    "APIRouter",
    "FastAPI",
    "Flask",
    "Blueprint",
    "Quart",
    "Sanic",
    "RouteTableDef",
];

/// Types a parameter holding a router is declared with, by their last name.
const ROUTER_TYPES: [&str; 11] = [
    "Router",
    "IRouter",
    "Express",
    "Application",
    "FastifyInstance",
    "Hono",
    "OpenAPIHono",
    "APIRouter",
    "FastAPI",
    "Flask",
    "Blueprint",
];

/// Keys and keyword arguments giving a router or mount its prefix.
const PREFIX_KEYS: [&str; 2] = ["prefix", "url_prefix"];

/// How deep mounts may nest within one file.
const MOUNT_DEPTH: usize = 8;

/// A mount written in this file: `receiver.use(prefix, target)`.
pub(super) struct WrittenMount {
    receiver: String,
    prefix: String,
    target: String,
}

impl Walk<'_> {
    /// Remembers a variable made a router: `const charges = express.Router()`,
    /// `router = APIRouter(prefix="/v1/charges")`.
    pub(super) fn router_definition(&mut self, node: Node<'_>) {
        if !matches!(self.dialect, Dialect::Script | Dialect::Python) {
            return;
        }
        let (name, value) = match node.kind() {
            "assignment" => (
                node.child_by_field_name("left"),
                node.child_by_field_name("right"),
            ),
            _ => (
                node.child_by_field_name("name"),
                node.child_by_field_name("value"),
            ),
        };
        let (Some(name), Some(value)) = (name, value) else {
            return;
        };
        if name.kind() != "identifier" {
            return;
        }
        if let Some(prefix) = self.made_router_prefix(value) {
            self.routers.insert(self.text(name).to_owned(), prefix);
        }
    }

    /// A parameter typed as a router: `function mount(r: Router)`,
    /// `def mount(router: APIRouter)`.
    pub(super) fn router_parameter(&mut self, node: Node<'_>) {
        let (name, annotation) = match node.kind() {
            "typed_parameter" => (node.named_child(0), node.child_by_field_name("type")),
            _ => (
                node.child_by_field_name("pattern"),
                node.child_by_field_name("type"),
            ),
        };
        let (Some(name), Some(annotation)) = (name, annotation) else {
            return;
        };
        let written = self.text(annotation).trim_start_matches(':').trim();
        let last = written.rsplit(['.', ' ']).next().unwrap_or(written);
        if name.kind() == "identifier" && ROUTER_TYPES.contains(&last) {
            self.routers
                .insert(self.text(name).to_owned(), String::new());
        }
    }

    /// The prefix of the router an expression makes, empty for none, or
    /// `None` when it makes no router.
    fn made_router_prefix(&self, value: Node<'_>) -> Option<String> {
        let (function, arguments) = match value.kind() {
            "call_expression" | "call" => (
                value.child_by_field_name("function")?,
                value.child_by_field_name("arguments")?,
            ),
            "new_expression" => (
                value.child_by_field_name("constructor")?,
                value.child_by_field_name("arguments")?,
            ),
            "await_expression" => return self.made_router_prefix(value.named_child(0)?),
            _ => return None,
        };
        let parts = self.chain_parts(function);
        let called = parts.last()?;
        // `new Hono().basePath("/v1")`: the prefix of what it is called on,
        // then its own.
        if called == "basePath" {
            let made = function
                .child_by_field_name("object")
                .and_then(|object| self.made_router_prefix(object))?;
            let own = self.first_string(arguments).unwrap_or_default();
            return Some(format!("{made}{own}"));
        }
        if !ROUTER_FACTORIES.contains(&called.as_str()) {
            return None;
        }
        Some(self.prefix_argument(arguments).unwrap_or_default())
    }

    /// Records a call mounting a router under a prefix, if `call` is one.
    pub(super) fn router_mount(&mut self, parts: &[String], arguments: Node<'_>) {
        if !matches!(self.dialect, Dialect::Script | Dialect::Python) {
            return;
        }
        let [.., receiver, called] = parts else {
            return;
        };
        let listed = Self::children(arguments);
        let mounted = match called.as_str() {
            // `app.use("/v1", auth, charges)`, `app.route("/v1", charges)`.
            "use" | "route" => {
                let Some((path, rest)) = listed.split_first() else {
                    return;
                };
                let prefix = self.string_value(*path);
                let target = rest
                    .iter()
                    .rev()
                    .find_map(|node| self.mounted_router(*node));
                prefix.zip(target)
            }
            // `app.register(charges, { prefix: "/v1" })`,
            // `app.include_router(charges, prefix="/v1")`.
            "register" | "include_router" | "register_blueprint" => {
                let target = listed.first().and_then(|node| self.mounted_router(*node));
                self.prefix_argument(arguments).zip(target)
            }
            _ => None,
        };
        let Some((prefix, target)) = mounted else {
            return;
        };
        if !prefix.starts_with('/') {
            return;
        }
        self.mounts.push(WrittenMount {
            receiver: receiver.clone(),
            prefix,
            target,
        });
    }

    /// The router an argument mounts, as written: `charges`,
    /// `routers.charges`, or `charges` in Koa's `charges.routes()`.
    fn mounted_router(&self, node: Node<'_>) -> Option<String> {
        match node.kind() {
            "identifier" => Some(self.text(node).to_owned()),
            "member_expression" | "attribute" => Some(self.chain_parts(node).join(".")),
            "call_expression" | "call" => {
                let function = node.child_by_field_name("function")?;
                let parts = self.chain_parts(function);
                let [.., object, "routes" | "allowedMethods"] =
                    parts.iter().map(String::as_str).collect::<Vec<_>>()[..]
                else {
                    return None;
                };
                Some(object.to_owned())
            }
            _ => None,
        }
    }

    /// A `prefix` or `url_prefix` given as a keyword argument or an
    /// option's key.
    fn prefix_argument(&self, arguments: Node<'_>) -> Option<String> {
        Self::children(arguments)
            .into_iter()
            .find_map(|argument| match argument.kind() {
                "keyword_argument" => self.named_string(argument, "name", "value"),
                "object" => Self::children(argument)
                    .into_iter()
                    .find_map(|pair| self.named_string(pair, "key", "value")),
                _ => None,
            })
    }

    /// The string value of a keyword argument or pair named as a prefix.
    fn named_string(&self, node: Node<'_>, name_field: &str, value_field: &str) -> Option<String> {
        let name = node.child_by_field_name(name_field)?;
        if !PREFIX_KEYS.contains(&self.text(name).trim_matches(['"', '\''])) {
            return None;
        }
        self.string_value(node.child_by_field_name(value_field)?)
    }

    fn first_string(&self, arguments: Node<'_>) -> Option<String> {
        Self::children(arguments)
            .into_iter()
            .find_map(|argument| self.string_value(argument))
    }

    /// Puts each registered route under the prefixes of its router, made
    /// or mounted in this file, and keeps for other files the routers and
    /// mounts this file leaves open.
    pub(super) fn finish_routers(&mut self) {
        let registered = std::mem::take(&mut self.registered);
        for (index, receiver) in registered {
            let prefix = self.full_prefix(&receiver, MOUNT_DEPTH);
            let is_mounted_here = self.mounts.iter().any(|mount| mount.target == receiver);
            let Some(route) = self.facts.method_paths.get_mut(index) else {
                continue;
            };
            let prefixed = if prefix.is_empty() {
                None
            } else {
                canonical_route(&format!("{prefix}{}", route.template))
            };
            if let Some(path) = prefixed {
                route.path = path;
            }
            if !is_mounted_here {
                route.router = Some(receiver);
            }
        }
        let open: Vec<Mount> = self
            .mounts
            .iter()
            .filter(|mount| !self.routers.contains_key(&mount.target))
            .map(|mount| Mount {
                prefix: format!(
                    "{}{}",
                    self.full_prefix(&mount.receiver, MOUNT_DEPTH),
                    mount.prefix
                ),
                target: mount.target.clone(),
            })
            .collect();
        self.facts.mounts.extend(open);
    }

    /// The prefix before the routes of `router`: where it is mounted in
    /// this file, then its own.
    fn full_prefix(&self, router: &str, depth: usize) -> String {
        let own = self.routers.get(router).map_or("", String::as_str);
        let Some(depth) = depth.checked_sub(1) else {
            return own.to_owned();
        };
        let Some(mount) = self.mounts.iter().find(|mount| mount.target == router) else {
            return own.to_owned();
        };
        let outer = self.full_prefix(&mount.receiver, depth);
        format!("{outer}{}{own}", mount.prefix)
    }
}

/// Routers by name, each with the prefix it was made with.
pub(super) type Routers = BTreeMap<String, String>;
