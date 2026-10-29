//! The `lemma` command-line tool.

mod output;

use std::fs;
use std::path::{Path, PathBuf};
use std::process::ExitCode;
use std::time::Duration;

use anyhow::{Context, bail};
use clap::{Args, Parser, Subcommand};
use lemma_model::{Language, ReferenceOutcome, SymbolKind};
use lemma_providers::ProviderRegistry;
use lemma_query::{LookupContext, ReferenceOptions, SessionPool, SymbolResolution, SymbolSelector};
use lemma_store::{NameMatch, Store, StoredSnapshot, StoredSymbol, SymbolFilter};

use crate::output::Format;

/// Directory, relative to an indexed root, where Lemma keeps its files.
const LEMMA_DIRECTORY: &str = ".lemma";
const INDEX_FILE: &str = "index.db";
const PROVIDERS_FILE: &str = "providers.toml";

/// Exit code when a name matches several symbols.
const EXIT_AMBIGUOUS: u8 = 2;
/// Exit code when a lookup ran but could not produce an answer.
const EXIT_NO_ANSWER: u8 = 3;

#[derive(Parser)]
#[command(
    name = "lemma",
    version,
    about = "Index code and query symbol references"
)]
struct Cli {
    /// Index database to use. Defaults to `.lemma/index.db` in the nearest
    /// directory that has one.
    #[arg(long, global = true)]
    db: Option<PathBuf>,

    /// Provider configuration to use instead of the indexed project's
    /// `.lemma/providers.toml`. Built-in providers are still available.
    #[arg(long, global = true)]
    providers: Option<PathBuf>,

    /// Print JSON instead of text. Positions are zero-based and columns
    /// count UTF-8 bytes.
    #[arg(long, global = true)]
    json: bool,

    #[command(subcommand)]
    command: Command,
}

#[derive(Subcommand)]
enum Command {
    /// Index source files and symbols under a directory.
    Index {
        /// Directory to index.
        #[arg(default_value = ".")]
        path: PathBuf,
    },
    /// Search indexed symbols by name.
    Symbols(SymbolsArgs),
    /// Find references to a symbol with its language's provider.
    ///
    /// Exits with 2 if the name matches several symbols and 3 if no provider
    /// could answer.
    Refs(RefsArgs),
    /// List configured providers and whether each is installed.
    Providers,
}

#[derive(Args)]
struct SymbolsArgs {
    /// Text to look for in names and qualified names.
    query: String,
    /// Match the whole name instead of any part of it.
    #[arg(long)]
    exact: bool,
    #[command(flatten)]
    filters: Filters,
    /// Maximum number of results.
    #[arg(long, default_value_t = 50)]
    limit: u32,
}

#[derive(Args)]
struct RefsArgs {
    /// A name, a qualified name such as `Router.delete`, or `#<id>` from
    /// `lemma symbols`.
    symbol: String,
    #[command(flatten)]
    filters: Filters,
    /// Count the definition itself as a reference.
    #[arg(long)]
    include_declaration: bool,
    /// Ask the provider again instead of using a cached answer.
    #[arg(long)]
    refresh: bool,
    /// Seconds to wait for the provider to start and to answer.
    #[arg(long, default_value_t = 60)]
    timeout_secs: u64,
}

/// Narrows which symbols a name refers to.
#[derive(Args)]
struct Filters {
    /// Only symbols of this kind, such as `function` or `method`.
    #[arg(long)]
    kind: Option<SymbolKind>,
    /// Only symbols in this language: typescript, tsx, python, or go.
    #[arg(long)]
    language: Option<Language>,
    /// Only symbols whose file path contains this text.
    #[arg(long)]
    path: Option<String>,
}

fn main() -> ExitCode {
    let cli = Cli::parse();
    let format = if cli.json { Format::Json } else { Format::Text };
    match run(cli, format) {
        Ok(code) => code,
        Err(error) => {
            eprintln!("error: {error:#}");
            ExitCode::FAILURE
        }
    }
}

fn run(cli: Cli, format: Format) -> anyhow::Result<ExitCode> {
    match cli.command {
        Command::Index { path } => index(&path, cli.db, format),
        Command::Symbols(args) => symbols(args, cli.db, format),
        Command::Refs(args) => references(args, cli.db, cli.providers.as_deref(), format),
        Command::Providers => providers(cli.db, cli.providers.as_deref(), format),
    }
}

fn index(path: &Path, db: Option<PathBuf>, format: Format) -> anyhow::Result<ExitCode> {
    let db_path = db.map_or_else(|| default_index_path(path), Ok)?;
    let mut store = open_store(&db_path)?;
    let summary = lemma_index::index_directory(path, &mut store)?;
    output::index_summary(&summary, &db_path, format)?;
    Ok(ExitCode::SUCCESS)
}

