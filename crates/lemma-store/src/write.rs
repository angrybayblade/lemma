//! Writing a store: append what changed, then a footer that makes it real.
//!
//! The store is a cache (spec 17), so commits are not flushed one by one:
//! each is written whole, nodes then footer, and the operating system
//! writes it back when it likes. A large commit, or a checkpoint, flushes
//! the file and then appends a footer recording that everything before it
//! is on disk, so a reader after a crash checks only what came since.

use std::fs::{File, OpenOptions, TryLockError};
use std::io::Write as _;
use std::path::{Path, PathBuf};

use crate::read::{self, Footer, Index, ReadMode};
use crate::{Change, Digest, SourceRecord, StoreError};

/// A commit this large is flushed at once: it is costly to compute again.
const FLUSH_BYTES: usize = 1 << 20;

/// The one writer of a store. Holding it holds the store's lock.
pub struct Writer {
    path: PathBuf,
    file: File,
    pub(crate) index: Index,
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

/// Everything one commit does.
#[derive(Debug, Clone, Copy)]
pub struct Write<'a> {
    /// A source to commit, with the changes from its parent.
    pub commit: Option<(Commit<'a>, &'a [Change])>,
    /// Sources to forget.
    pub removed_roots: &'a [String],
    /// Values to file in the attribute table.
    pub attributes: &'a [(Digest, Vec<u8>)],
}

/// Bytes appended by a commit, in memory until it is written whole.
pub(crate) struct Pending {
    start: u64,
    pub(crate) bytes: Vec<u8>,
}

impl Pending {
    pub(crate) fn new(start: u64) -> Self {
        Self {
            start,
            bytes: Vec::new(),
        }
    }

    /// Where the next byte appended will be in the file.
    pub(crate) fn position(&self) -> u64 {
        self.start + self.bytes.len() as u64
    }

    /// Appends a node and returns its offset.
    pub(crate) fn append_node(&mut self, kind: u8, payload: &[u8]) -> Result<u64, StoreError> {
        let length = u32::try_from(payload.len()).map_err(|_| StoreError::Damaged {
            what: "a node would be larger than 4 GiB",
        })?;
        let offset = self.position();
        self.bytes.push(kind);
        self.bytes.extend_from_slice(&length.to_le_bytes());
        self.bytes.extend_from_slice(payload);
        Ok(offset)
    }

    /// Appends a value's bare bytes and returns their offset.
    pub(crate) fn append_value(&mut self, value: &[u8]) -> u64 {
        let offset = self.position();
        self.bytes.extend_from_slice(value);
        offset
    }
}

impl Writer {
    /// Opens the store at `path` for writing, creating it if need be.
    /// Fails with [`StoreError::Busy`] while another writer holds it.
    pub fn open(path: &Path) -> Result<Self, StoreError> {
        Self::open_with(path, false)
    }

    /// Opens the store at `path` for writing, waiting while another writer
    /// holds it.
    pub fn open_waiting(path: &Path) -> Result<Self, StoreError> {
        Self::open_with(path, true)
    }

