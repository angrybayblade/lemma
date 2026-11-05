//! Calls through a client library, found without being declared. A client's
//! own code says what each of its methods requests: `makeAuthConfigs.list`
//! issues `GET /v1/auth_configs`, `AuthConfigsResource.update_status`
//! issues `PATCH /v1/auth_configs/{}/{}`. A consumer's call through a chain
//! (`this.client.authConfigs.list()`, `self._client.auth_configs.list()`)
//! reaches the method whose name is the chain's last name and whose
//! resource names (its file's path and its enclosing names) include the
//! name before it and as many of the earlier ones as possible, then has the
//! fewest names the chain does not mention: `client.tools.list()` is
//! `resources/tools`, not `resources/internal/logs/tools`.
//!
//! A chain counts as a call through the client only where the consumer
//! uses the client: its file imports the client's package, or its
//! repository depends on the package and the chain starts from something
//! local (`this`, `self`, a parameter) rather than another import, such as
//! a database client's `prisma.authConfig.delete()`.
//!
//! A client generated at build time has no code to read: its module is
//! imported but no file holds it. Its functions are named after the
//! operations' `operationId`s (`listTools`, `list_tools`), so a call of a
//! function imported from such a module, directly or through a module that
//! re-exports it whole, is a call of the operation it is named after.

use std::collections::{BTreeMap, BTreeSet};

use lemma_http::MemberCall;

/// A client method that issues a request.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord)]
pub(crate) struct ClientMethod {
    /// The client it belongs to.
    pub client: String,
    /// Its enclosing names and method, as the client's code writes them:
    /// `makeAuthConfigs.list`.
    pub symbol: String,
    /// Upper-case HTTP method.
    pub method: String,
    /// Canonical path.
    pub path: String,
    /// The file it is in.
    pub file: String,
    /// Its line.
    pub line: u32,
}

impl ClientMethod {
    /// How facts name it: `file:Symbol`, since one symbol (`ToolsResource.list`)
    /// can be defined in several files.
    pub fn key(&self) -> String {
        format!("{}:{}", self.file, self.symbol)
    }

    /// Its name, normalized: `updatestatus` for `update_status`.
    fn name(&self) -> String {
        normalized(self.symbol.rsplit('.').next().unwrap_or(&self.symbol))
    }

    /// The names its resource goes by, normalized: the directories and
    /// stem of its file, and its enclosing names.
    fn resource_names(&self) -> BTreeSet<String> {
        let file = self
            .file
            .rsplit_once('.')
            .map_or(self.file.as_str(), |(stem, _)| stem);
        let enclosing = self.symbol.rsplit_once('.').map_or("", |(outer, _)| outer);
        file.split('/')
            .chain(enclosing.split('.'))
            .map(normalized)
            .filter(|name| !name.is_empty())
            .collect()
    }
}

/// An operation a generated client's function may be named after.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord)]
pub(crate) struct NamedOperation {
    /// The service serving it.
    pub service: String,
    /// Upper-case HTTP method.
    pub method: String,
    /// Canonical path.
    pub path: String,
    /// The document describing it.
    pub document: String,
}

/// What the system's clients offer: their methods, the package names a
/// consumer imports or depends on them by, and the names a generated client
/// may give each operation.
#[derive(Debug, Clone, Default)]
pub(crate) struct Clients {
    methods: Vec<ClientMethod>,
    packages: BTreeSet<String>,
    generated: BTreeMap<String, BTreeSet<NamedOperation>>,
}

impl Clients {
    /// From client methods, package names, and operations by the names a
    /// generated client may give them.
    pub fn new(
        methods: Vec<ClientMethod>,
        packages: BTreeSet<String>,
        generated: BTreeMap<String, BTreeSet<NamedOperation>>,
    ) -> Self {
        Self {
            methods: one_per_request(methods),
            packages,
            generated,
        }
    }

    /// The operation a generated client's function is named after, if only
    /// one is; otherwise the one whose service the client's module names
    /// (`generated/billing` for `billing`).
    pub fn named_operation(&self, function: &str, module: &str) -> Option<&NamedOperation> {
        let candidates = self.generated.get(function)?;
        if let [only] = candidates.iter().collect::<Vec<_>>()[..] {
            return Some(only);
        }
        let module = normalized(module);
        let named: Vec<&NamedOperation> = candidates
            .iter()
            .filter(|operation| module.contains(&normalized(&operation.service)))
            .collect();
        match named[..] {
            [only] => Some(only),
            _ => None,
        }
    }

