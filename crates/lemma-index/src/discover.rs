//! Finding the source files to index.

use std::path::{Path, PathBuf};
use std::sync::{Arc, mpsc};

use ignore::{DirEntry, WalkBuilder, WalkState};
use lemma_model::Language;
use lemma_store::FileStamp;

use crate::scope::DirtyScope;
use crate::selection::Selection;

/// Directories never worth indexing, even when not ignored by Git.
const SKIPPED_DIRECTORIES: [&str; 1] = ["node_modules"];

/// A file Lemma knows how to parse.
pub(crate) struct SourceFile {
    pub absolute_path: PathBuf,
    /// Relative to the root, `/`-separated.
    pub relative_path: String,
    pub language: Language,
    /// Metadata at discovery time, before the file is read.
    pub stamp: FileStamp,
}

/// Every supported source file under `root` that `selection` covers,
/// sorted by relative path.
///
/// Respects `.gitignore` and skips hidden files, so build output and
/// Lemma's own `.lemma/` directory are left out.
pub(crate) fn source_files(
    root: &Path,
    selection: &Selection,
) -> Result<Vec<SourceFile>, ignore::Error> {
    source_files_in(root, None, selection)
}

/// Like [`source_files`], but only in the directories `scope` names. The
/// same ignore rules apply, since the walk still passes through every
/// directory above them.
pub(crate) fn source_files_in(
    root: &Path,
    scope: Option<&DirtyScope>,
    selection: &Selection,
) -> Result<Vec<SourceFile>, ignore::Error> {
    let filter_root = root.to_owned();
    let filter_scope = scope.cloned();
    let selection = Arc::new(selection.clone());
    let filter_selection = Arc::clone(&selection);
    let walker = WalkBuilder::new(root)
        .hidden(true)
        .git_ignore(true)
        .git_exclude(true)
        .require_git(false)
        .filter_entry(move |entry| {
            let name = entry.file_name().to_string_lossy();
            if SKIPPED_DIRECTORIES.contains(&name.as_ref()) {
                return false;
            }
            let is_directory = entry.file_type().is_some_and(|kind| kind.is_dir());
            let Some(relative) = relative_path(&filter_root, entry.path()) else {
                return false;
            };
            if is_directory && !filter_selection.enters(&relative) {
                return false;
            }
            let Some(scope) = &filter_scope else {
                return true;
            };
            if is_directory {
                relative.is_empty() || scope.enters(&relative)
            } else {
                scope.covers(&relative)
            }
        })
        .build_parallel();

    // Directories are listed and files stat'ed on several threads; the
    // result is sorted afterwards, so thread scheduling does not matter.
    let (sender, receiver) = mpsc::channel();
    walker.run(|| {
        let sender = sender.clone();
        let selection = Arc::clone(&selection);
        Box::new(move |entry| {
            let found = entry
                .map(|entry| source_file(root, entry, &selection))
                .transpose();
            if let Some(found) = found {
                // The receiver outlives the walk, so sending cannot fail.
                let _ = sender.send(found);
            }
            WalkState::Continue
        })
    });
    drop(sender);

    let mut files = receiver.into_iter().collect::<Result<Vec<_>, _>>()?;
    files.sort_by(|left, right| left.relative_path.cmp(&right.relative_path));
    Ok(files)
}

/// The directories under `top` (itself included) that discovery would enter
/// from `root`: not hidden, not skipped, not ignored by Git and entered by
/// `selection`, sorted. Walking from `root` rather than from `top` applies
/// every ignore file on the way down. For watching exactly what is indexed.
pub(crate) fn directories_under(root: &Path, top: &Path, selection: &Selection) -> Vec<PathBuf> {
    let Some(top_relative) = relative_path(root, top) else {
        return Vec::new();
    };
    let filter_root = root.to_owned();
    let filter_selection = selection.clone();
    let filter_top = top_relative.clone();
    let walker = WalkBuilder::new(root)
        .hidden(true)
        .git_ignore(true)
        .git_exclude(true)
        .require_git(false)
        .filter_entry(move |entry| {
            let is_directory = entry.file_type().is_some_and(|kind| kind.is_dir());
            let name = entry.file_name().to_string_lossy();
            if !is_directory || SKIPPED_DIRECTORIES.contains(&name.as_ref()) {
                return false;
            }
            let Some(relative) = relative_path(&filter_root, entry.path()) else {
                return false;
            };
            filter_selection.enters(&relative) && is_on_the_way(&relative, &filter_top)
        })
        .build();
    let mut directories: Vec<PathBuf> = walker
        .filter_map(Result::ok)
        .filter(|entry| entry.file_type().is_some_and(|kind| kind.is_dir()))
        .filter(|entry| {
            relative_path(root, entry.path())
                .is_some_and(|relative| is_within(&relative, &top_relative))
        })
        .map(DirEntry::into_path)
        .collect();
    directories.sort();
    directories
}

