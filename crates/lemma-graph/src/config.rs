//! `lemma.yaml`: what a repository is (`services`) and how it relates to
//! others (`graph`).
//!
//! ```yaml
//! services:
//!   web:
//!     operations:
//!       create_charge:
//!         path: client/charges.go:Charges.Create
//!         schema:
//!           request: client/charges.go#CreateChargeRequest
//!   "#billing":                     # a leading # names an external service
//!     path: ../billing              # or git: <url>, with ref: <commit or branch>
//! graph:
//!   links:
//!     - services.web.create_charge -> "#billing:services.api.create_charge"
//!   level: audit
//! ```

use std::collections::BTreeMap;

use serde::Deserialize;

/// The file every repository's description is read from, unless a service
/// names another (`manifest`).
pub const LEMMA_FILE: &str = "lemma.yaml";

/// One `lemma.yaml`.
#[derive(Debug, Clone, Default, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct LemmaFile {
    /// Services, by name. A name starting with `#` is an external service.
    #[serde(default)]
    pub services: BTreeMap<String, Service>,
    /// Relationships.
    #[serde(default)]
    pub graph: Graph,
}

/// A service: defined here (operations), or external (`#name`: another
/// repository, pinned).
#[derive(Debug, Clone, Default, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct Service {
    /// External: a Git repository to clone.
    #[serde(default)]
    pub git: Option<String>,
    /// External: a local directory, relative to this file.
    #[serde(default)]
    pub path: Option<String>,
    /// External: the commit, branch or tag to read; the working directory
    /// of a `path` source, or the default branch of a `git` one, if absent.
    #[serde(default, rename = "ref")]
    pub reference: Option<String>,
    /// External: where its `lemma.yaml` is, relative to its root. Alone, it
    /// names another `lemma.yaml` in this repository, as in a monorepo.
    #[serde(default)]
    pub manifest: Option<String>,
    /// Defined here: its operations, by name.
    #[serde(default)]
    pub operations: BTreeMap<String, Operation>,
    /// Defined here: interpreters and toolchains its schema resolvers use.
    #[serde(default)]
    pub runtime: Runtime,
    /// Defined here: `OpenAPI` documents it serves. Each operation with an
    /// `operationId` becomes an operation of this service, and calls to any
    /// of them from code elsewhere are found and checked (inferred links).
    #[serde(default)]
    pub openapi: Vec<String>,
    /// Defined here: directories with its route code.
    #[serde(default)]
    pub routes: Vec<String>,
    /// Defined here: whether it rejects request fields it does not know,
    /// even where its documents do not say so.
    #[serde(default)]
    pub rejects_unknown_fields: bool,
    /// Defined here: path prefixes it owns, such as `/v1/billing/`; a new call
    /// under one to a path it does not serve is reported. The longest prefix
    /// its operations share if empty.
    #[serde(default)]
    pub path_prefixes: Vec<String>,
    /// Defined here: prefixes its routes are also served under, such as
    /// `/v1/` routes also published as `/v2/`.
    #[serde(default)]
    pub route_aliases: BTreeMap<String, String>,
    /// Defined here: further directories with schemas its routes import,
    /// read to tell which reject unknown fields.
    #[serde(default)]
    pub shared_schemas: Vec<String>,
    /// Defined here: directories whose code is searched for calls to other
    /// services' operations. If empty, the whole repository for a service
    /// that is alone in it and declares no documents, client or operations;
    /// nothing otherwise.
    #[serde(default)]
    pub code: Vec<String>,
    /// Defined here: names its code calls generated clients through, such
    /// as `client`.
    #[serde(default)]
    pub client_receivers: Vec<String>,
    /// Defined here: modules of generated clients its code imports, such as
    /// `generated/billing`.
    #[serde(default)]
    pub client_modules: Vec<String>,
    /// Defined here: a client of another service, generated from its
    /// documents.
    #[serde(default)]
    pub client: Option<GeneratedClient>,
}

