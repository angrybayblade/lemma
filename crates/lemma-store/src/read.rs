//! Reading a store: find the last complete commit, then read the roots and
//! segments it names.

use std::borrow::Cow;
use std::collections::BTreeMap;
use std::fs::File;
use std::path::{Path, PathBuf};

use lemma_mapped::Mapped;
use lemma_model::SymbolKind;
use rayon::prelude::*;

use crate::codec::{self, Decoder, Encoder};
use crate::segment::Segment;
use crate::view::View;
use crate::{Digest, FileRecord, SourceRecord, StoreError};

pub(crate) const MAGIC: [u8; 4] = *b"LMLS";
pub(crate) const FOOTER_MAGIC: [u8; 4] = *b"LSFT";
pub(crate) const MAJOR: u16 = 2;
pub(crate) const HEADER_BYTES: u64 = 8;
/// Magic, six offsets, the commit's digest and the footer's own.
pub(crate) const FOOTER_BYTES: u64 = 4 + 6 * 8 + 32 + 32;
const FOOTER_BODY: usize = 4 + 6 * 8 + 32;
pub(crate) const NODE_HEADER_BYTES: u64 = 5;
/// How much of the file's end is examined at a time for a footer.
const SCAN_BYTES: u64 = 1 << 20;

/// Below this many bytes of commits since the last flush, opening hashes
/// them on one thread.
const PARALLEL_HASH_BYTES: u64 = 1 << 20;

pub(crate) const ROOTS_NODE: u8 = 1;
pub(crate) const SEGMENT_NODE: u8 = 2;
pub(crate) const RUN_NODE: u8 = 3;
pub(crate) const RUNS_NODE: u8 = 4;

/// How a store's bytes reach memory. Both read the same bytes and give the
/// same answers.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ReadMode {
    /// A memory map: no copies, and pages shared between processes.
    Mapped,
    /// A read per request: for filesystems where maps are unreliable.
    Positional,
}

enum Bytes {
    Mapped(Mapped),
    Positional(File),
}

/// A commit's footer.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct Footer {
    pub offset: u64,
    /// The roots node, or 0 when no source has been committed.
    pub roots: u64,
    /// The list of attribute runs, or 0 when none have been written.
    pub runs: u64,
    /// The footer before, or 0.
    pub previous: u64,
    /// Where the commit's bytes start.
    pub start: u64,
    /// Every byte before this offset was flushed before the commit.
    pub synced: u64,
    /// The file's length when it was last checked for compaction.
    pub checked: u64,
    /// A digest of the commit's bytes, from `start` to this footer.
    pub commit: Digest,
}

/// A store as of its last complete commit.
pub struct Index {
    path: PathBuf,
    bytes: Bytes,
    length: u64,
    footer: Option<Footer>,
    /// How many commits since the last flush end with the footer.
    unsynced: usize,
}

/// Where a process last left a store: its last footer, and how many
/// commits since the last flush end with it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct Tail {
    pub footer: Footer,
    pub unsynced: usize,
}

/// A named source.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Root {
    /// What the source is.
    pub source: SourceRecord,
    pub(crate) digest: Digest,
    /// Its segments' offsets, oldest first.
    pub(crate) segments: Vec<u64>,
}

impl Root {
    /// A digest of the source's files: the same files give the same digest,
    /// however they were committed.
    pub fn digest(&self) -> Digest {
        self.digest
    }
}

/// A file that differs between two sources.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FileChange {
    /// Its path.
    pub path: String,
    /// The file in the first source, if it is there.
    pub before: Option<FileRecord>,
    /// The file in the second source, if it is there.
    pub after: Option<FileRecord>,
}

/// How a symbol differs between two sources.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SymbolChangeKind {
    /// Only in the second.
    Added,
    /// Only in the first.
    Removed,
    /// In both, with different syntax.
    Changed,
}

/// A symbol that differs between two sources.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SymbolChange {
    /// Its file.
    pub path: String,
    /// Its name with its containers.
    pub qualified_name: String,
    /// What kind of definition it is.
    pub kind: SymbolKind,
    /// How it differs.
    pub change: SymbolChangeKind,
}

impl Index {
    /// Opens the store at `path` as of its last complete commit.
    pub fn open(path: &Path, mode: ReadMode) -> Result<Self, StoreError> {
        let mut index = Self::map(path, mode)?;
        index.check_header()?;
        index.settle()?;
        Ok(index)
    }

