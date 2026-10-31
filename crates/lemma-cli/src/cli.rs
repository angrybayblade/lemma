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
    /// Run, inspect, stop, or install the background daemon.
    ///
    /// Every other command starts the daemon automatically; run this to keep
    /// it in the foreground, check on it, stop it, or start it at login.
    Daemon(DaemonArgs),
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
