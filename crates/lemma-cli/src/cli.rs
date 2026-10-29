//! Command-line arguments.

use std::path::PathBuf;

use clap::{Args, Parser, Subcommand};
use lemma_model::{Language, SymbolKind};

#[derive(Parser)]
#[command(
    name = "lemma",
    version,
    about = "Index code and query symbol references"
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
    /// Find references to a symbol with its language's provider.
    ///
    /// Exits with 2 if the name matches several symbols and 3 if no provider
    /// could answer.
    Refs(RefsArgs),
    /// List configured providers and whether each is installed.
    Providers,
    /// Run or stop the background daemon.
    ///
    /// Every other command starts the daemon automatically; run this only to
    /// keep it in the foreground or to stop it.
    Daemon(DaemonArgs),
}

#[derive(Args)]
pub(crate) struct DaemonArgs {
    /// Stop the running daemon instead of starting one.
    #[arg(long)]
    pub stop: bool,
    /// Show the running daemon's language servers instead of starting one.
    #[arg(long, conflicts_with = "stop")]
    pub status: bool,
    /// Exit after this many seconds without a command, instead of the
    /// configured `idle_exit`.
    #[arg(long)]
    pub idle_secs: Option<u64>,
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
    /// A name, a qualified name such as `Router.delete`, or `#<id>` from
    /// `lemma symbols`.
    pub symbol: String,
    #[command(flatten)]
    pub filters: Filters,
    /// Count the definition itself as a reference.
    #[arg(long)]
    pub include_declaration: bool,
    /// Ask the provider again instead of using a cached answer.
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
