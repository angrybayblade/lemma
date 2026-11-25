//! Calls through a client library, found without being declared. A client's
//! own code says what each of its methods requests: `makeApiKeys.list`
//! issues `GET /v1/api_keys`, `ApiKeysResource.update_status`
//! issues `PATCH /v1/api_keys/{}/{}`. A consumer's call through a chain
//! (`this.client.apiKeys.list()`, `self._client.api_keys.list()`)
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
//! a database client's `prisma.apiKey.delete()`.
//!
//! A short call names no resource to match by: `addPet()`,
//! `PetService.add()`, `pets.add()` where `pets` is the module. It reaches
//! a client's method where its root is a name imported from that client,
//! from its package or from one of its files in the same repository, and
//! exactly one request fits the names (see `Clients::imported`). Where the
//! client's code does not say what a function requests, its description
//! may: a function it names after an operation is reached the same way
//! (see `Clients::declared`).
//!
//! In Go a client's method is called on a value, which names neither a
//! resource nor an import (`api.GetTaskWithResponse(...)`). There a call
//! reaches the one method of that name among the clients whose package
//! the file imports (see `Clients::exported`). The name is all that ties
//! them, so a method of the same name on another type would be taken for
//! the client's.
//!
//! TODO: tie a Go call to the client by what the receiver is, once
//! references are resolved, rather than by its name.
//!
//! A client generated at build time has no code to read: its module is
//! imported but no file holds it. Its functions are named after the
//! operations' `operationId`s (`listTools`, `list_tools`), so a call of a
//! function imported from such a module, directly or through a module that
//! re-exports it whole, is a call of the operation it is named after.

use std::collections::{BTreeMap, BTreeSet};
use std::path::{Path, PathBuf};

use lemma_http::MemberCall;

/// A client method that issues a request.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord)]
pub(crate) struct ClientMethod {
    /// The client it belongs to.
    pub client: String,
    /// Its enclosing names and method, as the client's code writes them:
    /// `makeApiKeys.list`.
    pub symbol: String,
    /// Upper-case HTTP method.
    pub method: String,
    /// Canonical path.
    pub path: String,
    /// The file it is in.
    pub file: String,
    /// Its line.
    pub line: u32,
    /// The names its body uses as fields: what it still sends, whatever the
    /// client's copy of the spec says.
    pub names: BTreeSet<String>,
    /// The method of its file that issues the request for it, where it only
    /// calls that one (`Client.GetTask` calling `NewGetTaskRequest`);
    /// `None` where it issues the request itself.
    pub holder: Option<String>,
}

impl ClientMethod {
    /// How facts name it: `file:Symbol`, since one symbol (`ToolsResource.list`)
    /// can be defined in several files. A method that only calls the one
    /// issuing its request is named as that one, so the request is a fact
    /// once however many methods lead to it.
    pub fn key(&self) -> String {
        let symbol = self.holder.as_ref().unwrap_or(&self.symbol);
        format!("{}:{symbol}", self.file)
    }

    /// Its name, normalized: `updatestatus` for `update_status`.
    fn name(&self) -> String {
        normalized(self.symbol.rsplit('.').next().unwrap_or(&self.symbol))
    }

