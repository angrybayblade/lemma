//! Units: what a producer read from one input, such as a file's code facts
//! or its functions, kept under a key naming everything the result depends
//! on (the build of Lemma, the producer, the path, the input's contents),
//! so an unchanged input is never read again. One table of key to value,
//! read and written in batches.
//!
//! A unit may also be indexed by terms, short strings saying what it
//! mentions (a path a file calls, say), so the units that could matter to a
//! question are found without reading them all. A unit is recorded as
//! indexed once its terms are written, so a lookup can tell an index that
//! covers a set of units from one that does not.

use std::collections::{BTreeMap, BTreeSet};
use std::path::{Path, PathBuf};
use std::sync::mpsc;
use std::thread::JoinHandle;
use std::time::Duration;

use rusqlite::{Connection, params};

use crate::StoreError;

const SCHEMA: &str = "CREATE TABLE IF NOT EXISTS units (
    key   TEXT PRIMARY KEY,
    value BLOB NOT NULL
) WITHOUT ROWID;
CREATE TABLE IF NOT EXISTS unit_terms (
    term TEXT NOT NULL,
    unit TEXT NOT NULL,
    PRIMARY KEY (term, unit)
) WITHOUT ROWID;
CREATE TABLE IF NOT EXISTS indexed_units (
    unit TEXT PRIMARY KEY
) WITHOUT ROWID;";

/// Keys looked up per query, under SQLite's limit on parameters.
const KEYS_PER_QUERY: usize = 500;

/// The page size of a new cache file.
const PAGE_SIZE_BYTES: i64 = 65_536;

/// How much of the cache file a parallel reader maps into memory rather
/// than copying through reads.
const MAPPED_BYTES: i64 = 1 << 32;

/// Units read and decoded at a time by [`UnitCache::get_decoded`].
const DECODED_PER_CHUNK: usize = 4096;

/// A cache of units in one SQLite file. Several Lemma processes may use
/// one at a time: writers wait for each other rather than fail.
pub struct UnitCache {
    path: PathBuf,
    connection: Connection,
    /// Writes units handed to [`UnitCache::put_later`]; started on first
    /// use.
    writer: Option<Writer>,
}

/// A thread writing batches of units on its own connection, so a caller
/// carries on while they are written and synced to disk.
struct Writer {
    batches: mpsc::Sender<Batch>,
    thread: JoinHandle<()>,
}

/// What the writer thread writes, each in one transaction.
enum Batch {
    /// Units, by key.
    Units(Vec<(String, Vec<u8>)>),
    /// Each unit's terms, marking the unit indexed.
    Terms(Vec<(String, Vec<String>)>),
}

impl UnitCache {
    /// Opens the cache at `path`, creating it if needed.
    pub fn open(path: &Path) -> Result<Self, StoreError> {
        let connection = Connection::open(path)?;
        connection.busy_timeout(Duration::from_secs(60))?;
        // Units are a few kilobytes each: with SQLite's default 4 KiB pages
        // most spill onto overflow pages, and reading followed those
        // chains. The page size only takes effect on a new file, before
        // the write-ahead log is turned on.
        connection.pragma_update(None, "page_size", PAGE_SIZE_BYTES)?;
        connection.pragma_update(None, "journal_mode", "WAL")?;
        connection.pragma_update(None, "synchronous", "NORMAL")?;
        connection.execute_batch(SCHEMA)?;
        Ok(Self {
            path: path.to_owned(),
            connection,
            writer: None,
        })
    }

    /// The values kept for those of `keys` the cache holds.
    pub fn get_many(&self, keys: &[String]) -> Result<BTreeMap<String, Vec<u8>>, StoreError> {
        select_many(&self.connection, keys)
    }

    /// The units kept under those of `keys` the cache holds, each turned
    /// into a value by `decode` in parallel. Read and decoded a chunk at a
    /// time, so the encoded units of a large repository are never all held
    /// at once beside the decoded ones. A unit `decode` rejects is left
    /// out, as if it were not kept.
    pub fn get_decoded<T: Send>(
        &self,
        keys: &[String],
        decode: impl Fn(&[u8]) -> Option<T> + Sync,
    ) -> Result<BTreeMap<String, T>, StoreError> {
        use rayon::prelude::*;
        // Chunks are read in parallel too, each reader with its own
        // connection: with the cache's write-ahead log, readers do not wait
        // for each other.
        let path = &self.path;
        let chunks: Vec<Result<Vec<(String, T)>, StoreError>> = keys
            .par_chunks(DECODED_PER_CHUNK)
            .map(|chunk| {
                let connection = Connection::open(path)?;
                connection.busy_timeout(Duration::from_secs(60))?;
                connection.pragma_update(None, "mmap_size", MAPPED_BYTES)?;
                let found = select_many(&connection, chunk)?;
                Ok(found
                    .into_iter()
                    .filter_map(|(key, bytes)| Some((key, decode(&bytes)?)))
                    .collect())
            })
            .collect();
        let mut decoded = BTreeMap::new();
        for chunk in chunks {
            decoded.extend(chunk?);
        }
        Ok(decoded)
    }

