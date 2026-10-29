//! Language servers kept running between lookups.
//!
//! Starting a server and letting it load a large workspace takes seconds,
//! while asking a loaded one takes milliseconds. A long-lived pool keeps
//! servers warm and tells them which files changed between lookups.

use std::collections::HashMap;
use std::collections::hash_map::Entry;
use std::path::PathBuf;
use std::time::Duration;

use lemma_providers::{FileChange, InstalledProvider, ProviderError, ProviderSession};
use lemma_store::{Store, StoreError, StoredSnapshot};

/// Why a session could not be provided.
pub(crate) enum PoolError {
    /// The server could not be started. The next provider may be tried.
    Start(ProviderError),
    /// The index could not be read.
    Store(StoreError),
}

/// Started provider sessions, at most one per provider configuration and
/// workspace root.
pub struct SessionPool {
    sessions: HashMap<SessionKey, PooledSession>,
    /// Whether sessions outlive one lookup, and so must be told about files
    /// that change between lookups.
    is_long_lived: bool,
}

#[derive(Debug, Clone, PartialEq, Eq, Hash)]
struct SessionKey {
    provider_id: String,
    config_fingerprint: String,
    root: PathBuf,
}

struct PooledSession {
    session: ProviderSession,
    /// The snapshot fingerprint the server's view matches.
    synced_fingerprint: String,
    /// Content hash of every file, by path, as of `synced_fingerprint`.
    /// Only kept in a long-lived pool.
    synced_files: HashMap<String, String>,
}

impl SessionPool {
    /// A pool for a single lookup, such as one command-line call. Sessions
    /// are stopped by [`SessionPool::shutdown`].
    pub fn for_one_lookup() -> Self {
        Self {
            sessions: HashMap::new(),
            is_long_lived: false,
        }
    }

    /// A pool whose sessions serve many lookups over time, as in a daemon.
    pub fn long_lived() -> Self {
        Self {
            sessions: HashMap::new(),
            is_long_lived: true,
        }
    }

    /// How many servers are running.
    pub fn len(&self) -> usize {
        self.sessions.len()
    }

    /// Whether no server is running.
    pub fn is_empty(&self) -> bool {
        self.sessions.is_empty()
    }

    /// Stops every server.
    pub fn shutdown(self) {
        for pooled in self.sessions.into_values() {
            pooled.session.shutdown();
        }
    }

    /// A running session for the provider on the snapshot's root: reused if
    /// one exists, after telling it which files changed since it last
    /// answered, or started otherwise.
    pub(crate) fn session(
        &mut self,
        store: &Store,
        snapshot: &StoredSnapshot,
        provider: &InstalledProvider,
        request_timeout: Duration,
    ) -> Result<&mut ProviderSession, PoolError> {
        let key = session_key(snapshot, provider);
        // Syncing may drop a session that could not be told about changes.
        self.sync(&key, store, snapshot)?;
        match self.sessions.entry(key) {
            Entry::Occupied(entry) => Ok(&mut entry.into_mut().session),
            Entry::Vacant(entry) => {
                let pooled = start(
                    store,
                    snapshot,
                    provider,
                    request_timeout,
                    self.is_long_lived,
                )?;
                Ok(&mut entry.insert(pooled).session)
            }
        }
    }

    /// Stops and forgets the provider's session, so the next lookup starts a
    /// fresh server.
    pub(crate) fn discard(&mut self, snapshot: &StoredSnapshot, provider: &InstalledProvider) {
        if let Some(pooled) = self.sessions.remove(&session_key(snapshot, provider)) {
            pooled.session.shutdown();
        }
    }

