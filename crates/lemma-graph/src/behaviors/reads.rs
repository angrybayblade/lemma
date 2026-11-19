//! What a run read, as evidence is kept by it (spec 13, section 10): each
//! file, directory and missing path a recorder saw the run rest on, inside
//! the repositories the checker reads, with the state it was in. Evidence
//! holds for as long as every one of them is in that state.
//!
//! Paths are named by repository, never by where a clone is, so a read set
//! means the same on another machine.

use std::collections::{BTreeMap, HashMap};
use std::path::{Path, PathBuf};
use std::sync::{Mutex, PoisonError};
use std::time::SystemTime;

use serde_json::{Value as Json, json};

use crate::recorder::Recorded;

/// A path that is not there.
const ABSENT: &str = "-";
/// A directory that is there, whatever it holds.
const DIRECTORY: &str = "d";

/// Where each repository a checker reads has its files.
pub(crate) type Roots = BTreeMap<String, PathBuf>;

/// The state of paths now, each found once for as long as no checker
/// runs: many cases read the same files, and a run may write any of them.
#[derive(Default)]
pub(crate) struct Current {
    found: Mutex<HashMap<(PathBuf, bool), String>>,
}

impl Current {
    /// Forgets every state found: a checker ran, and may have changed any.
    pub(crate) fn forget(&self) {
        self.found
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .clear();
    }

    /// The state of `path`: absent, a directory (with a digest of its
    /// entries' names when `is_listed`), or a file with a digest of its
    /// contents.
    fn state(&self, path: &Path, is_listed: bool) -> String {
        let key = (path.to_path_buf(), is_listed);
        let lock = || self.found.lock().unwrap_or_else(PoisonError::into_inner);
        if let Some(found) = lock().get(&key) {
            return found.clone();
        }
        let state = state_of(path, is_listed);
        lock().insert(key, state.clone());
        state
    }
}

fn state_of(path: &Path, is_listed: bool) -> String {
    let Ok(metadata) = std::fs::metadata(path) else {
        return ABSENT.to_owned();
    };
    if !metadata.is_dir() {
        return match std::fs::read(path) {
            Ok(contents) => format!("f:{}", lemma_model::content_hash(&contents)),
            Err(_) => ABSENT.to_owned(),
        };
    }
    if !is_listed {
        return DIRECTORY.to_owned();
    }
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

/// What a run read: for each repository, each path with its state.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub(crate) struct ReadSet {
    paths: BTreeMap<String, BTreeMap<String, String>>,
}

impl ReadSet {
    /// What `recorded` shows a run rested on inside `roots`, in the state
    /// it is in now, the run having started at `started` and just ended.
    /// Refused when a file changed under the run: its state now is not
    /// what the run read.
    pub(crate) fn of(
        recorded: &Recorded,
        roots: &Roots,
        started: SystemTime,
    ) -> Result<Self, String> {
        // Found afresh: states found before the run ended may be stale.
        let current = &Current::default();
        let mut paths: BTreeMap<String, BTreeMap<String, String>> = BTreeMap::new();
        let mut keep = |path: &Path, state: String| {
            if let Some((repository, relative)) = place(path, roots) {
                paths.entry(repository).or_default().insert(relative, state);
            }
        };
        // What the run itself wrote is its product, not what it rests on.
        let rests_on = |path: &&PathBuf| !recorded.written.contains(*path);
        for path in recorded.absent.iter().filter(rests_on) {
            if place(path, roots).is_some() {
                keep(path, ABSENT.to_owned());
            }
        }
        for path in recorded.read.iter().filter(rests_on) {
            if place(path, roots).is_none() {
                continue;
            }
            let state = current.state(path, false);
            // A directory's own time changes with its entries, which a
            // listing covers; only a file's says its contents changed.
            let is_changed_file = state.starts_with("f:") && changed_since(path, started);
            if state == ABSENT || is_changed_file {
                return Err(format!("{} changed while the checker ran", path.display()));
            }
            keep(path, state);
        }
        for path in &recorded.listed {
            if place(path, roots).is_some() {
                keep(path, current.state(path, true));
            }
        }
        Ok(Self { paths })
    }

    /// Whether every path is still in the state the run read it in.
    pub(crate) fn holds(&self, roots: &Roots, current: &Current) -> bool {
        self.paths.iter().all(|(repository, paths)| {
            let Some(root) = roots.get(repository) else {
                return false;
            };
            paths.iter().all(|(path, state)| {
                let is_listed = state.starts_with("l:");
                current.state(&root.join(path), is_listed) == *state
            })
        })
    }

    /// How many paths the run rested on.
    pub(crate) fn len(&self) -> usize {
        self.paths.values().map(BTreeMap::len).sum()
    }

    pub(crate) fn to_json(&self) -> Json {
        json!(self.paths)
    }

    pub(crate) fn from_json(value: &Json) -> Option<Self> {
        serde_json::from_value(value.clone())
            .ok()
            .map(|paths| Self { paths })
    }
}

