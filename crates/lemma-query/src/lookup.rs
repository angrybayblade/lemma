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

use lemma_legacy_store::{
    AnswerKey, FactStore, Question, StoreError, StoredAnswer, StoredFile, StoredSnapshot,
};
use lemma_model::{
    Language, Location, LocationQuery, Outcome, Provenance, ProviderAnswer, content_hash,
};
use lemma_providers::{
    ProviderDefinition, ProviderError, ProviderRegistry, SemanticProvider, SourcePoint,
};

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

    /// Whether the answer spans the whole workspace, so a project-scoped
    /// provider must load every project that might hold part of it.
    fn spans_workspace(&self) -> bool {
        false
    }

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

    fn spans_workspace(&self) -> bool {
        matches!(self, Self::References | Self::Implementations)
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
        let stored = store.cached_locations(key)?;
        Ok(stored.filter(|stored| !points_outside_what_exists(&stored.answer)))
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

/// Whether an answer names a file outside the snapshot that is gone, such
/// as a dependency in a module cache since removed: the provider would
/// answer differently now, so the stored answer is not used.
fn points_outside_what_exists(answer: &ProviderAnswer<Vec<Location>>) -> bool {
    let ProviderAnswer::Found(locations) = answer else {
        return false;
    };
    locations
        .iter()
        .any(|location| location.is_external && !Path::new(&location.path).exists())
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
    let question = asking.question().as_str();
    if let Some(answered) = answer_without_provider(&*store, context, at, asking)? {
        log_answer_without_provider(question, at.path, answered.from_cache);
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
            Err(PoolError::Files(source)) => return Err(QueryError::Files { source }),
            Err(PoolError::Start(error)) => {
                start_failures.push(skipped_provider(definition, &error));
                continue;
            }
        };
        let provider_version = session.version();
        let started = std::time::Instant::now();
        let asked = open_projects(&*store, snapshot, at, asking, session)
            .and_then(|()| asking.ask(session, at));
        log::info!(
            "{question} at {}: asked {} in {:.1}s",
            at.path,
            definition.id,
            started.elapsed().as_secs_f64()
        );
        let outcome = match asked {
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

/// Logs a question answered without asking a provider.
fn log_answer_without_provider(question: &str, path: &str, from_cache: bool) {
    let source = if from_cache {
        "the index's cache"
    } else {
        "the index"
    };
    log::info!("{question} at {path}: answered from {source}");
}

/// What a provider that could not start says in the answer, logged as the
/// next one is tried.
fn skipped_provider(definition: &ProviderDefinition, error: &ProviderError) -> String {
    let skipped = format!("{}: {}", definition.id, describe(error));
    log::warn!("could not start {skipped}; trying the next server");
    skipped
}

/// Before a workspace-wide question to a provider that only searches loaded
/// projects, opens a file in each project that mentions the name at `at`.
fn open_projects<A: Ask>(
    store: &dyn FactStore,
    snapshot: &StoredSnapshot,
    at: SourcePoint<'_>,
    asking: &A,
    provider: &mut dyn SemanticProvider,
) -> Result<(), ProviderError> {
    if !asking.spans_workspace() || !provider.is_project_scoped() {
        return Ok(());
    }
    let root = Path::new(&snapshot.snapshot.root);
    let Some(name) = identifier_at(root, at) else {
        return Ok(());
    };
    // A store failure only means no extra projects are opened.
    let files = store.files(snapshot.id).unwrap_or_default();
    let mentioning = files_mentioning(root, &files, at.language, &name);
    let paths: Vec<&str> = mentioning.iter().map(String::as_str).collect();
    provider.open_projects(&paths)
}

/// The identifier at a position, read from the file on disk.
pub(crate) fn identifier_at(root: &Path, at: SourcePoint<'_>) -> Option<String> {
    let source = fs::read_to_string(root.join(at.path)).ok()?;
    let line = source.lines().nth(at.position.line as usize)?;
    let column = at.position.column as usize;
    let is_part = |c: char| c.is_alphanumeric() || c == '_' || c == '$';
    let start = line
        .get(..column)?
        .rfind(|c: char| !is_part(c))
        .map_or(0, |i| i + 1);
    let end = line
        .get(column..)?
        .find(|c: char| !is_part(c))
        .map_or(line.len(), |i| column + i);
    let name = line.get(start..end)?;
    (!name.is_empty()).then(|| name.to_owned())
}

/// Indexed files of the same language family whose text contains `name`,
/// sorted.
fn files_mentioning(
    root: &Path,
    files: &[StoredFile],
    language: Language,
    name: &str,
) -> Vec<String> {
    let finder = memchr::memmem::Finder::new(name.as_bytes());
    files
        .iter()
        .filter(|file| same_family(file.language, language))
        .filter(|file| {
            fs::read(root.join(&file.path)).is_ok_and(|contents| finder.find(&contents).is_some())
        })
        .map(|file| file.path.clone())
        .collect()
}

/// TypeScript and TSX share projects.
pub(crate) fn same_family(left: Language, right: Language) -> bool {
    let family = |language| match language {
        Language::Tsx => Language::TypeScript,
        other => other,
    };
    family(left) == family(right)
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
pub(crate) fn describe(error: &dyn Error) -> String {
    let mut message = error.to_string();
    let mut source = error.source();
    while let Some(cause) = source {
        message.push_str(": ");
        message.push_str(&cause.to_string());
        source = cause.source();
    }
    message
}

#[cfg(test)]
mod tests {
    use super::*;
    use lemma_model::Position;

    fn located(path: &str, is_external: bool) -> Location {
        let position = Position { line: 0, column: 0 };
        Location {
            path: path.to_owned(),
            span: lemma_model::Span {
                start: position,
                end: position,
            },
            is_external,
        }
    }

    #[test]
    fn an_answer_naming_a_vanished_dependency_is_not_used() {
        let gone = located("/tmp/sandbox-that-is-gone/pkg/mod/lib.go", true);
        let here = located("internal/api/router.go", false);

        let answer = ProviderAnswer::Found(vec![here, gone]);

        assert!(points_outside_what_exists(&answer));
    }

    #[test]
    fn an_answer_inside_the_snapshot_is_used() {
        let answer = ProviderAnswer::Found(vec![located("internal/api/router.go", false)]);

        assert!(!points_outside_what_exists(&answer));
    }

    #[test]
    fn identifiers_are_read_around_the_column() {
        let directory =
            std::env::temp_dir().join(format!("lemma-identifier-{}", std::process::id()));
        fs::create_dir_all(&directory).unwrap();
        fs::write(directory.join("a.ts"), "export const $total_area = 1;\n").unwrap();
        let at = SourcePoint {
            path: "a.ts",
            language: Language::TypeScript,
            position: Position {
                line: 0,
                column: 16,
            },
        };

        let name = identifier_at(&directory, at);

        fs::remove_dir_all(&directory).unwrap();
        assert_eq!(name.as_deref(), Some("$total_area"));
    }

    #[test]
    fn typescript_and_tsx_are_one_family() {
        assert!(same_family(Language::Tsx, Language::TypeScript));
        assert!(!same_family(Language::Go, Language::TypeScript));
    }
}
