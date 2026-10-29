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
pub const SCHEMA_VERSION: &str = "1";

const SCHEMA: &str = include_str!("schema.sql");

/// Why a storage operation failed.
#[derive(Debug, thiserror::Error)]
pub enum StoreError {
    /// SQLite reported an error.
    #[error("database error")]
    Sqlite(#[from] rusqlite::Error),
    /// The database was written by a different version of Lemma.
    #[error("index uses schema version {found}, expected {SCHEMA_VERSION}; delete it and re-index")]
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
    pub fn replace_snapshot(
        &mut self,
        snapshot: &Snapshot,
        extractor_version: &str,
        files: &[IndexedFile],
    ) -> Result<i64, StoreError> {
        let transaction = self.connection.transaction()?;
        transaction.execute("DELETE FROM snapshots WHERE root = ?1", [&snapshot.root])?;
        transaction.execute(
            "INSERT INTO snapshots (root, revision, is_dirty, fingerprint, extractor_version)
             VALUES (?1, ?2, ?3, ?4, ?5)",
            params![
                snapshot.root,
                snapshot.revision,
                snapshot.is_dirty,
                snapshot.fingerprint,
                extractor_version
            ],
        )?;
        let snapshot_id = transaction.last_insert_rowid();
        insert_files(&transaction, snapshot_id, files)?;
        transaction.commit()?;
        Ok(snapshot_id)
    }

    /// The most recently stored snapshot.
    pub fn latest_snapshot(&self) -> Result<Option<StoredSnapshot>, StoreError> {
        let snapshot = self
            .connection
            .query_row(
                "SELECT id, root, revision, is_dirty, fingerprint, extractor_version
                 FROM snapshots ORDER BY id DESC LIMIT 1",
                [],
                |row| {
                    Ok(StoredSnapshot {
                        id: row.get(0)?,
                        extractor_version: row.get(5)?,
                        snapshot: Snapshot {
                            root: row.get(1)?,
                            revision: row.get(2)?,
                            is_dirty: row.get(3)?,
                            fingerprint: row.get(4)?,
                        },
                    })
                },
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
        let mut statement = self.connection.prepare_cached(
            r"SELECT s.id, f.path, f.language, s.name, s.qualified_name, s.kind,
                     s.start_line, s.start_column, s.end_line, s.end_column,
                     s.name_start_line, s.name_start_column, s.name_end_line, s.name_end_column
              FROM symbols s JOIN files f ON f.id = s.file_id
              WHERE f.snapshot_id = ?1
                AND CASE WHEN ?2 THEN (s.name = ?3 OR s.qualified_name = ?3)
                         ELSE (s.name LIKE ?4 ESCAPE '\' OR s.qualified_name LIKE ?4 ESCAPE '\')
                    END
                AND (?5 IS NULL OR s.kind = ?5)
                AND (?6 IS NULL OR f.language = ?6)
                AND (?7 IS NULL OR instr(f.path, ?7) > 0)
              ORDER BY (s.name = ?3 OR s.qualified_name = ?3) DESC,
                       length(s.qualified_name), f.path, s.start_line
              LIMIT ?8",
        )?;
        let rows = statement.query_map(
            params![
                snapshot_id,
                filter.name_match == NameMatch::Exact,
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

    /// One symbol by id, if it belongs to the snapshot.
    pub fn symbol(
        &self,
        snapshot_id: i64,
        symbol_id: i64,
    ) -> Result<Option<StoredSymbol>, StoreError> {
        let symbol = self
            .connection
            .query_row(
                "SELECT s.id, f.path, f.language, s.name, s.qualified_name, s.kind,
                        s.start_line, s.start_column, s.end_line, s.end_column,
                        s.name_start_line, s.name_start_column, s.name_end_line, s.name_end_column
                 FROM symbols s JOIN files f ON f.id = s.file_id
                 WHERE f.snapshot_id = ?1 AND s.id = ?2",
                params![snapshot_id, symbol_id],
                read_stored_symbol,
            )
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
    files: &[IndexedFile],
) -> Result<(), StoreError> {
    let mut insert_file = transaction.prepare_cached(
        "INSERT INTO files (snapshot_id, path, language, content_hash, has_syntax_errors)
         VALUES (?1, ?2, ?3, ?4, ?5)",
    )?;
    let mut insert_symbol = transaction.prepare_cached(
        "INSERT INTO symbols (file_id, name, qualified_name, kind,
                              start_line, start_column, end_line, end_column,
                              name_start_line, name_start_column, name_end_line, name_end_column)
         VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12)",
    )?;

    for file in files {
        insert_file.execute(params![
            snapshot_id,
            file.path,
            file.language.as_str(),
            file.content_hash,
            file.has_syntax_errors
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

    fn sample_files() -> Vec<IndexedFile> {
        vec![IndexedFile {
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
        }]
    }

    fn store_with_sample() -> (Store, i64) {
        let mut store = Store::open_in_memory().unwrap();
        let snapshot_id = store
            .replace_snapshot(&snapshot("/repo", "fp-1"), "1", &sample_files())
            .unwrap();
        (store, snapshot_id)
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

        let second_id = store
            .replace_snapshot(&snapshot("/repo", "fp-2"), "1", &sample_files())
            .unwrap();

        assert_ne!(first_id, second_id);
        assert_eq!(
            store
                .find_symbols(first_id, &filter("delete", NameMatch::Contains))
                .unwrap(),
            [] as [StoredSymbol; 0]
        );
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
