//! Segments (spec 12): immutable objects built from many units at once,
//! such as a repository's functions with their names interned, kept as
//! files named by a digest of everything they were built from. A run that
//! reads the same inputs again reads one file instead of every unit.
//!
//! A segment is written to a temporary name and renamed into place, so a
//! reader never sees part of one, and it is never rewritten: the same name
//! always holds the same bytes. Each segment belongs to a group, such as
//! one repository's functions; only the most recently used few of a group
//! are kept, since older ones are for commits a run is unlikely to read
//! again. A segment removed while wanted is rebuilt from its units, never
//! wrong.

use std::path::{Path, PathBuf};
use std::time::{Duration, SystemTime};

use crate::StoreError;

/// How many segments of one group are kept.
const KEPT_PER_GROUP: usize = 4;

/// How long a segment of any group is kept unused: a group whose
/// repository is not checked again, such as a CI workspace or a removed
/// worktree, is otherwise never tidied.
const UNUSED_FOR: Duration = Duration::from_secs(14 * 24 * 60 * 60);

/// How long a temporary file is left before it is taken for one a
/// stopped writer left behind.
const PARTIAL_FOR: Duration = Duration::from_secs(60 * 60);

/// Segments in one directory.
pub struct SegmentStore {
    directory: PathBuf,
}

/// What names a segment: its group, and a digest of its inputs. Both are
/// hexadecimal digests, so they are safe as file names.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SegmentKey {
    /// What the segment is of, such as one repository's functions.
    pub group: String,
    /// A digest of everything the segment was built from.
    pub inputs: String,
}

impl SegmentKey {
    fn file_name(&self) -> String {
        format!("{}-{}", self.group, self.inputs)
    }
}

impl SegmentStore {
    /// The segments kept in `directory`, created when the first is put.
    pub fn new(directory: &Path) -> Self {
        Self {
            directory: directory.to_owned(),
        }
    }

    /// The segment kept under `key`, if there is one. Marks it used, so it
    /// outlives its group's older segments.
    pub fn get(&self, key: &SegmentKey) -> Result<Option<Vec<u8>>, StoreError> {
        let path = self.directory.join(key.file_name());
        let bytes = match std::fs::read(&path) {
            Ok(bytes) => bytes,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(None),
            Err(error) => return Err(error.into()),
        };
        // Marking is tidying: a segment whose mark fails is only removed
        // sooner.
        let _ = touch(&path);
        Ok(Some(bytes))
    }

    /// Keeps `bytes` under `key`, then removes the group's segments beyond
    /// the most recently used few.
    pub fn put(&self, key: &SegmentKey, bytes: &[u8]) -> Result<(), StoreError> {
        std::fs::create_dir_all(&self.directory)?;
        let name = key.file_name();
        // Named for this process and thread, so two writers of the same
        // segment never write one temporary file.
        let writer = format!("{}-{:?}", std::process::id(), std::thread::current().id());
        let temporary = self.directory.join(format!(".{name}.{writer}.partial"));
        std::fs::write(&temporary, bytes)?;
        std::fs::rename(&temporary, self.directory.join(&name))?;
        self.remove_older(&key.group);
        Ok(())
    }

    /// Removes all but the group's most recently used segments, any
    /// segment unused for long, and temporary files writers left behind.
    /// Removing is tidying, so a failure leaves them for the next put.
    fn remove_older(&self, group: &str) {
        let Ok(entries) = std::fs::read_dir(&self.directory) else {
            return;
        };
        let now = SystemTime::now();
        let prefix = format!("{group}-");
        let mut in_group: Vec<(SystemTime, PathBuf)> = Vec::new();
        for entry in entries.filter_map(Result::ok) {
            let name = entry.file_name().to_string_lossy().into_owned();
            let used = entry
                .metadata()
                .and_then(|metadata| metadata.modified())
                .unwrap_or(SystemTime::UNIX_EPOCH);
            let age = now.duration_since(used).unwrap_or_default();
            let limit = if name.starts_with('.') {
                PARTIAL_FOR
            } else {
                UNUSED_FOR
            };
            if age > limit {
                let _ = std::fs::remove_file(entry.path());
            } else if name.starts_with(&prefix) {
                in_group.push((used, entry.path()));
            }
        }
        in_group.sort_by(|left, right| right.cmp(left));
        for (_, path) in in_group.into_iter().skip(KEPT_PER_GROUP) {
            let _ = std::fs::remove_file(path);
        }
    }
}

/// Sets `path`'s modification time to now.
fn touch(path: &Path) -> std::io::Result<()> {
    std::fs::File::options()
        .append(true)
        .open(path)?
        .set_modified(SystemTime::now())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn directory(name: &str) -> PathBuf {
        let directory =
            std::env::temp_dir().join(format!("lemma-segments-{name}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&directory);
        directory
    }

    fn key(group: &str, inputs: &str) -> SegmentKey {
        SegmentKey {
            group: group.to_owned(),
            inputs: inputs.to_owned(),
        }
    }

    #[test]
    fn a_kept_segment_is_found_by_its_key_and_others_are_not() {
        let directory = directory("found");
        let store = SegmentStore::new(&directory);

        store.put(&key("aa", "11"), b"segment").unwrap();

        assert_eq!(
            store.get(&key("aa", "11")).unwrap(),
            Some(b"segment".to_vec())
        );
        assert_eq!(store.get(&key("aa", "22")).unwrap(), None);
        let _ = std::fs::remove_dir_all(&directory);
    }

    #[test]
    fn segments_long_unused_and_stale_temporary_files_are_removed() {
        let directory = directory("stale");
        let store = SegmentStore::new(&directory);
        store.put(&key("aa", "0"), b"old").unwrap();
        let stale = directory.join(".bb-1.9.partial");
        std::fs::write(&stale, b"part").unwrap();
        for (path, age) in [
            (directory.join(key("aa", "0").file_name()), UNUSED_FOR),
            (stale.clone(), PARTIAL_FOR),
        ] {
            let then = SystemTime::now() - age - Duration::from_secs(60);
            let file = std::fs::File::options().append(true).open(path).unwrap();
            file.set_modified(then).unwrap();
        }

        store.put(&key("cc", "0"), b"new").unwrap();

        assert_eq!(store.get(&key("aa", "0")).unwrap(), None);
        assert!(!stale.exists());
        assert!(store.get(&key("cc", "0")).unwrap().is_some());
        let _ = std::fs::remove_dir_all(&directory);
    }

    #[test]
    fn only_a_groups_most_recent_segments_are_kept() {
        let directory = directory("kept");
        let store = SegmentStore::new(&directory);

        for number in 0..=KEPT_PER_GROUP {
            store.put(&key("aa", &number.to_string()), b"x").unwrap();
            let path = directory.join(key("aa", &number.to_string()).file_name());
            let age = std::time::Duration::from_secs(100 - number as u64);
            std::fs::File::options()
                .append(true)
                .open(path)
                .unwrap()
                .set_modified(SystemTime::now() - age)
                .unwrap();
        }
        store.put(&key("bb", "0"), b"y").unwrap();

        assert_eq!(store.get(&key("aa", "0")).unwrap(), None);
        assert!(store.get(&key("aa", "1")).unwrap().is_some());
        assert!(store.get(&key("bb", "0")).unwrap().is_some());
        let _ = std::fs::remove_dir_all(&directory);
    }
}
