//! Navigation's view of a store (spec 17): snapshots of directories, with
//! their files' stamps, stable symbol ids, exports, workspace members and
//! providers' answers, all in one `index.ls`.
//!
//! Each snapshot is a source named `snapshot:<id>`. What is not part of a
//! source's tree is filed in the attribute table: a catalog of snapshots
//! and exports, each file's stamp, each symbol's id, and answers. A write
//! is one commit, so a reader sees all of it or none of it.

use std::collections::{BTreeMap, BTreeSet};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, RwLock};

use lemma_model::{Export, ExportLink, Location, Position, ProviderAnswer, Snapshot, SymbolKind};
use serde::{Deserialize, Serialize};

use crate::attributes::Run;
use crate::codec::{self, Decoder, Encoder};
use crate::facts::{
    AnswerKey, FactStore, FileKey, FileStamp, NameMatch, NewSnapshot, SnapshotUpdate, StampedFile,
    StoredAnswer, StoredFile, StoredMember, StoredSnapshot, StoredSymbol, SymbolFilter, SymbolKey,
    hex,
};
use crate::read::{Index, ReadMode, Root};
use crate::{Change, Commit, Digest, FileRecord, SourceRecord, StoreError, Write, Writer};

/// How many answers wait in memory before they are written.
const PENDING_ANSWERS: usize = 256;

/// Values to file in the attribute table.
type Attributes = Vec<(Digest, Vec<u8>)>;

/// A file of a snapshot with its symbols' ids.
type IdentifiedFile = (String, FileRecord, Vec<i64>);

/// Navigation's store in one `index.ls`.
pub struct Store {
    path: PathBuf,
    current: RwLock<Arc<Opened>>,
    /// Answers saved but not yet written, by key.
    pending: BTreeMap<Digest, Vec<u8>>,
    /// Whether the file is a temporary one, removed when the store is.
    is_temporary: bool,
}

/// The store as last opened.
struct Opened {
    index: Index,
    runs: Vec<Run>,
    length: u64,
    catalog: Catalog,
}

/// What the store holds beside its sources' trees.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
struct Catalog {
    next_snapshot: i64,
    next_symbol: i64,
    snapshots: Vec<Entry>,
    members: Vec<Member>,
    workspace_exports: Vec<(String, Export)>,
    workspace_links: Vec<ExportLink>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
struct Entry {
    id: i64,
    snapshot: Snapshot,
    extractor_version: String,
    indexed_at_ns: i64,
    uncommitted: Vec<String>,
    /// Raised whenever file contents change, which retires every answer
    /// filed under the previous value.
    generation: u64,
    exports: Option<Published>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
struct Published {
    fingerprint: String,
    list: Vec<Export>,
    links: Vec<ExportLink>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
struct Member {
    name: String,
    root: String,
    revision: Option<String>,
    indexed_root: String,
    database: String,
}

fn key(space: &str, parts: &[&[u8]]) -> Digest {
    let mut hasher = blake3::Hasher::new();
    hasher.update(space.as_bytes());
    for part in parts {
        hasher.update(&(part.len() as u64).to_le_bytes());
        hasher.update(part);
    }
    *hasher.finalize().as_bytes()
}

fn catalog_key() -> Digest {
    key("catalog", &[])
}

fn stamp_key(snapshot_id: i64, path: &str) -> Digest {
    key("stamp", &[&snapshot_id.to_le_bytes(), path.as_bytes()])
}

fn ids_key(path: &str, file: &Digest) -> Digest {
    key("ids", &[path.as_bytes(), file])
}

fn identity_key(path: &str, qualified_name: &str, kind: SymbolKind, discriminator: u32) -> Digest {
    key(
        "identity",
        &[
            path.as_bytes(),
            qualified_name.as_bytes(),
            &[codec::kind_code(kind)],
            &discriminator.to_le_bytes(),
        ],
    )
}

fn symbol_key(id: i64) -> Digest {
    key("symbol", &[&id.to_le_bytes()])
}

fn answer_key(generation: u64, answer: &AnswerKey<'_>) -> Digest {
    key(
        "answer",
        &[
            &answer.snapshot_id.to_le_bytes(),
            &generation.to_le_bytes(),
            answer.question.as_str().as_bytes(),
            answer.path.as_bytes(),
            &answer.position.line.to_le_bytes(),
            &answer.position.column.to_le_bytes(),
            answer.provider_id.as_bytes(),
            answer.config_fingerprint.as_bytes(),
        ],
    )
}

fn root_name(snapshot_id: i64) -> String {
    format!("snapshot:{snapshot_id}")
}

static TEMPORARY: AtomicU64 = AtomicU64::new(0);

impl Store {
    /// Opens or creates the store at `path`.
    pub fn open(path: &Path) -> Result<Self, StoreError> {
        Self::open_as(path, false)
    }

    /// A store in a temporary file, removed when the store is dropped.
    pub fn open_in_memory() -> Result<Self, StoreError> {
        let number = TEMPORARY.fetch_add(1, Ordering::Relaxed);
        let name = format!("lemma-store-{}-{number}.ls", std::process::id());
        Self::open_as(&std::env::temp_dir().join(name), true)
    }

    fn open_as(path: &Path, is_temporary: bool) -> Result<Self, StoreError> {
        // Creating the file needs the writer; it is let go at once.
        drop(Writer::open_waiting(path)?);
        let opened = open_current(path)?;
        Ok(Self {
            path: path.to_owned(),
            current: RwLock::new(Arc::new(opened)),
            pending: BTreeMap::new(),
            is_temporary,
        })
    }

    /// The store as it is now, reopened when another writer has grown it.
    fn opened(&self) -> Result<Arc<Opened>, StoreError> {
        let length = std::fs::metadata(&self.path)
            .map_err(|source| StoreError::Io {
                path: self.path.clone(),
                source,
            })?
            .len();
        if let Ok(current) = self.current.read()
            && current.length == length
        {
            return Ok(Arc::clone(&current));
        }
        let opened = Arc::new(open_current(&self.path)?);
        if let Ok(mut current) = self.current.write() {
            *current = Arc::clone(&opened);
        }
        Ok(opened)
    }

    fn catalog(&self) -> Result<Catalog, StoreError> {
        Ok(self.opened()?.catalog.clone())
    }

    fn write(&mut self, write: &Write<'_>) -> Result<(), StoreError> {
        Writer::open_waiting(&self.path)?.write(write)?;
        let opened = open_current(&self.path)?;
        if let Ok(mut current) = self.current.write() {
            *current = Arc::new(opened);
        }
        Ok(())
    }

