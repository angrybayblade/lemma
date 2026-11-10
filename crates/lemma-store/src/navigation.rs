//! Navigation's view of a store (spec 17): snapshots of directories, with
//! their files' stamps, symbols, exports, workspace members and providers'
//! answers, all in one `index.ls`.
//!
//! Each snapshot is a root named `snapshot:<id>` whose segments hold its
//! files with their stamps; symbol ids come from each symbol's identity, so
//! nothing is stored per symbol beyond its row. What is not about files is
//! filed in the attribute table: a catalog of snapshots, exports and
//! members, and answers. A write is one commit, so a reader sees all of it
//! or none of it.

use std::collections::{BTreeMap, BTreeSet};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, RwLock};
use std::time::SystemTime;

use lemma_model::{Export, ExportLink, Location, Position, ProviderAnswer, Snapshot};
use rayon::prelude::*;
use serde::{Deserialize, Serialize};

use crate::attributes::Run;
use crate::codec::{Decoder, Encoder};
use crate::commit::{Edit, Edits, latest_by_path};
use crate::facts::{
    AnswerKey, FactStore, FileKey, NameMatch, NewSnapshot, SnapshotUpdate, StampedFile,
    StoredAnswer, StoredFile, StoredMember, StoredSnapshot, StoredSymbol, SymbolFilter, SymbolKey,
    hex,
};
use crate::ids::IdPrefix;
use crate::read::{Index, ReadMode, Root, Tail};
use crate::view::{At, SymbolAt, View};
use crate::{Commit, Digest, SourceRecord, StoreError, SymbolId, Write, Writer};

/// Candidates a thread ranks at a time: fewer are not worth a thread.
const PARALLEL_CANDIDATES: usize = 4096;

/// How many answers wait in memory before they are written.
const PENDING_ANSWERS: usize = 256;

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
    identity: FileIdentity,
    catalog: Catalog,
}

/// What tells one version of the file from another: appends change its
/// length, and compaction replaces it with a newer file.
type FileIdentity = (u64, Option<SystemTime>);

