//! What checks keep between runs, filed in a store's attribute table:
//! units (one producer's reading of one input), the terms each unit is
//! indexed by, and segments built from many units. Keys are strings naming
//! everything a value depends on, so a value is never wrong, only absent.

use std::collections::{BTreeMap, BTreeSet};
use std::path::{Path, PathBuf};
use std::sync::mpsc;
use std::sync::{Arc, RwLock};
use std::thread::JoinHandle;

use rayon::prelude::*;

use crate::attributes::Run;
use crate::codec::{Decoder, Encoder};
use crate::read::{Index, ReadMode};
use crate::{Digest, StoreError, Writer};

/// What names a segment: what it is of, and a digest of its inputs.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SegmentKey {
    /// What the segment is of, such as one repository's functions.
    pub group: String,
    /// A digest of everything the segment was built from.
    pub inputs: String,
}

/// Units, terms and segments in one store file.
pub struct Cache {
    path: PathBuf,
    current: RwLock<Option<Arc<Opened>>>,
    writer: Option<Background>,
}

/// The store as last opened, with its runs.
struct Opened {
    index: Index,
    runs: Vec<Run>,
    length: u64,
}

struct Background {
    batches: mpsc::Sender<Vec<(Digest, Vec<u8>)>>,
    thread: JoinHandle<()>,
}

fn get(opened: &Opened, key: &Digest) -> Result<Option<Vec<u8>>, StoreError> {
    opened.index.attribute_in(&opened.runs, key)
}

fn unit_key(key: &str) -> Digest {
    namespaced(b"unit", key)
}

fn terms_key(unit: &str) -> Digest {
    namespaced(b"terms", unit)
}

fn segment_key(key: &SegmentKey) -> Digest {
    namespaced(b"segment", &format!("{}\0{}", key.group, key.inputs))
}

fn namespaced(space: &[u8], key: &str) -> Digest {
    let mut hasher = blake3::Hasher::new();
    hasher.update(space);
    hasher.update(&[0]);
    hasher.update(key.as_bytes());
    *hasher.finalize().as_bytes()
}

impl Cache {
    /// The cache in the store at `path`, created when first written.
    pub fn open(path: &Path) -> Result<Self, StoreError> {
        Ok(Self {
            path: path.to_owned(),
            current: RwLock::new(None),
            writer: None,
        })
    }

    /// The store as it is now, reopened only when it has grown.
    fn opened(&self) -> Result<Option<Arc<Opened>>, StoreError> {
        let length = match std::fs::metadata(&self.path) {
            Ok(metadata) => metadata.len(),
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(None),
            Err(source) => {
                return Err(StoreError::Io {
                    path: self.path.clone(),
                    source,
                });
            }
        };
        if let Ok(current) = self.current.read()
            && let Some(opened) = current.as_ref().filter(|opened| opened.length == length)
        {
            return Ok(Some(Arc::clone(opened)));
        }
        let index = Index::open(&self.path, ReadMode::Mapped)?;
        let runs = index.runs()?;
        let opened = Arc::new(Opened {
            index,
            runs,
            length,
        });
        if let Ok(mut current) = self.current.write() {
            *current = Some(Arc::clone(&opened));
        }
        Ok(Some(opened))
    }

    /// The values kept for those of `keys` the cache holds.
    pub fn get_many(&self, keys: &[String]) -> Result<BTreeMap<String, Vec<u8>>, StoreError> {
        self.get_decoded(keys, |bytes| Some(bytes.to_vec()))
    }

    /// The units kept under those of `keys` the cache holds, each turned
    /// into a value by `decode` in parallel. A unit `decode` rejects is left
    /// out, as if it were not kept.
    pub fn get_decoded<T: Send>(
        &self,
        keys: &[String],
        decode: impl Fn(&[u8]) -> Option<T> + Sync,
    ) -> Result<BTreeMap<String, T>, StoreError> {
        let Some(opened) = self.opened()? else {
            return Ok(BTreeMap::new());
        };
        let found: Vec<Option<(String, T)>> = keys
            .par_iter()
            .map(|key| {
                let bytes = get(&opened, &unit_key(key))?;
                Ok(bytes.and_then(|bytes| Some((key.clone(), decode(&bytes)?))))
            })
            .collect::<Result<_, StoreError>>()?;
        Ok(found.into_iter().flatten().collect())
    }

    /// Keeps `entries` now.
    pub fn put_many(&mut self, entries: &[(String, Vec<u8>)]) -> Result<(), StoreError> {
        let entries: Vec<(Digest, Vec<u8>)> = entries
            .iter()
            .map(|(key, value)| (unit_key(key), value.clone()))
            .collect();
        Writer::open_waiting(&self.path)?.put_attributes(&entries)
    }