    /// The names its resource goes by, normalized: the directories and
    /// stem of its file, and its enclosing names.
    pub(crate) fn resource_names(&self) -> BTreeSet<String> {
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

/// Where one client's code is: what a consumer's import must name for a
/// short call to be a call through this client.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct ClientCode {
    /// The client.
    pub client: String,
    /// The repository holding its code.
    pub repository: PathBuf,
    /// Its code files, by their paths in that repository.
    pub files: BTreeSet<String>,
    /// The package names it is published as.
    pub packages: BTreeSet<String>,
    /// The functions its description names after operations, each with the
    /// requests (method, path) it may make.
    pub functions: BTreeMap<String, BTreeSet<(String, String)>>,
}

/// What an imported name is of a client: the module itself, or a name the
/// module exports.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Imported<'a> {
    /// A namespace or default import: `import * as pets from './pets'`.
    Module,
    /// A named import: `PetService` in `import { PetService } from './sdk'`.
    Name(&'a str),
}

/// Which of a client's methods an import can reach.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Reach<'a> {
    /// Every method: the import names the client's package.
    Package,
    /// The methods in the imported file or beside and below it: an index
    /// file hands on what the files of its directory export.
    File(&'a str),
}

/// What the system's clients offer: their methods, the package names a
/// consumer imports or depends on them by, where their code is, and the
/// names a generated client may give each operation.
#[derive(Debug, Clone, Default)]
pub(crate) struct Clients {
    methods: Vec<ClientMethod>,
    packages: BTreeSet<String>,
    code: Vec<ClientCode>,
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
            code: Vec::new(),
            generated,
        }
    }

    /// With where each client's code is, so that short calls rooted at an
    /// import of it can be told from calls of anything else.
    pub fn with_code(mut self, code: Vec<ClientCode>) -> Self {
        self.code = code;
        self
    }

    /// Whether any client's code is known, so that an import may name one.
    pub fn has_code(&self) -> bool {
        !self.code.is_empty()
    }

    /// The client whose package an import specifier names, or one of its
    /// modules.
    pub fn client_of_module(&self, specifier: &str) -> Option<&str> {
        self.code
            .iter()
            .find(|code| {
                code.packages
                    .iter()
                    .any(|package| names_package(specifier, package))
            })
            .map(|code| code.client.as_str())
    }

    /// The client one of whose code files is `file` of `repository`.
    pub fn client_of_file(&self, repository: &Path, file: &str) -> Option<&str> {
        self.code
            .iter()
            .find(|code| code.repository == repository && code.files.contains(file))
            .map(|code| code.client.as_str())
    }

    /// The method of `client` a short call reaches: `name` called on what
    /// was `imported`, among the methods the import can `reach`, if exactly
    /// one request fits.
    ///
    /// A named import called itself (`addPet()`) is a function the module
    /// exports, so a method nested in a class does not fit. A name called on
    /// a named import (`PetService.add()`, `get_pet.sync()`) is a method of
    /// the resource so named. A name called on the module is any method of
    /// that name.
    ///
    /// Change-driven evaluation finds the files that may make such a call
    /// by the called name (`terms.rs`): a call this resolves must be of at
    /// most two names, rooted at an import, with the method's name last.
    pub fn imported(
        &self,
        client: &str,
        reach: Reach<'_>,
        imported: Imported<'_>,
        parts: &[String],
    ) -> Option<&ClientMethod> {
        let (resource, name) = match (imported, parts) {
            (Imported::Name(exported), [_]) => (None, normalized(exported)),
            (Imported::Name(exported), [_, name]) => (Some(normalized(exported)), normalized(name)),
            (Imported::Module, [_, name]) => (None, normalized(name)),
            _ => return None,
        };
        let is_called_itself = parts.len() == 1;
        let fitting: Vec<&ClientMethod> = self
            .methods
            .iter()
            .filter(|method| method.client == client)
            .filter(|method| method.name() == name && reach.holds(&method.file))
            .filter(|method| !is_called_itself || !method.symbol.contains('.'))
            .filter(|method| {
                resource
                    .as_ref()
                    .is_none_or(|resource| method.resource_names().contains(resource))
            })
            .collect();
        let requests: BTreeSet<(&str, &str)> = fitting
            .iter()
            .map(|method| (method.method.as_str(), method.path.as_str()))
            .collect();
        if requests.len() != 1 {
            return None;
        }
        fitting
            .into_iter()
            .min_by_key(|method| (method.symbol.len(), method.symbol.clone()))
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
    /// The terms a call of the request `method` `path` is indexed by: for
    /// each client method making it, its name under each of its resources
    /// (`pair:`) and alone (`name:`), and for each generated function and
    /// each function a client's description names, its name (`name:`).
    pub(crate) fn terms_calling(&self, method: &str, path: &str) -> Vec<String> {
        let mut terms = Vec::new();
        for client in self
            .methods
            .iter()
            .filter(|client| client.method == method && client.path == path)
        {
            let name = client.symbol.rsplit('.').next().unwrap_or(&client.symbol);
            for resource in client.resource_names() {
                terms.push(crate::terms::pair_term(&resource, name));
            }
            // A short call rooted at an import names no resource.
            terms.push(crate::terms::name_term(name));
        }
        for (function, operations) in &self.generated {
            let calls = operations
                .iter()
                .any(|operation| operation.method == method && operation.path == path);
            if calls {
                terms.push(crate::terms::name_term(function));
            }
        }
        let declared = self.code.iter().flat_map(|code| &code.functions);
        for (function, requests) in declared {
            let calls = requests
                .iter()
                .any(|(made, at)| made == method && at == path);
            if calls {
                terms.push(crate::terms::name_term(function));
            }
        }
        terms
    }

    /// The method of `client` called `name` that a package exports, if
    /// exactly one request fits: what a Go call on some value reaches in a
    /// file importing the client's package.
    ///
    /// Change-driven evaluation finds the files that may make such a call
    /// by the called name (`terms.rs`), which a call of two names or a
    /// capitalised last name carries.
    pub fn exported(&self, client: &str, name: &str) -> Option<&ClientMethod> {
        if !name.starts_with(char::is_uppercase) {
            return None;
        }
        let name = normalized(name);
        let fitting: Vec<&ClientMethod> = self
            .methods
            .iter()
            .filter(|method| method.client == client && method.name() == name)
            .collect();
        let requests: BTreeSet<(&str, &str)> = fitting
            .iter()
            .map(|method| (method.method.as_str(), method.path.as_str()))
            .collect();
        if requests.len() != 1 {
            return None;
        }
        fitting
            .into_iter()
            .min_by_key(|method| (method.symbol.len(), method.symbol.clone()))
    }

    /// The function of `client` a short call names, among those its
    /// description names after operations: a named import called itself
    /// (`addPet()`), or a name called on the module (`pets.addPet()`).
    ///
    /// Change-driven evaluation finds the files that may make such a call
    /// by the called name (`terms.rs`), as for `imported`.
    pub fn declared<'p>(
        &self,
        client: &str,
        imported: Imported<'p>,
        parts: &'p [String],
    ) -> Option<&'p str> {
        let function = match (imported, parts) {
            (Imported::Name(exported), [_]) => exported,
            (Imported::Module, [_, name]) => name.as_str(),
            _ => return None,
        };
        self.code
            .iter()
            .any(|code| code.client == client && code.functions.contains_key(function))
            .then_some(function)
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
        self.packages
            .iter()
            .any(|package| names_package(specifier, package))
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
    ///
    /// Change-driven evaluation finds the files that may make such a call by
    /// the call's resource and last name (`terms.rs`): a call this resolves
    /// must keep a last name equal to the method's, in a chain of three
    /// names or more, after the resource `immediate_resource` reads.
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

