//! Naming the definitions around a use in a file the snapshot leaves out.
//!
//! A language server reads the files its project names, so a reference can
//! be in a test or a script the index does not hold. Such a file is parsed
//! when a walk first meets a use in it, as the index would parse it, and
//! what was found around the use is kept with the snapshot's answers: a
//! graph asked again reads the file to see it is unchanged, and parses
//! nothing.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use lemma_model::{Language, Position, ProviderAnswer, Symbol, SymbolKind, content_hash};
use lemma_store::{AnswerKey, FactStore, Question, StoredSymbol, SymbolId};
use lemma_syntax::{EXTRACTOR_VERSION, Extractor};
use serde::{Deserialize, Serialize};

use crate::QueryError;

/// The files outside a snapshot that a walk has met uses in, and the
/// definitions around those uses.
pub(crate) struct OutsideFiles {
    root: PathBuf,
    snapshot_id: i64,
    /// Each file met, as read; `None` for one that cannot be read or is in
    /// no language Lemma parses.
    files: BTreeMap<String, Option<OutsideFile>>,
    /// The parser, made when the first file needs parsing.
    extractor: Option<Extractor>,
}

struct OutsideFile {
    language: Language,
    source: String,
    /// What an answer about the file holds for: the extractor that parsed
    /// it and its contents, which no snapshot records.
    fingerprint: String,
    /// Its definitions in source order, parsed when first needed.
    definitions: Option<Vec<Around>>,
}

/// A definition, with its number among those of its file that share its
/// name and kind: what its id is made from, as the store makes it.
#[derive(Debug, Clone, Serialize, Deserialize)]
struct Around {
    discriminator: u32,
    symbol: Symbol,
}

impl OutsideFiles {
    pub(crate) fn new(root: &str, snapshot_id: i64) -> Self {
        Self {
            root: PathBuf::from(root),
            snapshot_id,
            files: BTreeMap::new(),
            extractor: None,
        }
    }

    /// The innermost definition containing `position` in the file at
    /// `path`, with the id it would have if the file were indexed. `None`
    /// at the file's top level, or when the file cannot be parsed.
    pub(crate) fn definition_at(
        &mut self,
        store: &mut dyn FactStore,
        path: &str,
        position: Position,
    ) -> Result<Option<StoredSymbol>, QueryError> {
        let Some(file) = self.file(path) else {
            return Ok(None);
        };
        let (language, fingerprint) = (file.language, file.fingerprint.clone());
        let key = AnswerKey {
            snapshot_id: self.snapshot_id,
            question: Question::DefinitionsAround,
            path,
            position,
            provider_id: "lemma",
            config_fingerprint: &fingerprint,
        };
        let kept = store.cached_text(&key)?.and_then(|kept| read(kept.answer));
        let around = if let Some(around) = kept {
            around
        } else {
            let around = self.parse_around(path, position);
            // What cannot be written down is found again next time.
            if let Some(answer) = written(&around) {
                store.save_text(&key, None, &answer)?;
            }
            around
        };
        let innermost = around.into_iter().next_back();
        Ok(innermost.map(|definition| stored(path, language, definition)))
    }

    fn file(&mut self, path: &str) -> Option<&mut OutsideFile> {
        let root = &self.root;
        self.files
            .entry(path.to_owned())
            .or_insert_with(|| read_file(root, path))
            .as_mut()
    }

    /// The definitions around `position`, outermost first, from the file
    /// as parsed now.
    fn parse_around(&mut self, path: &str, position: Position) -> Vec<Around> {
        if self.extractor.is_none() {
            self.extractor = Extractor::new().ok();
        }
        let Some(file) = self.files.get_mut(path).and_then(Option::as_mut) else {
            return Vec::new();
        };
        if file.definitions.is_none() {
            let parsed = self
                .extractor
                .as_mut()
                .and_then(|extractor| extractor.extract(file.language, &file.source).ok());
            let symbols = parsed.map(|extraction| extraction.symbols);
            file.definitions = Some(numbered(symbols.unwrap_or_default()));
        }
        let mut around: Vec<Around> = file
            .definitions
            .iter()
            .flatten()
            .filter(|definition| {
                let span = definition.symbol.span;
                span.start <= position && position < span.end
            })
            .cloned()
            .collect();
        around.sort_by_key(|definition| definition.symbol.span.start);
        around
    }
}

fn read_file(root: &Path, path: &str) -> Option<OutsideFile> {
    let extension = Path::new(path).extension()?.to_str()?;
    let language = Language::from_extension(extension)?;
    let source = std::fs::read_to_string(root.join(path)).ok()?;
    let contents = content_hash(source.as_bytes());
    Some(OutsideFile {
        language,
        fingerprint: format!("extractor {EXTRACTOR_VERSION} contents {contents}"),
        source,
        definitions: None,
    })
}

/// Numbers definitions as the store numbers those sharing a name and kind,
/// so each has the id it would have if its file were indexed.
fn numbered(symbols: Vec<Symbol>) -> Vec<Around> {
    let mut seen: BTreeMap<(String, SymbolKind), u32> = BTreeMap::new();
    let number = |symbol: Symbol| {
        let count = seen
            .entry((symbol.qualified_name.clone(), symbol.kind))
            .or_insert(0);
        let discriminator = *count;
        *count += 1;
        Around {
            discriminator,
            symbol,
        }
    };
    symbols.into_iter().map(number).collect()
}

