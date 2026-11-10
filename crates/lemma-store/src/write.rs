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

use crate::commit::Edits;
use crate::read::{self, Footer, Index, ReadMode, Tail};
use crate::{Change, Digest, SourceRecord, StoreError};

/// A commit this large is flushed at once: it is costly to compute again.
pub(crate) const FLUSH_BYTES: usize = 1 << 20;

/// How many bytes of commits may wait unflushed before the next commit
/// flushes them all: a reader opening the store hashes at most this much.
const UNSYNCED_BYTES: u64 = 4 << 20;

/// How many commits may wait unflushed before the next one flushes them:
/// a reader opening the store checks each on its own, and a flush costs
/// about as much as checking a few thousand small ones, so the two are
/// balanced for stores that take many small commits.
const UNSYNCED_COMMITS: usize = 64;

/// The one writer of a store. Holding it holds the store's lock.
pub struct Writer {
    path: PathBuf,
    pub(crate) file: File,
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

/// What a commit's footer names; `None` keeps what the last footer named.
#[derive(Debug, Clone, Copy, Default)]
pub(crate) struct Named {
    pub roots: Option<u64>,
    pub runs: Option<u64>,
    pub checked: Option<u64>,
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

    /// Appends a segment node holding `files`, built in place, and returns
    /// its offset.
    pub(crate) fn append_segment(
        &mut self,
        files: &[(&str, crate::segment::Entry<'_>)],
    ) -> Result<u64, StoreError> {
        let offset = self.position();
        let header = self.bytes.len();
        self.bytes.push(read::SEGMENT_NODE);
        self.bytes.extend_from_slice(&[0; 4]);
        crate::segment::build_into(files, &mut self.bytes)?;
        let length =
            u32::try_from(self.bytes.len() - header - 5).map_err(|_| StoreError::Damaged {
                what: "a node would be larger than 4 GiB",
            })?;
        self.bytes[header + 1..header + 5].copy_from_slice(&length.to_le_bytes());
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
        Self::open_with(path, false, None)
    }

    /// Opens the store at `path` for writing, waiting while another writer
    /// holds it.
    pub fn open_waiting(path: &Path) -> Result<Self, StoreError> {
        Self::open_with(path, true, None)
    }

    /// [`Writer::open_waiting`] for a process that has already read the
    /// store as far as `known`: while the file still ends there, what was
    /// checked is not hashed again.
    pub(crate) fn open_waiting_after(path: &Path, known: Option<Tail>) -> Result<Self, StoreError> {
        Self::open_with(path, true, known)
    }

    fn open_with(path: &Path, waits: bool, known: Option<Tail>) -> Result<Self, StoreError> {
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
        let opened = match known {
            Some(tail) => Index::open_written(path, tail),
            None => Index::open(path, ReadMode::Mapped),
        };
        let index = match opened {
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
        let commit = write
            .commit
            .map(|(commit, changes)| (commit, crate::commit::edits_of(changes)));
        let commit = commit.as_ref().map(|(commit, edits)| (commit, edits));
        self.write_edits(commit, write.removed_roots, write.attributes)
    }

    /// [`Writer::write`], with the commit's changes as sorted edits.
    pub(crate) fn write_edits<V: AsRef<[u8]>>(
        &mut self,
        commit: Option<(&Commit<'_>, &Edits<'_>)>,
        removed_roots: &[String],
        attributes: &[(Digest, V)],
    ) -> Result<Option<Digest>, StoreError> {
        let mut roots = self.index.roots()?;
        let mut pending = Pending::new(self.index.length());
        let mut digest = None;
        let roots_changed = commit.is_some() || !removed_roots.is_empty();
        if let Some((commit, edits)) = commit {
            let root = pending.commit_root(&self.index, &roots, commit, edits)?;
            digest = Some(root.digest);
            roots.insert(commit.root.to_owned(), root);
        }
        for name in removed_roots {
            roots.remove(name);
        }
        let runs = match attributes {
            [] => None,
            entries => Some(pending.append_attributes(&self.index, entries)?),
        };
        let roots = if roots_changed {
            Some(pending.append_node(read::ROOTS_NODE, &read::encode_roots(&roots))?)
        } else {
            None
        };
        self.publish(
            &pending.bytes,
            Named {
                roots,
                runs,
                checked: None,
            },
        )?;
        self.compact_if_due()?;
        Ok(digest)
    }

    /// Writes a commit's bytes and then its footer in one append. `None`
    /// keeps what the last footer named.
    pub(crate) fn publish(&mut self, bytes: &[u8], named: Named) -> Result<(), StoreError> {
        self.publish_after(0, bytes, named)
    }

    /// Writes the rest of a commit whose first `streamed` bytes are
    /// already in the file, then its footer. A commit of 1 MiB or more is
    /// flushed before its footer is written, so the footer can say it is
    /// on disk and no reader need hash it.
    pub(crate) fn publish_after(
        &mut self,
        streamed: u64,
        bytes: &[u8],
        named: Named,
    ) -> Result<(), StoreError> {
        let last = self.index.footer();
        let unsynced = self.index.tail().map_or(0, |tail| tail.unsynced);
        let start = self.index.length();
        let offset = start + streamed + bytes.len() as u64;
        self.append(bytes)?;
        let last_synced = last.map_or(read::HEADER_BYTES, |footer| footer.synced);
        let is_large = offset - start >= FLUSH_BYTES as u64;
        // Readers check every commit since the last flush when they open
        // the store, so that stretch is kept short.
        let is_overdue = offset - last_synced >= UNSYNCED_BYTES || unsynced + 1 >= UNSYNCED_COMMITS;
        let (synced, commit, unsynced) = if is_large || is_overdue {
            self.sync()?;
            (offset, [0; 32], 0)
        } else {
            let synced = last_synced;
            // Streaming is only for commits large enough to be flushed, so a
            // hashed commit is all in `bytes`.
            (synced, *blake3::hash(bytes).as_bytes(), unsynced + 1)
        };
        let footer = Footer {
            offset,
            roots: named.roots.or(last.map(|footer| footer.roots)).unwrap_or(0),
            runs: named.runs.or(last.map(|footer| footer.runs)).unwrap_or(0),
            previous: last.map_or(0, |footer| footer.offset),
            start,
            synced,
            checked: named
                .checked
                .or(last.map(|footer| footer.checked))
                .unwrap_or(0),
            commit,
        };
        self.append(&read::encode_footer(&footer))?;
        self.index = Index::open_written(&self.path, Tail { footer, unsynced })?;
        Ok(())
    }

    fn append(&mut self, bytes: &[u8]) -> Result<(), StoreError> {
        self.file.write_all(bytes).map_err(|source| StoreError::Io {
            path: self.path.clone(),
            source,
        })
    }

    fn sync(&self) -> Result<(), StoreError> {
        self.file.sync_data().map_err(|source| StoreError::Io {
            path: self.path.clone(),
            source,
        })
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
        self.index = Index::open_written(
            &self.path,
            Tail {
                footer,
                unsynced: 0,
            },
        )?;
        Ok(())
    }

    /// The store as this writer left it, read without hashing again what
    /// the writer wrote; the lock is let go.
    pub(crate) fn into_index(self) -> Index {
        self.index
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