    fn open_with(path: &Path, waits: bool) -> Result<Self, StoreError> {
        let io = |source| StoreError::Io {
            path: path.to_owned(),
            source,
        };
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent).map_err(io)?;
        }
        let lock = hold_lock(path, waits)?;
        let is_new = std::fs::metadata(path).map_or(true, |found| found.len() < read::HEADER_BYTES);
        if is_new {
            replace_with_empty(path)?;
        }
        let index = match Index::open(path, ReadMode::Mapped) {
            // Another layout's store is a cache like any other: start again.
            Err(StoreError::UnsupportedVersion { .. }) => {
                replace_with_empty(path)?;
                Index::open(path, ReadMode::Mapped)?
            }
            opened => opened?,
        };
        let file = OpenOptions::new().append(true).open(path).map_err(io)?;
        Ok(Self {
            path: path.to_owned(),
            file,
            index,
            _lock: lock,
        })
    }

    /// Applies `changes` and publishes the result as `commit.root`, and
    /// returns the digest of its files. Either all of it is visible to the
    /// next reader, or none of it.
    pub fn commit(
        &mut self,
        commit: &Commit<'_>,
        changes: &[Change],
    ) -> Result<Digest, StoreError> {
        let digest = self.write(&Write {
            commit: Some((*commit, changes)),
            removed_roots: &[],
            attributes: &[],
        })?;
        Ok(digest.unwrap_or_default())
    }

    /// Applies everything in `write` as one commit, and returns the digest
    /// of the committed source's files, if it commits one.
    pub fn write(&mut self, write: &Write<'_>) -> Result<Option<Digest>, StoreError> {
        let mut roots = self.index.roots()?;
        let mut pending = Pending::new(self.index.length());
        let mut digest = None;
        if let Some((commit, changes)) = &write.commit {
            let root = pending.commit_root(&self.index, &roots, commit, changes)?;
            digest = Some(root.digest);
            roots.insert(commit.root.to_owned(), root);
        }
        for name in write.removed_roots {
            roots.remove(name);
        }
        let runs = match write.attributes {
            [] => None,
            entries => Some(pending.append_attributes(&self.index, entries)?),
        };
        let roots_changed = write.commit.is_some() || !write.removed_roots.is_empty();
        let roots = if roots_changed {
            Some(pending.append_node(read::ROOTS_NODE, &read::encode_roots(&roots))?)
        } else {
            None
        };
        let is_large = pending.bytes.len() >= FLUSH_BYTES;
        self.publish(pending.bytes, roots, runs, None)?;
        if is_large {
            self.checkpoint()?;
        }
        self.compact_if_due()?;
        Ok(digest)
    }

    /// Writes a commit's bytes and then its footer in one append. `None`
    /// keeps what the last footer named.
    pub(crate) fn publish(
        &mut self,
        mut bytes: Vec<u8>,
        roots: Option<u64>,
        runs: Option<u64>,
        checked: Option<u64>,
    ) -> Result<(), StoreError> {
        let last = self.index.footer();
        let start = self.index.length();
        let footer = Footer {
            offset: start + bytes.len() as u64,
            roots: roots.or(last.map(|footer| footer.roots)).unwrap_or(0),
            runs: runs.or(last.map(|footer| footer.runs)).unwrap_or(0),
            previous: last.map_or(0, |footer| footer.offset),
            start,
            synced: last.map_or(read::HEADER_BYTES, |footer| footer.synced),
            checked: checked.or(last.map(|footer| footer.checked)).unwrap_or(0),
            commit: *blake3::hash(&bytes).as_bytes(),
        };
        bytes.extend_from_slice(&read::encode_footer(&footer));
        self.file
            .write_all(&bytes)
            .map_err(|source| StoreError::Io {
                path: self.path.clone(),
                source,
            })?;
        self.index = Index::open(&self.path, ReadMode::Mapped)?;
        Ok(())
    }

    /// Flushes the file, then appends a footer saying everything before it
    /// is on disk, so that a reader after a crash need not check it again.
    pub fn checkpoint(&mut self) -> Result<(), StoreError> {
        let Some(last) = self.index.footer() else {
            return Ok(());
        };
        self.file.sync_data().map_err(|source| StoreError::Io {
            path: self.path.clone(),
            source,
        })?;
        let start = self.index.length();
        let footer = Footer {
            offset: start,
            previous: last.offset,
            start,
            synced: start,
            commit: *blake3::hash(&[]).as_bytes(),
            ..last
        };
        self.file
            .write_all(&read::encode_footer(&footer))
            .map_err(|source| StoreError::Io {
                path: self.path.clone(),
                source,
            })?;
        self.index = Index::open(&self.path, ReadMode::Mapped)?;
        Ok(())
    }

    pub(crate) fn path(&self) -> &Path {
        &self.path
    }

    /// Takes up the file compaction put in place of the old one.
    pub(crate) fn reopen(&mut self) -> Result<(), StoreError> {
        self.file = OpenOptions::new()
            .append(true)
            .open(&self.path)
            .map_err(|source| StoreError::Io {
                path: self.path.clone(),
                source,
            })?;
        self.index = Index::open(&self.path, ReadMode::Mapped)?;
        Ok(())
    }
}

/// Puts an empty store at `path` by renaming a new file over it, so that a
/// reader that mapped an old file keeps its pages.
pub(crate) fn replace_with_empty(path: &Path) -> Result<(), StoreError> {
    let mut header = read::MAGIC.to_vec();
    header.extend_from_slice(&read::MAJOR.to_le_bytes());
    header.extend_from_slice(&0u16.to_le_bytes());
    replace_with(path, &header)
}

/// Puts `bytes` at `path`, flushed, by renaming a new file over it.
pub(crate) fn replace_with(path: &Path, bytes: &[u8]) -> Result<(), StoreError> {
    let io = |source| StoreError::Io {
        path: path.to_owned(),
        source,
    };
    let mut name = path.as_os_str().to_owned();
    name.push(".new");
    let fresh = PathBuf::from(name);
    let mut file = File::create(&fresh).map_err(io)?;
    file.write_all(bytes).map_err(io)?;
    file.sync_data().map_err(io)?;
    std::fs::rename(&fresh, path).map_err(io)
}

fn hold_lock(path: &Path, waits: bool) -> Result<File, StoreError> {
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
    if waits {
        lock.lock().map_err(io)?;
        return Ok(lock);
    }
    match lock.try_lock() {
        Ok(()) => Ok(lock),
        Err(TryLockError::WouldBlock) => Err(StoreError::Busy {
            path: path.to_owned(),
        }),
        Err(TryLockError::Error(source)) => Err(io(source)),
    }
}
