//! SQLite storage for snapshots, symbols, and provider results.
//!
//! This is Lemma's shared storage format: every language and provider writes
//! the same normalized rows, so tools can query them without knowing where
//! they came from.

use std::path::Path;
use std::str::FromStr;

use lemma_model::{
    IndexedFile, Language, Position, Reference, ReferenceOutcome, Snapshot, Span, Symbol,
    SymbolKind,
};
use rusqlite::{Connection, OptionalExtension, Row, Transaction, params};

/// Version of `schema.sql`. Databases written with another version are
/// rejected rather than migrated, since an index can always be rebuilt.
pub const SCHEMA_VERSION: &str = "2";

const SCHEMA: &str = include_str!("schema.sql");

/// Why a storage operation failed.
#[derive(Debug, thiserror::Error)]
pub enum StoreError {
    /// SQLite reported an error.
    #[error("database error")]
    Sqlite(#[from] rusqlite::Error),
    /// The database was written by a different version of Lemma.
    #[error(
        "index uses schema version {found}, expected {SCHEMA_VERSION}; run `lemma index` to rebuild it"
    )]
    SchemaMismatch {
        /// The version recorded in the database.
        found: String,
    },
}

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
    /// Row id, valid only within this database and snapshot.
    pub id: i64,
    /// Path relative to the snapshot root.
    pub path: String,
    /// Language of the containing file.
    pub language: Language,
    /// The symbol itself.
    pub symbol: Symbol,
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
    /// The file's contents as indexed.
    pub file: IndexedFile,
    /// Its metadata when it was read.
    pub stamp: FileStamp,
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
    /// Row id.
    pub id: i64,
    /// Row id of the file the symbol is in.
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
    /// Row id.
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

/// Everything that identifies a reference lookup within one snapshot.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ReferenceLookupKey {
    /// The symbol whose references were requested.
    pub symbol_id: i64,
    /// The provider that answered.
    pub provider_id: String,
    /// Fingerprint of the provider's configuration.
    pub config_fingerprint: String,
    /// Whether the declaration itself counts as a reference.
    pub include_declaration: bool,
}

/// A previously stored provider answer.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CachedReferences {
    /// The version the provider reported when it answered.
    pub provider_version: Option<String>,
    /// The answer. Enclosing symbols are not stored; callers fill them in.
    pub outcome: ReferenceOutcome,
}

/// An open Lemma database.
pub struct Store {
    connection: Connection,
}

impl Store {
    /// Opens or creates a database file.
    pub fn open(path: &Path) -> Result<Self, StoreError> {
        let connection = Connection::open(path)?;
        connection.pragma_update(None, "journal_mode", "WAL")?;
        Self::initialize(connection)
    }

    /// Opens a temporary database that lives only as long as the store.
    pub fn open_in_memory() -> Result<Self, StoreError> {
        Self::initialize(Connection::open_in_memory()?)
    }

    fn initialize(connection: Connection) -> Result<Self, StoreError> {
        connection.pragma_update(None, "foreign_keys", true)?;
        connection.execute_batch(SCHEMA)?;
        connection.execute(
            "INSERT OR IGNORE INTO schema_meta (key, value) VALUES ('schema_version', ?1)",
            [SCHEMA_VERSION],
        )?;

        let found: String = connection.query_row(
            "SELECT value FROM schema_meta WHERE key = 'schema_version'",
            [],
            |row| row.get(0),
        )?;
        if found != SCHEMA_VERSION {
            return Err(StoreError::SchemaMismatch { found });
        }
        Ok(Self { connection })
    }

