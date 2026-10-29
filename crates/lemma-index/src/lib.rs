//! Builds snapshots of a directory's source files and symbols.
//!
//! Indexing is syntax-only and deterministic: the same files and extractor
//! version always produce the same snapshot fingerprint and symbols.

mod discover;
mod git;

use std::collections::{BTreeMap, BTreeSet, HashMap, HashSet};
use std::fs;
use std::path::{Path, PathBuf};
use std::sync::{Mutex, PoisonError};
use std::thread;
use std::time::{SystemTime, UNIX_EPOCH};

use lemma_model::{IndexedFile, Language, Snapshot, content_hash};
use lemma_store::{
    FileStamp, NewSnapshot, SnapshotUpdate, StampedFile, Store, StoreError, StoredFile,
    StoredSnapshot,
};
use lemma_syntax::{EXTRACTOR_VERSION, Extraction, Extractor, SyntaxError};
use rayon::iter::{IntoParallelIterator, ParallelIterator};

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
    let indexed_at_ns = now_ns();
    let root = root.canonicalize().map_err(|source| IndexError::Root {
        path: root.to_owned(),
        source,
    })?;
    let root_text = root.display().to_string();

    let survey = survey(&root, store)?;
    let fingerprint = snapshot_fingerprint(&survey.scans);
    let snapshot = Snapshot {
        root: root_text,
        revision: survey.head.as_ref().map(|head| head.revision.clone()),
        is_dirty: !survey.uncommitted_paths.is_empty(),
        fingerprint,
    };
    let is_unchanged = survey
        .previous
        .as_ref()
        .is_some_and(|previous| previous.stored.snapshot.fingerprint == snapshot.fingerprint);

    let snapshot_id = match &survey.previous {
        Some(previous) => {
            let update = SnapshotUpdate {
                snapshot: &snapshot,
                indexed_at_ns,
                uncommitted_paths: &survey.uncommitted_paths,
                changed: &survey.changes.changed,
                removed: &survey.changes.removed,
                restamped: &survey.changes.restamped,
            };
            store.update_snapshot(previous.stored.id, &update)?;
            previous.stored.id
        }
        None => store.replace_snapshot(&NewSnapshot {
            snapshot: &snapshot,
            extractor_version: EXTRACTOR_VERSION,
            indexed_at_ns,
            files: &survey.changes.changed,
            uncommitted_paths: &survey.uncommitted_paths,
        })?,
    };
    Ok(summarize(
        snapshot_id,
        snapshot,
        is_unchanged,
        &survey.scans,
    ))
}

/// Everything learned about the directory before anything is written.
struct Survey {
    previous: Option<Previous>,
    scans: Vec<Scan>,
    changes: Changes,
    head: Option<git::Head>,
    uncommitted_paths: Vec<String>,
}

/// The stored snapshot of the directory, as the starting point for an
/// incremental update.
struct Previous {
    stored: StoredSnapshot,
    files: HashMap<String, StoredFile>,
    uncommitted_paths: Vec<String>,
}

/// Scans the directory and asks Git what changed, overlapping the slow parts:
/// the walk and Git run in the background while the stored files load.
fn survey(root: &Path, store: &Store) -> Result<Survey, IndexError> {
    let head_probe = git::HeadProbe::start(root);
    thread::scope(|scope| {
        let walk = scope.spawn(|| discover::source_files(root));
        let previous = load_previous(store, &root.display().to_string())?;
        let head = head_probe.finish();

        // A first index, or a different commit, needs a full `git status`;
        // start it now so it runs during the scan.
        let needs_full_status = head.as_ref().is_some_and(|head| {
            previous.as_ref().is_none_or(|previous| {
                previous.stored.snapshot.revision.as_deref() != Some(head.revision.as_str())
            })
        });
        let full_status = head
            .as_ref()
            .filter(|_| needs_full_status)
            .cloned()
            .map(|head| scope.spawn(move || git::uncommitted_paths(root, &head, None)));

        let known = previous.as_ref().map(|previous| &previous.files);
        let empty = HashMap::new();
        let reuse = ReusePolicy {
            known: known.unwrap_or(&empty),
            trusted_before_ns: previous
                .as_ref()
                .map(|previous| previous.stored.indexed_at_ns),
        };
        let scans = scan_files(join(walk)?, &reuse)?;
        let changes = Changes::between(reuse.known, scans.iter());

        let uncommitted = match (&head, full_status, &previous) {
            (None, _, _) => Vec::new(),
            (Some(_), Some(full_status), _) => join(full_status).unwrap_or_default(),
            (Some(head), None, Some(previous)) => {
                refresh_uncommitted(root, head, &previous.uncommitted_paths, &changes)
            }
            (Some(head), None, None) => {
                git::uncommitted_paths(root, head, None).unwrap_or_default()
            }
        };
        let uncommitted_paths = uncommitted
            .into_iter()
            .filter(|path| discover::is_in_scope(path))
            .collect();
        Ok(Survey {
            previous,
            scans,
            changes,
            head,
            uncommitted_paths,
        })
    })
}

