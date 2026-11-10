//! The attribute table (spec 17): values filed under 32-byte keys, beside
//! the tree rather than inside it, so storing one never changes a digest.
//!
//! Each write appends its values as nodes, then a run: the write's keys,
//! sorted, each with the offset of its value. The footer names the list of
//! runs, newest first, and a lookup searches them in that order, so a newer
//! value for a key hides an older one. When the list grows past a few runs,
//! the writer merges them into one; values are never copied.

use std::collections::BTreeMap;
use std::sync::Arc;

use crate::codec::{Decoder, Encoder};
use crate::read::{self, Index};
use crate::write::{Pending, Writer};
use crate::{Digest, StoreError};

/// Bytes of one run entry: a key and its value's offset.
const ENTRY_BYTES: usize = 40;

/// How many runs a lookup may search before a write merges them.
const MAX_RUNS: usize = 8;

/// A run's entries, sorted by key.
pub(crate) struct Run {
    bytes: Arc<Vec<u8>>,
}

impl Run {
    fn len(&self) -> usize {
        (self.bytes.len() - 4) / ENTRY_BYTES
    }

    fn entry(&self, position: usize) -> (&[u8], u64) {
        let start = 4 + position * ENTRY_BYTES;
        let key = &self.bytes[start..start + 32];
        let mut offset = [0; 8];
        offset.copy_from_slice(&self.bytes[start + 32..start + ENTRY_BYTES]);
        (key, u64::from_le_bytes(offset))
    }

    fn find(&self, key: &Digest) -> Option<u64> {
        let (mut low, mut high) = (0, self.len());
        while low < high {
            let middle = low + (high - low) / 2;
            let (found, offset) = self.entry(middle);
            match found.cmp(key.as_slice()) {
                std::cmp::Ordering::Equal => return Some(offset),
                std::cmp::Ordering::Less => low = middle + 1,
                std::cmp::Ordering::Greater => high = middle,
            }
        }
        None
    }
}

impl Index {
    /// The runs the last footer names, newest first.
    pub(crate) fn runs(&self) -> Result<Vec<Run>, StoreError> {
        let Some(footer) = self.footer().filter(|footer| footer.attributes != 0) else {
            return Ok(Vec::new());
        };
        let list = self.node(footer.attributes, read::RUNS_NODE)?;
        let mut decoder = Decoder::new(list.bytes());
        let count = decoder.count(8)?;
        let mut runs = Vec::with_capacity(count);
        for _ in 0..count {
            let payload = self.node(decoder.u64()?, read::RUN_NODE)?;
            let bytes = payload.bytes();
            if bytes.len() < 4 || (bytes.len() - 4) % ENTRY_BYTES != 0 {
                return Err(StoreError::Damaged {
                    what: "an attribute run has a partial entry",
                });
            }
            runs.push(Run {
                bytes: Arc::new(bytes.to_vec()),
            });
        }
        Ok(runs)
    }

    /// The value filed under `key` in the newest run that has one.
    pub(crate) fn attribute_in(
        &self,
        runs: &[Run],
        key: &Digest,
    ) -> Result<Option<Vec<u8>>, StoreError> {
        for run in runs {
            if let Some(offset) = run.find(key) {
                return Ok(Some(self.node(offset, read::VALUE_NODE)?.bytes().to_vec()));
            }
        }
        Ok(None)
    }
}

impl Writer {
    /// Files each value under its key, replacing earlier values, and makes
    /// them visible to the next reader. For a key given twice, the later
    /// value wins.
    pub fn put_attributes(&mut self, entries: &[(Digest, Vec<u8>)]) -> Result<(), StoreError> {
        if entries.is_empty() {
            return Ok(());
        }
        let runs = self.index.runs()?;
        let mut pending = Pending::new(&self.index);
        let mut fresh: BTreeMap<Digest, u64> = BTreeMap::new();
        for (key, value) in entries {
            fresh.insert(*key, pending.append(read::VALUE_NODE, value));
        }
        let mut list = vec![pending.append(read::RUN_NODE, &encode_run(&fresh))];
        if runs.len() + 1 > MAX_RUNS {
            list = vec![pending.append(read::RUN_NODE, &encode_run(&merged(fresh, &runs)))];
        } else {
            list.extend(self.run_offsets()?);
        }
        let list = pending.append(read::RUNS_NODE, &encode_list(&list));
        let nodes = pending.into_bytes();
        self.publish(&nodes, None, Some(list))
    }

    fn run_offsets(&self) -> Result<Vec<u64>, StoreError> {
        let Some(footer) = self.index.footer().filter(|footer| footer.attributes != 0) else {
            return Ok(Vec::new());
        };
        let list = self.index.node(footer.attributes, read::RUNS_NODE)?;
        let mut decoder = Decoder::new(list.bytes());
        let count = decoder.count(8)?;
        (0..count).map(|_| decoder.u64()).collect()
    }
}

/// Every key of `runs` and `fresh`, each with its newest value.
fn merged(mut fresh: BTreeMap<Digest, u64>, runs: &[Run]) -> BTreeMap<Digest, u64> {
    for run in runs {
        for position in 0..run.len() {
            let (key, offset) = run.entry(position);
            let mut digest = [0; 32];
            digest.copy_from_slice(key);
            fresh.entry(digest).or_insert(offset);
        }
    }
    fresh
}

fn encode_run(entries: &BTreeMap<Digest, u64>) -> Vec<u8> {
    let mut encoder = Encoder::default();
    encoder.count(entries.len());
    for (key, offset) in entries {
        encoder.digest(key);
        encoder.u64(*offset);
    }
    encoder.bytes
}

fn encode_list(offsets: &[u64]) -> Vec<u8> {
    let mut encoder = Encoder::default();
    encoder.count(offsets.len());
    for offset in offsets {
        encoder.u64(*offset);
    }
    encoder.bytes
}