/// A client generated from `OpenAPI` documents.
#[derive(Debug, Clone, Default, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct GeneratedClient {
    /// Documents it generates one function per operation from, as copied
    /// into this repository; calls to those functions are calls to the
    /// operations.
    #[serde(default)]
    pub generated_from: Vec<String>,
    /// Its copy of the service's documents: the parameters it declares are
    /// checked against the service's.
    #[serde(default)]
    pub specs: Vec<String>,
    /// How it names an operation's function from its `operationId`:
    /// templates with `{operationId}`, `{camel}`, `{pascal}` or `{snake}`.
    /// The `operationId` as written if empty.
    #[serde(default)]
    pub call_names: Vec<String>,
}

impl Service {
    /// Whether it defines anything: operations, documents, code or a client.
    fn defines_anything(&self) -> bool {
        let lists = [
            &self.openapi,
            &self.routes,
            &self.path_prefixes,
            &self.shared_schemas,
            &self.code,
            &self.client_receivers,
            &self.client_modules,
        ];
        !self.operations.is_empty()
            || self.runtime != Runtime::default()
            || lists.iter().any(|list| !list.is_empty())
            || self.rejects_unknown_fields
            || !self.route_aliases.is_empty()
            || self.client.is_some()
    }
}

/// A service whose fields contradict its name: an external service (`#name`)
/// is a pointer to a `lemma.yaml`, a local one defines operations.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
#[error("service `{service}`: {reason}")]
pub struct InvalidService {
    /// The service's name.
    pub service: String,
    /// What is wrong.
    pub reason: &'static str,
}

impl LemmaFile {
    /// Checks every service against the naming rule: `#name` services say
    /// where another `lemma.yaml` is (`git`, `path`, or only `manifest` for
    /// one elsewhere in the same repository) and define nothing; other
    /// services define operations and point nowhere.
    pub fn validate(&self) -> Result<(), InvalidService> {
        for (name, service) in &self.services {
            if let Some(reason) = service_problem(name, service) {
                return Err(InvalidService {
                    service: name.clone(),
                    reason,
                });
            }
        }
        Ok(())
    }
}

fn service_problem(name: &str, service: &Service) -> Option<&'static str> {
    let points_elsewhere = service.git.is_some()
        || service.path.is_some()
        || service.reference.is_some()
        || service.manifest.is_some();
    let defines = service.defines_anything();
    if !name.starts_with('#') {
        return points_elsewhere.then_some(
            "`git`, `path`, `ref` and `manifest` belong to external services; name it `#name`",
        );
    }
    if defines {
        return Some(
            "an external service defines nothing here; declare its operations in its own lemma.yaml",
        );
    }
    if service.git.is_none() && service.path.is_none() && service.manifest.is_none() {
        return Some("an external service needs `git`, `path` or `manifest`");
    }
    if service.git.is_some() && service.path.is_some() {
        return Some("give `git` or `path`, not both");
    }
    None
}

/// An operation a service provides or calls.
#[derive(Debug, Clone, Default, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct Operation {
    /// Where it is in code: `file:Symbol`.
    pub path: String,
    /// Where its request and response schemas come from.
    #[serde(default)]
    pub schema: Option<Schemas>,
}

/// An operation's schemas.
#[derive(Debug, Clone, Default, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct Schemas {
    /// A command whose output the `request` and `response` locations are in.
    #[serde(default)]
    pub resolver: Option<Resolver>,
    /// The request's schema.
    #[serde(default)]
    pub request: Option<SchemaSource>,
    /// The response's schema.
    #[serde(default)]
    pub response: Option<SchemaSource>,
}

/// A command producing a JSON document, run in the repository at the commit
/// being checked.
#[derive(Debug, Clone, Default, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct Resolver {
    /// The command, run with `sh -c`. `{python}`, `{node}`, `{go}` and
    /// `{cargo}` are replaced by the service's runtime.
    pub command: String,
    /// The file it writes, relative to the repository root; its standard
    /// output if absent.
    #[serde(default)]
    pub output: Option<String>,
    /// Extra environment variables.
    #[serde(default)]
    pub env: BTreeMap<String, String>,
}

/// Where a schema is.
#[derive(Debug, Clone, Deserialize, PartialEq, Eq)]
#[serde(untagged)]
pub enum SchemaSource {
    /// A type in code: `file#Type`.
    Code(String),
    /// A location in a JSON document: the resolver's output, or `file`.
    Document(DocumentLocation),
}