/// The paths a run both read and wrote, by the root of the repository each
/// is in: its own products, unless the repository keeps them, which only
/// Git can say.
pub(crate) fn rewritten(recorded: &Recorded, roots: &Roots) -> BTreeMap<PathBuf, Vec<String>> {
    let mut found: BTreeMap<PathBuf, Vec<String>> = BTreeMap::new();
    for path in recorded.written.intersection(&recorded.read) {
        let Some((repository, relative)) = place(path, roots) else {
            continue;
        };
        if let Some(root) = roots.get(&repository) {
            found.entry(root.clone()).or_default().push(relative);
        }
    }
    found
}

/// The repository `path` is in and its path there; none for a path outside
/// every repository, or inside one's Git directory, which holds history
/// and not code.
fn place(path: &Path, roots: &Roots) -> Option<(String, String)> {
    let (repository, relative) = roots
        .iter()
        .filter_map(|(repository, root)| Some((repository, path.strip_prefix(root).ok()?)))
        .min_by_key(|(_, relative)| relative.as_os_str().len())?;
    if relative.components().any(|part| part.as_os_str() == ".git") {
        return None;
    }
    Some((repository.clone(), relative.to_string_lossy().into_owned()))
}

/// Whether a file was written after `started`.
fn changed_since(path: &Path, started: SystemTime) -> bool {
    std::fs::metadata(path)
        .and_then(|metadata| metadata.modified())
        .is_ok_and(|modified| modified > started)
}

#[cfg(test)]
mod tests {
    use std::collections::BTreeSet;
    use std::time::Duration;

    use super::*;

    fn write(path: &Path, text: &str) {
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(path, text).unwrap();
    }

    /// A repository with two files and a directory, and a run that read
    /// one file, listed the directory and looked for a file not there.
    fn repository() -> (tempfile::TempDir, Roots, Recorded) {
        let directory = tempfile::tempdir().unwrap();
        let root = directory.path().canonicalize().unwrap();
        write(&root.join("src/used.py"), "x = 1\n");
        write(&root.join("src/unused.py"), "y = 1\n");
        write(&root.join(".git/index"), "history");
        let recorded = Recorded {
            read: BTreeSet::from([root.join("src/used.py"), root.join(".git/index")]),
            listed: BTreeSet::from([root.join("src")]),
            absent: BTreeSet::from([root.join("conftest.py")]),
            written: BTreeSet::new(),
        };
        let roots = Roots::from([("app".to_owned(), root)]);
        (directory, roots, recorded)
    }

    fn read_set(roots: &Roots, recorded: &Recorded) -> ReadSet {
        let later = SystemTime::now() + Duration::from_secs(60);
        ReadSet::of(recorded, roots, later).unwrap()
    }

    #[test]
    fn it_holds_until_a_file_the_run_read_changes() {
        let (directory, roots, recorded) = repository();
        let reads = read_set(&roots, &recorded);

        write(&directory.path().join("src/unused.py"), "y = 2\n");
        let after_unread = reads.holds(&roots, &Current::default());
        write(&directory.path().join("src/used.py"), "x = 2\n");
        let after_read = reads.holds(&roots, &Current::default());

        assert_eq!(reads.len(), 3, "{reads:?}");
        assert!(after_unread);
        assert!(!after_read);
    }

    #[test]
    fn it_stops_holding_when_a_missing_path_appears_or_a_listing_changes() {
        let (directory, roots, recorded) = repository();
        let reads = read_set(&roots, &recorded);

        write(&directory.path().join("conftest.py"), "");
        let after_appearing = reads.holds(&roots, &Current::default());
        std::fs::remove_file(directory.path().join("conftest.py")).unwrap();
        let after_removing = reads.holds(&roots, &Current::default());
        write(&directory.path().join("src/new.py"), "");
        let after_listing = reads.holds(&roots, &Current::default());

        assert!(!after_appearing);
        assert!(after_removing);
        assert!(!after_listing);
    }

    #[test]
    fn what_the_run_wrote_and_what_lies_outside_is_left_out() {
        let (directory, roots, mut recorded) = repository();
        let root = directory.path().canonicalize().unwrap();
        recorded.written.insert(root.join("src/used.py"));
        recorded.read.insert(PathBuf::from("/etc/hostname"));

        let reads = read_set(&roots, &recorded);

        assert_eq!(
            reads.to_json()["app"]
                .as_object()
                .unwrap()
                .keys()
                .collect::<Vec<_>>(),
            ["conftest.py", "src"]
        );
    }

    #[test]
    fn what_a_run_read_and_wrote_is_listed_by_repository() {
        let (directory, roots, mut recorded) = repository();
        let root = directory.path().canonicalize().unwrap();
        recorded.written.insert(root.join("src/used.py"));
        recorded.written.insert(root.join("src/made.py"));

        let found = rewritten(&recorded, &roots);

        assert_eq!(
            found,
            BTreeMap::from([(root, vec!["src/used.py".to_owned()])])
        );
    }

    #[test]
    fn a_file_changed_while_the_run_went_on_is_refused() {
        let (_directory, roots, recorded) = repository();
        let before = SystemTime::now() - Duration::from_secs(60);

        let refused = ReadSet::of(&recorded, &roots, before);

        assert!(
            refused
                .unwrap_err()
                .contains("changed while the checker ran")
        );
    }

    #[test]
    fn a_read_set_survives_being_written_down() {
        let (_directory, roots, recorded) = repository();
        let reads = read_set(&roots, &recorded);

        assert_eq!(ReadSet::from_json(&reads.to_json()), Some(reads));
    }
}
