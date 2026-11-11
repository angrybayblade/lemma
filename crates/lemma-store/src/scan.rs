//! Reading a working directory into changes for a store: each file parsed
//! once, and only when its contents differ from what the store holds.

use std::collections::{BTreeMap, BTreeSet};
use std::path::{Path, PathBuf};

use lemma_model::{Language, SymbolKind};
use lemma_syntax::{Extractor, Outline, SyntaxError};
use rayon::prelude::*;

use crate::{Change, Digest, FileRecord, FileStamp, SymbolRecord};

/// Why a directory could not be read.
#[derive(Debug, thiserror::Error)]
pub enum ScanError {
    /// Walking the directory failed.
    #[error("cannot walk {path}")]
    Walk {
        /// The directory.
        path: PathBuf,
        /// What went wrong.
        #[source]
        source: ignore::Error,
    },
    /// A parser could not be set up or gave up.
    #[error("cannot parse")]
    Syntax(#[from] SyntaxError),
}

/// Parses one file's contents into its record.
pub fn read_file(
    extractor: &mut Extractor,
    language: Language,
    source: &str,
) -> Result<FileRecord, SyntaxError> {
    let outline = extractor.outline(language, source)?;
    Ok(record_of(language, source, outline))
}

fn record_of(language: Language, source: &str, outline: Outline) -> FileRecord {
    let mut seen: BTreeMap<(String, SymbolKind), u32> = BTreeMap::new();
    let symbols = outline
        .symbols
        .into_iter()
        .map(|outlined| {
            let symbol = outlined.symbol;
            let key = (symbol.qualified_name.clone(), symbol.kind);
            let count = seen.entry(key).or_insert(0);
            let discriminator = *count;
            *count += 1;
            SymbolRecord {
                name: symbol.name,
                qualified_name: symbol.qualified_name,
                kind: symbol.kind,
                span: symbol.span,
                name_span: symbol.name_span,
                discriminator,
                digest: outlined.digest,
                mentions: outlined.mentions,
            }
        })
        .collect();
    FileRecord {
        blob: *blake3::hash(source.as_bytes()).as_bytes(),
        language,
        has_syntax_errors: outline.has_syntax_errors,
        symbols,
    }
}

/// What differs between the directory `root` and the `known` blobs of a
/// source, in no promised order; a commit sorts them.
pub fn changes_on_disk(
    root: &Path,
    known: &BTreeMap<String, Digest>,
) -> Result<Vec<Change>, ScanError> {
    let mut paths = Vec::new();
    for entry in ignore::WalkBuilder::new(root).build() {
        let entry = entry.map_err(|source| ScanError::Walk {
            path: root.to_owned(),
            source,
        })?;
        if entry.file_type().is_some_and(|kind| kind.is_file()) {
            paths.push(entry.into_path());
        }
    }
    let found: Vec<(String, Option<Change>)> = paths
        .par_iter()
        .map_init(Extractor::new, |extractor, path| {
            read_one(root, path, known, extractor.as_mut().ok())
        })
        .filter_map(Result::transpose)
        .collect::<Result<_, SyntaxError>>()?;

    let present: BTreeSet<&str> = found.iter().map(|(path, _)| path.as_str()).collect();
    let removed = known
        .keys()
        .filter(|path| !present.contains(path.as_str()))
        .map(|path| Change::Remove { path: path.clone() });
    let mut changes: Vec<Change> = removed.collect();
    let kept = found.iter().filter_map(|(_, change)| change.clone());
    changes.extend(kept);
    Ok(changes)
}

/// The path of a readable source file and its change, if it has one.
fn read_one(
    root: &Path,
    path: &Path,
    known: &BTreeMap<String, Digest>,
    extractor: Option<&mut Extractor>,
) -> Result<Option<(String, Option<Change>)>, SyntaxError> {
    let Some(language) = path
        .extension()
        .and_then(|extension| extension.to_str())
        .and_then(Language::from_extension)
    else {
        return Ok(None);
    };
    let Some(relative) = relative_path(root, path) else {
        return Ok(None);
    };
    let Ok(source) = std::fs::read_to_string(path) else {
        return Ok(None);
    };
    let blob = *blake3::hash(source.as_bytes()).as_bytes();
    if known.get(&relative) == Some(&blob) {
        return Ok(Some((relative, None)));
    }
    let Some(extractor) = extractor else {
        return Err(SyntaxError::ParseAborted { language });
    };
    let record = read_file(extractor, language, &source)?;
    let change = Change::Put {
        path: relative.clone(),
        record,
        stamp: FileStamp::default(),
    };
    Ok(Some((relative, Some(change))))
}

fn relative_path(root: &Path, path: &Path) -> Option<String> {
    let parts: Option<Vec<&str>> = path
        .strip_prefix(root)
        .ok()?
        .components()
        .map(|part| part.as_os_str().to_str())
        .collect();
    Some(parts?.join("/"))
}