    /// Stores a snapshot and its files, replacing earlier snapshots of the
    /// same root. A symbol still there under the same path, qualified name
    /// and kind keeps its id.
    pub fn replace_snapshot(&mut self, new: &NewSnapshot<'_>) -> Result<i64, StoreError> {
        let mut catalog = self.catalog()?;
        let replaced: Vec<String> = catalog
            .snapshots
            .iter()
            .filter(|entry| entry.snapshot.root == new.snapshot.root)
            .map(|entry| root_name(entry.id))
            .collect();
        catalog
            .snapshots
            .retain(|entry| entry.snapshot.root != new.snapshot.root);
        catalog.next_snapshot += 1;
        let id = catalog.next_snapshot;
        catalog.snapshots.push(Entry {
            id,
            snapshot: new.snapshot.clone(),
            extractor_version: new.extractor_version.to_owned(),
            indexed_at_ns: new.indexed_at_ns,
            uncommitted: sorted(new.uncommitted_paths),
            generation: 0,
            exports: None,
        });
        let written = self.file_changes(id, &mut catalog, new.files, &[])?;
        let (changes, attributes) = written.split(&catalog)?;
        let name = root_name(id);
        let source = source_of(new.snapshot, new.extractor_version);
        let commit = Commit {
            root: &name,
            parent: None,
            source: &source,
        };
        self.write(&Write {
            commit: Some((commit, &changes)),
            removed_roots: &replaced,
            attributes: &attributes,
        })?;
        Ok(id)
    }

    /// Updates a stored snapshot, rewriting only the files that changed.
    /// When any file's contents changed, every cached answer for the
    /// snapshot is retired: an edit in one file can change the references
    /// to a symbol defined in another.
    pub fn update_snapshot(
        &mut self,
        snapshot_id: i64,
        update: &SnapshotUpdate<'_>,
    ) -> Result<(), StoreError> {
        let mut catalog = self.catalog()?;
        let Some(entry) = catalog
            .snapshots
            .iter_mut()
            .find(|entry| entry.id == snapshot_id)
        else {
            return Ok(());
        };
        entry.snapshot = update.snapshot.clone();
        entry.indexed_at_ns = update.indexed_at_ns;
        entry.uncommitted = sorted(update.uncommitted_paths);
        let contents_changed = !update.changed.is_empty() || !update.removed.is_empty();
        if contents_changed {
            entry.generation += 1;
        }
        let extractor_version = entry.extractor_version.clone();
        let mut changes =
            self.file_changes(snapshot_id, &mut catalog, update.changed, update.restamped)?;
        changes.changes.extend(
            update
                .removed
                .iter()
                .map(|path| Change::Remove { path: path.clone() }),
        );
        let (changes, attributes) = changes.split(&catalog)?;
        let name = root_name(snapshot_id);
        let source = source_of(update.snapshot, &extractor_version);
        let commit = Commit {
            root: &name,
            parent: Some(&name),
            source: &source,
        };
        self.write(&Write {
            commit: Some((commit, &changes)),
            removed_roots: &[],
            attributes: &attributes,
        })
    }

    /// The changes and attributes for files written to a snapshot, giving
    /// new symbols the next ids.
    fn file_changes(
        &self,
        snapshot_id: i64,
        catalog: &mut Catalog,
        files: &[StampedFile],
        restamped: &[(String, FileStamp)],
    ) -> Result<FileChanges, StoreError> {
        let opened = self.opened()?;
        let mut written = FileChanges::default();
        for file in files {
            let digest = codec::file_digest(&file.record);
            let mut ids = Vec::with_capacity(file.record.symbols.len());
            for symbol in &file.record.symbols {
                ids.push(id_for(&opened, catalog, &mut written, &file.path, symbol)?);
            }
            written
                .attributes
                .push((ids_key(&file.path, &digest), encode_ids(&ids)));
            written
                .attributes
                .push((stamp_key(snapshot_id, &file.path), encode_stamp(file.stamp)));
            written.changes.push(Change::Put {
                path: file.path.clone(),
                record: file.record.clone(),
            });
        }
        for (path, stamp) in restamped {
            written
                .attributes
                .push((stamp_key(snapshot_id, path), encode_stamp(*stamp)));
        }
        Ok(written)
    }

    /// The stored snapshot of a directory, if it has been indexed.
    pub fn snapshot_for_root(&self, root: &str) -> Result<Option<StoredSnapshot>, StoreError> {
        let catalog = self.catalog()?;
        Ok(catalog
            .snapshots
            .iter()
            .find(|entry| entry.snapshot.root == root)
            .map(stored_snapshot))
    }

    /// Source files that differed from the snapshot's revision when it was
    /// taken, sorted.
    pub fn uncommitted_paths(&self, snapshot_id: i64) -> Result<Vec<String>, StoreError> {
        let catalog = self.catalog()?;
        Ok(entry(&catalog, snapshot_id).map_or_else(Vec::new, |entry| entry.uncommitted.clone()))
    }

    /// Replaces a snapshot's exports and links, recording what they were
    /// built from.
    pub fn replace_exports(
        &mut self,
        snapshot_id: i64,
        fingerprint: &str,
        exports: &[Export],
        links: &[ExportLink],
    ) -> Result<(), StoreError> {
        let mut catalog = self.catalog()?;
        let Some(entry) = catalog
            .snapshots
            .iter_mut()
            .find(|entry| entry.id == snapshot_id)
        else {
            return Ok(());
        };
        let owned = exports.iter().map(|export| (String::new(), export.clone()));
        entry.exports = Some(Published {
            fingerprint: fingerprint.to_owned(),
            list: unique_exports(owned)
                .into_iter()
                .map(|(_, export)| export)
                .collect(),
            links: unique_links(links),
        });
        self.write_catalog(&catalog)
    }

    fn write_catalog(&mut self, catalog: &Catalog) -> Result<(), StoreError> {
        let attributes = [(catalog_key(), encode_catalog(catalog)?)];
        self.write(&Write {
            commit: None,
            removed_roots: &[],
            attributes: &attributes,
        })
    }

    /// What a snapshot's exports were last built from, if they were.
    pub fn exports_fingerprint(&self, snapshot_id: i64) -> Result<Option<String>, StoreError> {
        let catalog = self.catalog()?;
        Ok(entry(&catalog, snapshot_id)
            .and_then(|entry| entry.exports.as_ref())
            .map(|exports| exports.fingerprint.clone()))
    }

    /// A snapshot's exports, by service, name and kind.
    pub fn exports(&self, snapshot_id: i64) -> Result<Vec<Export>, StoreError> {
        let catalog = self.catalog()?;
        Ok(entry(&catalog, snapshot_id)
            .and_then(|entry| entry.exports.as_ref())
            .map_or_else(Vec::new, |exports| exports.list.clone()))
    }

    /// A snapshot's declared links, sorted.
    pub fn export_links(&self, snapshot_id: i64) -> Result<Vec<ExportLink>, StoreError> {
        let catalog = self.catalog()?;
        Ok(entry(&catalog, snapshot_id)
            .and_then(|entry| entry.exports.as_ref())
            .map_or_else(Vec::new, |exports| exports.links.clone()))
    }

