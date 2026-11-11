//! Compaction (spec 17): the live segments and the newest value under each
//! key, copied into a new file that is flushed and renamed over the old
//! one. A reader that mapped the old file keeps reading it until it opens
//! the store again.

use std::collections::{BTreeMap, BTreeSet};
use std::fs::File;
use std::io::{BufWriter, Write as _};
use std::path::{Path, PathBuf};

use crate::attributes::{self, Located};
use crate::read::{self, Footer, Root};
use crate::write::{Named, Writer};
use crate::{Digest, StoreError};

/// Files smaller than this are never compacted: there is little to gain.
const MIN_COMPACT_BYTES: u64 = 8 << 20;

impl Writer {
    /// Compacts the store if it has doubled since it was last checked and
    /// less than half of it is live; otherwise records that it was checked.
    pub(crate) fn compact_if_due(&mut self) -> Result<(), StoreError> {
        let length = self.index.length();
        let checked = self.index.footer().map_or(0, |footer| footer.checked);
        if length < MIN_COMPACT_BYTES.max(checked.saturating_mul(2)) {
            return Ok(());
        }
        if self.live_bytes()?.saturating_mul(2) < length {
            return self.compact();
        }
        let named = Named {
            checked: Some(length),
            ..Named::default()
        };
        self.publish(&[], named)
    }

    /// Bytes that the roots and the newest attribute values still use.
    fn live_bytes(&self) -> Result<u64, StoreError> {
        let mut live = 0;
        for offset in live_segments(&self.index.roots()?) {
            let (_, length) = self.index.node_span(offset, read::SEGMENT_NODE)?;
            live += read::NODE_HEADER_BYTES + length as u64;
        }
        for (_, length) in self.index.attribute_entries()?.values() {
            live += u64::from(*length);
        }
        Ok(live)
    }

    /// Rewrites the store with only what is live, and takes up the new file.
    pub fn compact(&mut self) -> Result<(), StoreError> {
        let roots = self.index.roots()?;
        let entries = self.index.attribute_entries()?;
        let temporary = sibling(self.path(), ".compacting");
        let mut sink = Sink::create(&temporary)?;
        let mut moved = BTreeMap::new();
        for offset in live_segments(&roots) {
            let payload = self.index.node(offset, read::SEGMENT_NODE)?;
            moved.insert(offset, sink.node(read::SEGMENT_NODE, &payload)?);
        }
        let mut values: BTreeMap<Digest, Located> = BTreeMap::new();
        for (key, (offset, length)) in entries {
            let value = self.index.read(offset, length as usize)?;
            values.insert(key, (sink.value(&value)?, length));
        }
        let run = sink.node(read::RUN_NODE, &attributes::encode_run(&values))?;
        let runs = sink.node(read::RUNS_NODE, &attributes::encode_list(&[run]))?;
        let roots = moved_roots(roots, &moved)?;
        let roots = sink.node(read::ROOTS_NODE, &read::encode_roots(&roots))?;
        sink.finish(roots, runs)?;
        std::fs::rename(&temporary, self.path()).map_err(|source| StoreError::Io {
            path: self.path().to_owned(),
            source,
        })?;
        self.reopen()
    }
}

/// Every segment some root uses, once each, in offset order.
fn live_segments(roots: &BTreeMap<String, Root>) -> BTreeSet<u64> {
    roots
        .values()
        .flat_map(|root| root.segments.iter().copied())
        .collect()
}

fn moved_roots(
    roots: BTreeMap<String, Root>,
    moved: &BTreeMap<u64, u64>,
) -> Result<BTreeMap<String, Root>, StoreError> {
    roots
        .into_iter()
        .map(|(name, mut root)| {
            root.segments = root
                .segments
                .iter()
                .map(|offset| {
                    moved.get(offset).copied().ok_or(StoreError::Damaged {
                        what: "a segment was not copied",
                    })
                })
                .collect::<Result<_, _>>()?;
            Ok((name, root))
        })
        .collect()
}

fn sibling(path: &Path, suffix: &str) -> PathBuf {
    let mut name = path.as_os_str().to_owned();
    name.push(suffix);
    PathBuf::from(name)
}

/// A new store written front to back, hashed as it goes for its footer.
struct Sink {
    path: PathBuf,
    file: BufWriter<File>,
    position: u64,
    hasher: blake3::Hasher,
}

impl Sink {
    fn create(path: &Path) -> Result<Self, StoreError> {
        let file = File::create(path).map_err(|source| StoreError::Io {
            path: path.to_owned(),
            source,
        })?;
        let mut sink = Self {
            path: path.to_owned(),
            file: BufWriter::new(file),
            position: 0,
            hasher: blake3::Hasher::new(),
        };
        let mut header = read::MAGIC.to_vec();
        header.extend_from_slice(&read::MAJOR.to_le_bytes());
        header.extend_from_slice(&0u16.to_le_bytes());
        sink.raw(&header)?;
        sink.hasher = blake3::Hasher::new();
        Ok(sink)
    }

    fn raw(&mut self, bytes: &[u8]) -> Result<u64, StoreError> {
        let offset = self.position;
        self.file
            .write_all(bytes)
            .map_err(|source| StoreError::Io {
                path: self.path.clone(),
                source,
            })?;
        self.hasher.update(bytes);
        self.position += bytes.len() as u64;
        Ok(offset)
    }

    fn node(&mut self, kind: u8, payload: &[u8]) -> Result<u64, StoreError> {
        let length = u32::try_from(payload.len()).map_err(|_| StoreError::Damaged {
            what: "a node would be larger than 4 GiB",
        })?;
        let mut header = vec![kind];
        header.extend_from_slice(&length.to_le_bytes());
        let offset = self.raw(&header)?;
        self.raw(payload)?;
        Ok(offset)
    }

    fn value(&mut self, value: &[u8]) -> Result<u64, StoreError> {
        self.raw(value)
    }

    /// Writes the footer and flushes the file: everything in it is on disk
    /// before it takes the store's place.
    fn finish(mut self, roots: u64, runs: u64) -> Result<(), StoreError> {
        let offset = self.position;
        let footer = Footer {
            offset,
            roots,
            runs,
            previous: 0,
            start: read::HEADER_BYTES,
            synced: offset,
            checked: offset + read::FOOTER_BYTES,
            commit: *self.hasher.finalize().as_bytes(),
        };
        self.raw(&read::encode_footer(&footer))?;
        let io = |source| StoreError::Io {
            path: self.path.clone(),
            source,
        };
        let file = self
            .file
            .into_inner()
            .map_err(|error| io(error.into_error()))?;
        file.sync_data().map_err(io)
    }
}