    /// Stores a snapshot and its files, replacing earlier snapshots of the
    /// same root along with everything derived from them.
    pub fn replace_snapshot(&mut self, new: &NewSnapshot<'_>) -> Result<i64, StoreError> {
        let NewSnapshot {
            snapshot,
            extractor_version,
            indexed_at_ns,
            files,
            uncommitted_paths,
        } = *new;
        let transaction = self.connection.transaction()?;
        transaction.execute("DELETE FROM snapshots WHERE root = ?1", [&snapshot.root])?;
        transaction.execute(
            "INSERT INTO snapshots
                 (root, revision, is_dirty, fingerprint, extractor_version, indexed_at_ns)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6)",
            params![
                snapshot.root,
                snapshot.revision,
                snapshot.is_dirty,
                snapshot.fingerprint,
                extractor_version,
                indexed_at_ns
            ],
        )?;
        let snapshot_id = transaction.last_insert_rowid();
        insert_files(&transaction, snapshot_id, files)?;
        replace_uncommitted_paths(&transaction, snapshot_id, uncommitted_paths)?;
        transaction.commit()?;
        Ok(snapshot_id)
    }

    /// Updates a stored snapshot in place, rewriting only the files that
    /// changed. Symbols of untouched files keep their ids.
    ///
    /// When any file's contents changed, every cached provider answer for the
    /// snapshot is dropped: an edit in one file can change the references to
    /// a symbol defined in another.
    pub fn update_snapshot(
        &mut self,
        snapshot_id: i64,
        update: &SnapshotUpdate<'_>,
    ) -> Result<(), StoreError> {
        let transaction = self.connection.transaction()?;
        transaction.execute(
            "UPDATE snapshots
             SET revision = ?2, is_dirty = ?3, fingerprint = ?4, indexed_at_ns = ?5
             WHERE id = ?1",
            params![
                snapshot_id,
                update.snapshot.revision,
                update.snapshot.is_dirty,
                update.snapshot.fingerprint,
                update.indexed_at_ns
            ],
        )?;

        let contents_changed = !update.changed.is_empty() || !update.removed.is_empty();
        if contents_changed {
            // Checked per cached lookup rather than per symbol: there are few
            // lookups and hundreds of thousands of symbols.
            transaction.execute(
                "DELETE FROM reference_lookups WHERE EXISTS
                     (SELECT 1 FROM symbols s JOIN files f ON f.id = s.file_id
                      WHERE s.id = reference_lookups.symbol_id AND f.snapshot_id = ?1)",
                [snapshot_id],
            )?;
        }

        let mut delete_file =
            transaction.prepare_cached("DELETE FROM files WHERE snapshot_id = ?1 AND path = ?2")?;
        let changed_paths = update.changed.iter().map(|changed| &changed.file.path);
        for path in update.removed.iter().chain(changed_paths) {
            delete_file.execute(params![snapshot_id, path])?;
        }
        insert_files(&transaction, snapshot_id, update.changed)?;

        let mut restamp = transaction.prepare_cached(
            "UPDATE files SET size_bytes = ?3, modified_ns = ?4, changed_ns = ?5, inode = ?6
             WHERE snapshot_id = ?1 AND path = ?2",
        )?;
        for (path, stamp) in update.restamped {
            restamp.execute(params![
                snapshot_id,
                path,
                stamp_integer(stamp.size_bytes),
                stamp.modified_ns,
                stamp.changed_ns,
                stamp_integer(stamp.inode)
            ])?;
        }
        drop((delete_file, restamp));
        replace_uncommitted_paths(&transaction, snapshot_id, update.uncommitted_paths)?;
        transaction.commit()?;
        Ok(())
    }

    /// The stored snapshot of a directory, if it has been indexed.
    pub fn snapshot_for_root(&self, root: &str) -> Result<Option<StoredSnapshot>, StoreError> {
        let snapshot = self
            .connection
            .query_row(
                "SELECT id, root, revision, is_dirty, fingerprint, extractor_version, indexed_at_ns
                 FROM snapshots WHERE root = ?1",
                [root],
                read_stored_snapshot,
            )
            .optional()?;
        Ok(snapshot)
    }

    /// Source files that differed from the snapshot's revision when it was
    /// taken, sorted.
    pub fn uncommitted_paths(&self, snapshot_id: i64) -> Result<Vec<String>, StoreError> {
        let mut statement = self.connection.prepare_cached(
            "SELECT path FROM uncommitted_paths WHERE snapshot_id = ?1 ORDER BY path",
        )?;
        let rows = statement.query_map([snapshot_id], |row| row.get(0))?;
        Ok(rows.collect::<Result<_, _>>()?)
    }

    /// Every file in a snapshot, without symbols, sorted by path.
    pub fn files(&self, snapshot_id: i64) -> Result<Vec<StoredFile>, StoreError> {
        let mut statement = self.connection.prepare_cached(
            "SELECT path, language, content_hash, has_syntax_errors, symbol_count,
                    size_bytes, modified_ns, changed_ns, inode
             FROM files WHERE snapshot_id = ?1 ORDER BY path",
        )?;
        let rows = statement.query_map([snapshot_id], |row| {
            Ok(StoredFile {
                path: row.get(0)?,
                language: parse_column(row, 1)?,
                content_hash: row.get(2)?,
                has_syntax_errors: row.get(3)?,
                symbol_count: row.get(4)?,
                stamp: FileStamp {
                    size_bytes: unstamp_integer(row.get(5)?),
                    modified_ns: row.get(6)?,
                    changed_ns: row.get(7)?,
                    inode: unstamp_integer(row.get(8)?),
                },
            })
        })?;
        Ok(rows.collect::<Result<_, _>>()?)
    }

    /// The most recently stored snapshot.
    pub fn latest_snapshot(&self) -> Result<Option<StoredSnapshot>, StoreError> {
        let snapshot = self
            .connection
            .query_row(
                "SELECT id, root, revision, is_dirty, fingerprint, extractor_version, indexed_at_ns
                 FROM snapshots ORDER BY id DESC LIMIT 1",
                [],
                read_stored_snapshot,
            )
            .optional()?;
        Ok(snapshot)
    }

    /// Symbols matching a filter. Exact name matches come first, then shorter
    /// qualified names, then by path and line.
    pub fn find_symbols(
        &self,
        snapshot_id: i64,
        filter: &SymbolFilter,
    ) -> Result<Vec<StoredSymbol>, StoreError> {
        // Separate statements, so an exact search can use the name indexes
        // instead of scanning every symbol.
        let name_condition = match filter.name_match {
            NameMatch::Exact => "(s.name = ?2 OR s.qualified_name = ?2)",
            // A name is always the end of its qualified name, so searching
            // the qualified name alone finds both.
            NameMatch::Contains => r"s.qualified_name LIKE ?3 ESCAPE '\'",
        };
        let sql = format!(
            r"SELECT s.id, f.path, f.language, s.name, s.qualified_name, s.kind,
                     s.start_line, s.start_column, s.end_line, s.end_column,
                     s.name_start_line, s.name_start_column, s.name_end_line, s.name_end_column
              FROM symbols s JOIN files f ON f.id = s.file_id
              WHERE f.snapshot_id = ?1
                AND {name_condition}
                AND (?4 IS NULL OR s.kind = ?4)
                AND (?5 IS NULL OR f.language = ?5)
                AND (?6 IS NULL OR instr(f.path, ?6) > 0)
              ORDER BY (s.name = ?2 OR s.qualified_name = ?2) DESC,
                       length(s.qualified_name), f.path, s.start_line
              LIMIT ?7"
        );
        let mut statement = self.connection.prepare_cached(&sql)?;
        let rows = statement.query_map(
            params![
                snapshot_id,
                filter.text,
                like_pattern(&filter.text),
                filter.kind.map(SymbolKind::as_str),
                filter.language.map(Language::as_str),
                filter.path_contains,
                filter.limit,
            ],
            read_stored_symbol,
        )?;
        Ok(rows.collect::<Result<_, _>>()?)
    }

    /// Every symbol in a snapshot, in the compact form used for searching
    /// in memory.
    pub fn symbol_keys(&self, snapshot_id: i64) -> Result<Vec<SymbolKey>, StoreError> {
        let mut statement = self.connection.prepare_cached(
            "SELECT s.id, s.file_id, s.qualified_name, length(CAST(s.name AS BLOB)), s.kind,
                    s.start_line
             FROM symbols s JOIN files f ON f.id = s.file_id
             WHERE f.snapshot_id = ?1",
        )?;
        let rows = statement.query_map([snapshot_id], |row| {
            Ok(SymbolKey {
                id: row.get(0)?,
                file_id: row.get(1)?,
                qualified_name: row.get(2)?,
                name_length: row.get(3)?,
                kind: parse_column(row, 4)?,
                start_line: row.get(5)?,
            })
        })?;
        Ok(rows.collect::<Result<_, _>>()?)
    }

    /// Every file in a snapshot, as needed for searching its symbols.
    pub fn file_keys(&self, snapshot_id: i64) -> Result<Vec<FileKey>, StoreError> {
        let mut statement = self
            .connection
            .prepare_cached("SELECT id, path, language FROM files WHERE snapshot_id = ?1")?;
        let rows = statement.query_map([snapshot_id], |row| {
            Ok(FileKey {
                id: row.get(0)?,
                path: row.get(1)?,
                language: parse_column(row, 2)?,
            })
        })?;
        Ok(rows.collect::<Result<_, _>>()?)
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

    /// One symbol by id, if it belongs to the snapshot.
    pub fn symbol(
        &self,
        snapshot_id: i64,
        symbol_id: i64,
    ) -> Result<Option<StoredSymbol>, StoreError> {
        let symbol = self
            .connection
            .prepare_cached(
                "SELECT s.id, f.path, f.language, s.name, s.qualified_name, s.kind,
                        s.start_line, s.start_column, s.end_line, s.end_column,
                        s.name_start_line, s.name_start_column, s.name_end_line, s.name_end_column
                 FROM symbols s JOIN files f ON f.id = s.file_id
                 WHERE f.snapshot_id = ?1 AND s.id = ?2",
            )?
            .query_row(params![snapshot_id, symbol_id], read_stored_symbol)
            .optional()?;
        Ok(symbol)
    }

    /// The content hash a file had when the snapshot was taken.
    pub fn file_hash(&self, snapshot_id: i64, path: &str) -> Result<Option<String>, StoreError> {
        let hash = self
            .connection
            .query_row(
                "SELECT content_hash FROM files WHERE snapshot_id = ?1 AND path = ?2",
                params![snapshot_id, path],
                |row| row.get(0),
            )
            .optional()?;
        Ok(hash)
    }

    /// The qualified name of the innermost symbol whose definition contains a
    /// position, such as the function a call sits in.
    pub fn enclosing_symbol(
        &self,
        snapshot_id: i64,
        path: &str,
        position: Position,
    ) -> Result<Option<String>, StoreError> {
        let name = self
            .connection
            .prepare_cached(
                "SELECT s.qualified_name
                 FROM symbols s JOIN files f ON f.id = s.file_id
                 WHERE f.snapshot_id = ?1 AND f.path = ?2
                   AND (s.start_line, s.start_column) <= (?3, ?4)
                   AND (s.end_line, s.end_column) >= (?3, ?4)
                 ORDER BY s.start_line DESC, s.start_column DESC
                 LIMIT 1",
            )?
            .query_row(
                params![snapshot_id, path, position.line, position.column],
                |row| row.get(0),
            )
            .optional()?;
        Ok(name)
    }

    /// A stored provider answer for this exact lookup, if one exists.
    pub fn cached_references(
        &self,
        key: &ReferenceLookupKey,
    ) -> Result<Option<CachedReferences>, StoreError> {
        let lookup: Option<(i64, Option<String>, String)> = self
            .connection
            .query_row(
                "SELECT id, provider_version, outcome FROM reference_lookups
                 WHERE symbol_id = ?1 AND provider_id = ?2
                   AND config_fingerprint = ?3 AND include_declaration = ?4",
                params![
                    key.symbol_id,
                    key.provider_id,
                    key.config_fingerprint,
                    key.include_declaration
                ],
                |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)),
            )
            .optional()?;
        let Some((lookup_id, provider_version, label)) = lookup else {
            return Ok(None);
        };

        let outcome = match label.as_str() {
            "found" => ReferenceOutcome::Found {
                references: self.reference_results(lookup_id)?,
            },
            "empty" => ReferenceOutcome::Empty,
            "unsupported" => ReferenceOutcome::Unsupported,
            // An unknown label means the row predates the current meaning of
            // the column; treat it as a miss so the lookup is redone.
            _ => return Ok(None),
        };
        Ok(Some(CachedReferences {
            provider_version,
            outcome,
        }))
    }

    /// Stores a provider answer, replacing any earlier one for the same key.
    ///
    /// Only definitive answers are kept. Failures, stale files, and missing
    /// providers are not facts about the code, so they return `false` and are
    /// not stored.
    pub fn save_references(
        &mut self,
        key: &ReferenceLookupKey,
        provider_version: Option<&str>,
        outcome: &ReferenceOutcome,
    ) -> Result<bool, StoreError> {
        let Some(label) = cacheable_label(outcome) else {
            return Ok(false);
        };

        let transaction = self.connection.transaction()?;
        // Delete explicitly so the cascade removes old results; REPLACE does
        // not reliably fire foreign key actions.
        transaction.execute(
            "DELETE FROM reference_lookups
             WHERE symbol_id = ?1 AND provider_id = ?2
               AND config_fingerprint = ?3 AND include_declaration = ?4",
            params![
                key.symbol_id,
                key.provider_id,
                key.config_fingerprint,
                key.include_declaration
            ],
        )?;
        transaction.execute(
            "INSERT INTO reference_lookups
                 (symbol_id, provider_id, provider_version, config_fingerprint,
                  include_declaration, outcome)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6)",
            params![
                key.symbol_id,
                key.provider_id,
                provider_version,
                key.config_fingerprint,
                key.include_declaration,
                label
            ],
        )?;
        let lookup_id = transaction.last_insert_rowid();
        if let ReferenceOutcome::Found { references } = outcome {
            insert_reference_results(&transaction, lookup_id, references)?;
        }
        transaction.commit()?;
        Ok(true)
    }

    fn reference_results(&self, lookup_id: i64) -> Result<Vec<Reference>, StoreError> {
        let mut statement = self.connection.prepare_cached(
            "SELECT path, is_external, start_line, start_column, end_line, end_column
             FROM reference_results WHERE lookup_id = ?1
             ORDER BY path, start_line, start_column",
        )?;
        let rows = statement.query_map([lookup_id], |row| {
            Ok(Reference {
                path: row.get(0)?,
                is_external: row.get(1)?,
                span: read_span(row, 2)?,
                enclosing_symbol: None,
            })
        })?;
        Ok(rows.collect::<Result<_, _>>()?)
    }
}

