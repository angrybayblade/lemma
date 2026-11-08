//! Running commands, with whatever a long-lived process can keep warm
//! between them: open databases and running language servers.
//!
//! The same code runs a command in this process or inside the daemon. Paths
//! and the executable search path come from the [`Invocation`], never from
//! the process environment, because the daemon serves commands started from
//! many directories and shells.

use std::collections::{BTreeMap, HashMap};
use std::ffi::OsString;
use std::fmt::Write as _;
use std::fs;
use std::io::{self, Write};
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex, PoisonError};
use std::thread;
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use anyhow::{Context, bail};
use lemma_index::LiveIndex;
use lemma_index::retention::Retention;
use lemma_model::{Language, Outcome, Position, SymbolKind};
use lemma_providers::ProviderRegistry;
use lemma_query::{
    IdlePolicy, LookupContext, LookupOptions, ServerState, SessionPool, SymbolResolution,
    SymbolSearch, SymbolSelector, UsesAnswer, WarmUp,
};
use lemma_store::{
    FactStore, NameMatch, Store, StoreError, StoredMember, StoredSnapshot, StoredSymbol,
    SymbolFilter,
};

mod lsp;

use crate::addresses;
use crate::anchors::UsesRequest;
use crate::cli::{
    Cli, Command, DaemonAction, DaemonArgs, DefinitionArgs, GraphArgs, GraphDirection,
    GraphDrawing, GraphSourceArg, LookupArgs, RefsArgs, SymbolLookupArgs, SymbolTarget,
    SymbolsArgs,
};
use crate::output::{self, Format};
use crate::selection;

/// Directory, relative to an indexed root, where Lemma keeps its files.
const LEMMA_DIRECTORY: &str = ".lemma";
const INDEX_FILE: &str = "index.db";
const PROVIDERS_FILE: &str = "providers.toml";

/// Exit code when a name matches several symbols.
pub(crate) const EXIT_AMBIGUOUS: u8 = 2;
/// Exit code when a lookup ran but could not produce an answer.
pub(crate) const EXIT_NO_ANSWER: u8 = 3;

/// How long a server started ahead of time may take to load its project.
const WARM_UP_TIMEOUT: Duration = Duration::from_secs(300);

/// Where a command was started from.
pub(crate) struct Invocation {
    /// The caller's working directory, for relative paths.
    pub cwd: PathBuf,
    /// The caller's `PATH`, for finding language servers.
    pub path_env: Option<OsString>,
}

impl Invocation {
    fn resolve(&self, path: &Path) -> PathBuf {
        if path.is_absolute() {
            return path.to_owned();
        }
        self.cwd.join(path)
    }
}

/// Runs commands, keeping databases open and servers warm when long-lived.
pub(crate) struct Engine {
    stores: HashMap<PathBuf, OpenStore>,
    /// Indexes kept up to date between runs, by database and root.
    live_indexes: HashMap<(PathBuf, PathBuf), LiveIndex>,
    pool: SessionPool,
    /// Whether to start language servers right after indexing, so the first
    /// lookup finds them ready. Only worth it in a long-lived process.
    warms_up_after_index: bool,
    /// In-memory symbol name search, by database, built in the background.
    searches: HashMap<PathBuf, Arc<Mutex<SearchSlot>>>,
    /// Where each indexed directory was last indexed from, by database and
    /// root, so its servers can be started again later with the same `PATH`
    /// and provider configuration.
    origins: HashMap<(PathBuf, PathBuf), Origin>,
}

/// The parts of a command that decide which language servers it can use.
#[derive(Debug, Clone)]
struct Origin {
    path_env: Option<OsString>,
    cwd: PathBuf,
    providers_file: Option<PathBuf>,
}

impl Origin {
    /// Built-in providers plus the given file or the project's own,
    /// resolving executables on the caller's `PATH`.
    fn registry(&self, root: &Path) -> anyhow::Result<ProviderRegistry> {
        let registry = match &self.providers_file {
            Some(file) => {
                if !file.is_file() {
                    bail!("provider configuration {} does not exist", file.display());
                }
                ProviderRegistry::load(file)?
            }
            None => ProviderRegistry::load(&root.join(LEMMA_DIRECTORY).join(PROVIDERS_FILE))?,
        };
        Ok(match &self.path_env {
            Some(path) => registry.with_search_path(path.clone(), self.cwd.clone()),
            None => registry,
        })
    }
}

/// A database's in-memory search: the one ready to use, and the snapshot
/// fingerprint of one being built.
#[derive(Default)]
struct SearchSlot {
    ready: Option<SymbolSearch>,
    building: Option<String>,
}

/// A database connection, reopened if the file is replaced, as `lemma index`
/// does when rebuilding an index written by another version.
struct OpenStore {
    store: Store,
    inode: Option<u64>,
}

impl Engine {
    /// An engine for a single command.
    pub fn one_shot() -> Self {
        Self {
            stores: HashMap::new(),
            live_indexes: HashMap::new(),
            pool: SessionPool::for_one_lookup(),
            warms_up_after_index: false,
            searches: HashMap::new(),
            origins: HashMap::new(),
        }
    }

    /// An engine that serves many commands over time, as in the daemon.
    pub fn long_lived() -> Self {
        Self {
            stores: HashMap::new(),
            live_indexes: HashMap::new(),
            pool: SessionPool::long_lived(),
            warms_up_after_index: true,
            searches: HashMap::new(),
            origins: HashMap::new(),
        }
    }

    /// Stops every language server.
    pub fn shutdown(self) {
        self.pool.shutdown();
    }

    /// Runs one command, writing its output to `out`. Returns the exit code.
    pub fn execute(
        &mut self,
        cli: Cli,
        invocation: &Invocation,
        out: &mut dyn Write,
    ) -> anyhow::Result<u8> {
        let request = Request::new(&cli, invocation);
        match cli.command {
            Command::Index { path, keep, ttl } => {
                let retention = lemma_index::retention::Retention {
                    keep,
                    ttl: crate::duration::parse_duration(&ttl)?,
                };
                self.index(
                    &request,
                    &invocation.resolve(&path),
                    Indexing::Command(retention),
                    out,
                )
            }
            Command::Symbols(args) => {
                self.refresh_index(&request, Indexing::FilesOnly);
                self.symbols(&request, args, out)
            }
            Command::Outline { path } => {
                self.refresh_index(&request, Indexing::FilesOnly);
                self.outline(&request, &path, out)
            }
            Command::Refs(args) => {
                self.refresh_index(&request, refresh_for(&args.target));
                self.references(&request, args, out)
            }
            Command::Definition(args) => {
                self.refresh_index(&request, Indexing::FilesOnly);
                self.definition(&request, &args, out)
            }
            Command::Implementations(args) => {
                self.refresh_index(&request, refresh_for(&args.target));
                self.implementations(&request, args, out)
            }
            Command::Signature(args) => {
                self.refresh_index(&request, refresh_for(&args.target));
                self.signature(&request, args, out)
            }
            Command::Graph(args) => {
                // Callers read the exports only as `refs` does, for what
                // links to the symbol; callees follow every export's links.
                let indexing = match args.direction {
                    GraphDirection::Callers => refresh_for(&args.target),
                    _ => Indexing::Refresh,
                };
                self.refresh_index(&request, indexing);
                self.graph(&request, args, out)
            }
            Command::Providers => self.providers(&request, out),
            Command::Daemon(DaemonArgs {
                action: Some(DaemonAction::Lsp(args)),
                ..
            }) => self.lsp(&request, args, out),
            Command::Daemon(_) | Command::Mcp | Command::Verify(_) | Command::Check(_) => {
                bail!("this command runs in its own process, not in the daemon")
            }
        }
    }

    /// Brings the index a query reads up to date with its files first, so a
    /// query never answers from, or refuses over, a file edited since
    /// `lemma index`. Files whose size and times are unchanged cost a `stat`
    /// each. An index given with `--db` is left as it is: what it indexes is
    /// not known from its location.
    fn refresh_index(&mut self, request: &Request<'_>, indexing: Indexing) {
        if request.db.is_some() {
            return;
        }
        // Without an index the query reports that itself.
        let Ok(db_path) = request.find_index() else {
            return;
        };
        let Some(root) = db_path.parent().and_then(Path::parent) else {
            return;
        };
        let root = root.to_path_buf();
        // A refresh that fails (a `lemma.yaml` mid-edit, a pin that needs a
        // fetch while offline) leaves the index as it was: the query still
        // answers from it, and reports any file that changed as stale.
        let _ = self.index(request, &root, indexing, &mut io::sink());
    }

    fn index(
        &mut self,
        request: &Request<'_>,
        path: &Path,
        indexing: Indexing,
        out: &mut dyn Write,
    ) -> anyhow::Result<u8> {
        let (root, lemma_path) = selection::locate(path)?;
        let source = lemma_path
            .as_deref()
            .map(selection::read_lemma_file)
            .transpose()?;
        if let (Some(source), Some(lemma_path)) = (&source, &lemma_path) {
            if selection::is_workspace(source) {
                return self.index_workspace(request, lemma_path, indexing, out);
            }
        }
        let db_path = match &request.db {
            Some(db) => db.clone(),
            None => default_index_path(&root)?,
        };
        if matches!(indexing, Indexing::FilesOnly) {
            self.index_files(&db_path, &root, source.as_ref())?;
            return Ok(0);
        }
        let indexed = self.index_repository(&db_path, &root, source.as_ref())?;
        if let Indexing::Command(retention) = indexing {
            self.apply_retention(&db_path, &root, retention)?;
        }
        let mut warnings: Vec<String> = source
            .is_none()
            .then(|| selection::MISSING_LEMMA_FILE.to_owned())
            .into_iter()
            .collect();
        warnings.extend(indexed.exports.adapter_warnings.iter().cloned());
        let report = output::IndexReport {
            summary: &indexed.summary,
            exports: &indexed.exports,
            warnings: &warnings,
        };
        output::index_summary(out, &report, &db_path, request.format)?;
        if self.warms_up_after_index {
            let origin = request.origin();
            let summary = &indexed.summary;
            // A query's refresh starts no servers: the query starts the one it
            // needs.
            if matches!(indexing, Indexing::Command(_)) {
                let languages: Vec<Language> = summary.files_by_language.keys().copied().collect();
                self.warm_up(&db_path, &root, &languages, &origin)?;
            }
            self.origins.insert((db_path.clone(), root), origin);
            let slot = self.searches.entry(db_path.clone()).or_default();
            build_search(slot, &db_path, &summary.snapshot.fingerprint);
        }
        Ok(0)
    }

