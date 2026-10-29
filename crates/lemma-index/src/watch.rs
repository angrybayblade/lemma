//! Knowing which paths changed since the last index, without walking the
//! whole tree.
//!
//! The operating system reports changes (`FSEvents` on macOS, `inotify` on
//! Linux). Those reports arrive asynchronously, so a file saved just before
//! `lemma index` might not have been reported yet. Before each index the
//! watcher writes a marker file into a directory it also watches and waits
//! for that marker's own event: reports arrive in order, so once the marker
//! is seen, every earlier change has been too.

use std::collections::HashSet;
use std::fs;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Condvar, Mutex, PoisonError};
use std::time::{Duration, Instant};

use notify::{Event, RecommendedWatcher, RecursiveMode, Watcher};

/// How long to wait for the marker's event before giving up and walking
/// the whole tree instead.
const BARRIER_LIMIT: Duration = Duration::from_millis(500);

/// What changed since the last call to [`Watch::changes`].
#[derive(Debug, Default)]
pub(crate) struct Observed {
    /// Changed paths, absolute and canonical.
    pub paths: HashSet<PathBuf>,
    /// The watcher lost events or could not confirm it saw them all; only a
    /// full walk is safe.
    pub needs_full_walk: bool,
}

/// A running watcher on one directory.
pub(crate) struct Watch {
    _watcher: RecommendedWatcher,
    shared: Arc<Shared>,
    barrier_dir: PathBuf,
    next_barrier: u64,
}

struct Shared {
    state: Mutex<State>,
    changed: Condvar,
}

#[derive(Default)]
struct State {
    observed: Observed,
    /// The newest marker whose event has arrived.
    barrier_seen: u64,
}

impl Watch {
    /// Starts watching `root`, which must be canonical. `None` if the
    /// platform's watcher could not be set up; callers then walk every time.
    pub fn start(root: &Path) -> Option<Self> {
        let barrier_dir = barrier_directory()?;
        let shared = Arc::new(Shared {
            state: Mutex::new(State::default()),
            changed: Condvar::new(),
        });
        let handler_shared = Arc::clone(&shared);
        let handler_barrier = barrier_dir.clone();
        let mut watcher = notify::recommended_watcher(move |event: notify::Result<Event>| {
            record(&handler_shared, &handler_barrier, event);
        })
        .ok()?;
        watcher.watch(root, RecursiveMode::Recursive).ok()?;
        watcher
            .watch(&barrier_dir, RecursiveMode::NonRecursive)
            .ok()?;
        Some(Self {
            _watcher: watcher,
            shared,
            barrier_dir,
            next_barrier: 0,
        })
    }

    /// Whether any change has been reported since the last call to
    /// [`Watch::changes`]. Does not wait for late reports.
    pub fn has_pending(&self) -> bool {
        let state = lock(&self.shared.state);
        !state.observed.paths.is_empty() || state.observed.needs_full_walk
    }

    /// Every change reported since the last call, once all changes made
    /// before this call are known to have been reported.
    pub fn changes(&mut self) -> Observed {
        self.next_barrier += 1;
        let barrier = self.next_barrier;
        let marker = self.barrier_dir.join(barrier.to_string());
        let is_confirmed = fs::write(&marker, b"").is_ok() && self.wait_for(barrier);
        let _ = fs::remove_file(&marker);

        let mut state = lock(&self.shared.state);
        let mut observed = std::mem::take(&mut state.observed);
        observed.needs_full_walk |= !is_confirmed;
        observed
    }

    fn wait_for(&self, barrier: u64) -> bool {
        let deadline = Instant::now() + BARRIER_LIMIT;
        let mut state = lock(&self.shared.state);
        while state.barrier_seen < barrier {
            let now = Instant::now();
            if now >= deadline {
                return false;
            }
            state = self
                .shared
                .changed
                .wait_timeout(state, deadline - now)
                .unwrap_or_else(PoisonError::into_inner)
                .0;
        }
        true
    }
}

impl Drop for Watch {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.barrier_dir);
    }
}

/// Records one report from the operating system.
fn record(shared: &Shared, barrier_dir: &Path, event: notify::Result<Event>) {
    let mut state = lock(&shared.state);
    match event {
        Err(_) => state.observed.needs_full_walk = true,
        Ok(event) => {
            if event.need_rescan() {
                state.observed.needs_full_walk = true;
            }
            for path in event.paths {
                match barrier_number(barrier_dir, &path) {
                    Some(number) => state.barrier_seen = state.barrier_seen.max(number),
                    None => {
                        state.observed.paths.insert(path);
                    }
                }
            }
        }
    }
    shared.changed.notify_all();
}

/// The marker number, if `path` is one of this watcher's markers.
fn barrier_number(barrier_dir: &Path, path: &Path) -> Option<u64> {
    path.strip_prefix(barrier_dir).ok()?.to_str()?.parse().ok()
}

/// A fresh, canonical directory for markers. Canonical because the
/// operating system reports canonical paths, such as `/private/var/...` for
/// `/var/...` on macOS.
fn barrier_directory() -> Option<PathBuf> {
    // The process id and the time keep several watchers apart.
    let started = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default()
        .as_nanos();
    let directory =
        std::env::temp_dir().join(format!("lemma-watch-{}-{started}", std::process::id()));
    fs::create_dir_all(&directory).ok()?;
    directory.canonicalize().ok()
}

fn lock<T>(mutex: &Mutex<T>) -> std::sync::MutexGuard<'_, T> {
    mutex.lock().unwrap_or_else(PoisonError::into_inner)
}
