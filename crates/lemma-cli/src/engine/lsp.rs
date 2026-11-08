//! `lemma daemon lsp`: listing, starting and stopping the daemon's
//! language servers by hand, for an indexed repository or every member of
//! a workspace. A server is the one its language's first installed
//! provider names (or the one asked for), on the root the index serves,
//! as lookups would use.

use std::collections::{BTreeMap, BTreeSet};
use std::fmt::Write as _;
use std::io::{self, Write};
use std::path::{Path, PathBuf};
use std::str::FromStr;
use std::time::{Duration, Instant};

use anyhow::{Context, bail};
use lemma_model::Language;
use lemma_providers::{InstalledProvider, ProviderRegistry};
use lemma_query::{ServerState, SessionStatus, WarmUp};
use lemma_store::StoredSnapshot;
use serde::Serialize;

use super::{Engine, Indexing, Request, WARM_UP_TIMEOUT, default_index_path, format_duration};
use crate::cli::{LspAction, LspArgs, LspTarget};
use crate::output::Format;
use crate::selection;

/// An indexed root servers can run on: its database and directory.
struct Root {
    db_path: PathBuf,
    directory: PathBuf,
}

/// The server one provider runs on a root, for the languages it serves
/// there, or the languages no installed provider serves.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
struct ServerRow {
    root: PathBuf,
    languages: Vec<String>,
    /// The provider's id; `None` when none is installed.
    provider: Option<String>,
    /// `stopped`, `starting`, `running`, `paused`, `failed` or
    /// `not installed`.
    state: String,
    /// Why it failed, or how to install a missing provider.
    #[serde(skip_serializing_if = "Option::is_none")]
    detail: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    memory_bytes: Option<u64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    idle_secs: Option<u64>,
}

impl Engine {
    /// Runs `lemma daemon lsp`.
    pub(super) fn lsp(
        &mut self,
        request: &Request<'_>,
        args: LspArgs,
        out: &mut dyn Write,
    ) -> anyhow::Result<u8> {
        if !self.warms_up_after_index {
            bail!(
                "`lemma daemon lsp` manages the daemon's servers, which a command run with \
                 --no-daemon would stop as it exits"
            );
        }
        match args.action {
            None => self.list_servers(request, &args.target, out),
            Some(LspAction::List(target)) => self.list_servers(request, &target, out),
            Some(LspAction::Start(start)) => {
                self.start_servers(request, &start.target, start.wait, out)
            }
            Some(LspAction::Stop(target)) => self.stop_servers(request, &target, out),
        }
    }

    fn list_servers(
        &mut self,
        request: &Request<'_>,
        target: &LspTarget,
        out: &mut dyn Write,
    ) -> anyhow::Result<u8> {
        let roots = self.lsp_roots(request, target)?;
        let languages = chosen_languages(target)?;
        let statuses = self.pool.status(Instant::now());
        let mut rows = Vec::new();
        for root in &roots {
            let plan = self.server_plan(request, root, languages.as_ref(), &target.providers)?;
            rows.extend(
                plan.into_iter()
                    .map(|planned| planned.row(&root.directory, &statuses)),
            );
        }
        let shown: BTreeSet<PathBuf> = roots.iter().map(|root| root.directory.clone()).collect();
        let elsewhere: Vec<&SessionStatus> = statuses
            .iter()
            .filter(|status| !shown.contains(&status.root))
            .collect();
        print_rows(out, &rows, &elsewhere, request.format)?;
        Ok(0)
    }

    fn start_servers(
        &mut self,
        request: &Request<'_>,
        target: &LspTarget,
        wait: bool,
        out: &mut dyn Write,
    ) -> anyhow::Result<u8> {
        let roots = self.lsp_roots(request, target)?;
        let languages = chosen_languages(target)?;
        let mut failed = false;
        for root in &roots {
            let plan = self.server_plan(request, root, languages.as_ref(), &target.providers)?;
            let snapshot = self.lsp_snapshot(root)?;
            for planned in plan {
                let (line, has_failed) = self.start_server(root, &snapshot, &planned, wait);
                failed |= has_failed;
                writeln!(out, "{line}")?;
            }
        }
        Ok(u8::from(failed))
    }