    /// Indexes the code `source`'s services are made of under `root` into
    /// `db_path`, with their exports; without a `lemma.yaml`, every
    /// supported file, with no exports.
    fn index_repository(
        &mut self,
        db_path: &Path,
        root: &Path,
        source: Option<&lemma_graph::config::LemmaFile>,
    ) -> anyhow::Result<Indexed> {
        let summary = self.index_files(db_path, root, source)?;
        let store = store_for_writing(&mut self.stores, db_path)?;
        let exports = match source {
            Some(source) => update_exports(store, &summary, source, root)?,
            None => clear_exports(store, &summary)?,
        };
        Ok(Indexed { summary, exports })
    }

    /// Indexes the files `source`'s services are made of under `root` into
    /// `db_path`, leaving exports alone.
    fn index_files(
        &mut self,
        db_path: &Path,
        root: &Path,
        source: Option<&lemma_graph::config::LemmaFile>,
    ) -> anyhow::Result<lemma_index::IndexSummary> {
        let selection = match source {
            Some(source) => selection::repository_selection(source)?,
            None => lemma_index::Selection::everything(),
        };
        let key = (db_path.to_path_buf(), root.to_path_buf());
        // A changed selection starts a fresh index, whose first run walks
        // the whole tree and so drops files no longer selected.
        let is_current = self
            .live_indexes
            .get(&key)
            .is_some_and(|live| live.selection().description() == selection.description());
        if !is_current {
            // Only a long-lived engine benefits from watching the tree.
            let live = if self.warms_up_after_index {
                LiveIndex::watched(root, selection)?
            } else {
                LiveIndex::selecting(root, selection)?
            };
            self.live_indexes.insert(key.clone(), live);
        }
        let store = store_for_writing(&mut self.stores, db_path)?;
        let live = self
            .live_indexes
            .get_mut(&key)
            .context("the live index was lost")?;
        Ok(live.index(store)?)
    }

    /// Indexes every repository a workspace composes into its own index,
    /// then records the members and their exports and links in the
    /// workspace's index.
    fn index_workspace(
        &mut self,
        request: &Request<'_>,
        lemma_path: &Path,
        indexing: Indexing,
        out: &mut dyn Write,
    ) -> anyhow::Result<u8> {
        let root = lemma_path.parent().unwrap_or(Path::new("."));
        let cache = lemma_graph::repos::cache_directory();
        let members = lemma_graph::compose::members(lemma_path, &cache)?;
        if matches!(indexing, Indexing::FilesOnly) {
            for member in &members {
                let indexed_root = member_root(member, &cache)?;
                let db_path = default_index_path(&member.root)?;
                self.index_files(&db_path, &indexed_root, Some(&member.file))?;
            }
            return Ok(0);
        }
        let mut workspace = WorkspaceIndex::default();
        for member in &members {
            let result = self.index_member(member, &cache);
            if let (Ok(_), Indexing::Command(retention)) = (&result, indexing) {
                self.apply_retention(&default_index_path(&member.root)?, &member.root, retention)?;
            }
            workspace.record(member, result)?;
        }
        let db_path = match &request.db {
            Some(db) => db.clone(),
            None => default_index_path(root)?,
        };
        let store = store_for_writing(&mut self.stores, &db_path)?;
        store.replace_workspace(&workspace.members, &workspace.exports, &workspace.links)?;
        // A workspace's index holds its members, not code of its own: drop
        // any snapshot left from indexing the directory before it composed
        // repositories.
        for stale in store.snapshots()? {
            store.delete_snapshot(stale.id)?;
        }
        output::workspace_summary(out, &workspace.lines, &db_path, request.format)?;
        Ok(0)
    }

    /// Drops the commit snapshots `retention` no longer keeps from the index
    /// at `db_path`, whose own directory is `own_root`, and their worktrees.
    fn apply_retention(
        &mut self,
        db_path: &Path,
        own_root: &Path,
        retention: Retention,
    ) -> anyhow::Result<()> {
        let store = store_for_writing(&mut self.stores, db_path)?;
        let snapshots = store.snapshots()?;
        let kept: Vec<lemma_index::retention::Kept> = snapshots
            .iter()
            .map(|stored| lemma_index::retention::Kept {
                id: stored.id,
                root: stored.snapshot.root.clone(),
                indexed_at_ns: stored.indexed_at_ns,
            })
            .collect();
        let own = own_root
            .canonicalize()
            .unwrap_or_else(|_| own_root.to_path_buf());
        let dropped =
            lemma_index::retention::to_drop(&kept, &own.display().to_string(), retention, now_ns());
        let worktrees = lemma_graph::repos::cache_directory().join("worktrees");
        for id in dropped {
            let Some(root) = snapshots.iter().find(|stored| stored.id == id) else {
                continue;
            };
            let root = PathBuf::from(&root.snapshot.root);
            store.delete_snapshot(id)?;
            self.live_indexes
                .remove(&(db_path.to_path_buf(), root.clone()));
            if root.starts_with(&worktrees) && root.exists() {
                lemma_graph::repos::remove_worktree(&own, &root)?;
            }
        }
        Ok(())
    }

    /// Indexes one member into the index in its clone: at its pin through a
    /// worktree, or its working directory.
    fn index_member(
        &mut self,
        member: &lemma_graph::compose::Member,
        cache: &Path,
    ) -> anyhow::Result<(StoredMember, Indexed)> {
        let indexed_root = member_root(member, cache)?;
        let db_path = default_index_path(&member.root)?;
        let indexed = self.index_repository(&db_path, &indexed_root, Some(&member.file))?;
        let stored = StoredMember {
            name: member.name.clone(),
            root: member.root.display().to_string(),
            revision: member.commit.clone(),
            indexed_root: indexed_root.display().to_string(),
            database: db_path.display().to_string(),
        };
        Ok((stored, indexed))
    }

    fn symbols(
        &mut self,
        request: &Request<'_>,
        args: SymbolsArgs,
        out: &mut dyn Write,
    ) -> anyhow::Result<u8> {
        let db_path = request.find_index()?;
        let members = open_store(&mut self.stores, &db_path)?.workspace_members()?;
        let filter = SymbolFilter {
            text: args.query,
            name_match: if args.exact {
                NameMatch::Exact
            } else {
                NameMatch::Contains
            },
            kind: args.filters.kind,
            language: args.filters.language,
            path_contains: args.filters.path,
            limit: args.limit,
        };
        if !members.is_empty() {
            let found = self.workspace_symbols(&members, &filter)?;
            output::symbols(out, &found, request.format)?;
            return Ok(0);
        }
        let (store, snapshot) = index_at(&mut self.stores, &db_path)?;
        let in_memory = match filter.name_match {
            NameMatch::Contains if self.warms_up_after_index => {
                let slot = self.searches.entry(db_path.clone()).or_default();
                search_in_memory(slot, &db_path, store, &snapshot, &filter)?
            }
            _ => None,
        };
        let found = match in_memory {
            Some(found) => found,
            None => store.find_symbols(snapshot.id, &filter)?,
        };
        output::symbols(out, &found, request.format)?;
        Ok(0)
    }

    fn outline(
        &mut self,
        request: &Request<'_>,
        path: &Path,
        out: &mut dyn Write,
    ) -> anyhow::Result<u8> {
        let file = request.invocation.resolve(path);
        let found = self.index_of_file(request.find_index()?, &file)?;
        let store = open_store(&mut self.stores, &found.db_path)?;
        let mut symbols = lemma_query::symbols_in_file(store, found.snapshot.id, &found.relative)?;
        let Some(member) = found.member else {
            output::outline(out, &found.relative, &symbols, request.format)?;
            return Ok(0);
        };
        // Ids are numbered per repository, so in a workspace they name it.
        for symbol in &mut symbols {
            symbol.path = format!("{member}:{}", symbol.path);
        }
        let shown = format!("{member}:{}", found.relative);
        output::outline(out, &shown, &symbols, request.format)?;
        Ok(0)
    }

    /// The index, snapshot and snapshot-relative path to look `file` up
    /// in. From a workspace that is the index of the member holding it: the
    /// workspace's own index has no source snapshot.
    fn index_of_file(&mut self, db_path: PathBuf, file: &Path) -> anyhow::Result<FileIndex> {
        let members = open_store(&mut self.stores, &db_path)?.workspace_members()?;
        if let Some((member, relative)) = member_file(&members, file) {
            let member_db = PathBuf::from(&member.database);
            let store = open_store(&mut self.stores, &member_db)?;
            let Some(snapshot) = store.snapshot_for_root(&member.indexed_root)? else {
                bail!(
                    "{} has no index of {}; run `lemma index` on the workspace",
                    member.name,
                    member.indexed_root
                );
            };
            return Ok(FileIndex {
                db_path: member_db,
                snapshot,
                relative,
                member: Some(member.name.clone()),
            });
        }
        let (_, snapshot) = index_at(&mut self.stores, &db_path)?;
        let relative = relative_to_root(&snapshot, file)?;
        Ok(FileIndex {
            db_path,
            snapshot,
            relative,
            member: None,
        })
    }

