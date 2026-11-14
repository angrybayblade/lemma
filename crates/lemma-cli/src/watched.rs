//! A workspace's repositories, watched for changes, and what a long-lived
//! process keeps of what it read from them.
//!
//! Sentry answers checks from it (`.specs/14-sentry.md`), and a graph's
//! trace through a workspace reads through it. Nothing is evaluated here:
//! a change only drops what was kept of the files it touched, so whoever
//! reads next reads those again and nothing else.
//!
//! A repository that is not watched, because the watches allowed are used
//! up or the system refuses one, is checked instead: Git says, when an ask
//! comes, whether its working directory is as it was at the last one.

use std::collections::hash_map::DefaultHasher;
use std::collections::{BTreeSet, HashSet};
use std::hash::{Hash, Hasher};
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
    manifest: PathBuf,
    watched: Vec<Watched>,
    warm: Arc<lemma_graph::Warm>,
}

/// Why a repository that would take more watches than are left is not
/// watched.
const OVER_BUDGET: &str = "the watches graphs may take are used up ([graph] watches)";

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
        Self::start_within(manifest, None)
    }

    /// [`WatchedWorkspace::start`], taking at most `watches` watches where
    /// each directory takes one (`None` for no limit): the repositories
    /// that fit are watched, the largest first (see [`chosen_to_watch`]),
    /// and the others are checked at each ask.
    pub(crate) fn start_within(manifest: &Path, watches: Option<usize>) -> anyhow::Result<Self> {
        Ok(Self {
            manifest: manifest.to_path_buf(),
            watched: watch_repositories(manifest, watches)?,
            warm: Arc::default(),
        })
    }

    /// How many watches its watchers hold, where each directory takes one.
    pub(crate) fn watches(&self) -> usize {
        self.watched.iter().map(Watched::watches).sum()
    }

    /// A number that changes whenever a file a check can depend on may
    /// have, as of the last [`WatchedWorkspace::collect`]: each
    /// repository's generation and the workspace's own description.
    /// Something worked out from the workspace holds while it stays the
    /// same.
    pub(crate) fn state(&self) -> u64 {
        let mut hasher = DefaultHasher::new();
        for watched in &self.watched {
            watched.generation.hash(&mut hasher);
        }
        manifest_stamp(&self.manifest).hash(&mut hasher);
        hasher.finish()
    }

    /// What is kept of what was read.
    pub(crate) fn warm(&self) -> &Arc<lemma_graph::Warm> {
        &self.warm
    }

    /// The repositories watched.
    pub(crate) fn repositories(&self) -> &[Watched] {
        &self.watched
    }

    /// The directories, canonical, of the repositories whose changes are
    /// known, from a watcher or from Git: what is kept of any other is
    /// dropped on every collection that confirms.
    pub(crate) fn reported_roots(&self) -> Vec<PathBuf> {
        self.watched
            .iter()
            .filter(|watched| watched.watch.is_some() || watched.seen_as.is_some())
            .map(|watched| watched.root.clone())
            .collect()
    }

    /// Drops what is kept of what changed in each repository; the changes,
    /// by repository. `confirming` waits for every watcher's barrier, so a
    /// change made just before the call counts, and leaves nothing to
    /// prepare, since whoever asks reads what it needs; without it only
    /// repositories with reported changes are looked at. Git is asked
    /// about the unwatched ones only when confirming.
    pub(crate) fn collect(&mut self, confirming: bool) -> Vec<Batch> {
        if confirming {
            self.ask_git();
        }
        self.collect_reported(confirming)
    }

    /// Asks Git, about all unwatched repositories at once, whether each is
    /// as it was when last asked, and drops what is kept of those that are
    /// not, or that Git cannot speak for.
    fn ask_git(&mut self) {
        let warm = &self.warm;
        let unwatched: Vec<&mut Watched> = self
            .watched
            .iter_mut()
            .filter(|watched| watched.watch.is_none())
            .collect();
        let places: Vec<Place<'_>> = unwatched
            .iter()
            .map(|watched| Place {
                root: &watched.root,
                spelled: &watched.spelled,
                inner: &watched.inner,
            })
            .collect();
        let seen: Vec<Option<u64>> = std::thread::scope(|scope| {
            let asking: Vec<_> = places
                .iter()
                .map(|place| scope.spawn(|| seen_unwatched(warm, place)))
                .collect();
            asking
                .into_iter()
                .map(|asked| asked.join().unwrap_or(None))
                .collect()
        });
        for (watched, seen) in unwatched.into_iter().zip(seen) {
            if seen.is_some() && seen == watched.seen_as {
                continue;
            }
            warm.forget_working(&watched.root);
            watched.seen_as = seen;
            watched.generation += 1;
        }
    }

    /// The watched repositories' part of [`WatchedWorkspace::collect`].
    fn collect_reported(&mut self, confirming: bool) -> Vec<Batch> {
        let mut batches = Vec::new();
        for watched in &mut self.watched {
            if watched.watch.is_none() {
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

    /// What is kept, once every change made before this call has dropped
    /// what it touched.
    pub(crate) fn current(&mut self) -> Arc<lemma_graph::Warm> {
        self.collect(true);
        Arc::clone(&self.warm)
    }

    /// [`WatchedWorkspace::current`] for whoever called it already while
    /// answering the same ask: Git, asked then about the unwatched
    /// repositories, is not asked again.
    pub(crate) fn current_again(&mut self) -> Arc<lemma_graph::Warm> {
        self.collect_reported(true);
        Arc::clone(&self.warm)
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

/// An unwatched repository, as checking it needs it.
struct Place<'a> {
    /// Its directory, canonical.
    root: &'a Path,
    /// Its directory as checks name it.
    spelled: &'a Path,
    /// The repositories inside its directory, each checked on its own.
    inner: &'a [PathBuf],
}

/// What the working directory at `place` looks like without a watcher, as
/// one number that changes when a file a check reads may have; `None`
/// when nothing can say. Git speaks for every file it lists. A file it
/// ignores is kept only by name, so each of those is looked at for its
/// size and time. A directory outside Git, such as one that only holds a
/// workspace's description and its repositories, gives checks nothing but
/// its `.lm` files, which are looked at the same way.
fn seen_unwatched(warm: &lemma_graph::Warm, place: &Place<'_>) -> Option<u64> {
    // Named before Git is asked: a file read after this ask's answer from
    // Git is first counted at the next, where a new name is a change.
    let named = warm.files_read_by_name(place.spelled);
    let mut hasher = DefaultHasher::new();
    match warm.working_state_now(place.spelled) {
        Some(state) => state.hash(&mut hasher),
        // Git knows the directory and cannot say, as before a first commit.
        None if place.root.join(".git").exists() => return None,
        None => description_files(place.root, place.inner).hash(&mut hasher),
    }
    for file in named {
        file_stamp(&place.root.join(&file)).hash(&mut hasher);
        file.hash(&mut hasher);
    }
    Some(hasher.finish())
}

/// The `.lm` files under `directory`, sorted, each with its size and time,
/// leaving out the directories `inner`, those whose names start with a
/// dot, and any other repository's: its modules are its own.
fn description_files(directory: &Path, inner: &[PathBuf]) -> Vec<(PathBuf, (u64, u128))> {
    let mut found = Vec::new();
    let mut directories = vec![directory.to_path_buf()];
    while let Some(next) = directories.pop() {
        let Ok(entries) = std::fs::read_dir(&next) else {
            continue;
        };
        for entry in entries.flatten() {
            let path = entry.path();
            let is_hidden = entry.file_name().to_string_lossy().starts_with('.');
            let is_directory = entry.file_type().is_ok_and(|kind| kind.is_dir());
            let is_entered =
                is_directory && !is_hidden && !inner.contains(&path) && !path.join(".git").exists();
            if is_entered {
                directories.push(path);
            } else if !is_directory && path.extension().is_some_and(|ext| ext == "lm") {
                found.push((path.clone(), file_stamp(&path)));
            }
        }
    }
    found.sort();
    found
}

/// A watcher on each repository of the workspace whose description is
/// `manifest`, while they fit in `watches` (`None` for no limit).
fn watch_repositories(manifest: &Path, watches: Option<usize>) -> anyhow::Result<Vec<Watched>> {
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
    // A workspace's directory holds its repositories, each watched on
    // its own.
    let inner_of = |root: &PathBuf| -> Vec<PathBuf> {
        roots
            .iter()
            .filter(|other| *other != root && other.starts_with(root))
            .cloned()
            .collect()
    };
    let selection = lemma_index::Selection::everything_git_lists();
    let needed: Vec<usize> = match watches {
        // Nothing to fit them in: not counted.
        None | Some(0) => vec![0; roots.len()],
        Some(_) => roots
            .iter()
            .map(|root| lemma_index::watches_needed(root, &selection, &inner_of(root)))
            .collect(),
    };
    let chosen = chosen_to_watch(&needed, watches);
    let repositories = roots
        .iter()
        .zip(spelled)
        .zip(chosen)
        .map(|((root, spelled), is_chosen)| watch(root.clone(), spelled, inner_of(root), is_chosen))
        .collect();
    Ok(repositories)
}

/// Which repositories to watch, when they need `needed` watches each and
/// `watches` may be taken (`None` for no limit, and none at all for
/// nothing): the largest that fits first, then the largest that fits in
/// what is left, and so on. The repositories left out are checked at the
/// same time when an ask comes, which then waits for the slowest of them,
/// and Git takes longest over the largest.
fn chosen_to_watch(needed: &[usize], watches: Option<usize>) -> Vec<bool> {
    let Some(mut left) = watches else {
        return vec![true; needed.len()];
    };
    let mut chosen = vec![false; needed.len()];
    if left == 0 {
        return chosen;
    }
    let mut largest_first: Vec<usize> = (0..needed.len()).collect();
    largest_first.sort_by_key(|index| std::cmp::Reverse(needed[*index]));
    for index in largest_first {
        if needed[index] <= left {
            left -= needed[index];
            chosen[index] = true;
        }
    }
    chosen
}

/// The size and modification time of the workspace's own description
/// (`mod.lm` beside `manifest`), which sits beside, not inside, the
/// repositories watched.
fn manifest_stamp(manifest: &Path) -> (u64, u128) {
    let directory = manifest.parent().unwrap_or(manifest);
    file_stamp(&directory.join("mod.lm"))
}

/// The size and modification time of the file at `path`; zeros when it
/// cannot be read.
fn file_stamp(path: &Path) -> (u64, u128) {
    std::fs::metadata(path).map_or((0, 0), |metadata| {
        let modified = metadata
            .modified()
            .ok()
            .and_then(|time| time.duration_since(std::time::UNIX_EPOCH).ok())
            .map_or(0, |elapsed| elapsed.as_nanos());
        (metadata.len(), modified)
    })
}

/// One watched repository.
pub(crate) struct Watched {
    root: PathBuf,
    /// The root as a check names it, which is how reads are kept.
    spelled: PathBuf,
    /// The repositories inside its directory, each watched on its own.
    inner: Vec<PathBuf>,
    watch: Option<lemma_index::Watch>,
    /// Why it is not watched, when it is not.
    failure: Option<String>,
    /// Unwatched: what it looked like when last checked (see
    /// [`seen_unwatched`]). `None` before that, and when nothing can say.
    seen_as: Option<u64>,
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

    /// How many times only files no check reads changed.
    pub(crate) fn unread_generation(&self) -> u64 {
        self.unread_generation
    }

    /// How many watches its watcher holds, where each directory takes one.
    fn watches(&self) -> usize {
        self.watch.as_ref().map_or(0, watches_of)
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
                "not watching {root}: {}; checked at each ask",
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
/// checks, leaving out the repositories `inner` to it, or why there is
/// none: it was not chosen to be watched, or the system refused.
fn watch(root: PathBuf, spelled: PathBuf, inner: Vec<PathBuf>, is_chosen: bool) -> Watched {
    let (watch, failure) = match start_watch(&root, &inner, is_chosen) {
        Ok(watch) => (Some(watch), None),
        Err(reason) => {
            log::info!("not watching {}: {reason}", root.display());
            (None, Some(reason))
        }
    };
    Watched {
        root,
        spelled,
        inner,
        watch,
        failure,
        seen_as: None,
        generation: 0,
        unread_generation: 0,
        unprepared: None,
    }
}

/// Starts watching the repository at `root`, if it was chosen to be.
fn start_watch(
    root: &Path,
    inner: &[PathBuf],
    is_chosen: bool,
) -> Result<lemma_index::Watch, String> {
    if !is_chosen {
        return Err(OVER_BUDGET.to_owned());
    }
    let selection = lemma_index::Selection::everything_git_lists();
    lemma_index::Watch::start_without(root, &selection, inner)
}

/// How many watches `watch` holds, where each directory takes one.
fn watches_of(watch: &lemma_index::Watch) -> usize {
    match watch.report() {
        lemma_index::WatchReport::Watching {
            directories: Some(count),
        } => count,
        _ => 0,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn git(directory: &Path, args: &[&str]) {
        let status = std::process::Command::new("git")
            .arg("-C")
            .arg(directory)
            .args([
                "-c",
                "user.name=t",
                "-c",
                "user.email=t@t",
                "-c",
                "commit.gpgsign=false",
            ])
            .args(args)
            .status()
            .unwrap();
        assert!(status.success(), "git {args:?}");
    }

    fn write(path: &Path, text: &str) {
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(path, text).unwrap();
    }

    /// A workspace of one committed repository, `web`, under `root`; its
    /// description.
    fn workspace(root: &Path) -> PathBuf {
        let web = root.join("web");
        write(&web.join("src/charge.ts"), "export const charge = 1;\n");
        write(
            &web.join("mod.lm"),
            "module web\n\nexport service web {\n    index: { include: [\"src\"] }\n}\n",
        );
        git(&web, &["init", "-q"]);
        git(&web, &["add", "."]);
        git(&web, &["commit", "-q", "-m", "web"]);
        write(
            &root.join("mod.lm"),
            "module shop\n\nworkspace {\n    repo web = local(\"web\")\n}\n",
        );
        root.join("mod.lm").canonicalize().unwrap()
    }

    /// The untracked files of the repository whose directory ends in
    /// `name`, as what is kept lists them.
    fn untracked(watched: &WatchedWorkspace, warm: &lemma_graph::Warm, name: &str) -> Vec<String> {
        let repository = watched
            .repositories()
            .iter()
            .find(|repository| repository.spelled().ends_with(name))
            .unwrap();
        let listings = warm.verify().listings();
        listings.untracked(repository.spelled()).unwrap().to_vec()
    }

    #[test]
    fn a_file_written_after_a_read_is_seen_by_the_next() {
        let root = tempfile::tempdir().unwrap();
        let manifest = workspace(root.path());
        let mut watched = WatchedWorkspace::start(&manifest).unwrap();

        let warm = watched.current();
        let before = untracked(&watched, &warm, "web");
        write(&root.path().join("web/src/refund.ts"), "export {};\n");
        let warm = watched.current();
        let after = untracked(&watched, &warm, "web");

        assert_eq!(before, Vec::<String>::new());
        assert_eq!(after, ["src/refund.ts"]);
    }

    #[test]
    fn with_no_watches_git_says_what_changed_and_the_rest_is_kept() {
        let root = tempfile::tempdir().unwrap();
        let manifest = workspace(root.path());
        let mut watched = WatchedWorkspace::start_within(&manifest, Some(0)).unwrap();

        let warm = watched.current();
        let before = untracked(&watched, &warm, "web");
        let at_first = watched.state();
        watched.current();
        let with_nothing_changed = watched.state();
        write(&root.path().join("web/src/refund.ts"), "export {};\n");
        let warm = watched.current();
        let after = untracked(&watched, &warm, "web");

        assert_eq!(watched.watches(), 0);
        assert_eq!(before, Vec::<String>::new());
        assert_eq!(with_nothing_changed, at_first);
        assert_eq!(after, ["src/refund.ts"]);
        assert_ne!(watched.state(), at_first);
    }

    #[test]
    fn a_change_to_a_file_already_changed_is_seen_without_watches() {
        let root = tempfile::tempdir().unwrap();
        let manifest = workspace(root.path());
        let mut watched = WatchedWorkspace::start_within(&manifest, Some(0)).unwrap();
        write(
            &root.path().join("web/src/charge.ts"),
            "export const charge = 2;\n",
        );
        watched.current();
        let after_one_edit = watched.state();

        write(
            &root.path().join("web/src/charge.ts"),
            "export const charge = 3;\n",
        );
        watched.current();

        assert_ne!(watched.state(), after_one_edit);
    }

    #[test]
    fn git_is_asked_about_an_unwatched_repository_once_per_ask() {
        let root = tempfile::tempdir().unwrap();
        let manifest = workspace(root.path());
        let mut watched = WatchedWorkspace::start_within(&manifest, Some(0)).unwrap();
        watched.current();
        let asked = watched.state();

        write(&root.path().join("web/src/refund.ts"), "export {};\n");
        watched.current_again();
        let within_the_ask = watched.state();
        watched.current();

        assert_eq!(within_the_ask, asked);
        assert_ne!(watched.state(), asked);
    }

    #[test]
    fn a_repository_is_watched_only_if_it_fits_the_watches_left() {
        let root = tempfile::tempdir().unwrap();
        let manifest = workspace(root.path());
        let unlimited = WatchedWorkspace::start(&manifest).unwrap();
        let needed = unlimited.watches();
        drop(unlimited);

        let lines = |watched: &WatchedWorkspace| -> Vec<String> {
            let repositories = watched.repositories().iter();
            repositories.map(Watched::status_line).collect()
        };
        let fitting = WatchedWorkspace::start_within(&manifest, Some(needed)).unwrap();

        assert_eq!(fitting.watches(), needed);
        assert!(
            lines(&fitting)
                .iter()
                .all(|line| line.starts_with("watching "))
        );
        // Where a watcher takes a watch per directory, one too few leaves
        // a repository to be checked at each ask.
        if needed > 0 {
            let short = WatchedWorkspace::start_within(&manifest, Some(needed - 1)).unwrap();
            assert!(short.watches() < needed);
            assert!(
                lines(&short)
                    .iter()
                    .any(|line| line.ends_with("checked at each ask")),
                "{:?}",
                lines(&short)
            );
        }
    }

    #[test]
    fn the_largest_repositories_that_fit_are_the_ones_watched() {
        let needed = [5, 100, 20];

        assert_eq!(chosen_to_watch(&needed, None), [true, true, true]);
        assert_eq!(chosen_to_watch(&needed, Some(125)), [true, true, true]);
        assert_eq!(chosen_to_watch(&needed, Some(110)), [true, true, false]);
        assert_eq!(chosen_to_watch(&needed, Some(30)), [true, false, true]);
        assert_eq!(chosen_to_watch(&needed, Some(4)), [false, false, false]);
        assert_eq!(chosen_to_watch(&needed, Some(0)), [false, false, false]);
        // Where watching a tree takes no watch per directory, any number
        // but none watches everything.
        assert_eq!(chosen_to_watch(&[0, 0], Some(1)), [true, true]);
        assert_eq!(chosen_to_watch(&[0, 0], Some(0)), [false, false]);
    }

    #[test]
    fn what_is_kept_stays_while_nothing_changes() {
        let root = tempfile::tempdir().unwrap();
        let manifest = workspace(root.path());
        let mut watched = WatchedWorkspace::start(&manifest).unwrap();
        let first = watched.current();
        let before = watched.state();
        let second = watched.current();

        assert!(Arc::ptr_eq(&first, &second));
        assert_eq!(watched.state(), before);
    }

    #[test]
    fn a_change_to_a_repository_or_the_description_moves_the_state() {
        let root = tempfile::tempdir().unwrap();
        let manifest = workspace(root.path());
        let mut watched = WatchedWorkspace::start(&manifest).unwrap();
        watched.current();
        let at_first = watched.state();

        write(&root.path().join("web/src/refund.ts"), "export {};\n");
        watched.current();
        let after_a_file = watched.state();
        write(
            &manifest,
            "module shop\n\nworkspace {\n    repo web = local(\"web\")\n    config: { level: check }\n}\n",
        );
        watched.current();
        let after_the_description = watched.state();

        assert_ne!(after_a_file, at_first);
        assert_ne!(after_the_description, after_a_file);
    }
}
