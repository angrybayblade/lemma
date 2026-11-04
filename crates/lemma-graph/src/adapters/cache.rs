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

/// The state of the code in a working directory: its commit's tree, and a
/// digest of every file that differs from it, untracked ones included;
/// `None` outside a Git repository or before its first commit, where
/// nothing can be cached.
pub(crate) fn working_state(root: &Path) -> Result<Option<String>, GraphError> {
    let Ok(tree) = repos::git(root, &["rev-parse", "HEAD^{tree}"]) else {
        return Ok(None);
    };
    let status = repos::git(
        root,
        &["status", "--porcelain=v1", "-z", "--untracked-files=all"],
    )?;
    let changed = changed_paths(&status);
    let tree = format!("tree {}", tree.trim());
    if changed.is_empty() {
        return Ok(Some(tree));
    }
    let mut described = String::new();
    for path in changed {
        let contents = match std::fs::read(root.join(&path)) {
            Ok(contents) => lemma_model::content_hash(&contents),
            Err(_) => "absent".to_owned(),
        };
        described.push_str(&path);
        described.push('\t');
        described.push_str(&contents);
        described.push('\n');
    }
    let digest = lemma_model::content_hash(described.as_bytes());
    Ok(Some(format!("{tree} changes {digest}")))
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

    #[test]
    fn keys_differ_by_adapter_and_by_state() {
        let one = key("adapter-a", "tree 1");

        assert_ne!(one, key("adapter-b", "tree 1"));
        assert_ne!(one, key("adapter-a", "tree 2"));
        assert_eq!(one, key("adapter-a", "tree 1"));
    }
}
