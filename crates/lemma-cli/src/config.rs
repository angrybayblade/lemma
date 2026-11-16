//! The user's settings for the daemon: when it exits, how it treats
//! language servers nobody is using, and what warm runtimes may use.
//!
//! Read from `LEMMA_CONFIG`, else `$XDG_CONFIG_HOME/lemma/config.toml`, else
//! `~/.config/lemma/config.toml`. A missing file means the defaults. The
//! daemon reads it again whenever it changes.

use std::fs;
use std::path::{Path, PathBuf};
use std::time::{Duration, SystemTime};

use anyhow::{Context, bail};
use lemma_query::{IdlePolicy, WhenSlow};
use serde::Deserialize;

/// Who may start a daemon for a workspace that has none.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "lowercase")]
pub(crate) enum Autostart {
    /// Any command.
    #[default]
    Always,
    /// The MCP server and the editor's server. A command run from a shell
    /// does its work in its own process.
    Clients,
    /// Nothing: a daemon runs once `lemma daemon start` has started it.
    Never,
}

/// Everything the config file can set.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct Config {
    /// Who may start a daemon for a workspace that has none.
    pub autostart: Autostart,
    /// Exit the daemon after this long without a command. `None` keeps it
    /// running.
    pub idle_exit: Option<Duration>,
    /// When to pause and stop idle language servers.
    pub idle_policy: IdlePolicy,
    /// While sentry is attached: start a project's servers again when its
    /// files change, if they were stopped, so they are ready by the next
    /// lookup.
    pub warms_up_on_activity: bool,
    /// What a lookup does about a language server that does not answer in
    /// time.
    pub when_slow: WhenSlow,
    /// Whether the MCP server asks for sentry when it starts, as the
    /// editor's server does. On unless the user turns it off.
    pub sentry_for_mcp: bool,
    /// While sentry is attached: how many watches the workspaces graphs are
    /// traced through may take between them, where each directory takes
    /// one. `None` sets no limit; `Some(0)` watches nothing.
    pub graph_watches: Option<usize>,
    /// While sentry is attached: how far the daemon gets ready for the next
    /// graph through a workspace once its files change.
    pub graph_prepare: Prepare,
    /// How many of the graphs last asked through a workspace are answered
    /// ahead once its files change, where graphs are answered ahead.
    pub graphs_kept_ready: usize,
}

/// How many of the graphs last asked are kept ready unless the user says.
/// An edit retires every answer the edited repository's language servers
/// gave, so each graph kept is one more that costs nothing when asked
/// next, and one more to answer after every change.
const GRAPHS_KEPT_READY: usize = 4;

/// How far the daemon gets ready for the next graph through a workspace
/// once its files have changed and settled: a point between graphs that
/// read what changed when asked and graphs that find it read.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Deserialize)]
#[serde(rename_all = "lowercase")]
pub(crate) enum Prepare {
    /// Not at all.
    Off,
    /// Brings the index up to date as the last graph asked did: the
    /// changed files, the exports and the adapters' documents.
    Index,
    /// Also answers the last graph asked again, keeping what that reads:
    /// the traced system, the callers fetched and the language servers'
    /// answers. The default: it is the point where the next graph costs
    /// what a repeated one does.
    #[default]
    Graph,
}

/// The share of the system's limit on watches that graphs may take unless
/// the user says: the rest is left to the indexes, whose lookups need
/// theirs more, and to the user's other programs, an editor among them.
const GRAPH_SHARE_OF_WATCHES: usize = 4;

/// How many watches graphs may take when the config does not say: a
/// quarter of `limit`, the system's limit on watches, or no limit where
/// the system has none.
fn default_graph_watches(limit: Option<usize>) -> Option<usize> {
    limit.map(|limit| limit / GRAPH_SHARE_OF_WATCHES)
}

impl Default for Config {
    fn default() -> Self {
        Self {
            autostart: Autostart::default(),
            idle_exit: None,
            idle_policy: IdlePolicy {
                pause_after: Some(Duration::from_secs(10 * 60)),
                stop_after: Some(Duration::from_secs(2 * 60 * 60)),
                memory_budget_bytes: None,
            },
            warms_up_on_activity: true,
            when_slow: WhenSlow::default(),
            sentry_for_mcp: true,
            graph_watches: default_graph_watches(lemma_index::watch_limit()),
            graph_prepare: Prepare::default(),
            graphs_kept_ready: GRAPHS_KEPT_READY,
        }
    }
}