    /// Keeps `entries` in the background while the caller carries on;
    /// dropping the cache waits for every batch to be written. Keeping is
    /// best effort: a batch that cannot be written only costs a later run
    /// the work of producing it again.
    pub fn put_later(&mut self, entries: Vec<(String, Vec<u8>)>) {
        let entries = entries
            .into_iter()
            .map(|(key, value)| (unit_key(&key), value))
            .collect();
        self.send(entries);
    }

    /// Indexes each unit by its terms in the background, recording it as
    /// indexed even with no terms.
    pub fn put_terms_later(&mut self, terms: Vec<(String, Vec<String>)>) {
        let entries = terms
            .into_iter()
            .map(|(unit, unit_terms)| (terms_key(&unit), encode_terms(&unit_terms)))
            .collect();
        self.send(entries);
    }

    /// Keeps a segment in the background.
    pub fn put_segment_later(&mut self, key: &SegmentKey, bytes: Vec<u8>) {
        self.send(vec![(segment_key(key), bytes)]);
    }

    fn send(&mut self, entries: Vec<(Digest, Vec<u8>)>) {
        if entries.is_empty() {
            return;
        }
        let path = &self.path;
        let writer = self.writer.get_or_insert_with(|| start_writer(path));
        // A writer that has stopped drops what it is sent.
        let _ = writer.batches.send(entries);
    }

    /// The segment kept under `key`, if there is one.
    pub fn segment(&self, key: &SegmentKey) -> Result<Option<Vec<u8>>, StoreError> {
        let Some(opened) = self.opened()? else {
            return Ok(None);
        };
        get(&opened, &segment_key(key))
    }

    /// The terms of those of `units` that are indexed, by unit.
    fn indexed_terms(&self, units: &[String]) -> Result<BTreeMap<String, Vec<String>>, StoreError> {
        let Some(opened) = self.opened()? else {
            return Ok(BTreeMap::new());
        };
        let found: Vec<Option<(String, Vec<String>)>> = units
            .par_iter()
            .map(|unit| {
                let Some(bytes) = get(&opened, &terms_key(unit))? else {
                    return Ok(None);
                };
                Ok(Some((unit.clone(), decode_terms(&bytes)?)))
            })
            .collect::<Result<_, StoreError>>()?;
        Ok(found.into_iter().flatten().collect())
    }

    /// Those of `units` whose terms are written.
    pub fn indexed(&self, units: &[String]) -> Result<BTreeSet<String>, StoreError> {
        Ok(self.indexed_terms(units)?.into_keys().collect())
    }

    /// The terms each of `units` is indexed by; a unit indexed by none, or
    /// not indexed, is left out.
    pub fn terms_of(&self, units: &[String]) -> Result<BTreeMap<String, Vec<String>>, StoreError> {
        let mut found = self.indexed_terms(units)?;
        found.retain(|_, terms| !terms.is_empty());
        Ok(found)
    }

    /// Those of `among` indexed by any of `terms`.
    pub fn units_with(
        &self,
        terms: &[String],
        among: &BTreeSet<String>,
    ) -> Result<BTreeSet<String>, StoreError> {
        let wanted: BTreeSet<&str> = terms.iter().map(String::as_str).collect();
        let among: Vec<String> = among.iter().cloned().collect();
        Ok(self
            .indexed_terms(&among)?
            .into_iter()
            .filter(|(_, unit_terms)| unit_terms.iter().any(|term| wanted.contains(term.as_str())))
            .map(|(unit, _)| unit)
            .collect())
    }
}

impl Drop for Cache {
    fn drop(&mut self) {
        let Some(Background { batches, thread }) = self.writer.take() else {
            return;
        };
        drop(batches);
        // The writer only ends early by panicking, which loses no more
        // than its unwritten batches.
        let _ = thread.join();
    }
}

/// Starts a thread writing what it is sent to the store at `path`, taking
/// together whatever arrived while it wrote, one commit at a time.
fn start_writer(path: &Path) -> Background {
    let (batches, received) = mpsc::channel::<Vec<(Digest, Vec<u8>)>>();
    let path = path.to_owned();
    let thread = std::thread::spawn(move || {
        for first in &received {
            let mut entries = first;
            while let Ok(more) = received.try_recv() {
                entries.extend(more);
            }
            // Best effort, as `put_later` says.
            if let Ok(mut writer) = Writer::open_waiting(&path) {
                let _ = writer.put_attributes(&entries);
            }
        }
    });
    Background { batches, thread }
}

