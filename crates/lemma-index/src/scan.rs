//! Reading and parsing discovered files, reusing stored results where the
//! file cannot have changed.

use std::collections::BTreeMap;
use std::fs;
use std::path::Path;
use std::sync::{Mutex, PoisonError};

use lemma_model::{IndexedFile, Language, content_hash};
use lemma_store::{FileStamp, StampedFile, StoredFile};
use lemma_syntax::{Extraction, Extractor};
use rayon::iter::{IntoParallelIterator, ParallelIterator};

use crate::IndexError;
use crate::discover::SourceFile;

/// What scanning one discovered file found.
pub(crate) enum Scan {
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
pub(crate) struct ReusePolicy<'a> {
    pub known: &'a BTreeMap<String, StoredFile>,
    /// When the previous indexing run started, if there was one.
    pub trusted_before_ns: Option<i64>,
}

impl ReusePolicy<'_> {
    /// The stored file for this path, if it is the same language.
    fn stored(&self, file: &SourceFile) -> Option<&StoredFile> {
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
    pub fn trusts(&self, stored: &StoredFile, stamp: FileStamp) -> bool {
        let Some(trusted_before_ns) = self.trusted_before_ns else {
            return false;
        };
        stored.stamp == stamp && stamp.modified_ns.max(stamp.changed_ns) < trusted_before_ns
    }
}

/// Scans files on every core. `collect` keeps the discovery order, which
/// keeps the snapshot deterministic.
pub(crate) fn scan_files(
    discovered: Vec<SourceFile>,
    reuse: &ReusePolicy<'_>,
    extractors: &ExtractorPool,
) -> Result<Vec<Scan>, IndexError> {
    let scans: Vec<Result<Scan, IndexError>> = discovered
        .into_par_iter()
        .map(|file| scan_file(extractors, file, reuse))
        .collect();
    scans.into_iter().collect()
}

/// Reuses, rehashes, or parses one file.
fn scan_file(
    extractors: &ExtractorPool,
    file: SourceFile,
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
/// threads. A long-lived index keeps its pool, so later edits pay nothing.
pub(crate) struct ExtractorPool {
    slots: Vec<Mutex<Option<Extractor>>>,
}

impl ExtractorPool {
    pub fn new() -> Self {
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

fn read_utf8(path: &Path) -> Option<String> {
    let bytes = fs::read(path).ok()?;
    String::from_utf8(bytes).ok()
}

#[cfg(test)]
mod tests {
    use super::*;

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
        known: &BTreeMap<String, StoredFile>,
        trusted_before_ns: Option<i64>,
    ) -> ReusePolicy<'_> {
        ReusePolicy {
            known,
            trusted_before_ns,
        }
    }

    #[test]
    fn matching_stamps_from_before_the_last_run_are_trusted() {
        let known = BTreeMap::new();
        let reuse = policy(&known, Some(1_000));

        assert!(reuse.trusts(&stored_file(stamp(500)), stamp(500)));
    }

    #[test]
    fn stamps_written_during_the_last_run_are_not_trusted() {
        let known = BTreeMap::new();
        let reuse = policy(&known, Some(1_000));

        assert!(!reuse.trusts(&stored_file(stamp(1_000)), stamp(1_000)));
    }

    #[test]
    fn different_stamps_are_not_trusted() {
        let known = BTreeMap::new();
        let reuse = policy(&known, Some(1_000));

        assert!(!reuse.trusts(&stored_file(stamp(500)), stamp(600)));
    }

    #[test]
    fn nothing_is_trusted_without_a_previous_run() {
        let known = BTreeMap::new();
        let reuse = policy(&known, None);

        assert!(!reuse.trusts(&stored_file(stamp(500)), stamp(500)));
    }
}