    /// Finds the commit a reader should see.
    fn settle(&mut self) -> Result<(), StoreError> {
        let (footer, unsynced) = self.current_footer()?;
        self.footer = footer;
        self.unsynced = unsynced;
        Ok(())
    }

    /// Opens the store at `path` as it was when this process last left it
    /// at `tail`, written or checked by the process itself. While the file
    /// still ends with that footer, nothing after the last flush is hashed
    /// again; otherwise the store is opened as usual.
    pub(crate) fn open_written(path: &Path, tail: Tail) -> Result<Self, StoreError> {
        let mut index = Self::map(path, ReadMode::Mapped)?;
        let footer = tail.footer;
        let ends_with_footer = index.length == footer.offset + FOOTER_BYTES
            && index.footer_at(footer.offset) == Some(footer);
        if !ends_with_footer {
            index.check_header()?;
            index.settle()?;
            return Ok(index);
        }
        index.footer = Some(footer);
        index.unsynced = tail.unsynced;
        Ok(index)
    }

    fn map(path: &Path, mode: ReadMode) -> Result<Self, StoreError> {
        let io = |source| StoreError::Io {
            path: path.to_owned(),
            source,
        };
        let file = File::open(path).map_err(io)?;
        let length = file.metadata().map_err(io)?.len();
        // A map of an empty file is refused, and such a store has no
        // commits to read quickly anyway.
        let bytes = match mode {
            ReadMode::Mapped if length > 0 => Bytes::Mapped(Mapped::open(&file).map_err(io)?),
            _ => Bytes::Positional(file),
        };
        Ok(Self {
            path: path.to_owned(),
            bytes,
            length,
            footer: None,
            unsynced: 0,
        })
    }

    fn check_header(&self) -> Result<(), StoreError> {
        if self.length < HEADER_BYTES {
            // A store interrupted while being created has no commits.
            return Ok(());
        }
        let header = self.read(0, 8)?;
        if header[..4] != MAGIC {
            return Err(StoreError::NotAStore {
                path: self.path.clone(),
            });
        }
        let found = u16::from_le_bytes([header[4], header[5]]);
        if found != MAJOR {
            return Err(StoreError::UnsupportedVersion {
                found,
                expected: MAJOR,
            });
        }
        Ok(())
    }