    /// Starts the server `planned` names on `root`, waiting for it with
    /// `wait`; what happened, and whether it failed.
    fn start_server(
        &mut self,
        root: &Root,
        snapshot: &StoredSnapshot,
        planned: &Planned,
        wait: bool,
    ) -> (String, bool) {
        if planned.candidates.is_empty() {
            return (planned.row(&root.directory, &[]).describe(), false);
        }
        let started = Instant::now();
        self.pool.warm_up(WarmUp {
            db_path: root.db_path.clone(),
            snapshot: snapshot.clone(),
            providers: planned.candidates.clone(),
            language: planned.languages[0],
            request_timeout: WARM_UP_TIMEOUT,
        });
        if wait {
            self.pool.wait_for(snapshot, &planned.candidates);
        }
        let statuses = self.pool.status(Instant::now());
        let row = planned.row(&root.directory, &statuses);
        let took = wait.then(|| started.elapsed());
        (describe_start(&row, took), row.state == "failed")
    }

    fn stop_servers(
        &mut self,
        request: &Request<'_>,
        target: &LspTarget,
        out: &mut dyn Write,
    ) -> anyhow::Result<u8> {
        let roots = self.lsp_roots(request, target)?;
        let languages = chosen_languages(target)?;
        for root in &roots {
            let plan = self.server_plan(request, root, languages.as_ref(), &target.providers)?;
            let ids: BTreeSet<String> = plan
                .iter()
                .flat_map(|planned| &planned.candidates)
                .map(|provider| provider.definition.id.clone())
                .collect();
            let stopped = self.pool.stop_on(&root.directory, &|id| ids.contains(id));
            if stopped.is_empty() {
                writeln!(out, "no server running on {}", root.directory.display())?;
            }
            for id in stopped {
                writeln!(out, "stopped {id} on {}", root.directory.display())?;
            }
        }
        Ok(0)
    }

    /// The indexed roots at `target`'s path: the repository's, or each
    /// member's of a workspace, after bringing their indexes up to date so
    /// their languages are known.
    fn lsp_roots(
        &mut self,
        request: &Request<'_>,
        target: &LspTarget,
    ) -> anyhow::Result<Vec<Root>> {
        let path = request
            .invocation
            .resolve(target.path.as_deref().unwrap_or(Path::new(".")));
        let (directory, lemma_path) = selection::locate(&path)?;
        let workspace = lemma_path
            .as_deref()
            .map(selection::read_description)
            .transpose()?
            .filter(selection::is_workspace);
        let roots = match (workspace, lemma_path) {
            (Some(_), Some(lemma_path)) => workspace_roots(&lemma_path)?,
            _ => vec![Root {
                db_path: default_index_path(&directory)?,
                directory,
            }],
        };
        for root in &roots {
            if !root.db_path.exists() {
                bail!(
                    "{} is not indexed; run `lemma index {}` first",
                    root.directory.display(),
                    root.directory.display()
                );
            }
        }
        // Up to date, so the languages it holds are the code's.
        self.index(request, &path, Indexing::Refresh, &mut io::sink())?;
        Ok(roots)
    }

    fn lsp_snapshot(&mut self, root: &Root) -> anyhow::Result<StoredSnapshot> {
        let root_text = root.directory.display().to_string();
        self.store(&root.db_path)?
            .snapshot_for_root(&root_text)?
            .with_context(|| format!("no index of {root_text}"))
    }

