//! A verification manifest: which repositories make up the system, at which
//! commits, what role each plays, and which change is being verified.
//!
//! ```yaml
//! change: { repository: billing, base: main, head: HEAD }
//! repositories:
//!   billing:
//!     path: ../billing
//!     role: service
//!     specs: [openapi.json]
//!     routes: [src/routes]
//!   billing-client:
//!     path: ../billing-client
//!     commit: main
//!     role: client
//!     generated_from: [openapi.json]
//!     call_names: ["{camel}"]
//!   web:
//!     path: ../web
//!     commit: main
//!     role: consumer
//!     include: [src]
//!     client_receivers: [billing]
//! ```

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use serde::Deserialize;

use crate::VerifyError;

/// What a repository is to the system.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Role {
    /// Serves operations, described by specs and route code.
    Service,
    /// A generated client: functions generated from a service's spec.
    Client,
    /// Calls operations, directly or through a client.
    Consumer,
}

/// One repository.
#[derive(Debug, Clone, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct Repository {
    /// Its clone, relative to the manifest.
    pub path: PathBuf,
    /// The commit to read, unless it is the changed repository. Without
    /// one, the working directory is read.
    #[serde(default)]
    pub commit: Option<String>,
    /// What it is to the system.
    pub role: Role,
    /// A service's name, used in findings; the repository's name if absent.
    /// On a consumer, the service its code belongs to: its references to
    /// that service's own operations, such as route declarations, are not
    /// calls.
    #[serde(default)]
    pub service: Option<String>,
    /// `OpenAPI` documents: a service's contract, or a client's copy of it.
    #[serde(default)]
    pub specs: Vec<String>,
    /// Documents built from a service's code by an adapter, replacing
    /// `specs` where a snapshot is read: by `base`, `head`, or `both` for a
    /// repository read once. Set by Lemma, never by a manifest.
    #[serde(skip)]
    pub generated: BTreeMap<String, Vec<GeneratedDocument>>,
    /// Whether a service rejects requests with fields it does not know, as
    /// a strict schema does, even where its spec does not say so.
    #[serde(default)]
    pub rejects_unknown_fields: bool,
    /// Directories with a service's route code.
    #[serde(default)]
    pub routes: Vec<String>,
    /// Path prefixes a service owns, such as `/v1/billing/`: a new call to an
    /// unserved path under one is reported. The longest prefix all its
    /// operations share if empty.
    #[serde(default)]
    pub path_prefixes: Vec<String>,
    /// Path prefixes a service also serves its routes under, such as
    /// `/v1/` routes also published as `/v2/`: each route counts at
    /// its own path and at the aliased one.
    #[serde(default)]
    pub route_aliases: BTreeMap<String, String>,
    /// Further directories with schemas a service's routes import, such as
    /// shared zod schemas, read to tell which reject unknown keys.
    #[serde(default)]
    pub schemas: Vec<String>,
    /// `OpenAPI` documents a client is generated from, with one function per
    /// operation named after its `operationId` (see `call_names`).
    #[serde(default)]
    pub generated_from: Vec<String>,
    /// How a generated client names an operation's function, from its
    /// `operationId`: templates with `{operationId}`, `{camel}`, `{pascal}`
    /// or `{snake}`, such as `{camel}` or `{pascal}.Invoke`. The
    /// `operationId` as written if empty.
    #[serde(default)]
    pub call_names: Vec<String>,
    /// Directories with a consumer's code; all of it if empty.
    #[serde(default)]
    pub include: Vec<String>,
    /// Names a consumer calls its client through, such as `client`.
    #[serde(default)]
    pub client_receivers: Vec<String>,
    /// Modules of generated clients, such as `generated/billing`: functions
    /// a consumer imports from them are client methods.
    #[serde(default)]
    pub client_modules: Vec<String>,
}

/// The change being verified: one repository at two commits.
#[derive(Debug, Clone, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct Change {
    /// The changed repository.
    pub repository: String,
    /// The commit before the change.
    pub base: String,
    /// The commit with the change; the working directory, uncommitted
    /// changes included, when absent.
    #[serde(default)]
    pub head: Option<String>,
}

impl Change {
    /// The head commit, or the working directory's name for it, for
    /// showing to a person.
    pub fn head_label(&self) -> &str {
        self.head.as_deref().unwrap_or("working directory")
    }
}

/// A document built from a service's code, standing in for one of its
/// committed documents.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct GeneratedDocument {
    /// The committed document it stands for, so both snapshots of a change
    /// name the same document whichever file each was written to.
    pub name: String,
    /// Where it was written.
    pub path: PathBuf,
}

/// A manifest as written.
#[derive(Debug, Clone, Deserialize, PartialEq)]
#[serde(deny_unknown_fields)]
pub struct Manifest {
    /// The grammar file, relative to the manifest; the built-in HTTP
    /// contracts grammar if absent.
    #[serde(default)]
    pub grammar: Option<PathBuf>,
    /// The change.
    pub change: Change,
    /// Every repository, by name.
    pub repositories: BTreeMap<String, Repository>,
    /// A grammar already lowered, which wins over `grammar`: `lemma check`
    /// passes the one its `.lm` program lowers to.
    #[serde(skip)]
    pub lowered: Option<lemma_grammar::GrammarFile>,
}

impl Manifest {
    /// Reads a manifest and makes its paths absolute.
    pub fn load(path: &Path) -> Result<Self, VerifyError> {
        let text = std::fs::read_to_string(path).map_err(|source| VerifyError::Read {
            path: path.to_owned(),
            source,
        })?;
        let mut manifest: Self =
            yaml_serde::from_str(&text).map_err(|source| VerifyError::Manifest {
                path: path.to_owned(),
                source,
            })?;
        let directory = path.parent().unwrap_or(Path::new("."));
        manifest.grammar = manifest.grammar.map(|grammar| directory.join(grammar));
        for repository in manifest.repositories.values_mut() {
            repository.path = directory.join(&repository.path);
        }
        if !manifest
            .repositories
            .contains_key(&manifest.change.repository)
        {
            return Err(VerifyError::UnknownRepository {
                name: manifest.change.repository.clone(),
            });
        }
        Ok(manifest)
    }
}
