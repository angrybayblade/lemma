//! Language servers kept running between lookups.
//!
//! Starting a server and letting it load a large workspace takes seconds,
//! while asking a loaded one takes milliseconds. A long-lived pool keeps
//! servers warm and tells them which files changed between lookups.

use std::collections::hash_map::Entry;
use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};
use std::sync::mpsc::{self, Receiver, Sender};
use std::thread;
use std::time::{Duration, Instant};

use lemma_model::Language;

use lemma_providers::{FileChange, InstalledProvider, ProviderError, ProviderSession};
use lemma_store::{FactStore, Store, StoreError, StoredSnapshot};

/// Why a session could not be provided.
pub(crate) enum PoolError {
    /// The server could not be started. The next provider may be tried.
    Start(ProviderError),
    /// The index could not be read.
    Store(StoreError),
    /// The files of a root that is not indexed could not be listed.
    Files(FilesError),
}

/// Why the files of a root that is not indexed could not be listed.
pub type FilesError = Box<dyn std::error::Error + Send + Sync>;

/// The content hash of every file under a root, by path.
pub(crate) type FileHashes = HashMap<String, String>;

/// Started provider sessions, at most one per provider configuration and
/// workspace root.
pub struct SessionPool {
    sessions: HashMap<SessionKey, PooledSession>,
    /// Whether sessions outlive one lookup, and so must be told about files
    /// that change between lookups.
    is_long_lived: bool,
    /// Sessions being started in the background by [`SessionPool::warm_up`].
    warming: HashSet<SessionKey>,
    /// Warm-ups whose server failed to start, kept to report on first use.
    failed_warm_ups: HashMap<SessionKey, ProviderError>,
    arrivals: Sender<Arrival>,
    arrivals_inbox: Receiver<Arrival>,
}

/// A server to start ahead of the first lookup, so it is ready by then.
pub struct WarmUp {
    /// The index database, opened separately by the warm-up thread.
    pub db_path: PathBuf,
    /// The snapshot whose root the server will serve.
    pub snapshot: StoredSnapshot,
    /// The provider to start.
    pub provider: InstalledProvider,
    /// The language to warm up. A file of it is opened so the server loads
    /// that file's project.
    pub language: Language,
    /// How long to wait for the server to start and load.
    pub request_timeout: Duration,
}

/// The result of a background warm-up.
struct Arrival {
    key: SessionKey,
    outcome: WarmUpOutcome,
}

enum WarmUpOutcome {
    Ready(Box<PooledSession>),
    StartFailed(ProviderError),
    /// Something other than the server failed, such as reading the index;
    /// the next lookup starts the server itself.
    Abandoned,
}

#[derive(Debug, Clone, PartialEq, Eq, Hash)]
struct SessionKey {
    provider_id: String,
    config_fingerprint: String,
    root: PathBuf,
}

