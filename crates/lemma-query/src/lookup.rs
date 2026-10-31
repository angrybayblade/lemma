//! Asking providers a question about one position, through the store.
//!
//! Every semantic operation follows the same path: refuse to ask about a
//! file that changed since it was indexed, reuse a stored answer when the
//! snapshot, provider and configuration are unchanged, and otherwise try
//! the language's installed providers in order.

use std::error::Error;
use std::fs;
use std::path::Path;
use std::time::Duration;

use lemma_model::{Location, LocationQuery, Outcome, Provenance, ProviderAnswer, content_hash};
use lemma_providers::{
    ProviderDefinition, ProviderError, ProviderRegistry, SemanticProvider, SourcePoint,
};
use lemma_store::{AnswerKey, FactStore, Question, StoreError, StoredAnswer, StoredSnapshot};

use crate::QueryError;
use crate::sessions::{PoolError, SessionPool};

/// How to run a provider lookup.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct LookupOptions {
    /// Ask the provider again even if an answer is stored.
    pub refresh: bool,
    /// How long to wait for the provider to start and to answer.
    pub request_timeout: Duration,
}

/// What every provider lookup in a snapshot shares.
#[derive(Debug, Clone, Copy)]
pub struct LookupContext<'a> {
    /// The snapshot the question is about.
    pub snapshot: &'a StoredSnapshot,
    /// The providers that may answer.
    pub registry: &'a ProviderRegistry,
    /// How to run the lookup.
    pub options: &'a LookupOptions,
}

/// A provider's answer, with where it came from.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Answered<T> {
    /// What was found.
    pub outcome: Outcome<T>,
    /// The provider that answered, when one was chosen.
    pub provenance: Option<Provenance>,
    /// Whether the answer came from the store rather than a live provider.
    pub from_cache: bool,
    /// Providers tried before the one that answered, each with why it could
    /// not start.
    pub skipped_providers: Vec<String>,
}

impl<T> Answered<T> {
    /// An answer no provider was involved in, such as a stale file.
    pub(crate) fn without_provider(outcome: Outcome<T>) -> Self {
        Self {
            outcome,
            provenance: None,
            from_cache: false,
            skipped_providers: Vec::new(),
        }
    }

    /// Converts what was found, keeping where it came from.
    pub(crate) fn try_map<U>(
        self,
        convert: impl FnOnce(T) -> Result<Outcome<U>, QueryError>,
    ) -> Result<Answered<U>, QueryError> {
        Ok(Answered {
            outcome: self.outcome.try_and_then(convert)?,
            provenance: self.provenance,
            from_cache: self.from_cache,
            skipped_providers: self.skipped_providers,
        })
    }
}

/// A kind of question: how to ask it and how its answer is stored.
pub(crate) trait Ask {
    /// What a positive answer holds.
    type Facts;

    /// How the store files the answer.
    fn question(&self) -> Question;

    fn ask(
        &self,
        provider: &mut dyn SemanticProvider,
        at: SourcePoint<'_>,
    ) -> Result<ProviderAnswer<Self::Facts>, ProviderError>;

    fn load(
        &self,
        store: &dyn FactStore,
        key: &AnswerKey<'_>,
    ) -> Result<Option<StoredAnswer<Self::Facts>>, StoreError>;

    fn save(
        &self,
        store: &mut dyn FactStore,
        key: &AnswerKey<'_>,
        provider_version: Option<&str>,
        answer: &ProviderAnswer<Self::Facts>,
    ) -> Result<(), StoreError>;
}

impl Ask for LocationQuery {
    type Facts = Vec<Location>;

    fn question(&self) -> Question {
        Question::Locations(*self)
    }

    fn ask(
        &self,
        provider: &mut dyn SemanticProvider,
        at: SourcePoint<'_>,
    ) -> Result<ProviderAnswer<Vec<Location>>, ProviderError> {
        provider.locations(*self, at)
    }

    fn load(
        &self,
        store: &dyn FactStore,
        key: &AnswerKey<'_>,
    ) -> Result<Option<StoredAnswer<Vec<Location>>>, StoreError> {
        store.cached_locations(key)
    }

    fn save(
        &self,
        store: &mut dyn FactStore,
        key: &AnswerKey<'_>,
        provider_version: Option<&str>,
        answer: &ProviderAnswer<Vec<Location>>,
    ) -> Result<(), StoreError> {
        store.save_locations(key, provider_version, answer)
    }
}

/// What the provider shows for a symbol.
pub(crate) struct Hover;

impl Ask for Hover {
    type Facts = String;

    fn question(&self) -> Question {
        Question::Hover
    }

    fn ask(
        &self,
        provider: &mut dyn SemanticProvider,
        at: SourcePoint<'_>,
    ) -> Result<ProviderAnswer<String>, ProviderError> {
        provider.hover(at)
    }

    fn load(
        &self,
        store: &dyn FactStore,
        key: &AnswerKey<'_>,
    ) -> Result<Option<StoredAnswer<String>>, StoreError> {
        store.cached_text(key)
    }

