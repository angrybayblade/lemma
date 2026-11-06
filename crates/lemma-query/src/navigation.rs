//! Following code to where things are defined and implemented.

use lemma_model::{Language, Location, LocationQuery, Outcome, Position};
use lemma_providers::SourcePoint;
use lemma_store::{FactStore, StoredSymbol};

use crate::QueryError;
use crate::lookup::{Answered, LookupContext, ask_providers};
use crate::references::{SymbolReport, name_of};
use crate::sessions::SessionPool;

/// A place a provider pointed to, with the indexed symbol defined there,
/// if there is one.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Target {
    /// Where the provider pointed, usually at a name.
    pub location: Location,
    /// The indexed symbol whose name is at that location.
    pub symbol: Option<StoredSymbol>,
}

/// Where the symbol used at a position is defined.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DefinitionReport {
    /// The file asked about, relative to the snapshot root.
    pub path: String,
    /// The position asked about.
    pub position: Position,
    /// The definitions found.
    pub answer: Answered<Vec<Target>>,
}

/// What implements or overrides a symbol.
pub type ImplementationReport = SymbolReport<Vec<Target>>;

/// Finds where the symbol used at `position` in `path` is defined, such as
/// the function a call goes to. `path` is relative to the snapshot root.
pub fn find_definitions(
    store: &mut dyn FactStore,
    context: &LookupContext<'_>,
    path: &str,
    position: Position,
    pool: &mut SessionPool,
) -> Result<DefinitionReport, QueryError> {
    let language = language_of(path)?;
    let at = SourcePoint {
        path,
        language,
        position,
    };
    let answer = ask_providers(store, context, at, pool, &LocationQuery::Definitions)?;
    let answer = answer.try_map(|locations| to_targets(&*store, context.snapshot.id, locations))?;
    Ok(DefinitionReport {
        path: path.to_owned(),
        position,
        answer,
    })
}

/// Finds what implements an interface or abstract method, or overrides a
/// method.
pub fn find_implementations(
    store: &mut dyn FactStore,
    context: &LookupContext<'_>,
    symbol: StoredSymbol,
    pool: &mut SessionPool,
) -> Result<ImplementationReport, QueryError> {
    let at = name_of(&symbol);
    let answer = ask_providers(store, context, at, pool, &LocationQuery::Implementations)?;
    let answer = answer.try_map(|locations| to_targets(&*store, context.snapshot.id, locations))?;
    Ok(SymbolReport {
        symbol,
        answer,
        warnings: Vec::new(),
    })
}

/// The language a file is parsed as, from its extension.
pub(crate) fn language_of(path: &str) -> Result<Language, QueryError> {
    let extension = path.rsplit_once('.').map_or("", |(_, extension)| extension);
    Language::from_extension(extension).ok_or_else(|| QueryError::UnsupportedFile {
        path: path.to_owned(),
    })
}

fn to_targets(
    store: &dyn FactStore,
    snapshot_id: i64,
    locations: Vec<Location>,
) -> Result<Outcome<Vec<Target>>, QueryError> {
    let mut targets = Vec::with_capacity(locations.len());
    for location in locations {
        let symbol = if location.is_external {
            None
        } else {
            store.symbol_named_at(snapshot_id, &location.path, location.span.start)?
        };
        targets.push(Target { location, symbol });
    }
    Ok(Outcome::Found(targets))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn languages_come_from_extensions() {
        assert_eq!(language_of("src/app.tsx").unwrap(), Language::Tsx);
        assert_eq!(language_of("pkg/server.go").unwrap(), Language::Go);
    }

    #[test]
    fn unknown_extensions_are_rejected() {
        assert!(matches!(
            language_of("README.md"),
            Err(QueryError::UnsupportedFile { .. })
        ));
    }
}