    /// Replaces the workspace this store indexes: its members, and their
    /// exports and links (addresses absolute), each with its member's name.
    pub fn replace_workspace(
        &mut self,
        members: &[StoredMember],
        exports: &[(String, Export)],
        links: &[ExportLink],
    ) -> Result<(), StoreError> {
        let mut catalog = self.catalog()?;
        let mut members: Vec<Member> = members.iter().map(member_of).collect();
        members.sort_by(|left, right| left.name.cmp(&right.name));
        catalog.members = members;
        catalog.workspace_exports = unique_exports(exports.iter().cloned());
        catalog.workspace_links = unique_links(links);
        self.write_catalog(&catalog)
    }

    /// The workspace's members, by name; empty for a repository's index.
    pub fn workspace_members(&self) -> Result<Vec<StoredMember>, StoreError> {
        let catalog = self.catalog()?;
        Ok(catalog.members.iter().map(stored_member).collect())
    }

    /// Every member's exports, each with its member's name.
    pub fn workspace_exports(&self) -> Result<Vec<(String, Export)>, StoreError> {
        Ok(self.catalog()?.workspace_exports)
    }

    /// Every member's links, with absolute addresses, sorted.
    pub fn workspace_links(&self) -> Result<Vec<ExportLink>, StoreError> {
        Ok(self.catalog()?.workspace_links)
    }

    /// Every snapshot, by id.
    pub fn snapshots(&self) -> Result<Vec<StoredSnapshot>, StoreError> {
        let catalog = self.catalog()?;
        let mut snapshots: Vec<StoredSnapshot> =
            catalog.snapshots.iter().map(stored_snapshot).collect();
        snapshots.sort_by_key(|snapshot| snapshot.id);
        Ok(snapshots)
    }

    /// Forgets a snapshot and everything derived from it.
    pub fn delete_snapshot(&mut self, snapshot_id: i64) -> Result<(), StoreError> {
        let mut catalog = self.catalog()?;
        catalog.snapshots.retain(|entry| entry.id != snapshot_id);
        let attributes = [(catalog_key(), encode_catalog(&catalog)?)];
        let removed = [root_name(snapshot_id)];
        let opened = self.opened()?;
        let removed_roots: &[String] = match opened.index.root(&removed[0]) {
            Ok(_) => &removed,
            Err(_) => &[],
        };
        self.write(&Write {
            commit: None,
            removed_roots,
            attributes: &attributes,
        })
    }

    /// The most recently stored snapshot.
    pub fn latest_snapshot(&self) -> Result<Option<StoredSnapshot>, StoreError> {
        Ok(self.snapshots()?.pop())
    }

    /// Every symbol in a snapshot, in the compact form used for searching
    /// in memory.
    pub fn symbol_keys(&self, snapshot_id: i64) -> Result<Vec<SymbolKey>, StoreError> {
        let mut keys = Vec::new();
        for (file_id, (path, record, ids)) in
            self.files_with_ids(snapshot_id)?.into_iter().enumerate()
        {
            let _ = path;
            for (symbol, id) in record.symbols.iter().zip(ids) {
                keys.push(SymbolKey {
                    id,
                    file_id: i64::try_from(file_id).unwrap_or(i64::MAX),
                    qualified_name: symbol.qualified_name.clone(),
                    name_length: symbol.name.len(),
                    kind: symbol.kind,
                    start_line: symbol.span.start.line,
                });
            }
        }
        Ok(keys)
    }

    /// Every file in a snapshot, as needed for searching its symbols.
    pub fn file_keys(&self, snapshot_id: i64) -> Result<Vec<FileKey>, StoreError> {
        let opened = self.opened()?;
        let Some(root) = root_of(&opened, snapshot_id)? else {
            return Ok(Vec::new());
        };
        Ok(opened
            .index
            .files(&root)?
            .into_iter()
            .enumerate()
            .map(|(id, (path, record))| FileKey {
                id: i64::try_from(id).unwrap_or(i64::MAX),
                path,
                language: record.language,
            })
            .collect())
    }

    /// Symbols by id, in the order given, skipping any not in the snapshot.
    pub fn symbols_by_ids(
        &self,
        snapshot_id: i64,
        ids: &[i64],
    ) -> Result<Vec<StoredSymbol>, StoreError> {
        let mut found = Vec::with_capacity(ids.len());
        for id in ids {
            found.extend(self.symbol(snapshot_id, *id)?);
        }
        Ok(found)
    }

    /// Writes answers saved so far. Dropping the store does too.
    pub fn flush(&mut self) -> Result<(), StoreError> {
        if self.pending.is_empty() {
            return Ok(());
        }
        let attributes: Vec<(Digest, Vec<u8>)> =
            std::mem::take(&mut self.pending).into_iter().collect();
        self.write(&Write {
            commit: None,
            removed_roots: &[],
            attributes: &attributes,
        })
    }

    /// Each file of a snapshot with its symbols' ids, sorted by path.
    fn files_with_ids(&self, snapshot_id: i64) -> Result<Vec<IdentifiedFile>, StoreError> {
        let opened = self.opened()?;
        let Some(root) = root_of(&opened, snapshot_id)? else {
            return Ok(Vec::new());
        };
        let mut files = Vec::new();
        for (path, record) in opened.index.files(&root)? {
            let ids = ids_of(&opened, &path, &record)?;
            files.push((path, record, ids));
        }
        Ok(files)
    }

    /// One file of a snapshot with its symbols' ids.
    fn file_with_ids(
        &self,
        snapshot_id: i64,
        path: &str,
    ) -> Result<Option<(FileRecord, Vec<i64>)>, StoreError> {
        let opened = self.opened()?;
        let Some(root) = root_of(&opened, snapshot_id)? else {
            return Ok(None);
        };
        let Some(record) = opened.index.file(&root, path)? else {
            return Ok(None);
        };
        let ids = ids_of(&opened, path, &record)?;
        Ok(Some((record, ids)))
    }

    fn generation(&self, snapshot_id: i64) -> Result<u64, StoreError> {
        let catalog = self.catalog()?;
        Ok(entry(&catalog, snapshot_id).map_or(0, |entry| entry.generation))
    }

    fn answer(&self, key: &AnswerKey<'_>) -> Result<Option<Vec<u8>>, StoreError> {
        let digest = answer_key(self.generation(key.snapshot_id)?, key);
        if let Some(bytes) = self.pending.get(&digest) {
            return Ok(Some(bytes.clone()));
        }
        lookup(&*self.opened()?, &digest)
    }

