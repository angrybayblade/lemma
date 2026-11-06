//! Builds snapshots of a directory's source files and symbols.
//!
//! Indexing is syntax-only and deterministic: the same files and extractor
//! version always produce the same snapshot fingerprint and symbols.

mod discover;
mod git;
pub mod retention;
mod scan;
mod scope;
mod selection;
mod state;
mod uncommitted;
mod watch;

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::thread;
use std::time::{SystemTime, UNIX_EPOCH};

use lemma_model::{Language, Snapshot};
use lemma_store::{FactStore, NewSnapshot, SnapshotUpdate, Store, StoreError, StoredSnapshot};
use lemma_syntax::{EXTRACTOR_VERSION, ExtractorPool, SyntaxError};

pub use crate::selection::{InvalidPattern, RuleSpec, Selection};

use crate::scan::ReusePolicy;
use crate::scope::{DirtyScope, WalkScope};
use crate::state::{ChangeSet, Coverage, FileSet};

/// Why indexing failed.
#[derive(Debug, thiserror::Error)]
pub enum IndexError {
    /// The directory to index could not be resolved.
    #[error("cannot index {}", path.display())]
    Root {
        /// The requested directory.
        path: PathBuf,
        /// Why it could not be resolved.
        #[source]
        source: std::io::Error,
    },
    /// Walking the directory failed.
    #[error("could not list source files")]
    Walk(#[from] ignore::Error),
    /// The syntax extractor failed.
    #[error("symbol extraction failed")]
    Syntax(#[from] SyntaxError),
    /// Saving the snapshot failed.
    #[error("could not save the snapshot")]
    Store(#[from] StoreError),
}

/// What an indexing run did.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct IndexSummary {
    /// The stored snapshot's id.
    pub snapshot_id: i64,
    /// The snapshot that was indexed.
    pub snapshot: Snapshot,
    /// Whether nothing changed since the last run, so the stored snapshot and
    /// its cached provider results were kept.
    pub is_unchanged: bool,
    /// Indexed files per language.
    pub files_by_language: BTreeMap<Language, usize>,
    /// Total symbols extracted.
    pub symbol_count: usize,
    /// Files the parser had to recover in. Some of their symbols may be missing.
    pub files_with_syntax_errors: Vec<String>,
    /// Source files skipped because they are not valid UTF-8 or could not be read.
    pub unreadable_files: Vec<String>,
}

/// Indexes every supported source file under `root` and stores the snapshot.
///
/// Re-indexing is incremental: files whose stamp or contents are unchanged
/// since the stored snapshot are not parsed again, and only changed files are
/// rewritten in the store.
pub fn index_directory(root: &Path, store: &mut Store) -> Result<IndexSummary, IndexError> {
    LiveIndex::new(root)?.index(store)
}

/// An index of one directory kept up to date by a long-lived process.
///
/// Between runs it remembers the indexed files, their totals, and the
/// checked-out commit, so a change costs work in proportion to the change.
/// With a watcher it also knows which paths changed, so it lists only those
/// directories instead of walking the whole tree.
pub struct LiveIndex {
    root: PathBuf,
    previous: Option<Previous>,
    /// The commit seen by the last run.
    last_head: HeadState,
    watch: Option<watch::Watch>,
    extractors: ExtractorPool,
    /// The files it covers.
    selection: Selection,
}

/// What is known about the checked-out commit.
#[derive(Debug, Clone)]
enum HeadState {
    /// Not known: ask Git.
    Unknown,
    /// Known: a commit, or `None` outside a repository or before the first
    /// commit.
    Known(Option<git::Head>),
}

/// What the store holds for the directory after the last run.
struct Previous {
    stored: StoredSnapshot,
    files: FileSet,
    uncommitted_paths: Vec<String>,
}

impl LiveIndex {
    /// An index of every supported file that walks the whole tree on every
    /// run.
    pub fn new(root: &Path) -> Result<Self, IndexError> {
        Self::selecting(root, Selection::everything())
    }

    /// An index of the files `selection` covers that walks the whole tree
    /// on every run.
    pub fn selecting(root: &Path, selection: Selection) -> Result<Self, IndexError> {
        let root = root.canonicalize().map_err(|source| IndexError::Root {
            path: root.to_owned(),
            source,
        })?;
        Ok(Self {
            root,
            previous: None,
            last_head: HeadState::Unknown,
            watch: None,
            extractors: ExtractorPool::new(),
            selection,
        })
    }

    /// An index of the files `selection` covers that watches the tree and
    /// walks only what changed. Falls back to full walks if the platform
    /// cannot watch.
    pub fn watched(root: &Path, selection: Selection) -> Result<Self, IndexError> {
        let mut index = Self::selecting(root, selection)?;
        index.watch = watch::Watch::start(&index.root);
        Ok(index)
    }

    /// The files this index covers.
    pub fn selection(&self) -> &Selection {
        &self.selection
    }

    /// The directory this index covers.
    pub fn root(&self) -> &Path {
        &self.root
    }

    /// Whether the watcher has seen files change since the last index, a
    /// sign that someone is working in the directory.
    pub fn has_unindexed_changes(&self) -> bool {
        self.watch.as_ref().is_some_and(watch::Watch::has_pending)
    }

    /// Languages with indexed files, as of the last run.
    pub fn languages(&self) -> Vec<Language> {
        self.previous.as_ref().map_or_else(Vec::new, |previous| {
            previous.files.files_by_language().keys().copied().collect()
        })
    }

    /// Indexes the directory and stores the snapshot.
    pub fn index(&mut self, store: &mut Store) -> Result<IndexSummary, IndexError> {
        let indexed_at_ns = now_ns();
        let root_text = self.root.display().to_string();
        let ScanPlan {
            previous,
            walk_scope,
            known_head,
        } = match self.plan(store, &root_text)? {
            Plan::Unchanged(summary) => {
                log::info!("index of {root_text}: nothing changed since it was last read");
                return Ok(*summary);
            }
            Plan::Scan(plan) => *plan,
        };
        log_scan(&root_text, &walk_scope);
        let started = std::time::Instant::now();
        let plan = ScanPlan {
            previous,
            walk_scope,
            known_head,
        };
        let summary = self.scan(store, root_text, plan, indexed_at_ns)?;
        log::info!("indexed in {:.1}s", started.elapsed().as_secs_f64());
        Ok(summary)
    }

    /// Reads what `plan` says may have changed and stores the snapshot.
    fn scan(
        &mut self,
        store: &mut Store,
        root_text: String,
        plan: ScanPlan,
        indexed_at_ns: i64,
    ) -> Result<IndexSummary, IndexError> {
        let ScanPlan {
            previous,
            walk_scope,
            known_head,
        } = plan;
        let area = Area {
            root: &self.root,
            selection: &self.selection,
        };
        let survey = survey(
            &area,
            previous.as_ref(),
            &walk_scope,
            known_head,
            &self.extractors,
        )?;
        let coverage = match &walk_scope {
            WalkScope::Full => Coverage::All,
            WalkScope::Partial(dirty) => Coverage::Scope(dirty),
        };
        let (mut files, previous_stored) = match previous {
            Some(previous) => (previous.files, Some(previous.stored)),
            None => (FileSet::default(), None),
        };
        let snapshot = Snapshot {
            root: root_text,
            revision: survey.head.as_ref().map(|head| head.revision.clone()),
            is_dirty: !survey.uncommitted_paths.is_empty(),
            fingerprint: files.fingerprint_after(&survey.changes),
        };
        let write = SnapshotWrite {
            previous_id: previous_stored.as_ref().map(|stored| stored.id),
            snapshot: &snapshot,
            changes: &survey.changes,
            uncommitted_paths: &survey.uncommitted_paths,
            indexed_at_ns,
        };
        // If the write fails, memory no longer matches the store; `previous`
        // stays empty and the next run reloads from the store.
        let snapshot_id = write.save(store)?;
        log_changes(&survey.changes);
        files.apply(survey.changes, coverage);

        let is_unchanged = previous_stored
            .is_some_and(|stored| stored.snapshot.fingerprint == snapshot.fingerprint);
        let previous = Previous {
            stored: StoredSnapshot {
                id: snapshot_id,
                extractor_version: EXTRACTOR_VERSION.to_owned(),
                indexed_at_ns,
                snapshot,
            },
            files,
            uncommitted_paths: survey.uncommitted_paths,
        };
        let summary = previous.summary(is_unchanged);
        self.last_head = HeadState::Known(survey.head);
        self.previous = Some(previous);
        Ok(summary)
    }

    /// Decides what this run must look at, or answers straight away when
    /// nothing reported can have changed the stored snapshot.
    fn plan(&mut self, store: &Store, root_text: &str) -> Result<Plan, IndexError> {
        let observed = self.watch.as_mut().map(watch::Watch::changes);
        let (previous, is_remembered) = self.current_previous(store, root_text)?;
        let walk_scope = match (&previous, observed) {
            (Some(previous), Some(observed)) if is_remembered && !observed.needs_full_walk => {
                scope::walk_scope(
                    &self.root,
                    observed.paths.iter().map(PathBuf::as_path),
                    previous.files.files(),
                    Some(&self.root.join(".lemma")),
                )
            }
            _ => WalkScope::Full,
        };
        let known_head = self.known_head(&walk_scope);
        let is_quiet = matches!(&walk_scope, WalkScope::Partial(dirty) if dirty.is_empty());
        let is_head_known = matches!(known_head, HeadState::Known(_));
        match previous {
            Some(previous) if is_quiet && is_head_known => {
                let summary = previous.summary(true);
                self.previous = Some(previous);
                Ok(Plan::Unchanged(Box::new(summary)))
            }
            previous => Ok(Plan::Scan(Box::new(ScanPlan {
                previous,
                walk_scope,
                known_head,
            }))),
        }
    }

    /// The previous run's state, from memory when the store still holds
    /// exactly that snapshot, otherwise from the store. The flag says whether
    /// it came from memory, which is when watched changes can be trusted.
    fn current_previous(
        &mut self,
        store: &Store,
        root_text: &str,
    ) -> Result<(Option<Previous>, bool), IndexError> {
        let stored = store.snapshot_for_root(root_text)?;
        let remembered = self
            .previous
            .take()
            .filter(|previous| stored.as_ref() == Some(&previous.stored));
        if remembered.is_some() {
            return Ok((remembered, true));
        }
        let Some(stored) = stored.filter(|stored| stored.extractor_version == EXTRACTOR_VERSION)
        else {
            return Ok((None, false));
        };
        let previous = Previous {
            files: FileSet::from_stored(store.files(stored.id)?),
            uncommitted_paths: store.uncommitted_paths(stored.id)?,
            stored,
        };
        Ok((Some(previous), false))
    }

    /// The commit from the last run, if nothing under `.git` changed since.
    /// Only usable when `.git` is inside the watched directory.
    fn known_head(&self, walk_scope: &WalkScope) -> HeadState {
        let WalkScope::Partial(dirty) = walk_scope else {
            return HeadState::Unknown;
        };
        let is_git_watched = self.root.join(".git").exists();
        if dirty.git_changed || !is_git_watched {
            return HeadState::Unknown;
        }
        self.last_head.clone()
    }
}

/// What a run has to do.
enum Plan {
    /// Nothing changed; this is the answer.
    Unchanged(Box<IndexSummary>),
    /// Scan part of the tree.
    Scan(Box<ScanPlan>),
}

/// The part of the tree to scan, and what is already known.
struct ScanPlan {
    previous: Option<Previous>,
    walk_scope: WalkScope,
    known_head: HeadState,
}

impl Previous {
    fn summary(&self, is_unchanged: bool) -> IndexSummary {
        IndexSummary {
            snapshot_id: self.stored.id,
            snapshot: self.stored.snapshot.clone(),
            is_unchanged,
            files_by_language: self.files.files_by_language().clone(),
            symbol_count: self.files.symbol_count(),
            files_with_syntax_errors: self.files.files_with_syntax_errors(),
            unreadable_files: self.files.unreadable_files(),
        }
    }
}

/// Everything one run writes to the store.
struct SnapshotWrite<'a> {
    previous_id: Option<i64>,
    snapshot: &'a Snapshot,
    changes: &'a ChangeSet,
    uncommitted_paths: &'a [String],
    indexed_at_ns: i64,
}

impl SnapshotWrite<'_> {
    /// Saves a new snapshot, or updates the previous one in place.
    fn save(&self, store: &mut Store) -> Result<i64, IndexError> {
        let Some(previous_id) = self.previous_id else {
            return Ok(store.replace_snapshot(&NewSnapshot {
                snapshot: self.snapshot,
                extractor_version: EXTRACTOR_VERSION,
                indexed_at_ns: self.indexed_at_ns,
                files: &self.changes.changed,
                uncommitted_paths: self.uncommitted_paths,
            })?);
        };
        store.update_snapshot(
            previous_id,
            &SnapshotUpdate {
                snapshot: self.snapshot,
                indexed_at_ns: self.indexed_at_ns,
                uncommitted_paths: self.uncommitted_paths,
                changed: &self.changes.changed,
                removed: &self.changes.removed,
                restamped: &self.changes.restamped,
            },
        )?;
        Ok(previous_id)
    }
}