fn insert_files(
    transaction: &Transaction<'_>,
    snapshot_id: i64,
    files: &[StampedFile],
) -> Result<(), StoreError> {
    let mut insert_file = transaction.prepare_cached(
        "INSERT INTO files (snapshot_id, path, language, content_hash, has_syntax_errors,
                            symbol_count, size_bytes, modified_ns, changed_ns, inode)
         VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10)",
    )?;
    let mut insert_symbol = transaction.prepare_cached(
        "INSERT INTO symbols (file_id, name, qualified_name, kind,
                              start_line, start_column, end_line, end_column,
                              name_start_line, name_start_column, name_end_line, name_end_column)
         VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12)",
    )?;

    for StampedFile { file, stamp } in files {
        insert_file.execute(params![
            snapshot_id,
            file.path,
            file.language.as_str(),
            file.content_hash,
            file.has_syntax_errors,
            file.symbols.len(),
            stamp_integer(stamp.size_bytes),
            stamp.modified_ns,
            stamp.changed_ns,
            stamp_integer(stamp.inode)
        ])?;
        let file_id = transaction.last_insert_rowid();
        for symbol in &file.symbols {
            insert_symbol.execute(params![
                file_id,
                symbol.name,
                symbol.qualified_name,
                symbol.kind.as_str(),
                symbol.span.start.line,
                symbol.span.start.column,
                symbol.span.end.line,
                symbol.span.end.column,
                symbol.name_span.start.line,
                symbol.name_span.start.column,
                symbol.name_span.end.line,
                symbol.name_span.end.column,
            ])?;
        }
    }
    Ok(())
}

