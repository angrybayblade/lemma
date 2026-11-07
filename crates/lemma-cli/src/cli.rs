//! Command-line arguments.

use std::path::PathBuf;

use clap::{Args, Parser, Subcommand, ValueEnum};
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

    /// Log what Lemma is doing to stderr, at this level: `error`, `warn`,
    /// `info` (each step: in this process or the daemon, what is read,
    /// cached or run), `debug` or `trace`. Off when not given.
    #[arg(short = 'l', long = "logging", global = true, value_name = "LEVEL")]
    pub logging: Option<crate::logging::LogLevel>,

    #[command(subcommand)]
    pub command: Command,
}

#[derive(Subcommand)]
pub(crate) enum Command {
    /// Index source files and symbols under a directory.
    Index {
        /// A directory with a lemma.yaml, or a lemma.yaml.
        #[arg(default_value = ".")]
        path: PathBuf,
        /// Snapshots of pinned commits (worktrees a workspace indexes) to
        /// keep in each repository's index, most recently indexed first.
        #[arg(long, default_value_t = 5)]
        keep: usize,
        /// How long a pinned commit's snapshot is kept after it was last
        /// indexed: `14d`, `12h`, or `never` to keep it until `--keep`
        /// pushes it out.
        #[arg(long, default_value = "14d")]
        ttl: String,
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
    /// Show what uses a symbol and what it uses, followed to a depth, as a
    /// tree, Graphviz DOT or Mermaid.
    ///
    /// Exits with 2 if the name matches several symbols and 3 if no provider
    /// could answer anything.
    Graph(GraphArgs),
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
#[expect(
    clippy::struct_excessive_bools,
    reason = "independent command-line flags"
)]
pub(crate) struct CheckArgs {
    /// The parent lemma.yaml, which composes the repositories to check.
    #[arg(default_value = "lemma.yaml")]
    pub manifest: PathBuf,
    /// The repository that changed, as the workspace names it (`billing`;
    /// `#billing` in a lemma.yaml), or `self` for the workspace's own
    /// repository.
    #[arg(long)]
    pub change: Option<String>,
    /// The commit before the change. Default: where the repository's
    /// checked-out commit left its default branch (a remote's `HEAD`, or
    /// `main`, `master`, `develop` or `trunk`), so a branch's commits and
    /// uncommitted work are checked together; a merge commit's first parent
    /// when no such branch shares its history. A clean checkout with
    /// neither is an error, rather than an empty change.
    #[arg(long, requires = "change")]
    pub base: Option<String>,
    /// The commit with the change; the working directory, uncommitted
    /// changes included, if absent.
    #[arg(long, requires = "change")]
    pub head: Option<String>,
    /// `audit` reports differences as warnings; `check` fails on them and
    /// on violated behaviors; `strict` also fails on behaviors whose
    /// outcome is unknown. Overrides the graph's level.
    #[arg(long, value_parser = ["audit", "check", "strict"])]
    pub level: Option<String>,
    /// Run no checkers: behaviors use the evidence already kept, and are
    /// unknown, or stale when their inputs changed, without it.
    #[arg(long)]
    pub no_run: bool,
    /// With `--change`, also run checkers at the base, so a case the change
    /// fixed is reported as fixed. Without, the base uses kept evidence only.
    #[arg(long, requires = "change", conflicts_with = "no_run")]
    pub run_base: bool,
    /// The check of a parent `mod.lm` to run instead of its `check main`.
    #[arg(long, value_name = "CHECK")]
    pub select: Option<String>,
    /// Also list informational findings, not just their count.
    #[arg(long)]
    pub all: bool,
    /// The rule engine for inferred links: `souffle` or `reference`.
    /// Default: Soufflé if it is installed, otherwise the reference engine.
    #[arg(long)]
    pub engine: Option<String>,
    /// How much consumer code inference reads: `demand` (the code a finding
    /// can rest on, the default) or `full`.
    #[arg(long, value_parser = ["demand", "full"])]
    pub evaluation: Option<String>,
    /// Also list every link: those declared, and each call from code to a
    /// documented operation, by calling repository and the service called.
    /// Reads all consumer code; without `--change`, at head.
    #[arg(long)]
    pub links: bool,
    /// Also ask language servers who calls each link's caller anchor, and
    /// warn when a method nothing calls is anchored while a sibling method
    /// is called. Uses the running daemon's warm servers, or, without one,
    /// starts a server per repository, which takes longer.
    #[arg(long)]
    pub anchors: bool,
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
    /// How much consumer code to read: `demand` (only what a finding can
    /// rest on), `full`, or `both`, which runs the two and fails if their
    /// findings differ. Default: `demand`, or `full` with `--relation`, whose
    /// rows are then complete.
    #[arg(long)]
    pub evaluation: Option<String>,
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
    /// List, start or stop the daemon's language servers for a repository
    /// or workspace. Without an action, lists them.
    Lsp(LspArgs),
}

