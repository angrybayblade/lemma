//! Knowing which paths changed since the last index, without walking the
//! whole tree.
//!
//! The operating system reports changes (`FSEvents` on macOS, `inotify` on
//! Linux). Those reports arrive asynchronously, so a file saved just before
//! `lemma index` might not have been reported yet. Before each index the
//! watcher writes a marker file into a directory it also watches and waits
//! for that marker's own event: reports arrive in order, so once the marker
//! is seen, every earlier change has been too.
//!
//! On Linux each watched directory takes one of a limited number of
//! inotify watches per user, so only the directories discovery enters are
//! watched, one by one: not `node_modules`, virtual environments or build
//! output, which can be nine in ten directories of a JavaScript or Python
//! project. A directory created later is watched when changes are next
//! collected, before the index walks it; until then its creation, reported
//! by its parent, makes the index walk it in full. Elsewhere one recursive
//! watch costs nothing per directory, and is used instead.

use std::collections::HashSet;
use std::fs;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Condvar, Mutex, PoisonError};
use std::time::{Duration, Instant};

use notify::event::{AccessKind, AccessMode};
use notify::{Event, EventKind, RecommendedWatcher, RecursiveMode, Watcher};

use crate::discover;
use crate::selection::Selection;

/// How long to wait for the marker's event before giving up and walking
/// the whole tree instead.
const BARRIER_LIMIT: Duration = Duration::from_millis(500);

/// What changed since the last call to [`Watch::changes`].
#[derive(Debug, Default)]
pub struct Observed {
    /// Changed paths, absolute and canonical.
    pub paths: HashSet<PathBuf>,
    /// The watcher lost events or could not confirm it saw them all; only a
    /// full walk is safe.
    pub needs_full_walk: bool,
}

/// Whether one watch per directory is needed, because the platform's
/// watcher (inotify) has no recursive watch of its own.
const WATCHES_EACH_DIRECTORY: bool = cfg!(any(target_os = "linux", target_os = "android"));

/// A running watcher on one directory.
pub struct Watch {
    watcher: RecommendedWatcher,
    shared: Arc<Shared>,
    barrier_dir: PathBuf,
    next_barrier: u64,
    root: PathBuf,
    selection: Selection,
    /// The directories watched one by one; empty where one recursive watch
    /// covers the root.
    watched: HashSet<PathBuf>,
    /// Why some change may go unreported, once a later directory could not
    /// be watched; every collection then asks for a full walk.
    degraded: Option<String>,
    /// Directories under the root left to other watchers.
    excluded: Vec<PathBuf>,
}