fn replace_uncommitted_paths(
    transaction: &Transaction<'_>,
    snapshot_id: i64,
    paths: &[String],
) -> Result<(), StoreError> {
    transaction.execute(
        "DELETE FROM uncommitted_paths WHERE snapshot_id = ?1",
        [snapshot_id],
    )?;
    let mut insert = transaction.prepare_cached(
        "INSERT OR IGNORE INTO uncommitted_paths (snapshot_id, path) VALUES (?1, ?2)",
    )?;
    for path in paths {
        insert.execute(params![snapshot_id, path])?;
    }
    Ok(())
}

fn insert_reference_results(
    transaction: &Transaction<'_>,
    lookup_id: i64,
    references: &[Reference],
) -> Result<(), StoreError> {
    let mut insert = transaction.prepare_cached(
        "INSERT INTO reference_results
             (lookup_id, path, is_external, start_line, start_column, end_line, end_column)
         VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7)",
    )?;
    for reference in references {
        insert.execute(params![
            lookup_id,
            reference.path,
            reference.is_external,
            reference.span.start.line,
            reference.span.start.column,
            reference.span.end.line,
            reference.span.end.column,
        ])?;
    }
    Ok(())
}

fn cacheable_label(outcome: &ReferenceOutcome) -> Option<&'static str> {
    match outcome {
        ReferenceOutcome::Found { .. } => Some("found"),
        ReferenceOutcome::Empty => Some("empty"),
        ReferenceOutcome::Unsupported => Some("unsupported"),
        ReferenceOutcome::ProviderUnavailable { .. }
        | ReferenceOutcome::Stale
        | ReferenceOutcome::ProviderFailed { .. } => None,
    }
}

