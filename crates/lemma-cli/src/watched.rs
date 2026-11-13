//! A workspace's repositories, watched for changes, and what a long-lived
//! process keeps of what it read from them.
//!
//! Sentry answers checks from it (`.specs/14-sentry.md`). Nothing is
//! evaluated here: a change only drops what was kept of the files it
//! touched, so whoever reads next reads those again and nothing else.

use std::collections::{BTreeSet, HashSet};
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::{Duration, Instant};

/// How often a process that collects changes between asks does so.
pub(crate) const BATCH_INTERVAL: Duration = Duration::from_millis(200);
/// How long after a repository's first unprepared change it is prepared,
/// even if changes keep coming.
const PREPARE_WITHIN: Duration = Duration::from_secs(2);
/// More changed paths than this are a bulk state: too many to parse one by
/// one, so a batch only lists the repository again.
const BULK_PATHS: usize = 1000;

/// A workspace's repositories, each watched, and what is kept of what was
/// read from them: dropped, repository by repository, as their files
/// change.
pub(crate) struct WatchedWorkspace {
    watched: Vec<Watched>,
    warm: Arc<lemma_graph::Warm>,
}

/// The paths that changed together in one repository.
pub(crate) struct Batch {
    /// The repository's directory.
    pub repository: String,
    /// How many paths changed.
    pub paths: usize,
}

/// Changes to one repository that have settled, for reading ahead.
pub(crate) struct Settled {
    /// The repository's directory as a check names it.
    pub repository: PathBuf,
    /// The changed paths, relative to it; none in a bulk state.
    pub paths: Vec<String>,
}

impl WatchedWorkspace {
    /// Starts watching the repositories of the workspace whose description
    /// is `manifest`, before anything is read from them, so no change made
    /// after a read goes unseen.
    pub(crate) fn start(manifest: &Path) -> anyhow::Result<Self> {
        Ok(Self {
            watched: watch_repositories(manifest)?,
            warm: Arc::default(),
        })
    }

    /// What is kept of what was read.
    pub(crate) fn warm(&self) -> &Arc<lemma_graph::Warm> {
        &self.warm
    }

    /// The repositories watched.
    pub(crate) fn repositories(&self) -> &[Watched] {
        &self.watched
    }

    /// Drops what is kept of what changed in each repository; the changes,
    /// by repository. `confirming` waits for every watcher's barrier, so a
    /// change made just before the call counts, and leaves nothing to
    /// prepare, since whoever asks reads what it needs; without it only
    /// repositories with reported changes are looked at.
    pub(crate) fn collect(&mut self, confirming: bool) -> Vec<Batch> {
        let mut batches = Vec::new();
        for watched in &mut self.watched {
            if watched.watch.is_none() {
                // Unwatched: anything may have changed.
                self.warm.forget_working(&watched.root);
                watched.generation += 1;
                continue;
            }
            if let Some(paths) = watched.take_changes(&self.warm, confirming) {
                batches.push(Batch {
                    repository: watched.root.display().to_string(),
                    paths,
                });
            }
            if confirming {
                watched.unprepared = None;
            }
        }
        batches
    }

    /// Takes the changes whose repositories have settled as of `now` and
    /// that Git has let go of.
    pub(crate) fn take_settled(&mut self, now: Instant) -> Vec<Settled> {
        self.watched
            .iter_mut()
            .filter_map(|watched| {
                let unprepared = watched.take_settled(now)?;
                Some(Settled {
                    repository: watched.spelled.clone(),
                    paths: unprepared.paths.into_iter().collect(),
                })
            })
            .collect()
    }
}

/// A watcher on each repository of the workspace whose description is
/// `manifest`.
fn watch_repositories(manifest: &Path) -> anyhow::Result<Vec<Watched>> {
    let options = lemma_graph::CheckOptions {
        change: None,
        level: None,
        cache: lemma_graph::repos::cache_directory(),
        cloning: lemma_graph::repos::Cloning::Forbidden,
        explain: false,
        relations: Vec::new(),
        facts_cache: None,
        evaluation: lemma_verify::Evaluation::Demand,
        list_links: false,
        run_checkers: false,
        runs_base_checkers: false,
        select: None,
        warm: None,
        environment: None,
        runtime: None,
    };
    let spelled = lemma_graph::repository_roots(manifest, &options)?;
    let roots: Vec<PathBuf> = spelled
        .iter()
        .map(|root| root.canonicalize().unwrap_or_else(|_| root.clone()))
        .collect();
    let watched = roots
        .iter()
        .zip(spelled)
        .map(|(root, spelled)| {
            // A workspace's directory holds its repositories, each
            // watched on its own.
            let inner: Vec<PathBuf> = roots
                .iter()
                .filter(|other| *other != root && other.starts_with(root))
                .cloned()
                .collect();
            watch(root.clone(), spelled, &inner)
        })
        .collect();
    Ok(watched)
}

