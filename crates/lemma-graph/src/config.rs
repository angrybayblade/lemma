//! What a description says about one repository: what it is (`services`)
//! and how it relates to others (`graph`). `.lm` modules lower to these
//! structures; see [`crate::lm`].

use std::collections::BTreeMap;

/// One repository's description.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct LemmaFile {
    /// Services, by name. A name starting with `#` is an external service.
    pub services: BTreeMap<String, Service>,
    /// Relationships.
    pub graph: Graph,
}

/// A service: defined here (operations), or external (`#name`: another
/// repository, pinned).
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Service {
    /// External: a Git repository to clone.
    pub git: Option<String>,
    /// External: a local directory, relative to this file.
    pub path: Option<String>,
    /// External: the commit, branch or tag to read; the working directory
    /// of a `path` source, or the default branch of a `git` one, if absent.
    pub reference: Option<String>,
    /// Defined here: what Lemma reads for it: the code it is made of.
    pub index: IndexSettings,
    /// Defined here: what it offers other code: the entry points queries
    /// start from and links join.
    pub exports: Exports,
    /// Defined here: interpreters and toolchains its schema resolvers use.
    pub runtime: Runtime,
    /// Defined here: whether it rejects request fields it does not know,
    /// even where its documents do not say so.
    pub rejects_unknown_fields: bool,
}

/// The code a service is made of, which Lemma indexes and searches.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct IndexSettings {
    /// Directories (or files) relative to the repository root.
    pub include: Vec<String>,
    /// Glob patterns, relative to the repository root, never read, such as
    /// `**/generated/**`.
    pub exclude: Vec<String>,
    /// Only files in these languages (`typescript`, `tsx`, `python`, `go`,
    /// `rust`); all of them if empty.
    pub languages: Vec<String>,
}

/// What a service offers: its entry points.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Exports {
    /// `OpenAPI` documents it serves. Each operation with an `operationId`
    /// becomes one of its operations, and calls to any of them from code
    /// elsewhere are found and checked (inferred links).
    pub openapi: Vec<String>,
    /// Operations it provides or calls, by name, declared where its
    /// documents do not describe them.
    pub operations: BTreeMap<String, Operation>,
    /// Files whose definitions are its public interface, as a library's
    /// entry modules.
    pub symbols: Vec<String>,
    /// Set when the service is a client of another service, generated from
    /// its documents: its code is the client, so it is not searched for
    /// calls.
    pub client: Option<GeneratedClient>,
    /// The calls its code writes routes with, so handlers are found by
    /// what is declared instead of by reading conventions from the code.
    pub routes: Routes,
    /// An adapter that reads its operations from its code, which then is
    /// its contract wherever Lemma reads it: `openapi` documents are only
    /// compared against it.
    pub operations_from: Option<OperationsFrom>,
}

/// How a service's operations are read from its code: an adapter imports
/// the code in a separate process and writes the `OpenAPI` document the
/// framework describes for it.
#[derive(Debug, Clone, Default, PartialEq, Eq, Hash)]
pub struct OperationsFrom {
    /// A built-in adapter: `hono-zod-openapi` or `fastapi`.
    pub adapter: Option<String>,
    /// Instead of a built-in adapter, a command that writes an `OpenAPI`
    /// document to the file named by `$LEMMA_OUTPUT`.
    pub command: Option<String>,
    /// What the adapter imports, relative to `directory`: the directories
    /// routes are registered in (`hono-zod-openapi`), or a module with its
    /// app (`app.main:app`, `fastapi`). Given to the adapter as
    /// `$LEMMA_ENTRY`, one per line.
    pub entry: Vec<String>,
    /// Where the adapter runs, relative to the repository root, such as the
    /// app's package directory; the root if absent.
    pub directory: Option<String>,
    /// Run in Lemma's worktree of a commit before the adapter, to install
    /// and build what importing the code needs. Only commits no one has
    /// read from a clean working directory are built; the working
    /// directory is taken as ready.
    pub setup: Option<String>,
    /// Environment variables for the adapter and `setup`.
    pub env: BTreeMap<String, String>,
    /// Glob patterns of files under `entry` the adapter does not import,
    /// such as scripts that do work when imported. Given to the adapter as
    /// `$LEMMA_EXCLUDE`, one per line.
    pub exclude: Vec<String>,
    /// How long the adapter may run, in milliseconds, before it is stopped
    /// and its service checked against its committed documents; five
    /// minutes if absent.
    pub timeout_ms: Option<u64>,
}

/// The calls a service writes its routes with, by name (`exports.routes`).
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Routes {
    /// Calls that register a route with its handler, passed after the route:
    /// `Handle` for `r.Handle("/x", NewHandler(db))`, `openapi` for
    /// `router.openapi(DEF, handler)`.
    pub register: Vec<String>,
    /// Calls that describe a route without its handler, whose value a
    /// `register` call passes: `createRoute` for `const DEF =
    /// router.createRoute({ method, path })`.
    pub define: Vec<String>,
}

