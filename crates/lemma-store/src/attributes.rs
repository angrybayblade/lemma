//! The attribute table (spec 17): values filed under 32-byte keys, beside
//! the roots rather than inside them, so storing one changes no digest.
//!
//! Each write appends its values, then a run: the write's keys, sorted,
//! each with its value's offset and length, then a Bloom filter over the
//! keys. The footer names the list of runs, newest first, and a lookup
//! searches them in that order, skipping a run whose filter rules the key
//! out, so a newer value for a key hides an older one. A write merges the
//! newest runs into its own while they are no larger than what it has
//! gathered, so runs grow in size tiers: a small write rewrites only small
//! runs, and the list stays short. Values are never copied.

use std::borrow::Cow;
use std::collections::BTreeMap;

use crate::codec::{Decoder, Encoder};
use crate::read::{self, Index};
use crate::write::{FLUSH_BYTES, Named, Pending, Writer};
use crate::{Digest, StoreError, filter};

/// Bytes of one run entry: a key, its value's offset and length.
const ENTRY_BYTES: usize = 32 + 8 + 4;

/// How many runs a lookup may search; a write merges more of the newest
/// runs than their sizes ask for to keep within it.
const MAX_RUNS: usize = 8;

/// A write takes in the next older run while it holds at most this many
/// times the entries gathered so far.
const TIER_RATIO: usize = 2;

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
        if self.is_mapped() {
            return self.find_in_place(run, key);
        }
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

    /// [`Index::find_in`] over a mapped file: the run's filter and entries
    /// are read where they lie, and keys are compared in place.
    fn find_in_place(&self, run: &Run, key: &Digest) -> Result<Option<Located>, StoreError> {
        let filter = self.read(run.filter, run.words * 8)?;
        let (words, _) = filter.as_chunks::<8>();
        let word = |at: usize| -> Result<u64, StoreError> {
            words
                .get(at)
                .map(|word| u64::from_le_bytes(*word))
                .ok_or(PARTIAL)
        };
        if !filter::may_contain(key, run.words, word)? {
            return Ok(None);
        }
        let entries = self.read(run.entries, run.count * ENTRY_BYTES)?;
        let (rows, _) = entries.as_chunks::<ENTRY_BYTES>();
        let Ok(at) = rows.binary_search_by(|row| row[..32].cmp(key)) else {
            return Ok(None);
        };
        let row = &rows[at];
        let mut decoder = Decoder::new(&row[32..]);
        Ok(Some((decoder.u64()?, decoder.u32()?)))
    }

    /// The value filed under `key` in the newest run that has one.
    pub(crate) fn attribute_in(
        &self,
        runs: &[Run],
        key: &Digest,
    ) -> Result<Option<Vec<u8>>, StoreError> {
        Ok(self.attribute_bytes(runs, key)?.map(Cow::into_owned))
    }

    /// [`Index::attribute_in`], borrowing the value from a mapped file.
    pub(crate) fn attribute_bytes(
        &self,
        runs: &[Run],
        key: &Digest,
    ) -> Result<Option<Cow<'_, [u8]>>, StoreError> {
        for run in runs {
            if let Some((offset, length)) = self.find_in(run, key)? {
                return Ok(Some(self.read(offset, length as usize)?));
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
    pub fn put_attributes<V: AsRef<[u8]>>(
        &mut self,
        entries: &[(Digest, V)],
    ) -> Result<(), StoreError> {
        if entries.is_empty() {
            return Ok(());
        }
        let total: usize = entries.iter().map(|(_, value)| value.as_ref().len()).sum();
        let start = self.index.length();
        let (fresh, streamed) = if total >= FLUSH_BYTES {
            // Many values go straight to the file, not through the commit's
            // buffer: the commit is flushed whole before its footer anyway.
            self.stream_values(start, entries)?
        } else {
            (BTreeMap::new(), 0)
        };
        let mut pending = Pending::new(start + streamed);
        let runs = if streamed > 0 {
            pending.append_runs(&self.index, fresh)?
        } else {
            pending.append_attributes(&self.index, entries)?
        };
        let named = Named {
            runs: Some(runs),
            ..Named::default()
        };
        self.publish_after(streamed, &pending.parts(), named)?;
        self.compact_if_due()
    }

    /// Writes `entries`' values at `start`, the file's end, and returns
    /// where each went and how many bytes they took.
    fn stream_values<V: AsRef<[u8]>>(
        &mut self,
        start: u64,
        entries: &[(Digest, V)],
    ) -> Result<(BTreeMap<Digest, Located>, u64), StoreError> {
        let io = |source| StoreError::Io {
            path: self.path().to_owned(),
            source,
        };
        let mut out = std::io::BufWriter::with_capacity(1 << 20, &self.file);
        let mut fresh = BTreeMap::new();
        let mut offset = start;
        for (key, value) in entries {
            let value = value.as_ref();
            std::io::Write::write_all(&mut out, value).map_err(io)?;
            fresh.insert(*key, (offset, value_length(value)?));
            offset += value.len() as u64;
        }
        std::io::Write::flush(&mut out).map_err(io)?;
        Ok((fresh, offset - start))
    }
}

fn value_length(value: &[u8]) -> Result<u32, StoreError> {
    u32::try_from(value.len()).map_err(|_| StoreError::Damaged {
        what: "an attribute would be larger than 4 GiB",
    })
}

impl Pending {
    /// Appends `entries` and a new list of runs, merging the runs when
    /// there are too many; returns the list's offset.
    pub(crate) fn append_attributes<V: AsRef<[u8]>>(
        &mut self,
        index: &Index,
        entries: &[(Digest, V)],
    ) -> Result<u64, StoreError> {
        let mut fresh: BTreeMap<Digest, Located> = BTreeMap::new();
        for (key, value) in entries {
            let value = value.as_ref();
            fresh.insert(*key, (self.append_value(value), value_length(value)?));
        }
        self.append_runs(index, fresh)
    }

    /// Appends a run of `fresh`, merged with as many of the newest runs as
    /// their sizes ask for, and a new list of runs; returns the list's
    /// offset.
    pub(crate) fn append_runs(
        &mut self,
        index: &Index,
        mut fresh: BTreeMap<Digest, Located>,
    ) -> Result<u64, StoreError> {
        let runs = index.runs()?;
        let merged = runs_to_merge(&runs, fresh.len());
        for run in &runs[..merged] {
            for position in 0..run.count {
                let (key, located) = index.run_entry(run, position)?;
                fresh.entry(key).or_insert(located);
            }
        }
        let mut list = vec![self.append_node(read::RUN_NODE, &encode_run(&fresh))?];
        list.extend(runs[merged..].iter().map(|run| run.node));
        self.append_node(read::RUNS_NODE, &encode_list(&list))
    }
}

/// How many of the newest `runs` a write of `fresh` entries merges: each
/// next run while it is small beside what has been gathered, and more while
/// the list would be too long.
fn runs_to_merge(runs: &[Run], fresh: usize) -> usize {
    let mut gathered = fresh.max(1);
    let mut merged = 0;
    while let Some(run) = runs.get(merged) {
        let is_small = run.count <= gathered.saturating_mul(TIER_RATIO);
        let is_too_long = runs.len() - merged >= MAX_RUNS;
        if !is_small && !is_too_long {
            break;
        }
        gathered = gathered.saturating_add(run.count);
        merged += 1;
    }
    merged
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

#[cfg(test)]
mod tests {
    use super::*;

    fn runs_of(counts: &[usize]) -> Vec<Run> {
        counts
            .iter()
            .map(|count| Run {
                node: 0,
                entries: 0,
                count: *count,
                filter: 0,
                words: 0,
            })
            .collect()
    }

    #[test]
    fn a_small_write_leaves_a_large_run_alone() {
        let runs = runs_of(&[100_000]);

        let merged = runs_to_merge(&runs, 10);

        assert_eq!(merged, 0);
    }

    #[test]
    fn a_write_takes_in_newer_runs_of_its_size() {
        let runs = runs_of(&[10, 10, 100]);

        let merged = runs_to_merge(&runs, 10);

        assert_eq!(merged, 2);
    }

    #[test]
    fn too_many_runs_are_merged_from_the_newest() {
        let counts: Vec<usize> = (0..9).map(|power| 10usize.pow(power)).collect();
        let runs = runs_of(&counts);

        let merged = runs_to_merge(&runs, 1);

        assert_eq!(merged, 2);
        assert_eq!(1 + runs.len() - merged, MAX_RUNS);
    }
}
