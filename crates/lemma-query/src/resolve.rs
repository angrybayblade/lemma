//! Turning a name typed by a person or agent into exactly one symbol.

use lemma_model::{Language, SymbolKind};
use lemma_store::{NameMatch, Store, StoredSymbol, SymbolFilter};

use crate::QueryError;

/// More candidates than this are not useful to list.
const MAX_CANDIDATES: u32 = 50;

/// What the caller knows about the symbol they mean.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SymbolSelector {
    /// A name such as `delete` or a qualified name such as `TaskRouter.delete`.
    pub name: String,
    /// Only symbols of this kind.
    pub kind: Option<SymbolKind>,
    /// Only symbols in this language.
    pub language: Option<Language>,
    /// Only symbols whose file path contains this text.
    pub path_contains: Option<String>,
}

/// Which symbol a selector refers to.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SymbolResolution {
    /// Exactly one symbol matched.
    Resolved(StoredSymbol),
    /// Several symbols matched. Lemma does not guess between them.
    Ambiguous(Vec<StoredSymbol>),
    /// Nothing in the snapshot matched.
    Unresolved,
}

/// Finds the symbol a selector refers to by exact name or qualified name.
pub fn resolve_symbol(
    store: &Store,
    snapshot_id: i64,
    selector: &SymbolSelector,
) -> Result<SymbolResolution, QueryError> {
    let filter = SymbolFilter {
        text: selector.name.clone(),
        name_match: NameMatch::Exact,
        kind: selector.kind,
        language: selector.language,
        path_contains: selector.path_contains.clone(),
        limit: MAX_CANDIDATES,
    };
    Ok(classify(store.find_symbols(snapshot_id, &filter)?))
}

fn classify(mut candidates: Vec<StoredSymbol>) -> SymbolResolution {
    match candidates.len() {
        0 => SymbolResolution::Unresolved,
        1 => SymbolResolution::Resolved(candidates.remove(0)),
        _ => SymbolResolution::Ambiguous(candidates),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use lemma_model::{IndexedFile, Position, Snapshot, Span, Symbol};
    use lemma_store::{FileStamp, NewSnapshot, StampedFile};

    fn symbol(name: &str, qualified_name: &str, line: u32) -> Symbol {
        let span = Span {
            start: Position { line, column: 0 },
            end: Position {
                line: line + 1,
                column: 0,
            },
        };
        Symbol {
            name: name.to_owned(),
            qualified_name: qualified_name.to_owned(),
            kind: SymbolKind::Method,
            span,
            name_span: span,
        }
    }

    fn store_with_two_deletes() -> (Store, i64) {
        let mut store = Store::open_in_memory().unwrap();
        let snapshot = Snapshot {
            root: "/repo".to_owned(),
            revision: None,
            is_dirty: false,
            fingerprint: "fp".to_owned(),
        };
        let file = |path: &str, symbols| StampedFile {
            file: IndexedFile {
                path: path.to_owned(),
                language: Language::Python,
                content_hash: "hash".to_owned(),
                has_syntax_errors: false,
                symbols,
            },
            stamp: FileStamp::default(),
        };
        let files = [
            file("a.py", vec![symbol("delete", "Router.delete", 1)]),
            file("b.py", vec![symbol("delete", "Session.delete", 1)]),
        ];
        let snapshot_id = store
            .replace_snapshot(&NewSnapshot {
                snapshot: &snapshot,
                extractor_version: "1",
                indexed_at_ns: 0,
                files: &files,
                uncommitted_paths: &[],
            })
            .unwrap();
        (store, snapshot_id)
    }

    fn selector(name: &str) -> SymbolSelector {
        SymbolSelector {
            name: name.to_owned(),
            kind: None,
            language: None,
            path_contains: None,
        }
    }

    #[test]
    fn shared_names_are_ambiguous() {
        let (store, snapshot_id) = store_with_two_deletes();

        let resolution = resolve_symbol(&store, snapshot_id, &selector("delete")).unwrap();

        let SymbolResolution::Ambiguous(candidates) = resolution else {
            panic!("expected ambiguity, got {resolution:?}");
        };
        assert_eq!(candidates.len(), 2);
    }

    #[test]
    fn qualified_names_resolve_uniquely() {
        let (store, snapshot_id) = store_with_two_deletes();

        let resolution = resolve_symbol(&store, snapshot_id, &selector("Session.delete")).unwrap();

        let SymbolResolution::Resolved(found) = resolution else {
            panic!("expected a symbol, got {resolution:?}");
        };
        assert_eq!(found.path, "b.py");
    }

    #[test]
    fn path_filter_disambiguates() {
        let (store, snapshot_id) = store_with_two_deletes();
        let mut narrowed = selector("delete");
        narrowed.path_contains = Some("a.py".to_owned());

        let resolution = resolve_symbol(&store, snapshot_id, &narrowed).unwrap();

        assert!(matches!(resolution, SymbolResolution::Resolved(found) if found.path == "a.py"));
    }

    #[test]
    fn unknown_names_are_unresolved() {
        let (store, snapshot_id) = store_with_two_deletes();

        let resolution = resolve_symbol(&store, snapshot_id, &selector("missing")).unwrap();

        assert_eq!(resolution, SymbolResolution::Unresolved);
    }
}