    pub(crate) fn read(&self, offset: u64, count: usize) -> Result<Cow<'_, [u8]>, StoreError> {
        let outside = StoreError::Damaged {
            what: "a read is outside the file",
        };
        let end = u64::try_from(count)
            .ok()
            .and_then(|wanted| offset.checked_add(wanted))
            .ok_or(StoreError::Damaged {
                what: "a read overflows",
            })?;
        if end > self.length {
            return Err(outside);
        }
        match &self.bytes {
            Bytes::Mapped(mapped) => {
                let start = usize_of(offset)?;
                let slice = mapped.bytes().get(start..start + count).ok_or(outside)?;
                Ok(Cow::Borrowed(slice))
            }
            Bytes::Positional(file) => {
                let mut buffer = vec![0; count];
                read_exact_at(file, &mut buffer, offset).map_err(|source| StoreError::Io {
                    path: self.path.clone(),
                    source,
                })?;
                Ok(Cow::Owned(buffer))
            }
        }
    }

    /// The commit a reader should see: the last footer whose digest checks,
    /// stepped back past any commit written since the last flush whose
    /// bytes do not match its footer, as after a crash. With it, how many
    /// commits since the flush end with it.
    fn current_footer(&self) -> Result<(Option<Footer>, usize), StoreError> {
        let Some(last) = self.last_footer_before(self.length)? else {
            return Ok((None, 0));
        };
        // The commits since the last flush, newest first, then the newest
        // commit before them, which is trusted without reading it again.
        let mut unsynced = Vec::new();
        let mut trusted = None;
        let mut cursor = Some(last);
        while let Some(footer) = cursor {
            if footer.start < last.synced {
                trusted = Some(footer);
                break;
            }
            unsynced.push(footer);
            cursor = match footer.previous {
                0 => None,
                previous => match self.footer_at(previous) {
                    Some(found) => Some(found),
                    // A footer lost in the crash: look further back.
                    None => self.last_footer_before(footer.start)?,
                },
            };
        }
        // Each commit is hashed on its own, so a long stretch is checked
        // on every thread; the reader still stops at the first that does
        // not match. Waking the threads costs more than hashing a short one.
        unsynced.reverse();
        let bytes: u64 = unsynced
            .iter()
            .map(|footer| footer.offset.saturating_sub(footer.start))
            .sum();
        let matches: Vec<bool> = if bytes < PARALLEL_HASH_BYTES {
            unsynced
                .iter()
                .map(|footer| self.commit_matches(footer))
                .collect::<Result<_, _>>()?
        } else {
            unsynced
                .par_iter()
                .map(|footer| self.commit_matches(footer))
                .collect::<Result<_, _>>()?
        };
        let matching = matches.iter().take_while(|matches| **matches).count();
        Ok((unsynced[..matching].last().copied().or(trusted), matching))
    }

    fn commit_matches(&self, footer: &Footer) -> Result<bool, StoreError> {
        let Some(length) = footer.offset.checked_sub(footer.start) else {
            return Ok(false);
        };
        let bytes = self.read(footer.start, usize_of(length)?)?;
        Ok(blake3::hash(&bytes).as_bytes() == &footer.commit)
    }

    /// The last footer ending by `end` whose own digest checks.
    fn last_footer_before(&self, end: u64) -> Result<Option<Footer>, StoreError> {
        let mut end = end.min(self.length);
        while end >= HEADER_BYTES + FOOTER_BYTES {
            let start = end.saturating_sub(SCAN_BYTES).max(HEADER_BYTES);
            let window = self.read(start, usize_of(end - start)?)?;
            if let Some(footer) = self.footer_in(&window, start) {
                return Ok(Some(footer));
            }
            if start == HEADER_BYTES {
                break;
            }
            // Keep enough overlap that a magic across the boundary is seen.
            end = start + 3;
        }
        Ok(None)
    }

    /// The last valid footer starting in `window`, which begins at `start`.
    fn footer_in(&self, window: &[u8], start: u64) -> Option<Footer> {
        (0..window.len().saturating_sub(3))
            .rev()
            .filter(|at| window[*at..*at + 4] == FOOTER_MAGIC)
            .find_map(|at| self.footer_at(start + at as u64))
    }

    fn footer_at(&self, offset: u64) -> Option<Footer> {
        let bytes = self.read(offset, usize_of(FOOTER_BYTES).ok()?).ok()?;
        let (body, digest) = bytes.split_at(FOOTER_BODY);
        if body[..4] != FOOTER_MAGIC || blake3::hash(body).as_bytes() != digest {
            return None;
        }
        let mut decoder = Decoder::new(&body[4..]);
        Some(Footer {
            offset,
            roots: decoder.u64().ok()?,
            runs: decoder.u64().ok()?,
            previous: decoder.u64().ok()?,
            start: decoder.u64().ok()?,
            synced: decoder.u64().ok()?,
            checked: decoder.u64().ok()?,
            commit: decoder.digest().ok()?,
        })
    }

    pub(crate) fn footer(&self) -> Option<Footer> {
        self.footer
    }

    /// Where the store ends as this index read it.
    pub(crate) fn tail(&self) -> Option<Tail> {
        self.footer.map(|footer| Tail {
            footer,
            unsynced: self.unsynced,
        })
    }

    pub(crate) fn length(&self) -> u64 {
        self.length
    }

    /// A node's payload, after checking its kind.
    pub(crate) fn node(&self, offset: u64, kind: u8) -> Result<Cow<'_, [u8]>, StoreError> {
        let (payload, length) = self.node_span(offset, kind)?;
        self.read(payload, length)
    }

    /// Where a node's payload starts, and its length, after checking its
    /// kind.
    pub(crate) fn node_span(&self, offset: u64, kind: u8) -> Result<(u64, usize), StoreError> {
        let header = self.read(offset, 5)?;
        if header[0] != kind {
            return Err(StoreError::Damaged {
                what: "a node is of the wrong kind",
            });
        }
        let length = u32::from_le_bytes([header[1], header[2], header[3], header[4]]);
        Ok((offset + NODE_HEADER_BYTES, length as usize))
    }

    /// Every root, by name.
    pub(crate) fn roots(&self) -> Result<BTreeMap<String, Root>, StoreError> {
        let Some(footer) = self.footer.filter(|footer| footer.roots != 0) else {
            return Ok(BTreeMap::new());
        };
        decode_roots(&self.node(footer.roots, ROOTS_NODE)?)
    }

    /// The names of every source, sorted.
    pub fn root_names(&self) -> Result<Vec<String>, StoreError> {
        Ok(self.roots()?.into_keys().collect())
    }

    /// The source called `name`.
    pub fn root(&self, name: &str) -> Result<Root, StoreError> {
        self.roots()?
            .remove(name)
            .ok_or_else(|| StoreError::UnknownRoot {
                name: name.to_owned(),
            })
    }

    pub(crate) fn segment(&self, offset: u64) -> Result<Segment<'_>, StoreError> {
        Segment::parse(self.node(offset, SEGMENT_NODE)?)
    }

    pub(crate) fn view(&self, root: &Root) -> Result<View<'_>, StoreError> {
        let segments = root
            .segments
            .iter()
            .map(|offset| self.segment(*offset))
            .collect::<Result<_, _>>()?;
        Ok(View::new(segments))
    }

    /// The file at `path` in `root`, if there is one.
    pub fn file(&self, root: &Root, path: &str) -> Result<Option<FileRecord>, StoreError> {
        let view = self.view(root)?;
        let Some(at) = view.locate(path)? else {
            return Ok(None);
        };
        view.record(at).map(Some)
    }

    /// Every file of `root`, sorted by path.
    pub fn files(&self, root: &Root) -> Result<Vec<(String, FileRecord)>, StoreError> {
        let view = self.view(root)?;
        view.files()?
            .into_par_iter()
            .map(|at| Ok((view.path(at)?.to_owned(), view.record(at)?)))
            .collect()
    }

    /// Every file's blob digest, without decoding the files.
    pub fn blobs(&self, root: &Root) -> Result<BTreeMap<String, Digest>, StoreError> {
        let view = self.view(root)?;
        view.files()?
            .into_iter()
            .map(|at| Ok((view.path(at)?.to_owned(), view.row(at)?.blob)))
            .collect()
    }

    /// The files that differ between two sources, sorted by path. When one
    /// source's segments extend the other's, only the extra segments' paths
    /// are compared.
    pub fn changed_files(&self, base: &Root, head: &Root) -> Result<Vec<FileChange>, StoreError> {
        let before = self.view(base)?;
        let after = self.view(head)?;
        let shared = base
            .segments
            .iter()
            .zip(&head.segments)
            .take_while(|(left, right)| left == right)
            .count();
        let extends = shared == base.segments.len().min(head.segments.len());
        let paths: Vec<String> = if extends {
            extra_paths(&before, &after, shared)?
        } else {
            all_paths(&before, &after)?
        };
        let mut changes = Vec::new();
        for path in paths {
            changes.extend(compare_path(&before, &after, &path)?);
        }
        Ok(changes)
    }
}

