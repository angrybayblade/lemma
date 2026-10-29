//! Finding where a symbol is referenced, through its language's provider.

use std::error::Error;
use std::fs;
use std::path::Path;
use std::time::Duration;

use lemma_model::{Provenance, ReferenceOutcome, content_hash};
use lemma_providers::{ProviderDefinition, ProviderRegistry, ProviderSession, ReferenceAnswer};
use lemma_store::{ReferenceLookupKey, Store, StoredSnapshot, StoredSymbol};

use crate::QueryError;

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
/// Installed providers for the language are tried in order. One that fails
/// to start is skipped; one that starts and then fails is reported, since
/// silently switching would hide a real problem.
pub fn find_references(
    store: &mut Store,
    snapshot: &StoredSnapshot,
    registry: &ProviderRegistry,
    symbol: StoredSymbol,
    options: &ReferenceOptions,
) -> Result<ReferenceReport, QueryError> {
    let root = Path::new(&snapshot.snapshot.root);
    if is_stale(store, snapshot.id, root, &symbol.path)? {
        return Ok(uncached_report(symbol, ReferenceOutcome::Stale, None));
    }

    let providers = registry.installed(symbol.language);
    if providers.is_empty() {
        let candidates = registry.candidate_ids(symbol.language);
        let outcome = ReferenceOutcome::ProviderUnavailable { candidates };
        return Ok(uncached_report(symbol, outcome, None));
    }

    if !options.refresh {
        for provider in &providers {
            let key = lookup_key(&symbol, &provider.definition, options);
            let Some(cached) = store.cached_references(&key)? else {
                continue;
            };
            let outcome = with_enclosing_symbols(store, snapshot.id, cached.outcome)?;
            let provenance = provenance(&provider.definition, cached.provider_version);
            return Ok(ReferenceReport {
                symbol,
                outcome,
                provenance: Some(provenance),
                from_cache: true,
                skipped_providers: Vec::new(),
            });
        }
    }

    let mut start_failures = Vec::new();
    for provider in &providers {
        let definition = &provider.definition;
        let session = match ProviderSession::start(
            definition,
            &provider.executable,
            root,
            options.request_timeout,
        ) {
            Ok(session) => session,
            Err(error) => {
                start_failures.push(format!("{}: {}", definition.id, describe(&error)));
                continue;
            }
        };

        let (provider_version, outcome) = ask_provider(session, &symbol, options);
        let key = lookup_key(&symbol, definition, options);
        store.save_references(&key, provider_version.as_deref(), &outcome)?;
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

fn lookup_key(
    symbol: &StoredSymbol,
    definition: &ProviderDefinition,
    options: &ReferenceOptions,
) -> ReferenceLookupKey {
    ReferenceLookupKey {
        symbol_id: symbol.id,
        provider_id: definition.id.clone(),
        config_fingerprint: definition.fingerprint(),
        include_declaration: options.include_declaration,
    }
}

/// Asks a started provider, then stops it. Failures become outcomes: they
/// say something about this lookup, not about the index.
fn ask_provider(
    session: ProviderSession,
    symbol: &StoredSymbol,
    options: &ReferenceOptions,
) -> (Option<String>, ReferenceOutcome) {
    let provider_version = session.provider_version();
    let answer = session.references(
        &symbol.path,
        symbol.language,
        symbol.symbol.name_span.start,
        options.include_declaration,
    );
    session.shutdown();

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