fn stored(path: &str, language: Language, definition: Around) -> StoredSymbol {
    let Around {
        discriminator,
        symbol,
    } = definition;
    StoredSymbol {
        id: SymbolId::of(path, &symbol.qualified_name, symbol.kind, discriminator),
        path: path.to_owned(),
        language,
        symbol,
    }
}

/// A kept answer as definitions; `None` for one that cannot be read, which
/// is then found again.
fn read(kept: ProviderAnswer<String>) -> Option<Vec<Around>> {
    match kept {
        ProviderAnswer::Found(text) => serde_json::from_str(&text).ok(),
        ProviderAnswer::Empty => Some(Vec::new()),
        ProviderAnswer::Unsupported => None,
    }
}

/// Definitions as an answer to keep.
fn written(around: &[Around]) -> Option<ProviderAnswer<String>> {
    if around.is_empty() {
        return Some(ProviderAnswer::Empty);
    }
    serde_json::to_string(around)
        .ok()
        .map(ProviderAnswer::Found)
}

#[cfg(test)]
mod tests {
    use super::*;
    use lemma_store::{NewSnapshot, Store};

    const BACKFILL: &str = "import { charge } from \"../src/charge\";\nexport function backfill() {\n  const retry = () => charge(100);\n  return retry();\n}\ncharge(1);\n";

    /// A directory holding `scripts/backfill.ts`, and a store whose
    /// snapshot of the directory does not include it.
    fn unindexed_script() -> (tempfile::TempDir, Store, i64) {
        let directory = tempfile::tempdir().unwrap();
        std::fs::create_dir_all(directory.path().join("scripts")).unwrap();
        std::fs::write(directory.path().join("scripts/backfill.ts"), BACKFILL).unwrap();
        let snapshot = lemma_model::Snapshot {
            root: directory.path().display().to_string(),
            revision: None,
            is_dirty: false,
            fingerprint: "fp".to_owned(),
        };
        let mut store = Store::open_in_memory().unwrap();
        store
            .replace_snapshot(&NewSnapshot {
                snapshot: &snapshot,
                extractor_version: "1",
                indexed_at_ns: 0,
                files: &[],
                uncommitted_paths: &[],
            })
            .unwrap();
        let id = store.snapshot_for_root(&snapshot.root).unwrap().unwrap().id;
        (directory, store, id)
    }

    fn at(line: u32, column: u32) -> Position {
        Position { line, column }
    }

    #[test]
    fn a_use_is_in_the_innermost_definition_around_it_named_as_the_index_would() {
        let (directory, mut store, snapshot_id) = unindexed_script();
        let mut files = OutsideFiles::new(&directory.path().display().to_string(), snapshot_id);

        let in_backfill = files.definition_at(&mut store, "scripts/backfill.ts", at(3, 9));
        let at_the_top_level = files.definition_at(&mut store, "scripts/backfill.ts", at(5, 0));
        let in_no_file = files.definition_at(&mut store, "scripts/gone.ts", at(0, 0));
        let in_no_language = files.definition_at(&mut store, "scripts/notes.txt", at(0, 0));

        let backfill = in_backfill.unwrap().unwrap();
        let as_indexed = SymbolId::of("scripts/backfill.ts", "backfill", SymbolKind::Function, 0);
        assert_eq!(backfill.symbol.qualified_name, "backfill");
        assert_eq!(backfill.id, as_indexed);
        assert_eq!(at_the_top_level.unwrap(), None);
        assert_eq!(in_no_file.unwrap(), None);
        assert_eq!(in_no_language.unwrap(), None);
    }

    #[test]
    fn what_is_around_a_use_is_kept_and_read_back_without_parsing() {
        let (directory, mut store, snapshot_id) = unindexed_script();
        let root = directory.path().display().to_string();
        let first = OutsideFiles::new(&root, snapshot_id)
            .definition_at(&mut store, "scripts/backfill.ts", at(3, 9))
            .unwrap();
        let mut again = OutsideFiles::new(&root, snapshot_id);

        let kept = again
            .definition_at(&mut store, "scripts/backfill.ts", at(3, 9))
            .unwrap();

        assert_eq!(kept, first);
        assert!(again.extractor.is_none());
    }

    #[test]
    fn a_file_that_changed_is_parsed_again() {
        let (directory, mut store, snapshot_id) = unindexed_script();
        let root = directory.path().display().to_string();
        let script = directory.path().join("scripts/backfill.ts");
        OutsideFiles::new(&root, snapshot_id)
            .definition_at(&mut store, "scripts/backfill.ts", at(3, 9))
            .unwrap();
        std::fs::write(&script, BACKFILL.replace("backfill", "refill")).unwrap();

        let after = OutsideFiles::new(&root, snapshot_id)
            .definition_at(&mut store, "scripts/backfill.ts", at(3, 9))
            .unwrap();

        assert_eq!(after.unwrap().symbol.qualified_name, "refill");
    }
}