/// Most paths Git is asked about by name before a full status is cheaper.
const MAX_NAMED_STATUS_PATHS: usize = 1000;

/// Updates the previous run's uncommitted paths by asking Git only about
/// files whose contents changed since. Untouched files cannot have started or
/// stopped differing from an unchanged commit.
fn refresh_uncommitted(
    root: &Path,
    head: &git::Head,
    previous: &[String],
    changes: &Changes,
) -> Vec<String> {
    let changed_paths = changes
        .changed
        .iter()
        .map(|changed| changed.file.path.as_str());
    let touched: Vec<&str> = changed_paths
        .chain(changes.removed.iter().map(String::as_str))
        .collect();
    if touched.is_empty() {
        return previous.to_vec();
    }
    if touched.len() > MAX_NAMED_STATUS_PATHS {
        return git::uncommitted_paths(root, head, None).unwrap_or_default();
    }
    let Some(now_uncommitted) = git::uncommitted_paths(root, head, Some(&touched)) else {
        return git::uncommitted_paths(root, head, None).unwrap_or_default();
    };

    let touched: HashSet<&str> = touched.into_iter().collect();
    let mut paths: BTreeSet<String> = previous
        .iter()
        .filter(|path| !touched.contains(path.as_str()))
        .cloned()
        .collect();
    paths.extend(now_uncommitted);
    paths.into_iter().collect()
}

/// The stored snapshot of `root` and its files, if one exists and was made
/// by this extractor version. Older snapshots are rebuilt from scratch.
fn load_previous(store: &Store, root_text: &str) -> Result<Option<Previous>, IndexError> {
    let Some(stored) = store
        .snapshot_for_root(root_text)?
        .filter(|stored| stored.extractor_version == EXTRACTOR_VERSION)
    else {
        return Ok(None);
    };
    let files = known_files(store.files(stored.id)?);
    let uncommitted_paths = store.uncommitted_paths(stored.id)?;
    Ok(Some(Previous {
        stored,
        files,
        uncommitted_paths,
    }))
}

/// Scans files on every core. `collect` keeps the discovery order, which
/// keeps the snapshot deterministic.
fn scan_files(
    discovered: Vec<discover::SourceFile>,
    reuse: &ReusePolicy<'_>,
) -> Result<Vec<Scan>, IndexError> {
    let extractors = ExtractorPool::new();
    let scans: Vec<Result<Scan, IndexError>> = discovered
        .into_par_iter()
        .map(|file| scan_file(&extractors, file, reuse))
        .collect();
    scans.into_iter().collect()
}

/// Waits for a background thread, re-raising its panic if it had one.
fn join<T>(handle: thread::ScopedJoinHandle<'_, T>) -> T {
    handle
        .join()
        .unwrap_or_else(|payload| std::panic::resume_unwind(payload))
}

/// What scanning one discovered file found.
enum Scan {
    /// Stamp unchanged since the stored snapshot; not read.
    Unchanged(StoredFile),
    /// Read and hashed: same contents as stored, but a different stamp.
    Restamped(StoredFile),
    /// Parsed, because it is new or its contents changed.
    Parsed(StampedFile),
    /// Not valid UTF-8 or could not be read.
    Unreadable(String),
}

/// Which stored files a scan may reuse instead of parsing again.
struct ReusePolicy<'a> {
    known: &'a HashMap<String, StoredFile>,
    /// When the previous indexing run started, if there was one.
    trusted_before_ns: Option<i64>,
}

impl ReusePolicy<'_> {
    /// The stored file for this path, if it is the same language.
    fn stored(&self, file: &discover::SourceFile) -> Option<&StoredFile> {
        self.known
            .get(&file.relative_path)
            .filter(|stored| stored.language == file.language)
    }

    /// Whether an unchanged stamp is enough to skip reading the file.
    ///
    /// A file written during or after the previous run could have changed
    /// within the same timestamp tick as the stamp that run recorded. Like
    /// Git's "racily clean" check, such files are always read again; only
    /// stamps older than the previous run's start are trusted.
    fn trusts(&self, stored: &StoredFile, stamp: FileStamp) -> bool {
        let Some(trusted_before_ns) = self.trusted_before_ns else {
            return false;
        };
        stored.stamp == stamp && stamp.modified_ns.max(stamp.changed_ns) < trusted_before_ns
    }
}

