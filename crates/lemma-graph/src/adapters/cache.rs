//! Keys for the documents adapters generate. A document depends on the
//! adapter (its configuration, its script, the runtime it runs with, and
//! this version of Lemma) and on the code it reads. Code is named by its
//! Git tree, so a commit and a clean working directory at that commit share
//! one document, and a rebase that leaves the files alone keeps it; a
//! working directory with changes adds a digest of the changed files. An
//! adapter with `inputs` names its code by those paths alone, so a change
//! elsewhere in the repository keeps its document.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex, PoisonError};

use crate::GraphError;
use crate::config::{OperationsFrom, Runtime};
use crate::repos;

/// Bumped when what adapters write, or how Lemma reads it, changes.
// 3: a document patched from its base for a change in another language,
// kept under the change's key, may be wrong; such documents are dropped.
const PROTOCOL: &str = "3";

/// The part of a key that names the adapter.
pub(super) fn adapter_key(from: &OperationsFrom, runtime: &Runtime, script: &str) -> String {
    let described = format!(
        "{PROTOCOL}\n{}\n{from:?}\n{runtime:?}\n{script}",
        env!("CARGO_PKG_VERSION")
    );
    lemma_model::content_hash(described.as_bytes())
}

/// The key of the document `adapter` generates from code in `state`.
pub(super) fn key(adapter: &str, state: &str) -> String {
    lemma_model::content_hash(format!("{adapter}\n{state}").as_bytes())
}

/// The state of a commit's code: its tree.
pub(crate) fn commit_state(root: &Path, commit: &str) -> Result<String, GraphError> {
    let tree = repos::git(root, &["rev-parse", &format!("{commit}^{{tree}}")])?;
    Ok(format!("tree {}", tree.trim()))
}

/// The state of a commit's code at `inputs`: each path's object, or its
/// absence; the whole tree when `inputs` is empty or names nothing there.
pub(crate) fn inputs_commit_state(
    root: &Path,
    commit: &str,
    inputs: &[String],
) -> Result<String, GraphError> {
    match input_objects(root, commit, inputs) {
        Some(objects) => Ok(objects_state(&objects)),
        None => commit_state(root, commit),
    }
}

/// Each input's object at `commit`, by its path; `None` for the whole
/// repository, when there are no inputs or none of them exists there.
/// Inputs naming nothing (a typo, or a directory not yet created) would
/// give every commit the same state, and a document generated at one
/// commit would be reused at all of them; the whole tree is safe.
fn input_objects(root: &Path, commit: &str, inputs: &[String]) -> Option<Vec<(String, String)>> {
    let paths = scoped(inputs);
    if paths.is_empty() {
        return None;
    }
    let mut objects = Vec::new();
    for path in paths {
        let object = repos::git(
            root,
            &[
                "rev-parse",
                "--verify",
                "--quiet",
                &format!("{commit}:{path}"),
            ],
        )
        .map_or_else(|_| "absent".to_owned(), |found| found.trim().to_owned());
        objects.push((path, object));
    }
    if objects.iter().all(|(_, object)| object == "absent") {
        log::warn!(
            "adapter: no input of {} exists at {commit}; keeping its document by the whole repository",
            inputs.join(", ")
        );
        return None;
    }
    Some(objects)
}

/// The state inputs' objects make.
fn objects_state(objects: &[(String, String)]) -> String {
    let mut state = String::from("inputs");
    for (path, object) in objects {
        state.push('\n');
        state.push_str(path);
        state.push('\t');
        state.push_str(object);
    }
    state
}

/// Inputs as repository paths, sorted and without duplicates; empty, for
/// the whole repository, when one of them is the root.
fn scoped(inputs: &[String]) -> Vec<String> {
    let mut paths = Vec::new();
    for input in inputs {
        let path = input.trim_start_matches("./").trim_end_matches('/');
        if path.is_empty() || path == "." {
            return Vec::new();
        }
        paths.push(path.to_owned());
    }
    paths.sort();
    paths.dedup();
    paths
}

/// Whether `path` is one of `inputs` or inside one.
fn is_under(path: &str, inputs: &[String]) -> bool {
    inputs.iter().any(|input| {
        path == input
            || path
                .strip_prefix(input.as_str())
                .is_some_and(|rest| rest.starts_with('/'))
    })
}

