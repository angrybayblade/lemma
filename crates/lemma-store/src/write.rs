//! Writing a store: append what changed, then a footer that makes it real.

use std::collections::BTreeMap;
use std::fs::{File, OpenOptions, TryLockError};
use std::io::Write;
use std::path::{Path, PathBuf};

use crate::codec::{self, Encoder, Entry, EntryKind};
use crate::read::{self, Index, ReadMode};
use crate::{Change, Digest, FileRecord, SourceRecord, StoreError};

/// The one writer of a store. Holding it holds the store's lock.
pub struct Writer {
    path: PathBuf,
    file: File,
    index: Index,
    /// Released when the writer is dropped.
    _lock: File,
}

/// What one commit produces.
#[derive(Debug, Clone, Copy)]
pub struct Commit<'a> {
    /// The name of the source this commit writes.
    pub root: &'a str,
    /// The source the changes apply to; `None` starts from nothing.
    pub parent: Option<&'a str>,
    /// What the new source is.
    pub source: &'a SourceRecord,
}

/// One path's change, as it descends the tree.
struct Step<'a> {
    rest: &'a str,
    record: Option<&'a FileRecord>,
}

/// Nodes appended by a commit, in memory until it is written whole.
struct Pending<'a> {
    index: &'a Index,
    start: u64,
    bytes: Vec<u8>,
}

impl Writer {
    /// Opens the store at `path` for writing, creating it if need be.
    /// Fails with [`StoreError::Busy`] while another writer holds it.
    pub fn open(path: &Path) -> Result<Self, StoreError> {
        let io = |source| StoreError::Io {
            path: path.to_owned(),
            source,
        };
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent).map_err(io)?;
        }
        let lock = hold_lock(path)?;
        let mut file = OpenOptions::new()
            .create(true)
            .append(true)
            .open(path)
            .map_err(io)?;
        if file.metadata().map_err(io)?.len() < read::HEADER_BYTES {
            file.set_len(0).map_err(io)?;
            file.write_all(&read::MAGIC).map_err(io)?;
            file.write_all(&read::MAJOR.to_le_bytes()).map_err(io)?;
            file.write_all(&0u16.to_le_bytes()).map_err(io)?;
            file.sync_data().map_err(io)?;
        }
        // A commit that did not finish stays as ignored bytes before the
        // next one; truncating could pull pages from under a mapped reader.
        let index = Index::open(path, ReadMode::Positional)?;
        Ok(Self {
            path: path.to_owned(),
            file,
            index,
            _lock: lock,
        })
    }

    /// Applies `changes` and publishes the result as `commit.root`, and
    /// returns the digest of its tree. Either all of it is visible to the
    /// next reader, or none of it.
    pub fn commit(
        &mut self,
        commit: &Commit<'_>,
        changes: &[Change],
    ) -> Result<Digest, StoreError> {
        let old_tree = match commit.parent {
            Some(name) => Some(self.index.root(name)?.tree),
            None => None,
        };
        let mut sorted: Vec<&Change> = changes.iter().collect();
        sorted.sort_by(|left, right| left.path().cmp(right.path()));
        let steps: Vec<Step<'_>> = sorted.iter().map(|change| step_of(change)).collect();

        let mut pending = Pending {
            index: &self.index,
            start: self.index.length(),
            bytes: Vec::new(),
        };
        let tree = match pending.rewrite(old_tree.as_ref(), &steps)? {
            Some(tree) => tree,
            None => pending.empty_directory(),
        };
        let source = codec::encode_source(commit.source, &tree);
        let source = pending.append(read::SOURCE_NODE, &source);
        let mut roots = self.index.root_offsets()?;
        roots.insert(commit.root.to_owned(), source);
        let roots = pending.append(read::ROOTS_NODE, &encode_roots(&roots));
        let nodes = pending.bytes;
        self.publish(&nodes, roots)?;
        Ok(tree.digest)
    }

    /// Writes nodes and then the footer, each durable before the next, so
    /// a footer on disk always means its nodes are too.
    fn publish(&mut self, nodes: &[u8], roots: u64) -> Result<(), StoreError> {
        let io = |source| StoreError::Io {
            path: self.path.clone(),
            source,
        };
        let previous = self.index.footer().map_or(0, |footer| footer.offset);
        self.file.write_all(nodes).map_err(io)?;
        self.file.sync_data().map_err(io)?;
        self.file
            .write_all(&read::encode_footer(roots, previous))
            .map_err(io)?;
        self.file.sync_data().map_err(io)?;
        self.index = Index::open(&self.path, ReadMode::Positional)?;
        Ok(())
    }
}