    /// Keeps `entries`, in one transaction.
    pub fn put_many(&mut self, entries: &[(String, Vec<u8>)]) -> Result<(), StoreError> {
        insert_many(&mut self.connection, entries)
    }

    /// Keeps `entries` in the background, in one transaction, while the
    /// caller carries on; dropping the cache waits for every batch to be
    /// written. Keeping is best effort: a batch that cannot be written
    /// only costs a later run the work of producing it again.
    pub fn put_later(&mut self, entries: Vec<(String, Vec<u8>)>) {
        if entries.is_empty() {
            return;
        }
        self.send(Batch::Units(entries));
    }

    /// Indexes each unit by its terms in the background, as
    /// [`UnitCache::put_later`] keeps units, recording it as indexed even
    /// with no terms.
    pub fn put_terms_later(&mut self, terms: Vec<(String, Vec<String>)>) {
        if terms.is_empty() {
            return;
        }
        self.send(Batch::Terms(terms));
    }

    fn send(&mut self, batch: Batch) {
        let path = &self.path;
        let writer = self.writer.get_or_insert_with(|| start_writer(path));
        // A writer that has stopped drops what it is sent, as it would
        // fail to write it.
        let _ = writer.batches.send(batch);
    }

    /// Those of `units` whose terms are written.
    pub fn indexed(&self, units: &[String]) -> Result<BTreeSet<String>, StoreError> {
        select_column(
            &self.connection,
            "SELECT unit FROM indexed_units WHERE unit IN",
            units,
        )
    }

    /// The units indexed by any of `terms`.
    pub fn units_with(&self, terms: &[String]) -> Result<BTreeSet<String>, StoreError> {
        select_column(
            &self.connection,
            "SELECT unit FROM unit_terms WHERE term IN",
            terms,
        )
    }
}

impl Drop for UnitCache {
    fn drop(&mut self) {
        let Some(Writer { batches, thread }) = self.writer.take() else {
            return;
        };
        drop(batches);
        // The writer only ends early by panicking, which loses no more
        // than its unwritten batches.
        let _ = thread.join();
    }
}

/// Starts a thread writing the batches it is sent to the cache at `path`.
fn start_writer(path: &Path) -> Writer {
    let (batches, received) = mpsc::channel::<Batch>();
    let path = path.to_owned();
    let thread = std::thread::spawn(move || {
        let Ok(mut connection) = open_writer(&path) else {
            return;
        };
        for batch in received {
            // Best effort, as `put_later` says.
            let _ = match batch {
                Batch::Units(entries) => insert_many(&mut connection, &entries),
                Batch::Terms(terms) => insert_terms(&mut connection, &terms),
            };
        }
    });
    Writer { batches, thread }
}

/// Writes each unit's terms and marks it indexed, in one transaction.
fn insert_terms(
    connection: &mut Connection,
    terms: &[(String, Vec<String>)],
) -> Result<(), StoreError> {
    let transaction = connection.transaction()?;
    {
        let mut term = transaction
            .prepare_cached("INSERT OR IGNORE INTO unit_terms (term, unit) VALUES (?1, ?2)")?;
        let mut indexed =
            transaction.prepare_cached("INSERT OR IGNORE INTO indexed_units (unit) VALUES (?1)")?;
        for (unit, unit_terms) in terms {
            for text in unit_terms {
                term.execute(params![text, unit])?;
            }
            indexed.execute(params![unit])?;
        }
    }
    transaction.commit()?;
    Ok(())
}

/// The values of a one-column `query` (`SELECT x FROM t WHERE y IN`) over
/// `values`, a batch of them per statement.
fn select_column(
    connection: &Connection,
    query: &str,
    values: &[String],
) -> Result<BTreeSet<String>, StoreError> {
    let mut found = BTreeSet::new();
    for batch in values.chunks(KEYS_PER_QUERY) {
        let placeholders = vec!["?"; batch.len()].join(",");
        let mut statement = connection.prepare_cached(&format!("{query} ({placeholders})"))?;
        let rows = statement.query_map(rusqlite::params_from_iter(batch), |row| row.get(0))?;
        for row in rows {
            found.insert(row?);
        }
    }
    Ok(found)
}