#[derive(Deserialize, Default)]
#[serde(deny_unknown_fields)]
struct ConfigFile {
    #[serde(default)]
    daemon: DaemonSection,
    #[serde(default)]
    servers: ServersSection,
    #[serde(default)]
    sentry: SentrySection,
    #[serde(default)]
    repositories: RepositoriesSection,
    #[serde(default)]
    runtime: RuntimeSection,
}

/// A number of watches, or `"none"` for no limit.
#[derive(Deserialize)]
#[serde(untagged)]
enum Watches {
    Count(usize),
    Words(String),
}

/// How many watches `watches` allows: `None` for no limit.
fn parse_watches(watches: &Watches) -> anyhow::Result<Option<usize>> {
    match watches {
        Watches::Count(count) => Ok(Some(*count)),
        Watches::Words(words) if words.trim() == "none" => Ok(None),
        Watches::Words(words) => bail!("watches is a number or \"none\", not \"{words}\""),
    }
}

/// What warm runtimes may use: how much memory they may hold, and how long
/// an idle one is kept. Each is a point between checks that start a
/// repository's interpreter from nothing every time (`memory = "0"`) and
/// checks that always find it ready.
#[derive(Deserialize, Default)]
#[serde(deny_unknown_fields)]
struct RuntimeSection {
    memory: Option<String>,
    stop_after: Option<String>,
}

#[derive(Deserialize, Default)]
#[serde(deny_unknown_fields)]
struct SentrySection {
    mcp: Option<bool>,
    /// Whether a project's language servers are started again when its
    /// files change.
    servers: Option<bool>,
    /// How far sentry gets ready for the next graph once files change.
    graphs: Option<Prepare>,
    /// How many of the graphs last asked are answered ahead.
    kept_ready: Option<usize>,
    /// How many watches graphs across the workspace may take: a point
    /// between graphs that ask Git about every repository each time
    /// (`watches = 0`) and graphs that are told of every change.
    watches: Option<Watches>,
}

#[derive(Deserialize, Default)]
#[serde(deny_unknown_fields)]
struct RepositoriesSection {
    clone: Option<ClonePolicy>,
}

/// Whether `lemma index` and `lemma check` fetch a `git(...)` repository that
/// is not in the clone cache.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Deserialize)]
#[serde(rename_all = "lowercase")]
pub(crate) enum ClonePolicy {
    /// Ask in an interactive terminal, and clone otherwise.
    #[default]
    Ask,
    /// Clone without asking.
    Always,
    /// Never clone: stop at the repositories that are here.
    Never,
}

/// The clone policy for a description in `directory`: its
/// `.lemma/config.toml`, then the user's config file.
pub(crate) fn clone_policy(directory: &Path) -> anyhow::Result<ClonePolicy> {
    let local = directory.join(".lemma").join("config.toml");
    if let Some(policy) = repositories_clone(&local)? {
        return Ok(policy);
    }
    let Some(global) = config_path() else {
        return Ok(ClonePolicy::default());
    };
    Ok(repositories_clone(&global)?.unwrap_or_default())
}

fn repositories_clone(path: &Path) -> anyhow::Result<Option<ClonePolicy>> {
    if !path.is_file() {
        return Ok(None);
    }
    let text =
        fs::read_to_string(path).with_context(|| format!("could not read {}", path.display()))?;
    let file: ConfigFile =
        toml::from_str(&text).with_context(|| format!("invalid {}", path.display()))?;
    Ok(file.repositories.clone)
}

#[derive(Deserialize, Default)]
#[serde(deny_unknown_fields)]
struct DaemonSection {
    autostart: Option<Autostart>,
    idle_exit: Option<String>,
}

#[derive(Deserialize, Default)]
#[serde(deny_unknown_fields)]
struct ServersSection {
    #[serde(alias = "freeze_after")]
    pause_after: Option<String>,
    stop_after: Option<String>,
    memory_budget: Option<String>,
    when_slow: Option<SlowServer>,
    give_way_after: Option<String>,
    try_again_after: Option<String>,
}

