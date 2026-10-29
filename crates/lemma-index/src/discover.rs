//! Finding the source files to index.

use std::path::{Path, PathBuf};
use std::sync::mpsc;

use ignore::{DirEntry, WalkBuilder, WalkState};
use lemma_model::Language;
use lemma_store::FileStamp;

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

/// Every supported source file under `root`, sorted by relative path.
///
/// Respects `.gitignore` and skips hidden files, so build output and
/// Lemma's own `.lemma/` directory are left out.
pub(crate) fn source_files(root: &Path) -> Result<Vec<SourceFile>, ignore::Error> {
    let walker = WalkBuilder::new(root)
        .hidden(true)
        .git_ignore(true)
        .git_exclude(true)
        .require_git(false)
        .filter_entry(|entry| {
            let name = entry.file_name().to_string_lossy();
            !SKIPPED_DIRECTORIES.contains(&name.as_ref())
        })
        .build_parallel();

    // Directories are listed and files stat'ed on several threads; the
    // result is sorted afterwards, so thread scheduling does not matter.
    let (sender, receiver) = mpsc::channel();
    walker.run(|| {
        let sender = sender.clone();
        Box::new(move |entry| {
            let found = entry.map(|entry| source_file(root, entry)).transpose();
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

/// Whether a path relative to the root is one discovery could return: a
/// supported extension, with no hidden or skipped directory on the way.
/// Used to keep Git's view of changes to the same files Lemma indexes.
pub(crate) fn is_in_scope(relative_path: &str) -> bool {
    let mut components = relative_path.split('/').peekable();
    while let Some(component) = components.next() {
        if component.starts_with('.') || SKIPPED_DIRECTORIES.contains(&component) {
            return false;
        }
        if components.peek().is_none() {
            return language_of(Path::new(component)).is_some();
        }
    }
    false
}

/// The entry as a source file, or `None` if it is not one Lemma parses.
fn source_file(root: &Path, entry: DirEntry) -> Option<SourceFile> {
    if !entry
        .file_type()
        .is_some_and(|file_type| file_type.is_file())
    {
        return None;
    }
    let language = language_of(entry.path())?;
    let relative_path = relative_path(root, entry.path())?;
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
    fn supported_files_outside_hidden_or_skipped_directories_are_in_scope() {
        assert!(is_in_scope("src/app.py"));
        assert!(is_in_scope("main.go"));
    }

    #[test]
    fn hidden_skipped_or_unsupported_paths_are_out_of_scope() {
        assert!(!is_in_scope(".github/scripts/release.py"));
        assert!(!is_in_scope("web/node_modules/dep/index.ts"));
        assert!(!is_in_scope("README.md"));
        assert!(!is_in_scope("src/.hidden.py"));
    }
}