/// A second connection to the cache at `path`, for writing.
fn open_writer(path: &Path) -> Result<Connection, StoreError> {
    let connection = Connection::open(path)?;
    connection.busy_timeout(Duration::from_secs(60))?;
    connection.pragma_update(None, "synchronous", "NORMAL")?;
    Ok(connection)
}

/// Writes `entries` on `connection` in one transaction.
fn insert_many(
    connection: &mut Connection,
    entries: &[(String, Vec<u8>)],
) -> Result<(), StoreError> {
    if entries.is_empty() {
        return Ok(());
    }
    let transaction = connection.transaction()?;
    {
        let mut statement = transaction
            .prepare_cached("INSERT OR REPLACE INTO units (key, value) VALUES (?1, ?2)")?;
        for (key, value) in entries {
            statement.execute(params![key, value])?;
        }
    }
    transaction.commit()?;
    Ok(())
}

/// The values `connection`'s cache holds under those of `keys`.
fn select_many(
    connection: &Connection,
    keys: &[String],
) -> Result<BTreeMap<String, Vec<u8>>, StoreError> {
    let mut found = BTreeMap::new();
    // One query per batch of keys rather than per key: at 100k files
    // the round trips, not the lookups, were the cost.
    for batch in keys.chunks(KEYS_PER_QUERY) {
        let placeholders = vec!["?"; batch.len()].join(",");
        let mut statement = connection.prepare_cached(&format!(
            "SELECT key, value FROM units WHERE key IN ({placeholders})"
        ))?;
        let rows = statement.query_map(rusqlite::params_from_iter(batch), |row| {
            Ok((row.get::<_, String>(0)?, row.get::<_, Vec<u8>>(1)?))
        })?;
        for row in rows {
            let (key, value) = row?;
            found.insert(key, value);
        }
    }
    Ok(found)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn units_kept_later_are_written_by_the_time_the_cache_is_dropped() {
        let directory = tempfile_directory("later");
        let path = directory.join("units.db");
        let mut cache = UnitCache::open(&path).unwrap();

        cache.put_later(vec![("a".to_owned(), b"one".to_vec())]);
        drop(cache);
        let found = UnitCache::open(&path)
            .unwrap()
            .get_many(&["a".to_owned()])
            .unwrap();

        assert_eq!(found, BTreeMap::from([("a".to_owned(), b"one".to_vec())]));
        let _ = std::fs::remove_dir_all(&directory);
    }

    fn tempfile_directory(name: &str) -> PathBuf {
        let directory =
            std::env::temp_dir().join(format!("lemma-units-{name}-{}", std::process::id()));
        std::fs::create_dir_all(&directory).unwrap();
        directory
    }

    #[test]
    fn units_are_found_by_their_terms_once_indexed() {
        let directory = tempfile_directory("terms");
        let path = directory.join("units.db");
        let mut cache = UnitCache::open(&path).unwrap();

        cache.put_terms_later(vec![
            ("a".to_owned(), vec!["path:/v1/users".to_owned()]),
            ("b".to_owned(), Vec::new()),
        ]);
        drop(cache);
        let cache = UnitCache::open(&path).unwrap();
        let keys = ["a".to_owned(), "b".to_owned(), "c".to_owned()];
        let indexed = cache.indexed(&keys).unwrap();
        let calling = cache.units_with(&["path:/v1/users".to_owned()]).unwrap();

        assert_eq!(indexed, BTreeSet::from(["a".to_owned(), "b".to_owned()]));
        assert_eq!(calling, BTreeSet::from(["a".to_owned()]));
        let _ = std::fs::remove_dir_all(&directory);
    }

    #[test]
    fn kept_units_are_found_by_key_and_others_are_not() {
        let directory = std::env::temp_dir().join(format!("lemma-units-{}", std::process::id()));
        std::fs::create_dir_all(&directory).unwrap();
        let path = directory.join("units.db");
        let mut cache = UnitCache::open(&path).unwrap();

        cache
            .put_many(&[
                ("a".to_owned(), b"one".to_vec()),
                ("b".to_owned(), b"two".to_vec()),
            ])
            .unwrap();
        let found = cache
            .get_many(&["a".to_owned(), "missing".to_owned()])
            .unwrap();
        let decoded = cache
            .get_decoded(&["a".to_owned(), "b".to_owned()], |bytes| {
                (bytes != b"two").then_some(bytes.len())
            })
            .unwrap();

        assert_eq!(found, BTreeMap::from([("a".to_owned(), b"one".to_vec())]));
        assert_eq!(decoded, BTreeMap::from([("a".to_owned(), 3)]));
        drop(cache);
        let _ = std::fs::remove_dir_all(&directory);
    }
}