    /// For each provider the root's languages would use, the languages it
    /// serves there; languages no installed provider serves come last.
    fn server_plan(
        &mut self,
        request: &Request<'_>,
        root: &Root,
        chosen: Option<&BTreeSet<Language>>,
        providers: &[String],
    ) -> anyhow::Result<Vec<Planned>> {
        let key = (root.db_path.clone(), root.directory.clone());
        let indexed = self
            .live_indexes
            .get(&key)
            .map(super::LiveIndex::languages)
            .unwrap_or_default();
        let languages = indexed.into_iter().filter(|language| {
            chosen
                .as_ref()
                .is_none_or(|chosen| chosen.contains(language))
        });
        let registry = request.registry(&root.directory)?;
        Ok(plan(languages, &registry, providers))
    }
}

/// The servers for `languages`: for each, the installed providers among
/// `providers` (all, when empty) in the order a lookup tries them,
/// grouped by that list, so TypeScript and TSX sharing one server are one.
fn plan(
    languages: impl Iterator<Item = Language>,
    registry: &ProviderRegistry,
    providers: &[String],
) -> Vec<Planned> {
    let mut by_candidates: BTreeMap<Vec<String>, Planned> = BTreeMap::new();
    let mut missing = Vec::new();
    for language in languages {
        let candidates: Vec<InstalledProvider> = registry
            .installed(language)
            .into_iter()
            .filter(|provider| providers.is_empty() || providers.contains(&provider.definition.id))
            .collect();
        if candidates.is_empty() {
            if providers.is_empty() {
                missing.push(Planned::missing(language, registry));
            }
            continue;
        }
        let ids = candidates
            .iter()
            .map(|provider| provider.definition.id.clone())
            .collect();
        by_candidates
            .entry(ids)
            .or_insert_with(|| Planned {
                languages: Vec::new(),
                candidates,
                install: None,
            })
            .languages
            .push(language);
    }
    by_candidates.into_values().chain(missing).collect()
}

/// A server a root's languages would use.
struct Planned {
    languages: Vec<Language>,
    /// The providers that could serve them, in the order they are tried;
    /// empty when none is installed.
    candidates: Vec<InstalledProvider>,
    /// How to install a provider for the languages, when none is.
    install: Option<String>,
}

impl Planned {
    fn missing(language: Language, registry: &ProviderRegistry) -> Self {
        let first = registry.candidate_ids(language).into_iter().next();
        let install = first.and_then(|id| {
            registry
                .definitions()
                .iter()
                .find(|definition| definition.id == id)
                .and_then(|definition| definition.install.clone())
        });
        Self {
            languages: vec![language],
            candidates: Vec::new(),
            install,
        }
    }

    /// What serves these languages on `root`, from the pool's `statuses`:
    /// the first candidate running, paused or starting; otherwise the
    /// first, stopped or failed. A candidate that failed before the one
    /// serving is noted.
    fn row(&self, root: &Path, statuses: &[SessionStatus]) -> ServerRow {
        let languages = self
            .languages
            .iter()
            .map(|language| language.as_str().to_owned())
            .collect();
        let status_of = |provider: &InstalledProvider| {
            statuses
                .iter()
                .find(|status| status.root == root && status.provider_id == provider.definition.id)
        };
        let Some(first) = self.candidates.first() else {
            return ServerRow {
                root: root.to_owned(),
                languages,
                provider: None,
                state: "not installed".to_owned(),
                detail: self
                    .install
                    .as_ref()
                    .map(|install| format!("install: {install}")),
                memory_bytes: None,
                idle_secs: None,
            };
        };
        let serving = self.candidates.iter().find(|provider| {
            status_of(provider).is_some_and(|status| status.state != ServerState::Failed)
        });
        let shown = serving.unwrap_or(first);
        let status = status_of(shown);
        let failed_before = serving
            .filter(|serving| serving.definition.id != first.definition.id)
            .map(|_| format!("{} could not start", first.definition.id));
        ServerRow {
            root: root.to_owned(),
            languages,
            provider: Some(shown.definition.id.clone()),
            state: status
                .map_or("stopped", |status| state_name(status.state))
                .to_owned(),
            detail: status
                .and_then(|status| status.failure.clone())
                .or(failed_before),
            memory_bytes: status.and_then(|status| status.memory_bytes),
            idle_secs: status
                .filter(|status| matches!(status.state, ServerState::Running | ServerState::Paused))
                .map(|status| status.idle.as_secs()),
        }
    }
}

