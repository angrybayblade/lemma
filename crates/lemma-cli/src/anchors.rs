//! Finding where a symbol is used, for `lemma check --anchors`: through the
//! running daemon, whose language servers stay warm between checks, or,
//! without one, through servers started for this check alone.

use std::collections::{BTreeMap, HashMap};
use std::path::{Path, PathBuf};
use std::time::Duration;

use lemma_model::{Language, Position};
use lemma_providers::{ProviderRegistry, SourcePoint};
use lemma_query::{FilesError, SessionPool, UnindexedRoot, UsesAnswer, find_uses};
use serde::{Deserialize, Serialize};

/// How long a server may take to start and to answer.
pub(crate) const REQUEST_TIMEOUT: Duration = Duration::from_secs(120);

/// Where providers are configured for a root, unless a file is given.
const PROJECT_PROVIDERS: &str = ".lemma/providers.toml";

/// A question about a symbol's uses, as the daemon is asked it.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub(crate) struct UsesRequest {
    pub root: PathBuf,
    pub file: String,
    pub language: Language,
    pub position: Position,
    /// A fingerprint of the root's files, so a warm server is told what
    /// changed only when they did.
    pub fingerprint: String,
    /// Provider configuration given with `--providers`.
    pub providers_file: Option<PathBuf>,
}

/// Why a symbol's uses are not known.
#[derive(Debug, thiserror::Error)]
pub(crate) enum UsesError {
    /// The root's files could not be listed to fingerprint them.
    #[error("could not list the files under {}", root.display())]
    Listing {
        root: PathBuf,
        #[source]
        source: FilesError,
    },
    /// Every installed language server failed to answer.
    #[error("{message}")]
    ProviderFailed { message: String },
}

/// Answers `lemma check --anchors`' questions, from the daemon when one is
/// running and otherwise from servers it starts itself.
pub(crate) struct CheckReferences {
    asks_daemon: bool,
    providers_file: Option<PathBuf>,
    /// Servers started in this process; `None` once stopped.
    pool: Option<SessionPool>,
    fingerprints: BTreeMap<PathBuf, String>,
}

impl CheckReferences {
    /// Answers through the running daemon when `asks_daemon`, with
    /// providers from `providers_file` or each root's own configuration.
    pub(crate) fn new(asks_daemon: bool, providers_file: Option<PathBuf>) -> Self {
        Self {
            asks_daemon,
            providers_file,
            pool: Some(SessionPool::for_one_lookup()),
            fingerprints: BTreeMap::new(),
        }
    }

    /// The fingerprint of `root`'s files, listed once per check.
    fn fingerprint(&mut self, root: &Path) -> Result<String, UsesError> {
        if let Some(fingerprint) = self.fingerprints.get(root) {
            return Ok(fingerprint.clone());
        }
        let files = root_files(root).map_err(|source| UsesError::Listing {
            root: root.to_owned(),
            source,
        })?;
        let fingerprint = fingerprint(&files);
        self.fingerprints
            .insert(root.to_owned(), fingerprint.clone());
        Ok(fingerprint)
    }

    /// Asks the daemon, if it is to be asked and is running. `None` means
    /// it could not answer, and is not asked again.
    fn ask_daemon(&mut self, request: &UsesRequest) -> Option<UsesAnswer> {
        if !self.asks_daemon {
            return None;
        }
        let answer = ask_daemon(request);
        self.asks_daemon = answer.is_some();
        answer
    }

    fn ask_here(&mut self, request: &UsesRequest) -> UsesAnswer {
        let registry = match registry(&request.root, request.providers_file.as_deref()) {
            Ok(registry) => registry,
            Err(error) => {
                return UsesAnswer::Failed {
                    message: format!("{error:#}"),
                };
            }
        };
        let pool = self.pool.get_or_insert_with(SessionPool::for_one_lookup);
        uses_in(pool, &registry, request)
    }
}

