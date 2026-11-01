//! Finding where a symbol is referenced, through its language's provider.

use lemma_model::{Location, LocationQuery, Outcome, Reference};
use lemma_providers::SourcePoint;
use lemma_store::{FactStore, StoredSymbol};

use crate::QueryError;
use crate::lines::LineReader;
use crate::lookup::{Answered, LookupContext, ask_providers};
use crate::sessions::SessionPool;

/// The answer to a question about one symbol, with where it came from.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SymbolReport<T> {
    /// The symbol that was asked about.
    pub symbol: StoredSymbol,
    /// The answer.
    pub answer: Answered<T>,
}

/// Where a symbol is referenced.
pub type ReferenceReport = SymbolReport<Vec<Reference>>;

/// Finds references to a symbol, using a stored answer when the snapshot,
/// provider, and configuration are unchanged.
///
/// Providers are always asked with declarations included and the answer
/// is stored that way, so lookups with and without `include_declaration`
/// share it.
pub fn find_references(
    store: &mut dyn FactStore,
    context: &LookupContext<'_>,
    symbol: StoredSymbol,
    include_declaration: bool,
    pool: &mut SessionPool,
) -> Result<ReferenceReport, QueryError> {
    let at = name_of(&symbol);
    let answer = ask_providers(store, context, at, pool, &LocationQuery::References)?;
    let snapshot_id = context.snapshot.id;
    let answer = answer.try_map(|locations| {
        let locations = without_own_declaration(locations, &symbol, include_declaration);
        if locations.is_empty() {
            return Ok(Outcome::Empty);
        }
        let root = std::path::Path::new(&context.snapshot.snapshot.root);
        Ok(Outcome::Found(with_context(
            &*store,
            snapshot_id,
            root,
            locations,
        )?))
    })?;
    Ok(SymbolReport { symbol, answer })
}

/// Where providers are asked about a symbol: the start of its name.
pub(crate) fn name_of(symbol: &StoredSymbol) -> SourcePoint<'_> {
    SourcePoint {
        path: &symbol.path,
        language: symbol.language,
        position: symbol.symbol.name_span.start,
    }
}

/// Drops the symbol's own definition from the answer, unless it was asked
/// for.
///
/// Lemma asks providers to include declarations because some, such as ty,
/// count import statements as declarations and leave them out otherwise.
/// An import is a use of the symbol, so it should be reported.
fn without_own_declaration(
    mut locations: Vec<Location>,
    symbol: &StoredSymbol,
    include_declaration: bool,
) -> Vec<Location> {
    if include_declaration {
        return locations;
    }
    let declared_at = symbol.symbol.name_span.start;
    locations.retain(|location| location.path != symbol.path || location.span.start != declared_at);
    locations
}

/// Adds to each in-root reference the function or class it sits in and
/// the text of its line.
fn with_context(
    store: &dyn FactStore,
    snapshot_id: i64,
    root: &std::path::Path,
    locations: Vec<Location>,
) -> Result<Vec<Reference>, QueryError> {
    let mut lines = LineReader::new(root);
    let mut references = Vec::with_capacity(locations.len());
    for location in locations {
        let (enclosing_symbol, line_text) = if location.is_external {
            (None, None)
        } else {
            let start = location.span.start;
            let enclosing = store.enclosing_symbol(snapshot_id, &location.path, start)?;
            (enclosing, lines.line(&location.path, start.line))
        };
        references.push(Reference {
            path: location.path,
            span: location.span,
            is_external: location.is_external,
            enclosing_symbol,
            line_text,
        });
    }
    Ok(references)
}

#[cfg(test)]
mod tests {
    use super::*;
    use lemma_model::{Language, Position, Span, Symbol, SymbolKind};

    fn at(line: u32, column: u32) -> Span {
        let start = Position { line, column };
        Span { start, end: start }
    }

    fn symbol() -> StoredSymbol {
        StoredSymbol {
            id: 1,
            path: "src/router.py".to_owned(),
            language: Language::Python,
            symbol: Symbol {
                name: "delete".to_owned(),
                qualified_name: "Router.delete".to_owned(),
                kind: SymbolKind::Method,
                span: at(2, 4),
                name_span: at(2, 8),
            },
        }
    }

    fn location(path: &str, span: Span) -> Location {
        Location {
            path: path.to_owned(),
            span,
            is_external: false,
        }
    }

    #[test]
    fn the_own_declaration_is_dropped_unless_asked_for() {
        let found = vec![
            location("src/router.py", at(2, 8)),
            location("src/app.py", at(9, 4)),
        ];

        let without = without_own_declaration(found.clone(), &symbol(), false);
        let with = without_own_declaration(found, &symbol(), true);

        assert_eq!(without, [location("src/app.py", at(9, 4))]);
        assert_eq!(with.len(), 2);
    }
}