/// Everything learned about the directory before anything is written.
struct Survey {
    changes: ChangeSet,
    head: Option<git::Head>,
    uncommitted_paths: Vec<String>,
}

/// Logs what an indexing run found changed.
fn log_changes(changes: &ChangeSet) {
    log::info!(
        "{} files parsed, {} only restamped, {} removed",
        changes.changed.len(),
        changes.restamped.len(),
        changes.removed.len()
    );
}

/// Logs what an indexing run looks at.
fn log_scan(root: &str, walk_scope: &WalkScope) {
    match walk_scope {
        WalkScope::Full => log::info!("indexing {root}: checking every file for changes"),
        WalkScope::Partial(_) => {
            log::info!("indexing {root}: checking what the watcher saw change");
        }
    }
}

/// How this run learns which source files differ from the commit.
enum GitPlan {
    /// Not a repository.
    NoRepository,
    /// The commit is new to this index, or Git's own state changed: ask
    /// about every source file.
    Everything(git::Head),
    /// Same commit, and the watcher named what changed: ask about those
    /// paths while the scan runs.
    Paths(git::Head, Vec<String>),
    /// Same commit, but what changed is only known once the scan finishes.
    AfterScan(git::Head),
}

/// The part of a tree an index covers.
struct Area<'a> {
    root: &'a Path,
    selection: &'a Selection,
}

