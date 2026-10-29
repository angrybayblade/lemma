//! Builds snapshots of a directory's source files and symbols.
//!
//! Indexing is syntax-only and deterministic: the same files and extractor
//! version always produce the same snapshot fingerprint and symbols.

mod discover;
mod git;

use std::collections::BTreeMap;
use std::fs;
use std::path::{Path, PathBuf};

use lemma_model::{IndexedFile, Language, Snapshot, content_hash};
use lemma_store::{Store, StoreError};
use lemma_syntax::{EXTRACTOR_VERSION, Extractor, SyntaxError};

/// Why indexing failed.
#[derive(Debug, thiserror::Error)]
pub enum IndexError {
    /// The directory to index could not be resolved.
    #[error("cannot index {}", path.display())]
    Root {
        /// The requested directory.
        path: PathBuf,
        /// Why it could not be resolved.
        #[source]
        source: std::io::Error,
    },
    /// Walking the directory failed.
    #[error("could not list source files")]
    Walk(#[from] ignore::Error),
    /// The syntax extractor failed.
    #[error("symbol extraction failed")]
    Syntax(#[from] SyntaxError),
    /// Saving the snapshot failed.
    #[error("could not save the snapshot")]
    Store(#[from] StoreError),
}

/// What an indexing run did.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct IndexSummary {
    /// The stored snapshot's id.
    pub snapshot_id: i64,
    /// The snapshot that was indexed.
    pub snapshot: Snapshot,
    /// Whether nothing changed since the last run, so the stored snapshot and
    /// its cached provider results were kept.
    pub is_unchanged: bool,
    /// Indexed files per language.
    pub files_by_language: BTreeMap<Language, usize>,
    /// Total symbols extracted.
    pub symbol_count: usize,
    /// Files the parser had to recover in. Some of their symbols may be missing.
    pub files_with_syntax_errors: Vec<String>,
    /// Source files skipped because they are not valid UTF-8 or could not be read.
    pub unreadable_files: Vec<String>,
}

/// Indexes every supported source file under `root` and stores the snapshot.
pub fn index_directory(root: &Path, store: &mut Store) -> Result<IndexSummary, IndexError> {
    let root = root.canonicalize().map_err(|source| IndexError::Root {
        path: root.to_owned(),
        source,
    })?;

    let mut extractor = Extractor::new()?;
    let mut files = Vec::new();
    let mut unreadable_files = Vec::new();
    for source_file in discover::source_files(&root)? {
        let Some(contents) = read_utf8(&source_file.absolute_path) else {
            unreadable_files.push(source_file.relative_path);
            continue;
        };
        let extraction = extractor.extract(source_file.language, &contents)?;
        files.push(IndexedFile {
            path: source_file.relative_path,
            language: source_file.language,
            content_hash: content_hash(contents.as_bytes()),
            has_syntax_errors: extraction.has_syntax_errors,
            symbols: extraction.symbols,
        });
    }

    let git_state = git::git_state(&root);
    let snapshot = Snapshot {
        root: root.display().to_string(),
        revision: git_state.revision,
        is_dirty: git_state.is_dirty,
        fingerprint: snapshot_fingerprint(&files),
    };

    let unchanged_id = store
        .latest_snapshot()?
        .filter(|stored| stored.snapshot.root == snapshot.root)
        .filter(|stored| stored.snapshot.fingerprint == snapshot.fingerprint)
        .map(|stored| stored.id);
    let snapshot_id = match unchanged_id {
        Some(id) => id,
        None => store.replace_snapshot(&snapshot, EXTRACTOR_VERSION, &files)?,
    };

    Ok(summarize(
        snapshot_id,
        snapshot,
        unchanged_id.is_some(),
        &files,
        unreadable_files,
    ))
}

/// Hash of the extractor version and every file's path and content hash.
/// Files must already be sorted by path.
fn snapshot_fingerprint(files: &[IndexedFile]) -> String {
    let mut hasher = blake3::Hasher::new();
    hasher.update(EXTRACTOR_VERSION.as_bytes());
    hasher.update(b"\n");
    for file in files {
        hasher.update(file.path.as_bytes());
        hasher.update(b"\0");
        hasher.update(file.content_hash.as_bytes());
        hasher.update(b"\n");
    }
    hasher.finalize().to_hex().to_string()
}

fn summarize(
    snapshot_id: i64,
    snapshot: Snapshot,
    is_unchanged: bool,
    files: &[IndexedFile],
    unreadable_files: Vec<String>,
) -> IndexSummary {
    let mut files_by_language = BTreeMap::new();
    for file in files {
        *files_by_language.entry(file.language).or_insert(0) += 1;
    }
    IndexSummary {
        snapshot_id,
        snapshot,
        is_unchanged,
        files_by_language,
        symbol_count: files.iter().map(|file| file.symbols.len()).sum(),
        files_with_syntax_errors: files
            .iter()
            .filter(|file| file.has_syntax_errors)
            .map(|file| file.path.clone())
            .collect(),
        unreadable_files,
    }
}

fn read_utf8(path: &Path) -> Option<String> {
    let bytes = fs::read(path).ok()?;
    String::from_utf8(bytes).ok()
}

#[cfg(test)]
mod tests {
    use super::*;
    use lemma_store::{NameMatch, SymbolFilter};

    fn write(root: &Path, relative_path: &str, contents: &str) {
        let path = root.join(relative_path);
        fs::create_dir_all(path.parent().unwrap()).unwrap();
        fs::write(path, contents).unwrap();
    }

    fn sample_repository() -> tempfile::TempDir {
        let directory = tempfile::tempdir().unwrap();
        write(
            directory.path(),
            "src/router.ts",
            "export class Router { delete() {} }\n",
        );
        write(
            directory.path(),
            "py/client.py",
            "def delete_session():\n    pass\n",
        );
        write(
            directory.path(),
            "go/main.go",
            "package main\nfunc Delete() {}\n",
        );
        write(
            directory.path(),
            "node_modules/dep/index.ts",
            "export function ignored() {}\n",
        );
        write(directory.path(), "README.md", "# not source\n");
        directory
    }

    #[test]
    fn indexes_supported_files_and_skips_dependencies() {
        let repository = sample_repository();
        let mut store = Store::open_in_memory().unwrap();

        let summary = index_directory(repository.path(), &mut store).unwrap();

        assert_eq!(
            summary.files_by_language,
            BTreeMap::from([
                (Language::TypeScript, 1),
                (Language::Python, 1),
                (Language::Go, 1)
            ])
        );
        assert_eq!(summary.symbol_count, 4);
        assert!(!summary.is_unchanged);

        let ignored = SymbolFilter {
            text: "ignored".to_owned(),
            name_match: NameMatch::Exact,
            kind: None,
            language: None,
            path_contains: None,
            limit: 10,
        };
        assert_eq!(
            store.find_symbols(summary.snapshot_id, &ignored).unwrap(),
            [] as [lemma_store::StoredSymbol; 0]
        );
    }

    #[test]
    fn reindexing_unchanged_files_keeps_the_snapshot() {
        let repository = sample_repository();
        let mut store = Store::open_in_memory().unwrap();

        let first = index_directory(repository.path(), &mut store).unwrap();
        let second = index_directory(repository.path(), &mut store).unwrap();

        assert!(second.is_unchanged);
        assert_eq!(first.snapshot_id, second.snapshot_id);
        assert_eq!(first.snapshot.fingerprint, second.snapshot.fingerprint);
    }

    #[test]
    fn changing_a_file_changes_the_fingerprint() {
        let repository = sample_repository();
        let mut store = Store::open_in_memory().unwrap();

        let first = index_directory(repository.path(), &mut store).unwrap();
        write(
            repository.path(),
            "py/client.py",
            "def delete_session(force):\n    pass\n",
        );
        let second = index_directory(repository.path(), &mut store).unwrap();

        assert!(!second.is_unchanged);
        assert_ne!(first.snapshot.fingerprint, second.snapshot.fingerprint);
    }

    #[test]
    fn non_utf8_files_are_reported_not_indexed() {
        let repository = sample_repository();
        fs::write(repository.path().join("src/binary.ts"), [0xff, 0xfe, 0x00]).unwrap();
        let mut store = Store::open_in_memory().unwrap();

        let summary = index_directory(repository.path(), &mut store).unwrap();

        assert_eq!(summary.unreadable_files, ["src/binary.ts"]);
    }
}
