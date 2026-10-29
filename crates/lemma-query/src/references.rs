//! Finding where a symbol is referenced, through its language's provider.

use std::error::Error;
use std::fs;
use std::path::Path;
use std::time::Duration;

use lemma_model::{Provenance, ReferenceOutcome, content_hash};
use lemma_providers::{ProviderDefinition, ProviderRegistry, ProviderSession, ReferenceAnswer};
use lemma_store::{ReferenceLookupKey, Store, StoredSnapshot, StoredSymbol};

use crate::QueryError;
use crate::sessions::{PoolError, SessionPool};

/// How to run a reference lookup.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ReferenceOptions {
    /// Whether the symbol's own definition counts as a reference.
    pub include_declaration: bool,
    /// Ask the provider again even if an answer is cached.
    pub refresh: bool,
    /// How long to wait for the provider to start and to answer.
    pub request_timeout: Duration,
}

/// What every reference lookup in a snapshot shares.
#[derive(Debug, Clone, Copy)]
pub struct LookupContext<'a> {
    /// The snapshot the symbol belongs to.
    pub snapshot: &'a StoredSnapshot,
    /// The providers that may answer.
    pub registry: &'a ProviderRegistry,
    /// How to run the lookup.
    pub options: &'a ReferenceOptions,
}

/// The answer to a reference lookup, with where it came from.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ReferenceReport {
    /// The symbol that was looked up.
    pub symbol: StoredSymbol,
    /// What was found.
    pub outcome: ReferenceOutcome,
    /// The provider that answered, when one was chosen.
    pub provenance: Option<Provenance>,
    /// Whether the answer came from the index rather than a live provider.
    pub from_cache: bool,
    /// Providers tried before the one that answered, each with why it could
    /// not start.
    pub skipped_providers: Vec<String>,
}

/// Finds references to a symbol, using a cached answer when the snapshot,
/// provider, and configuration are unchanged.
///
/// Installed providers for the language are tried in order, through `pool`
/// so a server already running for an earlier lookup is reused. One that
/// fails to start is skipped; one that starts and then fails is reported,
/// since silently switching would hide a real problem.
pub fn find_references(
    store: &mut Store,
    context: &LookupContext<'_>,
    symbol: StoredSymbol,
    pool: &mut SessionPool,
) -> Result<ReferenceReport, QueryError> {
    if let Some(report) = answer_without_provider(store, context, &symbol)? {
        return Ok(report);
    }
    let LookupContext {
        snapshot,
        registry,
        options,
    } = *context;

    let mut start_failures = Vec::new();
    for provider in registry.installed(symbol.language) {
        let definition = &provider.definition;
        let session = match pool.session(store, snapshot, &provider, options.request_timeout) {
            Ok(session) => session,
            Err(PoolError::Store(error)) => return Err(error.into()),
            Err(PoolError::Start(error)) => {
                start_failures.push(format!("{}: {}", definition.id, describe(&error)));
                continue;
            }
        };

        let (provider_version, outcome) = ask_provider(session, &symbol);
        if matches!(outcome, ReferenceOutcome::ProviderFailed { .. }) {
            // The server may be wedged or gone; start a fresh one next time.
            pool.discard(snapshot, &provider);
        }
        let key = lookup_key(&symbol, definition);
        store.save_references(&key, provider_version.as_deref(), &outcome)?;
        let outcome = without_own_declaration(outcome, &symbol, options);
        let outcome = with_enclosing_symbols(store, snapshot.id, outcome)?;
        let provenance = provenance(definition, provider_version);
        let mut report = uncached_report(symbol, outcome, Some(provenance));
        report.skipped_providers = start_failures;
        return Ok(report);
    }

    let outcome = ReferenceOutcome::ProviderFailed {
        message: start_failures.join("; "),
    };
    Ok(uncached_report(symbol, outcome, None))
}

/// The answer to a lookup when no live provider is needed: the file is
/// stale, no provider is installed, or an answer is cached. `None` means a
/// provider has to be asked.
pub fn answer_without_provider(
    store: &Store,
    context: &LookupContext<'_>,
    symbol: &StoredSymbol,
) -> Result<Option<ReferenceReport>, QueryError> {
    let LookupContext {
        snapshot,
        registry,
        options,
    } = *context;
    let root = Path::new(&snapshot.snapshot.root);
    if is_stale(store, snapshot.id, root, &symbol.path)? {
        let report = uncached_report(symbol.clone(), ReferenceOutcome::Stale, None);
        return Ok(Some(report));
    }

    let providers = registry.installed(symbol.language);
    if providers.is_empty() {
        let candidates = registry.candidate_ids(symbol.language);
        let outcome = ReferenceOutcome::ProviderUnavailable { candidates };
        return Ok(Some(uncached_report(symbol.clone(), outcome, None)));
    }
    if options.refresh {
        return Ok(None);
    }

    for provider in &providers {
        let key = lookup_key(symbol, &provider.definition);
        let Some(cached) = store.cached_references(&key)? else {
            continue;
        };
        let outcome = without_own_declaration(cached.outcome, symbol, options);
        let outcome = with_enclosing_symbols(store, snapshot.id, outcome)?;
        let provenance = provenance(&provider.definition, cached.provider_version);
        return Ok(Some(ReferenceReport {
            symbol: symbol.clone(),
            outcome,
            provenance: Some(provenance),
            from_cache: true,
            skipped_providers: Vec::new(),
        }));
    }
    Ok(None)
}

