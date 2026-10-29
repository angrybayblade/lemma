//! Finding the source files to index.

use std::path::{Path, PathBuf};

use ignore::WalkBuilder;
use lemma_model::Language;

/// Directories never worth indexing, even when not ignored by Git.
const SKIPPED_DIRECTORIES: [&str; 1] = ["node_modules"];

/// A file Lemma knows how to parse.
pub(crate) struct SourceFile {
    pub absolute_path: PathBuf,
    /// Relative to the root, `/`-separated.
    pub relative_path: String,
    pub language: Language,
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
        .build();

    let mut files = Vec::new();
    for entry in walker {
        let entry = entry?;
        if !entry
            .file_type()
            .is_some_and(|file_type| file_type.is_file())
        {
            continue;
        }
        let Some(language) = language_of(entry.path()) else {
            continue;
        };
        let Some(relative_path) = relative_path(root, entry.path()) else {
            continue;
        };
        files.push(SourceFile {
            absolute_path: entry.into_path(),
            relative_path,
            language,
        });
    }
    files.sort_by(|left, right| left.relative_path.cmp(&right.relative_path));
    Ok(files)
}

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
