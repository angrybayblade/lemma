//! The state of paths now, which read sets are compared with (spec 13,
//! section 10): a path is absent, a directory, a listing with a digest of
//! its entries' names, or a file with a digest of its contents.
//!
//! Reading every file a read set names on every check would cost more than
//! the checks it saves. So a digest is remembered with what tells, without
//! reading the file, that it has not changed since: its size, its times
//! and its inode, as Git's index does. A file is read again when any of
//! those differs, or when the digest was taken within two seconds of the
//! file's last change, since a change that close may not show in its times.

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex, MutexGuard, PoisonError};
use std::time::{SystemTime, UNIX_EPOCH};

/// A path that is not there.
pub(crate) const ABSENT: &str = "-";
/// A directory that is there, whatever it holds.
pub(crate) const DIRECTORY: &str = "d";

/// How long before a digest is taken a file must have last changed for its
/// times to be trusted to show the next change.
const SETTLED_NS: u128 = 2_000_000_000;

/// What tells that a file or directory is as it was: size, time of last
/// change to its contents and to its metadata, inode and device.
type Identity = [u128; 5];

/// A digest taken before, with the file's identity then.
#[derive(Debug, Clone, PartialEq, Eq)]
struct Remembered {
    identity: Identity,
    taken_ns: u128,
    state: String,
}

impl Remembered {
    /// Whether the digest still stands for a file of `identity`: nothing
    /// that identifies it differs, and when the digest was taken the file
    /// had not changed for `settled_ns`.
    fn stands_for(&self, identity: &Identity, settled_ns: u128) -> bool {
        let last_change = self.identity[1].max(self.identity[2]);
        self.identity == *identity && last_change + settled_ns < self.taken_ns
    }
}

/// Digests remembered between checks, for each path as a file and as a
/// listing.
#[derive(Default)]
struct Memory {
    known: HashMap<String, [Option<Remembered>; 2]>,
    is_changed: bool,
}

/// The state of paths now. Each is found once for as long as no checker
/// runs: many cases read the same files, and a run may write any of them.
pub(crate) struct Current {
    memory: Arc<Mutex<Memory>>,
    /// How long a file must have gone unchanged when its digest is taken.
    settled_ns: u128,
    /// Where digests are remembered between checks, if anywhere.
    kept_at: Option<PathBuf>,
    /// For each path, its state and its state as a listing, once found.
    found: Mutex<HashMap<String, [Option<String>; 2]>>,
    /// Whether a part of a read set, read from a directory, holds there.
    parts: Mutex<HashMap<(String, String), bool>>,
}

impl Default for Current {
    fn default() -> Self {
        Self {
            memory: Arc::default(),
            settled_ns: SETTLED_NS,
            kept_at: None,
            found: Mutex::default(),
            parts: Mutex::default(),
        }
    }
}

fn locked<T>(mutex: &Mutex<T>) -> MutexGuard<'_, T> {
    mutex.lock().unwrap_or_else(PoisonError::into_inner)
}

impl Current {
    /// States found with the digests remembered in `file`, which
    /// [`Current::remember`] brings up to date.
    pub(crate) fn remembering(file: PathBuf) -> Self {
        let known = std::fs::read_to_string(&file)
            .map(|text| read_memory(&text))
            .unwrap_or_default();
        let memory = Memory {
            known,
            is_changed: false,
        };
        Self {
            memory: Arc::new(Mutex::new(memory)),
            kept_at: Some(file),
            ..Self::default()
        }
    }

    /// The same remembered digests, with nothing found yet: for states
    /// that must be those of this moment, as after a checker has run.
    pub(crate) fn afresh(&self) -> Self {
        Self {
            memory: Arc::clone(&self.memory),
            settled_ns: self.settled_ns,
            ..Self::default()
        }
    }

    /// Forgets every state found: a checker ran, and may have changed any
    /// path. Remembered digests stay, each still checked against its file.
    pub(crate) fn forget(&self) {
        locked(&self.found).clear();
        locked(&self.parts).clear();
    }

    /// Writes down the digests taken since they were read, if any were. A
    /// failure only means files are read again, so it is logged.
    pub(crate) fn remember(&self) {
        let Some(file) = &self.kept_at else {
            return;
        };
        let mut memory = locked(&self.memory);
        if !memory.is_changed {
            return;
        }
        let beside = file.with_extension(format!("{}.tmp", std::process::id()));
        let written = std::fs::write(&beside, write_memory(&memory.known))
            .and_then(|()| std::fs::rename(&beside, file));
        match written {
            Ok(()) => memory.is_changed = false,
            Err(error) => log::warn!("digests were not remembered: {error}"),
        }
    }