impl ServerRow {
    /// One line: languages, provider, state, and what else is known.
    fn describe(&self) -> String {
        let provider = self.provider.as_deref().unwrap_or("-");
        let mut line = format!(
            "{:<16} {:<28} {}",
            self.languages.join(","),
            provider,
            self.state
        );
        if let Some(idle) = self.idle_secs {
            let _ = write!(
                line,
                "  idle {}",
                format_duration(Duration::from_secs(idle))
            );
        }
        if let Some(bytes) = self.memory_bytes {
            let _ = write!(line, "  {} MB", bytes / (1024 * 1024));
        }
        if let Some(detail) = &self.detail {
            let _ = write!(line, ": {detail}");
        }
        line
    }
}

/// What `start` did for one server: started (and how long it took to
/// load, when waited for), already running, or failed.
fn describe_start(row: &ServerRow, took: Option<Duration>) -> String {
    let provider = row.provider.as_deref().unwrap_or("-");
    let languages = row.languages.join(",");
    let root = row.root.display();
    match (row.state.as_str(), took) {
        ("failed", _) => format!(
            "{provider} ({languages}) on {root}: failed: {}",
            row.detail.as_deref().unwrap_or("no reason given")
        ),
        ("starting", _) => {
            format!("{provider} ({languages}) on {root}: starting in the background")
        }
        (_, Some(took)) => format!(
            "{provider} ({languages}) on {root}: ready ({:.1}s)",
            took.as_secs_f64()
        ),
        (state, None) => format!("{provider} ({languages}) on {root}: {state}"),
    }
}

fn state_name(state: ServerState) -> &'static str {
    match state {
        ServerState::Starting => "starting",
        ServerState::Running => "running",
        ServerState::Paused => "paused",
        ServerState::Failed => "failed",
    }
}

/// The languages `--language` names; `None` for all.
fn chosen_languages(target: &LspTarget) -> anyhow::Result<Option<BTreeSet<Language>>> {
    if target.languages.is_empty() {
        return Ok(None);
    }
    let mut chosen = BTreeSet::new();
    for name in &target.languages {
        let language = Language::from_str(name).map_err(|_| {
            anyhow::anyhow!("unknown language `{name}`; use typescript, tsx, python, go or rust")
        })?;
        chosen.insert(language);
    }
    Ok(Some(chosen))
}

/// Each member's indexed root in the workspace whose `lemma.yaml` is at
/// `lemma_path`: its clone, or the worktree at its pin.
fn workspace_roots(lemma_path: &Path) -> anyhow::Result<Vec<Root>> {
    let cache = lemma_graph::repos::cache_directory();
    let members = lemma_graph::compose::members(lemma_path, &cache)?;
    let mut roots = Vec::new();
    for member in members {
        let directory = match &member.commit {
            Some(commit) => lemma_graph::repos::Checkout {
                root: member.root.clone(),
                commit: Some(commit.clone()),
            }
            .worktree(&cache)?,
            None => member.root.clone(),
        };
        roots.push(Root {
            db_path: default_index_path(&member.root)?,
            directory,
        });
    }
    Ok(roots)
}