/// What a lookup does about a language server that does not answer in the
/// time the lookup allows (`--timeout-secs`).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Deserialize)]
#[serde(rename_all = "lowercase")]
enum SlowServer {
    /// The lookup fails and says so. The default: few repositories are
    /// large enough for a server to be that slow, and one that is slow for
    /// another reason should be seen.
    #[default]
    Report,
    /// The next installed server for the language is asked once the slow
    /// one has had `give_way_after`, and the slow one is passed over in
    /// that repository for `try_again_after`.
    Next,
}

/// How long a slow server is passed over unless the user says: long enough
/// that waiting for it again is rare, short enough that a server slow only
/// while the machine was busy is back within the hour.
const TRY_AGAIN_AFTER: Duration = Duration::from_secs(30 * 60);

/// What `[servers]` says lookups do about a slow server.
fn when_slow(section: &ServersSection) -> anyhow::Result<WhenSlow> {
    let give_way_after = match &section.give_way_after {
        Some(text) => Some(parse_give_way_after(text)?),
        None => None,
    };
    let try_again_after = match &section.try_again_after {
        Some(text) => crate::duration::parse_duration(text)?,
        None => Some(TRY_AGAIN_AFTER),
    };
    Ok(match section.when_slow.unwrap_or_default() {
        SlowServer::Report => WhenSlow::Report,
        SlowServer::Next => WhenSlow::Next {
            give_way_after,
            try_again_after,
        },
    })
}

/// How long a server may take over a question before the next is asked: a
/// time, since each end of the range has a setting that says it better.
fn parse_give_way_after(text: &str) -> anyhow::Result<Duration> {
    match crate::duration::parse_duration(text)? {
        None => bail!("give_way_after is a time; `when_slow = \"report\"` never asks the next"),
        Some(Duration::ZERO) => bail!(
            "give_way_after is more than no time; to ask another server first, list it in `.lemma/providers.toml`"
        ),
        Some(after) => Ok(after),
    }
}

/// Who may start a daemon, by the user's config file; the default when
/// the file cannot be read, which whatever starts then reports.
pub(crate) fn autostart() -> Autostart {
    load(config_path().as_deref()).map_or_else(|_| Autostart::default(), |config| config.autostart)
}

/// Whether the MCP server asks for sentry when it starts, by the user's
/// config file; it does when the file cannot be read.
pub(crate) fn sentry_for_mcp() -> bool {
    load(config_path().as_deref()).map_or(true, |config| config.sentry_for_mcp)
}

/// What lookups in a command's own process do about a slow server, by the
/// user's config file: the daemon is told the same as part of its settings.
pub(crate) fn when_slow_for_one_process() -> anyhow::Result<WhenSlow> {
    Ok(load(config_path().as_deref())?.when_slow)
}

/// What warm runtimes may use for a description in `directory`: the user's
/// config file says, and the project's `.lemma/config.toml` says otherwise
/// for itself, setting by setting. `LEMMA_RUNTIME=off` keeps none warm,
/// whatever either file says.
pub(crate) fn runtime_allowance(directory: &Path) -> anyhow::Result<crate::runtime::Allowance> {
    let local = directory.join(".lemma").join("config.toml");
    let files: Vec<PathBuf> = config_path().into_iter().chain([local]).collect();
    let mut allowance = layered_allowance(&files)?;
    if std::env::var_os("LEMMA_RUNTIME").is_some_and(|value| value == "off") {
        allowance.memory_bytes = 0;
    }
    Ok(allowance)
}

/// The defaults with what each of `files` that exists says set on top, in
/// order: a later file overrides an earlier one setting by setting.
fn layered_allowance(files: &[PathBuf]) -> anyhow::Result<crate::runtime::Allowance> {
    let mut allowance = crate::runtime::Allowance::default();
    for path in files {
        if let Some(section) = runtime_section(path)? {
            apply_runtime(&section, &mut allowance)
                .with_context(|| format!("invalid settings in {}", path.display()))?;
        }
    }
    Ok(allowance)
}