impl lemma_graph::References for CheckReferences {
    fn uses(
        &mut self,
        root: &Path,
        file: &str,
        language: Language,
        position: Position,
    ) -> lemma_graph::Uses {
        let request = UsesRequest {
            root: root.to_owned(),
            file: file.to_owned(),
            language,
            position,
            fingerprint: self.fingerprint(root)?,
            providers_file: self.providers_file.clone(),
        };
        let answer = match self.ask_daemon(&request) {
            Some(answer) => answer,
            None => self.ask_here(&request),
        };
        match answer {
            UsesAnswer::Found { locations } => Ok(Some(locations)),
            UsesAnswer::NoProvider => Ok(None),
            UsesAnswer::Failed { message } => Err(Box::new(UsesError::ProviderFailed { message })),
        }
    }
}

impl Drop for CheckReferences {
    fn drop(&mut self) {
        if let Some(pool) = self.pool.take() {
            pool.shutdown();
        }
    }
}

/// Answers `request` with servers from `pool`: the daemon's, kept warm, or
/// this check's own.
pub(crate) fn uses_in(
    pool: &mut SessionPool,
    registry: &ProviderRegistry,
    request: &UsesRequest,
) -> UsesAnswer {
    let root = &request.root;
    let mut files = || root_files(root);
    let mut unindexed = UnindexedRoot {
        root,
        fingerprint: &request.fingerprint,
        files: &mut files,
    };
    let at = SourcePoint {
        path: &request.file,
        language: request.language,
        position: request.position,
    };
    find_uses(pool, registry, &mut unindexed, at, REQUEST_TIMEOUT)
}

/// The built-in providers, plus those in `providers_file`, or in the root's
/// own configuration when none is given.
pub(crate) fn registry(
    root: &Path,
    providers_file: Option<&Path>,
) -> anyhow::Result<ProviderRegistry> {
    let Some(file) = providers_file else {
        return Ok(ProviderRegistry::load(&root.join(PROJECT_PROVIDERS))?);
    };
    anyhow::ensure!(
        file.is_file(),
        "provider configuration {} does not exist",
        file.display()
    );
    Ok(ProviderRegistry::load(file)?)
}

/// The content of every file under `root` as Git sees it, by path: the
/// blob for a file Git vouches for, otherwise a hash of what is on disk.
pub(crate) fn root_files(root: &Path) -> Result<HashMap<String, String>, FilesError> {
    let listed = lemma_http::list_with_blobs(root, None)?;
    let mut files = HashMap::new();
    for file in listed {
        let content = match file.blob {
            Some(blob) => blob,
            None => match std::fs::read(root.join(&file.path)) {
                Ok(bytes) => format!("disk:{}", blake3::hash(&bytes)),
                // Gone since it was listed.
                Err(_) => continue,
            },
        };
        files.insert(file.path, content);
    }
    Ok(files)
}

/// A fingerprint of `files`: the same for the same paths and contents.
fn fingerprint(files: &HashMap<String, String>) -> String {
    let mut sorted: Vec<(&String, &String)> = files.iter().collect();
    sorted.sort();
    let mut hasher = blake3::Hasher::new();
    for (path, content) in sorted {
        hasher.update(path.as_bytes());
        hasher.update(&[0]);
        hasher.update(content.as_bytes());
        hasher.update(&[0]);
    }
    hasher.finalize().to_hex().to_string()
}

#[cfg(unix)]
fn ask_daemon(request: &UsesRequest) -> Option<UsesAnswer> {
    crate::daemon::uses(request)
}

#[cfg(not(unix))]
fn ask_daemon(_request: &UsesRequest) -> Option<UsesAnswer> {
    None
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_fingerprint_follows_paths_and_contents_not_order() {
        let first = HashMap::from([
            ("a.go".to_owned(), "1".to_owned()),
            ("b.go".to_owned(), "2".to_owned()),
        ]);
        let same = HashMap::from([
            ("b.go".to_owned(), "2".to_owned()),
            ("a.go".to_owned(), "1".to_owned()),
        ]);
        let edited = HashMap::from([
            ("a.go".to_owned(), "1".to_owned()),
            ("b.go".to_owned(), "3".to_owned()),
        ]);

        assert_eq!(fingerprint(&first), fingerprint(&same));
        assert_ne!(fingerprint(&first), fingerprint(&edited));
    }
}