/// The paths held by either view's segments after the first `shared`.
fn extra_paths(
    before: &View<'_>,
    after: &View<'_>,
    shared: usize,
) -> Result<Vec<String>, StoreError> {
    let mut paths = std::collections::BTreeSet::new();
    for view in [before, after] {
        for segment in view.segments().iter().skip(shared) {
            for file in 0..segment.file_count() {
                paths.insert(segment.path(file)?.to_owned());
            }
        }
    }
    Ok(paths.into_iter().collect())
}

/// Every path either view has.
fn all_paths(before: &View<'_>, after: &View<'_>) -> Result<Vec<String>, StoreError> {
    let mut paths = std::collections::BTreeSet::new();
    for view in [before, after] {
        for at in view.files()? {
            paths.insert(view.path(at)?.to_owned());
        }
    }
    Ok(paths.into_iter().collect())
}

/// How `path` differs between two views, if it does.
fn compare_path(
    before: &View<'_>,
    after: &View<'_>,
    path: &str,
) -> Result<Option<FileChange>, StoreError> {
    let old = before.locate(path)?;
    let new = after.locate(path)?;
    if let (Some(old), Some(new)) = (old, new)
        && before.row(old)?.blob == after.row(new)?.blob
    {
        return Ok(None);
    }
    if old.is_none() && new.is_none() {
        return Ok(None);
    }
    Ok(Some(FileChange {
        path: path.to_owned(),
        before: old.map(|at| before.record(at)).transpose()?,
        after: new.map(|at| after.record(at)).transpose()?,
    }))
}

