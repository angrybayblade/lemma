//! What the query layer reads and records: snapshots, files and symbols
//! from the syntax index, and answers from providers.

use lemma_model::{
    Language, Location, LocationQuery, Position, ProviderAnswer, Snapshot, Symbol, SymbolKind,
};

use crate::{Digest, FileRecord, StoreError, SymbolId};

/// A snapshot as stored, with its row id.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct StoredSnapshot {
    /// Row id, valid only within this database.
    pub id: i64,
    /// Version of the extractor that produced the snapshot's symbols.
    pub extractor_version: String,
    /// Wall-clock time, in nanoseconds since the Unix epoch, when the indexing
    /// run that produced this snapshot started.
    pub indexed_at_ns: i64,
    /// The snapshot itself.
    pub snapshot: Snapshot,
}

/// A symbol as stored, with the file it belongs to.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct StoredSymbol {
    /// Its id, the same in any store; see [`SymbolId`].
    pub id: SymbolId,
    /// Path relative to the snapshot root.
    pub path: String,
    /// Language of the containing file.
    pub language: Language,
    /// The symbol itself.
    pub symbol: Symbol,
}

/// A repository a workspace index composes.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct StoredMember {
    /// Its name in the workspace, such as `#billing`.
    pub name: String,
    /// Its local clone.
    pub root: String,
    /// The commit it is pinned to, if it is.
    pub revision: Option<String>,
    /// The directory indexed: the clone, or a worktree at the pin.
    pub indexed_root: String,
    /// The database holding its index.
    pub database: String,
}

/// File system metadata recorded when a file was read.
///
/// If a file's stamp is unchanged, its contents almost certainly are too, so
/// indexing can skip reading it. See `lemma-index` for when a stamp is trusted.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct FileStamp {
    /// File size.
    pub size_bytes: u64,
    /// Last content modification, in nanoseconds since the Unix epoch.
    pub modified_ns: i64,
    /// Last metadata change, such as a rename or permission change, in
    /// nanoseconds since the Unix epoch. Zero where the platform has none.
    pub changed_ns: i64,
    /// File system inode number. Zero where the platform has none.
    pub inode: u64,
}

/// A file to store along with its symbols.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct StampedFile {
    /// Path relative to the snapshot root, using `/` separators.
    pub path: String,
    /// What was read from it.
    pub record: FileRecord,
    /// Its metadata when it was read.
    pub stamp: FileStamp,
}

impl StampedFile {
    /// The hash of its contents, as [`lemma_model::content_hash`] writes it.
    pub fn content_hash(&self) -> String {
        hex(&self.record.blob)
    }
}

/// A digest written as lowercase hexadecimal.
pub(crate) fn hex(digest: &Digest) -> String {
    const DIGITS: &[u8; 16] = b"0123456789abcdef";
    let mut text = String::with_capacity(64);
    for byte in digest {
        text.push(char::from(DIGITS[usize::from(byte >> 4)]));
        text.push(char::from(DIGITS[usize::from(byte & 0x0f)]));
    }
    text
}

/// A stored file, without its symbols.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct StoredFile {
    /// Path relative to the snapshot root.
    pub path: String,
    /// The file's language.
    pub language: Language,
    /// Hash of its contents when indexed.
    pub content_hash: String,
    /// Whether the parser recovered from syntax errors.
    pub has_syntax_errors: bool,
    /// How many symbols it defines.
    pub symbol_count: usize,
    /// Its metadata when it was read.
    pub stamp: FileStamp,
}

/// A snapshot to store from scratch.
#[derive(Debug, Clone, Copy)]
pub struct NewSnapshot<'a> {
    /// The snapshot itself.
    pub snapshot: &'a Snapshot,
    /// Version of the extractor that produced the symbols.
    pub extractor_version: &'a str,
    /// When the indexing run started; see [`StoredSnapshot::indexed_at_ns`].
    pub indexed_at_ns: i64,
    /// Every file with its symbols.
    pub files: &'a [StampedFile],
    /// Source files that differ from the snapshot's revision.
    pub uncommitted_paths: &'a [String],
}

/// Changes to bring a stored snapshot up to date with the files on disk.
#[derive(Debug, Clone, Copy)]
pub struct SnapshotUpdate<'a> {
    /// The snapshot as it is now.
    pub snapshot: &'a Snapshot,
    /// When the indexing run started; see [`StoredSnapshot::indexed_at_ns`].
    pub indexed_at_ns: i64,
    /// Source files that now differ from the snapshot's revision.
    pub uncommitted_paths: &'a [String],
    /// Files that are new or whose contents changed.
    pub changed: &'a [StampedFile],
    /// Paths of files that no longer exist or can no longer be read.
    pub removed: &'a [String],
    /// Files whose contents are unchanged but whose metadata moved on.
    pub restamped: &'a [(String, FileStamp)],
}

/// Just enough of a symbol to search and rank it in memory.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SymbolKey {
    /// Its id.
    pub id: SymbolId,
    /// The position of its file in the snapshot's files, sorted by path.
    pub file_id: i64,
    /// The symbol's qualified name; its name is the last part.
    pub qualified_name: String,
    /// Length of the name, in bytes, at the end of `qualified_name`.
    pub name_length: usize,
    /// What kind of symbol it is.
    pub kind: SymbolKind,
    /// First line of the definition.
    pub start_line: u32,
}