/// Scans the directory and asks Git what changed, overlapping the slow
/// parts: Git runs while the walk and parse do.
fn survey(
    area: &Area<'_>,
    previous: Option<&Previous>,
    walk_scope: &WalkScope,
    known_head: HeadState,
    extractors: &ExtractorPool,
) -> Result<Survey, IndexError> {
    let Area { root, selection } = *area;
    let head_probe = matches!(known_head, HeadState::Unknown).then(|| git::HeadProbe::start(root));
    thread::scope(|scope| {
        let walk = scope.spawn(|| match walk_scope {
            WalkScope::Full => discover::source_files(root, selection),
            WalkScope::Partial(dirty) => discover::source_files_in(root, Some(dirty), selection),
        });
        let head = match (head_probe, known_head) {
            (Some(probe), _) => probe.finish(),
            (None, HeadState::Known(head)) => head,
            (None, HeadState::Unknown) => None,
        };
        let previous_uncommitted =
            previous.map_or(&[][..], |previous| previous.uncommitted_paths.as_slice());
        let plan = git_plan(head.clone(), previous, walk_scope);
        let early_git = match &plan {
            GitPlan::Everything(head) => {
                let head = head.clone();
                Some(scope.spawn(move || uncommitted::all(root, &head, selection)))
            }
            GitPlan::Paths(head, paths) => {
                let head = head.clone();
                let paths = paths.clone();
                Some(scope.spawn(move || {
                    let paths: Vec<&str> = paths.iter().map(String::as_str).collect();
                    uncommitted::refreshed(root, &head, previous_uncommitted, &paths, selection)
                }))
            }
            GitPlan::NoRepository | GitPlan::AfterScan(_) => None,
        };

        let empty = BTreeMap::new();
        let reuse = ReusePolicy {
            known: previous.map_or(&empty, |previous| previous.files.files()),
            trusted_before_ns: previous.map(|previous| previous.stored.indexed_at_ns),
        };
        let scans = scan::scan_files(join(walk)?, &reuse, extractors)?;
        let coverage = match walk_scope {
            WalkScope::Full => Coverage::All,
            WalkScope::Partial(dirty) => Coverage::Scope(dirty),
        };
        let changes = match previous {
            Some(previous) => previous.files.changes(scans, coverage),
            None => FileSet::default().changes(scans, coverage),
        };

        let uncommitted_paths = match (plan, early_git) {
            (_, Some(early)) => join(early),
            (GitPlan::AfterScan(head), None) => {
                let touched: Vec<&str> = changes.touched_paths().collect();
                uncommitted::refreshed(root, &head, previous_uncommitted, &touched, selection)
            }
            _ => Vec::new(),
        };
        Ok(Survey {
            changes,
            head,
            uncommitted_paths,
        })
    })
}