/// One watched repository.
pub(crate) struct Watched {
    root: PathBuf,
    /// The root as a check names it, which is how reads are kept.
    spelled: PathBuf,
    watch: Option<lemma_index::Watch>,
    /// Why it is not watched, when watching failed.
    failure: Option<String>,
    /// Bumped whenever a file a check can depend on may have changed.
    generation: u64,
    /// Bumped when only files no check reads changed. Those matter only to
    /// an answer that ran a behavior, whose checker can read anything.
    unread_generation: u64,
    /// Changes not prepared yet.
    unprepared: Option<Unprepared>,
}

/// A repository's changes since it was last prepared.
struct Unprepared {
    /// Changed paths, relative to the repository.
    paths: BTreeSet<String>,
    /// Whether there are too many to prepare one by one, or some were lost.
    is_bulk: bool,
    first: Instant,
    last: Instant,
}

impl Watched {
    /// The repository's directory as a check names it.
    pub(crate) fn spelled(&self) -> &Path {
        &self.spelled
    }

    /// How many times a file a check can depend on may have changed.
    pub(crate) fn generation(&self) -> u64 {
        self.generation
    }

    /// How many times only files no check reads changed.
    pub(crate) fn unread_generation(&self) -> u64 {
        self.unread_generation
    }

    /// Whether it is watched, and how, in a line for status.
    pub(crate) fn status_line(&self) -> String {
        let root = self.root.display();
        match (&self.watch, &self.failure) {
            (Some(watch), _) => match watch.report() {
                lemma_index::WatchReport::Watching {
                    directories: Some(count),
                } => format!("watching {root}: {count} directories"),
                lemma_index::WatchReport::Watching { directories: None } => {
                    format!("watching {root}")
                }
                lemma_index::WatchReport::Walking { reason } => {
                    format!("not watching {root}: {reason}")
                }
            },
            (None, failure) => format!(
                "not watching {root}: {}",
                failure.as_deref().unwrap_or("unknown")
            ),
        }
    }

    /// Takes the reported changes, dropping what is kept of the paths they
    /// touched; whether there were any, and how many paths.
    fn take_changes(&mut self, warm: &lemma_graph::Warm, confirming: bool) -> Option<usize> {
        let watch = self.watch.as_mut()?;
        if !confirming && !watch.has_pending() {
            return None;
        }
        let observed = watch.changes();
        if !observed.needs_full_walk && observed.paths.is_empty() {
            return None;
        }
        let changed = relative_paths(&self.root, &observed.paths);
        let is_unread = !observed.needs_full_walk
            && !changed.is_empty()
            && !changed
                .iter()
                .any(|path| warm.can_depend_on(&self.root, path));
        if is_unread {
            // No check reads them, but the repository's state counts them.
            warm.forget_state(&self.root);
            self.unread_generation += 1;
            return None;
        }
        if observed.needs_full_walk {
            warm.forget_working(&self.root);
        } else {
            warm.forget_paths(&self.root, &changed);
        }
        self.generation += 1;
        self.note_unprepared(changed, observed.needs_full_walk);
        Some(observed.paths.len())
    }

    fn note_unprepared(&mut self, changed: Vec<String>, is_lost: bool) {
        let now = Instant::now();
        let unprepared = self.unprepared.get_or_insert_with(|| Unprepared {
            paths: BTreeSet::new(),
            is_bulk: false,
            first: now,
            last: now,
        });
        unprepared.last = now;
        unprepared.paths.extend(changed);
        unprepared.is_bulk |= is_lost || unprepared.paths.len() > BULK_PATHS;
        if unprepared.is_bulk {
            unprepared.paths.clear();
        }
    }

    /// Takes the changes to prepare, once they have settled and Git has
    /// let go of the repository.
    fn take_settled(&mut self, now: Instant) -> Option<Unprepared> {
        let unprepared = self.unprepared.as_ref()?;
        let is_settled = now.duration_since(unprepared.last) >= BATCH_INTERVAL
            || now.duration_since(unprepared.first) >= PREPARE_WITHIN;
        if !is_settled || self.root.join(".git/index.lock").exists() {
            return None;
        }
        self.unprepared.take()
    }
}

/// `paths` relative to `root`, with `/`, those outside it and in `.git`
/// left out: the latter say nothing about a file's contents.
fn relative_paths(root: &Path, paths: &HashSet<PathBuf>) -> Vec<String> {
    let mut relative: Vec<String> = paths
        .iter()
        .filter_map(|path| path.strip_prefix(root).ok())
        .filter(|path| !path.starts_with(".git"))
        .map(|path| path.to_string_lossy().replace('\\', "/"))
        .collect();
    relative.sort();
    relative
}

/// A watcher for the repository at `root` (canonical), named `spelled` by
/// checks, leaving out the repositories `inner` to it, or why there is none.
fn watch(root: PathBuf, spelled: PathBuf, inner: &[PathBuf]) -> Watched {
    let selection = lemma_index::Selection::everything_git_lists();
    let started = lemma_index::Watch::start_without(&root, &selection, inner);
    let (watch, failure) = match started {
        Ok(watch) => (Some(watch), None),
        Err(reason) => {
            log::warn!("not watching {}: {reason}", root.display());
            (None, Some(reason))
        }
    };
    Watched {
        root,
        spelled,
        watch,
        failure,
        generation: 0,
        unread_generation: 0,
        unprepared: None,
    }
}