/// A file as needed for searching its symbols.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FileKey {
    /// Its position in the snapshot's files, sorted by path.
    pub id: i64,
    /// Path relative to the snapshot root.
    pub path: String,
    /// The file's language.
    pub language: Language,
}

/// How a symbol search compares names.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum NameMatch {
    /// The name or qualified name equals the text.
    Exact,
    /// The name or qualified name contains the text, ignoring ASCII case.
    Contains,
}

/// Criteria for finding symbols in a snapshot.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SymbolFilter {
    /// Text compared against both the name and the qualified name.
    pub text: String,
    /// How `text` is compared.
    pub name_match: NameMatch,
    /// Only symbols of this kind.
    pub kind: Option<SymbolKind>,
    /// Only symbols in files of this language.
    pub language: Option<Language>,
    /// Only symbols whose file path contains this text.
    pub path_contains: Option<String>,
    /// Maximum number of results.
    pub limit: u32,
}

/// A question asked of a provider, as the store files its answer.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Question {
    /// A question answered with locations.
    Locations(LocationQuery),
    /// What the provider shows for a symbol.
    Hover,
    /// Lemma's own warnings about files a provider leaves out of a
    /// symbol's references; costly to find, so kept with the answers.
    UnseenFiles,
}

impl Question {
    /// The name stored in the `answers` table.
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Locations(query) => query.as_str(),
            Self::Hover => "hover",
            Self::UnseenFiles => "unseen_files",
        }
    }
}

/// Everything that identifies a provider's answer within one snapshot.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct AnswerKey<'a> {
    /// The snapshot the question was asked in.
    pub snapshot_id: i64,
    /// What was asked.
    pub question: Question,
    /// The file the question was asked about, relative to the root.
    pub path: &'a str,
    /// The position the question was asked at, such as a symbol's name.
    pub position: Position,
    /// The provider that answered.
    pub provider_id: &'a str,
    /// Fingerprint of the provider's configuration.
    pub config_fingerprint: &'a str,
}

/// A previously stored provider answer.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct StoredAnswer<T> {
    /// The version the provider reported when it answered.
    pub provider_version: Option<String>,
    /// The answer.
    pub answer: ProviderAnswer<T>,
}

/// The facts the query layer reads and records: symbols from the syntax
/// index and answers from providers.
///
/// [`crate::Store`] implements it over `index.ls`.
pub trait FactStore {
    /// Every file in a snapshot, without symbols, sorted by path.
    fn files(&self, snapshot_id: i64) -> Result<Vec<StoredFile>, StoreError>;

    /// Symbols matching a filter. Exact name matches come first, then
    /// shorter qualified names, then by path and line.
    fn find_symbols(
        &self,
        snapshot_id: i64,
        filter: &SymbolFilter,
    ) -> Result<Vec<StoredSymbol>, StoreError>;

    /// One symbol by id, if it belongs to the snapshot.
    fn symbol(
        &self,
        snapshot_id: i64,
        symbol_id: SymbolId,
    ) -> Result<Option<StoredSymbol>, StoreError>;

    /// Every file in a snapshot with its symbols, in source order, files
    /// sorted by path: one read of the whole snapshot, for callers that
    /// would otherwise ask file by file.
    fn symbols_by_file(
        &self,
        snapshot_id: i64,
    ) -> Result<Vec<(String, Vec<StoredSymbol>)>, StoreError>;

    /// Every symbol defined in a file, in source order.
    fn symbols_in_file(
        &self,
        snapshot_id: i64,
        path: &str,
    ) -> Result<Vec<StoredSymbol>, StoreError>;

    /// The symbol whose name is at a position, such as where a provider
    /// says something is defined.
    fn symbol_named_at(
        &self,
        snapshot_id: i64,
        path: &str,
        position: Position,
    ) -> Result<Option<StoredSymbol>, StoreError>;

    /// The content hash a file had when the snapshot was taken.
    fn file_hash(&self, snapshot_id: i64, path: &str) -> Result<Option<String>, StoreError>;

    /// The qualified name of the innermost symbol whose definition contains
    /// a position, such as the function a call sits in.
    fn enclosing_symbol(
        &self,
        snapshot_id: i64,
        path: &str,
        position: Position,
    ) -> Result<Option<String>, StoreError>;

    /// A stored answer made of locations, if this exact question was asked.
    fn cached_locations(
        &self,
        key: &AnswerKey<'_>,
    ) -> Result<Option<StoredAnswer<Vec<Location>>>, StoreError>;

    /// Stores an answer made of locations, replacing any earlier one.
    fn save_locations(
        &mut self,
        key: &AnswerKey<'_>,
        provider_version: Option<&str>,
        answer: &ProviderAnswer<Vec<Location>>,
    ) -> Result<(), StoreError>;

    /// A stored text answer, if this exact question was asked.
    fn cached_text(&self, key: &AnswerKey<'_>) -> Result<Option<StoredAnswer<String>>, StoreError>;

    /// Stores a text answer, replacing any earlier one.
    fn save_text(
        &mut self,
        key: &AnswerKey<'_>,
        provider_version: Option<&str>,
        answer: &ProviderAnswer<String>,
    ) -> Result<(), StoreError>;
}