struct PooledSession {
    session: ProviderSession,
    /// When a lookup last used the session.
    last_used: Instant,
    /// Whether the server is paused; it is resumed before any use.
    is_paused: bool,
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
        Self::new(false)
    }

    /// A pool whose sessions serve many lookups over time, as in a daemon.
    pub fn long_lived() -> Self {
        Self::new(true)
    }

    fn new(is_long_lived: bool) -> Self {
        let (arrivals, arrivals_inbox) = mpsc::channel();
        Self {
            sessions: HashMap::new(),
            is_long_lived,
            warming: HashSet::new(),
            failed_warm_ups: HashMap::new(),
            arrivals,
            arrivals_inbox,
        }
    }

    /// Starts a server in the background unless one is running or starting
    /// already. A lookup that needs it meanwhile waits for it instead of
    /// starting a second one.
    pub fn warm_up(&mut self, warm_up: WarmUp) {
        self.collect_arrivals(None);
        let key = session_key(&warm_up.snapshot, &warm_up.provider);
        if self.sessions.contains_key(&key) || self.warming.contains(&key) {
            return;
        }
        self.warming.insert(key.clone());
        let arrivals = self.arrivals.clone();
        let is_long_lived = self.is_long_lived;
        thread::spawn(move || {
            let outcome = warm_session(&warm_up, is_long_lived);
            // The pool may have been dropped; the session then stops with it.
            let _ = arrivals.send(Arrival { key, outcome });
        });
    }

    /// How many servers are running.
    pub fn len(&self) -> usize {
        self.sessions.len()
    }

    /// Whether no server is running.
    pub fn is_empty(&self) -> bool {
        self.sessions.is_empty()
    }

    /// Whether a server for this root is running or starting.
    pub fn serves_root(&self, root: &Path) -> bool {
        self.sessions
            .keys()
            .chain(&self.warming)
            .any(|key| key.root == root)
    }

    /// Applies the idle policy: pauses servers idle longer than
    /// `pause_after`, stops those idle longer than `stop_after`, then stops
    /// the least recently used until the servers fit the memory budget.
    pub fn maintain(&mut self, policy: &IdlePolicy, now: Instant) {
        self.collect_arrivals(None);
        let mut expired = Vec::new();
        for (key, pooled) in &mut self.sessions {
            let idle = now.saturating_duration_since(pooled.last_used);
            if policy.stop_after.is_some_and(|limit| idle >= limit) {
                expired.push(key.clone());
            } else if policy.pause_after.is_some_and(|limit| idle >= limit) && !pooled.is_paused {
                pooled.is_paused = pooled.session.pause();
            }
        }
        for key in expired {
            self.stop(&key);
        }
        if let Some(budget) = policy.memory_budget_bytes {
            self.fit_budget(budget);
        }
    }

    /// What every server is doing, including those still starting.
    pub fn status(&mut self, now: Instant) -> Vec<SessionStatus> {
        self.collect_arrivals(None);
        let ids: Vec<u32> = self
            .sessions
            .values()
            .map(|pooled| pooled.session.process_id())
            .collect();
        let memory = lemma_lsp::memory_bytes(&ids);
        let mut statuses: Vec<SessionStatus> = self
            .sessions
            .iter()
            .map(|(key, pooled)| SessionStatus {
                provider_id: key.provider_id.clone(),
                root: key.root.clone(),
                idle: now.saturating_duration_since(pooled.last_used),
                state: if pooled.is_paused {
                    ServerState::Paused
                } else {
                    ServerState::Running
                },
                memory_bytes: memory.get(&pooled.session.process_id()).copied(),
            })
            .collect();
        statuses.extend(self.warming.iter().map(|key| SessionStatus {
            provider_id: key.provider_id.clone(),
            root: key.root.clone(),
            idle: Duration::ZERO,
            state: ServerState::Starting,
            memory_bytes: None,
        }));
        statuses.sort_by(|left, right| {
            left.root
                .cmp(&right.root)
                .then(left.provider_id.cmp(&right.provider_id))
        });
        statuses
    }

    fn fit_budget(&mut self, budget: u64) {
        let ids: Vec<u32> = self
            .sessions
            .values()
            .map(|pooled| pooled.session.process_id())
            .collect();
        let memory = lemma_lsp::memory_bytes(&ids);
        let mut by_age: Vec<(Instant, SessionKey, u64)> = self
            .sessions
            .iter()
            .map(|(key, pooled)| {
                let bytes = memory
                    .get(&pooled.session.process_id())
                    .copied()
                    .unwrap_or(0);
                (pooled.last_used, key.clone(), bytes)
            })
            .collect();
        by_age.sort_by_key(|(last_used, _, _)| *last_used);
        let mut total: u64 = by_age.iter().map(|(_, _, bytes)| bytes).sum();
        for (_, key, bytes) in by_age {
            if total <= budget {
                break;
            }
            self.stop(&key);
            total = total.saturating_sub(bytes);
        }
    }

    fn stop(&mut self, key: &SessionKey) {
        if let Some(mut pooled) = self.sessions.remove(key) {
            pooled.wake();
            pooled.session.shutdown();
        }
    }

    /// Stops every server, waiting for any still starting.
    pub fn shutdown(mut self) {
        while !self.warming.is_empty() {
            let Some(key) = self.warming.iter().next().cloned() else {
                break;
            };
            self.collect_arrivals(Some(&key));
        }
        for mut pooled in self.sessions.into_values() {
            pooled.wake();
            pooled.session.shutdown();
        }
    }

    /// Moves finished warm-ups into the pool. With `waiting_for`, blocks
    /// until that session's warm-up has finished, if it is still running.
    fn collect_arrivals(&mut self, waiting_for: Option<&SessionKey>) {
        loop {
            let must_wait = waiting_for.is_some_and(|key| self.warming.contains(key));
            let arrival = if must_wait {
                self.arrivals_inbox.recv().ok()
            } else {
                self.arrivals_inbox.try_recv().ok()
            };
            let Some(Arrival { key, outcome }) = arrival else {
                return;
            };
            self.warming.remove(&key);
            match outcome {
                WarmUpOutcome::Ready(pooled) => {
                    self.sessions.insert(key, *pooled);
                }
                WarmUpOutcome::StartFailed(error) => {
                    self.failed_warm_ups.insert(key, error);
                }
                WarmUpOutcome::Abandoned => {}
            }
        }
    }

    /// A running session for the provider on the snapshot's root: reused if
    /// one exists, after telling it which files changed since it last
    /// answered, or started otherwise.
    pub(crate) fn session(
        &mut self,
        store: &dyn FactStore,
        snapshot: &StoredSnapshot,
        provider: &InstalledProvider,
        request_timeout: Duration,
    ) -> Result<&mut ProviderSession, PoolError> {
        let view = RootView {
            root: Path::new(&snapshot.snapshot.root),
            fingerprint: &snapshot.snapshot.fingerprint,
        };
        let mut files = || file_hashes(store, snapshot);
        self.session_at(&view, &mut files, provider, request_timeout)
    }

    /// A running session for the provider on `view`'s root, as
    /// [`SessionPool::session`] gives for an indexed snapshot. `files` lists
    /// the root's file hashes, asked for only when the server must be told
    /// what changed or needs a baseline to compare with later.
    pub(crate) fn session_at(
        &mut self,
        view: &RootView<'_>,
        files: &mut dyn FnMut() -> Result<FileHashes, PoolError>,
        provider: &InstalledProvider,
        request_timeout: Duration,
    ) -> Result<&mut ProviderSession, PoolError> {
        let key = root_key(view.root, provider);
        self.collect_arrivals(Some(&key));
        if let Some(error) = self.failed_warm_ups.remove(&key) {
            return Err(PoolError::Start(error));
        }
        // A paused server cannot read what sync sends it, so resume first.
        if let Some(pooled) = self.sessions.get_mut(&key) {
            pooled.wake();
        }
        // Syncing may drop a session that could not be told about changes.
        self.sync(&key, view.fingerprint, files)?;
        match self.sessions.entry(key) {
            Entry::Occupied(entry) => {
                let pooled = entry.into_mut();
                pooled.last_used = Instant::now();
                Ok(&mut pooled.session)
            }
            Entry::Vacant(entry) => {
                let pooled = start(view, files, provider, request_timeout, self.is_long_lived)?;
                Ok(&mut entry.insert(pooled).session)
            }
        }
    }

    /// Stops and forgets the provider's session, so the next lookup starts a
    /// fresh server.
    pub(crate) fn discard(&mut self, snapshot: &StoredSnapshot, provider: &InstalledProvider) {
        self.discard_at(Path::new(&snapshot.snapshot.root), provider);
    }

    /// Stops and forgets the provider's session on `root`.
    pub(crate) fn discard_at(&mut self, root: &Path, provider: &InstalledProvider) {
        if let Some(mut pooled) = self.sessions.remove(&root_key(root, provider)) {
            pooled.wake();
            pooled.session.shutdown();
        }
    }

    fn sync(
        &mut self,
        key: &SessionKey,
        fingerprint: &str,
        files: &mut dyn FnMut() -> Result<FileHashes, PoolError>,
    ) -> Result<(), PoolError> {
        let Some(pooled) = self.sessions.get_mut(key) else {
            return Ok(());
        };
        if pooled.synced_fingerprint == fingerprint {
            return Ok(());
        }
        let current = files()?;
        let changes = changes_between(&pooled.synced_files, &current);
        if pooled.session.files_changed(&changes).is_ok() {
            fingerprint.clone_into(&mut pooled.synced_fingerprint);
            pooled.synced_files = current;
            return Ok(());
        }
        // A server that cannot be told is replaced rather than trusted.
        if let Some(mut stale) = self.sessions.remove(key) {
            stale.wake();
            stale.session.shutdown();
        }
        Ok(())
    }
}