impl Reach<'_> {
    /// Whether a method in `file` is within reach.
    fn holds(self, file: &str) -> bool {
        let Reach::File(imported) = self else {
            return true;
        };
        if file == imported {
            return true;
        }
        match imported.rsplit_once('/') {
            Some((directory, _)) => file.starts_with(&format!("{directory}/")),
            None => true,
        }
    }
}

/// Whether an import specifier names `package` or one of its modules:
/// `@acme/client`, `@acme/client/resources`, `acme_client.types`.
fn names_package(specifier: &str, package: &str) -> bool {
    specifier == package
        || specifier.starts_with(&format!("{package}/"))
        || specifier.starts_with(&format!("{package}."))
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

/// Letters and digits only, in lower case, so `api_keys`,
/// `apiKeys` and `api-keys` compare equal.
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
            names: BTreeSet::new(),
            holder: None,
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
                    "makeApiKeys.list",
                    "GET",
                    "/v1/api_keys",
                    "src/resources/api-keys.ts",
                ),
                method(
                    "ApiKeysResource.update_status",
                    "PATCH",
                    "/v1/api_keys/{}/{}",
                    "python/resources/api_keys.py",
                ),
                method(
                    "SessionResource.create",
                    "POST",
                    "/v1/task_router/session",
                    "python/resources/task_router/session.py",
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
    fn every_chain_that_resolves_carries_a_term_its_operation_is_looked_up_by() {
        let clients = clients();
        let chains = [
            "this.client.apiKeys.list",
            "self._client.api_keys.update_status",
            "client.task_router.session.create",
            "sdk.v1.auth.session.create",
        ];

        for chain in chains {
            let called = call(chain);
            let Some(method) = clients.resolve(&called) else {
                continue;
            };
            let resource = crate::terms::immediate_resource(&called.parts).unwrap();
            let file_term = crate::terms::pair_term(resource, called.parts.last().unwrap());
            let looked_up = clients.terms_calling(&method.method, &method.path);

            assert!(
                looked_up.contains(&file_term),
                "{chain}: {file_term} not in {looked_up:?}"
            );
        }
    }

    #[test]
    fn a_chain_reaches_the_method_its_names_spell() {
        let clients = clients();

        let typed = clients.resolve(&call("this.client.apiKeys.list"));
        let python = clients.resolve(&call("self._client.api_keys.update_status"));

        assert_eq!(typed.map(|found| found.path.as_str()), Some("/v1/api_keys"));
        assert_eq!(python.map(|found| found.method.as_str()), Some("PATCH"));
    }

    #[test]
    fn the_names_before_a_resource_tell_apart_resources_of_one_name() {
        let clients = clients();

        let found = clients.resolve(&call("client.task_router.session.create"));
        let unclear = clients.resolve(&call("client.session.create"));

        assert_eq!(
            found.map(|found| found.path.as_str()),
            Some("/v1/task_router/session")
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

        assert_eq!(clients.resolve(&call("ctx.db.apiKey.list")), None);
    }

    #[test]
    fn client_modules_and_local_roots_are_recognised() {
        let clients = clients();
        let imported = vec!["prisma".to_owned()];

        assert!(clients.is_client_module("@acme/client/resources"));
        assert!(clients.is_client_module("acme_client.types"));
        assert!(!clients.is_client_module("@acme/clientele"));
        assert!(starts_locally(&call("this.client.apiKeys.list"), &imported));
        assert!(!starts_locally(&call("prisma.apiKey.delete"), &imported));
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

    fn names(chain: &str) -> Vec<String> {
        chain.split('.').map(str::to_owned).collect()
    }

    fn only_methods(methods: Vec<ClientMethod>) -> Clients {
        Clients::new(methods, BTreeSet::new(), BTreeMap::new())
    }

    fn code() -> ClientCode {
        ClientCode {
            client: "sdk".to_owned(),
            repository: PathBuf::from("/work/client"),
            files: BTreeSet::from(["src/sdk.gen.ts".to_owned()]),
            packages: BTreeSet::from(["@acme/client".to_owned()]),
            functions: BTreeMap::from([(
                "useListRepos".to_owned(),
                BTreeSet::from([("GET".to_owned(), "/v1/repos".to_owned())]),
            )]),
        }
    }

    #[test]
    fn a_function_imported_by_name_reaches_the_function_not_a_method_of_its_name() {
        let clients = only_methods(vec![
            method("addPet", "POST", "/v1/pets", "src/sdk.gen.ts"),
            method("PetService.addPet", "PUT", "/v1/pets", "src/sdk.gen.ts"),
        ]);

        let found = clients.imported(
            "sdk",
            Reach::Package,
            Imported::Name("addPet"),
            &names("addPet"),
        );

        assert_eq!(found.map(|found| found.method.as_str()), Some("POST"));
    }

    #[test]
    fn a_name_called_on_an_imported_class_reaches_that_classes_method() {
        let clients = only_methods(vec![
            method("ItemsService.list", "GET", "/v1/items", "src/sdk.gen.ts"),
            method("UsersService.list", "GET", "/v1/users", "src/sdk.gen.ts"),
        ]);

        let found = clients.imported(
            "sdk",
            Reach::Package,
            Imported::Name("UsersService"),
            &names("UsersService.list"),
        );

        assert_eq!(found.map(|found| found.path.as_str()), Some("/v1/users"));
    }

    #[test]
    fn a_short_call_two_requests_fit_reaches_none() {
        let clients = only_methods(vec![
            method("ItemsService.list", "GET", "/v1/items", "src/sdk.gen.ts"),
            method("UsersService.list", "GET", "/v1/users", "src/sdk.gen.ts"),
        ]);

        let found = clients.imported("sdk", Reach::Package, Imported::Module, &names("api.list"));

        assert_eq!(found, None);
    }

    #[test]
    fn an_imported_file_reaches_the_methods_beside_and_below_it() {
        let clients = only_methods(vec![
            method("listRepos", "GET", "/v1/repos", "src/services/code/gen.ts"),
            method("listRepos", "GET", "/v1/spaces", "src/services/cde/gen.ts"),
        ]);
        let called = names("listRepos");
        let imported = Imported::Name("listRepos");

        let beside = clients.imported(
            "sdk",
            Reach::File("src/services/code/index.ts"),
            imported,
            &called,
        );
        let anywhere = clients.imported("sdk", Reach::Package, imported, &called);

        assert_eq!(beside.map(|found| found.path.as_str()), Some("/v1/repos"));
        assert_eq!(anywhere, None);
    }

    #[test]
    fn a_function_the_description_names_is_reached_by_its_name_as_written() {
        let clients = only_methods(Vec::new()).with_code(vec![code()]);
        let called_itself = names("useListRepos");
        let called_on_module = names("api.useListRepos");
        let not_described = names("listRepos");

        let hook = clients.declared("sdk", Imported::Name("useListRepos"), &called_itself);
        let on_module = clients.declared("sdk", Imported::Module, &called_on_module);
        let other = clients.declared("sdk", Imported::Name("listRepos"), &not_described);

        assert_eq!(hook, Some("useListRepos"));
        assert_eq!(on_module, Some("useListRepos"));
        assert_eq!(other, None);
    }

    #[test]
    fn a_short_call_that_resolves_carries_a_term_its_operation_is_looked_up_by() {
        let clients = only_methods(vec![method(
            "PetService.addPet",
            "POST",
            "/v1/pets",
            "src/sdk.gen.ts",
        )])
        .with_code(vec![code()]);

        let read = clients.terms_calling("POST", "/v1/pets");
        let described = clients.terms_calling("GET", "/v1/repos");

        assert!(
            read.contains(&crate::terms::name_term("addPet")),
            "{read:?}"
        );
        assert!(
            described.contains(&crate::terms::name_term("useListRepos")),
            "{described:?}"
        );
    }

    #[test]
    fn an_exported_method_is_reached_by_its_name_when_one_request_fits() {
        let clients = only_methods(vec![
            method(
                "ClientWithResponses.GetTaskWithResponse",
                "GET",
                "/v1/tasks/{}",
                "pkg/rest/gen.go",
            ),
            method("Client.List", "GET", "/v1/tasks", "pkg/rest/gen.go"),
            method("Admin.List", "GET", "/v1/users", "pkg/rest/gen.go"),
            method("Client.send", "POST", "/v1/tasks", "pkg/rest/gen.go"),
        ]);

        let found = clients.exported("sdk", "GetTaskWithResponse");

        assert_eq!(found.map(|found| found.path.as_str()), Some("/v1/tasks/{}"));
        assert_eq!(clients.exported("sdk", "List"), None);
        assert_eq!(clients.exported("sdk", "send"), None);
        assert_eq!(clients.exported("other", "GetTaskWithResponse"), None);
    }

    #[test]
    fn a_client_is_told_by_its_package_or_by_a_file_of_its_repository() {
        let clients = only_methods(Vec::new()).with_code(vec![code()]);
        let repository = Path::new("/work/client");

        assert_eq!(clients.client_of_module("@acme/client/sdk"), Some("sdk"));
        assert_eq!(clients.client_of_module("@acme/clientele"), None);
        assert_eq!(
            clients.client_of_file(repository, "src/sdk.gen.ts"),
            Some("sdk")
        );
        assert_eq!(clients.client_of_file(repository, "src/app.ts"), None);
        assert_eq!(
            clients.client_of_file(Path::new("/work/web"), "src/sdk.gen.ts"),
            None
        );
    }
}