fn encode_terms(terms: &[String]) -> Vec<u8> {
    let mut encoder = Encoder::default();
    encoder.count(terms.len());
    for term in terms {
        encoder.text(term);
    }
    encoder.bytes
}

fn decode_terms(bytes: &[u8]) -> Result<Vec<String>, StoreError> {
    let mut decoder = Decoder::new(bytes);
    let count = decoder.count(4)?;
    (0..count)
        .map(|_| decoder.text().map(str::to_owned))
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn cache_in(directory: &tempfile::TempDir) -> Cache {
        Cache::open(&directory.path().join("index.ls")).unwrap()
    }

    #[test]
    fn a_kept_unit_is_read_back() {
        let directory = tempfile::tempdir().unwrap();
        let mut cache = cache_in(&directory);

        cache
            .put_many(&[("a".to_owned(), b"one".to_vec())])
            .unwrap();

        let found = cache.get_many(&["a".to_owned(), "b".to_owned()]).unwrap();
        assert_eq!(found, BTreeMap::from([("a".to_owned(), b"one".to_vec())]));
    }

    #[test]
    fn a_later_value_replaces_an_earlier_one_across_merges() {
        let directory = tempfile::tempdir().unwrap();
        let mut cache = cache_in(&directory);

        for round in 0..20u8 {
            let entries = [
                ("same".to_owned(), vec![round]),
                (format!("only{round}"), vec![round]),
            ];
            cache.put_many(&entries).unwrap();
        }

        let keys: Vec<String> = (0..20).map(|round| format!("only{round}")).collect();
        assert_eq!(cache.get_many(&keys).unwrap().len(), 20);
        assert_eq!(cache.get_many(&["same".to_owned()]).unwrap()["same"], [19]);
    }

    #[test]
    fn units_kept_in_the_background_are_there_once_the_cache_is_dropped() {
        let directory = tempfile::tempdir().unwrap();
        let mut cache = cache_in(&directory);
        cache.put_later(vec![("a".to_owned(), b"one".to_vec())]);
        cache.put_terms_later(vec![
            ("a".to_owned(), vec!["path:/x".to_owned()]),
            ("b".to_owned(), Vec::new()),
        ]);
        let key = SegmentKey {
            group: "functions".to_owned(),
            inputs: "digest".to_owned(),
        };
        cache.put_segment_later(&key, b"segment".to_vec());
        drop(cache);

        let cache = cache_in(&directory);
        let units = ["a".to_owned(), "b".to_owned(), "c".to_owned()];
        assert_eq!(cache.get_many(&units).unwrap().len(), 1);
        assert_eq!(
            cache.indexed(&units).unwrap(),
            BTreeSet::from(["a".to_owned(), "b".to_owned()])
        );
        assert_eq!(cache.terms_of(&units).unwrap().len(), 1);
        let among = BTreeSet::from(["a".to_owned(), "b".to_owned()]);
        let hits = cache.units_with(&["path:/x".to_owned()], &among).unwrap();
        assert_eq!(hits, BTreeSet::from(["a".to_owned()]));
        assert_eq!(cache.segment(&key).unwrap(), Some(b"segment".to_vec()));
    }

    #[test]
    fn a_missing_store_holds_nothing() {
        let directory = tempfile::tempdir().unwrap();
        let cache = cache_in(&directory);

        assert!(cache.get_many(&["a".to_owned()]).unwrap().is_empty());
        assert_eq!(
            cache
                .segment(&SegmentKey {
                    group: "g".into(),
                    inputs: "i".into()
                })
                .unwrap(),
            None
        );
    }

    #[test]
    fn attributes_leave_committed_sources_readable() {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("index.ls");
        let source = crate::SourceRecord {
            repository: "r".into(),
            revision: "v".into(),
            profile: [0; 32],
        };
        let commit = crate::Commit {
            root: "head",
            parent: None,
            source: &source,
        };
        Writer::open(&path).unwrap().commit(&commit, &[]).unwrap();
        let mut cache = Cache::open(&path).unwrap();

        cache
            .put_many(&[("a".to_owned(), b"one".to_vec())])
            .unwrap();
        Writer::open(&path)
            .unwrap()
            .commit(
                &crate::Commit {
                    root: "work",
                    parent: Some("head"),
                    source: &source,
                },
                &[],
            )
            .unwrap();

        let index = Index::open(&path, ReadMode::Mapped).unwrap();
        assert_eq!(index.root_names().unwrap(), ["head", "work"]);
        assert_eq!(cache.get_many(&["a".to_owned()]).unwrap().len(), 1);
    }
}