    fn save_answer(&mut self, key: &AnswerKey<'_>, bytes: Vec<u8>) -> Result<(), StoreError> {
        let digest = answer_key(self.generation(key.snapshot_id)?, key);
        self.pending.insert(digest, bytes);
        if self.pending.len() >= PENDING_ANSWERS {
            self.flush()?;
        }
        Ok(())
    }
}

impl Drop for Store {
    fn drop(&mut self) {
        // Answers are a cache: one that cannot be written is asked again.
        let _ = self.flush();
        if self.is_temporary {
            let _ = std::fs::remove_file(&self.path);
            let mut lock = self.path.as_os_str().to_owned();
            lock.push(".lock");
            let _ = std::fs::remove_file(PathBuf::from(lock));
        }
    }
}

/// The id of a file's symbol: the one it had, or the next.
fn id_for(
    opened: &Opened,
    catalog: &mut Catalog,
    written: &mut FileChanges,
    path: &str,
    symbol: &crate::SymbolRecord,
) -> Result<i64, StoreError> {
    let identity = identity_key(
        path,
        &symbol.qualified_name,
        symbol.kind,
        symbol.discriminator,
    );
    if let Some(id) = written.identities.get(&identity) {
        return decode_id(id);
    }
    if let Some(bytes) = lookup(opened, &identity)? {
        return decode_id(&bytes);
    }
    catalog.next_symbol += 1;
    let id = catalog.next_symbol;
    written
        .identities
        .insert(identity, id.to_le_bytes().to_vec());
    let symbol_identity = encode_symbol_identity(
        path,
        symbol.discriminator,
        &symbol.qualified_name,
        symbol.kind,
    );
    written.attributes.push((symbol_key(id), symbol_identity));
    Ok(id)
}

/// Files written by one store call: the tree's changes, the attributes
/// beside them, and identities given ids by this call.
#[derive(Default)]
struct FileChanges {
    changes: Vec<Change>,
    attributes: Vec<(Digest, Vec<u8>)>,
    identities: BTreeMap<Digest, Vec<u8>>,
}

impl FileChanges {
    /// The tree's changes, and every attribute to write, the catalog last.
    fn split(self, catalog: &Catalog) -> Result<(Vec<Change>, Attributes), StoreError> {
        let mut attributes = self.attributes;
        attributes.extend(self.identities);
        attributes.push((catalog_key(), encode_catalog(catalog)?));
        Ok((self.changes, attributes))
    }
}

fn open_current(path: &Path) -> Result<Opened, StoreError> {
    let index = Index::open(path, ReadMode::Mapped)?;
    let length = index.length();
    let runs = index.runs()?;
    let catalog = match index.attribute_in(&runs, &catalog_key())? {
        Some(bytes) => serde_json::from_slice(&bytes).map_err(|_| StoreError::Damaged {
            what: "the catalog is not readable",
        })?,
        None => Catalog::default(),
    };
    Ok(Opened {
        index,
        runs,
        length,
        catalog,
    })
}

fn lookup(opened: &Opened, key: &Digest) -> Result<Option<Vec<u8>>, StoreError> {
    opened.index.attribute_in(&opened.runs, key)
}

fn root_of(opened: &Opened, snapshot_id: i64) -> Result<Option<Root>, StoreError> {
    if entry(&opened.catalog, snapshot_id).is_none() {
        return Ok(None);
    }
    match opened.index.root(&root_name(snapshot_id)) {
        Ok(root) => Ok(Some(root)),
        Err(StoreError::UnknownRoot { .. }) => Ok(None),
        Err(error) => Err(error),
    }
}

fn ids_of(opened: &Opened, path: &str, record: &FileRecord) -> Result<Vec<i64>, StoreError> {
    let digest = codec::file_digest(record);
    let Some(bytes) = lookup(opened, &ids_key(path, &digest))? else {
        return Err(StoreError::Damaged {
            what: "a file's symbol ids are missing",
        });
    };
    let mut decoder = Decoder::new(&bytes);
    let count = decoder.count(8)?;
    (0..count).map(|_| decoder.i64()).collect()
}

fn entry(catalog: &Catalog, snapshot_id: i64) -> Option<&Entry> {
    catalog
        .snapshots
        .iter()
        .find(|entry| entry.id == snapshot_id)
}

fn sorted(paths: &[String]) -> Vec<String> {
    let set: BTreeSet<&String> = paths.iter().collect();
    set.into_iter().cloned().collect()
}

fn source_of(snapshot: &Snapshot, extractor_version: &str) -> SourceRecord {
    SourceRecord {
        repository: snapshot.root.clone(),
        revision: snapshot.revision.clone().unwrap_or_default(),
        profile: *blake3::hash(extractor_version.as_bytes()).as_bytes(),
    }
}

fn stored_snapshot(entry: &Entry) -> StoredSnapshot {
    StoredSnapshot {
        id: entry.id,
        extractor_version: entry.extractor_version.clone(),
        indexed_at_ns: entry.indexed_at_ns,
        snapshot: entry.snapshot.clone(),
    }
}

fn member_of(member: &StoredMember) -> Member {
    Member {
        name: member.name.clone(),
        root: member.root.clone(),
        revision: member.revision.clone(),
        indexed_root: member.indexed_root.clone(),
        database: member.database.clone(),
    }
}

fn stored_member(member: &Member) -> StoredMember {
    StoredMember {
        name: member.name.clone(),
        root: member.root.clone(),
        revision: member.revision.clone(),
        indexed_root: member.indexed_root.clone(),
        database: member.database.clone(),
    }
}

/// Exports keyed by owner, service, name and kind, the last of each kept.
fn unique_exports(exports: impl Iterator<Item = (String, Export)>) -> Vec<(String, Export)> {
    let mut unique = BTreeMap::new();
    for (owner, export) in exports {
        let key = (
            owner.clone(),
            export.service.clone(),
            export.name.clone(),
            export.kind.as_str(),
        );
        unique.insert(key, (owner, export));
    }
    unique.into_values().collect()
}

fn unique_links(links: &[ExportLink]) -> Vec<ExportLink> {
    let set: BTreeSet<&ExportLink> = links.iter().collect();
    set.into_iter().cloned().collect()
}

fn encode_catalog(catalog: &Catalog) -> Result<Vec<u8>, StoreError> {
    serde_json::to_vec(catalog).map_err(|_| StoreError::Damaged {
        what: "the catalog cannot be written",
    })
}

fn encode_ids(ids: &[i64]) -> Vec<u8> {
    let mut encoder = Encoder::default();
    encoder.count(ids.len());
    for id in ids {
        encoder.i64(*id);
    }
    encoder.bytes
}

fn decode_id(bytes: &[u8]) -> Result<i64, StoreError> {
    Decoder::new(bytes).i64()
}

fn encode_symbol_identity(
    path: &str,
    discriminator: u32,
    qualified_name: &str,
    kind: SymbolKind,
) -> Vec<u8> {
    let mut encoder = Encoder::default();
    encoder.text(path);
    encoder.text(qualified_name);
    encoder.u8(codec::kind_code(kind));
    encoder.u32(discriminator);
    encoder.bytes
}

fn encode_stamp(stamp: FileStamp) -> Vec<u8> {
    let mut encoder = Encoder::default();
    encoder.u64(stamp.size_bytes);
    encoder.i64(stamp.modified_ns);
    encoder.i64(stamp.changed_ns);
    encoder.u64(stamp.inode);
    encoder.bytes
}

fn decode_stamp(bytes: &[u8]) -> Result<FileStamp, StoreError> {
    let mut decoder = Decoder::new(bytes);
    Ok(FileStamp {
        size_bytes: decoder.u64()?,
        modified_ns: decoder.i64()?,
        changed_ns: decoder.i64()?,
        inode: decoder.u64()?,
    })
}

fn stored_symbol(path: &str, record: &FileRecord, position: usize, id: i64) -> StoredSymbol {
    let symbol = &record.symbols[position];
    StoredSymbol {
        id,
        path: path.to_owned(),
        language: record.language,
        symbol: lemma_model::Symbol {
            name: symbol.name.clone(),
            qualified_name: symbol.qualified_name.clone(),
            kind: symbol.kind,
            span: symbol.span,
            name_span: symbol.name_span,
        },
    }
}

fn matches(
    filter: &SymbolFilter,
    path: &str,
    record: &FileRecord,
    symbol: &crate::SymbolRecord,
) -> bool {
    let named = match filter.name_match {
        NameMatch::Exact => symbol.name == filter.text || symbol.qualified_name == filter.text,
        NameMatch::Contains => symbol
            .qualified_name
            .to_ascii_lowercase()
            .contains(&filter.text.to_ascii_lowercase()),
    };
    named
        && filter.kind.is_none_or(|kind| kind == symbol.kind)
        && filter
            .language
            .is_none_or(|language| language == record.language)
        && filter
            .path_contains
            .as_deref()
            .is_none_or(|text| path.contains(text))
}

/// The symbols of one file a filter matches, each marked by whether its
/// name matches exactly.
fn matching_symbols(
    filter: &SymbolFilter,
    path: &str,
    record: &FileRecord,
    ids: &[i64],
) -> Vec<(bool, StoredSymbol)> {
    record
        .symbols
        .iter()
        .enumerate()
        .filter(|(_, symbol)| matches(filter, path, record, symbol))
        .map(|(position, symbol)| {
            let is_exact = symbol.name == filter.text || symbol.qualified_name == filter.text;
            (
                is_exact,
                stored_symbol(path, record, position, ids[position]),
            )
        })
        .collect()
}

const FOUND: u8 = 0;
const EMPTY: u8 = 1;
const UNSUPPORTED: u8 = 2;

fn encode_answer<T>(
    provider_version: Option<&str>,
    answer: &ProviderAnswer<T>,
    write: impl Fn(&mut Encoder, &T),
) -> Vec<u8> {
    let mut encoder = Encoder::default();
    encoder.u8(u8::from(provider_version.is_some()));
    encoder.text(provider_version.unwrap_or_default());
    match answer {
        ProviderAnswer::Found(value) => {
            encoder.u8(FOUND);
            write(&mut encoder, value);
        }
        ProviderAnswer::Empty => encoder.u8(EMPTY),
        ProviderAnswer::Unsupported => encoder.u8(UNSUPPORTED),
    }
    encoder.bytes
}

fn decode_answer<T>(
    bytes: &[u8],
    read: impl Fn(&mut Decoder<'_>) -> Result<T, StoreError>,
) -> Result<StoredAnswer<T>, StoreError> {
    let mut decoder = Decoder::new(bytes);
    let has_version = decoder.u8()? != 0;
    let version = decoder.text()?.to_owned();
    let answer = match decoder.u8()? {
        FOUND => ProviderAnswer::Found(read(&mut decoder)?),
        EMPTY => ProviderAnswer::Empty,
        _ => ProviderAnswer::Unsupported,
    };
    Ok(StoredAnswer {
        provider_version: has_version.then_some(version),
        answer,
    })
}

fn write_locations(encoder: &mut Encoder, locations: &[Location]) {
    let mut sorted: Vec<&Location> = locations.iter().collect();
    sorted.sort_by(|left, right| {
        (&left.path, left.span.start.line, left.span.start.column).cmp(&(
            &right.path,
            right.span.start.line,
            right.span.start.column,
        ))
    });
    encoder.count(sorted.len());
    for location in sorted {
        encoder.text(&location.path);
        encoder.u8(u8::from(location.is_external));
        encoder.span(location.span);
    }
}

fn read_locations(decoder: &mut Decoder<'_>) -> Result<Vec<Location>, StoreError> {
    let count = decoder.count(21)?;
    (0..count)
        .map(|_| {
            let path = decoder.text()?.to_owned();
            let is_external = decoder.u8()? != 0;
            let span = decoder.span()?;
            Ok(Location {
                path,
                is_external,
                span,
            })
        })
        .collect()
}

impl FactStore for Store {
    fn files(&self, snapshot_id: i64) -> Result<Vec<StoredFile>, StoreError> {
        let opened = self.opened()?;
        let Some(root) = root_of(&opened, snapshot_id)? else {
            return Ok(Vec::new());
        };
        let mut files = Vec::new();
        for (path, record) in opened.index.files(&root)? {
            let stamp = match lookup(&opened, &stamp_key(snapshot_id, &path))? {
                Some(bytes) => decode_stamp(&bytes)?,
                None => FileStamp::default(),
            };
            files.push(StoredFile {
                content_hash: hex(&record.blob),
                language: record.language,
                has_syntax_errors: record.has_syntax_errors,
                symbol_count: record.symbols.len(),
                stamp,
                path,
            });
        }
        Ok(files)
    }

    fn find_symbols(
        &self,
        snapshot_id: i64,
        filter: &SymbolFilter,
    ) -> Result<Vec<StoredSymbol>, StoreError> {
        let mut found: Vec<(bool, StoredSymbol)> = Vec::new();
        for (path, record, ids) in self.files_with_ids(snapshot_id)? {
            found.extend(matching_symbols(filter, &path, &record, &ids));
        }
        found.sort_by(|(left_exact, left), (right_exact, right)| {
            right_exact
                .cmp(left_exact)
                .then(
                    left.symbol
                        .qualified_name
                        .len()
                        .cmp(&right.symbol.qualified_name.len()),
                )
                .then(left.path.cmp(&right.path))
                .then(
                    left.symbol
                        .span
                        .start
                        .line
                        .cmp(&right.symbol.span.start.line),
                )
        });
        found.truncate(filter.limit as usize);
        Ok(found.into_iter().map(|(_, symbol)| symbol).collect())
    }

    fn symbol(&self, snapshot_id: i64, symbol_id: i64) -> Result<Option<StoredSymbol>, StoreError> {
        let Some(bytes) = lookup(&*self.opened()?, &symbol_key(symbol_id))? else {
            return Ok(None);
        };
        let mut decoder = Decoder::new(&bytes);
        let path = decoder.text()?.to_owned();
        let Some((record, ids)) = self.file_with_ids(snapshot_id, &path)? else {
            return Ok(None);
        };
        Ok(ids
            .iter()
            .position(|id| *id == symbol_id)
            .map(|position| stored_symbol(&path, &record, position, symbol_id)))
    }

    fn symbols_in_file(
        &self,
        snapshot_id: i64,
        path: &str,
    ) -> Result<Vec<StoredSymbol>, StoreError> {
        let Some((record, ids)) = self.file_with_ids(snapshot_id, path)? else {
            return Ok(Vec::new());
        };
        let mut symbols: Vec<StoredSymbol> = (0..record.symbols.len())
            .map(|position| stored_symbol(path, &record, position, ids[position]))
            .collect();
        symbols.sort_by_key(|symbol| {
            (
                symbol.symbol.span.start.line,
                symbol.symbol.span.start.column,
            )
        });
        Ok(symbols)
    }

    fn symbol_named_at(
        &self,
        snapshot_id: i64,
        path: &str,
        position: Position,
    ) -> Result<Option<StoredSymbol>, StoreError> {
        let at = (position.line, position.column);
        Ok(self
            .symbols_in_file(snapshot_id, path)?
            .into_iter()
            .find(|symbol| {
                let name = symbol.symbol.name_span;
                (name.start.line, name.start.column) <= at && at < (name.end.line, name.end.column)
            }))
    }

    fn file_hash(&self, snapshot_id: i64, path: &str) -> Result<Option<String>, StoreError> {
        Ok(self
            .file_with_ids(snapshot_id, path)?
            .map(|(record, _)| hex(&record.blob)))
    }

    fn enclosing_symbol(
        &self,
        snapshot_id: i64,
        path: &str,
        position: Position,
    ) -> Result<Option<String>, StoreError> {
        let at = (position.line, position.column);
        let symbols = self.symbols_in_file(snapshot_id, path)?;
        Ok(symbols
            .into_iter()
            .filter(|symbol| {
                let span = symbol.symbol.span;
                (span.start.line, span.start.column) <= at && at <= (span.end.line, span.end.column)
            })
            .max_by_key(|symbol| {
                (
                    symbol.symbol.span.start.line,
                    symbol.symbol.span.start.column,
                )
            })
            .map(|symbol| symbol.symbol.qualified_name))
    }

    fn cached_locations(
        &self,
        key: &AnswerKey<'_>,
    ) -> Result<Option<StoredAnswer<Vec<Location>>>, StoreError> {
        let Some(bytes) = self.answer(key)? else {
            return Ok(None);
        };
        decode_answer(&bytes, read_locations).map(Some)
    }

    fn save_locations(
        &mut self,
        key: &AnswerKey<'_>,
        provider_version: Option<&str>,
        answer: &ProviderAnswer<Vec<Location>>,
    ) -> Result<(), StoreError> {
        let bytes = encode_answer(
            provider_version,
            answer,
            |encoder, locations: &Vec<Location>| {
                write_locations(encoder, locations);
            },
        );
        self.save_answer(key, bytes)
    }

    fn cached_text(&self, key: &AnswerKey<'_>) -> Result<Option<StoredAnswer<String>>, StoreError> {
        let Some(bytes) = self.answer(key)? else {
            return Ok(None);
        };
        decode_answer(&bytes, |decoder| decoder.text().map(str::to_owned)).map(Some)
    }

    fn save_text(
        &mut self,
        key: &AnswerKey<'_>,
        provider_version: Option<&str>,
        answer: &ProviderAnswer<String>,
    ) -> Result<(), StoreError> {
        let bytes = encode_answer(provider_version, answer, |encoder, text: &String| {
            encoder.text(text);
        });
        self.save_answer(key, bytes)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::facts::Question;
    use lemma_model::{ExportKind, FoundBy, IndexedFile, Language, LocationQuery, Span, Symbol};

    /// A file's record as indexing would give it, with a digest standing in
    /// for its contents.
    fn record_of(file: &IndexedFile) -> FileRecord {
        let mut seen: BTreeMap<(String, SymbolKind), u32> = BTreeMap::new();
        let symbols = file
            .symbols
            .iter()
            .map(|symbol| {
                let count = seen
                    .entry((symbol.qualified_name.clone(), symbol.kind))
                    .or_insert(0);
                let discriminator = *count;
                *count += 1;
                crate::SymbolRecord {
                    name: symbol.name.clone(),
                    qualified_name: symbol.qualified_name.clone(),
                    kind: symbol.kind,
                    span: symbol.span,
                    name_span: symbol.name_span,
                    discriminator,
                    digest: *blake3::hash(symbol.qualified_name.as_bytes()).as_bytes(),
                    mentions: Vec::new(),
                }
            })
            .collect();
        FileRecord {
            blob: *blake3::hash(file.content_hash.as_bytes()).as_bytes(),
            language: file.language,
            has_syntax_errors: file.has_syntax_errors,
            symbols,
        }
    }

    fn span(line: u32, start_column: u32, end_line: u32, end_column: u32) -> Span {
        Span {
            start: Position {
                line,
                column: start_column,
            },
            end: Position {
                line: end_line,
                column: end_column,
            },
        }
    }

    fn symbol(name: &str, qualified_name: &str, kind: SymbolKind, span: Span) -> Symbol {
        Symbol {
            name: name.to_owned(),
            qualified_name: qualified_name.to_owned(),
            kind,
            span,
            name_span: span,
        }
    }

    fn snapshot(root: &str, fingerprint: &str) -> Snapshot {
        Snapshot {
            root: root.to_owned(),
            revision: Some("abc123".to_owned()),
            is_dirty: false,
            fingerprint: fingerprint.to_owned(),
        }
    }

    fn stamped(file: &IndexedFile) -> StampedFile {
        StampedFile {
            path: file.path.clone(),
            record: record_of(file),
            stamp: FileStamp {
                size_bytes: 10,
                modified_ns: 1,
                changed_ns: 1,
                inode: 7,
            },
        }
    }

    fn save(store: &mut Store, snapshot: &Snapshot, files: &[StampedFile]) -> i64 {
        store
            .replace_snapshot(&NewSnapshot {
                snapshot,
                extractor_version: "1",
                indexed_at_ns: 100,
                files,
                uncommitted_paths: &[],
            })
            .unwrap()
    }

    fn sample_files() -> Vec<StampedFile> {
        vec![stamped(&IndexedFile {
            path: "src/router.py".to_owned(),
            language: Language::Python,
            content_hash: "hash-1".to_owned(),
            has_syntax_errors: false,
            symbols: vec![
                symbol("Router", "Router", SymbolKind::Class, span(0, 0, 10, 0)),
                symbol(
                    "delete",
                    "Router.delete",
                    SymbolKind::Method,
                    span(2, 4, 5, 0),
                ),
                symbol(
                    "delete_all",
                    "delete_all",
                    SymbolKind::Function,
                    span(12, 0, 14, 0),
                ),
            ],
        })]
    }

    fn store_with_sample() -> (Store, i64) {
        let mut store = Store::open_in_memory().unwrap();
        let snapshot_id = save(&mut store, &snapshot("/repo", "fp-1"), &sample_files());
        (store, snapshot_id)
    }

    fn other_file(path: &str, name: &str) -> StampedFile {
        stamped(&IndexedFile {
            path: path.to_owned(),
            language: Language::Python,
            content_hash: format!("hash-{name}"),
            has_syntax_errors: false,
            symbols: vec![symbol(name, name, SymbolKind::Function, span(0, 0, 1, 0))],
        })
    }

    fn update<'a>(
        snapshot: &'a Snapshot,
        changed: &'a [StampedFile],
        removed: &'a [String],
        restamped: &'a [(String, FileStamp)],
    ) -> SnapshotUpdate<'a> {
        SnapshotUpdate {
            snapshot,
            indexed_at_ns: 200,
            uncommitted_paths: &[],
            changed,
            removed,
            restamped,
        }
    }

    fn key(snapshot_id: i64, question: Question) -> AnswerKey<'static> {
        AnswerKey {
            snapshot_id,
            question,
            path: "src/router.py",
            position: Position { line: 2, column: 8 },
            provider_id: "pyright",
            config_fingerprint: "cfg",
        }
    }

