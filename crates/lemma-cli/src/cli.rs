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
    /// Index database to use. Defaults to `.lemma/index.ls` in the nearest
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

    /// Do a `check`'s or a `verify`'s work in this process and do not ask
    /// the daemon to, while still using the daemon's warm servers and
    /// runtimes: how the daemon runs one for whoever asked it.
    #[arg(long, global = true, hide = true)]
    pub in_own_process: bool,

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
        /// A directory with a mod.lm, or the module to read.
        #[arg(default_value = ".")]
        path: PathBuf,
        /// Snapshots of pinned commits (worktrees a workspace indexes) to
        /// keep in each repository's index, most recently indexed first.
        #[arg(long, default_value_t = 5)]
        keep: usize,
        /// Never clone a repository: stop at those that are here.
        #[arg(long)]
        no_clone: bool,
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
    /// Show an editor what saved changes break, in every repository of the
    /// workspace, as Language Server Protocol diagnostics over stdin and
    /// stdout. Experimental.
    ///
    /// Each repository is watched; when its saved files change, it is
    /// checked as `lemma check --change` would, and each finding the change
    /// introduced is shown on the line of code it names, in whichever
    /// repository that is. `lm lsp` serves `.lm` modules instead.
    Lsp(LanguageServerArgs),
    /// Check that a description still matches the code: its configuration
    /// and its links, at one snapshot. Exits 4 if a finding is an error.
    Verify(VerifyArgs),
    /// Check the links between operations declared in composable
    /// descriptions: that each link persists, and that both sides'
    /// request and response schemas agree.
    ///
    /// Exits with 4 if any finding is an error (level `check`).
    Check(CheckArgs),
    /// Run, inspect or stop the background daemon.
    ///
    /// Every other command starts the daemon automatically; run this to keep
    /// it in the foreground, check on it, or stop it.
    Daemon(DaemonArgs),
}

#[derive(Args)]
#[expect(
    clippy::struct_excessive_bools,
    reason = "independent command-line flags"
)]
pub(crate) struct CheckArgs {
    /// The entry module, which composes the repositories to check.
    #[arg(default_value = "mod.lm")]
    pub manifest: PathBuf,
    /// Never clone a repository: check those that are here, and exit 5 if
    /// that is not all of them.
    #[arg(long)]
    pub no_clone: bool,
    /// The repository that changed, as the workspace names it or by its
    /// directory (`billing`), or `self` for the workspace's own
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
    /// The check of a parent `mod.lm` to run instead of its `check main`, or
    /// one behavior or requirement by name; from a description with no
    /// program, one built-in requirement, whose findings alone are listed.
    #[arg(long, value_name = "CHECK")]
    pub select: Option<String>,
    /// With the daemon's sentry attached: list only the findings that came
    /// and went since the answer this token (the last line of an earlier
    /// answer) names, for the same options.
    #[arg(long, value_name = "TOKEN")]
    pub since: Option<String>,
    /// Also list informational findings, not just their count.
    #[arg(long)]
    pub all: bool,
    /// The rule engine for inferred links: `souffle` or `reference`.
    /// Default: Soufflé if it is installed, otherwise the reference engine.
    #[arg(long)]
    pub engine: Option<String>,
    /// How much consumer code inference reads: `demand` (the code a finding
    /// can rest on, the default) or `full`. `both` runs each and fails if
    /// their findings differ.
    #[arg(long, value_parser = ["demand", "full", "both"])]
    pub evaluation: Option<String>,
    /// Show why each inferred finding holds.
    #[arg(long)]
    pub explain: bool,
}

#[derive(Args, Clone)]
pub(crate) struct LanguageServerArgs {
    /// The workspace's entry module. Default: the editor's `manifest`
    /// initialization option, or else the nearest `mod.lm` above the
    /// editor's folder that declares a workspace.
    pub manifest: Option<PathBuf>,
    /// `audit` reports differences as warnings; `check` fails on them and
    /// on violated behaviors; `strict` also fails on behaviors whose
    /// outcome is unknown. Overrides the graph's level.
    #[arg(long, value_parser = ["audit", "check", "strict"])]
    pub level: Option<String>,
    /// Run no checkers: behaviors use the evidence already kept, and are
    /// unknown, or stale when their inputs changed, without it.
    #[arg(long)]
    pub no_run: bool,
    /// The rule engine for inferred links: `souffle` or `reference`.
    /// Default: Soufflé if it is installed, otherwise the reference engine.
    #[arg(long)]
    pub engine: Option<String>,
}