fn hold_lock(path: &Path) -> Result<File, StoreError> {
    let mut name = path.as_os_str().to_owned();
    name.push(".lock");
    let io = |source| StoreError::Io {
        path: path.to_owned(),
        source,
    };
    let lock = OpenOptions::new()
        .create(true)
        .truncate(false)
        .write(true)
        .open(PathBuf::from(name))
        .map_err(io)?;
    match lock.try_lock() {
        Ok(()) => Ok(lock),
        Err(TryLockError::WouldBlock) => Err(StoreError::Busy {
            path: path.to_owned(),
        }),
        Err(TryLockError::Error(source)) => Err(io(source)),
    }
}

fn step_of(change: &Change) -> Step<'_> {
    match change {
        Change::Put { path, record } => Step {
            rest: path,
            record: Some(record),
        },
        Change::Remove { path } => Step {
            rest: path,
            record: None,
        },
    }
}

fn encode_roots(roots: &BTreeMap<String, u64>) -> Vec<u8> {
    let mut encoder = Encoder::default();
    encoder.count(roots.len());
    for (name, offset) in roots {
        encoder.text(name);
        encoder.u64(*offset);
    }
    encoder.bytes
}

impl Pending<'_> {
    fn append(&mut self, kind: u8, payload: &[u8]) -> u64 {
        let offset = self.start + self.bytes.len() as u64;
        let begin = self.bytes.len();
        self.bytes.push(kind);
        let length = u32::try_from(payload.len()).unwrap_or(u32::MAX);
        self.bytes.extend_from_slice(&length.to_le_bytes());
        self.bytes.extend_from_slice(payload);
        let digest = blake3::hash(&self.bytes[begin..]);
        self.bytes.extend_from_slice(digest.as_bytes());
        offset
    }

    fn empty_directory(&mut self) -> Entry {
        let entries: BTreeMap<String, Entry> = BTreeMap::new();
        Entry {
            kind: EntryKind::Directory,
            digest: codec::directory_digest(entries.iter()),
            offset: self.append(
                read::DIRECTORY_NODE,
                &codec::encode_directory(entries.iter()),
            ),
        }
    }

    /// The directory `old` with `steps` applied, or `None` when nothing is
    /// left in it. Unchanged children keep their nodes.
    fn rewrite(
        &mut self,
        old: Option<&Entry>,
        steps: &[Step<'_>],
    ) -> Result<Option<Entry>, StoreError> {
        let mut entries: BTreeMap<String, Entry> = match old {
            Some(entry) => self.index.directory(entry.offset)?.into_iter().collect(),
            None => BTreeMap::new(),
        };
        let mut below: BTreeMap<&str, Vec<Step<'_>>> = BTreeMap::new();
        for step in steps {
            match step.rest.split_once('/') {
                None => self.apply_file(&mut entries, step),
                Some((name, rest)) => below.entry(name).or_default().push(Step {
                    rest,
                    record: step.record,
                }),
            }
        }
        for (name, steps) in below {
            let child = entries
                .get(name)
                .filter(|entry| entry.kind == EntryKind::Directory)
                .copied();
            match self.rewrite(child.as_ref(), &steps)? {
                Some(entry) => entries.insert(name.to_owned(), entry),
                None => entries.remove(name),
            };
        }
        if entries.is_empty() {
            return Ok(None);
        }
        Ok(Some(self.finish_directory(old, &entries)))
    }

    fn apply_file(&mut self, entries: &mut BTreeMap<String, Entry>, step: &Step<'_>) {
        let Some(record) = step.record else {
            entries.remove(step.rest);
            return;
        };
        let payload = codec::encode_file(record);
        let digest = *blake3::hash(&payload).as_bytes();
        let unchanged = entries
            .get(step.rest)
            .is_some_and(|entry| entry.kind == EntryKind::File && entry.digest == digest);
        if unchanged {
            return;
        }
        let offset = self.append(read::FILE_NODE, &payload);
        let entry = Entry {
            kind: EntryKind::File,
            digest,
            offset,
        };
        entries.insert(step.rest.to_owned(), entry);
    }

    fn finish_directory(
        &mut self,
        old: Option<&Entry>,
        entries: &BTreeMap<String, Entry>,
    ) -> Entry {
        let digest = codec::directory_digest(entries.iter());
        if let Some(old) = old.filter(|old| old.digest == digest) {
            return *old;
        }
        let offset = self.append(
            read::DIRECTORY_NODE,
            &codec::encode_directory(entries.iter()),
        );
        Entry {
            kind: EntryKind::Directory,
            digest,
            offset,
        }
    }
}