fn git_plan(
    head: Option<git::Head>,
    previous: Option<&Previous>,
    walk_scope: &WalkScope,
) -> GitPlan {
    let Some(head) = head else {
        return GitPlan::NoRepository;
    };
    let previous_revision =
        previous.and_then(|previous| previous.stored.snapshot.revision.as_deref());
    if previous_revision != Some(head.revision.as_str()) {
        return GitPlan::Everything(head);
    }
    match walk_scope {
        // Staging or committing changes `.git` without moving files; only a
        // full status sees that.
        WalkScope::Partial(DirtyScope {
            git_changed: true, ..
        }) => GitPlan::Everything(head),
        WalkScope::Partial(dirty) => GitPlan::Paths(head, dirty.paths.iter().cloned().collect()),
        WalkScope::Full => GitPlan::AfterScan(head),
    }
}

/// Waits for a background thread, re-raising its panic if it had one.
fn join<T>(handle: thread::ScopedJoinHandle<'_, T>) -> T {
    handle
        .join()
        .unwrap_or_else(|payload| std::panic::resume_unwind(payload))
}

/// The current wall-clock time. Only used to decide which file stamps can be
/// trusted, never in anything stored as a fact or hashed.
fn now_ns() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .ok()
        .and_then(|elapsed| i64::try_from(elapsed.as_nanos()).ok())
        .unwrap_or_default()
}