    fn references(
        &mut self,
        request: &Request<'_>,
        args: RefsArgs,
        out: &mut dyn Write,
    ) -> anyhow::Result<u8> {
        let Some(located) = self.locate(request, args.target, out)? else {
            return Ok(EXIT_AMBIGUOUS);
        };
        let Located {
            db_path,
            snapshot,
            symbol: target,
            linked,
            workspace: _,
            repository,
        } = located;
        let store = open_store(&mut self.stores, &db_path)?;
        let root = snapshot.snapshot.root.clone();
        let registry = request.registry(Path::new(&root))?;
        let options = lookup_options(&args.lookup);
        let context = LookupContext {
            snapshot: &snapshot,
            registry: &registry,
            options: &options,
        };
        let mut report = lemma_query::find_references(
            store,
            &context,
            target,
            args.include_declaration,
            &mut self.pool,
        )?;
        label_with_repository(&mut report.symbol, repository.as_deref());
        output::reference_report(out, &report, &linked, &root, request.format)?;
        Ok(exit_code(&report.answer.outcome))
    }

    fn definition(
        &mut self,
        request: &Request<'_>,
        args: &DefinitionArgs,
        out: &mut dyn Write,
    ) -> anyhow::Result<u8> {
        let (path, position) = parse_location(&args.location)?;
        let file = request.invocation.resolve(&path);
        let FileIndex {
            db_path,
            snapshot,
            relative,
            member: _,
        } = self.index_of_file(request.find_index()?, &file)?;
        let store = open_store(&mut self.stores, &db_path)?;
        let registry = request.registry(Path::new(&snapshot.snapshot.root))?;
        let options = lookup_options(&args.lookup);
        let context = LookupContext {
            snapshot: &snapshot,
            registry: &registry,
            options: &options,
        };
        let report =
            lemma_query::find_definitions(store, &context, &relative, position, &mut self.pool)?;
        output::definition_report(out, &report, request.format)?;
        Ok(exit_code(&report.answer.outcome))
    }

    fn implementations(
        &mut self,
        request: &Request<'_>,
        mut args: SymbolLookupArgs,
        out: &mut dyn Write,
    ) -> anyhow::Result<u8> {
        args.target.filters.prefers_implementable = true;
        let Some(located) = self.locate(request, args.target, out)? else {
            return Ok(EXIT_AMBIGUOUS);
        };
        let Located {
            db_path,
            snapshot,
            symbol: target,
            linked: _,
            workspace: _,
            repository,
        } = located;
        let store = open_store(&mut self.stores, &db_path)?;
        let registry = request.registry(Path::new(&snapshot.snapshot.root))?;
        let options = lookup_options(&args.lookup);
        let context = LookupContext {
            snapshot: &snapshot,
            registry: &registry,
            options: &options,
        };
        let mut report =
            lemma_query::find_implementations(store, &context, target, &mut self.pool)?;
        label_with_repository(&mut report.symbol, repository.as_deref());
        if let lemma_model::Outcome::Found(targets) = &mut report.answer.outcome {
            for symbol in targets
                .iter_mut()
                .filter_map(|target| target.symbol.as_mut())
            {
                label_with_repository(symbol, repository.as_deref());
            }
        }
        output::implementation_report(out, &report, request.format)?;
        Ok(exit_code(&report.answer.outcome))
    }

    fn signature(
        &mut self,
        request: &Request<'_>,
        args: SymbolLookupArgs,
        out: &mut dyn Write,
    ) -> anyhow::Result<u8> {
        let Some(located) = self.locate(request, args.target, out)? else {
            return Ok(EXIT_AMBIGUOUS);
        };
        let Located {
            db_path,
            snapshot,
            symbol: target,
            linked: _,
            workspace: _,
            repository,
        } = located;
        let store = open_store(&mut self.stores, &db_path)?;
        let registry = request.registry(Path::new(&snapshot.snapshot.root))?;
        let options = lookup_options(&args.lookup);
        let context = LookupContext {
            snapshot: &snapshot,
            registry: &registry,
            options: &options,
        };
        let mut report = lemma_query::signature_of(store, &context, target, &mut self.pool)?;
        label_with_repository(&mut report.symbol, repository.as_deref());
        output::signature_report(out, &report, request.format)?;
        let is_described = report.declaration.is_some() || report.hover.outcome.is_answered();
        Ok(if is_described { 0 } else { EXIT_NO_ANSWER })
    }

    /// Symbols matching `filter` in every member of a workspace, each path
    /// prefixed with its repository (`#billing:src/charges.go`), up to the
    /// filter's limit.
    fn workspace_symbols(
        &mut self,
        members: &[StoredMember],
        filter: &SymbolFilter,
    ) -> anyhow::Result<Vec<StoredSymbol>> {
        let mut found = Vec::new();
        // A path naming a member (`platform/apps/api`, `#platform`) searches
        // that member, the rest of the path inside it.
        let scoped: Vec<(&StoredMember, SymbolFilter)> = members
            .iter()
            .filter_map(|member| Some((member, member_filter(member, filter)?)))
            .collect();
        let searched: Vec<(&StoredMember, SymbolFilter)> = if scoped.is_empty() {
            members
                .iter()
                .map(|member| (member, filter.clone()))
                .collect()
        } else {
            scoped
        };
        for (member, filter) in &searched {
            let Some((store, snapshot)) = self.member_index(member)? else {
                continue;
            };
            for mut symbol in store.find_symbols(snapshot.id, filter)? {
                symbol.path = format!("{}:{}", member.name, symbol.path);
                found.push(symbol);
            }
        }
        found.truncate(usize::try_from(filter.limit).unwrap_or(usize::MAX));
        Ok(found)
    }

    /// A member's index and its snapshot of the member, if it has been
    /// indexed.
    fn member_index(
        &mut self,
        member: &StoredMember,
    ) -> anyhow::Result<Option<(&Store, StoredSnapshot)>> {
        let db_path = PathBuf::from(&member.database);
        if !db_path.exists() {
            return Ok(None);
        }
        let store = open_store(&mut self.stores, &db_path)?;
        let snapshot = store.snapshot_for_root(&member.indexed_root)?;
        Ok(snapshot.map(|snapshot| (&*store, snapshot)))
    }

    fn graph(
        &mut self,
        request: &Request<'_>,
        args: GraphArgs,
        out: &mut dyn Write,
    ) -> anyhow::Result<u8> {
        let Some(located) = self.locate(request, args.target, out)? else {
            return Ok(EXIT_AMBIGUOUS);
        };
        let Located {
            db_path,
            snapshot,
            symbol: target,
            linked,
            workspace,
            repository: _,
        } = located;
        let store = open_store(&mut self.stores, &db_path)?;
        let registry = request.registry(Path::new(&snapshot.snapshot.root))?;
        let options = lookup_options(&args.lookup);
        let context = LookupContext {
            snapshot: &snapshot,
            registry: &registry,
            options: &options,
        };
        let graph_options = graph_options(
            args.direction,
            args.depth,
            args.source,
            args.max_nodes,
            args.only.clone(),
        );
        let mut graph =
            lemma_query::symbol_graph(store, &context, &target, &graph_options, &mut self.pool)?;
        addresses::add_root_links(&mut graph, &linked, graph_options.direction);
        if graph_options.direction != lemma_query::Direction::Callers {
            let exports = store.exports(snapshot.id)?;
            let links = store.export_links(snapshot.id)?;
            addresses::add_links(&mut graph, &exports, &links);
        }
        // Run from a workspace, a graph follows callers across it unasked.
        let workspace = args
            .workspace
            .as_ref()
            .map(|workspace| request.invocation.resolve(workspace))
            .or(workspace);
        if let Some(workspace) = &workspace {
            let root = PathBuf::from(&snapshot.snapshot.root);
            let across = Across {
                direction: graph_options.direction,
                calls: args.across,
            };
            add_system_trace(&mut graph, workspace, &root, across)?;
        }
        let drawing = match args.format {
            GraphDrawing::Tree => output::Drawing::Tree,
            GraphDrawing::Dot => output::Drawing::Dot,
            GraphDrawing::Mermaid => output::Drawing::Mermaid,
        };
        output::use_graph(out, &graph, drawing, request.format)?;
        let is_unanswered = graph.relationships.is_empty() && !graph.notes.is_empty();
        Ok(if is_unanswered { EXIT_NO_ANSWER } else { 0 })
    }

    /// The symbol a command is about, the index holding it, and what
    /// lemma.yaml links it to. In a workspace's index the symbol is named by
    /// an export's address and found in its member's index. `None` when a
    /// name matches several symbols, which are printed.
    fn locate(
        &mut self,
        request: &Request<'_>,
        target: SymbolTarget,
        out: &mut dyn Write,
    ) -> anyhow::Result<Option<Located>> {
        let db_path = request.find_index()?;
        let members = open_store(&mut self.stores, &db_path)?.workspace_members()?;
        if !members.is_empty() {
            return self.locate_in_workspace(request, &members, &target, out);
        }
        let (store, snapshot) = index_at(&mut self.stores, &db_path)?;
        let Some(symbol) = choose_symbol(store, &snapshot, target, request.format, out)? else {
            return Ok(None);
        };
        let linked = addresses::repository_links(store, &snapshot, &symbol)?;
        Ok(Some(Located {
            db_path,
            snapshot,
            symbol,
            linked,
            workspace: None,
            repository: None,
        }))
    }

