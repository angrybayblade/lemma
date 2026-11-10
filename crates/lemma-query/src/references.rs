//! Finding where a symbol is referenced, through its language's provider.

use std::path::Path;

use lemma_legacy_store::{AnswerKey, FactStore, Question, StoredSymbol};
use lemma_model::{Location, LocationQuery, Outcome, ProviderAnswer, Reference};
use lemma_providers::SourcePoint;

use crate::QueryError;
use crate::lookup::{Answered, LookupContext, ask_providers};
use crate::sessions::SessionPool;
use crate::unseen::unseen_files;

/// The answer to a question about one symbol, with where it came from.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SymbolReport<T> {
    /// The symbol that was asked about.
    pub symbol: StoredSymbol,
    /// The answer.
    pub answer: Answered<T>,
    /// Where the answer may be incomplete: indexed files the provider is
    /// known not to read, such as Go files behind build tags.
    pub warnings: Vec<String>,
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
    let mut report = ask_references(store, context, symbol, include_declaration, pool)?;
    let found: &[Reference] = match &report.answer.outcome {
        Outcome::Found(references) => references,
        Outcome::Empty => &[],
        _ => return Ok(report),
    };
    report.warnings = cached_unseen_files(store, context, &report.symbol, found)?;
    Ok(report)
}

/// The references providers give for `symbol`, without warnings about
/// files they leave out: for walks that read only the answer, as a graph's
/// callers do, since finding those files reads every file of the language.
pub fn ask_references(
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
        Ok(Outcome::Found(with_enclosing_symbols(
            &*store,
            snapshot_id,
            locations,
        )?))
    })?;
    Ok(SymbolReport {
        symbol,
        answer,
        warnings: Vec::new(),
    })
}

/// Warnings about files the provider left out of `symbol`'s references,
/// kept with the snapshot's answers: finding them reads every file of the
/// symbol's language, and they hold until the code or the answer changes.
fn cached_unseen_files(
    store: &mut dyn FactStore,
    context: &LookupContext<'_>,
    symbol: &StoredSymbol,
    references: &[Reference],
) -> Result<Vec<String>, QueryError> {
    let mut listed: Vec<&str> = references
        .iter()
        .map(|reference| reference.path.as_str())
        .collect();
    listed.sort_unstable();
    listed.dedup();
    let answer = lemma_model::content_hash(listed.join("\n").as_bytes());
    let key = AnswerKey {
        snapshot_id: context.snapshot.id,
        question: Question::UnseenFiles,
        path: &symbol.path,
        position: symbol.symbol.name_span.start,
        provider_id: "lemma",
        config_fingerprint: &answer,
    };
    if let Some(kept) = store.cached_text(&key)? {
        let ProviderAnswer::Found(text) = kept.answer else {
            return Ok(Vec::new());
        };
        return Ok(text.lines().map(str::to_owned).collect());
    }
    let root = Path::new(&context.snapshot.snapshot.root);
    let warnings = unseen_files(&*store, context.snapshot.id, root, symbol, references)?;
    let kept = if warnings.is_empty() {
        ProviderAnswer::Empty
    } else {
        ProviderAnswer::Found(warnings.join("\n"))
    };
    store.save_text(&key, None, &kept)?;
    Ok(warnings)
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

/// Names the function or class each in-root reference sits in.
fn with_enclosing_symbols(
    store: &dyn FactStore,
    snapshot_id: i64,
    locations: Vec<Location>,
) -> Result<Vec<Reference>, QueryError> {
    let mut references = Vec::with_capacity(locations.len());
    for location in locations {
        let enclosing_symbol = if location.is_external {
            None
        } else {
            store.enclosing_symbol(snapshot_id, &location.path, location.span.start)?
        };
        references.push(Reference {
            path: location.path,
            span: location.span,
            is_external: location.is_external,
            enclosing_symbol,
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