    /// The state of `path`: absent, a directory (with a digest of its
    /// entries' names when `is_listed`), or a file with a digest of its
    /// contents.
    pub(crate) fn state(&self, path: &str, is_listed: bool) -> String {
        let index = usize::from(is_listed);
        let found = locked(&self.found)
            .get(path)
            .and_then(|states| states[index].clone());
        if let Some(found) = found {
            return found;
        }
        let state = self.state_now(path, is_listed);
        locked(&self.found).entry(path.to_owned()).or_default()[index] = Some(state.clone());
        state
    }

    /// The state of `path` from the file system, with its digest taken
    /// only if the one remembered no longer stands.
    fn state_now(&self, path: &str, is_listed: bool) -> String {
        let Ok(metadata) = std::fs::metadata(path) else {
            return ABSENT.to_owned();
        };
        if metadata.is_dir() && !is_listed {
            return DIRECTORY.to_owned();
        }
        let index = usize::from(metadata.is_dir());
        let identity = identity(&metadata);
        let remembered = locked(&self.memory)
            .known
            .get(path)
            .and_then(|kinds| kinds[index].clone());
        if let (Some(remembered), Some(identity)) = (&remembered, &identity) {
            if remembered.stands_for(identity, self.settled_ns) {
                return remembered.state.clone();
            }
        }
        let taken_ns = now_ns();
        let state = if metadata.is_dir() {
            listing(Path::new(path))
        } else {
            contents(Path::new(path))
        };
        if let Some(identity) = identity {
            let mut memory = locked(&self.memory);
            memory.known.entry(path.to_owned()).or_default()[index] = Some(Remembered {
                identity,
                taken_ns,
                state: state.clone(),
            });
            memory.is_changed = true;
        }
        state
    }

    /// Whether the part named `digest` is known to hold, or not to, for
    /// the paths under `directory`.
    pub(crate) fn part(&self, directory: &str, digest: &str) -> Option<bool> {
        let key = (directory.to_owned(), digest.to_owned());
        locked(&self.parts).get(&key).copied()
    }

    /// Keeps whether a part holds for the paths under `directory`.
    pub(crate) fn found_part(&self, directory: &str, digest: &str, holds: bool) {
        let key = (directory.to_owned(), digest.to_owned());
        locked(&self.parts).insert(key, holds);
    }
}

/// Whether a file was written, replaced or renamed into place after
/// `started`: its contents' time or, where the system keeps one, the time
/// its metadata last changed, which moving an older file there also sets.
pub(crate) fn changed_since(path: &Path, started: SystemTime) -> bool {
    let Ok(metadata) = std::fs::metadata(path) else {
        return false;
    };
    let started_ns = started
        .duration_since(UNIX_EPOCH)
        .map_or(0, |since| since.as_nanos());
    match identity(&metadata) {
        Some(identity) => identity[1].max(identity[2]) > started_ns,
        None => metadata.modified().is_ok_and(|modified| modified > started),
    }
}

fn now_ns() -> u128 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0, |since| since.as_nanos())
}

/// A file's state: a digest of its contents.
fn contents(path: &Path) -> String {
    match std::fs::read(path) {
        Ok(contents) => format!("f:{}", lemma_model::content_hash(&contents)),
        Err(_) => ABSENT.to_owned(),
    }
}

/// A listing's state: a digest of its entries' names.
fn listing(path: &Path) -> String {
    let Ok(entries) = std::fs::read_dir(path) else {
        return ABSENT.to_owned();
    };
    let mut names: Vec<Vec<u8>> = entries
        .filter_map(Result::ok)
        .map(|entry| entry.file_name().as_encoded_bytes().to_vec())
        .collect();
    names.sort();
    format!("l:{}", lemma_model::content_hash(&names.join(&0u8)))
}

/// What identifies a file as unchanged, where the system says: none
/// elsewhere, so every digest is taken again.
#[cfg(unix)]
fn identity(metadata: &std::fs::Metadata) -> Option<Identity> {
    use std::os::unix::fs::MetadataExt;
    let nanoseconds = |seconds: i64, rest: i64| {
        let seconds = u128::try_from(seconds).ok()?;
        Some(seconds * 1_000_000_000 + u128::try_from(rest).ok()?)
    };
    Some([
        u128::from(metadata.size()),
        nanoseconds(metadata.mtime(), metadata.mtime_nsec())?,
        nanoseconds(metadata.ctime(), metadata.ctime_nsec())?,
        u128::from(metadata.ino()),
        u128::from(metadata.dev()),
    ])
}

#[cfg(not(unix))]
fn identity(_metadata: &std::fs::Metadata) -> Option<Identity> {
    None
}

/// Remembered digests as text: a line for each, the path last.
fn write_memory(known: &HashMap<String, [Option<Remembered>; 2]>) -> String {
    let mut paths: Vec<&String> = known.keys().collect();
    paths.sort();
    let mut text = String::new();
    for path in paths {
        for remembered in known[path].iter().flatten() {
            let numbers: Vec<String> = remembered
                .identity
                .iter()
                .chain([&remembered.taken_ns])
                .map(u128::to_string)
                .collect();
            text.push_str(&numbers.join("\t"));
            text.push('\t');
            text.push_str(&remembered.state);
            text.push('\t');
            text.push_str(&path.replace('\\', "\\\\").replace('\n', "\\n"));
            text.push('\n');
        }
    }
    text
}