fn runtime_section(path: &Path) -> anyhow::Result<Option<RuntimeSection>> {
    if !path.is_file() {
        return Ok(None);
    }
    let text =
        fs::read_to_string(path).with_context(|| format!("could not read {}", path.display()))?;
    let file: ConfigFile =
        toml::from_str(&text).with_context(|| format!("invalid {}", path.display()))?;
    Ok(Some(file.runtime))
}

/// Sets on `allowance` what `section` says.
fn apply_runtime(
    section: &RuntimeSection,
    allowance: &mut crate::runtime::Allowance,
) -> anyhow::Result<()> {
    if let Some(memory) = &section.memory {
        allowance.memory_bytes = parse_memory(memory)?;
    }
    if let Some(stop_after) = &section.stop_after {
        let kept = crate::duration::parse_duration(stop_after)?;
        // `never` keeps an idle runtime as long as the daemon runs.
        allowance.stop_after_ms = kept.map_or(u64::MAX, |kept| {
            u64::try_from(kept.as_millis()).unwrap_or(u64::MAX)
        });
    }
    Ok(())
}

/// `0` (keep none), a size such as `256MB` or `2GB`, or `none` for no limit.
fn parse_memory(text: &str) -> anyhow::Result<u64> {
    if text.trim() == "0" {
        return Ok(0);
    }
    Ok(parse_size(text)?.unwrap_or(u64::MAX))
}

/// Where the config file is, whether or not it exists.
pub(crate) fn config_path() -> Option<PathBuf> {
    if let Some(path) = std::env::var_os("LEMMA_CONFIG") {
        return Some(PathBuf::from(path));
    }
    let base = std::env::var_os("XDG_CONFIG_HOME")
        .map(PathBuf::from)
        .or_else(|| std::env::var_os("HOME").map(|home| PathBuf::from(home).join(".config")))?;
    Some(base.join("lemma").join("config.toml"))
}

/// The settings in `path`, or the defaults if it does not exist.
pub(crate) fn load(path: Option<&Path>) -> anyhow::Result<Config> {
    let Some(path) = path.filter(|path| path.exists()) else {
        return Ok(Config::default());
    };
    let text =
        fs::read_to_string(path).with_context(|| format!("could not read {}", path.display()))?;
    parse(&text).with_context(|| format!("invalid settings in {}", path.display()))
}

/// When the file at `path` last changed, to notice edits.
pub(crate) fn modified(path: Option<&Path>) -> Option<SystemTime> {
    fs::metadata(path?)
        .and_then(|metadata| metadata.modified())
        .ok()
}

fn parse(text: &str) -> anyhow::Result<Config> {
    let file: ConfigFile = toml::from_str(text)?;
    let defaults = Config::default();
    let when_slow = when_slow(&file.servers)?;
    let pick = |value: Option<String>, default: Option<Duration>| match value {
        Some(text) => crate::duration::parse_duration(&text),
        None => Ok(default),
    };
    Ok(Config {
        autostart: file.daemon.autostart.unwrap_or_default(),
        idle_exit: pick(file.daemon.idle_exit, defaults.idle_exit)?,
        idle_policy: IdlePolicy {
            pause_after: pick(file.servers.pause_after, defaults.idle_policy.pause_after)?,
            stop_after: pick(file.servers.stop_after, defaults.idle_policy.stop_after)?,
            memory_budget_bytes: match file.servers.memory_budget {
                Some(text) => parse_size(&text)?,
                None => defaults.idle_policy.memory_budget_bytes,
            },
        },
        warms_up_on_activity: file.sentry.servers.unwrap_or(defaults.warms_up_on_activity),
        when_slow,
        sentry_for_mcp: file.sentry.mcp.unwrap_or(defaults.sentry_for_mcp),
        graph_watches: match &file.sentry.watches {
            Some(watches) => parse_watches(watches)?,
            None => defaults.graph_watches,
        },
        graph_prepare: file.sentry.graphs.unwrap_or(defaults.graph_prepare),
        graphs_kept_ready: file.sentry.kept_ready.unwrap_or(defaults.graphs_kept_ready),
    })
}