/// What the store holds beside its roots.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
struct Catalog {
    next_snapshot: i64,
    snapshots: Vec<Entry>,
    /// The repositories a workspace mounts: each is read from its own
    /// store when asked, never copied into this one.
    members: Vec<Member>,
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
        // Creating the file needs the writer; it is let go at once. An
        // existing store is only read, so a busy writer does not hold this up.
        if !path.exists() {
            drop(Writer::open_waiting(path)?);
        }
        let opened = open_current(path)?;
        Ok(Self {
            path: path.to_owned(),
            current: RwLock::new(Arc::new(opened)),
            pending: BTreeMap::new(),
            is_temporary,
        })
    }

    /// The store as it is now, reopened when another writer has changed it.
    fn opened(&self) -> Result<Arc<Opened>, StoreError> {
        let identity = identity_of(&self.path)?;
        if let Ok(current) = self.current.read()
            && current.identity == identity
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
        let mut writer = Writer::open_waiting_after(&self.path, self.known_tail())?;
        writer.write(write)?;
        self.adopt(writer)
    }

    fn write_edits(
        &mut self,
        commit: Option<(&Commit<'_>, &Edits<'_>)>,
        removed_roots: &[String],
        attributes: &[(Digest, Vec<u8>)],
    ) -> Result<(), StoreError> {
        let mut writer = Writer::open_waiting_after(&self.path, self.known_tail())?;
        writer.write_edits(commit, removed_roots, attributes)?;
        self.adopt(writer)
    }

    /// Where the store ended when this process last read it.
    fn known_tail(&self) -> Option<Tail> {
        self.current
            .read()
            .ok()
            .and_then(|current| current.index.tail())
    }

    /// Reads from now on from the store as `writer` left it. Its identity
    /// is taken while the writer still holds the lock, so no other writer
    /// can come between the two.
    fn adopt(&mut self, writer: Writer) -> Result<(), StoreError> {
        let identity = identity_of(&self.path)?;
        let opened = opened_with(identity, writer.into_index())?;
        if let Ok(mut current) = self.current.write() {
            *current = Arc::new(opened);
        }
        Ok(())
    }

    /// Stores a snapshot and its files, replacing earlier snapshots of the
    /// same root. A symbol keeps its id while its path, qualified name,
    /// kind and discriminator stay the same.
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
        let edits = latest_by_path(new.files.iter().map(put).collect());
        let name = root_name(id);
        let source = source_of(new.snapshot, new.extractor_version);
        let commit = Commit {
            root: &name,
            parent: None,
            source: &source,
        };
        let attributes = [(catalog_key(), encode_catalog(&catalog)?)];
        self.write_edits(Some((&commit, &edits)), &replaced, &attributes)?;
        Ok(id)
    }

    /// Updates a stored snapshot, writing only the files that changed.
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
        if !update.changed.is_empty() || !update.removed.is_empty() {
            entry.generation += 1;
        }
        let source = source_of(update.snapshot, &entry.extractor_version);
        let mut edits: Edits<'_> = update.changed.iter().map(put).collect();
        edits.extend(
            update
                .removed
                .iter()
                .map(|path| (path.as_str(), Edit::Remove)),
        );
        edits.extend(
            update
                .restamped
                .iter()
                .map(|(path, stamp)| (path.as_str(), Edit::Restamp { stamp: *stamp })),
        );
        let name = root_name(snapshot_id);
        let commit = Commit {
            root: &name,
            parent: Some(&name),
            source: &source,
        };
        let attributes = [(catalog_key(), encode_catalog(&catalog)?)];
        let edits = latest_by_path(edits);
        self.write_edits(Some((&commit, &edits)), &[], &attributes)
    }

    /// The stored snapshot of a directory, if it has been indexed.
    pub fn snapshot_for_root(&self, root: &str) -> Result<Option<StoredSnapshot>, StoreError> {
        let opened = self.opened()?;
        let catalog = &opened.catalog;
        Ok(catalog
            .snapshots
            .iter()
            .find(|entry| entry.snapshot.root == root)
            .map(stored_snapshot))
    }

    /// Source files that differed from the snapshot's revision when it was
    /// taken, sorted.
    pub fn uncommitted_paths(&self, snapshot_id: i64) -> Result<Vec<String>, StoreError> {
        let opened = self.opened()?;
        let catalog = &opened.catalog;
        Ok(entry(catalog, snapshot_id).map_or_else(Vec::new, |entry| entry.uncommitted.clone()))
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
        let opened = self.opened()?;
        let catalog = &opened.catalog;
        Ok(entry(catalog, snapshot_id)
            .and_then(|entry| entry.exports.as_ref())
            .map(|exports| exports.fingerprint.clone()))
    }

    /// A snapshot's exports, by service, name and kind.
    pub fn exports(&self, snapshot_id: i64) -> Result<Vec<Export>, StoreError> {
        let opened = self.opened()?;
        let catalog = &opened.catalog;
        Ok(entry(catalog, snapshot_id)
            .and_then(|entry| entry.exports.as_ref())
            .map_or_else(Vec::new, |exports| exports.list.clone()))
    }

    /// A snapshot's declared links, sorted.
    pub fn export_links(&self, snapshot_id: i64) -> Result<Vec<ExportLink>, StoreError> {
        let opened = self.opened()?;
        let catalog = &opened.catalog;
        Ok(entry(catalog, snapshot_id)
            .and_then(|entry| entry.exports.as_ref())
            .map_or_else(Vec::new, |exports| exports.links.clone()))
    }

    /// Mounts the repositories a workspace composes: records where each
    /// one's store is, and reads their exports and links from there.
    pub fn mount_workspace(&mut self, members: &[StoredMember]) -> Result<(), StoreError> {
        let mut catalog = self.catalog()?;
        let mut members: Vec<Member> = members.iter().map(member_of).collect();
        members.sort_by(|left, right| left.name.cmp(&right.name));
        catalog.members = members;
        self.write_catalog(&catalog)
    }

    /// The workspace's members, by name; empty for a repository's index.
    pub fn workspace_members(&self) -> Result<Vec<StoredMember>, StoreError> {
        let opened = self.opened()?;
        let catalog = &opened.catalog;
        Ok(catalog.members.iter().map(stored_member).collect())
    }

    /// Every member's exports, each with its member's name, read from the
    /// members' stores. A member not indexed yet has none.
    pub fn workspace_exports(&self) -> Result<Vec<(String, Export)>, StoreError> {
        let mut exports = Vec::new();
        for (member, store, snapshot_id) in self.mounted()? {
            for export in store.exports(snapshot_id)? {
                exports.push((member.name.clone(), export));
            }
        }
        Ok(unique_exports(exports.into_iter()))
    }

    /// Every member's links, with absolute addresses, sorted.
    pub fn workspace_links(&self) -> Result<Vec<ExportLink>, StoreError> {
        let mut links = Vec::new();
        for (member, store, snapshot_id) in self.mounted()? {
            for link in store.export_links(snapshot_id)? {
                links.push(ExportLink {
                    caller: absolute_address(&member.name, &link.caller),
                    provider: absolute_address(&member.name, &link.provider),
                });
            }
        }
        Ok(unique_links(&links))
    }

    /// Each member with its store and the snapshot of its indexed root.
    fn mounted(&self) -> Result<Vec<(Member, Store, i64)>, StoreError> {
        let mut mounted = Vec::new();
        let opened = self.opened()?;
        for member in opened.catalog.members.iter().cloned() {
            let path = Path::new(&member.database);
            if !path.exists() {
                continue;
            }
            let store = Store::open(path)?;
            if let Some(snapshot) = store.snapshot_for_root(&member.indexed_root)? {
                mounted.push((member, store, snapshot.id));
            }
        }
        Ok(mounted)
    }

    /// Every snapshot, by id.
    pub fn snapshots(&self) -> Result<Vec<StoredSnapshot>, StoreError> {
        let opened = self.opened()?;
        let catalog = &opened.catalog;
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
        let opened = self.opened()?;
        let Some(view) = view_of(&opened, snapshot_id)? else {
            return Ok(Vec::new());
        };
        let per_file: Vec<Vec<SymbolKey>> = view
            .files()?
            .into_par_iter()
            .enumerate()
            .map(|(file_id, at)| file_symbol_keys(&view, file_id, at))
            .collect::<Result<_, _>>()?;
        Ok(per_file.into_iter().flatten().collect())
    }

    /// Every file in a snapshot, as needed for searching its symbols.
    pub fn file_keys(&self, snapshot_id: i64) -> Result<Vec<FileKey>, StoreError> {
        let opened = self.opened()?;
        let Some(view) = view_of(&opened, snapshot_id)? else {
            return Ok(Vec::new());
        };
        view.files()?
            .into_iter()
            .enumerate()
            .map(|(id, at)| {
                Ok(FileKey {
                    id: i64::try_from(id).unwrap_or(i64::MAX),
                    path: view.path(at)?.to_owned(),
                    language: view.row(at)?.language,
                })
            })
            .collect()
    }

    /// Symbols by id, in the order given, skipping any not in the snapshot.
    pub fn symbols_by_ids(
        &self,
        snapshot_id: i64,
        ids: &[SymbolId],
    ) -> Result<Vec<StoredSymbol>, StoreError> {
        let opened = self.opened()?;
        let Some(view) = view_of(&opened, snapshot_id)? else {
            return Ok(Vec::new());
        };
        let mut found = Vec::with_capacity(ids.len());
        for id in ids {
            if let Some(at) = view.find_id(*id)? {
                found.push(stored_symbol(&view, at)?);
            }
        }
        Ok(found)
    }

    /// The symbols whose ids begin with `prefix`, sorted by path and line.
    pub fn symbols_with_id_prefix(
        &self,
        snapshot_id: i64,
        prefix: IdPrefix,
    ) -> Result<Vec<StoredSymbol>, StoreError> {
        let opened = self.opened()?;
        let Some(view) = view_of(&opened, snapshot_id)? else {
            return Ok(Vec::new());
        };
        let mut found = view
            .ids_with(prefix)?
            .into_iter()
            .map(|at| stored_symbol(&view, at))
            .collect::<Result<Vec<_>, _>>()?;
        found.sort_by(|left, right| {
            (&left.path, left.symbol.span.start.line)
                .cmp(&(&right.path, right.symbol.span.start.line))
        });
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

    fn generation(&self, snapshot_id: i64) -> Result<u64, StoreError> {
        let opened = self.opened()?;
        let catalog = &opened.catalog;
        Ok(entry(catalog, snapshot_id).map_or(0, |entry| entry.generation))
    }

    fn answer(&self, key: &AnswerKey<'_>) -> Result<Option<Vec<u8>>, StoreError> {
        let digest = answer_key(self.generation(key.snapshot_id)?, key);
        if let Some(bytes) = self.pending.get(&digest) {
            return Ok(Some(bytes.clone()));
        }
        let opened = self.opened()?;
        opened.index.attribute_in(&opened.runs, &digest)
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

fn put(file: &StampedFile) -> (&str, Edit<'_>) {
    let edit = Edit::Put {
        record: &file.record,
        stamp: file.stamp,
    };
    (file.path.as_str(), edit)
}

/// The search keys of one file's symbols; `file_id` is its position among
/// the snapshot's files.
fn file_symbol_keys(view: &View<'_>, file_id: usize, at: At) -> Result<Vec<SymbolKey>, StoreError> {
    let segment = view.segment(at.segment)?;
    let file_id = i64::try_from(file_id).unwrap_or(i64::MAX);
    view.row(at)?
        .symbols
        .map(|symbol| {
            let row = segment.symbol(symbol)?;
            Ok(SymbolKey {
                id: row.id,
                file_id,
                qualified_name: row.qualified_name.to_owned(),
                name_length: row.name.len(),
                kind: row.kind,
                start_line: row.span.start.line,
            })
        })
        .collect()
}

fn identity_of(path: &Path) -> Result<FileIdentity, StoreError> {
    let metadata = std::fs::metadata(path).map_err(|source| StoreError::Io {
        path: path.to_owned(),
        source,
    })?;
    Ok((metadata.len(), metadata.modified().ok()))
}

fn open_current(path: &Path) -> Result<Opened, StoreError> {
    let identity = identity_of(path)?;
    let index = Index::open(path, ReadMode::Mapped)?;
    opened_with(identity, index)
}

fn opened_with(identity: FileIdentity, index: Index) -> Result<Opened, StoreError> {
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
        identity,
        catalog,
    })
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

fn view_of(opened: &Opened, snapshot_id: i64) -> Result<Option<View<'_>>, StoreError> {
    let Some(root) = root_of(opened, snapshot_id)? else {
        return Ok(None);
    };
    opened.index.view(&root).map(Some)
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

/// An address as a workspace names it: one written inside `member`'s
/// description (`services.a.op`) gets the member's name.
fn absolute_address(member: &str, address: &str) -> String {
    if address.starts_with('#') {
        return address.to_owned();
    }
    format!("{member}:{address}")
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

fn stored_symbol(view: &View<'_>, at: SymbolAt) -> Result<StoredSymbol, StoreError> {
    let segment = view.segment(at.segment)?;
    let row = segment.symbol(at.symbol)?;
    Ok(StoredSymbol {
        id: row.id,
        path: segment.path(row.file)?.to_owned(),
        language: segment.file(row.file)?.language,
        symbol: lemma_model::Symbol {
            name: row.name.to_owned(),
            qualified_name: row.qualified_name.to_owned(),
            kind: row.kind,
            span: row.span,
            name_span: row.name_span,
        },
    })
}

/// A symbol a search found, with what ranks it: exact names first, then
/// shorter qualified names, then by path and line.
struct Ranked<'v> {
    is_exact: bool,
    qualified_length: usize,
    path: &'v str,
    line: u32,
    at: SymbolAt,
}

impl Ranked<'_> {
    fn order(&self) -> (bool, usize, &str, u32, SymbolAt) {
        (
            !self.is_exact,
            self.qualified_length,
            self.path,
            self.line,
            self.at,
        )
    }
}

