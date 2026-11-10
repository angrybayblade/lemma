//! Reading a store: find the last complete commit, then follow its nodes.

use std::borrow::Cow;
use std::collections::{BTreeMap, HashSet};
use std::fs::File;
use std::path::{Path, PathBuf};
use std::sync::Mutex;

use lemma_mapped::Mapped;
use lemma_model::SymbolKind;

use crate::codec::{self, Decoder, Encoder, Entry, EntryKind};
use crate::{Digest, FileRecord, SourceRecord, StoreError};

pub(crate) const MAGIC: [u8; 4] = *b"LMLS";
pub(crate) const FOOTER_MAGIC: [u8; 4] = *b"LSFT";
pub(crate) const MAJOR: u16 = 1;
pub(crate) const HEADER_BYTES: u64 = 8;
pub(crate) const FOOTER_BYTES: u64 = 60;
const NODE_HEADER_BYTES: usize = 5;
const NODE_DIGEST_BYTES: usize = 32;
/// How much of the file's end is examined at a time for a footer.
const SCAN_BYTES: u64 = 1 << 20;

pub(crate) const FILE_NODE: u8 = 1;
pub(crate) const DIRECTORY_NODE: u8 = 2;
pub(crate) const SOURCE_NODE: u8 = 3;
pub(crate) const ROOTS_NODE: u8 = 4;

/// How a store's bytes reach memory. Both read the same bytes and give the
/// same answers.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ReadMode {
    /// A memory map: no copies, and pages shared between processes.
    Mapped,
    /// A read per node: for filesystems where maps are unreliable.
    Positional,
}

enum Bytes {
    Mapped(Mapped),
    Positional(File),
}

/// A commit's footer.
#[derive(Debug, Clone, Copy)]
pub(crate) struct Footer {
    pub offset: u64,
    pub roots: u64,
}

/// A store as of its last complete commit.
pub struct Index {
    path: PathBuf,
    bytes: Bytes,
    length: u64,
    footer: Option<Footer>,
    /// The nodes whose digests have been checked.
    verified: Mutex<HashSet<u64>>,
}

/// A named source.
#[derive(Debug, Clone)]
pub struct Root {
    /// What the source is.
    pub source: SourceRecord,
    pub(crate) tree: Entry,
}