/// What differs between the symbols of one changed file, sorted by name.
/// A symbol that only moved within the file keeps its digest and is not
/// reported.
pub fn changed_symbols(change: &FileChange) -> Vec<SymbolChange> {
    type Key = (String, u8, u32);
    let digests = |file: &Option<FileRecord>| -> BTreeMap<Key, (SymbolKind, Digest)> {
        file.iter()
            .flat_map(|record| &record.symbols)
            .map(|symbol| {
                let key = (
                    symbol.qualified_name.clone(),
                    codec::kind_code(symbol.kind),
                    symbol.discriminator,
                );
                (key, (symbol.kind, symbol.digest))
            })
            .collect()
    };
    let before = digests(&change.before);
    let after = digests(&change.after);
    let keys: std::collections::BTreeSet<&Key> = before.keys().chain(after.keys()).collect();
    let mut changes = Vec::new();
    for key in keys {
        let (kind, how) = match (before.get(key), after.get(key)) {
            (Some(old), Some(new)) if old.1 == new.1 => continue,
            (Some(_), Some(new)) => (new.0, SymbolChangeKind::Changed),
            (None, Some(new)) => (new.0, SymbolChangeKind::Added),
            (Some(old), None) => (old.0, SymbolChangeKind::Removed),
            (None, None) => continue,
        };
        changes.push(SymbolChange {
            path: change.path.clone(),
            qualified_name: key.0.clone(),
            kind,
            change: how,
        });
    }
    changes
}

pub(crate) fn encode_roots(roots: &BTreeMap<String, Root>) -> Vec<u8> {
    let mut encoder = Encoder::default();
    encoder.count(roots.len());
    for (name, root) in roots {
        encoder.text(name);
        encoder.text(&root.source.repository);
        encoder.text(&root.source.revision);
        encoder.digest(&root.source.profile);
        encoder.digest(&root.digest);
        encoder.count(root.segments.len());
        for offset in &root.segments {
            encoder.u64(*offset);
        }
    }
    encoder.bytes
}

fn decode_roots(payload: &[u8]) -> Result<BTreeMap<String, Root>, StoreError> {
    let mut decoder = Decoder::new(payload);
    let count = decoder.count(4 * 4 + 64 + 4)?;
    let mut roots = BTreeMap::new();
    for _ in 0..count {
        let name = decoder.text()?.to_owned();
        let source = SourceRecord {
            repository: decoder.text()?.to_owned(),
            revision: decoder.text()?.to_owned(),
            profile: decoder.digest()?,
        };
        let digest = decoder.digest()?;
        let segment_count = decoder.count(8)?;
        let segments = (0..segment_count)
            .map(|_| decoder.u64())
            .collect::<Result<_, _>>()?;
        roots.insert(
            name,
            Root {
                source,
                digest,
                segments,
            },
        );
    }
    Ok(roots)
}

/// A footer for a commit of `commit_bytes` starting at `footer.start`.
pub(crate) fn encode_footer(footer: &Footer) -> Vec<u8> {
    let mut encoder = Encoder::default();
    encoder.bytes.extend_from_slice(&FOOTER_MAGIC);
    encoder.u64(footer.roots);
    encoder.u64(footer.runs);
    encoder.u64(footer.previous);
    encoder.u64(footer.start);
    encoder.u64(footer.synced);
    encoder.u64(footer.checked);
    encoder.digest(&footer.commit);
    let digest = blake3::hash(&encoder.bytes);
    encoder.digest(digest.as_bytes());
    encoder.bytes
}

pub(crate) fn usize_of(value: u64) -> Result<usize, StoreError> {
    usize::try_from(value).map_err(|_| StoreError::Damaged {
        what: "a length is too large",
    })
}

#[cfg(unix)]
fn read_exact_at(file: &File, buffer: &mut [u8], offset: u64) -> std::io::Result<()> {
    std::os::unix::fs::FileExt::read_exact_at(file, buffer, offset)
}

#[cfg(not(unix))]
fn read_exact_at(file: &File, buffer: &mut [u8], offset: u64) -> std::io::Result<()> {
    use std::io::{Read, Seek, SeekFrom};
    let mut file = file.try_clone()?;
    file.seek(SeekFrom::Start(offset))?;
    file.read_exact(buffer)
}