#[cfg(test)]
mod tests {
    use super::*;
    use lemma_store::{NameMatch, SymbolFilter};
    use std::fs;

    fn write(root: &Path, relative_path: &str, contents: &str) {
        let path = root.join(relative_path);
        fs::create_dir_all(path.parent().unwrap()).unwrap();
        fs::write(path, contents).unwrap();
    }

    fn sample_repository() -> tempfile::TempDir {
        let directory = tempfile::tempdir().unwrap();
        write(
            directory.path(),
            "src/router.ts",
            "export class Router { delete() {} }\n",
        );
        write(
            directory.path(),
            "py/client.py",
            "def delete_session():\n    pass\n",
        );
        write(
            directory.path(),
            "go/main.go",
            "package main\nfunc Delete() {}\n",
        );
        write(
            directory.path(),
            "node_modules/dep/index.ts",
            "export function ignored() {}\n",
        );
        write(directory.path(), "README.md", "# not source\n");
        directory
    }

    #[test]
    fn indexes_supported_files_and_skips_dependencies() {
        let repository = sample_repository();
        let mut store = Store::open_in_memory().unwrap();

        let summary = index_directory(repository.path(), &mut store).unwrap();

        assert_eq!(
            summary.files_by_language,
            BTreeMap::from([
                (Language::TypeScript, 1),
                (Language::Python, 1),
                (Language::Go, 1)
            ])
        );
        assert_eq!(summary.symbol_count, 4);
        assert!(!summary.is_unchanged);

        let ignored = SymbolFilter {
            text: "ignored".to_owned(),
            name_match: NameMatch::Exact,
            kind: None,
            language: None,
            path_contains: None,
            limit: 10,
        };
        assert_eq!(
            store.find_symbols(summary.snapshot_id, &ignored).unwrap(),
            [] as [lemma_store::StoredSymbol; 0]
        );
    }

    #[test]
    fn reindexing_unchanged_files_keeps_the_snapshot() {
        let repository = sample_repository();
        let mut store = Store::open_in_memory().unwrap();

        let first = index_directory(repository.path(), &mut store).unwrap();
        let second = index_directory(repository.path(), &mut store).unwrap();

        assert!(second.is_unchanged);
        assert_eq!(first.snapshot_id, second.snapshot_id);
        assert_eq!(first.snapshot.fingerprint, second.snapshot.fingerprint);
    }

    #[test]
    fn changing_a_file_changes_the_fingerprint() {
        let repository = sample_repository();
        let mut store = Store::open_in_memory().unwrap();

        let first = index_directory(repository.path(), &mut store).unwrap();
        write(
            repository.path(),
            "py/client.py",
            "def delete_session(force):\n    pass\n",
        );
        let second = index_directory(repository.path(), &mut store).unwrap();

        assert!(!second.is_unchanged);
        assert_ne!(first.snapshot.fingerprint, second.snapshot.fingerprint);
    }