impl Root {
    /// A digest of everything in the source's tree.
    pub fn digest(&self) -> Digest {
        self.tree.digest
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
        let mut index = Self {
            path: path.to_owned(),
            bytes,
            length,
            footer: None,
            verified: Mutex::new(HashSet::new()),
        };
        index.check_header()?;
        index.footer = index.last_footer()?;
        Ok(index)
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

    fn read(&self, offset: u64, count: usize) -> Result<Cow<'_, [u8]>, StoreError> {
        let outside = StoreError::Damaged {
            what: "a read is outside the file",
        };
        let wanted = u64::try_from(count).map_err(|_| StoreError::Damaged {
            what: "a read is too long",
        })?;
        let end = offset.checked_add(wanted).ok_or(StoreError::Damaged {
            what: "a read overflows",
        })?;
        if end > self.length {
            return Err(outside);
        }
        match &self.bytes {
            Bytes::Mapped(mapped) => {
                let start = usize::try_from(offset).map_err(|_| StoreError::Damaged {
                    what: "an offset is too large",
                })?;
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

    /// The last footer whose digest checks. Anything after it is a commit
    /// that did not finish.
    fn last_footer(&self) -> Result<Option<Footer>, StoreError> {
        let mut end = self.length;
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
        let (body, digest) = bytes.split_at(28);
        if blake3::hash(body).as_bytes() != digest {
            return None;
        }
        let mut decoder = Decoder::new(&body[4..]);
        let roots = decoder.u64().ok()?;
        Some(Footer { offset, roots })
    }

    pub(crate) fn footer(&self) -> Option<Footer> {
        self.footer
    }

    pub(crate) fn length(&self) -> u64 {
        self.length
    }

    /// A node's payload, after checking its kind and, on first use, its
    /// digest.
    fn node(&self, offset: u64, kind: u8) -> Result<Payload<'_>, StoreError> {
        let header = self.read(offset, NODE_HEADER_BYTES)?;
        if header[0] != kind {
            return Err(StoreError::Damaged {
                what: "a node is of the wrong kind",
            });
        }
        let length = u32::from_le_bytes([header[1], header[2], header[3], header[4]]);
        let length = usize::try_from(length).map_err(|_| StoreError::Damaged {
            what: "a node is too long",
        })?;
        let bytes = self.read(offset, NODE_HEADER_BYTES + length + NODE_DIGEST_BYTES)?;
        let end = NODE_HEADER_BYTES + length;
        if !self.is_verified(offset) {
            if blake3::hash(&bytes[..end]).as_bytes() != &bytes[end..] {
                return Err(StoreError::Damaged {
                    what: "a node's digest does not match",
                });
            }
            self.mark_verified(offset);
        }
        Ok(Payload { bytes, end })
    }

    fn is_verified(&self, offset: u64) -> bool {
        self.verified
            .lock()
            .is_ok_and(|verified| verified.contains(&offset))
    }

    fn mark_verified(&self, offset: u64) {
        // A poisoned set only means the node is checked again.
        if let Ok(mut verified) = self.verified.lock() {
            verified.insert(offset);
        }
    }

    pub(crate) fn root_offsets(&self) -> Result<BTreeMap<String, u64>, StoreError> {
        let Some(footer) = self.footer else {
            return Ok(BTreeMap::new());
        };
        let payload = self.node(footer.roots, ROOTS_NODE)?;
        let mut decoder = Decoder::new(payload.bytes());
        let count = decoder.count(8)?;
        let mut roots = BTreeMap::new();
        for _ in 0..count {
            let name = decoder.text()?.to_owned();
            roots.insert(name, decoder.u64()?);
        }
        Ok(roots)
    }

    /// The names of every source, sorted.
    pub fn root_names(&self) -> Result<Vec<String>, StoreError> {
        Ok(self.root_offsets()?.into_keys().collect())
    }

    /// The source called `name`.
    pub fn root(&self, name: &str) -> Result<Root, StoreError> {
        let roots = self.root_offsets()?;
        let offset = roots.get(name).ok_or_else(|| StoreError::UnknownRoot {
            name: name.to_owned(),
        })?;
        let payload = self.node(*offset, SOURCE_NODE)?;
        let decoded = codec::decode_source(payload.bytes())?;
        Ok(Root {
            source: decoded.record,
            tree: decoded.tree,
        })
    }

    pub(crate) fn directory(&self, offset: u64) -> Result<Vec<(String, Entry)>, StoreError> {
        codec::decode_directory(self.node(offset, DIRECTORY_NODE)?.bytes())
    }

    fn file_at(&self, offset: u64) -> Result<FileRecord, StoreError> {
        codec::decode_file(self.node(offset, FILE_NODE)?.bytes())
    }

    /// The file at `path` in `root`, if there is one.
    pub fn file(&self, root: &Root, path: &str) -> Result<Option<FileRecord>, StoreError> {
        let mut entry = root.tree;
        let mut names = path.split('/').peekable();
        while let Some(name) = names.next() {
            let children = self.directory(entry.offset)?;
            let found = children.binary_search_by(|(child, _)| child.as_str().cmp(name));
            let Ok(position) = found else {
                return Ok(None);
            };
            entry = children[position].1;
            let wanted = match names.peek() {
                Some(_) => EntryKind::Directory,
                None => EntryKind::File,
            };
            if entry.kind != wanted {
                return Ok(None);
            }
        }
        self.file_at(entry.offset).map(Some)
    }

    /// Every file of `root`, sorted by path.
    pub fn files(&self, root: &Root) -> Result<Vec<(String, FileRecord)>, StoreError> {
        let mut files = Vec::new();
        self.visit_files(root.tree.offset, "", &mut |path, offset| {
            files.push((path.to_owned(), self.file_at(offset)?));
            Ok(())
        })?;
        files.sort_by(|left, right| left.0.cmp(&right.0));
        Ok(files)
    }

    /// Every file's blob digest, without decoding the files.
    pub fn blobs(&self, root: &Root) -> Result<BTreeMap<String, Digest>, StoreError> {
        let mut blobs = BTreeMap::new();
        self.visit_files(root.tree.offset, "", &mut |path, offset| {
            let payload = self.node(offset, FILE_NODE)?;
            blobs.insert(path.to_owned(), codec::decode_blob(payload.bytes())?);
            Ok(())
        })?;
        Ok(blobs)
    }

    fn visit_files(
        &self,
        directory: u64,
        prefix: &str,
        visit: &mut dyn FnMut(&str, u64) -> Result<(), StoreError>,
    ) -> Result<(), StoreError> {
        for (name, entry) in self.directory(directory)? {
            let path = join(prefix, &name);
            match entry.kind {
                EntryKind::File => visit(&path, entry.offset)?,
                EntryKind::Directory => self.visit_files(entry.offset, &path, visit)?,
            }
        }
        Ok(())
    }

    /// The files that differ between two sources, sorted by path. Reads
    /// only the directories whose digests differ.
    pub fn changed_files(&self, base: &Root, head: &Root) -> Result<Vec<FileChange>, StoreError> {
        let mut changes = Vec::new();
        self.compare(Some(base.tree), Some(head.tree), "", &mut changes)?;
        changes.sort_by(|left, right| left.path.cmp(&right.path));
        Ok(changes)
    }

    fn compare(
        &self,
        before: Option<Entry>,
        after: Option<Entry>,
        path: &str,
        changes: &mut Vec<FileChange>,
    ) -> Result<(), StoreError> {
        if before.map(|e| (e.kind, e.digest)) == after.map(|e| (e.kind, e.digest)) {
            return Ok(());
        }
        let before_file = self.file_of(before)?;
        let after_file = self.file_of(after)?;
        if before_file.is_some() || after_file.is_some() {
            changes.push(FileChange {
                path: path.to_owned(),
                before: before_file,
                after: after_file,
            });
        }
        let mut children: BTreeMap<String, (Option<Entry>, Option<Entry>)> = BTreeMap::new();
        for (name, entry) in self.children_of(before)? {
            children.entry(name).or_default().0 = Some(entry);
        }
        for (name, entry) in self.children_of(after)? {
            children.entry(name).or_default().1 = Some(entry);
        }
        for (name, (left, right)) in children {
            self.compare(left, right, &join(path, &name), changes)?;
        }
        Ok(())
    }

    fn file_of(&self, entry: Option<Entry>) -> Result<Option<FileRecord>, StoreError> {
        match entry {
            Some(entry) if entry.kind == EntryKind::File => self.file_at(entry.offset).map(Some),
            _ => Ok(None),
        }
    }

    fn children_of(&self, entry: Option<Entry>) -> Result<Vec<(String, Entry)>, StoreError> {
        match entry {
            Some(entry) if entry.kind == EntryKind::Directory => self.directory(entry.offset),
            _ => Ok(Vec::new()),
        }
    }
}

/// A node's bytes, kept whole so a mapped read need not copy them.
struct Payload<'a> {
    bytes: Cow<'a, [u8]>,
    end: usize,
}

impl Payload<'_> {
    fn bytes(&self) -> &[u8] {
        &self.bytes[NODE_HEADER_BYTES..self.end]
    }
}