    fn references_key(snapshot_id: i64) -> AnswerKey<'static> {
        key(snapshot_id, Question::Locations(LocationQuery::References))
    }

    fn location(path: &str, line: u32) -> Location {
        Location {
            path: path.to_owned(),
            span: span(line, 2, line, 8),
            is_external: false,
        }
    }

    fn cache_empty_answer(store: &mut Store, snapshot_id: i64) -> AnswerKey<'static> {
        let key = references_key(snapshot_id);
        store
            .save_locations(&key, None, &ProviderAnswer::Empty)
            .unwrap();
        key
    }

    fn symbol_id(store: &Store, snapshot_id: i64, name: &str) -> i64 {
        store
            .find_symbols(snapshot_id, &filter(name, NameMatch::Exact))
            .unwrap()[0]
            .id
    }

    fn filter(text: &str, name_match: NameMatch) -> SymbolFilter {
        SymbolFilter {
            text: text.to_owned(),
            name_match,
            kind: None,
            language: None,
            path_contains: None,
            limit: 50,
        }
    }

    fn qualified_names(symbols: &[StoredSymbol]) -> Vec<&str> {
        symbols
            .iter()
            .map(|stored| stored.symbol.qualified_name.as_str())
            .collect()
    }

    #[test]
    fn snapshot_round_trips() {
        let (store, snapshot_id) = store_with_sample();

        let latest = store.latest_snapshot().unwrap().unwrap();
        assert_eq!(latest.id, snapshot_id);
        assert_eq!(latest.snapshot, snapshot("/repo", "fp-1"));
    }