/// Wraps search text for `LIKE`, escaping its wildcards so they match literally.
fn like_pattern(text: &str) -> String {
    let escaped = text
        .replace('\\', r"\\")
        .replace('%', r"\%")
        .replace('_', r"\_");
    format!("%{escaped}%")
}

fn read_stored_snapshot(row: &Row<'_>) -> rusqlite::Result<StoredSnapshot> {
    Ok(StoredSnapshot {
        id: row.get(0)?,
        extractor_version: row.get(5)?,
        indexed_at_ns: row.get(6)?,
        snapshot: Snapshot {
            root: row.get(1)?,
            revision: row.get(2)?,
            is_dirty: row.get(3)?,
            fingerprint: row.get(4)?,
        },
    })
}

/// SQLite integers are signed. Sizes and inode numbers never reach 2^63 in
/// practice; the bit pattern is kept either way, so equality still holds.
#[expect(clippy::cast_possible_wrap, reason = "only compared for equality")]
fn stamp_integer(value: u64) -> i64 {
    value as i64
}

/// Reverses [`stamp_integer`].
#[expect(clippy::cast_sign_loss, reason = "restores the stored bit pattern")]
fn unstamp_integer(value: i64) -> u64 {
    value as u64
}

fn read_stored_symbol(row: &Row<'_>) -> rusqlite::Result<StoredSymbol> {
    Ok(StoredSymbol {
        id: row.get(0)?,
        path: row.get(1)?,
        language: parse_column(row, 2)?,
        symbol: Symbol {
            name: row.get(3)?,
            qualified_name: row.get(4)?,
            kind: parse_column(row, 5)?,
            span: read_span(row, 6)?,
            name_span: read_span(row, 10)?,
        },
    })
}