/// Lemma's own index and caches, which change on every query: counted in
/// the state, an unignored `.lemma/` would make every key new.
const LEMMA_FILES: &str = ":(exclude,glob)**/.lemma/**";

/// A working directory against the commit it is on: the commit, the state
/// of its code, the files that differ from it, untracked ones included,
/// sorted, and the state those make. [`WorkingChanges::scoped`] narrows it
/// to an adapter's inputs.
#[derive(Clone)]
pub(crate) struct WorkingChanges {
    pub(crate) commit: String,
    commit_state: String,
    pub(crate) changed: Vec<String>,
    /// The state of the code: the commit's tree, and a digest of every
    /// file that differs from it.
    pub(crate) state: String,
}

/// The working directory at `root` against its commit; `None` outside a
/// Git repository or before its first commit, where nothing can be cached.
pub(crate) fn working_changes(root: &Path) -> Result<Option<WorkingChanges>, GraphError> {
    let Ok(named) = repos::git(root, &["rev-parse", "HEAD", "HEAD^{tree}"]) else {
        return Ok(None);
    };
    let mut lines = named.lines().map(str::trim);
    let (Some(commit), Some(tree)) = (lines.next(), lines.next()) else {
        return Ok(None);
    };
    let status = repos::git(
        root,
        &[
            "status",
            "--porcelain=v1",
            "-z",
            "--untracked-files=all",
            // A submodule is a repository of its own, keyed by itself: its
            // commit counts here, but not what its working tree holds,
            // which would scan every submodule (a workspace of clones).
            "--ignore-submodules=dirty",
            "--",
            LEMMA_FILES,
        ],
    )?;
    let changed = changed_paths(&status);
    let commit_state = format!("tree {tree}");
    let state = state_of(root, &commit_state, &changed);
    Ok(Some(WorkingChanges {
        commit: commit.to_owned(),
        commit_state,
        changed,
        state,
    }))
}

impl WorkingChanges {
    /// The state of the commit's code: its tree, as [`commit_state`] gives,
    /// or its inputs' objects once [`scoped`](Self::scoped).
    pub(crate) fn commit_state(&self) -> String {
        self.commit_state.clone()
    }

    /// The same working directory seen through `inputs`: the commit's state
    /// at them, and only the changed files under them. Read from what Git
    /// already said, so it asks only for the inputs' objects.
    pub(crate) fn scoped(&self, root: &Path, inputs: &[String]) -> Self {
        let Some(objects) = input_objects(root, &self.commit, inputs) else {
            return self.clone();
        };
        let paths: Vec<String> = objects.iter().map(|(path, _)| path.clone()).collect();
        let changed: Vec<String> = self
            .changed
            .iter()
            .filter(|path| is_under(path, &paths))
            .cloned()
            .collect();
        let commit_state = objects_state(&objects);
        let state = state_of(root, &commit_state, &changed);
        Self {
            commit: self.commit.clone(),
            commit_state,
            changed,
            state,
        }
    }
}

/// The state of the code in the working directory at `root`, on a commit
/// whose code's state is `commit_state`, where `changed` differ from it:
/// that state, and a digest of each of those files.
fn state_of(root: &Path, commit_state: &str, changed: &[String]) -> String {
    let tree = commit_state.to_owned();
    if changed.is_empty() {
        return tree;
    }
    let mut described = String::new();
    for path in changed {
        let contents = match std::fs::read(root.join(path)) {
            Ok(contents) => lemma_model::content_hash(&contents),
            Err(_) => "absent".to_owned(),
        };
        described.push_str(path);
        described.push('\t');
        described.push_str(&contents);
        described.push('\n');
    }
    let digest = lemma_model::content_hash(described.as_bytes());
    format!("{tree} changes {digest}")
}

/// The state of the code in a working directory (see
/// [`WorkingChanges::state`]); `None` outside a Git repository or before
/// its first commit, where nothing can be cached.
#[cfg(test)]
pub(crate) fn working_state(root: &Path) -> Result<Option<String>, GraphError> {
    Ok(working_changes(root)?.map(|changes| changes.state))
}

/// The state of working directories' code, asked of Git once per
/// repository for everything that keys what it keeps by it: adapters'
/// documents, resolvers' outputs and checkers' evidence. A large
/// repository's takes Git a while, and one check asks it many times.
///
/// Kept by a long-lived process from check to check, a repository's is
/// dropped with [`States::forget`] when anything in its working directory
/// or its Git state changes.
#[derive(Default)]
pub struct States {
    kept: Mutex<BTreeMap<PathBuf, Place>>,
}