    /// In a workspace's index: an export's address is found in its
    /// member's index, and a name in whichever member defines it. A name
    /// several members define is printed with each one's repository.
    fn locate_in_workspace(
        &mut self,
        request: &Request<'_>,
        members: &[StoredMember],
        target: &SymbolTarget,
        out: &mut dyn Write,
    ) -> anyhow::Result<Option<Located>> {
        let workspace_db = request.find_index()?;
        let workspace = workspace_file(&workspace_db);
        let text = target.symbol.clone();
        let Some(address) =
            addresses::as_address(&text).filter(|address| address.repository.is_some())
        else {
            return match self.locate_name_in_workspace(members, target, workspace)? {
                InWorkspace::Found(located) => Ok(Some(*located)),
                InWorkspace::Several(candidates) => {
                    output::ambiguous(out, &text, &candidates, request.format)?;
                    Ok(None)
                }
            };
        };
        let repository = address.repository.clone().unwrap_or_default();
        let Some(member) = members.iter().find(|member| member.name == repository) else {
            bail!("the workspace composes no repository `{repository}`");
        };
        let (exports, links) = {
            let store = open_store(&mut self.stores, &workspace_db)?;
            (store.workspace_exports()?, store.workspace_links()?)
        };
        let db_path = PathBuf::from(&member.database);
        let store = open_store(&mut self.stores, &db_path)?;
        let Some(snapshot) = store.snapshot_for_root(&member.indexed_root)? else {
            bail!(
                "{repository} has no index of {}; run `lemma index` on the workspace",
                member.indexed_root
            );
        };
        let local = lemma_graph::address::Address {
            repository: None,
            ..address.clone()
        };
        let SymbolResolution::Resolved(symbol) =
            addresses::resolve_export(store, &snapshot, &local)?
        else {
            bail!("`{address}` resolves to no indexed symbol");
        };
        let linked = addresses::workspace_links(&address.to_string(), &exports, &links);
        Ok(Some(Located {
            db_path,
            snapshot,
            symbol,
            linked,
            workspace,
            repository: Some(repository),
        }))
    }

    /// A name, found in the one member of a workspace that defines it.
    fn locate_name_in_workspace(
        &mut self,
        members: &[StoredMember],
        target: &SymbolTarget,
        workspace: Option<PathBuf>,
    ) -> anyhow::Result<InWorkspace> {
        let mut resolved = Vec::new();
        let mut candidates = Vec::new();
        // A repository-qualified id (`#billing:607`) names a symbol in that
        // member only: ids are numbered per repository, so a bare one can
        // match a different symbol in every member.
        let qualified = qualified_id(&target.symbol);
        let (wanted, symbol) = match &qualified {
            Some((repository, id)) => (Some(repository.as_str()), format!("#{id}")),
            None => (None, target.symbol.clone()),
        };
        for member in members {
            if wanted.is_some_and(|repository| repository != member.name) {
                continue;
            }
            let Some((store, snapshot)) = self.member_index(member)? else {
                continue;
            };
            let found = select_symbol(store, &snapshot, &symbol, target.filters.clone())?;
            let labelled = |mut symbol: StoredSymbol| {
                symbol.path = format!("{}:{}", member.name, symbol.path);
                symbol
            };
            match found {
                SymbolResolution::Resolved(symbol) => {
                    candidates.push(labelled(symbol.clone()));
                    resolved.push((member, snapshot, symbol));
                }
                SymbolResolution::Ambiguous(several) => {
                    candidates.extend(several.into_iter().map(labelled));
                }
                SymbolResolution::Unresolved => {}
            }
        }
        if candidates.is_empty() {
            bail!(
                "no repository in the workspace has an indexed symbol matching `{}`",
                target.symbol
            );
        }
        let [(member, snapshot, symbol)] = &resolved[..] else {
            return Ok(InWorkspace::Several(candidates));
        };
        if candidates.len() > 1 {
            return Ok(InWorkspace::Several(candidates));
        }
        let db_path = PathBuf::from(&member.database);
        let store = open_store(&mut self.stores, &db_path)?;
        let linked = addresses::repository_links(store, snapshot, symbol)?;
        Ok(InWorkspace::Found(Box::new(Located {
            db_path,
            snapshot: snapshot.clone(),
            symbol: symbol.clone(),
            linked,
            workspace,
            repository: Some(member.name.clone()),
        })))
    }

    fn providers(&mut self, request: &Request<'_>, out: &mut dyn Write) -> anyhow::Result<u8> {
        // Without an index, fall back to the current directory's project file.
        let indexed_root = match request.find_index() {
            Ok(db_path) => self
                .store(&db_path)?
                .latest_snapshot()?
                .map(|stored| PathBuf::from(stored.snapshot.root)),
            Err(_) => None,
        };
        let root = indexed_root.unwrap_or_else(|| request.invocation.cwd.clone());
        let registry = request.registry(&root)?;
        let home = std::env::var_os("HOME").map(PathBuf::from);
        output::providers(out, &registry, home.as_deref(), request.format)?;
        Ok(0)
    }

    /// Starts, in the background, the server each indexed language would use,
    /// so the first lookup does not wait for it to load the project.
    fn warm_up(
        &mut self,
        db_path: &Path,
        root: &Path,
        languages: &[Language],
        origin: &Origin,
    ) -> anyhow::Result<()> {
        let store = self.store(db_path)?;
        let Some(snapshot) = store.snapshot_for_root(&root.display().to_string())? else {
            return Ok(());
        };
        let registry = origin.registry(root)?;
        for language in languages {
            self.pool.warm_up(WarmUp {
                db_path: db_path.to_owned(),
                snapshot: snapshot.clone(),
                providers: registry.installed(*language),
                language: *language,
                request_timeout: WARM_UP_TIMEOUT,
            });
        }
        Ok(())
    }

    /// Where a symbol is used under a root that need not be indexed, asked of
    /// the pool's servers so they stay warm for the next question.
    pub fn uses(&mut self, question: &UsesRequest, invocation: &Invocation) -> UsesAnswer {
        let origin = Origin {
            path_env: invocation.path_env.clone(),
            cwd: invocation.cwd.clone(),
            providers_file: question.providers_file.clone(),
        };
        let registry = match origin.registry(&question.root) {
            Ok(registry) => registry,
            Err(error) => {
                return UsesAnswer::Failed {
                    message: format!("{error:#}"),
                };
            }
        };
        crate::anchors::uses_in(&mut self.pool, &registry, question)
    }

    /// Pauses and stops idle servers by `policy`, and, with
    /// `warms_up_on_activity`, starts servers again for directories whose
    /// files are changing, so they are ready by the next lookup.
    pub fn maintain(&mut self, policy: &IdlePolicy, warms_up_on_activity: bool) {
        self.pool.maintain(policy, Instant::now());
        if !warms_up_on_activity {
            return;
        }
        let active: Vec<((PathBuf, PathBuf), Vec<Language>)> = self
            .live_indexes
            .iter()
            .filter(|(_, live)| live.has_unindexed_changes() && !self.pool.serves_root(live.root()))
            .map(|(key, live)| (key.clone(), live.languages()))
            .collect();
        for ((db_path, root), languages) in active {
            let Some(origin) = self.origins.get(&(db_path.clone(), root.clone())).cloned() else {
                continue;
            };
            // Best effort: a failure here shows up again on the next lookup.
            let _ = self.warm_up(&db_path, &root, &languages, &origin);
        }
    }

    /// A line per running language server: what it serves, how long it has
    /// been idle, whether it is paused, and its memory.
    pub fn status(&mut self) -> String {
        let statuses = self.pool.status(Instant::now());
        if statuses.is_empty() {
            return "no language servers\n".to_owned();
        }
        let mut text = String::new();
        for status in statuses {
            let memory = status.memory_bytes.map_or_else(
                || "?".to_owned(),
                |bytes| format!("{} MB", bytes / (1024 * 1024)),
            );
            let state = match status.state {
                ServerState::Starting => "starting",
                ServerState::Running => "running",
                ServerState::Paused => "paused",
                ServerState::Failed => "failed",
            };
            let _ = writeln!(
                text,
                "{:<28} {:<8} idle {:>6}  {:>8}  {}",
                status.provider_id,
                state,
                format_duration(status.idle),
                memory,
                status.root.display()
            );
        }
        text
    }

    fn store(&mut self, db_path: &Path) -> anyhow::Result<&mut Store> {
        open_store(&mut self.stores, db_path)
    }
}

/// Nanoseconds since the Unix epoch, for deciding which snapshots are old
/// enough to drop; it never affects what an index holds.
fn now_ns() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0, |elapsed| {
            i64::try_from(elapsed.as_nanos()).unwrap_or(i64::MAX)
        })
}

/// A symbol a command is about, where it is indexed, and the symbols
/// lemma.yaml links it to.
struct Located {
    db_path: PathBuf,
    snapshot: StoredSnapshot,
    symbol: StoredSymbol,
    linked: Vec<output::Linked>,
    /// The workspace lemma.yaml, when the symbol was found through a
    /// workspace's index.
    workspace: Option<PathBuf>,
    /// The workspace member the symbol is in, such as `#billing`, so
    /// answers show the id to pass back (`#billing:607`).
    repository: Option<String>,
}

/// Labels an answer's symbol with the workspace member it is in, as
/// workspace searches label theirs, once the lookup no longer reads its
/// path.
fn label_with_repository(symbol: &mut StoredSymbol, repository: Option<&str>) {
    if let Some(repository) = repository {
        symbol.path = format!("{repository}:{}", symbol.path);
    }
}

/// What a name means across a workspace: one symbol, or several, each
/// labelled with its repository.
enum InWorkspace {
    Found(Box<Located>),
    Several(Vec<StoredSymbol>),
}

/// The lemma.yaml a workspace's index at `db_path` (`<root>/.lemma/index.db`)
/// was built from, if it is there.
fn workspace_file(db_path: &Path) -> Option<PathBuf> {
    let root = db_path.parent()?.parent()?;
    let file = root.join(lemma_graph::config::LEMMA_FILE);
    file.exists().then_some(file)
}