/// Reads four consecutive columns as start line, start column, end line, end column.
fn read_span(row: &Row<'_>, first: usize) -> rusqlite::Result<Span> {
    Ok(Span {
        start: Position {
            line: row.get(first)?,
            column: row.get(first + 1)?,
        },
        end: Position {
            line: row.get(first + 2)?,
            column: row.get(first + 3)?,
        },
    })
}

fn parse_column<T>(row: &Row<'_>, index: usize) -> rusqlite::Result<T>
where
    T: FromStr,
    T::Err: std::error::Error + Send + Sync + 'static,
{
    let text: String = row.get(index)?;
    text.parse().map_err(|error| {
        rusqlite::Error::FromSqlConversionFailure(
            index,
            rusqlite::types::Type::Text,
            Box::new(error),
        )
    })
}

#[cfg(test)]
mod tests {
    use super::*;

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

    fn stamped(file: IndexedFile) -> StampedFile {
        StampedFile {
            file,
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
        vec![stamped(IndexedFile {
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
        stamped(IndexedFile {
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

    fn cache_empty_answer(store: &mut Store, symbol_id: i64) -> ReferenceLookupKey {
        let key = ReferenceLookupKey {
            symbol_id,
            provider_id: "pyright".to_owned(),
            config_fingerprint: "config".to_owned(),
            include_declaration: false,
        };
        store
            .save_references(&key, None, &ReferenceOutcome::Empty)
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
        let router_id = symbol_id(&store, snapshot_id, "Router");
        let key = cache_empty_answer(&mut store, router_id);

        let changed = [other_file("src/new.py", "new_helper")];
        let next = snapshot("/repo", "fp-2");
        store
            .update_snapshot(snapshot_id, &update(&next, &changed, &[], &[]))
            .unwrap();

        assert_eq!(store.cached_references(&key).unwrap(), None);
    }

    #[test]
    fn restamping_keeps_cached_answers() {
        let (mut store, snapshot_id) = store_with_sample();
        let router_id = symbol_id(&store, snapshot_id, "Router");
        let key = cache_empty_answer(&mut store, router_id);
        let stamp = FileStamp {
            modified_ns: 50,
            ..sample_files()[0].stamp
        };

        let restamped = [("src/router.py".to_owned(), stamp)];
        let same = snapshot("/repo", "fp-1");
        store
            .update_snapshot(snapshot_id, &update(&same, &[], &[], &restamped))
            .unwrap();

        assert!(store.cached_references(&key).unwrap().is_some());
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
    fn found_references_round_trip_through_the_cache() {
        let (mut store, snapshot_id) = store_with_sample();
        let symbol_id = store
            .find_symbols(snapshot_id, &filter("delete", NameMatch::Exact))
            .unwrap()[0]
            .id;
        let key = ReferenceLookupKey {
            symbol_id,
            provider_id: "pyright".to_owned(),
            config_fingerprint: "cfg".to_owned(),
            include_declaration: false,
        };
        let outcome = ReferenceOutcome::Found {
            references: vec![Reference {
                path: "src/app.py".to_owned(),
                span: span(4, 2, 4, 8),
                is_external: false,
                enclosing_symbol: None,
            }],
        };

        assert!(
            store
                .save_references(&key, Some("1.1.400"), &outcome)
                .unwrap()
        );
        let cached = store.cached_references(&key).unwrap().unwrap();

        assert_eq!(cached.provider_version.as_deref(), Some("1.1.400"));
        assert_eq!(cached.outcome, outcome);
    }

    #[test]
    fn saving_again_replaces_earlier_results() {
        let (mut store, snapshot_id) = store_with_sample();
        let symbol_id = store
            .find_symbols(snapshot_id, &filter("delete", NameMatch::Exact))
            .unwrap()[0]
            .id;
        let key = ReferenceLookupKey {
            symbol_id,
            provider_id: "pyright".to_owned(),
            config_fingerprint: "cfg".to_owned(),
            include_declaration: false,
        };
        let first = ReferenceOutcome::Found {
            references: vec![Reference {
                path: "src/old.py".to_owned(),
                span: span(1, 0, 1, 6),
                is_external: false,
                enclosing_symbol: None,
            }],
        };

        store.save_references(&key, None, &first).unwrap();
        store
            .save_references(&key, None, &ReferenceOutcome::Empty)
            .unwrap();

        let cached = store.cached_references(&key).unwrap().unwrap();
        assert_eq!(cached.outcome, ReferenceOutcome::Empty);
        let orphaned: i64 = store
            .connection
            .query_row("SELECT count(*) FROM reference_results", [], |row| {
                row.get(0)
            })
            .unwrap();
        assert_eq!(orphaned, 0);
    }

    #[test]
    fn failures_are_not_cached() {
        let (mut store, snapshot_id) = store_with_sample();
        let symbol_id = store
            .find_symbols(snapshot_id, &filter("delete", NameMatch::Exact))
            .unwrap()[0]
            .id;
        let key = ReferenceLookupKey {
            symbol_id,
            provider_id: "pyright".to_owned(),
            config_fingerprint: "cfg".to_owned(),
            include_declaration: false,
        };
        let failure = ReferenceOutcome::ProviderFailed {
            message: "timed out".to_owned(),
        };

        assert!(!store.save_references(&key, None, &failure).unwrap());
        assert!(store.cached_references(&key).unwrap().is_none());
    }
}