/// One repository's state, locked while Git is asked, so whoever asks
/// meanwhile waits for the answer. The inner `None` is for a directory
/// outside Git.
type Place = Arc<Mutex<Option<Option<Arc<WorkingChanges>>>>>;

impl States {
    /// The working directory at `root` against its commit (see
    /// [`working_changes`]).
    pub(crate) fn working(&self, root: &Path) -> Result<Option<Arc<WorkingChanges>>, GraphError> {
        let place = Arc::clone(self.locked().entry(root.to_owned()).or_default());
        let mut kept = place.lock().unwrap_or_else(PoisonError::into_inner);
        if let Some(found) = kept.as_ref() {
            return Ok(found.clone());
        }
        let found = working_changes(root)?.map(Arc::new);
        *kept = Some(found.clone());
        Ok(found)
    }

    /// The state of the code in the working directory at `root`; `None`
    /// outside a Git repository or before its first commit.
    pub(crate) fn working_state(&self, root: &Path) -> Result<Option<String>, GraphError> {
        Ok(self.working(root)?.map(|changes| changes.state.clone()))
    }

    /// Forgets the state of the working directory at `repository`, after
    /// its files or its Git state changed.
    pub fn forget(&self, repository: &Path) {
        self.locked()
            .retain(|root, _| !crate::schema::same_path(root, repository));
    }

    // A poisoned lock only means a thread asking Git panicked; the states
    // already kept are whole.
    fn locked(&self) -> std::sync::MutexGuard<'_, BTreeMap<PathBuf, Place>> {
        self.kept.lock().unwrap_or_else(PoisonError::into_inner)
    }
}

