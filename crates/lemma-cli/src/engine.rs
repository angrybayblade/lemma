//! Running commands, with whatever a long-lived process can keep warm
//! between them: open databases and running language servers.
//!
//! The same code runs a command in this process or inside the daemon. Paths
//! and the executable search path come from the [`Invocation`], never from
//! the process environment, because the daemon serves commands started from
//! many directories and shells.

use std::collections::HashMap;
use std::ffi::OsString;
use std::fmt::Write as _;
use std::fs;
use std::io::Write;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex, PoisonError};
use std::thread;
use std::time::{Duration, Instant};

use anyhow::{Context, bail};
use lemma_index::LiveIndex;
use lemma_model::{Language, Outcome, Position};
use lemma_providers::ProviderRegistry;
use lemma_query::{
    IdlePolicy, LookupContext, LookupOptions, ServerState, SessionPool, SymbolResolution,
    SymbolSearch, SymbolSelector, WarmUp,
};
use lemma_store::{
    FactStore, NameMatch, Store, StoreError, StoredSnapshot, StoredSymbol, SymbolFilter,
};

use crate::addresses;
use crate::cli::{
    Cli, Command, DefinitionArgs, GraphArgs, GraphDirection, GraphDrawing, GraphSourceArg,
    LookupArgs, RefsArgs, SymbolLookupArgs, SymbolTarget, SymbolsArgs,
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
        let format = if cli.json { Format::Json } else { Format::Text };
        let db = cli.db.map(|db| invocation.resolve(&db));
        let providers_file = cli.providers.map(|file| invocation.resolve(&file));
        let request = Request {
            invocation,
            db,
            providers_file,
            format,
        };
        match cli.command {
            Command::Index { path } => self.index(&request, &invocation.resolve(&path), out),
            Command::Symbols(args) => self.symbols(&request, args, out),
            Command::Outline { path } => self.outline(&request, &path, out),
            Command::Refs(args) => self.references(&request, args, out),
            Command::Definition(args) => self.definition(&request, &args, out),
            Command::Implementations(args) => self.implementations(&request, args, out),
            Command::Signature(args) => self.signature(&request, args, out),
            Command::Graph(args) => self.graph(&request, args, out),
            Command::Providers => self.providers(&request, out),
            Command::Daemon(_) => bail!("`lemma daemon` cannot run inside the daemon"),
            Command::Mcp => bail!("`lemma mcp` cannot run inside the daemon"),
            Command::Verify(_) => bail!("`lemma verify` runs in its own process"),
            Command::Check(_) => bail!("`lemma check` runs in its own process"),
        }
    }

    fn index(
        &mut self,
        request: &Request<'_>,
        path: &Path,
        out: &mut dyn Write,
    ) -> anyhow::Result<u8> {
        let db_path = match &request.db {
            Some(db) => db.clone(),
            None => default_index_path(path)?,
        };
        let root = path
            .canonicalize()
            .with_context(|| format!("cannot index {}", path.display()))?;
        let lemma_source = selection::read_lemma_file(&root)?;
        let selection = selection::repository_selection(&lemma_source.file)?;
        let key = (db_path.clone(), root);
        // A changed selection starts a fresh index, whose first run walks
        // the whole tree and so drops files no longer selected.
        let is_current = self
            .live_indexes
            .get(&key)
            .is_some_and(|live| live.selection().description() == selection.description());
        if !is_current {
            // Only a long-lived engine benefits from watching the tree.
            let live = if self.warms_up_after_index {
                LiveIndex::watched(&key.1, selection)?
            } else {
                LiveIndex::selecting(&key.1, selection)?
            };
            self.live_indexes.insert(key.clone(), live);
        }
        let store = store_for_writing(&mut self.stores, &db_path)?;
        let live = self
            .live_indexes
            .get_mut(&key)
            .context("the live index was lost")?;
        let summary = live.index(store)?;
        let exports = update_exports(store, &summary, &lemma_source, &key.1)?;
        output::index_summary(out, &summary, &exports, &db_path, request.format)?;
        if self.warms_up_after_index {
            let origin = request.origin();
            let languages: Vec<Language> = summary.files_by_language.keys().copied().collect();
            self.warm_up(&db_path, &key.1, &languages, &origin)?;
            self.origins.insert(key, origin);
            let slot = self.searches.entry(db_path.clone()).or_default();
            build_search(slot, &db_path, &summary.snapshot.fingerprint);
        }
        Ok(0)
    }

    fn symbols(
        &mut self,
        request: &Request<'_>,
        args: SymbolsArgs,
        out: &mut dyn Write,
    ) -> anyhow::Result<u8> {
        let db_path = request.find_index()?;
        let (store, snapshot) = index_at(&mut self.stores, &db_path)?;
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
        let db_path = request.find_index()?;
        let (store, snapshot) = index_at(&mut self.stores, &db_path)?;
        let relative = relative_to_root(&snapshot, &request.invocation.resolve(path))?;
        let symbols = lemma_query::symbols_in_file(store, snapshot.id, &relative)?;
        output::outline(out, &relative, &symbols, request.format)?;
        Ok(0)
    }

    fn references(
        &mut self,
        request: &Request<'_>,
        args: RefsArgs,
        out: &mut dyn Write,
    ) -> anyhow::Result<u8> {
        let db_path = request.find_index()?;
        let (store, snapshot) = index_at(&mut self.stores, &db_path)?;
        let Some(target) = choose_symbol(store, &snapshot, args.target, request.format, out)?
        else {
            return Ok(EXIT_AMBIGUOUS);
        };
        let root = snapshot.snapshot.root.clone();
        let registry = request.registry(Path::new(&root))?;
        let options = lookup_options(&args.lookup);
        let context = LookupContext {
            snapshot: &snapshot,
            registry: &registry,
            options: &options,
        };
        let report = lemma_query::find_references(
            store,
            &context,
            target,
            args.include_declaration,
            &mut self.pool,
        )?;
        output::reference_report(out, &report, &root, request.format)?;
        Ok(exit_code(&report.answer.outcome))
    }

    fn definition(
        &mut self,
        request: &Request<'_>,
        args: &DefinitionArgs,
        out: &mut dyn Write,
    ) -> anyhow::Result<u8> {
        let (path, position) = parse_location(&args.location)?;
        let db_path = request.find_index()?;
        let (store, snapshot) = index_at(&mut self.stores, &db_path)?;
        let relative = relative_to_root(&snapshot, &request.invocation.resolve(&path))?;
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
        args: SymbolLookupArgs,
        out: &mut dyn Write,
    ) -> anyhow::Result<u8> {
        let db_path = request.find_index()?;
        let (store, snapshot) = index_at(&mut self.stores, &db_path)?;
        let Some(target) = choose_symbol(store, &snapshot, args.target, request.format, out)?
        else {
            return Ok(EXIT_AMBIGUOUS);
        };
        let registry = request.registry(Path::new(&snapshot.snapshot.root))?;
        let options = lookup_options(&args.lookup);
        let context = LookupContext {
            snapshot: &snapshot,
            registry: &registry,
            options: &options,
        };
        let report = lemma_query::find_implementations(store, &context, target, &mut self.pool)?;
        output::implementation_report(out, &report, request.format)?;
        Ok(exit_code(&report.answer.outcome))
    }

    fn signature(
        &mut self,
        request: &Request<'_>,
        args: SymbolLookupArgs,
        out: &mut dyn Write,
    ) -> anyhow::Result<u8> {
        let db_path = request.find_index()?;
        let (store, snapshot) = index_at(&mut self.stores, &db_path)?;
        let Some(target) = choose_symbol(store, &snapshot, args.target, request.format, out)?
        else {
            return Ok(EXIT_AMBIGUOUS);
        };
        let registry = request.registry(Path::new(&snapshot.snapshot.root))?;
        let options = lookup_options(&args.lookup);
        let context = LookupContext {
            snapshot: &snapshot,
            registry: &registry,
            options: &options,
        };
        let report = lemma_query::signature_of(store, &context, target, &mut self.pool)?;
        output::signature_report(out, &report, request.format)?;
        let is_described = report.declaration.is_some() || report.hover.outcome.is_answered();
        Ok(if is_described { 0 } else { EXIT_NO_ANSWER })
    }

    fn graph(
        &mut self,
        request: &Request<'_>,
        args: GraphArgs,
        out: &mut dyn Write,
    ) -> anyhow::Result<u8> {
        let db_path = request.find_index()?;
        let (store, snapshot) = index_at(&mut self.stores, &db_path)?;
        let Some(target) = choose_symbol(store, &snapshot, args.target, request.format, out)?
        else {
            return Ok(EXIT_AMBIGUOUS);
        };
        let registry = request.registry(Path::new(&snapshot.snapshot.root))?;
        let options = lookup_options(&args.lookup);
        let context = LookupContext {
            snapshot: &snapshot,
            registry: &registry,
            options: &options,
        };
        let graph_options = graph_options(args.direction, args.depth, args.source, args.max_nodes);
        let mut graph =
            lemma_query::symbol_graph(store, &context, &target, &graph_options, &mut self.pool)?;
        if graph_options.direction != lemma_query::Direction::Callers {
            let exports = store.exports(snapshot.id)?;
            let links = store.export_links(snapshot.id)?;
            addresses::add_links(&mut graph, &exports, &links);
        }
        if let Some(workspace) = &args.workspace {
            let workspace = request.invocation.resolve(workspace);
            let root = PathBuf::from(&snapshot.snapshot.root);
            add_system_trace(&mut graph, &workspace, &root, graph_options.direction)?;
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
        output::providers(out, &registry, request.format)?;
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
            let Some(provider) = registry.installed(*language).into_iter().next() else {
                continue;
            };
            self.pool.warm_up(WarmUp {
                db_path: db_path.to_owned(),
                snapshot: snapshot.clone(),
                provider,
                language: *language,
                request_timeout: WARM_UP_TIMEOUT,
            });
        }
        Ok(())
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

/// Rebuilds the snapshot's exports and links unless they were built from
/// this `lemma.yaml` and snapshot already; how many there are.
fn update_exports(
    store: &mut Store,
    summary: &lemma_index::IndexSummary,
    source: &selection::LemmaSource,
    root: &Path,
) -> anyhow::Result<output::ExportCounts> {
    let snapshot_id = summary.snapshot_id;
    let fingerprint = lemma_model::content_hash(
        format!(
            "{}\n{}\n{}",
            lemma_graph::exports::VERSION,
            source.text,
            summary.snapshot.fingerprint
        )
        .as_bytes(),
    );
    if store.exports_fingerprint(snapshot_id)?.as_deref() != Some(fingerprint.as_str()) {
        let (exports, links) = {
            let unique = |name: &str| unique_definition(store, snapshot_id, name);
            let top_level = |path: &str| top_level_definitions(store, snapshot_id, path);
            let symbols = lemma_graph::exports::IndexedSymbols {
                unique: &unique,
                top_level: &top_level,
            };
            lemma_graph::exports::exports(&source.file, root, &symbols)?
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
    })
}

/// The function, method or type named `name`, if exactly one is.
fn unique_definition(store: &Store, snapshot_id: i64, name: &str) -> Option<(String, String)> {
    let filter = SymbolFilter {
        text: name.to_owned(),
        name_match: NameMatch::Exact,
        kind: None,
        language: None,
        path_contains: None,
        limit: 3,
    };
    let found = store.find_symbols(snapshot_id, &filter).ok()?;
    let mut matching = found.into_iter().filter(|symbol| {
        symbol.symbol.name == name
            && !matches!(
                symbol.symbol.kind,
                lemma_model::SymbolKind::Variable | lemma_model::SymbolKind::Constant
            )
    });
    let only = matching.next()?;
    if matching.next().is_some() {
        return None;
    }
    Some((only.path, only.symbol.qualified_name))
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

/// The database and its newest snapshot.
fn index_at<'a>(
    stores: &'a mut HashMap<PathBuf, OpenStore>,
    db_path: &Path,
) -> anyhow::Result<(&'a mut Store, StoredSnapshot)> {
    let store = open_store(stores, db_path)?;
    let Some(snapshot) = store.latest_snapshot()? else {
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

impl Request<'_> {
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
    if let Some(id) = symbol.strip_prefix('#') {
        let id: i64 = id
            .parse()
            .with_context(|| format!("`{symbol}` is not a valid symbol id"))?;
        let found: Option<StoredSymbol> = store.symbol(snapshot.id, id)?;
        return Ok(found.map_or(SymbolResolution::Unresolved, SymbolResolution::Resolved));
    }
    let selector = SymbolSelector {
        name: symbol.to_owned(),
        kind: filters.kind,
        language: filters.language,
        path_contains: filters.path,
    };
    Ok(lemma_query::resolve_symbol(store, snapshot.id, &selector)?)
}

/// Adds to `graph` what its callers reach beyond their repository: the
/// operations their handlers serve and the code elsewhere calling them.
fn add_system_trace(
    graph: &mut lemma_model::UseGraph,
    workspace: &Path,
    index_root: &Path,
    direction: lemma_query::Direction,
) -> anyhow::Result<()> {
    if direction == lemma_query::Direction::Callees {
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
    let cache = lemma_graph::repos::cache_directory();
    let trace = match lemma_souffle::SouffleEngine::find() {
        Ok(souffle) => {
            lemma_graph::trace_through_system(workspace, index_root, &reached, &cache, &souffle)?
        }
        Err(_) => lemma_graph::trace_through_system(
            workspace,
            index_root,
            &reached,
            &cache,
            &lemma_logic::ReferenceEngine,
        )?,
    };
    for node in trace.nodes {
        if !graph.nodes.iter().any(|existing| existing.id == node.id) {
            graph.nodes.push(node);
        }
    }
    graph.relationships.extend(trace.relationships);
    graph.relationships.sort();
    graph.relationships.dedup();
    graph.notes.extend(trace.notes);
    Ok(())
}

fn graph_options(
    direction: GraphDirection,
    depth: usize,
    source: GraphSourceArg,
    max_nodes: usize,
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