/// `512MB`, `2GB`, or `none`.
fn parse_size(text: &str) -> anyhow::Result<Option<u64>> {
    let text = text.trim();
    if text.eq_ignore_ascii_case("none") {
        return Ok(None);
    }
    let (number, unit) = crate::duration::split_number(text);
    let Ok(amount) = number.parse::<u64>() else {
        bail!("`{text}` is not a size; use a number with MB or GB, or `none`");
    };
    let bytes = match unit.to_ascii_uppercase().as_str() {
        "MB" => amount * 1024 * 1024,
        "GB" => amount * 1024 * 1024 * 1024,
        _ => bail!("`{text}` needs a unit: MB or GB"),
    };
    Ok(Some(bytes))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn written(directory: &Path, name: &str, text: &str) -> PathBuf {
        let path = directory.join(name);
        fs::write(&path, text).unwrap();
        path
    }

    #[test]
    fn a_projects_runtime_settings_override_the_users_one_by_one() {
        let directory = tempfile::tempdir().unwrap();
        let user = written(
            directory.path(),
            "user.toml",
            "[runtime]\nmemory = \"0\"\nstop_after = \"5m\"\n",
        );
        let project = written(
            directory.path(),
            "project.toml",
            "[runtime]\nmemory = \"256MB\"\n",
        );

        let allowance = layered_allowance(&[user, project]).unwrap();

        assert_eq!(allowance.memory_bytes, 256 * 1024 * 1024);
        assert_eq!(allowance.stop_after_ms, 5 * 60 * 1000);
    }

    #[test]
    fn with_no_runtime_settings_runtimes_are_kept_warm() {
        let directory = tempfile::tempdir().unwrap();
        let missing = directory.path().join("missing.toml");

        let allowance = layered_allowance(&[missing]).unwrap();

        assert_eq!(allowance, crate::runtime::Allowance::default());
        assert!(allowance.memory_bytes > 0);
    }

    #[test]
    fn runtime_memory_is_nothing_a_size_or_unbounded() {
        assert_eq!(parse_memory("0").unwrap(), 0);
        assert_eq!(parse_memory("64MB").unwrap(), 64 * 1024 * 1024);
        assert_eq!(parse_memory("none").unwrap(), u64::MAX);
        assert!(parse_memory("lots").is_err());
    }

    #[test]
    fn the_watches_graphs_may_take_are_a_number_or_none() {
        let watches = |text: &str| parse(text).map(|config| config.graph_watches);

        assert_eq!(watches("[sentry]\nwatches = 0\n").unwrap(), Some(0));
        assert_eq!(watches("[sentry]\nwatches = 5000\n").unwrap(), Some(5000));
        assert_eq!(watches("[sentry]\nwatches = \"none\"\n").unwrap(), None);
        assert!(watches("[sentry]\nwatches = \"many\"\n").is_err());
    }

    #[test]
    fn four_graphs_are_kept_ready_unless_the_user_says() {
        let kept_ready = |text: &str| parse(text).map(|config| config.graphs_kept_ready);

        assert_eq!(kept_ready("").unwrap(), 4);
        assert_eq!(kept_ready("[sentry]\nkept_ready = 1\n").unwrap(), 1);
        assert_eq!(kept_ready("[sentry]\nkept_ready = 0\n").unwrap(), 0);
        assert!(kept_ready("[sentry]\nkept_ready = \"all\"\n").is_err());
    }

    #[test]
    fn a_slow_server_is_reported_unless_the_next_is_to_be_asked() {
        let when_slow = |text: &str| parse(text).map(|config| config.when_slow);
        let next = |try_again_after| WhenSlow::Next {
            give_way_after: None,
            try_again_after,
        };
        let minutes = |count: u64| Some(Duration::from_secs(count * 60));

        assert_eq!(when_slow("").unwrap(), WhenSlow::Report);
        assert_eq!(
            when_slow("[servers]\nwhen_slow = \"report\"\ntry_again_after = \"5m\"\n").unwrap(),
            WhenSlow::Report
        );
        assert_eq!(
            when_slow("[servers]\nwhen_slow = \"next\"\n").unwrap(),
            next(minutes(30))
        );
        assert_eq!(
            when_slow("[servers]\nwhen_slow = \"next\"\ntry_again_after = \"5m\"\n").unwrap(),
            next(minutes(5))
        );
        assert_eq!(
            when_slow("[servers]\nwhen_slow = \"next\"\ntry_again_after = \"0s\"\n").unwrap(),
            next(minutes(0))
        );
        assert_eq!(
            when_slow("[servers]\nwhen_slow = \"next\"\ntry_again_after = \"never\"\n").unwrap(),
            next(None)
        );
        assert!(when_slow("[servers]\nwhen_slow = \"skip\"\n").is_err());
        assert!(
            when_slow("[servers]\nwhen_slow = \"next\"\ntry_again_after = \"soon\"\n").is_err()
        );
    }

    #[test]
    fn how_long_a_slow_server_gets_is_the_lookups_wait_unless_set() {
        let give_way_after = |text: &str| {
            let settings = format!("[servers]\nwhen_slow = \"next\"\n{text}");
            match parse(&settings)?.when_slow {
                WhenSlow::Next { give_way_after, .. } => Ok(give_way_after),
                WhenSlow::Report => bail!("the next server is to be asked"),
            }
        };

        assert_eq!(give_way_after("").unwrap(), None);
        assert_eq!(
            give_way_after("give_way_after = \"20s\"\n").unwrap(),
            Some(Duration::from_secs(20))
        );
        assert!(give_way_after("give_way_after = \"never\"\n").is_err());
        assert!(give_way_after("give_way_after = \"0s\"\n").is_err());
    }

    #[test]
    fn how_far_graphs_are_prepared_is_one_of_three_points() {
        let prepare = |text: &str| parse(text).map(|config| config.graph_prepare);

        assert_eq!(prepare("").unwrap(), Prepare::Graph);
        assert_eq!(
            prepare("[sentry]\ngraphs = \"index\"\n").unwrap(),
            Prepare::Index
        );
        assert_eq!(
            prepare("[sentry]\ngraphs = \"off\"\n").unwrap(),
            Prepare::Off
        );
        assert_eq!(
            prepare("[sentry]\ngraphs = \"graph\"\n").unwrap(),
            Prepare::Graph
        );
        assert!(prepare("[sentry]\ngraphs = \"everything\"\n").is_err());
    }

    #[test]
    fn unless_set_graphs_may_take_a_quarter_of_the_systems_watches() {
        assert_eq!(default_graph_watches(Some(8192)), Some(2048));
        assert_eq!(default_graph_watches(None), None);
    }

    #[test]
    fn any_command_starts_a_daemon_unless_the_file_says_who_may() {
        let who = |text: &str| parse(text).map(|config| config.autostart);

        assert_eq!(who("").unwrap(), Autostart::Always);
        assert_eq!(
            who("[daemon]\nautostart = \"clients\"\n").unwrap(),
            Autostart::Clients
        );
        assert_eq!(
            who("[daemon]\nautostart = \"never\"\n").unwrap(),
            Autostart::Never
        );
        assert!(who("[daemon]\nautostart = \"sometimes\"\n").is_err());
    }

    #[test]
    fn an_empty_file_gives_the_defaults() {
        assert_eq!(parse("").unwrap(), Config::default());
    }

    #[test]
    fn every_setting_can_be_changed() {
        let config = parse(
            "[daemon]\nidle_exit = \"30m\"\n\n[servers]\npause_after = \"never\"\nstop_after = \"1d\"\nmemory_budget = \"2GB\"\n\n[sentry]\nservers = false\n",
        )
        .unwrap();

        assert_eq!(config.idle_exit, Some(Duration::from_secs(1800)));
        assert_eq!(config.idle_policy.pause_after, None);
        assert_eq!(
            config.idle_policy.stop_after,
            Some(Duration::from_secs(86_400))
        );
        assert_eq!(
            config.idle_policy.memory_budget_bytes,
            Some(2 * 1024 * 1024 * 1024)
        );
        assert!(!config.warms_up_on_activity);
    }

    #[test]
    fn freeze_after_is_accepted_for_pause_after() {
        let config = parse("[servers]\nfreeze_after = \"5m\"\n").unwrap();

        assert_eq!(
            config.idle_policy.pause_after,
            Some(Duration::from_secs(300))
        );
    }

    #[test]
    fn mistakes_are_reported() {
        assert!(parse("[servers]\npause_after = \"10 minutes\"\n").is_err());
        assert!(parse("[servers]\nmemory_budget = \"2TB\"\n").is_err());
        assert!(parse("[servers]\npause_afetr = \"10m\"\n").is_err());
    }
}