    fn sync(
        &mut self,
        key: &SessionKey,
        store: &Store,
        snapshot: &StoredSnapshot,
    ) -> Result<(), PoolError> {
        let Some(pooled) = self.sessions.get_mut(key) else {
            return Ok(());
        };
        if pooled.synced_fingerprint == snapshot.snapshot.fingerprint {
            return Ok(());
        }
        let current = file_hashes(store, snapshot)?;
        let changes = changes_between(&pooled.synced_files, &current);
        if pooled.session.files_changed(&changes).is_ok() {
            pooled
                .synced_fingerprint
                .clone_from(&snapshot.snapshot.fingerprint);
            pooled.synced_files = current;
            return Ok(());
        }
        // A server that cannot be told is replaced rather than trusted.
        if let Some(stale) = self.sessions.remove(key) {
            stale.session.shutdown();
        }
        Ok(())
    }
}

fn start(
    store: &Store,
    snapshot: &StoredSnapshot,
    provider: &InstalledProvider,
    request_timeout: Duration,
    is_long_lived: bool,
) -> Result<PooledSession, PoolError> {
    let root = PathBuf::from(&snapshot.snapshot.root);
    let session = ProviderSession::start(
        &provider.definition,
        &provider.executable,
        &root,
        request_timeout,
    )
    .map_err(PoolError::Start)?;
    // A fresh server reads files from disk itself; only a long-lived one
    // needs a baseline to compare later snapshots against.
    let synced_files = if is_long_lived {
        file_hashes(store, snapshot)?
    } else {
        HashMap::new()
    };
    Ok(PooledSession {
        session,
        synced_fingerprint: snapshot.snapshot.fingerprint.clone(),
        synced_files,
    })
}

fn session_key(snapshot: &StoredSnapshot, provider: &InstalledProvider) -> SessionKey {
    SessionKey {
        provider_id: provider.definition.id.clone(),
        config_fingerprint: provider.definition.fingerprint(),
        root: PathBuf::from(&snapshot.snapshot.root),
    }
}

fn file_hashes(
    store: &Store,
    snapshot: &StoredSnapshot,
) -> Result<HashMap<String, String>, PoolError> {
    let files = store.files(snapshot.id).map_err(PoolError::Store)?;
    Ok(files
        .into_iter()
        .map(|file| (file.path, file.content_hash))
        .collect())
}

/// What a server that saw `before` needs to hear to see `after`, sorted by
/// path.
fn changes_between(
    before: &HashMap<String, String>,
    after: &HashMap<String, String>,
) -> Vec<(String, FileChange)> {
    let mut changes: Vec<(String, FileChange)> = after
        .iter()
        .filter_map(|(path, hash)| match before.get(path) {
            None => Some((path.clone(), FileChange::Created)),
            Some(previous) if previous != hash => Some((path.clone(), FileChange::Changed)),
            Some(_) => None,
        })
        .collect();
    changes.extend(
        before
            .keys()
            .filter(|path| !after.contains_key(*path))
            .map(|path| (path.clone(), FileChange::Deleted)),
    );
    changes.sort_by(|left, right| left.0.cmp(&right.0));
    changes
}

#[cfg(test)]
mod tests {
    use super::*;

    fn hashes(entries: &[(&str, &str)]) -> HashMap<String, String> {
        entries
            .iter()
            .map(|(path, hash)| ((*path).to_owned(), (*hash).to_owned()))
            .collect()
    }

    #[test]
    fn changes_cover_created_changed_and_deleted_files() {
        let before = hashes(&[("a.py", "1"), ("b.py", "2"), ("c.py", "3")]);
        let after = hashes(&[("a.py", "1"), ("b.py", "9"), ("d.py", "4")]);

        let changes = changes_between(&before, &after);

        assert_eq!(
            changes,
            [
                ("b.py".to_owned(), FileChange::Changed),
                ("c.py".to_owned(), FileChange::Deleted),
                ("d.py".to_owned(), FileChange::Created),
            ]
        );
    }

    #[test]
    fn identical_files_need_no_changes() {
        let files = hashes(&[("a.py", "1")]);

        assert_eq!(changes_between(&files, &files), []);
    }
}