    #[test]
    fn non_utf8_files_are_reported_not_indexed() {
        let repository = sample_repository();
        fs::write(repository.path().join("src/binary.ts"), [0xff, 0xfe, 0x00]).unwrap();
        let mut store = Store::open_in_memory().unwrap();

        let summary = index_directory(repository.path(), &mut store).unwrap();

        assert_eq!(summary.unreadable_files, ["src/binary.ts"]);
    }

    fn exact(name: &str) -> SymbolFilter {
        SymbolFilter {
            text: name.to_owned(),
            name_match: NameMatch::Exact,
            kind: None,
            language: None,
            path_contains: None,
            limit: 10,
        }
    }

    fn symbol_ids(store: &Store, snapshot_id: i64, name: &str) -> Vec<i64> {
        store
            .find_symbols(snapshot_id, &exact(name))
            .unwrap()
            .into_iter()
            .map(|stored| stored.id)
            .collect()
    }

    #[test]
    fn reindexing_after_an_edit_keeps_ids_of_untouched_symbols() {
        let repository = sample_repository();
        let mut store = Store::open_in_memory().unwrap();
        let first = index_directory(repository.path(), &mut store).unwrap();
        let router_ids = symbol_ids(&store, first.snapshot_id, "Router.delete");

        write(
            repository.path(),
            "py/client.py",
            "def close_session():\n    pass\n",
        );
        let second = index_directory(repository.path(), &mut store).unwrap();

        assert_eq!(second.snapshot_id, first.snapshot_id);
        assert_eq!(
            symbol_ids(&store, second.snapshot_id, "Router.delete"),
            router_ids
        );
        assert_eq!(symbol_ids(&store, second.snapshot_id, "delete_session"), []);
        assert_eq!(
            symbol_ids(&store, second.snapshot_id, "close_session").len(),
            1
        );
        assert_eq!(second.symbol_count, first.symbol_count);
    }

    #[test]
    fn symbols_still_in_an_edited_file_keep_their_ids() {
        let repository = sample_repository();
        let mut store = Store::open_in_memory().unwrap();
        write(
            repository.path(),
            "py/edited.py",
            "def kept():\n    pass\n\ndef dropped():\n    pass\n",
        );
        let first = index_directory(repository.path(), &mut store).unwrap();
        let kept_ids = symbol_ids(&store, first.snapshot_id, "kept");

        write(
            repository.path(),
            "py/edited.py",
            "# moved down\n\ndef added():\n    pass\n\ndef kept():\n    return 1\n",
        );
        let second = index_directory(repository.path(), &mut store).unwrap();

        assert_eq!(symbol_ids(&store, second.snapshot_id, "kept"), kept_ids);
        assert_eq!(symbol_ids(&store, second.snapshot_id, "dropped"), []);
        assert_eq!(symbol_ids(&store, second.snapshot_id, "added").len(), 1);
    }

    #[test]
    fn deleted_files_leave_the_index() {
        let repository = sample_repository();
        let mut store = Store::open_in_memory().unwrap();
        index_directory(repository.path(), &mut store).unwrap();

        fs::remove_file(repository.path().join("go/main.go")).unwrap();
        let summary = index_directory(repository.path(), &mut store).unwrap();

        assert!(!summary.files_by_language.contains_key(&Language::Go));
        assert_eq!(symbol_ids(&store, summary.snapshot_id, "Delete"), []);
    }

    #[test]
    fn a_live_index_matches_a_fresh_one_after_edits() {
        let repository = sample_repository();
        let mut live_store = Store::open_in_memory().unwrap();
        let mut fresh_store = Store::open_in_memory().unwrap();
        let mut live = LiveIndex::new(repository.path()).unwrap();
        live.index(&mut live_store).unwrap();

        write(repository.path(), "py/extra.py", "def extra():\n    pass\n");
        fs::remove_file(repository.path().join("go/main.go")).unwrap();
        let after_edit = live.index(&mut live_store).unwrap();
        let fresh = index_directory(repository.path(), &mut fresh_store).unwrap();

        assert_eq!(after_edit.snapshot.fingerprint, fresh.snapshot.fingerprint);
        assert_eq!(after_edit.symbol_count, fresh.symbol_count);
        assert_eq!(after_edit.files_by_language, fresh.files_by_language);
    }
}
