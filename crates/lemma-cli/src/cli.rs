//! Command-line arguments.

use std::path::PathBuf;

use clap::{Args, Parser, Subcommand};
use lemma_model::{Language, SymbolKind};

#[derive(Parser)]
#[command(
    name = "lemma",
    version,
    about = "Index code and ask about its symbols"
)]
pub(crate) struct Cli {
    /// Index database to use. Defaults to `.lemma/index.db` in the nearest
    /// directory that has one.
    #[arg(long, global = true)]
    pub db: Option<PathBuf>,

    /// Provider configuration to use instead of the indexed project's
    /// `.lemma/providers.toml`. Built-in providers are still available.
    #[arg(long, global = true)]
    pub providers: Option<PathBuf>,

    /// Print JSON instead of text. Positions are zero-based and columns
    /// count UTF-8 bytes.
    #[arg(long, global = true)]
    pub json: bool,

    /// Run in this process instead of the background daemon that keeps the
    /// index open and language servers warm. Also set by `LEMMA_NO_DAEMON=1`.
    #[arg(long, global = true)]
    pub no_daemon: bool,

    #[command(subcommand)]
    pub command: Command,
}

#[derive(Subcommand)]
pub(crate) enum Command {
    /// Index source files and symbols under a directory.
    Index {
        /// Directory to index.
        #[arg(default_value = ".")]
        path: PathBuf,
    },
    /// Search indexed symbols by name.
    Symbols(SymbolsArgs),
    /// List the symbols defined in a file, in source order.
    Outline {
        /// The file, relative to the current directory.
        path: PathBuf,
    },
    /// Find references to a symbol with its language's provider.
    ///
    /// Exits with 2 if the name matches several symbols and 3 if no provider
    /// could answer.
    Refs(RefsArgs),
    /// Find where the symbol used at a position is defined.
    ///
    /// Exits with 3 if no provider could answer.
    Definition(DefinitionArgs),
    /// Find what implements an interface or abstract method, or overrides a
    /// method.
    ///
    /// Exits with 2 if the name matches several symbols and 3 if no provider
    /// could answer.
    Implementations(SymbolLookupArgs),
    /// Show a symbol's declaration and what its provider says about it.
    ///
    /// Exits with 2 if the name matches several symbols and 3 if neither the
    /// source nor a provider could describe it.
    Signature(SymbolLookupArgs),
    /// List configured providers and whether each is installed.
    Providers,
    /// Serve these commands to agents as MCP tools, over stdin and stdout.
    ///
    /// Run it from the project directory; tool calls go through the
    /// daemon like any other command.
    Mcp,
    /// Verify a change across repositories: extract facts from each
    /// repository in a manifest, evaluate a grammar's requirements, and
    /// report what the change breaks.
    ///
    /// Exits with 4 if any finding is an error.
    Verify(VerifyArgs),
    /// Check the links between operations declared in composable
    /// lemma.yaml files: that each link persists, and that both sides'
    /// request and response schemas agree.
    ///
    /// Exits with 4 if any finding is an error (level `check`).
    Check(CheckArgs),
    /// Run, inspect, stop, or install the background daemon.
    ///
    /// Every other command starts the daemon automatically; run this to keep
    /// it in the foreground, check on it, stop it, or start it at login.
    Daemon(DaemonArgs),
}

#[derive(Args)]
pub(crate) struct CheckArgs {
    /// The parent lemma.yaml, which composes the repositories to check.
    #[arg(default_value = "lemma.yaml")]
    pub manifest: PathBuf,
    /// The repository that changed: an external service's name (`#billing`),
    /// or `self` for the parent's own repository. Needs `--base` and `--head`.
    #[arg(long, requires_all = ["base", "head"])]
    pub change: Option<String>,
    /// The commit before the change.
    #[arg(long, requires = "change")]
    pub base: Option<String>,
    /// The commit with the change.
    #[arg(long, requires = "change")]
    pub head: Option<String>,
    /// `audit` reports differences as warnings; `check` fails on them.
    /// Overrides the graph's level.
    #[arg(long, value_parser = ["audit", "check"])]
    pub level: Option<String>,
    /// Also list informational findings, not just their count.
    #[arg(long)]
    pub all: bool,
}