    /// The names a call of the operation `method path` may go by: the
    /// names of client methods requesting it and of generated functions
    /// named after it.
    pub fn names_calling(&self, method: &str, path: &str) -> Vec<String> {
        let methods = self
            .methods
            .iter()
            .filter(|client| client.method == method && client.path == path)
            .map(|client| {
                client
                    .symbol
                    .rsplit('.')
                    .next()
                    .unwrap_or(&client.symbol)
                    .to_owned()
            });
        let generated = self
            .generated
            .iter()
            .filter(|(_, operations)| {
                operations
                    .iter()
                    .any(|operation| operation.method == method && operation.path == path)
            })
            .map(|(function, _)| function.clone());
        methods.chain(generated).collect()
    }

    /// One client's methods.
    pub fn methods_of<'a>(&'a self, client: &'a str) -> impl Iterator<Item = &'a ClientMethod> {
        self.methods
            .iter()
            .filter(move |method| method.client == client)
    }

    /// Whether there are client methods to match calls against.
    pub fn is_empty(&self) -> bool {
        self.methods.is_empty()
    }

    /// Whether an import specifier names a client package or one of its
    /// modules: `@acme/client`, `@acme/client/resources`, `acme_client.types`.
    pub fn is_client_module(&self, specifier: &str) -> bool {
        self.packages.iter().any(|package| {
            specifier == package
                || specifier.starts_with(&format!("{package}/"))
                || specifier.starts_with(&format!("{package}."))
        })
    }

    /// Whether a dependency manifest's text names a client package.
    pub fn is_depended_on_in(&self, manifest: &str) -> bool {
        self.packages
            .iter()
            .any(|package| manifest.contains(package.as_str()))
    }

    /// The method a call through a chain reaches, if exactly one request
    /// fits: the method named like the chain's last name, in the resource
    /// matching most of the names before it.
    pub fn resolve(&self, call: &MemberCall) -> Option<&ClientMethod> {
        let (last, before) = call.parts.split_last()?;
        let name = normalized(last);
        let resources: Vec<String> = before
            .iter()
            .skip(1)
            .map(|part| normalized(part))
            .filter(|part| part.len() > 1)
            .collect();
        let immediate = resources.last()?;
        let mut best: Vec<&ClientMethod> = Vec::new();
        let mut best_score = (0, usize::MIN);
        for method in self.methods.iter().filter(|method| method.name() == name) {
            let names = method.resource_names();
            if !names.contains(immediate) {
                continue;
            }
            let matched = resources
                .iter()
                .filter(|part| names.contains(*part))
                .count();
            let unmentioned = names
                .iter()
                .filter(|name| !resources.contains(name))
                .count();
            let score = (matched, usize::MAX - unmentioned);
            if score > best_score {
                best_score = score;
                best.clear();
            }
            if score == best_score {
                best.push(method);
            }
        }
        let requests: BTreeSet<(&str, &str)> = best
            .iter()
            .map(|method| (method.method.as_str(), method.path.as_str()))
            .collect();
        if requests.len() != 1 {
            return None;
        }
        best.into_iter()
            .min_by_key(|method| (method.symbol.len(), method.symbol.clone()))
    }
}

/// The methods with one kept for each file, name and request: a client
/// often defines a method several times over (sync and async, raw and
/// streaming variants), and the shortest name stands for them.
fn one_per_request(mut methods: Vec<ClientMethod>) -> Vec<ClientMethod> {
    methods.sort_by(|left, right| {
        let key = |method: &ClientMethod| {
            (
                method.client.clone(),
                method.file.clone(),
                method.name(),
                method.method.clone(),
                method.path.clone(),
                method.symbol.len(),
                method.symbol.clone(),
            )
        };
        key(left).cmp(&key(right))
    });
    methods.dedup_by(|later, kept| {
        later.client == kept.client
            && later.file == kept.file
            && later.name() == kept.name()
            && later.method == kept.method
            && later.path == kept.path
    });
    methods
}

/// Whether a call's chain starts from something local to its file rather
/// than from a name another module provides.
pub(crate) fn starts_locally(call: &MemberCall, imported_names: &[String]) -> bool {
    call.parts
        .first()
        .is_some_and(|root| imported_names.binary_search(root).is_err())
}

