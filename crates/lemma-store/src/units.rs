//! Units: what a producer read from one input, such as a file's code facts
//! or its functions, kept under a key naming everything the result depends
//! on (the build of Lemma, the producer, the path, the input's contents),
//! so an unchanged input is never read again. One table of key to value,
//! read and written in batches.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::time::Duration;

use rusqlite::{Connection, params};

use crate::StoreError;

const SCHEMA: &str = "CREATE TABLE IF NOT EXISTS units (
    key   TEXT PRIMARY KEY,
    value BLOB NOT NULL
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
        if entries.is_empty() {
            return Ok(());
        }
        let transaction = self.connection.transaction()?;
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