/// Whether the symbol's file differs from what was indexed. Stored positions
/// would point at the wrong code, so the provider is not asked.
fn is_stale(store: &Store, snapshot_id: i64, root: &Path, path: &str) -> Result<bool, QueryError> {
    let Some(indexed_hash) = store.file_hash(snapshot_id, path)? else {
        return Ok(true);
    };
    let Ok(contents) = fs::read(root.join(path)) else {
        return Ok(true);
    };
    Ok(content_hash(&contents) != indexed_hash)
}

/// Providers are always asked with declarations included and the answer is
/// cached that way, so both kinds of lookup share it.
fn lookup_key(symbol: &StoredSymbol, definition: &ProviderDefinition) -> ReferenceLookupKey {
    ReferenceLookupKey {
        symbol_id: symbol.id,
        provider_id: definition.id.clone(),
        config_fingerprint: definition.fingerprint(),
        include_declaration: true,
    }
}

/// Drops the symbol's own definition from the answer, unless it was asked
/// for.
///
/// Lemma asks providers to include declarations because some, such as ty,
/// count import statements as declarations and leave them out otherwise.
/// An import is a use of the symbol, so it should be reported.
fn without_own_declaration(
    outcome: ReferenceOutcome,
    symbol: &StoredSymbol,
    options: &ReferenceOptions,
) -> ReferenceOutcome {
    if options.include_declaration {
        return outcome;
    }
    let ReferenceOutcome::Found { mut references } = outcome else {
        return outcome;
    };
    let declared_at = symbol.symbol.name_span.start;
    references
        .retain(|reference| reference.path != symbol.path || reference.span.start != declared_at);
    if references.is_empty() {
        return ReferenceOutcome::Empty;
    }
    ReferenceOutcome::Found { references }
}

/// Asks a started provider. Failures become outcomes: they say something
/// about this lookup, not about the index.
fn ask_provider(
    session: &mut ProviderSession,
    symbol: &StoredSymbol,
) -> (Option<String>, ReferenceOutcome) {
    let provider_version = session.provider_version();
    let answer = session.references(
        &symbol.path,
        symbol.language,
        symbol.symbol.name_span.start,
        true,
    );

    let outcome = match answer {
        Ok(ReferenceAnswer::Found(references)) => ReferenceOutcome::Found { references },
        Ok(ReferenceAnswer::Empty) => ReferenceOutcome::Empty,
        Ok(ReferenceAnswer::Unsupported) => ReferenceOutcome::Unsupported,
        Err(error) => ReferenceOutcome::ProviderFailed {
            message: describe(&error),
        },
    };
    (provider_version, outcome)
}

/// Names the function or class each in-root reference sits in.
fn with_enclosing_symbols(
    store: &Store,
    snapshot_id: i64,
    outcome: ReferenceOutcome,
) -> Result<ReferenceOutcome, QueryError> {
    let ReferenceOutcome::Found { mut references } = outcome else {
        return Ok(outcome);
    };
    for reference in references
        .iter_mut()
        .filter(|reference| !reference.is_external)
    {
        reference.enclosing_symbol =
            store.enclosing_symbol(snapshot_id, &reference.path, reference.span.start)?;
    }
    Ok(ReferenceOutcome::Found { references })
}

fn provenance(definition: &ProviderDefinition, provider_version: Option<String>) -> Provenance {
    Provenance {
        provider_id: definition.id.clone(),
        provider_version,
        config_fingerprint: definition.fingerprint(),
    }
}

fn uncached_report(
    symbol: StoredSymbol,
    outcome: ReferenceOutcome,
    provenance: Option<Provenance>,
) -> ReferenceReport {
    ReferenceReport {
        symbol,
        outcome,
        provenance,
        from_cache: false,
        skipped_providers: Vec::new(),
    }
}

/// The error and its whole cause chain, since the top-level message alone
/// ("provider failed") rarely says what to fix.
fn describe(error: &dyn Error) -> String {
    let mut message = error.to_string();
    let mut source = error.source();
    while let Some(cause) = source {
        message.push_str(": ");
        message.push_str(&cause.to_string());
        source = cause.source();
    }
    message
}