/// What differs between the symbols of one changed file, sorted by name.
/// A symbol that only moved within the file keeps its digest and is not
/// reported.
pub fn changed_symbols(change: &FileChange) -> Vec<SymbolChange> {
    type Key<'a> = (&'a str, u8, u32);
    let digests =
        |file: &'_ Option<FileRecord>| -> BTreeMap<(String, u8, u32), (SymbolKind, Digest)> {
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
    let mut keys: Vec<Key<'_>> = before
        .keys()
        .chain(after.keys())
        .map(|(name, kind, discriminator)| (name.as_str(), *kind, *discriminator))
        .collect();
    keys.sort_unstable();
    keys.dedup();
    let mut changes = Vec::new();
    for (name, kind, discriminator) in keys {
        let key = (name.to_owned(), kind, discriminator);
        let (kind, how) = match (before.get(&key), after.get(&key)) {
            (Some(old), Some(new)) if old.1 == new.1 => continue,
            (Some(_), Some(new)) => (new.0, SymbolChangeKind::Changed),
            (None, Some(new)) => (new.0, SymbolChangeKind::Added),
            (Some(old), None) => (old.0, SymbolChangeKind::Removed),
            (None, None) => continue,
        };
        changes.push(SymbolChange {
            path: change.path.clone(),
            qualified_name: key.0,
            kind,
            change: how,
        });
    }
    changes
}

fn join(prefix: &str, name: &str) -> String {
    if prefix.is_empty() {
        return name.to_owned();
    }
    format!("{prefix}/{name}")
}

fn usize_of(value: u64) -> Result<usize, StoreError> {
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

/// Encodes a footer naming the roots node at `roots`.
pub(crate) fn encode_footer(roots: u64, previous: u64) -> Vec<u8> {
    let mut encoder = Encoder::default();
    encoder.bytes.extend_from_slice(&FOOTER_MAGIC);
    encoder.u64(roots);
    // Attributes arrive in stage 4 of spec 17.
    encoder.u64(0);
    encoder.u64(previous);
    let digest = blake3::hash(&encoder.bytes);
    encoder.digest(digest.as_bytes());
    encoder.bytes
}