/// Letters and digits only, in lower case, so `auth_configs`,
/// `authConfigs` and `auth-configs` compare equal.
pub(crate) fn normalized(text: &str) -> String {
    text.chars()
        .filter(char::is_ascii_alphanumeric)
        .map(|character| character.to_ascii_lowercase())
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use lemma_http::Location;

    fn method(symbol: &str, method: &str, path: &str, file: &str) -> ClientMethod {
        ClientMethod {
            client: "sdk".to_owned(),
            symbol: symbol.to_owned(),
            method: method.to_owned(),
            path: path.to_owned(),
            file: file.to_owned(),
            line: 1,
        }
    }

    fn call(chain: &str) -> MemberCall {
        MemberCall {
            parts: chain.split('.').map(str::to_owned).collect(),
            location: Location {
                file: "a.ts".to_owned(),
                line: 1,
                symbol: None,
                function: None,
            },
        }
    }

    fn clients() -> Clients {
        Clients::new(
            vec![
                method(
                    "makeAuthConfigs.list",
                    "GET",
                    "/v1/auth_configs",
                    "src/resources/auth-configs.ts",
                ),
                method(
                    "AuthConfigsResource.update_status",
                    "PATCH",
                    "/v1/auth_configs/{}/{}",
                    "python/resources/auth_configs.py",
                ),
                method(
                    "SessionResource.create",
                    "POST",
                    "/v1/tool_router/session",
                    "python/resources/tool_router/session.py",
                ),
                method(
                    "SessionResource.create",
                    "POST",
                    "/v1/auth/session",
                    "python/resources/auth/session.py",
                ),
            ],
            BTreeSet::from(["@acme/client".to_owned(), "acme_client".to_owned()]),
            BTreeMap::new(),
        )
    }

    #[test]
    fn a_chain_reaches_the_method_its_names_spell() {
        let clients = clients();

        let typed = clients.resolve(&call("this.client.authConfigs.list"));
        let python = clients.resolve(&call("self._client.auth_configs.update_status"));

        assert_eq!(
            typed.map(|found| found.path.as_str()),
            Some("/v1/auth_configs")
        );
        assert_eq!(python.map(|found| found.method.as_str()), Some("PATCH"));
    }

    #[test]
    fn the_names_before_a_resource_tell_apart_resources_of_one_name() {
        let clients = clients();

        let found = clients.resolve(&call("client.tool_router.session.create"));
        let unclear = clients.resolve(&call("client.session.create"));

        assert_eq!(
            found.map(|found| found.path.as_str()),
            Some("/v1/tool_router/session")
        );
        assert_eq!(unclear, None);
    }

    #[test]
    fn the_resource_with_fewest_unmentioned_names_wins() {
        let clients = Clients::new(
            vec![
                method(
                    "makeTools.list",
                    "GET",
                    "/v1/tools",
                    "src/resources/tools.ts",
                ),
                method(
                    "makeTools.list",
                    "POST",
                    "/v1/internal/logs",
                    "src/resources/internal/logs/tools.ts",
                ),
            ],
            BTreeSet::new(),
            BTreeMap::new(),
        );

        let found = clients.resolve(&call("this.client.tools.list"));

        assert_eq!(found.map(|found| found.path.as_str()), Some("/v1/tools"));
    }

    #[test]
    fn a_resource_must_be_named_exactly() {
        let clients = clients();

        assert_eq!(clients.resolve(&call("ctx.db.authConfig.list")), None);
    }

    #[test]
    fn client_modules_and_local_roots_are_recognised() {
        let clients = clients();
        let imported = vec!["prisma".to_owned()];

        assert!(clients.is_client_module("@acme/client/resources"));
        assert!(clients.is_client_module("acme_client.types"));
        assert!(!clients.is_client_module("@acme/clientele"));
        assert!(starts_locally(
            &call("this.client.authConfigs.list"),
            &imported
        ));
        assert!(!starts_locally(
            &call("prisma.authConfig.delete"),
            &imported
        ));
    }

    #[test]
    fn a_generated_function_is_the_operation_its_module_names() {
        let operation = |service: &str| NamedOperation {
            service: service.to_owned(),
            method: "GET".to_owned(),
            path: format!("/{service}/tools"),
            document: format!("{service}.json"),
        };
        let clients = Clients::new(
            Vec::new(),
            BTreeSet::new(),
            BTreeMap::from([
                (
                    "listTools".to_owned(),
                    BTreeSet::from([operation("billing"), operation("search")]),
                ),
                (
                    "getHealth".to_owned(),
                    BTreeSet::from([operation("billing")]),
                ),
            ]),
        );

        let named = clients.named_operation("listTools", "../generated/search");
        let only = clients.named_operation("getHealth", "@/gen/api");
        let unclear = clients.named_operation("listTools", "../generated/api");

        assert_eq!(named.map(|found| found.service.as_str()), Some("search"));
        assert_eq!(only.map(|found| found.service.as_str()), Some("billing"));
        assert_eq!(unclear, None);
    }

    #[test]
    fn variants_of_a_method_are_kept_once() {
        let clients = Clients::new(
            vec![
                method(
                    "AsyncToolsResource.list",
                    "GET",
                    "/v1/tools",
                    "resources/tools.py",
                ),
                method(
                    "ToolsResource.list",
                    "GET",
                    "/v1/tools",
                    "resources/tools.py",
                ),
                method(
                    "ToolsResourceWithRawResponse.list",
                    "GET",
                    "/v1/tools",
                    "resources/tools.py",
                ),
            ],
            BTreeSet::new(),
            BTreeMap::new(),
        );

        let kept: Vec<&str> = clients
            .methods_of("sdk")
            .map(|found| found.symbol.as_str())
            .collect();

        assert_eq!(kept, ["ToolsResource.list"]);
    }
}