/// Where a member is indexed: a worktree at its pin, or its clone.
fn member_root(member: &lemma_graph::compose::Member, cache: &Path) -> anyhow::Result<PathBuf> {
    let Some(commit) = &member.commit else {
        return Ok(member.root.clone());
    };
    let checkout = lemma_graph::repos::Checkout {
        root: member.root.clone(),
        commit: Some(commit.clone()),
    };
    Ok(checkout.worktree(cache)?)
}

/// How to refresh before a query about `target`: an export's address is
/// resolved through the exports, so they are brought up to date too. A
/// symbol named otherwise leaves them as the last refresh did, sparing a
/// query after an edit the adapters' run; what `refs` lists as linked is
/// then as of that refresh, which only a lemma.yaml or adapter change
/// makes out of date.
fn refresh_for(target: &SymbolTarget) -> Indexing {
    if addresses::as_address(&target.symbol).is_some() {
        return Indexing::Refresh;
    }
    Indexing::FilesOnly
}

/// Why an index is being brought up to date.
#[derive(Clone, Copy)]
enum Indexing {
    /// `lemma index`: drops snapshots `retention` no longer keeps, and in the
    /// daemon starts language servers for what it indexed.
    Command(Retention),
    /// Before a query: indexes changed files and nothing more, keeping
    /// whatever snapshots the last `lemma index` kept.
    Refresh,
    /// Before a query that reads no exports (an outline, a definition):
    /// as `Refresh`, but leaves exports as they were, sparing the adapters
    /// a changed file would run. The next query that reads them updates
    /// them.
    FilesOnly,
}

/// What indexing one repository produced.
struct Indexed {
    summary: lemma_index::IndexSummary,
    exports: output::ExportCounts,
}

/// A workspace index as its members are indexed.
#[derive(Default)]
struct WorkspaceIndex {
    members: Vec<StoredMember>,
    exports: Vec<(String, lemma_model::Export)>,
    links: Vec<lemma_model::ExportLink>,
    lines: Vec<output::MemberLine>,
}

impl WorkspaceIndex {
    /// Records a member's index, or why it has none.
    fn record(
        &mut self,
        member: &lemma_graph::compose::Member,
        result: anyhow::Result<(StoredMember, Indexed)>,
    ) -> anyhow::Result<()> {
        let (stored, indexed) = match result {
            Ok(found) => found,
            Err(error) => {
                self.lines.push(output::MemberLine::skipped(
                    &member.name,
                    format!("{error:#}"),
                ));
                return Ok(());
            }
        };
        let store = Store::open(Path::new(&stored.database))?;
        let snapshot_id = indexed.summary.snapshot_id;
        for export in store.exports(snapshot_id)? {
            self.exports.push((member.name.clone(), export));
        }
        for link in store.export_links(snapshot_id)? {
            self.links.push(lemma_model::ExportLink {
                caller: absolute_address(&member.name, &link.caller),
                provider: absolute_address(&member.name, &link.provider),
            });
        }
        self.lines.push(output::MemberLine::indexed(
            &stored,
            &indexed.summary,
            &indexed.exports,
        ));
        self.members.push(stored);
        Ok(())
    }
}

/// An address as a workspace names it: one written inside `member`'s
/// lemma.yaml (`services.a.op`) gets the member's name.
fn absolute_address(member: &str, address: &str) -> String {
    if address.starts_with('#') {
        return address.to_owned();
    }
    format!("{member}:{address}")
}

/// Clears exports and links a `lemma.yaml` since removed left behind.
fn clear_exports(
    store: &mut Store,
    summary: &lemma_index::IndexSummary,
) -> anyhow::Result<output::ExportCounts> {
    const NO_LEMMA_FILE: &str = "no lemma.yaml";
    let snapshot_id = summary.snapshot_id;
    if store.exports_fingerprint(snapshot_id)?.as_deref() != Some(NO_LEMMA_FILE) {
        store.replace_exports(snapshot_id, NO_LEMMA_FILE, &[], &[])?;
    }
    Ok(output::ExportCounts::default())
}

/// Rebuilds the snapshot's exports and links unless they were built from
/// this `lemma.yaml` and snapshot already; how many there are.
fn update_exports(
    store: &mut Store,
    summary: &lemma_index::IndexSummary,
    source: &lemma_graph::config::LemmaFile,
    root: &Path,
) -> anyhow::Result<output::ExportCounts> {
    let snapshot_id = summary.snapshot_id;
    let mut adapter_warnings = Vec::new();
    // Read every time: a document kept for the code as it is costs a file
    // read, and its warnings are shown on every index, not only the one
    // that rebuilt the exports.
    let from_code = documents_from_code(source, root, &mut adapter_warnings);
    let from_code_digests: Vec<String> = from_code
        .iter()
        .map(|(service, generated)| format!("{service} {}", generated.digest))
        .collect();
    let fingerprint = lemma_model::content_hash(
        // The parsed file, not its text: a workspace member's file is loaded
        // rather than read, and both must give one fingerprint, or moving
        // between the workspace and the member rebuilds the exports each time.
        format!(
            "{}\n{source:?}\n{}\n{}",
            lemma_graph::exports::VERSION,
            summary.snapshot.fingerprint,
            from_code_digests.join("\n")
        )
        .as_bytes(),
    );
    if store.exports_fingerprint(snapshot_id)?.as_deref() != Some(fingerprint.as_str()) {
        let (exports, links) = {
            let unique = |name: &str, from: &str| unique_definition(store, snapshot_id, name, from);
            let enclosing =
                |path: &str, line: u32| enclosing_definition(store, snapshot_id, path, line);
            let top_level = |path: &str| top_level_definitions(store, snapshot_id, path);
            let symbols = lemma_graph::exports::IndexedSymbols {
                lookup: lemma_graph::handlers::Lookup {
                    unique: &unique,
                    enclosing: &enclosing,
                },
                top_level: &top_level,
            };
            lemma_graph::exports::exports(source, root, &symbols, &from_code)?
        };
        store.replace_exports(snapshot_id, &fingerprint, &exports, &links)?;
    }
    let exports = store.exports(snapshot_id)?;
    let count =
        |kind: lemma_model::ExportKind| exports.iter().filter(|export| export.kind == kind).count();
    Ok(output::ExportCounts {
        operations: count(lemma_model::ExportKind::Operation),
        endpoints: count(lemma_model::ExportKind::Endpoint),
        endpoints_at_handlers: exports
            .iter()
            .filter(|export| {
                export.kind == lemma_model::ExportKind::Endpoint && export.symbol.is_some()
            })
            .count(),
        symbols: count(lemma_model::ExportKind::Symbol),
        links: store.export_links(snapshot_id)?.len(),
        adapter_warnings,
    })
}

/// The documents each local service's adapter generates from the code at
/// `root`, by service; what failed or went unread becomes a warning.
fn documents_from_code(
    file: &lemma_graph::config::LemmaFile,
    root: &Path,
    warnings: &mut Vec<String>,
) -> BTreeMap<String, lemma_graph::adapters::Generated> {
    let cache = lemma_graph::repos::cache_directory();
    let mut from_code = BTreeMap::new();
    for (service, generated) in lemma_graph::adapters::from_working_directory(file, root, &cache) {
        match generated {
            Ok(generated) => {
                warnings.extend(unread_warning(&service, &generated.unread));
                from_code.insert(service, generated);
            }
            Err(error) => warnings.push(format!(
                "{service}: could not read its operations from its code, so its committed documents were used: {:#}",
                anyhow::Error::from(error)
            )),
        }
    }
    from_code
}

/// What `service`'s adapter said it could not read, as one warning.
fn unread_warning(service: &str, unread: &[String]) -> Option<String> {
    let first = unread.first()?;
    Some(format!(
        "{service}: its adapter could not read {} part(s) of the code, such as {first}; operations it missed keep their committed definitions",
        unread.len()
    ))
}

/// The function, method or type named `name`, if exactly one is, in the
/// language of `from` and outside tests.
fn unique_definition(
    store: &Store,
    snapshot_id: i64,
    name: &str,
    from: &str,
) -> Option<lemma_graph::handlers::Definition> {
    let filter = SymbolFilter {
        text: name.to_owned(),
        name_match: NameMatch::Exact,
        kind: None,
        language: None,
        path_contains: None,
        limit: 50,
    };
    let family = |path: &str| {
        let extension = path.rsplit_once('.').map_or("", |(_, extension)| extension);
        Language::from_extension(extension).map(|language| match language {
            Language::Tsx => Language::TypeScript,
            other => other,
        })
    };
    let wanted = family(from);
    let found = store.find_symbols(snapshot_id, &filter).ok()?;
    let mut matching = found.into_iter().filter(|symbol| {
        symbol.symbol.name == name
            && !matches!(
                symbol.symbol.kind,
                lemma_model::SymbolKind::Variable | lemma_model::SymbolKind::Constant
            )
            && !lemma_http::is_test_file(&symbol.path)
            && family(&symbol.path) == wanted
    });
    let only = matching.next()?;
    if matching.next().is_some() {
        return None;
    }
    Some(lemma_graph::handlers::Definition {
        path: only.path,
        qualified: only.symbol.qualified_name,
        kind: only.symbol.kind.as_str().to_owned(),
    })
}

/// The innermost definition around a line (counting from 1) of a file.
fn enclosing_definition(
    store: &Store,
    snapshot_id: i64,
    path: &str,
    line: u32,
) -> Option<lemma_graph::handlers::Definition> {
    let zero_based = line.checked_sub(1)?;
    store
        .symbols_in_file(snapshot_id, path)
        .ok()?
        .into_iter()
        .filter(|symbol| {
            symbol.symbol.span.start.line <= zero_based && zero_based <= symbol.symbol.span.end.line
        })
        .max_by_key(|symbol| symbol.symbol.span.start)
        .map(|symbol| lemma_graph::handlers::Definition {
            path: symbol.path,
            qualified: symbol.symbol.qualified_name,
            kind: symbol.symbol.kind.as_str().to_owned(),
        })
}