/// The candidate at `at`, if it passes the filter's kind, language and path.
fn ranked<'v>(
    view: &'v View<'_>,
    filter: &SymbolFilter,
    at: SymbolAt,
) -> Result<Option<Ranked<'v>>, StoreError> {
    let segment = view.segment(at.segment)?;
    let row = segment.symbol(at.symbol)?;
    let path = segment.path(row.file)?;
    let language = segment.file(row.file)?.language;
    let passes = filter.kind.is_none_or(|kind| kind == row.kind)
        && filter.language.is_none_or(|wanted| wanted == language)
        && filter
            .path_contains
            .as_deref()
            .is_none_or(|text| path.contains(text));
    if !passes {
        return Ok(None);
    }
    Ok(Some(Ranked {
        is_exact: row.name == filter.text || row.qualified_name == filter.text,
        qualified_length: row.qualified_name.len(),
        path,
        line: row.span.start.line,
        at,
    }))
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
        let Some(view) = view_of(&opened, snapshot_id)? else {
            return Ok(Vec::new());
        };
        view.files()?
            .into_par_iter()
            .map(|at| {
                let row = view.row(at)?;
                Ok(StoredFile {
                    path: view.path(at)?.to_owned(),
                    language: row.language,
                    content_hash: hex(&row.blob),
                    has_syntax_errors: row.has_syntax_errors,
                    symbol_count: row.symbols.len(),
                    stamp: row.stamp,
                })
            })
            .collect()
    }

    fn find_symbols(
        &self,
        snapshot_id: i64,
        filter: &SymbolFilter,
    ) -> Result<Vec<StoredSymbol>, StoreError> {
        let opened = self.opened()?;
        let Some(view) = view_of(&opened, snapshot_id)? else {
            return Ok(Vec::new());
        };
        let candidates = match filter.name_match {
            NameMatch::Exact => view.exactly_named(&filter.text)?,
            NameMatch::Contains => view.containing(&filter.text)?,
        };
        let mut found: Vec<Ranked<'_>> = candidates
            .into_par_iter()
            .with_min_len(PARALLEL_CANDIDATES)
            .filter_map(|at| ranked(&view, filter, at).transpose())
            .collect::<Result<_, _>>()?;
        let limit = filter.limit as usize;
        // Only the first `limit` are shown: pick them, then sort just those.
        if found.len() > limit && limit > 0 {
            found.select_nth_unstable_by(limit - 1, |left, right| left.order().cmp(&right.order()));
        }
        found.truncate(limit);
        found.sort_unstable_by(|left, right| left.order().cmp(&right.order()));
        found
            .into_iter()
            .map(|ranked| stored_symbol(&view, ranked.at))
            .collect()
    }

    fn symbol(
        &self,
        snapshot_id: i64,
        symbol_id: SymbolId,
    ) -> Result<Option<StoredSymbol>, StoreError> {
        let opened = self.opened()?;
        let Some(view) = view_of(&opened, snapshot_id)? else {
            return Ok(None);
        };
        let Some(at) = view.find_id(symbol_id)? else {
            return Ok(None);
        };
        stored_symbol(&view, at).map(Some)
    }

    fn symbols_in_file(
        &self,
        snapshot_id: i64,
        path: &str,
    ) -> Result<Vec<StoredSymbol>, StoreError> {
        let opened = self.opened()?;
        let Some(view) = view_of(&opened, snapshot_id)? else {
            return Ok(Vec::new());
        };
        let mut symbols = view
            .symbols_in(path)?
            .into_iter()
            .map(|at| stored_symbol(&view, at))
            .collect::<Result<Vec<_>, _>>()?;
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
        let opened = self.opened()?;
        let Some(view) = view_of(&opened, snapshot_id)? else {
            return Ok(None);
        };
        let Some(at) = view.locate(path)? else {
            return Ok(None);
        };
        Ok(Some(hex(&view.row(at)?.blob)))
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
    use crate::FileRecord;
    use crate::facts::{FileStamp, Question};
    use lemma_model::{
        ExportKind, FoundBy, IndexedFile, Language, LocationQuery, Span, Symbol, SymbolKind,
    };

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

    fn symbol_id(store: &Store, snapshot_id: i64, name: &str) -> SymbolId {
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

    fn member(directory: &std::path::Path, name: &str) -> StoredMember {
        StoredMember {
            name: format!("#{name}"),
            root: format!("/{name}"),
            revision: None,
            indexed_root: format!("/{name}"),
            database: directory.join(format!("{name}.ls")).display().to_string(),
        }
    }

    fn export(service: &str, name: &str) -> Export {
        Export {
            service: service.to_owned(),
            name: name.to_owned(),
            kind: ExportKind::Operation,
            path: None,
            symbol: None,
            detail: None,
            found_by: None,
        }
    }

    #[test]
    fn a_workspace_reads_its_members_exports_from_their_stores() {
        let directory = std::env::temp_dir().join(format!("lemma-mount-{}", std::process::id()));
        std::fs::create_dir_all(&directory).unwrap();
        let billing = member(&directory, "billing");
        let mut store = Store::open(Path::new(&billing.database)).unwrap();
        let id = save(&mut store, &snapshot("/billing", "fp"), &sample_files());
        let links = [ExportLink {
            caller: "services.api.charge".to_owned(),
            provider: "#ledger:services.api.post".to_owned(),
        }];
        store
            .replace_exports(id, "fp", &[export("api", "charge")], &links)
            .unwrap();
        let mut workspace = Store::open(&directory.join("workspace.ls")).unwrap();

        workspace
            .mount_workspace(&[billing.clone(), member(&directory, "absent")])
            .unwrap();
        let exports = workspace.workspace_exports().unwrap();
        store
            .replace_exports(id, "fp2", &[export("api", "refund")], &links)
            .unwrap();
        let after = workspace.workspace_exports().unwrap();
        let mounted_links = workspace.workspace_links().unwrap();

        assert_eq!(exports, [("#billing".to_owned(), export("api", "charge"))]);
        assert_eq!(after, [("#billing".to_owned(), export("api", "refund"))]);
        assert_eq!(mounted_links[0].caller, "#billing:services.api.charge");
        assert_eq!(mounted_links[0].provider, "#ledger:services.api.post");
        assert_eq!(workspace.workspace_members().unwrap().len(), 2);
        drop((store, workspace));
        let _ = std::fs::remove_dir_all(&directory);
    }
}
