//! Units: what a producer read from one input, such as a file's code facts
//! or its functions, kept under a key naming everything the result depends
//! on (the build of Lemma, the producer, the path, the input's contents),
//! so an unchanged input is never read again. One table of key to value,
//! read and written in batches.

use std::collections::BTreeMap;
use std::path::Path;
use std::time::Duration;

use rusqlite::{Connection, params};

use crate::StoreError;

const SCHEMA: &str = "CREATE TABLE IF NOT EXISTS units (
    key   TEXT PRIMARY KEY,
    value BLOB NOT NULL
) WITHOUT ROWID;";

/// A cache of units in one SQLite file. Several Lemma processes may use
/// one at a time: writers wait for each other rather than fail.
pub struct UnitCache {
    connection: Connection,
}

impl UnitCache {
    /// Opens the cache at `path`, creating it if needed.
    pub fn open(path: &Path) -> Result<Self, StoreError> {
        let connection = Connection::open(path)?;
        connection.busy_timeout(Duration::from_secs(60))?;
        connection.pragma_update(None, "journal_mode", "WAL")?;
        connection.pragma_update(None, "synchronous", "NORMAL")?;
        connection.execute_batch(SCHEMA)?;
        Ok(Self { connection })
    }

    /// The values kept for those of `keys` the cache holds.
    pub fn get_many(&self, keys: &[String]) -> Result<BTreeMap<String, Vec<u8>>, StoreError> {
        let mut statement = self
            .connection
            .prepare_cached("SELECT value FROM units WHERE key = ?1")?;
        let mut found = BTreeMap::new();
        for key in keys {
            let value: Option<Vec<u8>> = statement
                .query_row(params![key], |row| row.get(0))
                .map(Some)
                .or_else(|error| match error {
                    rusqlite::Error::QueryReturnedNoRows => Ok(None),
                    other => Err(other),
                })?;
            if let Some(value) = value {
                found.insert(key.clone(), value);
            }
        }
        Ok(found)
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

        assert_eq!(found, BTreeMap::from([("a".to_owned(), b"one".to_vec())]));
        drop(cache);
        let _ = std::fs::remove_dir_all(&directory);
    }
}