fn symbols(args: SymbolsArgs, db: Option<PathBuf>, format: Format) -> anyhow::Result<ExitCode> {
    let (store, snapshot) = open_index(db)?;
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
    let found = store.find_symbols(snapshot.id, &filter)?;
    output::symbols(&found, format)?;
    Ok(ExitCode::SUCCESS)
}

fn references(
    args: RefsArgs,
    db: Option<PathBuf>,
    providers_file: Option<&Path>,
    format: Format,
) -> anyhow::Result<ExitCode> {
    let options = ReferenceOptions {
        include_declaration: args.include_declaration,
        refresh: args.refresh,
        request_timeout: Duration::from_secs(args.timeout_secs),
    };
    let symbol = args.symbol.as_str();
    let (mut store, snapshot) = open_index(db)?;
    let target = match select_symbol(&store, &snapshot, symbol, args.filters)? {
        SymbolResolution::Resolved(target) => target,
        SymbolResolution::Ambiguous(candidates) => {
            output::ambiguous(symbol, &candidates, format)?;
            return Ok(ExitCode::from(EXIT_AMBIGUOUS));
        }
        SymbolResolution::Unresolved => bail!("no indexed symbol matches `{symbol}`"),
    };

    let registry = load_registry(Path::new(&snapshot.snapshot.root), providers_file)?;
    let context = LookupContext {
        snapshot: &snapshot,
        registry: &registry,
        options: &options,
    };
    let mut pool = SessionPool::for_one_lookup();
    let report = lemma_query::find_references(&mut store, &context, target, &mut pool);
    pool.shutdown();
    let report = report?;
    output::reference_report(&report, &snapshot.snapshot.root, format)?;

    let answered = matches!(
        report.outcome,
        ReferenceOutcome::Found { .. } | ReferenceOutcome::Empty
    );
    Ok(if answered {
        ExitCode::SUCCESS
    } else {
        ExitCode::from(EXIT_NO_ANSWER)
    })
}

fn providers(
    db: Option<PathBuf>,
    providers_file: Option<&Path>,
    format: Format,
) -> anyhow::Result<ExitCode> {
    // Without an index, fall back to the current directory's project file.
    let root = match find_index(db) {
        Ok(db_path) => open_store(&db_path)?
            .latest_snapshot()?
            .map(|stored| PathBuf::from(stored.snapshot.root)),
        Err(_) => None,
    };
    let root = match root {
        Some(root) => root,
        None => std::env::current_dir().context("could not read the current directory")?,
    };
    let registry = load_registry(&root, providers_file)?;
    output::providers(&registry, format)?;
    Ok(ExitCode::SUCCESS)
}

/// A `#<id>` selects one symbol directly; anything else is resolved by name.
fn select_symbol(
    store: &Store,
    snapshot: &StoredSnapshot,
    symbol: &str,
    filters: Filters,
) -> anyhow::Result<SymbolResolution> {
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

/// `.lemma/index.db` under the indexed directory, creating the directory.
fn default_index_path(root: &Path) -> anyhow::Result<PathBuf> {
    let directory = root.join(LEMMA_DIRECTORY);
    fs::create_dir_all(&directory)
        .with_context(|| format!("could not create {}", directory.display()))?;
    Ok(directory.join(INDEX_FILE))
}

fn open_index(db: Option<PathBuf>) -> anyhow::Result<(Store, StoredSnapshot)> {
    let db_path = find_index(db)?;
    let store = open_store(&db_path)?;
    let Some(snapshot) = store.latest_snapshot()? else {
        bail!(
            "{} has no snapshot; run `lemma index` first",
            db_path.display()
        );
    };
    Ok((store, snapshot))
}

fn open_store(db_path: &Path) -> anyhow::Result<Store> {
    Store::open(db_path).with_context(|| format!("could not open index {}", db_path.display()))
}

/// Built-in providers plus the given file, or the project's own file.
fn load_registry(root: &Path, providers_file: Option<&Path>) -> anyhow::Result<ProviderRegistry> {
    if let Some(file) = providers_file {
        if !file.is_file() {
            bail!("provider configuration {} does not exist", file.display());
        }
        return Ok(ProviderRegistry::load(file)?);
    }
    let project_file = root.join(LEMMA_DIRECTORY).join(PROVIDERS_FILE);
    Ok(ProviderRegistry::load(&project_file)?)
}

/// The given database, or the nearest `.lemma/index.db` from the current
/// directory upwards.
fn find_index(db: Option<PathBuf>) -> anyhow::Result<PathBuf> {
    if let Some(db_path) = db {
        return Ok(db_path);
    }
    let current = std::env::current_dir().context("could not read the current directory")?;
    current
        .ancestors()
        .map(|directory| directory.join(LEMMA_DIRECTORY).join(INDEX_FILE))
        .find(|candidate| candidate.is_file())
        .context("no index found here or in any parent directory; run `lemma index` first")
}