/// A schema inside a JSON document.
#[derive(Debug, Clone, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct DocumentLocation {
    /// `json-schema` or `openapi`.
    #[serde(default = "json_schema")]
    pub format: String,
    /// A JSON path into the document, such as `$.create_charge.request`.
    pub path: String,
    /// The document, relative to the repository root, when there is no
    /// resolver.
    #[serde(default)]
    pub file: Option<String>,
}

fn json_schema() -> String {
    "json-schema".to_owned()
}

/// Interpreters and toolchains. Defaults: `python3`, `node`, `go`, `cargo`.
#[derive(Debug, Clone, Default, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct Runtime {
    /// A Python interpreter, such as `.venv/bin/python`.
    #[serde(default)]
    pub python: Option<String>,
    /// A Node.js executable.
    #[serde(default)]
    pub node: Option<String>,
    /// The Go toolchain.
    #[serde(default)]
    pub go: Option<String>,
    /// The Rust toolchain.
    #[serde(default)]
    pub cargo: Option<String>,
    /// Environment variables for every command run for this service, such
    /// as `PYTHONPATH: src` for a repository whose imports start below its
    /// root. A resolver's own `env` wins over these.
    #[serde(default)]
    pub env: BTreeMap<String, String>,
}

/// Relationships, and how strictly they are checked.
#[derive(Debug, Clone, Default, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct Graph {
    /// Links between operations: `caller -> provider`.
    #[serde(default)]
    pub links: Vec<String>,
    /// `audit` reports differences as warnings; `check` fails on them.
    #[serde(default)]
    pub level: Option<Level>,
}

/// How strictly links are checked.
#[derive(Debug, Clone, Copy, Default, Deserialize, PartialEq, Eq, serde::Serialize)]
#[serde(rename_all = "lowercase")]
pub enum Level {
    /// Report differences as warnings.
    #[default]
    Audit,
    /// Fail on differences.
    Check,
}

/// Reads a `lemma.yaml`.
pub fn parse(text: &str) -> Result<LemmaFile, yaml_serde::Error> {
    yaml_serde::from_str(text)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn services_operations_externals_and_links_are_read() {
        let text = r##"
services:
  web:
    runtime: { go: go }
    operations:
      create_charge:
        path: client/charges.go:Charges.Create
        schema:
          request: client/charges.go#CreateChargeRequest
  "#billing":
    path: ../billing
    ref: HEAD
graph:
  links:
    - services.web.create_charge -> "#billing:services.api.create_charge"
  level: check
"##;

        let file = parse(text).unwrap();

        let web = &file.services["web"];
        let schema = web.operations["create_charge"].schema.as_ref().unwrap();
        assert_eq!(
            schema.request,
            Some(SchemaSource::Code(
                "client/charges.go#CreateChargeRequest".to_owned()
            ))
        );
        assert_eq!(
            file.services["#billing"].path.as_deref(),
            Some("../billing")
        );
        assert_eq!(file.graph.level, Some(Level::Check));
        assert_eq!(file.graph.links.len(), 1);
    }

    #[test]
    fn services_must_match_their_names() {
        let local_with_source = parse("services:\n  api: { path: ../api }\n").unwrap();
        let external_without_source = parse("services:\n  \"#api\": {}\n").unwrap();
        let external_with_operations = parse(
            "services:\n  \"#api\":\n    path: ../api\n    operations: { o: { path: a.py } }\n",
        )
        .unwrap();
        let monorepo = parse("services:\n  \"#api\": { manifest: api/lemma.yaml }\n").unwrap();

        assert!(local_with_source.validate().is_err());
        assert!(external_without_source.validate().is_err());
        assert!(external_with_operations.validate().is_err());
        assert!(monorepo.validate().is_ok());
    }

    #[test]
    fn document_locations_default_to_json_schema() {
        let text = "services:\n  s:\n    operations:\n      o:\n        path: a.py:o\n        schema:\n          resolver: { command: make spec, output: spec.json }\n          response: { path: \"$.o.response\" }\n";

        let file = parse(text).unwrap();

        let schema = file.services["s"].operations["o"].schema.clone().unwrap();
        assert_eq!(
            schema.resolver.unwrap().output.as_deref(),
            Some("spec.json")
        );
        let Some(SchemaSource::Document(location)) = schema.response else {
            panic!("expected a document location");
        };
        assert_eq!(location.format, "json-schema");
    }
}