/// `lemma daemon lsp`: the daemon's language servers, by hand.
#[derive(Args)]
#[command(args_conflicts_with_subcommands = true)]
pub(crate) struct LspArgs {
    #[command(subcommand)]
    pub action: Option<LspAction>,
    #[command(flatten)]
    pub target: LspTarget,
}

#[derive(Subcommand)]
pub(crate) enum LspAction {
    /// List the servers each language of the indexed code would use, and
    /// what each is doing: stopped, starting, running, paused or failed.
    List(LspTarget),
    /// Start servers in the daemon, in the background unless `--wait`.
    Start(LspStartArgs),
    /// Stop servers.
    Stop(LspTarget),
}

/// Which servers: those of the code indexed at a path, narrowed by
/// language or provider.
#[derive(Args, Clone, Default)]
pub(crate) struct LspTarget {
    /// An indexed repository, or a workspace whose members are indexed.
    /// Default: the current directory.
    pub path: Option<PathBuf>,
    /// Only the servers for these languages: typescript, tsx, python, go,
    /// rust.
    #[arg(long = "language", value_name = "LANGUAGE")]
    pub languages: Vec<String>,
    /// Only these providers, by id (see `lemma providers`); for starting,
    /// the provider to use instead of the first installed one.
    #[arg(long = "provider", value_name = "ID")]
    pub providers: Vec<String>,
}

#[derive(Args)]
pub(crate) struct LspStartArgs {
    #[command(flatten)]
    pub target: LspTarget,
    /// Wait until each server has loaded its project, and report any that
    /// failed to start.
    #[arg(long)]
    pub wait: bool,
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
pub(crate) struct GraphArgs {
    #[command(flatten)]
    pub target: SymbolTarget,
    /// Which way to follow uses from the symbol.
    #[arg(long, value_enum, default_value_t = GraphDirection::Both)]
    pub direction: GraphDirection,
    /// How many uses away from the symbol to go.
    #[arg(long, default_value_t = 2)]
    pub depth: usize,
    /// Where relationships come from: `lsp` asks the language server
    /// (precise, cached); `names` matches names in the indexed code
    /// (instant, approximate, no server needed).
    #[arg(long, value_enum, default_value_t = GraphSourceArg::Lsp)]
    pub source: GraphSourceArg,
    /// How to draw it. `--json` prints the graph as data instead.
    #[arg(long, value_enum, default_value_t = GraphDrawing::Tree)]
    pub format: GraphDrawing,
    /// The most nodes to include.
    #[arg(long, default_value_t = 200)]
    pub max_nodes: usize,
    /// Only symbols of these kinds besides the symbol itself, such as
    /// `function,method` for the calls without the fields and types used.
    #[arg(long, value_delimiter = ',', value_name = "KINDS")]
    pub only: Vec<SymbolKind>,
    /// A workspace lemma.yaml: follow callers out of the repository too,
    /// through the operations their handlers serve to the code in other
    /// repositories that calls them, and on up. Runs inference over the
    /// whole system, so it takes as long as `lemma check`.
    #[arg(long)]
    pub workspace: Option<PathBuf>,
    /// Across a workspace, how many calls from other code to follow: 1
    /// stops at the code calling the operations the handlers serve; each
    /// more goes on through that code to its own operations and their
    /// callers.
    #[arg(long, default_value_t = 1, value_name = "CALLS")]
    pub across: usize,
    #[command(flatten)]
    pub lookup: LookupArgs,
}

/// Which way `lemma graph` follows uses.
#[derive(Debug, Clone, Copy, PartialEq, Eq, ValueEnum)]
pub(crate) enum GraphDirection {
    /// What uses the symbol.
    Callers,
    /// What the symbol uses.
    Callees,
    /// Both.
    Both,
}

/// Where `lemma graph` finds relationships.
#[derive(Debug, Clone, Copy, PartialEq, Eq, ValueEnum)]
pub(crate) enum GraphSourceArg {
    /// The language server.
    Lsp,
    /// Names in the indexed code.
    Names,
}

/// How `lemma graph` draws.
#[derive(Debug, Clone, Copy, PartialEq, Eq, ValueEnum)]
pub(crate) enum GraphDrawing {
    /// Indented trees.
    Tree,
    /// Graphviz DOT.
    Dot,
    /// A Mermaid flowchart.
    Mermaid,
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
    /// A name, a qualified name such as `Router.delete`, `#<id>` from
    /// `lemma symbols`, or an export's address from lemma.yaml, such as
    /// `services.api.charge`.
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
#[derive(Args, Clone)]
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
    /// When a name is ambiguous, keep only symbols that can have
    /// implementations: set by `implementations`, not a flag.
    #[arg(skip)]
    pub prefers_implementable: bool,
}