    fn save(
        &self,
        store: &mut dyn FactStore,
        key: &AnswerKey<'_>,
        provider_version: Option<&str>,
        answer: &ProviderAnswer<String>,
    ) -> Result<(), StoreError> {
        store.save_text(key, provider_version, answer)
    }
}

/// Asks the question at `at`, from the store if possible.
///
/// Installed providers for the language are tried in order, through `pool`
/// so a server already running for an earlier lookup is reused. One that
/// fails to start is skipped; one that starts and then fails is reported,
/// since silently switching would hide a real problem.
pub(crate) fn ask_providers<A: Ask>(
    store: &mut dyn FactStore,
    context: &LookupContext<'_>,
    at: SourcePoint<'_>,
    pool: &mut SessionPool,
    asking: &A,
) -> Result<Answered<A::Facts>, QueryError> {
    if let Some(answered) = answer_without_provider(&*store, context, at, asking)? {
        return Ok(answered);
    }
    let LookupContext {
        snapshot,
        registry,
        options,
    } = *context;

    let mut start_failures = Vec::new();
    for provider in registry.installed(at.language) {
        let definition = &provider.definition;
        let session = match pool.session(&*store, snapshot, &provider, options.request_timeout) {
            Ok(session) => session,
            Err(PoolError::Store(error)) => return Err(error.into()),
            Err(PoolError::Start(error)) => {
                start_failures.push(format!("{}: {}", definition.id, describe(&error)));
                continue;
            }
        };
        let provider_version = session.version();
        let outcome = match asking.ask(session, at) {
            Ok(answer) => {
                let fingerprint = definition.fingerprint();
                let key = answer_key(snapshot, at, asking, definition, &fingerprint);
                asking.save(store, &key, provider_version.as_deref(), &answer)?;
                Outcome::from(answer)
            }
            Err(error) => {
                // The server may be wedged or gone; start a fresh one next time.
                pool.discard(snapshot, &provider);
                Outcome::ProviderFailed {
                    message: describe(&error),
                }
            }
        };
        return Ok(Answered {
            outcome,
            provenance: Some(provenance(definition, provider_version)),
            from_cache: false,
            skipped_providers: start_failures,
        });
    }

    Ok(Answered::without_provider(Outcome::ProviderFailed {
        message: start_failures.join("; "),
    }))
}

/// The answer when no live provider is needed: the file is stale, no
/// provider is installed, or an answer is stored. `None` means a provider
/// has to be asked.
fn answer_without_provider<A: Ask>(
    store: &dyn FactStore,
    context: &LookupContext<'_>,
    at: SourcePoint<'_>,
    asking: &A,
) -> Result<Option<Answered<A::Facts>>, QueryError> {
    let LookupContext {
        snapshot,
        registry,
        options,
    } = *context;
    let root = Path::new(&snapshot.snapshot.root);
    if is_stale(store, snapshot.id, root, at.path)? {
        return Ok(Some(Answered::without_provider(Outcome::Stale)));
    }

    let providers = registry.installed(at.language);
    if providers.is_empty() {
        let candidates = registry.candidate_ids(at.language);
        let outcome = Outcome::ProviderUnavailable { candidates };
        return Ok(Some(Answered::without_provider(outcome)));
    }
    if options.refresh {
        return Ok(None);
    }

    for provider in &providers {
        let definition = &provider.definition;
        let fingerprint = definition.fingerprint();
        let key = answer_key(snapshot, at, asking, definition, &fingerprint);
        let Some(stored) = asking.load(store, &key)? else {
            continue;
        };
        return Ok(Some(Answered {
            outcome: Outcome::from(stored.answer),
            provenance: Some(provenance(definition, stored.provider_version)),
            from_cache: true,
            skipped_providers: Vec::new(),
        }));
    }
    Ok(None)
}

/// Whether the file differs from what was indexed. Stored positions would
/// point at the wrong code, so the provider is not asked.
pub(crate) fn is_stale(
    store: &dyn FactStore,
    snapshot_id: i64,
    root: &Path,
    path: &str,
) -> Result<bool, QueryError> {
    let Some(indexed_hash) = store.file_hash(snapshot_id, path)? else {
        return Ok(true);
    };
    let Ok(contents) = fs::read(root.join(path)) else {
        return Ok(true);
    };
    Ok(content_hash(&contents) != indexed_hash)
}

fn answer_key<'a, A: Ask>(
    snapshot: &StoredSnapshot,
    at: SourcePoint<'a>,
    asking: &A,
    definition: &'a ProviderDefinition,
    config_fingerprint: &'a str,
) -> AnswerKey<'a> {
    AnswerKey {
        snapshot_id: snapshot.id,
        question: asking.question(),
        path: at.path,
        position: at.position,
        provider_id: &definition.id,
        config_fingerprint,
    }
}

fn provenance(definition: &ProviderDefinition, provider_version: Option<String>) -> Provenance {
    Provenance {
        provider_id: definition.id.clone(),
        provider_version,
        config_fingerprint: definition.fingerprint(),
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