/// The servers by root, then servers running on other roots.
fn print_rows(
    out: &mut dyn Write,
    rows: &[ServerRow],
    elsewhere: &[&SessionStatus],
    format: Format,
) -> io::Result<()> {
    if format == Format::Json {
        let json = serde_json::to_string_pretty(rows).map_err(io::Error::other)?;
        return writeln!(out, "{json}");
    }
    let mut current: Option<&Path> = None;
    for row in rows {
        if current != Some(row.root.as_path()) {
            writeln!(out, "{}", row.root.display())?;
            current = Some(row.root.as_path());
        }
        writeln!(out, "  {}", row.describe())?;
    }
    let (failed, live): (Vec<&&SessionStatus>, Vec<&&SessionStatus>) = elsewhere
        .iter()
        .partition(|status| status.state == ServerState::Failed);
    if !live.is_empty() {
        writeln!(out, "running on other roots")?;
        for status in live {
            writeln!(
                out,
                "  {:<28} {:<8} {}",
                status.provider_id,
                state_name(status.state),
                status.root.display()
            )?;
        }
    }
    if !failed.is_empty() {
        writeln!(
            out,
            "{} failed starts on other roots; `lemma daemon lsp <root>` says why",
            failed.len()
        )?;
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn provider(id: &str) -> InstalledProvider {
        let registry = ProviderRegistry::builtin().unwrap();
        let definition = registry
            .definitions()
            .iter()
            .find(|definition| definition.id == id)
            .unwrap()
            .clone();
        InstalledProvider {
            definition,
            executable: PathBuf::from("/bin/server"),
            search_path: None,
        }
    }

    fn status(id: &str, state: ServerState, failure: Option<&str>) -> SessionStatus {
        SessionStatus {
            provider_id: id.to_owned(),
            root: PathBuf::from("/repo"),
            idle: Duration::from_secs(5),
            state,
            memory_bytes: Some(200 * 1024 * 1024),
            failure: failure.map(str::to_owned),
        }
    }

    fn typescript() -> Planned {
        Planned {
            languages: vec![Language::TypeScript],
            candidates: vec![
                provider("typescript-language-server"),
                provider("typescript-native"),
            ],
            install: None,
        }
    }

    #[test]
    fn a_fallback_serving_is_shown_with_the_provider_that_could_not_start() {
        let statuses = [
            status(
                "typescript-language-server",
                ServerState::Failed,
                Some("no tsserver"),
            ),
            status("typescript-native", ServerState::Running, None),
        ];

        let row = typescript().row(Path::new("/repo"), &statuses);

        assert_eq!(row.provider.as_deref(), Some("typescript-native"));
        assert_eq!(row.state, "running");
        assert_eq!(
            row.detail.as_deref(),
            Some("typescript-language-server could not start")
        );
    }

    #[test]
    fn a_server_nothing_started_is_the_first_provider_stopped() {
        let row = typescript().row(Path::new("/repo"), &[]);

        assert_eq!(row.provider.as_deref(), Some("typescript-language-server"));
        assert_eq!(row.state, "stopped");
    }

    #[test]
    fn a_language_without_a_server_says_how_to_install_one() {
        let registry = ProviderRegistry::builtin().unwrap();

        let row = Planned::missing(Language::Go, &registry).row(Path::new("/repo"), &[]);

        assert_eq!(row.state, "not installed");
        assert_eq!(
            row.detail.as_deref(),
            Some("install: go install golang.org/x/tools/gopls@latest")
        );
    }

    #[test]
    fn a_start_waited_for_says_how_long_the_server_took_to_load() {
        let row = typescript().row(
            Path::new("/repo"),
            &[status(
                "typescript-language-server",
                ServerState::Running,
                None,
            )],
        );

        let line = describe_start(&row, Some(Duration::from_millis(1500)));

        assert_eq!(
            line,
            "typescript-language-server (typescript) on /repo: ready (1.5s)"
        );
    }

    #[test]
    fn unknown_languages_are_rejected_by_name() {
        let target = LspTarget {
            languages: vec!["go".to_owned(), "cobol".to_owned()],
            ..LspTarget::default()
        };

        let chosen = chosen_languages(&target);

        assert!(chosen.unwrap_err().to_string().contains("`cobol`"));
    }
}