impl Exports {
    /// Whether it exports nothing.
    pub fn is_empty(&self) -> bool {
        self.openapi.is_empty()
            && self.operations_from.is_none()
            && self.operations.is_empty()
            && self.symbols.is_empty()
            && self.client.is_none()
    }
}

/// A service that is a client of another, generated from its documents.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct GeneratedClient {
    /// Documents it generates one function per operation from, as copied
    /// into this repository; calls to those functions are calls to the
    /// operations.
    pub generated_from: Vec<String>,
    /// Its copy of the service's documents: the parameters it declares are
    /// checked against the service's.
    pub specs: Vec<String>,
    /// How it names an operation's function from its `operationId`:
    /// templates with `{operationId}`, `{camel}`, `{pascal}` or `{snake}`.
    /// The `operationId` as written if empty.
    pub call_names: Vec<String>,
}

impl Service {
    /// Whether it defines anything: code, exports or settings.
    pub(crate) fn defines_anything(&self) -> bool {
        self.index != IndexSettings::default()
            || !self.exports.is_empty()
            || self.runtime != Runtime::default()
            || self.rejects_unknown_fields
    }

    /// Whether its code is searched for calls to other services: unless it
    /// is itself a client, whose code is the client, not a caller of it.
    pub fn searches_calls(&self) -> bool {
        self.exports.client.is_none()
    }
}

/// An operation a service provides or calls.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Operation {
    /// Where it is in code: `file:Symbol`.
    pub path: String,
    /// Where its request and response schemas come from.
    pub schema: Option<Schemas>,
    /// Where it was before a rename, each `<service>.<operation>` in the
    /// same repository, so a change that renames it compares it with its
    /// old self instead of finding it new.
    pub previously: Vec<String>,
}

/// An operation's schemas.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Schemas {
    /// A command whose output the `request` and `response` locations are in.
    pub resolver: Option<Resolver>,
    /// The request's schema.
    pub request: Option<SchemaSource>,
    /// The response's schema.
    pub response: Option<SchemaSource>,
}

/// A command producing a JSON document, run in the repository at the commit
/// being checked.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Resolver {
    /// The command, run with `sh -c`. `{python}`, `{node}`, `{go}` and
    /// `{cargo}` are replaced by the service's runtime.
    pub command: String,
    /// The file it writes, relative to the repository root; its standard
    /// output if absent.
    pub output: Option<String>,
    /// Extra environment variables.
    pub env: BTreeMap<String, String>,
    /// Whether its output may be kept for the commit it was produced at.
    /// Lemma keeps it by the code's Git tree, the command, its environment
    /// and, when it runs Python, the interpreter and its installed packages;
    /// set `false` for a command whose output depends on anything else (a
    /// running service, the network, ignored generated files).
    pub cache: bool,
}

/// Where a schema is.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SchemaSource {
    /// A type in code: `file#Type`.
    Code(String),
    /// A location in a JSON document: the resolver's output, or `file`.
    Document(DocumentLocation),
}

/// A schema inside a JSON document.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DocumentLocation {
    /// `json-schema` or `openapi`.
    pub format: String,
    /// A JSON path into the document, such as `$.create_charge.request`.
    pub path: String,
    /// The document, relative to the repository root, when there is no
    /// resolver.
    pub file: Option<String>,
}

/// Interpreters and toolchains. Defaults: `python3`, `node`, `go`, `cargo`.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Runtime {
    /// A Python interpreter, such as `.venv/bin/python`.
    pub python: Option<String>,
    /// A Node.js executable.
    pub node: Option<String>,
    /// A Bun executable, for TypeScript run directly.
    pub bun: Option<String>,
    /// The Go toolchain.
    pub go: Option<String>,
    /// The Rust toolchain.
    pub cargo: Option<String>,
    /// Environment variables for every command run for this service, such
    /// as `PYTHONPATH: src` for a repository whose imports start below its
    /// root. A resolver's own `env` wins over these.
    pub env: BTreeMap<String, String>,
}

/// Relationships, and how strictly they are checked.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Graph {
    /// Links between operations: `caller -> provider`.
    pub links: Vec<String>,
    /// `audit` reports differences as warnings; `check` fails on them.
    pub level: Option<Level>,
}

/// How strictly links are checked.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, serde::Serialize)]
#[serde(rename_all = "lowercase")]
pub enum Level {
    /// Report differences as warnings.
    #[default]
    Audit,
    /// Fail on differences, and on behaviors violated.
    Check,
    /// Also fail on error-severity behaviors whose outcome is unknown.
    Strict,
}