/// A root a server serves, and a fingerprint of its files: the same
/// fingerprint means the same files.
pub(crate) struct RootView<'a> {
    pub root: &'a Path,
    pub fingerprint: &'a str,
}

fn start(
    view: &RootView<'_>,
    files: &mut dyn FnMut() -> Result<FileHashes, PoolError>,
    provider: &InstalledProvider,
    request_timeout: Duration,
    is_long_lived: bool,
) -> Result<PooledSession, PoolError> {
    let session = ProviderSession::start(
        &provider.definition,
        &provider.executable,
        view.root,
        request_timeout,
    )
    .map_err(PoolError::Start)?;
    // A fresh server reads files from disk itself; only a long-lived one
    // needs a baseline to compare later snapshots against.
    let synced_files = if is_long_lived {
        files()?
    } else {
        HashMap::new()
    };
    Ok(PooledSession {
        session,
        last_used: Instant::now(),
        is_paused: false,
        synced_fingerprint: view.fingerprint.to_owned(),
        synced_files,
    })
}

/// Starts and readies a server on a background thread, with its own
/// connection to the index for the baseline of file hashes.
fn warm_session(warm_up: &WarmUp, is_long_lived: bool) -> WarmUpOutcome {
    let Ok(store) = Store::open(&warm_up.db_path) else {
        return WarmUpOutcome::Abandoned;
    };
    let view = RootView {
        root: Path::new(&warm_up.snapshot.snapshot.root),
        fingerprint: &warm_up.snapshot.snapshot.fingerprint,
    };
    let mut files = || file_hashes(&store, &warm_up.snapshot);
    let started = start(
        &view,
        &mut files,
        &warm_up.provider,
        warm_up.request_timeout,
        is_long_lived,
    );
    let mut pooled = match started {
        Ok(pooled) => pooled,
        Err(PoolError::Start(error)) => return WarmUpOutcome::StartFailed(error),
        Err(PoolError::Store(_) | PoolError::Files(_)) => return WarmUpOutcome::Abandoned,
    };
    let files = store.files(warm_up.snapshot.id).unwrap_or_default();
    let paths = files
        .iter()
        .filter(|file| file.language == warm_up.language)
        .map(|file| file.path.as_str());
    // A server without a sample, or whose sample cannot be opened, is still
    // usable; the first lookup then waits for readiness itself.
    if let Some(sample_path) = representative_file(paths) {
        let _ = pooled.session.warm_up(&sample_path, warm_up.language);
    }
    WarmUpOutcome::Ready(Box::new(pooled))
}

