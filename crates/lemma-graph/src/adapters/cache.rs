//! Keys for the documents adapters generate. A document depends on the
//! adapter (its configuration, its script, the runtime it runs with, and
//! this version of Lemma) and on the code it reads. Code is named by its
//! Git tree, so a commit and a clean working directory at that commit share
//! one document, and a rebase that leaves the files alone keeps it; a
//! working directory with changes adds a digest of the changed files.

use std::path::Path;

use crate::GraphError;
use crate::config::{OperationsFrom, Runtime};
use crate::repos;

/// Bumped when what adapters write, or how Lemma reads it, changes.
const PROTOCOL: &str = "2";

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

/// Lemma's own index and caches, which change on every query: counted in
/// the state, an unignored `.lemma/` would make every key new.
const LEMMA_FILES: &str = ":(exclude,glob)**/.lemma/**";

/// A working directory against the commit it is on: the commit, its tree,
/// and the files that differ from it, untracked ones included, sorted.
pub(crate) struct WorkingChanges {
    pub(crate) commit: String,
    tree: String,
    pub(crate) changed: Vec<String>,
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
    Ok(Some(WorkingChanges {
        commit: commit.to_owned(),
        tree: tree.to_owned(),
        changed: changed_paths(&status),
    }))
}

impl WorkingChanges {
    /// The state of the commit's code: its tree, as [`commit_state`] gives.
    pub(crate) fn commit_state(&self) -> String {
        format!("tree {}", self.tree)
    }

    /// The state of the code in the working directory at `root`: the
    /// commit's tree, and a digest of every file that differs from it.
    pub(crate) fn state(&self, root: &Path) -> String {
        let tree = self.commit_state();
        if self.changed.is_empty() {
            return tree;
        }
        let mut described = String::new();
        for path in &self.changed {
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
}

/// The state of the code in a working directory (see
/// [`WorkingChanges::state`]); `None` outside a Git repository or before
/// its first commit, where nothing can be cached.
pub(crate) fn working_state(root: &Path) -> Result<Option<String>, GraphError> {
    Ok(working_changes(root)?.map(|changes| changes.state(root)))
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

    #[test]
    fn keys_differ_by_adapter_and_by_state() {
        let one = key("adapter-a", "tree 1");

        assert_ne!(one, key("adapter-b", "tree 1"));
        assert_ne!(one, key("adapter-a", "tree 2"));
        assert_eq!(one, key("adapter-a", "tree 1"));
    }
}
