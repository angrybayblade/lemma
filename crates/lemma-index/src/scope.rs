//! Turning reported changes into the smallest part of the tree to walk.

use std::collections::{BTreeMap, BTreeSet};
use std::path::Path;

use lemma_store::StoredFile;

/// Which part of the tree an index has to look at.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum WalkScope {
    /// Everything, as on a first index or when changes may have been missed.
    Full,
    /// Only these directories.
    Partial(DirtyScope),
}

/// Directories that may have changed, relative to the root with `/`
/// separators; the root itself is the empty string.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub(crate) struct DirtyScope {
    /// Directories whose own files are listed again, but not their
    /// subdirectories.
    pub listed: BTreeSet<String>,
    /// Directories walked again in full, such as renamed or new ones.
    pub walked: BTreeSet<String>,
    /// The changed paths themselves, files or directories, for asking Git
    /// which of them now differ from the commit.
    pub paths: BTreeSet<String>,
    /// Whether anything under `.git` changed, so the checked-out commit may
    /// have moved.
    pub git_changed: bool,
}

/// Files whose change means the ignore rules may differ, so what is indexed
/// can change anywhere.
const IGNORE_FILES: [&str; 2] = [".gitignore", ".ignore"];

/// The part of the tree to walk after these changes. `known` is the
/// previous index, to tell a deleted file from a deleted directory.
pub(crate) fn walk_scope<'a>(
    root: &Path,
    changed: impl IntoIterator<Item = &'a Path>,
    known: &BTreeMap<String, StoredFile>,
    ignored_directory: Option<&Path>,
) -> WalkScope {
    let mut scope = DirtyScope::default();
    for path in changed {
        if ignored_directory.is_some_and(|ignored| path.starts_with(ignored)) {
            continue;
        }
        let Some(relative) = relative(root, path) else {
            continue;
        };
        if relative == ".git" || relative.starts_with(".git/") {
            if relative == ".git/info/exclude" {
                log::debug!("full walk: {relative} changed");
                return WalkScope::Full;
            }
            scope.git_changed = true;
            continue;
        }
        let name = relative.rsplit('/').next().unwrap_or_default();
        if IGNORE_FILES.contains(&name) {
            log::debug!("full walk: {relative} changed");
            return WalkScope::Full;
        }
        // Discovery never indexes these, so their churn (a virtual
        // environment, an npm install) cannot change the index.
        if relative
            .split('/')
            .any(|part| part.starts_with('.') || part == "node_modules")
        {
            continue;
        }
        add_change(&mut scope, path, relative, known);
    }
    WalkScope::Partial(scope)
}

fn add_change(
    scope: &mut DirtyScope,
    path: &Path,
    relative: String,
    known: &BTreeMap<String, StoredFile>,
) {
    let parent = relative
        .rsplit_once('/')
        .map_or_else(String::new, |(parent, _)| parent.to_owned());
    scope.listed.insert(parent);
    // A directory, or something gone that was not a known file, may have
    // held files: walk all of it again.
    let is_known_file = known.contains_key(&relative);
    if path.is_dir() || (!path.exists() && !is_known_file) {
        scope.walked.insert(relative.clone());
    }
    scope.paths.insert(relative);
}

impl DirtyScope {
    /// Whether nothing that could affect the index changed.
    pub fn is_empty(&self) -> bool {
        self.listed.is_empty() && self.walked.is_empty() && !self.git_changed
    }

    /// Whether the walk must enter this directory.
    pub fn enters(&self, directory: &str) -> bool {
        let targets = self.listed.iter().chain(&self.walked);
        targets.clone().any(|target| is_within(target, directory))
            || self
                .walked
                .iter()
                .any(|walked| is_within(directory, walked))
    }

    /// Whether this file is part of the scope.
    pub fn covers(&self, file: &str) -> bool {
        let parent = file.rsplit_once('/').map_or("", |(parent, _)| parent);
        self.listed.contains(parent) || self.walked.iter().any(|walked| is_within(file, walked))
    }
}

/// Whether `path` is `ancestor` or below it.
fn is_within(path: &str, ancestor: &str) -> bool {
    ancestor.is_empty()
        || path == ancestor
        || path
            .strip_prefix(ancestor)
            .is_some_and(|rest| rest.starts_with('/'))
}

fn relative(root: &Path, path: &Path) -> Option<String> {
    let relative = path.strip_prefix(root).ok()?;
    let parts: Vec<_> = relative
        .components()
        .map(|part| part.as_os_str().to_string_lossy())
        .collect();
    Some(parts.join("/"))
}

#[cfg(test)]
mod tests {
    use super::*;
    use lemma_model::Language;
    use lemma_store::FileStamp;

    fn known(paths: &[&str]) -> BTreeMap<String, StoredFile> {
        paths
            .iter()
            .map(|path| {
                let file = StoredFile {
                    path: (*path).to_owned(),
                    language: Language::Python,
                    content_hash: String::new(),
                    has_syntax_errors: false,
                    symbol_count: 0,
                    stamp: FileStamp::default(),
                };
                ((*path).to_owned(), file)
            })
            .collect()
    }

    fn scope(changed: &[&str], known_paths: &[&str]) -> WalkScope {
        let root = Path::new("/nonexistent-root");
        let paths: Vec<_> = changed.iter().map(|path| root.join(path)).collect();
        walk_scope(
            root,
            paths.iter().map(PathBuf::as_path),
            &known(known_paths),
            Some(&root.join(".lemma")),
        )
    }

    use std::path::PathBuf;

    #[test]
    fn a_deleted_known_file_lists_only_its_directory() {
        let WalkScope::Partial(dirty) = scope(&["src/app.py"], &["src/app.py"]) else {
            panic!("expected a partial walk");
        };

        assert_eq!(dirty.listed, BTreeSet::from(["src".to_owned()]));
        assert!(dirty.walked.is_empty());
        assert!(dirty.enters(""));
        assert!(dirty.enters("src"));
        assert!(!dirty.enters("tests"));
        assert!(dirty.covers("src/other.py"));
        assert!(!dirty.covers("src/deep/other.py"));
    }

    #[test]
    fn a_vanished_unknown_path_is_walked_as_a_directory() {
        let WalkScope::Partial(dirty) = scope(&["old_package"], &["old_package/a.py"]) else {
            panic!("expected a partial walk");
        };

        assert!(dirty.walked.contains("old_package"));
        assert!(dirty.covers("old_package/deep/a.py"));
        assert!(dirty.enters("old_package/deep"));
    }

    #[test]
    fn ignore_file_changes_need_a_full_walk() {
        assert_eq!(scope(&["src/.gitignore"], &[]), WalkScope::Full);
        assert_eq!(scope(&[".git/info/exclude"], &[]), WalkScope::Full);
    }

    #[test]
    fn hidden_and_dependency_directories_are_skipped() {
        let WalkScope::Partial(dirty) = scope(&[".venv/lib/x.py", "web/node_modules/a/i.ts"], &[])
        else {
            panic!("expected a partial walk");
        };

        assert!(dirty.listed.is_empty());
    }

    #[test]
    fn git_changes_are_noted_and_lemma_files_ignored() {
        let WalkScope::Partial(dirty) = scope(&[".git/HEAD", ".lemma/index.ls"], &[]) else {
            panic!("expected a partial walk");
        };

        assert!(dirty.git_changed);
        assert!(dirty.listed.is_empty());
    }
}