/// Most directory levels to descend when choosing a representative file.
const REPRESENTATIVE_DEPTH: usize = 4;

/// A file in the densest part of the tree: from the root, repeatedly step
/// into the subdirectory holding the most of these files. Opening it makes
/// a server load the project most lookups will need, rather than whatever
/// sorts first, such as a configuration file at the root.
fn representative_file<'a>(paths: impl Iterator<Item = &'a str>) -> Option<String> {
    let mut candidates: Vec<&str> = paths.collect();
    candidates.sort_unstable();
    let mut prefix = String::new();
    for _ in 0..REPRESENTATIVE_DEPTH {
        let mut counts: HashMap<&str, usize> = HashMap::new();
        for path in &candidates {
            let rest = &path[prefix.len()..];
            if let Some((directory, _)) = rest.split_once('/') {
                *counts.entry(directory).or_insert(0) += 1;
            }
        }
        let Some((densest, _)) = counts
            .into_iter()
            .max_by(|left, right| left.1.cmp(&right.1).then(right.0.cmp(left.0)))
        else {
            break;
        };
        let next_prefix = format!("{prefix}{densest}/");
        candidates.retain(|path| path.starts_with(&next_prefix));
        prefix = next_prefix;
    }
    candidates.first().map(|path| (*path).to_owned())
}

impl PooledSession {
    /// Resumes the server if it is paused.
    fn wake(&mut self) {
        if self.is_paused {
            self.session.resume();
            self.is_paused = false;
        }
    }
}

/// When to pause and stop servers nobody is using.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct IdlePolicy {
    /// Pause a server idle this long, so it uses no CPU and its memory can
    /// be compressed or swapped. `None` never pauses.
    pub pause_after: Option<Duration>,
    /// Stop a server idle this long. `None` keeps it.
    pub stop_after: Option<Duration>,
    /// Stop the least recently used servers while all of them together use
    /// more resident memory than this. `None` sets no limit.
    pub memory_budget_bytes: Option<u64>,
}

/// What a server in the pool is doing.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ServerState {
    /// Being started and loading its project in the background.
    Starting,
    /// Ready to answer.
    Running,
    /// Paused while idle; resumed on the next lookup.
    Paused,
}

/// One server, for status reports.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SessionStatus {
    /// The provider it serves.
    pub provider_id: String,
    /// The workspace root it serves.
    pub root: PathBuf,
    /// How long since a lookup used it; zero while starting.
    pub idle: Duration,
    /// Whether it is starting, running or paused.
    pub state: ServerState,
    /// Resident memory of the server and its helpers, if it could be read.
    pub memory_bytes: Option<u64>,
}

fn session_key(snapshot: &StoredSnapshot, provider: &InstalledProvider) -> SessionKey {
    root_key(Path::new(&snapshot.snapshot.root), provider)
}

fn root_key(root: &Path, provider: &InstalledProvider) -> SessionKey {
    SessionKey {
        provider_id: provider.definition.id.clone(),
        config_fingerprint: provider.definition.fingerprint(),
        root: root.to_owned(),
    }
}

fn file_hashes(store: &dyn FactStore, snapshot: &StoredSnapshot) -> Result<FileHashes, PoolError> {
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
    fn the_representative_file_is_in_the_densest_directory() {
        let paths = [
            "a.config.ts",
            "apps/api/src/one.ts",
            "apps/api/src/two.ts",
            "apps/api/src/three.ts",
            "apps/web/index.ts",
            "tools/x.ts",
        ];

        let chosen = representative_file(paths.into_iter());

        assert_eq!(chosen.as_deref(), Some("apps/api/src/one.ts"));
    }

    #[test]
    fn identical_files_need_no_changes() {
        let files = hashes(&[("a.py", "1")]);

        assert_eq!(changes_between(&files, &files), []);
    }
}