#[derive(Args)]
pub(crate) struct VerifyArgs {
    /// The entry module: a workspace verifies every repository it composes.
    #[arg(default_value = "mod.lm")]
    pub module: PathBuf,
    /// List every informational finding, not just the first ten.
    #[arg(long)]
    pub all: bool,
    /// Also list every link, declared and inferred.
    #[arg(long)]
    pub links: bool,
    /// Also print every row of a derived relation, such as `calls`.
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
    /// When running: the root of the workspace to serve. Default: the
    /// workspace the current directory belongs to.
    #[arg(long, value_name = "DIRECTORY")]
    pub root: Option<PathBuf>,
}

#[derive(Subcommand)]
pub(crate) enum DaemonAction {
    /// Start the daemon of the workspace the current directory belongs to,
    /// in the background, unless one already serves it.
    Start,
    /// Show the workspace this directory's daemon serves and its language
    /// servers.
    Status {
        /// Every daemon of this user, not only this directory's.
        #[arg(long)]
        all: bool,
    },
    /// Stop the daemon serving this directory.
    Stop {
        /// Every daemon of this user, not only this directory's.
        #[arg(long)]
        all: bool,
    },
    /// List, start or stop the daemon's language servers for a repository
    /// or workspace. Without an action, lists them.
    Lsp(LspArgs),
    /// Experimental: attach the daemon's sentry to a workspace, so its checks reuse what
    /// earlier checks read and only what changed is read again; detach it,
    /// or report on it.
    Sentry(SentryArgs),
}

/// `lemma daemon sentry`: the daemon's watch over one workspace.
#[derive(Args)]
pub(crate) struct SentryArgs {
    #[command(subcommand)]
    pub action: SentryCommand,
}

#[derive(Subcommand)]
pub(crate) enum SentryCommand {
    /// Attach to the workspace of a manifest, starting its daemon if none
    /// runs.
    Start {
        /// The workspace's mod.lm.
        #[arg(default_value = "mod.lm")]
        manifest: PathBuf,
    },
    /// Detach.
    Stop,
    /// What it watches and how many checks it answered.
    Status,
    /// What happened since a token: batches of changes and checks answered,
    /// a line each, then the latest token.
    Events {
        /// A token from an earlier answer or event list; without one, every
        /// event kept.
        #[arg(long, value_name = "TOKEN")]
        since: Option<String>,
        /// Wait this long for an event when there is none yet, such as `30s`.
        #[arg(long, value_name = "DURATION")]
        wait: Option<String>,
    },
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
    // Its own id: the one clap would derive from the field's name belongs
    // to the global `--providers`, and two arguments with one id panic
    // when either is read.
    #[arg(id = "provider", long = "provider", value_name = "ID")]
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

#[derive(Args, Clone, PartialEq)]
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
    /// How many callers in files outside the snapshot to ask about in
    /// turn. Each is named either way; asking one finds its own callers,
    /// and is a lookup in a file the index does not hold, slow the first
    /// time.
    #[arg(long, default_value_t = 0, value_name = "CALLERS")]
    pub outside: usize,
    /// Only symbols of these kinds besides the symbol itself, such as
    /// `function,method` for the calls without the fields and types used.
    #[arg(long, value_delimiter = ',', value_name = "KINDS")]
    pub only: Vec<SymbolKind>,
    /// A workspace description: follow callers out of the repository too,
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
#[derive(Args, Clone, PartialEq)]
pub(crate) struct SymbolTarget {
    /// A name, a qualified name such as `Router.delete`, `#<id>` from
    /// `lemma symbols`, or an export's address from a description, such as
    /// `services.api.charge`.
    pub symbol: String,
    #[command(flatten)]
    pub filters: Filters,
}

/// How to ask a provider.
#[derive(Args, Clone, PartialEq)]
pub(crate) struct LookupArgs {
    /// Ask the provider again instead of using a stored answer.
    #[arg(long)]
    pub refresh: bool,
    /// Seconds to wait for the provider to start and to answer.
    #[arg(long, default_value_t = 60)]
    pub timeout_secs: u64,
}

/// Narrows which symbols a name refers to.
#[derive(Args, Clone, PartialEq)]
pub(crate) struct Filters {
    /// Only symbols of this kind, such as `function` or `method`.
    #[arg(long)]
    pub kind: Option<SymbolKind>,
    /// Only symbols in this language: typescript, tsx, python, go, or rust.
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

#[cfg(test)]
mod tests {
    use clap::{CommandFactory, Parser};

    use super::Cli;