/// Reuses, rehashes, or parses one file.
fn scan_file(
    extractors: &ExtractorPool,
    file: discover::SourceFile,
    reuse: &ReusePolicy<'_>,
) -> Result<Scan, IndexError> {
    let stored = reuse.stored(&file);
    if let Some(stored) = stored.filter(|stored| reuse.trusts(stored, file.stamp)) {
        return Ok(Scan::Unchanged(stored.clone()));
    }

    let Some(contents) = read_utf8(&file.absolute_path) else {
        return Ok(Scan::Unreadable(file.relative_path));
    };
    let content_hash = content_hash(contents.as_bytes());
    if let Some(stored) = stored.filter(|stored| stored.content_hash == content_hash) {
        let restamped = StoredFile {
            stamp: file.stamp,
            ..stored.clone()
        };
        return Ok(Scan::Restamped(restamped));
    }

    let extraction = extractors.extract(file.language, &contents)?;
    Ok(Scan::Parsed(StampedFile {
        file: IndexedFile {
            path: file.relative_path,
            language: file.language,
            content_hash,
            has_syntax_errors: extraction.has_syntax_errors,
            symbols: extraction.symbols,
        },
        stamp: file.stamp,
    }))
}

/// One extractor per worker thread, created the first time that thread has
/// to parse. Setting one up compiles every query (about 10 ms), so a no-op
/// re-index should not pay for it, and parsers cannot be shared between
/// threads.
struct ExtractorPool {
    slots: Vec<Mutex<Option<Extractor>>>,
}

impl ExtractorPool {
    fn new() -> Self {
        // One slot per pool thread, plus one for a caller outside the pool.
        let slots = (0..=rayon::current_num_threads())
            .map(|_| Mutex::new(None))
            .collect();
        Self { slots }
    }

    fn extract(&self, language: Language, source: &str) -> Result<Extraction, IndexError> {
        let index = rayon::current_thread_index().map_or(0, |thread| thread + 1);
        let Some(slot) = self.slots.get(index) else {
            return Ok(Extractor::new()?.extract(language, source)?);
        };
        // Each slot is only used by its own thread, so a poisoned lock just
        // means an earlier extraction on this thread failed; the extractor
        // itself is still usable.
        let mut slot = slot.lock().unwrap_or_else(PoisonError::into_inner);
        let extractor = match slot.take() {
            Some(extractor) => extractor,
            None => Extractor::new()?,
        };
        Ok(slot.insert(extractor).extract(language, source)?)
    }
}

/// The store writes needed to go from the stored files to the scanned ones.
struct Changes {
    changed: Vec<StampedFile>,
    removed: Vec<String>,
    restamped: Vec<(String, FileStamp)>,
}

impl Changes {
    fn between<'a>(
        known: &HashMap<String, StoredFile>,
        scans: impl Iterator<Item = &'a Scan>,
    ) -> Self {
        let mut changes = Self {
            changed: Vec::new(),
            removed: Vec::new(),
            restamped: Vec::new(),
        };
        let mut present = HashSet::new();
        for scan in scans {
            match scan {
                Scan::Unchanged(stored) => {
                    present.insert(stored.path.as_str());
                }
                Scan::Restamped(stored) => {
                    present.insert(stored.path.as_str());
                    changes.restamped.push((stored.path.clone(), stored.stamp));
                }
                Scan::Parsed(parsed) => {
                    present.insert(parsed.file.path.as_str());
                    changes.changed.push(parsed.clone());
                }
                Scan::Unreadable(_) => {}
            }
        }
        changes.removed = known
            .keys()
            .filter(|path| !present.contains(path.as_str()))
            .cloned()
            .collect();
        changes.removed.sort();
        changes
    }
}

/// Per-file facts every kind of scan can report.
struct FileFacts<'a> {
    path: &'a str,
    language: Language,
    content_hash: &'a str,
    has_syntax_errors: bool,
    symbol_count: usize,
}