/// The paths `git status --porcelain=v1 -z` lists, sorted: each entry is
/// two status letters, a space and a path, and a rename or copy is followed
/// by its original path, which is not itself changed.
fn changed_paths(status: &str) -> Vec<String> {
    let mut paths = Vec::new();
    let mut entries = status.split('\0').filter(|entry| !entry.is_empty());
    while let Some(entry) = entries.next() {
        let Some(path) = entry.get(3..) else {
            continue;
        };
        paths.push(path.to_owned());
        if entry.starts_with('R') || entry.starts_with('C') {
            entries.next();
        }
    }
    paths.sort();
    paths.dedup();
    paths
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn renames_name_only_their_new_path() {
        let status = " M src/a.ts\0R  src/new.ts\0src/old.ts\0?? notes.md\0";

        let paths = changed_paths(status);

        assert_eq!(paths, ["notes.md", "src/a.ts", "src/new.ts"]);
    }

    fn run_git(root: &Path, args: &[&str]) {
        repos::git(root, args).expect("git runs in the test repository");
    }

    #[test]
    fn lemma_files_do_not_change_the_working_state() {
        let directory = tempfile::tempdir().expect("a temporary directory");
        let root = directory.path();
        run_git(root, &["init", "-q"]);
        std::fs::write(root.join("main.go"), "package main\n").expect("writes");
        run_git(root, &["add", "."]);
        run_git(
            root,
            &[
                "-c",
                "user.name=t",
                "-c",
                "user.email=t@t",
                "commit",
                "-q",
                "-m",
                "one",
            ],
        );
        let before = working_state(root).expect("state");

        std::fs::create_dir(root.join(".lemma")).expect("creates");
        std::fs::write(root.join(".lemma/index.ls"), "changed").expect("writes");
        let after = working_state(root).expect("state");

        assert_eq!(before, after);
    }

    fn committed_repository() -> tempfile::TempDir {
        let directory = tempfile::tempdir().expect("a temporary directory");
        let root = directory.path();
        run_git(root, &["init", "-q"]);
        std::fs::write(root.join("main.go"), "package main\n").expect("writes");
        run_git(root, &["add", "."]);
        let identity = ["-c", "user.name=t", "-c", "user.email=t@t"];
        run_git(
            root,
            &[&identity[..], &["commit", "-q", "-m", "one"]].concat(),
        );
        directory
    }

    #[test]
    fn a_working_directorys_state_is_kept_until_it_is_forgotten() {
        let directory = committed_repository();
        let root = directory.path();
        let states = States::default();
        let clean = states.working_state(root).expect("state");

        std::fs::write(root.join("main.go"), "package main\n\nfunc main() {}\n").expect("writes");
        let kept = states.working_state(root).expect("state");
        states.forget(root);
        let asked_again = states.working_state(root).expect("state");

        assert_eq!(kept, clean);
        assert_ne!(asked_again, clean);
        assert_eq!(asked_again, working_state(root).expect("state"));
    }

    #[test]
    fn a_clean_working_directory_has_its_commits_state() {
        let directory = committed_repository();
        let root = directory.path();

        let working = States::default().working_state(root).expect("state");

        assert_eq!(working, Some(commit_state(root, "HEAD").expect("state")));
    }

    /// The state of the working directory at `root` through `inputs`.
    fn inputs_working_state(root: &Path, inputs: &[String]) -> Result<Option<String>, GraphError> {
        let Some(working) = working_changes(root)? else {
            return Ok(None);
        };
        Ok(Some(working.scoped(root, inputs).state))
    }

    fn commit_all(root: &Path) {
        run_git(root, &["add", "."]);
        run_git(
            root,
            &[
                "-c",
                "user.name=t",
                "-c",
                "user.email=t@t",
                "commit",
                "-q",
                "-m",
                "c",
            ],
        );
    }

    fn repository_with(files: &[(&str, &str)]) -> tempfile::TempDir {
        let directory = tempfile::tempdir().expect("a temporary directory");
        let root = directory.path();
        run_git(root, &["init", "-q"]);
        for (path, text) in files {
            let path = root.join(path);
            std::fs::create_dir_all(path.parent().expect("a parent")).expect("creates");
            std::fs::write(path, text).expect("writes");
        }
        commit_all(root);
        directory
    }

    #[test]
    fn a_change_outside_the_inputs_keeps_their_state() {
        let directory = repository_with(&[("api/main.go", "package main\n"), ("web/a.ts", "1\n")]);
        let root = directory.path();
        let inputs = ["api".to_owned()];
        let before = inputs_working_state(root, &inputs).expect("state");

        std::fs::write(root.join("web/a.ts"), "2\n").expect("writes");
        let uncommitted = inputs_working_state(root, &inputs).expect("state");
        commit_all(root);
        let committed = inputs_working_state(root, &inputs).expect("state");

        assert_eq!(before, uncommitted);
        assert_eq!(before, committed);
    }

    #[test]
    fn a_change_inside_the_inputs_changes_their_state() {
        let directory = repository_with(&[("api/main.go", "package main\n"), ("web/a.ts", "1\n")]);
        let root = directory.path();
        let inputs = ["api/".to_owned()];
        let before = inputs_working_state(root, &inputs).expect("state");

        std::fs::write(root.join("api/main.go"), "package main // edited\n").expect("writes");
        let uncommitted = inputs_working_state(root, &inputs).expect("state");
        commit_all(root);
        let committed = inputs_working_state(root, &inputs).expect("state");

        assert_ne!(before, uncommitted);
        assert_ne!(before, committed);
        assert_ne!(uncommitted, committed);
    }

    #[test]
    fn the_root_as_an_input_is_the_whole_repository() {
        let directory = repository_with(&[("api/main.go", "package main\n")]);
        let root = directory.path();

        let whole = working_state(root).expect("state");
        let rooted = inputs_working_state(root, &["./".to_owned()]).expect("state");

        assert_eq!(whole, rooted);
    }

    #[test]
    fn a_missing_input_is_part_of_the_state_as_absent() {
        let directory = repository_with(&[("api/main.go", "package main\n")]);
        let root = directory.path();
        let inputs = ["api".to_owned(), "gone".to_owned()];

        let state = inputs_commit_state(root, "HEAD", &inputs).expect("state");

        assert!(state.ends_with("gone\tabsent"), "{state}");
    }

    #[test]
    fn inputs_naming_nothing_are_the_whole_repository() {
        let directory = repository_with(&[("api/main.go", "package main\n")]);
        let root = directory.path();

        let named = inputs_commit_state(root, "HEAD", &["typo".to_owned()]).expect("state");
        let whole = commit_state(root, "HEAD").expect("state");

        assert_eq!(named, whole);
    }

    #[test]
    fn keys_differ_by_adapter_and_by_state() {
        let one = key("adapter-a", "tree 1");

        assert_ne!(one, key("adapter-b", "tree 1"));
        assert_ne!(one, key("adapter-a", "tree 2"));
        assert_eq!(one, key("adapter-a", "tree 1"));
    }
}