    #[test]
    fn a_provider_can_be_named_beside_the_providers_file() {
        let parsed = Cli::try_parse_from([
            "lemma",
            "--providers",
            "servers.toml",
            "daemon",
            "lsp",
            "start",
            "--provider",
            "ty",
        ]);

        let cli = parsed.unwrap();
        assert_eq!(cli.providers.unwrap().to_str(), Some("servers.toml"));
        let super::Command::Daemon(daemon) = cli.command else {
            panic!("not the daemon command");
        };
        let Some(super::DaemonAction::Lsp(lsp)) = daemon.action else {
            panic!("not the lsp command");
        };
        let Some(super::LspAction::Start(start)) = lsp.action else {
            panic!("not the start command");
        };
        assert_eq!(start.target.providers, ["ty"]);
    }

    /// The command reference, which every command and option must be in.
    const REFERENCE: &str = include_str!("../../../docs/commands.md");
    /// The MCP tools `lemma mcp` serves, which the reference lists too.
    const MCP_TOOLS: &str = include_str!("mcp_tools.json");

    /// The reference's section under the heading that starts with
    /// `heading`, up to the next heading of that level; empty when there is
    /// no such heading.
    fn section(heading: &str) -> &'static str {
        let Some(start) = REFERENCE.find(heading) else {
            return "";
        };
        let body = &REFERENCE[start + heading.len()..];
        body.find("\n## ").map_or(body, |end| &body[..end])
    }

    /// Each long option of `command` (named `path`) that `section` does not
    /// mention, and each subcommand below it that `section` does not name,
    /// with its options. Options every command takes are left to the
    /// section on global options.
    fn unmentioned(command: &clap::Command, path: &str, section: &str, missing: &mut Vec<String>) {
        for argument in command.get_arguments() {
            if argument.is_hide_set() || argument.is_global_set() {
                continue;
            }
            let Some(long) = argument.get_long() else {
                continue;
            };
            let option = format!("--{long}");
            if !["--help", "--version"].contains(&option.as_str()) && !section.contains(&option) {
                missing.push(format!("{path} {option}"));
            }
        }
        for subcommand in command.get_subcommands() {
            if subcommand.is_hide_set() || subcommand.get_name() == "help" {
                continue;
            }
            let named = format!("{path} {}", subcommand.get_name());
            if !section.contains(&named) {
                missing.push(named.clone());
            }
            unmentioned(subcommand, &named, section, missing);
        }
    }

    /// The global options `section` does not mention.
    fn unmentioned_global_options(command: &clap::Command, section: &str) -> Vec<String> {
        command
            .get_arguments()
            .filter(|argument| argument.is_global_set() && !argument.is_hide_set())
            .filter_map(clap::Arg::get_long)
            .map(|long| format!("--{long}"))
            .filter(|option| !section.contains(option))
            .collect()
    }

    #[test]
    fn every_command_and_option_is_in_its_section_of_the_command_reference() {
        let cli = Cli::command();
        let mut missing = unmentioned_global_options(&cli, section("## Global options"));

        for command in cli.get_subcommands() {
            if command.is_hide_set() || command.get_name() == "help" {
                continue;
            }
            let named = format!("lemma {}", command.get_name());
            let of_command = section(&format!("## `{named}"));
            if of_command.is_empty() {
                missing.push(format!("{named}: no section"));
            }
            unmentioned(command, &named, of_command, &mut missing);
        }

        assert_eq!(
            missing,
            Vec::<String>::new(),
            "missing from docs/commands.md"
        );
    }

    /// The arguments of the MCP tool `tool` that its reference `row` does
    /// not name.
    fn unlisted_arguments<'a>(
        tool: &'a serde_json::Value,
        row: &'a str,
    ) -> impl Iterator<Item = &'a String> + 'a {
        tool["inputSchema"]["properties"]
            .as_object()
            .into_iter()
            .flat_map(serde_json::Map::keys)
            .filter(move |argument| !row.contains(&format!("`{argument}`")))
    }

    #[test]
    fn every_mcp_tool_and_argument_is_in_the_command_reference() {
        let tools: serde_json::Value = serde_json::from_str(MCP_TOOLS).unwrap();
        let mut missing = Vec::new();

        for tool in tools.as_array().unwrap() {
            let name = tool["name"].as_str().unwrap();
            let row_start = format!("| `{name}` |");
            let Some(row) = REFERENCE.lines().find(|line| line.starts_with(&row_start)) else {
                missing.push(name.to_owned());
                continue;
            };
            let unlisted = unlisted_arguments(tool, row);
            missing.extend(unlisted.map(|argument| format!("{name}.{argument}")));
        }

        assert_eq!(
            missing,
            Vec::<String>::new(),
            "missing from docs/commands.md"
        );
    }
}