/// The qualified names of a file's top-level definitions.
fn top_level_definitions(store: &Store, snapshot_id: i64, path: &str) -> Vec<String> {
    store
        .symbols_in_file(snapshot_id, path)
        .unwrap_or_default()
        .into_iter()
        .map(|symbol| symbol.symbol.qualified_name)
        .filter(|qualified| !qualified.contains('.'))
        .collect()
}

/// Opens the index for writing. One written by another version of Lemma is
/// deleted and started afresh: an index can always be rebuilt.
fn store_for_writing<'a>(
    stores: &'a mut HashMap<PathBuf, OpenStore>,
    db_path: &Path,
) -> anyhow::Result<&'a mut Store> {
    match Store::open(db_path) {
        Err(StoreError::SchemaMismatch { .. }) => {
            stores.remove(db_path);
            remove_database(db_path)?;
        }
        Err(error) => {
            return Err(error)
                .with_context(|| format!("could not open index {}", db_path.display()));
        }
        Ok(_) => {}
    }
    open_store(stores, db_path)
}

/// Searches the in-memory names if they match the snapshot. Otherwise
/// starts building them in the background and returns `None`, so this query
/// falls back to SQLite.
fn search_in_memory(
    slot: &Arc<Mutex<SearchSlot>>,
    db_path: &Path,
    store: &Store,
    snapshot: &StoredSnapshot,
    filter: &SymbolFilter,
) -> anyhow::Result<Option<Vec<StoredSymbol>>> {
    let state = lock(slot);
    if let Some(search) = state
        .ready
        .as_ref()
        .filter(|search| search.is_for(snapshot))
    {
        return Ok(Some(search.find(store, filter)?));
    }
    drop(state);
    build_search(slot, db_path, &snapshot.snapshot.fingerprint);
    Ok(None)
}

/// Builds the in-memory search for the database's newest snapshot on a
/// background thread with its own connection, unless one is already being
/// built for this fingerprint.
fn build_search(slot: &Arc<Mutex<SearchSlot>>, db_path: &Path, fingerprint: &str) {
    let mut state = lock(slot);
    if state.building.as_deref() == Some(fingerprint) {
        return;
    }
    state.building = Some(fingerprint.to_owned());
    drop(state);
    let slot = Arc::clone(slot);
    let db_path = db_path.to_owned();
    thread::spawn(move || {
        let built = Store::open(&db_path).ok().and_then(|store| {
            let snapshot = store.latest_snapshot().ok()??;
            SymbolSearch::load(&store, &snapshot).ok()
        });
        let mut state = lock(&slot);
        state.building = None;
        if built.is_some() {
            state.ready = built;
        }
    });
}

fn lock<T>(mutex: &Mutex<T>) -> std::sync::MutexGuard<'_, T> {
    mutex.lock().unwrap_or_else(PoisonError::into_inner)
}

/// The database and the snapshot of the directory it belongs to: the one
/// holding `.lemma/`, whose index may also hold snapshots of worktrees at
/// pinned commits. The newest snapshot for a database kept elsewhere.
fn index_at<'a>(
    stores: &'a mut HashMap<PathBuf, OpenStore>,
    db_path: &Path,
) -> anyhow::Result<(&'a mut Store, StoredSnapshot)> {
    let store = open_store(stores, db_path)?;
    let own_root = db_path
        .parent()
        .filter(|directory| directory.ends_with(LEMMA_DIRECTORY))
        .and_then(Path::parent)
        .and_then(|root| root.canonicalize().ok());
    let own = match own_root {
        Some(root) => store.snapshot_for_root(&root.display().to_string())?,
        None => None,
    };
    let Some(snapshot) = own.map_or_else(|| store.latest_snapshot(), |found| Ok(Some(found)))?
    else {
        bail!(
            "{} has no snapshot; run `lemma index` first",
            db_path.display()
        );
    };
    Ok((store, snapshot))
}

/// The open connection to a database, reopened if the file was replaced.
fn open_store<'a>(
    stores: &'a mut HashMap<PathBuf, OpenStore>,
    db_path: &Path,
) -> anyhow::Result<&'a mut Store> {
    let inode = inode_of(db_path);
    let is_current = stores.get(db_path).is_some_and(|open| open.inode == inode);
    if !is_current {
        let store = Store::open(db_path)
            .with_context(|| format!("could not open index {}", db_path.display()))?;
        let inode = inode_of(db_path);
        stores.insert(db_path.to_owned(), OpenStore { store, inode });
    }
    stores
        .get_mut(db_path)
        .map(|open| &mut open.store)
        .context("the index connection was lost")
}

/// The parts of a command every subcommand needs.
struct Request<'a> {
    invocation: &'a Invocation,
    db: Option<PathBuf>,
    providers_file: Option<PathBuf>,
    format: Format,
}

impl<'a> Request<'a> {
    /// A request from the command line's global options, with paths
    /// resolved against the invocation's directory.
    fn new(cli: &Cli, invocation: &'a Invocation) -> Self {
        Self {
            invocation,
            db: cli.db.as_ref().map(|db| invocation.resolve(db)),
            providers_file: cli.providers.as_ref().map(|file| invocation.resolve(file)),
            format: if cli.json { Format::Json } else { Format::Text },
        }
    }

    /// The given database, or the nearest `.lemma/index.db` from the working
    /// directory upwards.
    fn find_index(&self) -> anyhow::Result<PathBuf> {
        if let Some(db_path) = &self.db {
            return Ok(db_path.clone());
        }
        self.invocation
            .cwd
            .ancestors()
            .map(|directory| directory.join(LEMMA_DIRECTORY).join(INDEX_FILE))
            .find(|candidate| candidate.is_file())
            .context("no index found here or in any parent directory; run `lemma index` first")
    }

    /// Built-in providers plus the given file or the project's own,
    /// resolving executables on the caller's `PATH`.
    fn registry(&self, root: &Path) -> anyhow::Result<ProviderRegistry> {
        self.origin().registry(root)
    }

    fn origin(&self) -> Origin {
        Origin {
            path_env: self.invocation.path_env.clone(),
            cwd: self.invocation.cwd.clone(),
            providers_file: self.providers_file.clone(),
        }
    }
}

/// The symbol a command is about. Prints the candidates and returns
/// `None` when the name matches several.
fn choose_symbol(
    store: &Store,
    snapshot: &StoredSnapshot,
    target: SymbolTarget,
    format: output::Format,
    out: &mut dyn Write,
) -> anyhow::Result<Option<StoredSymbol>> {
    let name = target.symbol;
    match select_symbol(store, snapshot, &name, target.filters)? {
        SymbolResolution::Resolved(symbol) => Ok(Some(symbol)),
        SymbolResolution::Ambiguous(candidates) => {
            output::ambiguous(out, &name, &candidates, format)?;
            Ok(None)
        }
        SymbolResolution::Unresolved => bail!("no indexed symbol matches `{name}`"),
    }
}

/// An export's address selects the symbol implementing it, a `#<id>` one
/// symbol directly; anything else is resolved by name.
fn select_symbol(
    store: &Store,
    snapshot: &StoredSnapshot,
    symbol: &str,
    filters: crate::cli::Filters,
) -> anyhow::Result<SymbolResolution> {
    if let Some(address) = addresses::as_address(symbol) {
        return addresses::resolve_export(store, snapshot, &address);
    }
    // A member's ids are its own index's, so `#billing:607` copied from
    // the workspace names the same symbol inside billing, and nothing in
    // another repository, where 607 is some other symbol.
    if let Some((repository, id)) = qualified_id(symbol) {
        let root = Path::new(&snapshot.snapshot.root);
        let here = root
            .file_name()
            .map(|name| name.to_string_lossy().into_owned())
            .unwrap_or_default();
        if !is_same_repository(&repository, &here) && !is_composed_as(root, &repository) {
            bail!(
                "`{symbol}` is an id in {repository}, but this index is of `{here}`: ids are \
                 numbered per repository, so ask from the workspace or inside {repository}, or \
                 use an id this repository's `lemma symbols` shows"
            );
        }
        let found = store
            .symbol(snapshot.id, id)?
            .filter(|found| matches_filters(found, &filters));
        return Ok(found.map_or(SymbolResolution::Unresolved, SymbolResolution::Resolved));
    }
    if let Some(id) = symbol.strip_prefix('#') {
        let id: i64 = id.parse().with_context(|| {
            format!(
                "`{symbol}` is not a symbol id: ids are written `#607`, or `#billing:607` for a \
                 workspace member"
            )
        })?;
        let found = store
            .symbol(snapshot.id, id)?
            .filter(|found| matches_filters(found, &filters));
        return Ok(found.map_or(SymbolResolution::Unresolved, SymbolResolution::Resolved));
    }
    let prefers_implementable = filters.prefers_implementable;
    let selector = SymbolSelector {
        name: symbol.to_owned(),
        kind: filters.kind,
        language: filters.language,
        path_contains: filters.path,
    };
    let resolution = lemma_query::resolve_symbol(store, snapshot.id, &selector)?;
    if !prefers_implementable {
        return Ok(resolution);
    }
    Ok(narrow_to_implementable(resolution))
}