    #[test]
    fn replacing_a_snapshot_removes_the_previous_one() {
        let (mut store, first_id) = store_with_sample();

        let second_id = save(&mut store, &snapshot("/repo", "fp-2"), &sample_files());

        assert_ne!(first_id, second_id);
        assert_eq!(
            store
                .find_symbols(first_id, &filter("delete", NameMatch::Contains))
                .unwrap(),
            [] as [StoredSymbol; 0]
        );
    }

    #[test]
    fn replacing_a_snapshot_keeps_the_ids_of_symbols_still_there() {
        let (mut store, first_id) = store_with_sample();
        let router_before = symbol_id(&store, first_id, "Router");

        let second_id = save(&mut store, &snapshot("/repo", "fp-2"), &sample_files());

        assert_eq!(symbol_id(&store, second_id, "Router"), router_before);
    }

    #[test]
    fn files_list_stamps_and_symbol_counts() {
        let (store, snapshot_id) = store_with_sample();

        let files = store.files(snapshot_id).unwrap();

        assert_eq!(files.len(), 1);
        assert_eq!(files[0].path, "src/router.py");
        assert_eq!(files[0].symbol_count, 3);
        assert_eq!(files[0].stamp, sample_files()[0].stamp);
    }

    #[test]
    fn updating_rewrites_changed_files_and_keeps_other_symbol_ids() {
        let mut store = Store::open_in_memory().unwrap();
        let mut files = sample_files();
        files.push(other_file("src/old.py", "old_helper"));
        let snapshot_id = save(&mut store, &snapshot("/repo", "fp-1"), &files);
        let router_id = symbol_id(&store, snapshot_id, "Router");

        let changed = [other_file("src/new.py", "new_helper")];
        let removed = ["src/old.py".to_owned()];
        let next = snapshot("/repo", "fp-2");
        store
            .update_snapshot(snapshot_id, &update(&next, &changed, &removed, &[]))
            .unwrap();

        let paths: Vec<_> = store
            .files(snapshot_id)
            .unwrap()
            .into_iter()
            .map(|file| file.path)
            .collect();
        assert_eq!(paths, ["src/new.py", "src/router.py"]);
        assert_eq!(symbol_id(&store, snapshot_id, "Router"), router_id);
        assert_eq!(store.latest_snapshot().unwrap().unwrap().snapshot, next);
    }

