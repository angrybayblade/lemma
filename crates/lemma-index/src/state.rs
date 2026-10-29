//! The indexed files of a directory, kept so that a change costs work in
//! proportion to the change, not to the size of the repository.

use std::collections::{BTreeMap, BTreeSet, HashSet};

use lemma_model::Language;
use lemma_store::{FileStamp, StampedFile, StoredFile};
use lemma_syntax::EXTRACTOR_VERSION;

use crate::scan::Scan;
use crate::scope::DirtyScope;

/// Every indexed file, with running totals.
#[derive(Debug, Default)]
pub(crate) struct FileSet {
    files: BTreeMap<String, StoredFile>,
    fingerprint: FingerprintSum,
    files_by_language: BTreeMap<Language, usize>,
    symbol_count: usize,
    files_with_syntax_errors: BTreeSet<String>,
    unreadable_files: BTreeSet<String>,
}

/// Which files a scan looked at.
#[derive(Debug, Clone, Copy)]
pub(crate) enum Coverage<'a> {
    All,
    Scope(&'a DirtyScope),
}

impl Coverage<'_> {
    fn covers(self, path: &str) -> bool {
        match self {
            Coverage::All => true,
            Coverage::Scope(scope) => scope.covers(path),
        }
    }
}

/// How the indexed files differ from what a scan found.
#[derive(Debug, Default)]
pub(crate) struct ChangeSet {
    /// New files, and files whose contents changed.
    pub changed: Vec<StampedFile>,
    /// Files whose contents are the same but whose stamp moved on.
    pub restamped: Vec<(String, FileStamp)>,
    /// Files gone, or no longer readable.
    pub removed: Vec<String>,
    /// Unreadable files among those scanned.
    unreadable: Vec<String>,
}

impl ChangeSet {
    /// Paths whose contents changed, for asking Git about them.
    pub fn touched_paths(&self) -> impl Iterator<Item = &str> {
        let changed = self
            .changed
            .iter()
            .map(|changed| changed.file.path.as_str());
        changed.chain(self.removed.iter().map(String::as_str))
    }
}

impl FileSet {
    /// A file set built from stored files, as when a process first opens an
    /// index another run wrote.
    pub fn from_stored(files: Vec<StoredFile>) -> Self {
        let mut set = Self::default();
        for file in files {
            set.insert(file);
        }
        set
    }

    pub fn files(&self) -> &BTreeMap<String, StoredFile> {
        &self.files
    }

    /// The snapshot fingerprint: changes whenever any file's path or
    /// contents, or the extractor, change.
    #[cfg(test)]
    pub fn fingerprint(&self) -> String {
        fingerprint_of(self.fingerprint)
    }

    /// The fingerprint the set will have once `changes` is applied.
    pub fn fingerprint_after(&self, changes: &ChangeSet) -> String {
        let mut sum = self.fingerprint;
        for path in &changes.removed {
            if let Some(file) = self.files.get(path) {
                sum.subtract(&file.path, &file.content_hash);
            }
        }
        for changed in &changes.changed {
            if let Some(file) = self.files.get(&changed.file.path) {
                sum.subtract(&file.path, &file.content_hash);
            }
            sum.add(&changed.file.path, &changed.file.content_hash);
        }
        fingerprint_of(sum)
    }

    pub fn files_by_language(&self) -> &BTreeMap<Language, usize> {
        &self.files_by_language
    }

    pub fn symbol_count(&self) -> usize {
        self.symbol_count
    }

    pub fn files_with_syntax_errors(&self) -> Vec<String> {
        self.files_with_syntax_errors.iter().cloned().collect()
    }

    pub fn unreadable_files(&self) -> Vec<String> {
        self.unreadable_files.iter().cloned().collect()
    }

    /// What differs between these files and a scan of `coverage`. Files
    /// outside the coverage were not looked at and are left alone.
    pub fn changes(&self, scans: Vec<Scan>, coverage: Coverage<'_>) -> ChangeSet {
        let mut changes = ChangeSet::default();
        let mut readable = HashSet::new();
        for scan in scans {
            match scan {
                Scan::Unchanged(stored) => {
                    readable.insert(stored.path);
                }
                Scan::Restamped(stored) => {
                    changes.restamped.push((stored.path.clone(), stored.stamp));
                    readable.insert(stored.path);
                }
                Scan::Parsed(parsed) => {
                    readable.insert(parsed.file.path.clone());
                    changes.changed.push(parsed);
                }
                Scan::Unreadable(path) => changes.unreadable.push(path),
            }
        }
        changes.removed = self
            .covered_paths(coverage)
            .filter(|path| !readable.contains(*path))
            .map(str::to_owned)
            .collect();
        changes
    }

    /// Brings the set up to date with a change set computed against it.
    pub fn apply(&mut self, changes: ChangeSet, coverage: Coverage<'_>) {
        for path in &changes.removed {
            self.remove(path);
        }
        for (path, stamp) in changes.restamped {
            if let Some(file) = self.files.get_mut(&path) {
                file.stamp = stamp;
            }
        }
        for StampedFile { file, stamp } in changes.changed {
            self.remove(&file.path);
            self.insert(StoredFile {
                symbol_count: file.symbols.len(),
                path: file.path,
                language: file.language,
                content_hash: file.content_hash,
                has_syntax_errors: file.has_syntax_errors,
                stamp,
            });
        }
        self.unreadable_files.retain(|path| !coverage.covers(path));
        self.unreadable_files.extend(changes.unreadable);
    }

