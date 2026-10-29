//! Substring search over every symbol name, in memory.
//!
//! SQLite can only answer "names containing this text" by reading every
//! symbol, which takes about 75 ms on half a million symbols. Keeping the
//! names in one lowercase buffer and searching it with a vectorised
//! substring search takes a few milliseconds, at the cost of holding the
//! names in memory, which only a long-lived process can amortise.

use std::cmp::Ordering;
use std::collections::HashMap;

use lemma_model::{Language, SymbolKind};
use lemma_store::{Store, StoreError, StoredSnapshot, StoredSymbol, SymbolFilter};
use memchr::memmem;

/// Separates names in the search buffer. Names never contain it.
const SEPARATOR: u8 = b'\n';

/// Every symbol name of one snapshot, ready to search.
pub struct SymbolSearch {
    snapshot_id: i64,
    fingerprint: String,
    /// Lowercase qualified names, each followed by [`SEPARATOR`].
    lowercase: Vec<u8>,
    /// Where each symbol's name starts in `lowercase`, in entry order.
    starts: Vec<usize>,
    entries: Vec<Entry>,
    files: Vec<FileEntry>,
}

struct Entry {
    id: i64,
    qualified_name: String,
    name_length: usize,
    kind: SymbolKind,
    file: usize,
    start_line: u32,
}

struct FileEntry {
    path: String,
    language: Language,
}

impl SymbolSearch {
    /// Reads every symbol name of the snapshot.
    pub fn load(store: &Store, snapshot: &StoredSnapshot) -> Result<Self, StoreError> {
        let file_keys = store.file_keys(snapshot.id)?;
        let mut file_index = HashMap::with_capacity(file_keys.len());
        let mut files = Vec::with_capacity(file_keys.len());
        for key in file_keys {
            file_index.insert(key.id, files.len());
            files.push(FileEntry {
                path: key.path,
                language: key.language,
            });
        }

        let keys = store.symbol_keys(snapshot.id)?;
        let mut lowercase = Vec::new();
        let mut starts = Vec::with_capacity(keys.len());
        let mut entries = Vec::with_capacity(keys.len());
        for key in keys {
            let Some(file) = file_index.get(&key.file_id).copied() else {
                continue;
            };
            starts.push(lowercase.len());
            lowercase.extend(
                key.qualified_name
                    .bytes()
                    .map(|byte| byte.to_ascii_lowercase()),
            );
            lowercase.push(SEPARATOR);
            entries.push(Entry {
                id: key.id,
                qualified_name: key.qualified_name,
                name_length: key.name_length,
                kind: key.kind,
                file,
                start_line: key.start_line,
            });
        }
        Ok(Self {
            snapshot_id: snapshot.id,
            fingerprint: snapshot.snapshot.fingerprint.clone(),
            lowercase,
            starts,
            entries,
            files,
        })
    }

    /// Whether this was built from exactly this snapshot.
    pub fn is_for(&self, snapshot: &StoredSnapshot) -> bool {
        self.snapshot_id == snapshot.id && self.fingerprint == snapshot.snapshot.fingerprint
    }

    /// Symbols whose qualified name contains the filter's text, ignoring
    /// ASCII case, ranked like the SQL search: exact names first, then
    /// shorter qualified names, then by path and line.
    pub fn find(
        &self,
        store: &Store,
        filter: &SymbolFilter,
    ) -> Result<Vec<StoredSymbol>, StoreError> {
        let needle = filter.text.to_ascii_lowercase();
        let mut matches: Vec<&Entry> = self
            .matching_entries(needle.as_bytes())
            .filter(|entry| self.passes(entry, filter))
            .collect();
        let limit = usize::try_from(filter.limit).unwrap_or(usize::MAX);
        let by_rank = |left: &&Entry, right: &&Entry| self.rank(left, right, &filter.text);
        if matches.len() > limit {
            matches.select_nth_unstable_by(limit, by_rank);
            matches.truncate(limit);
        }
        matches.sort_by(by_rank);
        let ids: Vec<i64> = matches.iter().map(|entry| entry.id).collect();
        store.symbols_by_ids(self.snapshot_id, &ids)
    }

    /// Entries whose lowercase name contains `needle`, each once.
    fn matching_entries<'a>(&'a self, needle: &'a [u8]) -> impl Iterator<Item = &'a Entry> + 'a {
        let mut last = None;
        memmem::find_iter(&self.lowercase, needle).filter_map(move |offset| {
            let index = self.starts.partition_point(|start| *start <= offset) - 1;
            let end = self
                .starts
                .get(index + 1)
                .map_or(self.lowercase.len(), |next| next - 1);
            if offset + needle.len() > end || last == Some(index) {
                return None;
            }
            last = Some(index);
            self.entries.get(index)
        })
    }

    fn passes(&self, entry: &Entry, filter: &SymbolFilter) -> bool {
        let file = &self.files[entry.file];
        filter.kind.is_none_or(|kind| kind == entry.kind)
            && filter
                .language
                .is_none_or(|language| language == file.language)
            && filter
                .path_contains
                .as_deref()
                .is_none_or(|text| file.path.contains(text))
    }

    fn rank(&self, left: &Entry, right: &Entry, text: &str) -> Ordering {
        let is_exact = |entry: &Entry| {
            let name_start = entry.qualified_name.len() - entry.name_length;
            entry.qualified_name == text || entry.qualified_name.get(name_start..) == Some(text)
        };
        is_exact(right)
            .cmp(&is_exact(left))
            .then(left.qualified_name.len().cmp(&right.qualified_name.len()))
            .then_with(|| self.files[left.file].path.cmp(&self.files[right.file].path))
            .then(left.start_line.cmp(&right.start_line))
    }
}