    #[test]
    fn changing_contents_drops_cached_answers() {
        let (mut store, snapshot_id) = store_with_sample();
        let key = cache_empty_answer(&mut store, snapshot_id);

        let changed = [other_file("src/new.py", "new_helper")];
        let next = snapshot("/repo", "fp-2");
        store
            .update_snapshot(snapshot_id, &update(&next, &changed, &[], &[]))
            .unwrap();

        assert_eq!(store.cached_locations(&key).unwrap(), None);
    }

    #[test]
    fn restamping_keeps_cached_answers() {
        let (mut store, snapshot_id) = store_with_sample();
        let key = cache_empty_answer(&mut store, snapshot_id);
        let stamp = FileStamp {
            modified_ns: 50,
            ..sample_files()[0].stamp
        };

        let restamped = [("src/router.py".to_owned(), stamp)];
        let same = snapshot("/repo", "fp-1");
        store
            .update_snapshot(snapshot_id, &update(&same, &[], &[], &restamped))
            .unwrap();

        assert!(store.cached_locations(&key).unwrap().is_some());
        assert_eq!(store.files(snapshot_id).unwrap()[0].stamp, stamp);
    }

    #[test]
    fn uncommitted_paths_are_replaced_on_update() {
        let (mut store, snapshot_id) = store_with_sample();
        let same = snapshot("/repo", "fp-1");
        let uncommitted = ["b.py".to_owned(), "a.py".to_owned()];

        store
            .update_snapshot(
                snapshot_id,
                &SnapshotUpdate {
                    uncommitted_paths: &uncommitted,
                    ..update(&same, &[], &[], &[])
                },
            )
            .unwrap();

        assert_eq!(
            store.uncommitted_paths(snapshot_id).unwrap(),
            ["a.py", "b.py"]
        );
    }