    fn insert(&mut self, file: StoredFile) {
        self.fingerprint.add(&file.path, &file.content_hash);
        *self.files_by_language.entry(file.language).or_insert(0) += 1;
        self.symbol_count += file.symbol_count;
        if file.has_syntax_errors {
            self.files_with_syntax_errors.insert(file.path.clone());
        }
        self.files.insert(file.path.clone(), file);
    }

    fn remove(&mut self, path: &str) {
        let Some(file) = self.files.remove(path) else {
            return;
        };
        self.fingerprint.subtract(&file.path, &file.content_hash);
        if let Some(count) = self.files_by_language.get_mut(&file.language) {
            *count -= 1;
            if *count == 0 {
                self.files_by_language.remove(&file.language);
            }
        }
        self.symbol_count -= file.symbol_count;
        self.files_with_syntax_errors.remove(&file.path);
    }

    /// Indexed paths a scan of `coverage` looked at. Uses the sorted map to
    /// visit only the directories in scope, not every file.
    fn covered_paths<'a>(
        &'a self,
        coverage: Coverage<'a>,
    ) -> Box<dyn Iterator<Item = &'a str> + 'a> {
        let Coverage::Scope(scope) = coverage else {
            return Box::new(self.files.keys().map(String::as_str));
        };
        let mut paths = BTreeSet::new();
        for directory in scope.listed.iter().chain(&scope.walked) {
            if directory.is_empty() {
                paths.extend(
                    self.files
                        .keys()
                        .map(String::as_str)
                        .filter(|path| scope.covers(path)),
                );
                continue;
            }
            let prefix = format!("{directory}/");
            let below = self
                .files
                .range(prefix.clone()..)
                .map(|(path, _)| path.as_str())
                .take_while(|path| path.starts_with(&prefix))
                .filter(|path| scope.covers(path));
            paths.extend(below);
        }
        Box::new(paths.into_iter())
    }
}

fn fingerprint_of(sum: FingerprintSum) -> String {
    let mut hasher = blake3::Hasher::new();
    hasher.update(EXTRACTOR_VERSION.as_bytes());
    hasher.update(b"\0");
    for lane in sum.0 {
        hasher.update(&lane.to_le_bytes());
    }
    hasher.finalize().to_hex().to_string()
}

/// An order-independent sum of per-file hashes, so adding or removing one
/// file updates the fingerprint without rehashing every other file.
#[derive(Debug, Default, Clone, Copy, PartialEq, Eq)]
struct FingerprintSum([u64; 4]);

impl FingerprintSum {
    fn add(&mut self, path: &str, content_hash: &str) {
        let lanes = file_lanes(path, content_hash);
        for (lane, value) in self.0.iter_mut().zip(lanes) {
            *lane = lane.wrapping_add(value);
        }
    }

    fn subtract(&mut self, path: &str, content_hash: &str) {
        let lanes = file_lanes(path, content_hash);
        for (lane, value) in self.0.iter_mut().zip(lanes) {
            *lane = lane.wrapping_sub(value);
        }
    }
}

fn file_lanes(path: &str, content_hash: &str) -> [u64; 4] {
    let mut hasher = blake3::Hasher::new();
    hasher.update(path.as_bytes());
    hasher.update(b"\0");
    hasher.update(content_hash.as_bytes());
    let bytes = hasher.finalize();
    let mut lanes = [0; 4];
    for (lane, chunk) in lanes.iter_mut().zip(bytes.as_bytes().chunks_exact(8)) {
        let mut word = [0; 8];
        word.copy_from_slice(chunk);
        *lane = u64::from_le_bytes(word);
    }
    lanes
}

#[cfg(test)]
mod tests {
    use super::*;

    fn stored(path: &str, hash: &str) -> StoredFile {
        StoredFile {
            path: path.to_owned(),
            language: Language::Python,
            content_hash: hash.to_owned(),
            has_syntax_errors: false,
            symbol_count: 2,
            stamp: FileStamp::default(),
        }
    }

    #[test]
    fn the_fingerprint_depends_on_contents_not_insertion_order() {
        let forward = FileSet::from_stored(vec![stored("a.py", "1"), stored("b.py", "2")]);
        let backward = FileSet::from_stored(vec![stored("b.py", "2"), stored("a.py", "1")]);
        let changed = FileSet::from_stored(vec![stored("a.py", "1"), stored("b.py", "3")]);

        assert_eq!(forward.fingerprint(), backward.fingerprint());
        assert_ne!(forward.fingerprint(), changed.fingerprint());
    }

    #[test]
    fn removing_and_re_adding_restores_the_fingerprint_and_totals() {
        let mut set = FileSet::from_stored(vec![stored("a.py", "1"), stored("b.py", "2")]);
        let before = set.fingerprint();

        set.remove("b.py");
        assert_eq!(set.symbol_count(), 2);
        set.insert(stored("b.py", "2"));

        assert_eq!(set.fingerprint(), before);
        assert_eq!(set.fingerprint_after(&ChangeSet::default()), before);
        assert_eq!(set.symbol_count(), 4);
        assert_eq!(set.files_by_language()[&Language::Python], 2);
    }

    #[test]
    fn a_partial_scan_removes_only_missing_files_it_covered() {
        let set = FileSet::from_stored(vec![stored("src/a.py", "1"), stored("lib/b.py", "2")]);
        let scope = DirtyScope {
            listed: BTreeSet::from(["src".to_owned()]),
            ..DirtyScope::default()
        };

        let changes = set.changes(Vec::new(), Coverage::Scope(&scope));

        assert_eq!(changes.removed, ["src/a.py"]);
    }
}