#[derive(Args)]
pub(crate) struct VerifyArgs {
    /// The manifest: repositories, their roles and commits, and the change.
    pub manifest: PathBuf,
    /// Show why each finding holds.
    #[arg(long)]
    pub explain: bool,
    /// The rule engine: `souffle` or `reference`. Default: Soufflé if it is
    /// installed, otherwise the reference engine.
    #[arg(long)]
    pub engine: Option<String>,
    /// Also list informational findings, not just their count.
    #[arg(long)]
    pub all: bool,
    /// The commit before the change, instead of the manifest's `change.base`.
    #[arg(long)]
    pub base: Option<String>,
    /// The commit with the change, instead of the manifest's `change.head`.
    #[arg(long)]
    pub head: Option<String>,
    /// Also print every row of a derived relation, such as `calls`.
    /// Repeatable.
    #[arg(long = "relation", value_name = "NAME")]
    pub relations: Vec<String>,
}

#[derive(Args)]
pub(crate) struct DaemonArgs {
    /// What to do; without one, run the daemon in the foreground.
    #[command(subcommand)]
    pub action: Option<DaemonAction>,
    /// When running: exit after this many seconds without a command,
    /// instead of the configured `idle_exit`.
    #[arg(long)]
    pub idle_secs: Option<u64>,
}

#[derive(Subcommand)]
pub(crate) enum DaemonAction {
    /// Show the daemon's language servers and whether it runs as a service.
    Status,
    /// Stop the running daemon.
    Stop,
    /// Run the daemon as a login service (launchd on macOS, systemd on
    /// Linux), so it is already running when the first command arrives.
    Install,
    /// Remove the login service and stop its daemon.
    Uninstall,
}

#[derive(Args)]
pub(crate) struct SymbolsArgs {
    /// Text to look for in names and qualified names.
    pub query: String,
    /// Match the whole name instead of any part of it.
    #[arg(long)]
    pub exact: bool,
    #[command(flatten)]
    pub filters: Filters,
    /// Maximum number of results.
    #[arg(long, default_value_t = 50)]
    pub limit: u32,
}

#[derive(Args)]
pub(crate) struct RefsArgs {
    #[command(flatten)]
    pub target: SymbolTarget,
    /// Count the definition itself as a reference.
    #[arg(long)]
    pub include_declaration: bool,
    #[command(flatten)]
    pub lookup: LookupArgs,
}

#[derive(Args)]
pub(crate) struct SymbolLookupArgs {
    #[command(flatten)]
    pub target: SymbolTarget,
    #[command(flatten)]
    pub lookup: LookupArgs,
}

#[derive(Args)]
pub(crate) struct DefinitionArgs {
    /// Where the symbol is used, as `file:line:column`, counting lines and
    /// columns from 1 as editors do.
    pub location: String,
    #[command(flatten)]
    pub lookup: LookupArgs,
}

/// Which symbol a command is about.
#[derive(Args)]
pub(crate) struct SymbolTarget {
    /// A name, a qualified name such as `Router.delete`, or `#<id>` from
    /// `lemma symbols`.
    pub symbol: String,
    #[command(flatten)]
    pub filters: Filters,
}

/// How to ask a provider.
#[derive(Args)]
pub(crate) struct LookupArgs {
    /// Ask the provider again instead of using a stored answer.
    #[arg(long)]
    pub refresh: bool,
    /// Seconds to wait for the provider to start and to answer.
    #[arg(long, default_value_t = 60)]
    pub timeout_secs: u64,
}

/// Narrows which symbols a name refers to.
#[derive(Args)]
pub(crate) struct Filters {
    /// Only symbols of this kind, such as `function` or `method`.
    #[arg(long)]
    pub kind: Option<SymbolKind>,
    /// Only symbols in this language: typescript, tsx, python, or go.
    #[arg(long)]
    pub language: Option<Language>,
    /// Only symbols whose file path contains this text.
    #[arg(long)]
    pub path: Option<String>,
}