    #[test]
    fn snapshot_for_root_ignores_other_roots() {
        let (mut store, snapshot_id) = store_with_sample();
        save(&mut store, &snapshot("/other", "fp-9"), &[]);

        let found = store.snapshot_for_root("/repo").unwrap().unwrap();

        assert_eq!(found.id, snapshot_id);
        assert_eq!(found.indexed_at_ns, 100);
    }

    #[test]
    fn exact_search_matches_name_or_qualified_name() {
        let (store, snapshot_id) = store_with_sample();

        let by_name = store
            .find_symbols(snapshot_id, &filter("delete", NameMatch::Exact))
            .unwrap();
        let by_qualified = store
            .find_symbols(snapshot_id, &filter("Router.delete", NameMatch::Exact))
            .unwrap();

        assert_eq!(qualified_names(&by_name), ["Router.delete"]);
        assert_eq!(qualified_names(&by_qualified), ["Router.delete"]);
    }

    #[test]
    fn contains_search_ranks_exact_names_first() {
        let (store, snapshot_id) = store_with_sample();

        let found = store
            .find_symbols(snapshot_id, &filter("delete", NameMatch::Contains))
            .unwrap();

        assert_eq!(qualified_names(&found), ["Router.delete", "delete_all"]);
    }

    #[test]
    fn contains_search_treats_wildcards_literally() {
        let (store, snapshot_id) = store_with_sample();

        let found = store
            .find_symbols(snapshot_id, &filter("e_a", NameMatch::Contains))
            .unwrap();
        let none = store
            .find_symbols(snapshot_id, &filter("%", NameMatch::Contains))
            .unwrap();

        assert_eq!(qualified_names(&found), ["delete_all"]);
        assert_eq!(none, [] as [StoredSymbol; 0]);
    }

    #[test]
    fn kind_filter_narrows_results() {
        let (store, snapshot_id) = store_with_sample();
        let mut only_functions = filter("delete", NameMatch::Contains);
        only_functions.kind = Some(SymbolKind::Function);

        let found = store.find_symbols(snapshot_id, &only_functions).unwrap();

        assert_eq!(qualified_names(&found), ["delete_all"]);
    }

    #[test]
    fn exports_and_links_are_replaced_whole() {
        let (mut store, snapshot_id) = store_with_sample();
        let export = Export {
            service: "api".to_owned(),
            name: "createCharge".to_owned(),
            kind: ExportKind::Endpoint,
            path: Some("src/router.py".to_owned()),
            symbol: Some("Router.delete".to_owned()),
            detail: Some("POST /v1/charges".to_owned()),
            found_by: Some(FoundBy::Code),
        };
        let link = ExportLink {
            caller: "services.api.createCharge".to_owned(),
            provider: "#billing:services.core.charge".to_owned(),
        };

        store
            .replace_exports(snapshot_id, "first", std::slice::from_ref(&export), &[link])
            .unwrap();
        store
            .replace_exports(snapshot_id, "second", std::slice::from_ref(&export), &[])
            .unwrap();

        assert_eq!(store.exports(snapshot_id).unwrap(), [export]);
        assert_eq!(store.export_links(snapshot_id).unwrap(), []);
        assert_eq!(
            store.exports_fingerprint(snapshot_id).unwrap().as_deref(),
            Some("second")
        );
    }

    #[test]
    fn enclosing_symbol_is_the_innermost_definition() {
        let (store, snapshot_id) = store_with_sample();

        let inside_method = Position { line: 3, column: 8 };
        let inside_class_only = Position { line: 7, column: 0 };
        let outside = Position {
            line: 11,
            column: 0,
        };

        let enclosing = |position| {
            store
                .enclosing_symbol(snapshot_id, "src/router.py", position)
                .unwrap()
        };
        assert_eq!(enclosing(inside_method).as_deref(), Some("Router.delete"));
        assert_eq!(enclosing(inside_class_only).as_deref(), Some("Router"));
        assert_eq!(enclosing(outside), None);
    }

    #[test]
    fn symbols_in_file_are_in_source_order() {
        let (store, snapshot_id) = store_with_sample();

        let found = store.symbols_in_file(snapshot_id, "src/router.py").unwrap();

        assert_eq!(
            qualified_names(&found),
            ["Router", "Router.delete", "delete_all"]
        );
    }

    #[test]
    fn symbol_named_at_matches_only_the_name() {
        let mut files = sample_files();
        files[0].record.symbols[0].name_span = span(0, 6, 0, 12);
        files[0].record.symbols[1].name_span = span(2, 8, 2, 14);
        let mut store = Store::open_in_memory().unwrap();
        let snapshot_id = save(&mut store, &snapshot("/repo", "fp-1"), &files);

        let named = |line, column| {
            store
                .symbol_named_at(snapshot_id, "src/router.py", Position { line, column })
                .unwrap()
                .map(|stored| stored.symbol.qualified_name)
        };

        assert_eq!(named(2, 10).as_deref(), Some("Router.delete"));
        assert_eq!(named(3, 10), None);
    }

    #[test]
    fn found_locations_round_trip_through_the_cache() {
        let (mut store, snapshot_id) = store_with_sample();
        let key = references_key(snapshot_id);
        let answer = ProviderAnswer::Found(vec![location("src/app.py", 4)]);

        store
            .save_locations(&key, Some("1.1.400"), &answer)
            .unwrap();
        let cached = store.cached_locations(&key).unwrap().unwrap();

        assert_eq!(cached.provider_version.as_deref(), Some("1.1.400"));
        assert_eq!(cached.answer, answer);
    }

    #[test]
    fn answers_to_different_questions_are_kept_apart() {
        let (mut store, snapshot_id) = store_with_sample();
        let references = references_key(snapshot_id);
        let definitions = key(snapshot_id, Question::Locations(LocationQuery::Definitions));

        store
            .save_locations(&references, None, &ProviderAnswer::Empty)
            .unwrap();

        assert!(store.cached_locations(&references).unwrap().is_some());
        assert_eq!(store.cached_locations(&definitions).unwrap(), None);
    }

    #[test]
    fn text_answers_round_trip_through_the_cache() {
        let (mut store, snapshot_id) = store_with_sample();
        let hover = key(snapshot_id, Question::Hover);
        let answer = ProviderAnswer::Found("def delete(self) -> None".to_owned());

        store.save_text(&hover, None, &answer).unwrap();

        assert_eq!(store.cached_text(&hover).unwrap().unwrap().answer, answer);
    }

    #[test]
    fn saving_again_replaces_earlier_locations() {
        let (mut store, snapshot_id) = store_with_sample();
        let key = references_key(snapshot_id);
        let first = ProviderAnswer::Found(vec![location("src/old.py", 1)]);

        store.save_locations(&key, None, &first).unwrap();
        store
            .save_locations(&key, None, &ProviderAnswer::Empty)
            .unwrap();

        let cached = store.cached_locations(&key).unwrap().unwrap();
        assert_eq!(cached.answer, ProviderAnswer::Empty);
    }
}
