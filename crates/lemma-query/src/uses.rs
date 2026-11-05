//! Finding where a symbol is used under a root that is not indexed, such as
//! a repository at the commit `lemma check` reads, through the same pool of
//! language servers indexed lookups use.

use std::collections::HashMap;
use std::fs;
use std::path::Path;
use std::time::Duration;

use lemma_model::{Language, Location, LocationQuery, ProviderAnswer};
use lemma_providers::{ProviderRegistry, SemanticProvider, SourcePoint};

use crate::lookup::{describe, identifier_at, same_family};
use crate::sessions::{FileHashes, FilesError, PoolError, RootView, SessionPool};

/// A root that is not indexed: where it is, a fingerprint of its files (the
/// same fingerprint means the same files), and how to list their content
/// hashes by path when a server must be told what changed.
pub struct UnindexedRoot<'a> {
    /// The directory the server serves.
    pub root: &'a Path,
    /// A fingerprint of the files under it.
    pub fingerprint: &'a str,
    /// Lists every file's content hash, by path relative to the root.
    pub files: &'a mut dyn FnMut() -> Result<HashMap<String, String>, FilesError>,
}

/// Where a symbol is used, as a provider answered.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(tag = "outcome", rename_all = "snake_case")]
pub enum UsesAnswer {
    /// The uses found, the symbol's own declaration possibly among them.
    Found {
        /// Where.
        locations: Vec<Location>,
    },
    /// No provider for the language is installed.
    NoProvider,
    /// Every installed provider failed to answer.
    Failed {
        /// What went wrong, with its causes.
        message: String,
    },
}

/// Where the symbol at `at` under `root` is used, asking the first
/// installed provider that starts. Servers are kept in `pool`, so a
/// long-lived pool answers later questions about the root warm.
pub fn find_uses(
    pool: &mut SessionPool,
    registry: &ProviderRegistry,
    root: &mut UnindexedRoot<'_>,
    at: SourcePoint<'_>,
    request_timeout: Duration,
) -> UsesAnswer {
    let view = RootView {
        root: root.root,
        fingerprint: root.fingerprint,
    };
    let mut listed: Option<FileHashes> = None;
    let mut files = || -> Result<FileHashes, PoolError> {
        if let Some(listed) = &listed {
            return Ok(listed.clone());
        }
        let hashes = (root.files)().map_err(PoolError::Files)?;
        listed = Some(hashes.clone());
        Ok(hashes)
    };
    let installed = registry.installed(at.language);
    if installed.is_empty() {
        return UsesAnswer::NoProvider;
    }
    let mut failures = Vec::new();
    for provider in installed {
        let session = match pool.session_at(&view, &mut files, &provider, request_timeout) {
            Ok(session) => session,
            Err(PoolError::Start(error)) => {
                failures.push(format!("{}: {}", provider.definition.id, describe(&error)));
                continue;
            }
            Err(PoolError::Files(error)) => {
                let message = format!("could not list the files: {}", describe(error.as_ref()));
                return UsesAnswer::Failed { message };
            }
            Err(PoolError::Store(error)) => {
                let message = describe(&error);
                return UsesAnswer::Failed { message };
            }
        };
        let answered = ask(session, view.root, &mut files, at);
        if let UsesAnswer::Failed { message } = answered {
            // The server may be wedged or gone; start a fresh one next time.
            pool.discard_at(view.root, &provider);
            let message = format!("{}: {message}", provider.definition.id);
            return UsesAnswer::Failed { message };
        }
        return answered;
    }
    UsesAnswer::Failed {
        message: failures.join("; "),
    }
}

/// Asks a started server where the symbol at `at` is used, loading the
/// projects that mention it first if the server needs that.
fn ask(
    session: &mut dyn SemanticProvider,
    root: &Path,
    files: &mut dyn FnMut() -> Result<FileHashes, PoolError>,
    at: SourcePoint<'_>,
) -> UsesAnswer {
    let paths = if session.is_project_scoped() {
        mentioning(root, files, at)
    } else {
        Vec::new()
    };
    let paths: Vec<&str> = paths.iter().map(String::as_str).collect();
    let asked = session
        .open_projects(&paths)
        .and_then(|()| session.locations(LocationQuery::References, at));
    match asked {
        Ok(ProviderAnswer::Found(locations)) => UsesAnswer::Found { locations },
        Ok(ProviderAnswer::Empty | ProviderAnswer::Unsupported) => UsesAnswer::Found {
            locations: Vec::new(),
        },
        Err(error) => UsesAnswer::Failed {
            message: describe(&error),
        },
    }
}

/// The asked file, then the root's files of the same language family whose
/// text contains the name at `at`, sorted: the projects a server that only
/// searches loaded ones must load first. None beyond the file if the root
/// cannot be listed.
fn mentioning(
    root: &Path,
    files: &mut dyn FnMut() -> Result<FileHashes, PoolError>,
    at: SourcePoint<'_>,
) -> Vec<String> {
    let mut paths = vec![at.path.to_owned()];
    let Some(name) = identifier_at(root, at) else {
        return paths;
    };
    let Ok(listed) = files() else {
        return paths;
    };
    let finder = memchr::memmem::Finder::new(name.as_bytes());
    let mut found: Vec<String> = listed
        .into_keys()
        .filter(|path| path != at.path)
        .filter(|path| {
            let extension = path.rsplit_once('.').map_or("", |(_, extension)| extension);
            Language::from_extension(extension)
                .is_some_and(|language| same_family(language, at.language))
        })
        .filter(|path| {
            fs::read(root.join(path)).is_ok_and(|contents| finder.find(&contents).is_some())
        })
        .collect();
    found.sort();
    paths.extend(found);
    paths
}