/// An ambiguous name narrowed to the symbols that can have implementations:
/// interfaces, classes, structs, enums, type aliases and methods. A field
/// named after its type (`store Store`) then no longer hides the type.
/// Unchanged when that leaves nothing.
fn narrow_to_implementable(resolution: SymbolResolution) -> SymbolResolution {
    let SymbolResolution::Ambiguous(candidates) = resolution else {
        return resolution;
    };
    let can_be_implemented = |symbol: &StoredSymbol| {
        matches!(
            symbol.symbol.kind,
            SymbolKind::Interface
                | SymbolKind::Class
                | SymbolKind::Struct
                | SymbolKind::Enum
                | SymbolKind::TypeAlias
                | SymbolKind::Method
        )
    };
    let mut kept: Vec<StoredSymbol> = candidates
        .iter()
        .filter(|s| can_be_implemented(s))
        .cloned()
        .collect();
    // An interface is what implementations are asked about: a struct or
    // class of the same name beside it (Go's `Backend` in the package of
    // `type Backend interface`) implements, rather than is implemented.
    // One elsewhere is unrelated, and the name stays ambiguous.
    let directory = |symbol: &StoredSymbol| {
        Path::new(&symbol.path)
            .parent()
            .map(Path::to_path_buf)
            .unwrap_or_default()
    };
    let interface_directories: Vec<PathBuf> = kept
        .iter()
        .filter(|symbol| symbol.symbol.kind == SymbolKind::Interface)
        .map(directory)
        .collect();
    let is_beside_its_interface = |symbol: &StoredSymbol| {
        symbol.symbol.kind == SymbolKind::Interface
            || interface_directories.contains(&directory(symbol))
    };
    if !interface_directories.is_empty() && kept.iter().all(is_beside_its_interface) {
        kept.retain(|symbol| symbol.symbol.kind == SymbolKind::Interface);
    }
    match kept.len() {
        0 => SymbolResolution::Ambiguous(candidates),
        1 => SymbolResolution::Resolved(kept.remove(0)),
        _ => SymbolResolution::Ambiguous(kept),
    }
}

/// Whether a workspace's name for a member (`#web_app`) names the
/// repository in directory `directory` (`web-app`): workspaces write
/// names with `_` where directories often have `-`.
fn is_same_repository(name: &str, directory: &str) -> bool {
    let normal = |text: &str| {
        text.trim_start_matches('#')
            .to_ascii_lowercase()
            .replace('-', "_")
    };
    normal(name) == normal(directory)
}

/// Whether a workspace in a directory above `root` composes the repository
/// at `root` under `name`, as `repo billing = local("../billing-service")`
/// does, whatever its directory is called. A worktree of the member's
/// clone is the same repository.
fn is_composed_as(root: &Path, name: &str) -> bool {
    let wanted = name.trim_start_matches('#');
    let cache = lemma_graph::repos::cache_directory();
    let manifests = root
        .ancestors()
        .skip(1)
        .take(4)
        .flat_map(|directory| {
            ["mod.lm", lemma_graph::config::LEMMA_FILE].map(|file| directory.join(file))
        })
        .filter(|file| file.exists());
    for manifest in manifests {
        let Ok(members) = lemma_graph::compose::members(&manifest, &cache) else {
            continue;
        };
        let member = members
            .iter()
            .find(|member| member.name.trim_start_matches('#') == wanted);
        if let Some(member) = member {
            return same_clone(&member.root, root);
        }
    }
    false
}

/// Whether two directories are one clone, or worktrees of one clone.
fn same_clone(left: &Path, right: &Path) -> bool {
    let common = |directory: &Path| {
        lemma_graph::repos::git_common_directory(directory)
            .ok()
            .and_then(|common| std::fs::canonicalize(common).ok())
    };
    let canonical = |directory: &Path| std::fs::canonicalize(directory).ok();
    canonical(left).is_some_and(|left| Some(left) == canonical(right))
        || common(left).is_some_and(|left| Some(left) == common(right))
}

/// `filter` for searching inside `member` when its path starts with the
/// member's directory or name, the rest of the path applied inside it;
/// `None` when the path names no member.
fn member_filter(member: &StoredMember, filter: &SymbolFilter) -> Option<SymbolFilter> {
    let path = filter.path_contains.as_deref()?;
    let path = path.trim_start_matches("./");
    let directory = Path::new(&member.root)
        .file_name()
        .map(|name| name.to_string_lossy().into_owned())
        .unwrap_or_default();
    let name = member.name.trim_start_matches('#');
    [directory.as_str(), name, member.name.as_str()]
        .into_iter()
        .filter(|prefix| !prefix.is_empty())
        .find_map(|prefix| {
            let rest = path.strip_prefix(prefix)?;
            let rest = match rest.strip_prefix(['/', ':']) {
                Some(rest) => rest,
                None if rest.is_empty() => rest,
                None => return None,
            };
            Some(SymbolFilter {
                path_contains: (!rest.is_empty()).then(|| rest.to_owned()),
                ..filter.clone()
            })
        })
}

/// Whether `symbol` passes `filters`, as a symbol found by name must.
fn matches_filters(symbol: &StoredSymbol, filters: &crate::cli::Filters) -> bool {
    filters.kind.is_none_or(|kind| symbol.symbol.kind == kind)
        && filters
            .language
            .is_none_or(|language| symbol.language == language)
        && filters
            .path
            .as_deref()
            .is_none_or(|path| symbol.path.contains(path))
}

/// A repository-qualified symbol id: `#billing:607`, also written
/// `#billing:#607` or `#billing#607`. The repository keeps its `#`.
fn qualified_id(text: &str) -> Option<(String, i64)> {
    let rest = text.strip_prefix('#')?;
    let split = rest.find([':', '#'])?;
    let (name, id) = (&rest[..split], &rest[split + 1..]);
    let id = id.strip_prefix('#').unwrap_or(id).parse().ok()?;
    (!name.is_empty()).then(|| (format!("#{name}"), id))
}

/// How far a graph follows callers across a workspace.
#[derive(Clone, Copy)]
struct Across {
    direction: lemma_query::Direction,
    /// Calls from other code to follow (`--across`).
    calls: usize,
}

/// Adds to `graph` what its callers reach beyond their repository: the
/// operations their handlers serve and the code elsewhere calling them.
fn add_system_trace(
    graph: &mut lemma_model::UseGraph,
    workspace: &Path,
    index_root: &Path,
    across: Across,
) -> anyhow::Result<()> {
    if across.direction == lemma_query::Direction::Callees {
        graph
            .notes
            .push("--workspace follows callers; it adds nothing to callees".to_owned());
        return Ok(());
    }
    let reached: Vec<lemma_graph::Reached> = graph
        .nodes
        .iter()
        .filter(|node| node.id.starts_with("symbol:"))
        .filter_map(|node| {
            Some(lemma_graph::Reached {
                node: node.id.clone(),
                file: node.path.clone()?,
                line: node.line?,
            })
        })
        .collect();
    let clones = lemma_graph::repos::cache_directory();
    let facts = crate::verify::facts_cache();
    let caches = lemma_graph::TraceCaches {
        clones: &clones,
        facts: facts.as_ref(),
    };
    let trace = match lemma_souffle::SouffleEngine::find() {
        Ok(souffle) => {
            lemma_graph::trace_through_system(workspace, index_root, &reached, &caches, &souffle)?
        }
        Err(_) => lemma_graph::trace_through_system(
            workspace,
            index_root,
            &reached,
            &caches,
            &lemma_logic::ReferenceEngine,
        )?,
    };
    let starts = reached
        .iter()
        .map(|function| function.node.clone())
        .collect();
    let trace = trace.within_calls(&starts, across.calls);
    for node in trace.nodes {
        if !graph.nodes.iter().any(|existing| existing.id == node.id) {
            graph.nodes.push(node);
        }
    }
    graph.relationships.extend(trace.relationships);
    lemma_model::dedup_uses(&mut graph.relationships);
    graph.notes.extend(trace.notes);
    Ok(())
}

fn graph_options(
    direction: GraphDirection,
    depth: usize,
    source: GraphSourceArg,
    max_nodes: usize,
    only: Vec<SymbolKind>,
) -> lemma_query::GraphOptions {
    lemma_query::GraphOptions {
        direction: match direction {
            GraphDirection::Callers => lemma_query::Direction::Callers,
            GraphDirection::Callees => lemma_query::Direction::Callees,
            GraphDirection::Both => lemma_query::Direction::Both,
        },
        depth,
        source: match source {
            GraphSourceArg::Lsp => lemma_query::GraphSource::Lsp,
            GraphSourceArg::Names => lemma_query::GraphSource::Names,
        },
        max_nodes,
        only,
    }
}

fn lookup_options(args: &LookupArgs) -> LookupOptions {
    LookupOptions {
        refresh: args.refresh,
        request_timeout: Duration::from_secs(args.timeout_secs),
    }
}

/// 0 when a provider answered, with something or nothing.
fn exit_code<T>(outcome: &Outcome<T>) -> u8 {
    if outcome.is_answered() {
        0
    } else {
        EXIT_NO_ANSWER
    }
}

/// A file as the index names it: relative to the snapshot root, with `/`.
/// Where to look a file up: an index, its snapshot, the file's path in it,
/// and, from a workspace, the member holding it.
struct FileIndex {
    db_path: PathBuf,
    snapshot: StoredSnapshot,
    relative: String,
    member: Option<String>,
}

/// The workspace member holding `file`, in its clone or in the worktree it
/// is indexed in, and the file's path inside it. The deepest root wins, as
/// members can be nested.
fn member_file<'m>(members: &'m [StoredMember], file: &Path) -> Option<(&'m StoredMember, String)> {
    let file = file.canonicalize().ok()?;
    members
        .iter()
        .flat_map(|member| [(member, &member.indexed_root), (member, &member.root)])
        .filter_map(|(member, root)| {
            let root = Path::new(root).canonicalize().ok()?;
            let inside = file.strip_prefix(&root).ok()?;
            let parts: Vec<String> = inside
                .components()
                .map(|part| part.as_os_str().to_string_lossy().into_owned())
                .collect();
            Some((root.components().count(), member, parts.join("/")))
        })
        .max_by_key(|(depth, _, _)| *depth)
        .map(|(_, member, relative)| (member, relative))
}