/// The digests a text remembers; a line that does not read is passed over.
fn read_memory(text: &str) -> HashMap<String, [Option<Remembered>; 2]> {
    let mut known: HashMap<String, [Option<Remembered>; 2]> = HashMap::new();
    for line in text.lines() {
        let fields: Vec<&str> = line.splitn(8, '\t').collect();
        let [numbers @ .., state, path] = fields.as_slice() else {
            continue;
        };
        let numbers: Vec<u128> = numbers.iter().filter_map(|n| n.parse().ok()).collect();
        let Ok([size, modified, changed, inode, device, taken_ns]) = <[u128; 6]>::try_from(numbers)
        else {
            continue;
        };
        let path = path.replace("\\n", "\n").replace("\\\\", "\\");
        let index = usize::from(state.starts_with("l:"));
        known.entry(path).or_default()[index] = Some(Remembered {
            identity: [size, modified, changed, inode, device],
            taken_ns,
            state: (*state).to_owned(),
        });
    }
    known
}

#[cfg(test)]
mod tests {
    use super::*;

    fn write(path: &Path, text: &str) {
        std::fs::write(path, text).unwrap();
    }

    #[test]
    fn a_path_is_absent_a_directory_a_listing_or_a_file() {
        let directory = tempfile::tempdir().unwrap();
        let root = directory.path().to_str().unwrap();
        write(&directory.path().join("a.txt"), "a");
        let current = Current::default();

        assert_eq!(current.state(&format!("{root}/none"), false), ABSENT);
        assert_eq!(current.state(root, false), DIRECTORY);
        assert!(current.state(root, true).starts_with("l:"));
        assert!(
            current
                .state(&format!("{root}/a.txt"), false)
                .starts_with("f:")
        );
    }

    #[test]
    fn a_state_found_is_kept_until_forgotten() {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("a.txt");
        write(&path, "one");
        let current = Current::default();
        let first = current.state(path.to_str().unwrap(), false);

        write(&path, "two");
        let kept = current.state(path.to_str().unwrap(), false);
        current.forget();
        let found_again = current.state(path.to_str().unwrap(), false);

        assert_eq!(kept, first);
        assert_ne!(found_again, first);
    }

    #[cfg(unix)]
    #[test]
    fn a_remembered_digest_stands_only_for_a_settled_file_that_is_as_it_was() {
        let identity_of = |path: &Path| identity(&std::fs::metadata(path).unwrap()).unwrap();
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("a.txt");
        write(&path, "one");
        let remembered = Remembered {
            identity: identity_of(&path),
            taken_ns: now_ns() + 1,
            state: "f:one".to_owned(),
        };
        let as_it_was = identity_of(&path);
        write(&path, "two, and longer");

        // Just written, the file had not settled for two seconds; had no
        // wait been asked, the digest would stand until the file changed.
        assert!(!remembered.stands_for(&as_it_was, SETTLED_NS));
        assert!(remembered.stands_for(&as_it_was, 0));
        assert!(!remembered.stands_for(&identity_of(&path), 0));
    }

    #[cfg(unix)]
    #[test]
    fn digests_are_remembered_between_checks_and_not_taken_again() {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("a.txt");
        let memory = directory.path().join("states");
        write(&path, "one");
        let without_waiting = |file: PathBuf| Current {
            settled_ns: 0,
            ..Current::remembering(file)
        };
        let first = without_waiting(memory.clone());
        let taken = first.state(path.to_str().unwrap(), false);
        first.remember();

        // The digest is not taken again: a remembered one that says
        // something else is believed while the file is as it was.
        let text = std::fs::read_to_string(&memory).unwrap();
        std::fs::write(&memory, text.replace(&taken, "f:remembered")).unwrap();
        let second = without_waiting(memory);
        let believed = second.state(path.to_str().unwrap(), false);
        write(&path, "two, and longer");
        let after_a_change = second.afresh().state(path.to_str().unwrap(), false);

        assert_eq!(believed, "f:remembered");
        assert!(after_a_change.starts_with("f:") && after_a_change != "f:remembered");
    }

    #[test]
    fn what_is_remembered_survives_being_written_down() {
        let remembered = Remembered {
            identity: [
                5,
                1_700_000_000_000_000_000,
                1_700_000_000_000_000_001,
                42,
                7,
            ],
            taken_ns: 1_800_000_000_000_000_000,
            state: "l:abc".to_owned(),
        };
        let known = HashMap::from([("/odd\nname\\x".to_owned(), [None, Some(remembered)])]);

        assert_eq!(read_memory(&write_memory(&known)), known);
    }
}