/// What a watcher covers, for status.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum WatchReport {
    /// Every change is reported.
    Watching {
        /// How many directories are watched one by one, or `None` where one
        /// recursive watch covers the root.
        directories: Option<usize>,
    },
    /// Changes may go unreported, so every index walks the whole tree.
    Walking {
        /// Why, such as the limit on watches being reached.
        reason: String,
    },
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
    /// Starts watching `root`, which must be canonical, and the directories
    /// `selection` enters below it. The error says why the platform's
    /// watcher could not be set up, such as the limit on watches; callers
    /// then walk every time.
    pub fn start(root: &Path, selection: &Selection) -> Result<Self, String> {
        Self::start_without(root, selection, &[])
    }

    /// [`Watch::start`], leaving the directories `excluded` (canonical, under
    /// `root`) and what is below them to other watchers, such as the
    /// repositories inside a workspace's directory.
    pub fn start_without(
        root: &Path,
        selection: &Selection,
        excluded: &[PathBuf],
    ) -> Result<Self, String> {
        let barrier_dir = barrier_directory().ok_or("could not create a directory for markers")?;
        let shared = Arc::new(Shared {
            state: Mutex::new(State::default()),
            changed: Condvar::new(),
        });
        let handler_shared = Arc::clone(&shared);
        let handler_barrier = barrier_dir.clone();
        let watcher = notify::recommended_watcher(move |event: notify::Result<Event>| {
            record(&handler_shared, &handler_barrier, event);
        })
        .map_err(|error| describe(&error))?;
        let mut watch = Self {
            watcher,
            shared,
            barrier_dir,
            next_barrier: 0,
            root: root.to_owned(),
            selection: selection.clone(),
            watched: HashSet::new(),
            degraded: None,
            excluded: excluded.to_vec(),
        };
        let barrier = watch.barrier_dir.clone();
        watch.add(&barrier, RecursiveMode::NonRecursive)?;
        if !WATCHES_EACH_DIRECTORY {
            watch.add(root, RecursiveMode::Recursive)?;
            return Ok(watch);
        }
        watch.watch_tree(root)?;
        watch.watch_git_metadata();
        Ok(watch)
    }

    /// What this watcher covers.
    pub fn report(&self) -> WatchReport {
        if let Some(reason) = &self.degraded {
            return WatchReport::Walking {
                reason: reason.clone(),
            };
        }
        let directories = WATCHES_EACH_DIRECTORY.then_some(self.watched.len());
        WatchReport::Watching { directories }
    }

    /// Watches `top` and every directory below it that discovery enters.
    fn watch_tree(&mut self, top: &Path) -> Result<(), String> {
        let directories =
            discover::directories_under(&self.root, top, &self.selection, &self.excluded);
        for directory in directories {
            if self.watched.contains(&directory) {
                continue;
            }
            self.add(&directory, RecursiveMode::NonRecursive)?;
            self.watched.insert(directory);
        }
        Ok(())
    }

    /// Watches what tells that the checked-out commit moved: `.git` itself
    /// (`HEAD`, the index, packed refs) and the directories of loose refs.
    /// Best effort: a commit Lemma misses is asked of Git on a full walk.
    fn watch_git_metadata(&mut self) {
        let git = self.root.join(".git");
        if !git.is_dir() {
            return;
        }
        let mut pending = vec![git.clone(), git.join("refs")];
        while let Some(directory) = pending.pop() {
            if self.add(&directory, RecursiveMode::NonRecursive).is_err() {
                continue;
            }
            self.watched.insert(directory.clone());
            if directory == git {
                continue;
            }
            let Ok(entries) = fs::read_dir(&directory) else {
                continue;
            };
            let children = entries.filter_map(Result::ok).map(|entry| entry.path());
            pending.extend(children.filter(|path| path.is_dir()));
        }
    }

    fn add(&mut self, path: &Path, mode: RecursiveMode) -> Result<(), String> {
        self.watcher
            .watch(path, mode)
            .map_err(|error| format!("could not watch {}: {}", path.display(), describe(&error)))
    }

    /// Watches the directories among `paths` that appeared since the last
    /// collection, and forgets those that are gone. A failure leaves the
    /// watcher degraded: from then on every collection asks for a full walk.
    fn follow_directories(&mut self, paths: &HashSet<PathBuf>) {
        if !WATCHES_EACH_DIRECTORY || self.degraded.is_some() {
            return;
        }
        for path in paths {
            if let Err(reason) = self.follow(path) {
                log::warn!(
                    "watching {}: {reason}; walking instead",
                    self.root.display()
                );
                self.degraded = Some(reason);
                return;
            }
        }
    }

    /// Watches `path` if it is a new directory under the root; forgets it,
    /// and what was below it, if it is gone.
    fn follow(&mut self, path: &Path) -> Result<(), String> {
        if !path.exists() {
            self.watched.retain(|watched| !watched.starts_with(path));
            return Ok(());
        }
        if !path.is_dir() || self.watched.contains(path) || !path.starts_with(&self.root) {
            return Ok(());
        }
        self.watch_tree(path)
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

        let mut observed = std::mem::take(&mut lock(&self.shared.state).observed);
        observed.needs_full_walk |= !is_confirmed;
        // Before the index walks the new directories, so a file written in
        // one after this is reported, and one written before is walked.
        self.follow_directories(&observed.paths);
        observed.needs_full_walk |= self.degraded.is_some();
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
            let paths = if is_change(event.kind) {
                event.paths
            } else {
                Vec::new()
            };
            for path in paths {
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

/// Whether an event can mean a file's contents or presence changed.
///
/// Opening or reading a file is reported too, and Git reads files such as
/// `.git/info/exclude` on every `git status`, including Lemma's own: counted
/// as changes, those reads would force a full walk on every run.
fn is_change(kind: EventKind) -> bool {
    match kind {
        EventKind::Access(AccessKind::Close(AccessMode::Write)) => true,
        EventKind::Access(_) => false,
        _ => true,
    }
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

/// The watcher's error in words, naming the limit when that is the cause.
fn describe(error: &notify::Error) -> String {
    if matches!(error.kind, notify::ErrorKind::MaxFilesWatch) {
        return "the limit on watches per user is reached (fs.inotify.max_user_watches)".to_owned();
    }
    error.to_string()
}

fn lock<T>(mutex: &Mutex<T>) -> std::sync::MutexGuard<'_, T> {
    mutex.lock().unwrap_or_else(PoisonError::into_inner)
}

#[cfg(test)]
mod tests {
    use notify::event::{CreateKind, ModifyKind};

    use super::*;

    #[cfg(any(target_os = "linux", target_os = "android"))]
    #[test]
    fn a_selection_of_what_git_lists_watches_hidden_directories_but_not_lemmas_own() {
        let directory = tempfile::tempdir().unwrap();
        let root = directory.path().canonicalize().unwrap();
        fs::create_dir_all(root.join(".github")).unwrap();
        fs::create_dir_all(root.join(".lemma")).unwrap();
        let mut watch = Watch::start(&root, &Selection::everything_git_lists()).unwrap();

        fs::write(root.join(".github/spec.json"), "{}").unwrap();
        fs::write(root.join(".lemma/index.ls"), "x").unwrap();
        let observed = watch.changes();

        assert!(observed.paths.contains(&root.join(".github/spec.json")));
        assert!(!observed.paths.contains(&root.join(".lemma/index.ls")));
    }

    #[cfg(any(target_os = "linux", target_os = "android"))]
    #[test]
    fn an_index_selection_leaves_hidden_directories_unwatched() {
        let directory = tempfile::tempdir().unwrap();
        let root = directory.path().canonicalize().unwrap();
        fs::create_dir_all(root.join(".github")).unwrap();
        let mut watch = Watch::start(&root, &Selection::everything()).unwrap();

        fs::write(root.join(".github/spec.json"), "{}").unwrap();
        let observed = watch.changes();

        assert!(!observed.paths.contains(&root.join(".github/spec.json")));
    }

    #[test]
    fn reading_a_file_is_not_a_change() {
        let read = EventKind::Access(AccessKind::Close(AccessMode::Read));
        let opened = EventKind::Access(AccessKind::Open(AccessMode::Any));

        assert!(!is_change(read));
        assert!(!is_change(opened));
    }

    #[test]
    fn writing_creating_and_modifying_are_changes() {
        let written = EventKind::Access(AccessKind::Close(AccessMode::Write));
        let created = EventKind::Create(CreateKind::File);
        let modified = EventKind::Modify(ModifyKind::Any);

        assert!(is_change(written));
        assert!(is_change(created));
        assert!(is_change(modified));
    }
}
