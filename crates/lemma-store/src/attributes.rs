//! The attribute table (spec 17): values filed under 32-byte keys, beside
//! the roots rather than inside them, so storing one changes no digest.
//!
//! Each write appends its values, then a run: the write's keys, sorted,
//! each with its value's offset and length, then a Bloom filter over the
//! keys. The footer names the list of runs, newest first, and a lookup
//! searches them in that order, skipping a run whose filter rules the key
//! out, so a newer value for a key hides an older one. When the list grows
//! past a few runs, the writer merges them into one; values are not copied.

use std::collections::BTreeMap;

use crate::codec::{Decoder, Encoder};
use crate::read::{self, Index};
use crate::write::{Pending, Writer};
use crate::{Digest, StoreError, filter};

/// Bytes of one run entry: a key, its value's offset and length.
const ENTRY_BYTES: usize = 32 + 8 + 4;

/// How many runs a lookup may search before a write merges them.
const MAX_RUNS: usize = 8;

/// A key's value: where it is, and how long.
pub(crate) type Located = (u64, u32);

/// A run as the store holds it.
#[derive(Debug, Clone, Copy)]
pub(crate) struct Run {
    node: u64,
    entries: u64,
    count: usize,
    filter: u64,
    words: usize,
}

const PARTIAL: StoreError = StoreError::Damaged {
    what: "an attribute run is damaged",
};

impl Index {
    /// The runs the last footer names, newest first.
    pub(crate) fn runs(&self) -> Result<Vec<Run>, StoreError> {
        let Some(footer) = self.footer().filter(|footer| footer.runs != 0) else {
            return Ok(Vec::new());
        };
        let list = self.node(footer.runs, read::RUNS_NODE)?;
        let mut decoder = Decoder::new(&list);
        let count = decoder.count(8)?;
        (0..count).map(|_| self.run_at(decoder.u64()?)).collect()
    }

    fn run_at(&self, node: u64) -> Result<Run, StoreError> {
        let (payload, length) = self.node_span(node, read::RUN_NODE)?;
        let count = self.u32_at(payload)? as usize;
        let filter = count
            .checked_mul(ENTRY_BYTES)
            .map(|bytes| payload + 4 + bytes as u64)
            .ok_or(PARTIAL)?;
        let words = self.u32_at(filter)? as usize;
        let expected = words
            .checked_mul(8)
            .and_then(|bytes| bytes.checked_add(8 + count * ENTRY_BYTES));
        if expected != Some(length) {
            return Err(PARTIAL);
        }
        Ok(Run {
            node,
            entries: payload + 4,
            count,
            filter: filter + 4,
            words,
        })
    }

    fn u32_at(&self, offset: u64) -> Result<u32, StoreError> {
        let bytes = self.read(offset, 4)?;
        Ok(u32::from_le_bytes([bytes[0], bytes[1], bytes[2], bytes[3]]))
    }

    fn run_entry(&self, run: &Run, position: usize) -> Result<(Digest, Located), StoreError> {
        let bytes = self.read(run.entries + (position * ENTRY_BYTES) as u64, ENTRY_BYTES)?;
        let mut decoder = Decoder::new(&bytes);
        let key = decoder.digest()?;
        let offset = decoder.u64()?;
        Ok((key, (offset, decoder.u32()?)))
    }

    fn find_in(&self, run: &Run, key: &Digest) -> Result<Option<Located>, StoreError> {
        let word = |at: usize| -> Result<u64, StoreError> {
            let bytes = self.read(run.filter + (at * 8) as u64, 8)?;
            let mut decoder = Decoder::new(&bytes);
            decoder.u64()
        };
        if !filter::may_contain(key, run.words, word)? {
            return Ok(None);
        }
        let (mut low, mut high) = (0, run.count);
        while low < high {
            let middle = low + (high - low) / 2;
            let (found, located) = self.run_entry(run, middle)?;
            match found.cmp(key) {
                std::cmp::Ordering::Equal => return Ok(Some(located)),
                std::cmp::Ordering::Less => low = middle + 1,
                std::cmp::Ordering::Greater => high = middle,
            }
        }
        Ok(None)
    }

    /// The value filed under `key` in the newest run that has one.
    pub(crate) fn attribute_in(
        &self,
        runs: &[Run],
        key: &Digest,
    ) -> Result<Option<Vec<u8>>, StoreError> {
        for run in runs {
            if let Some((offset, length)) = self.find_in(run, key)? {
                return Ok(Some(self.read(offset, length as usize)?.into_owned()));
            }
        }
        Ok(None)
    }

    /// Every key's newest value, sorted by key.
    pub(crate) fn attribute_entries(&self) -> Result<BTreeMap<Digest, Located>, StoreError> {
        let mut entries = BTreeMap::new();
        for run in self.runs()? {
            for position in 0..run.count {
                let (key, located) = self.run_entry(&run, position)?;
                entries.entry(key).or_insert(located);
            }
        }
        Ok(entries)
    }
}

impl Writer {
    /// Files each value under its key, replacing earlier values, and makes
    /// them visible to the next reader. For a key given twice, the later
    /// value wins.
    pub fn put_attributes(&mut self, entries: &[(Digest, Vec<u8>)]) -> Result<(), StoreError> {
        self.write(&crate::Write {
            commit: None,
            removed_roots: &[],
            attributes: entries,
        })
        .map(drop)
    }
}

impl Pending {
    /// Appends `entries` and a new list of runs, merging the runs when
    /// there are too many; returns the list's offset.
    pub(crate) fn append_attributes(
        &mut self,
        index: &Index,
        entries: &[(Digest, Vec<u8>)],
    ) -> Result<u64, StoreError> {
        let mut fresh: BTreeMap<Digest, Located> = BTreeMap::new();
        for (key, value) in entries {
            let length = u32::try_from(value.len()).map_err(|_| StoreError::Damaged {
                what: "an attribute would be larger than 4 GiB",
            })?;
            fresh.insert(*key, (self.append_value(value), length));
        }
        let runs = index.runs()?;
        let list = if runs.len() < MAX_RUNS {
            let mut list = vec![self.append_node(read::RUN_NODE, &encode_run(&fresh))?];
            list.extend(runs.iter().map(|run| run.node));
            list
        } else {
            for (key, located) in index.attribute_entries()? {
                fresh.entry(key).or_insert(located);
            }
            vec![self.append_node(read::RUN_NODE, &encode_run(&fresh))?]
        };
        self.append_node(read::RUNS_NODE, &encode_list(&list))
    }
}

/// A run's payload: its entries, sorted by key, then its filter.
pub(crate) fn encode_run(entries: &BTreeMap<Digest, Located>) -> Vec<u8> {
    let mut encoder = Encoder::default();
    encoder.count(entries.len());
    for (key, (offset, length)) in entries {
        encoder.digest(key);
        encoder.u64(*offset);
        encoder.u32(*length);
    }
    let words = filter::build(entries.keys());
    encoder.count(words.len());
    for word in words {
        encoder.u64(word);
    }
    encoder.bytes
}

pub(crate) fn encode_list(offsets: &[u64]) -> Vec<u8> {
    let mut encoder = Encoder::default();
    encoder.count(offsets.len());
    for offset in offsets {
        encoder.u64(*offset);
    }
    encoder.bytes
}