fn relative_to_root(snapshot: &StoredSnapshot, file: &Path) -> anyhow::Result<String> {
    let file = file
        .canonicalize()
        .with_context(|| format!("cannot read {}", file.display()))?;
    let root = Path::new(&snapshot.snapshot.root);
    let relative = file.strip_prefix(root).with_context(|| {
        format!(
            "{} is outside the indexed directory {}",
            file.display(),
            root.display()
        )
    })?;
    let parts: Vec<_> = relative
        .components()
        .map(|part| part.as_os_str().to_string_lossy())
        .collect();
    Ok(parts.join("/"))
}

/// Splits `file:line:column`, counted from 1, into a path and a zero-based
/// position.
fn parse_location(location: &str) -> anyhow::Result<(PathBuf, Position)> {
    let invalid = || format!("`{location}` is not `file:line:column`");
    let (rest, column) = location.rsplit_once(':').with_context(invalid)?;
    let (path, line) = rest.rsplit_once(':').with_context(invalid)?;
    let line: u32 = line.parse().with_context(invalid)?;
    let column: u32 = column.parse().with_context(invalid)?;
    if path.is_empty() || line == 0 || column == 0 {
        bail!("{}; lines and columns count from 1", invalid());
    }
    let position = Position {
        line: line - 1,
        column: column - 1,
    };
    Ok((PathBuf::from(path), position))
}

/// `45s`, `12m`, `3h`: the largest whole unit.
fn format_duration(duration: Duration) -> String {
    let seconds = duration.as_secs();
    match seconds {
        0..60 => format!("{seconds}s"),
        60..3600 => format!("{}m", seconds / 60),
        _ => format!("{}h", seconds / 3600),
    }
}

/// `.lemma/index.db` under the indexed directory, creating the directory.
fn default_index_path(root: &Path) -> anyhow::Result<PathBuf> {
    let directory = root.join(LEMMA_DIRECTORY);
    fs::create_dir_all(&directory)
        .with_context(|| format!("could not create {}", directory.display()))?;
    Ok(directory.join(INDEX_FILE))
}

fn remove_database(db_path: &Path) -> anyhow::Result<()> {
    for suffix in ["", "-wal", "-shm"] {
        let mut file = db_path.as_os_str().to_owned();
        file.push(suffix);
        let file = PathBuf::from(file);
        if file.exists() {
            fs::remove_file(&file)
                .with_context(|| format!("could not remove {}", file.display()))?;
        }
    }
    Ok(())
}

#[cfg(unix)]
fn inode_of(path: &Path) -> Option<u64> {
    use std::os::unix::fs::MetadataExt;
    fs::metadata(path).ok().map(|metadata| metadata.ino())
}

#[cfg(not(unix))]
fn inode_of(_path: &Path) -> Option<u64> {
    None
}

#[cfg(test)]
mod tests {
    use super::*;

    fn candidate(id: i64, name: &str, kind: SymbolKind) -> StoredSymbol {
        let position = Position { line: 0, column: 0 };
        let span = lemma_model::Span {
            start: position,
            end: position,
        };
        StoredSymbol {
            id,
            path: "store.go".to_owned(),
            language: Language::Go,
            symbol: lemma_model::Symbol {
                name: name.rsplit('.').next().unwrap_or(name).to_owned(),
                qualified_name: name.to_owned(),
                kind,
                span,
                name_span: span,
            },
        }
    }

    #[test]
    fn a_field_named_after_its_type_does_not_hide_the_type() {
        let ambiguous = SymbolResolution::Ambiguous(vec![
            candidate(1, "Store", SymbolKind::Interface),
            candidate(2, "Server.Store", SymbolKind::Field),
        ]);

        let narrowed = narrow_to_implementable(ambiguous);

        assert!(matches!(narrowed, SymbolResolution::Resolved(symbol) if symbol.id == 1));
    }

    #[test]
    fn a_members_name_matches_its_directory_with_dashes_or_underscores() {
        assert!(is_same_repository("#web_app", "web-app"));
        assert!(is_same_repository("#functions", "functions"));
        assert!(!is_same_repository("#platform", "functions"));
    }

    #[test]
    fn an_interface_wins_over_a_struct_of_the_same_name() {
        let ambiguous = SymbolResolution::Ambiguous(vec![
            candidate(1, "Backend", SymbolKind::Struct),
            candidate(2, "Backend", SymbolKind::Interface),
        ]);

        let narrowed = narrow_to_implementable(ambiguous);

        assert!(matches!(narrowed, SymbolResolution::Resolved(symbol) if symbol.id == 2));
    }

    #[test]
    fn a_struct_in_another_package_leaves_the_name_ambiguous() {
        let mut elsewhere = candidate(1, "Backend", SymbolKind::Struct);
        elsewhere.path = "storage/backend.go".to_owned();
        let ambiguous = SymbolResolution::Ambiguous(vec![
            elsewhere,
            candidate(2, "Backend", SymbolKind::Interface),
        ]);

        let narrowed = narrow_to_implementable(ambiguous);

        assert!(matches!(narrowed, SymbolResolution::Ambiguous(several) if several.len() == 2));
    }

    #[test]
    fn a_name_with_no_implementable_symbol_stays_ambiguous() {
        let ambiguous = SymbolResolution::Ambiguous(vec![
            candidate(1, "A.store", SymbolKind::Field),
            candidate(2, "B.store", SymbolKind::Field),
        ]);

        let narrowed = narrow_to_implementable(ambiguous);

        assert!(matches!(narrowed, SymbolResolution::Ambiguous(several) if several.len() == 2));
    }

    #[test]
    fn repository_qualified_ids_are_read_in_every_written_form() {
        let forms = ["#billing:607", "#billing:#607", "#billing#607"];

        let read: Vec<Option<(String, i64)>> =
            forms.iter().map(|form| qualified_id(form)).collect();

        assert!(
            read.iter()
                .all(|id| *id == Some(("#billing".to_owned(), 607)))
        );
        assert_eq!(qualified_id("#607"), None);
        assert_eq!(qualified_id("#billing:services.api.create"), None);
    }

    #[test]
    fn a_member_is_known_by_the_workspace_composing_it_whatever_its_directory() {
        let workspace = tempfile::tempdir().unwrap();
        let git = |directory: &Path, args: &[&str]| {
            let status = std::process::Command::new("git")
                .arg("-C")
                .arg(directory)
                .args(["-c", "user.name=t", "-c", "user.email=t@t"])
                .args(args)
                .status()
                .unwrap();
            assert!(status.success());
        };
        let billing = workspace.path().join("billing-service");
        std::fs::create_dir_all(&billing).unwrap();
        std::fs::write(billing.join("mod.lm"), "module billing\n").unwrap();
        git(&billing, &["init", "-q"]);
        git(&billing, &["add", "."]);
        git(&billing, &["commit", "-q", "-m", "billing"]);
        let worktree = workspace.path().join("billing-feature");
        let target = worktree.display().to_string();
        git(&billing, &["worktree", "add", "-q", "--detach", &target]);
        std::fs::write(
            workspace.path().join("mod.lm"),
            "module shop\n\nworkspace {\n    repo billing = local(\"billing-service\")\n}\n",
        )
        .unwrap();

        assert!(is_composed_as(&billing, "#billing"));
        assert!(is_composed_as(&worktree, "#billing"));
        assert!(!is_composed_as(&billing, "#web"));
    }

    #[test]
    fn a_search_path_naming_a_member_searches_inside_it() {
        let member = StoredMember {
            name: "#platform".to_owned(),
            root: "/work/platform".to_owned(),
            revision: None,
            indexed_root: "/work/platform".to_owned(),
            database: String::new(),
        };
        let filter = |path: &str| SymbolFilter {
            text: "Session".to_owned(),
            name_match: lemma_store::NameMatch::Contains,
            kind: None,
            language: None,
            path_contains: Some(path.to_owned()),
            limit: 10,
        };
        let inside = |path: &str| member_filter(&member, &filter(path)).map(|f| f.path_contains);

        assert_eq!(inside("platform"), Some(None));
        assert_eq!(
            inside("platform/apps/api"),
            Some(Some("apps/api".to_owned()))
        );
        assert_eq!(inside("#platform:apps"), Some(Some("apps".to_owned())));
        assert_eq!(inside("apps/api"), None);
        assert_eq!(inside("platforms"), None);
    }

    #[test]
    fn a_workspace_path_maps_onto_the_member_holding_it() {
        let workspace = tempfile::tempdir().unwrap();
        let platform = workspace.path().join("repos/platform");
        let worktree = workspace.path().join("worktrees/platform");
        std::fs::create_dir_all(platform.join("apps/worker")).unwrap();
        std::fs::create_dir_all(&worktree).unwrap();
        let file = platform.join("apps/worker/execute.go");
        std::fs::write(&file, "package worker\n").unwrap();
        let member = |name: &str, root: &Path, indexed: &Path| StoredMember {
            name: name.to_owned(),
            root: root.display().to_string(),
            revision: None,
            indexed_root: indexed.display().to_string(),
            database: String::new(),
        };
        let members = [
            member(
                "#web",
                &workspace.path().join("repos/web"),
                &workspace.path().join("repos/web"),
            ),
            member("#platform", &platform, &worktree),
        ];

        let (found, relative) = member_file(&members, &file).unwrap();

        assert_eq!(found.name, "#platform");
        assert_eq!(relative, "apps/worker/execute.go");
        assert!(member_file(&members, workspace.path()).is_none());
    }

    #[test]
    fn locations_count_from_one() {
        let (path, position) = parse_location("src/app.py:12:5").unwrap();

        assert_eq!(path, PathBuf::from("src/app.py"));
        assert_eq!(
            position,
            Position {
                line: 11,
                column: 4
            }
        );
    }

    #[test]
    fn locations_need_a_line_and_column() {
        assert!(parse_location("src/app.py:12").is_err());
        assert!(parse_location("src/app.py:0:1").is_err());
    }
}
