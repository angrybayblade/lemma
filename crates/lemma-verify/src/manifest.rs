//! A verification manifest: which repositories make up the system, at which
//! commits, what role each plays, and which change is being verified.
//!
//! ```yaml
//! grammar: ../grammars/http-contracts.yaml
//! change: { repository: platform, base: fa87ef98a7^, head: fa87ef98a7 }
//! repositories:
//!   platform:
//!     path: corpora/platform
//!     role: service
//!     service: gateway
//!     specs: [apps/gateway/openapi-v3.1.json]
//!     routes: [apps/gateway/src/pages/api]
//!   platform-client:
//!     path: corpora/platform-client
//!     commit: 9de76a65
//!     role: client
//!     tables: [src/compat/generated-operations.ts]
//!     specs: [spec/openapi.json]
//!   sdk:
//!     path: corpora/sdk
//!     commit: d7a25360
//!     role: consumer
//!     include: [ts/packages/core/src]
//!     client_receivers: [client]
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
    /// A generated client: operation tables mapping methods to operations.
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
    /// Whether a service rejects requests with fields it does not know, as
    /// a strict schema does, even where its spec does not say so.
    #[serde(default)]
    pub rejects_unknown_fields: bool,
    /// Directories with a service's route code.
    #[serde(default)]
    pub routes: Vec<String>,
    /// Path prefixes a service also serves its routes under, such as
    /// `/api/v3/` routes published as `/api/v3.1/`: each route counts at
    /// its own path and at the aliased one.
    #[serde(default)]
    pub route_aliases: BTreeMap<String, String>,
    /// Further directories with schemas a service's routes import, such as
    /// shared zod schemas, read to tell which reject unknown keys.
    #[serde(default)]
    pub schemas: Vec<String>,
    /// Files with a client's operation tables.
    #[serde(default)]
    pub tables: Vec<String>,
    /// `OpenAPI` documents a client is generated from, with one function per
    /// operation named after its `operationId`.
    #[serde(default)]
    pub generated_from: Vec<String>,
    /// Directories with a consumer's code; all of it if empty.
    #[serde(default)]
    pub include: Vec<String>,
    /// Names a consumer calls its client through, such as `client`.
    #[serde(default)]
    pub client_receivers: Vec<String>,
    /// Modules of generated clients, such as `generated/worker`: functions
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
    /// The commit with the change.
    pub head: String,
}

/// A manifest as written.
#[derive(Debug, Clone, Deserialize, PartialEq, Eq)]
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
