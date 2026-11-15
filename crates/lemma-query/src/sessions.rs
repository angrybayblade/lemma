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
    /// The lookup was told to stop: see [`SessionPool::interrupt_when`].
    Interrupted,
}

/// How long a pool that stays leaves a server that could not start
/// before starting it again: long enough that one command's lookups try
/// it once, short enough that fixing what was wrong shows within a minute.
const START_AGAIN_AFTER: Duration = Duration::from_secs(60);

/// Says whether lookups nobody is waiting for should stop.
pub type Interrupt = lemma_providers::Interrupt;

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
    /// Servers that could not start for a lookup, with when and why: a
    /// graph asks dozens of questions, and each would start the server
    /// again only to fall back to the next one.
    failed_starts: HashMap<SessionKey, (Instant, String)>,
    /// What a lookup does about a server that does not answer in time.
    when_slow: WhenSlow,
    /// Servers that did not answer in time where the next one did, with
    /// when: lookups pass them over for as long as `when_slow` says.
    slow: HashMap<SessionKey, Instant>,
    arrivals: Sender<Arrival>,
    arrivals_inbox: Receiver<Arrival>,
    /// Asked before a server is: see [`SessionPool::interrupt_when`].
    interrupt: Option<Interrupt>,
}

/// A server to start ahead of the first lookup, so it is ready by then.
pub struct WarmUp {
    /// The index database, opened separately by the warm-up thread.
    pub db_path: PathBuf,
    /// The snapshot whose root the server will serve.
    pub snapshot: StoredSnapshot,
    /// The providers to try, in order: the first that starts serves, as a
    /// lookup would choose. A server that cannot start (one needing a
    /// dependency the project lacks) gives way to the next.
    pub providers: Vec<InstalledProvider>,
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
            failed_starts: HashMap::new(),
            when_slow: WhenSlow::default(),
            slow: HashMap::new(),
            arrivals,
            arrivals_inbox,
            interrupt: None,
        }
    }

    /// Has every lookup stop with [`QueryError::Interrupted`] instead of
    /// asking a server, or waiting longer for one asked, from when
    /// `is_wanted_elsewhere` first says so: for lookups made ahead of
    /// anyone asking, which give way to a command that arrives. A server
    /// goes on with what it was asked, so it is not asked in vain. `None`
    /// lets lookups run to their end again.
    ///
    /// [`QueryError::Interrupted`]: crate::QueryError::Interrupted
    pub fn interrupt_when(&mut self, is_wanted_elsewhere: Option<Interrupt>) {
        for pooled in self.sessions.values() {
            pooled.session.interrupt_when(is_wanted_elsewhere.clone());
        }
        self.interrupt = is_wanted_elsewhere;
    }

    /// Whether lookups are being stopped: see [`SessionPool::interrupt_when`].
    pub fn is_interrupted(&self) -> bool {
        self.interrupt.as_ref().is_some_and(|interrupt| interrupt())
    }

    /// Sets what a lookup does about a server that does not answer in
    /// time. A change has every server passed over so far tried again.
    pub fn set_when_slow(&mut self, when_slow: WhenSlow) {
        if self.when_slow != when_slow {
            self.slow.clear();
        }
        self.when_slow = when_slow;
    }

    /// How long a server with another after it may take over one question
    /// of a lookup that waits `request_timeout` for any server, where the
    /// next is asked when one is slow; `None` where a slow one is reported.
    pub(crate) fn gives_way_after(&self, request_timeout: Duration) -> Option<Duration> {
        match self.when_slow {
            WhenSlow::Report => None,
            WhenSlow::Next { give_way_after, .. } => {
                Some(give_way_after.unwrap_or(request_timeout))
            }
        }
    }

    /// Has lookups on `root` pass `provider` over from `now`: it did not
    /// answer in time where the next server did.
    pub(crate) fn pass_over(&mut self, root: &Path, provider: &InstalledProvider, now: Instant) {
        self.slow.insert(root_key(root, provider), now);
    }

    /// `providers` as a lookup on `root` tries them at `now`: those to
    /// ask, in order, then those passed over. When every one is passed
    /// over all are asked, the slowest answer being better than none.
    pub(crate) fn askable(
        &mut self,
        root: &Path,
        providers: Vec<InstalledProvider>,
        now: Instant,
    ) -> (Vec<InstalledProvider>, Vec<InstalledProvider>) {
        let when_slow = self.when_slow;
        self.slow
            .retain(|_, since| when_slow.passes_over(now.saturating_duration_since(*since)));
        let is_passed_over =
            |provider: &InstalledProvider| self.slow.contains_key(&root_key(root, provider));
        if providers.iter().all(is_passed_over) {
            return (providers, Vec::new());
        }
        providers
            .into_iter()
            .partition(|provider| !is_passed_over(provider))
    }

    /// The servers lookups pass over at `now`, for status reports.
    pub fn passed_over(&self, now: Instant) -> Vec<PassedOver> {
        let WhenSlow::Next {
            try_again_after, ..
        } = self.when_slow
        else {
            return Vec::new();
        };
        let mut passed_over: Vec<PassedOver> = self
            .slow
            .iter()
            .map(|(key, since)| (key, now.saturating_duration_since(*since)))
            .filter(|(_, since)| self.when_slow.passes_over(*since))
            .map(|(key, since)| PassedOver {
                provider_id: key.provider_id.clone(),
                root: key.root.clone(),
                since,
                tried_again_in: try_again_after.map(|after| after.saturating_sub(since)),
            })
            .collect();
        passed_over.sort_by(|left, right| {
            left.root
                .cmp(&right.root)
                .then(left.provider_id.cmp(&right.provider_id))
        });
        passed_over
    }

    /// Starts a server in the background, trying the providers in order,
    /// unless one of them is running or starting already. A lookup that
    /// needs one meanwhile waits for it instead of starting a second. A
    /// server lookups pass over is not started: the one they ask is.
    pub fn warm_up(&mut self, mut warm_up: WarmUp) {
        self.collect_arrivals(None);
        let root = PathBuf::from(&warm_up.snapshot.snapshot.root);
        let providers = std::mem::take(&mut warm_up.providers);
        (warm_up.providers, _) = self.askable(&root, providers, Instant::now());
        let keys: Vec<SessionKey> = warm_up
            .providers
            .iter()
            .map(|provider| session_key(&warm_up.snapshot, provider))
            .collect();
        let is_served =
            |key: &SessionKey| self.sessions.contains_key(key) || self.warming.contains(key);
        if keys.is_empty() || keys.iter().any(is_served) {
            return;
        }
        for key in &keys {
            // Starting again is how a failed start is retried.
            self.failed_warm_ups.remove(key);
            self.warming.insert(key.clone());
        }
        let arrivals = self.arrivals.clone();
        let is_long_lived = self.is_long_lived;
        thread::spawn(move || warm_in_order(&warm_up, keys, &arrivals, is_long_lived));
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
                failure: None,
            })
            .collect();
        statuses.extend(self.warming.iter().map(|key| SessionStatus {
            provider_id: key.provider_id.clone(),
            root: key.root.clone(),
            idle: Duration::ZERO,
            state: ServerState::Starting,
            memory_bytes: None,
            failure: None,
        }));
        statuses.extend(
            self.failed_warm_ups
                .iter()
                .map(|(key, error)| SessionStatus {
                    provider_id: key.provider_id.clone(),
                    root: key.root.clone(),
                    idle: Duration::ZERO,
                    state: ServerState::Failed,
                    memory_bytes: None,
                    failure: Some(crate::lookup::describe(error)),
                }),
        );
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

    /// Waits until one of `providers` serves the snapshot's root, or every
    /// one of them has failed to start; their statuses then say which.
    pub fn wait_for(&mut self, snapshot: &StoredSnapshot, providers: &[InstalledProvider]) {
        for provider in providers {
            let key = session_key(snapshot, provider);
            self.collect_arrivals(Some(&key));
            if self.sessions.contains_key(&key) {
                return;
            }
        }
    }

    /// Stops the servers on `root` whose provider `is_chosen` accepts, and
    /// forgets their failed starts; the ids of those stopped, sorted. A
    /// server still starting is left to finish.
    pub fn stop_on(&mut self, root: &Path, is_chosen: &dyn Fn(&str) -> bool) -> Vec<String> {
        self.collect_arrivals(None);
        let matches = |key: &SessionKey| key.root == root && is_chosen(&key.provider_id);
        let chosen: Vec<SessionKey> = self
            .sessions
            .keys()
            .filter(|key| matches(key))
            .cloned()
            .collect();
        self.failed_warm_ups.retain(|key, _| !matches(key));
        let mut stopped = Vec::new();
        for key in chosen {
            stopped.push(key.provider_id.clone());
            self.stop(&key);
        }
        stopped.sort();
        stopped
    }

    /// Stops the servers whose root directory no longer exists, such as a
    /// worktree deleted after use, and forgets their failed starts.
    pub fn stop_vanished(&mut self) {
        self.collect_arrivals(None);
        let vanished: Vec<SessionKey> = self
            .sessions
            .keys()
            .filter(|key| !key.root.exists())
            .cloned()
            .collect();
        self.failed_warm_ups.retain(|key, _| key.root.exists());
        self.failed_starts.retain(|key, _| key.root.exists());
        for key in vanished {
            self.stop(&key);
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
        // Before waiting for a server that is starting, or asking one.
        if self.is_interrupted() {
            return Err(PoolError::Interrupted);
        }
        let key = root_key(view.root, provider);
        self.collect_arrivals(Some(&key));
        if let Some(error) = self.failed_warm_ups.remove(&key) {
            return Err(PoolError::Start(error));
        }
        // One started since, as `lemma daemon lsp start` may have, serves.
        if !self.sessions.contains_key(&key)
            && let Some(message) = self.failed_start(&key)
        {
            return Err(PoolError::Start(ProviderError::CouldNotStart { message }));
        }
        // A paused server cannot read what sync sends it, so resume first.
        if let Some(pooled) = self.sessions.get_mut(&key) {
            pooled.wake();
        }
        // Syncing may drop a session that could not be told about changes.
        self.sync(&key, view.fingerprint, files)?;
        match self.sessions.entry(key) {
            Entry::Occupied(entry) => {
                log::debug!(
                    "reusing the running {} on {}",
                    provider.definition.id,
                    view.root.display()
                );
                let pooled = entry.into_mut();
                pooled.last_used = Instant::now();
                Ok(&mut pooled.session)
            }
            Entry::Vacant(entry) => {
                log::info!(
                    "starting {} ({}) on {}",
                    provider.definition.id,
                    provider.executable.display(),
                    view.root.display()
                );
                let started = Instant::now();
                let started_session =
                    start(view, files, provider, request_timeout, self.is_long_lived);
                if let Err(PoolError::Start(error)) = &started_session {
                    let failed = (Instant::now(), crate::lookup::describe(error));
                    self.failed_starts.insert(entry.key().clone(), failed);
                }
                let pooled = started_session?;
                pooled.session.interrupt_when(self.interrupt.clone());
                log::info!(
                    "{} started in {:.1}s",
                    provider.definition.id,
                    started.elapsed().as_secs_f64()
                );
                Ok(&mut entry.insert(pooled).session)
            }
        }
    }

    /// Why the server for `key` could not start, if it failed recently
    /// enough not to be tried again: within [`START_AGAIN_AFTER`] in a
    /// pool that stays, at any time in a pool for one lookup.
    fn failed_start(&mut self, key: &SessionKey) -> Option<String> {
        let (failed_at, message) = self.failed_starts.get(key)?;
        if self.is_long_lived && failed_at.elapsed() >= START_AGAIN_AFTER {
            self.failed_starts.remove(key);
            return None;
        }
        Some(message.clone())
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
        provider.search_path.as_deref(),
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
/// Tries `warm_up`'s providers in order, each only if the one before
/// could not start, reporting each by its key; those after the one that
/// serves are reported as not needed.
fn warm_in_order(
    warm_up: &WarmUp,
    keys: Vec<SessionKey>,
    arrivals: &mpsc::Sender<Arrival>,
    is_long_lived: bool,
) {
    let mut is_done = false;
    for (provider, key) in warm_up.providers.iter().zip(keys) {
        let outcome = if is_done {
            WarmUpOutcome::Abandoned
        } else {
            warm_session(warm_up, provider, is_long_lived)
        };
        is_done = !matches!(outcome, WarmUpOutcome::StartFailed(_));
        // The pool may have been dropped; the session then stops with it.
        let _ = arrivals.send(Arrival { key, outcome });
    }
}

fn warm_session(
    warm_up: &WarmUp,
    provider: &InstalledProvider,
    is_long_lived: bool,
) -> WarmUpOutcome {
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
        provider,
        warm_up.request_timeout,
        is_long_lived,
    );
    let mut pooled = match started {
        Ok(pooled) => pooled,
        Err(PoolError::Start(error)) => return WarmUpOutcome::StartFailed(error),
        Err(PoolError::Store(_) | PoolError::Files(_) | PoolError::Interrupted) => {
            return WarmUpOutcome::Abandoned;
        }
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

/// What a lookup does when a server does not answer in the time the lookup
/// was given: a point between waiting for the first installed server
/// every time and leaving it for the next one for good.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum WhenSlow {
    /// The lookup fails and says so: another server answering unasked
    /// would hide that the first could not.
    #[default]
    Report,
    /// The next installed server for the language is asked. Once one
    /// answers, the slow server is passed over on that root until
    /// `try_again_after` has gone by: zero tries it first every time,
    /// `None` leaves it for as long as the pool lives.
    Next {
        /// How long a server with another after it may take over one
        /// question, whoever started it; `None` for as long as the lookup
        /// waits for any server.
        give_way_after: Option<Duration>,
        /// How long the slow server is passed over.
        try_again_after: Option<Duration>,
    },
}

impl WhenSlow {
    /// Whether a server that was too slow `since` ago is still passed
    /// over.
    fn passes_over(self, since: Duration) -> bool {
        match self {
            Self::Report => false,
            Self::Next {
                try_again_after, ..
            } => try_again_after.is_none_or(|after| since < after),
        }
    }
}

/// A server lookups pass over, for status reports.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PassedOver {
    /// The provider passed over.
    pub provider_id: String,
    /// The workspace root it is passed over on.
    pub root: PathBuf,
    /// How long ago it did not answer in time.
    pub since: Duration,
    /// How long until a lookup tries it again; `None` when none will.
    pub tried_again_in: Option<Duration>,
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
    /// Its last start failed; the status says why. Starting it again, or
    /// a lookup that needs it, tries again.
    Failed,
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
    /// Why its last start failed, for a failed server.
    pub failure: Option<String>,
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
    fn a_server_that_could_not_start_is_not_started_again_at_once() {
        let registry = lemma_providers::ProviderRegistry::builtin().unwrap();
        let provider = InstalledProvider {
            definition: registry.definitions()[0].clone(),
            executable: PathBuf::from("/bin/server-that-is-not-there"),
            search_path: None,
        };
        let view = RootView {
            root: Path::new("/nowhere"),
            fingerprint: "files",
        };
        let mut files = || Ok(FileHashes::new());
        let mut pool = SessionPool::long_lived();
        let timeout = Duration::from_secs(1);
        let said = |started: Result<&mut ProviderSession, PoolError>| match started {
            Err(PoolError::Start(error)) => {
                let is_remembered = matches!(error, ProviderError::CouldNotStart { .. });
                (is_remembered, crate::lookup::describe(&error))
            }
            _ => panic!("the server is not there"),
        };

        let first = said(pool.session_at(&view, &mut files, &provider, timeout));
        let second = said(pool.session_at(&view, &mut files, &provider, timeout));

        assert!(!first.0);
        assert!(second.0);
        assert_eq!(second.1, first.1);
    }

    /// Two providers of one language as a lookup would try them, and a
    /// pool that asks the next when one is slow.
    fn two_providers(try_again_after: Option<Duration>) -> (Vec<InstalledProvider>, SessionPool) {
        let registry = lemma_providers::ProviderRegistry::builtin().unwrap();
        let providers = registry.definitions()[..2]
            .iter()
            .map(|definition| InstalledProvider {
                definition: definition.clone(),
                executable: PathBuf::from("/bin/server"),
                search_path: None,
            })
            .collect();
        let mut pool = SessionPool::long_lived();
        pool.set_when_slow(WhenSlow::Next {
            give_way_after: None,
            try_again_after,
        });
        (providers, pool)
    }

    fn ids(providers: &[InstalledProvider]) -> Vec<&str> {
        providers
            .iter()
            .map(|provider| provider.definition.id.as_str())
            .collect()
    }

    const MINUTE: Duration = Duration::from_secs(60);

    #[test]
    fn a_slow_server_is_passed_over_until_it_is_to_be_tried_again() {
        let (providers, mut pool) = two_providers(Some(30 * MINUTE));
        let (first, second) = (providers[0].clone(), providers[1].clone());
        let root = Path::new("/repository");
        let slow_at = Instant::now();
        pool.pass_over(root, &first, slow_at);

        let soon = pool.askable(root, providers.clone(), slow_at + MINUTE);
        let elsewhere = pool.askable(Path::new("/other"), providers.clone(), slow_at + MINUTE);
        let later = pool.askable(root, providers.clone(), slow_at + 30 * MINUTE);

        assert_eq!(ids(&soon.0), [second.definition.id.as_str()]);
        assert_eq!(ids(&soon.1), [first.definition.id.as_str()]);
        assert_eq!(ids(&elsewhere.0), ids(&providers));
        assert_eq!(ids(&later.0), ids(&providers));
        assert_eq!(ids(&later.1), [] as [&str; 0]);
    }

    #[test]
    fn how_long_a_slow_server_is_passed_over_runs_from_never_to_for_good() {
        let root = Path::new("/repository");
        let slow_at = Instant::now();
        let asked_first_after = |try_again_after: Option<Duration>, after: Duration| {
            let (providers, mut pool) = two_providers(try_again_after);
            pool.pass_over(root, &providers[0], slow_at);
            let (asked, _) = pool.askable(root, providers.clone(), slow_at + after);
            asked[0].definition.id == providers[0].definition.id
        };

        assert!(asked_first_after(Some(Duration::ZERO), Duration::ZERO));
        assert!(!asked_first_after(Some(MINUTE), Duration::ZERO));
        assert!(!asked_first_after(None, 24 * 60 * MINUTE));
    }

    #[test]
    fn a_server_gives_way_after_the_lookups_wait_unless_a_time_is_set() {
        let mut pool = SessionPool::long_lived();
        let next = |give_way_after| WhenSlow::Next {
            give_way_after,
            try_again_after: None,
        };

        let reported = pool.gives_way_after(MINUTE);
        pool.set_when_slow(next(None));
        let unset = pool.gives_way_after(MINUTE);
        pool.set_when_slow(next(Some(Duration::from_secs(20))));
        let set = pool.gives_way_after(MINUTE);

        assert_eq!(reported, None);
        assert_eq!(unset, Some(MINUTE));
        assert_eq!(set, Some(Duration::from_secs(20)));
    }

    #[test]
    fn servers_all_passed_over_are_all_asked() {
        let (providers, mut pool) = two_providers(None);
        let root = Path::new("/repository");
        let slow_at = Instant::now();
        for provider in &providers {
            pool.pass_over(root, provider, slow_at);
        }

        let (asked, passed_over) = pool.askable(root, providers.clone(), slow_at + MINUTE);

        assert_eq!(ids(&asked), ids(&providers));
        assert_eq!(ids(&passed_over), [] as [&str; 0]);
    }

    #[test]
    fn a_change_of_setting_has_every_server_tried_again() {
        let (providers, mut pool) = two_providers(None);
        let root = Path::new("/repository");
        let slow_at = Instant::now();
        pool.pass_over(root, &providers[0], slow_at);

        pool.set_when_slow(WhenSlow::Next {
            give_way_after: None,
            try_again_after: Some(MINUTE),
        });
        let (asked, _) = pool.askable(root, providers.clone(), slow_at);

        assert_eq!(ids(&asked), ids(&providers));
    }

    #[test]
    fn status_lists_a_server_passed_over_with_when_it_is_tried_again() {
        let (providers, mut pool) = two_providers(Some(30 * MINUTE));
        let slow_at = Instant::now();
        pool.pass_over(Path::new("/repository"), &providers[0], slow_at);

        let passed_over = pool.passed_over(slow_at + 10 * MINUTE);

        assert_eq!(
            passed_over,
            [PassedOver {
                provider_id: providers[0].definition.id.clone(),
                root: PathBuf::from("/repository"),
                since: 10 * MINUTE,
                tried_again_in: Some(20 * MINUTE),
            }]
        );
        assert_eq!(pool.passed_over(slow_at + 30 * MINUTE), []);
    }

    #[test]
    fn a_pool_told_to_stop_asks_no_server() {
        let registry = lemma_providers::ProviderRegistry::builtin().unwrap();
        let provider = InstalledProvider {
            definition: registry.definitions()[0].clone(),
            executable: PathBuf::from("/bin/server-that-is-not-there"),
            search_path: None,
        };
        let view = RootView {
            root: Path::new("/nowhere"),
            fingerprint: "files",
        };
        let mut files = || Ok(FileHashes::new());
        let mut pool = SessionPool::long_lived();
        pool.interrupt_when(Some(std::sync::Arc::new(|| true)));

        let timeout = Duration::from_secs(1);
        let is_stopped = matches!(
            pool.session_at(&view, &mut files, &provider, timeout),
            Err(PoolError::Interrupted)
        );
        pool.interrupt_when(None);
        let is_asked = matches!(
            pool.session_at(&view, &mut files, &provider, timeout),
            Err(PoolError::Start(_))
        );

        assert!(is_stopped);
        assert!(is_asked);
    }

    #[test]
    fn identical_files_need_no_changes() {
        let files = hashes(&[("a.py", "1")]);

        assert_eq!(changes_between(&files, &files), []);
    }
}