/// Whether the walk must enter `directory` to reach or cover `top`: one of
/// its ancestors, or `top` and what is below it. The root is the empty path.
fn is_on_the_way(directory: &str, top: &str) -> bool {
    is_within(directory, top) || is_within(top, directory)
}

/// Whether `path` is `directory` or below it.
fn is_within(path: &str, directory: &str) -> bool {
    directory.is_empty()
        || path == directory
        || path
            .strip_prefix(directory)
            .is_some_and(|rest| rest.starts_with('/'))
}

/// Whether a path relative to the root is one discovery could return: a
/// supported extension the selection covers, with no hidden or skipped
/// directory on the way. Used to keep Git's view of changes to the same
/// files Lemma indexes.
pub(crate) fn is_in_scope(relative_path: &str, selection: &Selection) -> bool {
    let mut components = relative_path.split('/').peekable();
    while let Some(component) = components.next() {
        if component.starts_with('.') || SKIPPED_DIRECTORIES.contains(&component) {
            return false;
        }
        if components.peek().is_none() {
            return language_of(Path::new(component))
                .is_some_and(|language| selection.covers(relative_path, language));
        }
    }
    false
}

/// The entry as a source file, or `None` if it is not one Lemma parses.
fn source_file(root: &Path, entry: DirEntry, selection: &Selection) -> Option<SourceFile> {
    if !entry
        .file_type()
        .is_some_and(|file_type| file_type.is_file())
    {
        return None;
    }
    let language = language_of(entry.path())?;
    let relative_path = relative_path(root, entry.path())?;
    if !selection.covers(&relative_path, language) {
        return None;
    }
    // A file that cannot be stat'ed gets an empty stamp, which never
    // matches a stored one, so it is read and reported if unreadable.
    let stamp = entry
        .metadata()
        .map(|metadata| stamp_of(&metadata))
        .unwrap_or_default();
    Some(SourceFile {
        absolute_path: entry.into_path(),
        relative_path,
        language,
        stamp,
    })
}

#[cfg(unix)]
fn stamp_of(metadata: &std::fs::Metadata) -> FileStamp {
    use std::os::unix::fs::MetadataExt;
    FileStamp {
        size_bytes: metadata.size(),
        modified_ns: metadata.mtime() * NANOS_PER_SECOND + metadata.mtime_nsec(),
        changed_ns: metadata.ctime() * NANOS_PER_SECOND + metadata.ctime_nsec(),
        inode: metadata.ino(),
    }
}

#[cfg(not(unix))]
fn stamp_of(metadata: &std::fs::Metadata) -> FileStamp {
    let modified_ns = metadata
        .modified()
        .ok()
        .and_then(|time| time.duration_since(std::time::UNIX_EPOCH).ok())
        .and_then(|elapsed| i64::try_from(elapsed.as_nanos()).ok())
        .unwrap_or_default();
    FileStamp {
        size_bytes: metadata.len(),
        modified_ns,
        changed_ns: 0,
        inode: 0,
    }
}

#[cfg(unix)]
const NANOS_PER_SECOND: i64 = 1_000_000_000;

fn language_of(path: &Path) -> Option<Language> {
    let extension = path.extension()?.to_str()?;
    Language::from_extension(extension)
}

fn relative_path(root: &Path, path: &Path) -> Option<String> {
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

    #[test]
    fn only_directories_discovery_enters_are_listed() {
        let root = tempfile::tempdir().unwrap();
        for directory in [
            "src/api",
            "node_modules/dep",
            ".venv/lib",
            "build/out",
            "docs",
        ] {
            std::fs::create_dir_all(root.path().join(directory)).unwrap();
        }
        std::fs::write(root.path().join(".gitignore"), "build/\n").unwrap();
        let listed = |top: &str| -> Vec<String> {
            directories_under(
                root.path(),
                &root.path().join(top),
                &Selection::everything(),
            )
            .iter()
            .map(|path| relative_path(root.path(), path).unwrap())
            .collect()
        };

        let everything = listed("");
        let under_src = listed("src");
        let ignored = listed("build/out");

        assert_eq!(everything, ["", "docs", "src", "src/api"]);
        assert_eq!(under_src, ["src", "src/api"]);
        assert_eq!(ignored, Vec::<String>::new());
    }

    #[test]
    fn supported_files_outside_hidden_or_skipped_directories_are_in_scope() {
        let everything = Selection::everything();
        assert!(is_in_scope("src/app.py", &everything));
        assert!(is_in_scope("main.go", &everything));
    }

    #[test]
    fn hidden_skipped_or_unsupported_paths_are_out_of_scope() {
        let everything = Selection::everything();
        assert!(!is_in_scope(".github/scripts/release.py", &everything));
        assert!(!is_in_scope("web/node_modules/dep/index.ts", &everything));
        assert!(!is_in_scope("README.md", &everything));
        assert!(!is_in_scope("src/.hidden.py", &everything));
    }
}