impl Scan {
    fn facts(&self) -> Option<FileFacts<'_>> {
        match self {
            Scan::Unchanged(stored) | Scan::Restamped(stored) => Some(FileFacts {
                path: &stored.path,
                language: stored.language,
                content_hash: &stored.content_hash,
                has_syntax_errors: stored.has_syntax_errors,
                symbol_count: stored.symbol_count,
            }),
            Scan::Parsed(parsed) => Some(FileFacts {
                path: &parsed.file.path,
                language: parsed.file.language,
                content_hash: &parsed.file.content_hash,
                has_syntax_errors: parsed.file.has_syntax_errors,
                symbol_count: parsed.file.symbols.len(),
            }),
            Scan::Unreadable(_) => None,
        }
    }
}

fn known_files(files: Vec<StoredFile>) -> HashMap<String, StoredFile> {
    files
        .into_iter()
        .map(|file| (file.path.clone(), file))
        .collect()
}

/// Hash of the extractor version and every file's path and content hash.
/// Scans must already be sorted by path.
fn snapshot_fingerprint(scans: &[Scan]) -> String {
    let mut hasher = blake3::Hasher::new();
    hasher.update(EXTRACTOR_VERSION.as_bytes());
    hasher.update(b"\n");
    for facts in scans.iter().filter_map(Scan::facts) {
        hasher.update(facts.path.as_bytes());
        hasher.update(b"\0");
        hasher.update(facts.content_hash.as_bytes());
        hasher.update(b"\n");
    }
    hasher.finalize().to_hex().to_string()
}

fn summarize(
    snapshot_id: i64,
    snapshot: Snapshot,
    is_unchanged: bool,
    scans: &[Scan],
) -> IndexSummary {
    let mut files_by_language = BTreeMap::new();
    let mut symbol_count = 0;
    let mut files_with_syntax_errors = Vec::new();
    let mut unreadable_files = Vec::new();
    for scan in scans {
        let Some(facts) = scan.facts() else {
            if let Scan::Unreadable(path) = scan {
                unreadable_files.push(path.clone());
            }
            continue;
        };
        *files_by_language.entry(facts.language).or_insert(0) += 1;
        symbol_count += facts.symbol_count;
        if facts.has_syntax_errors {
            files_with_syntax_errors.push(facts.path.to_owned());
        }
    }
    IndexSummary {
        snapshot_id,
        snapshot,
        is_unchanged,
        files_by_language,
        symbol_count,
        files_with_syntax_errors,
        unreadable_files,
    }
}

fn read_utf8(path: &Path) -> Option<String> {
    let bytes = fs::read(path).ok()?;
    String::from_utf8(bytes).ok()
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
    fn deleted_files_leave_the_index() {
        let repository = sample_repository();
        let mut store = Store::open_in_memory().unwrap();
        index_directory(repository.path(), &mut store).unwrap();

        fs::remove_file(repository.path().join("go/main.go")).unwrap();
        let summary = index_directory(repository.path(), &mut store).unwrap();

        assert!(!summary.files_by_language.contains_key(&Language::Go));
        assert_eq!(symbol_ids(&store, summary.snapshot_id, "Delete"), []);
    }

    fn stored_file(stamp: FileStamp) -> StoredFile {
        StoredFile {
            path: "a.py".to_owned(),
            language: Language::Python,
            content_hash: "hash".to_owned(),
            has_syntax_errors: false,
            symbol_count: 1,
            stamp,
        }
    }

    fn stamp(modified_ns: i64) -> FileStamp {
        FileStamp {
            size_bytes: 10,
            modified_ns,
            changed_ns: modified_ns,
            inode: 1,
        }
    }

    fn policy(
        known: &HashMap<String, StoredFile>,
        trusted_before_ns: Option<i64>,
    ) -> ReusePolicy<'_> {
        ReusePolicy {
            known,
            trusted_before_ns,
        }
    }

    #[test]
    fn matching_stamps_from_before_the_last_run_are_trusted() {
        let known = HashMap::new();
        let reuse = policy(&known, Some(1_000));

        assert!(reuse.trusts(&stored_file(stamp(500)), stamp(500)));
    }

    #[test]
    fn stamps_written_during_the_last_run_are_not_trusted() {
        let known = HashMap::new();
        let reuse = policy(&known, Some(1_000));

        assert!(!reuse.trusts(&stored_file(stamp(1_000)), stamp(1_000)));
    }

    #[test]
    fn different_stamps_are_not_trusted() {
        let known = HashMap::new();
        let reuse = policy(&known, Some(1_000));

        assert!(!reuse.trusts(&stored_file(stamp(500)), stamp(600)));
    }

    #[test]
    fn nothing_is_trusted_without_a_previous_run() {
        let known = HashMap::new();
        let reuse = policy(&known, None);

        assert!(!reuse.trusts(&stored_file(stamp(500)), stamp(500)));
    }
}
