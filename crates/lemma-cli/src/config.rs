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
use lemma_query::IdlePolicy;
use serde::Deserialize;

/// Everything the config file can set.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct Config {
    /// Exit the daemon after this long without a command. `None` keeps it
    /// running.
    pub idle_exit: Option<Duration>,
    /// When to pause and stop idle language servers.
    pub idle_policy: IdlePolicy,
    /// Start a project's servers again when its files change, if they were
    /// stopped, so they are ready by the next lookup.
    pub warms_up_on_activity: bool,
    /// Attach sentry to a workspace on its first `lemma check` while the
    /// daemon runs. Off unless the user turns it on.
    pub sentry_auto_attach: bool,
    /// How many watches the workspaces graphs are traced through may take
    /// between them, where each directory takes one. `None` sets no limit;
    /// `Some(0)` watches nothing.
    pub graph_watches: Option<usize>,
    /// How far the daemon gets ready for the next graph through a
    /// workspace once its files change.
    pub graph_prepare: Prepare,
}

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
            idle_exit: None,
            idle_policy: IdlePolicy {
                pause_after: Some(Duration::from_secs(10 * 60)),
                stop_after: Some(Duration::from_secs(2 * 60 * 60)),
                memory_budget_bytes: None,
            },
            warms_up_on_activity: true,
            sentry_auto_attach: false,
            graph_watches: default_graph_watches(lemma_index::watch_limit()),
            graph_prepare: Prepare::default(),
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
    #[serde(default)]
    graph: GraphSection,
}

/// What the daemon may take to keep graphs across a workspace quick. The
/// watches are a point between graphs that ask Git about every repository
/// each time (`watches = 0`) and graphs that are told of every change.
#[derive(Deserialize, Default)]
#[serde(deny_unknown_fields)]
struct GraphSection {
    watches: Option<Watches>,
    prepare: Option<Prepare>,
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
    auto_attach: Option<bool>,
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
    idle_exit: Option<String>,
}

#[derive(Deserialize, Default)]
#[serde(deny_unknown_fields)]
struct ServersSection {
    #[serde(alias = "freeze_after")]
    pause_after: Option<String>,
    stop_after: Option<String>,
    memory_budget: Option<String>,
    warm_on_activity: Option<bool>,
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
    let pick = |value: Option<String>, default: Option<Duration>| match value {
        Some(text) => crate::duration::parse_duration(&text),
        None => Ok(default),
    };
    Ok(Config {
        idle_exit: pick(file.daemon.idle_exit, defaults.idle_exit)?,
        idle_policy: IdlePolicy {
            pause_after: pick(file.servers.pause_after, defaults.idle_policy.pause_after)?,
            stop_after: pick(file.servers.stop_after, defaults.idle_policy.stop_after)?,
            memory_budget_bytes: match file.servers.memory_budget {
                Some(text) => parse_size(&text)?,
                None => defaults.idle_policy.memory_budget_bytes,
            },
        },
        warms_up_on_activity: file
            .servers
            .warm_on_activity
            .unwrap_or(defaults.warms_up_on_activity),
        sentry_auto_attach: file
            .sentry
            .auto_attach
            .unwrap_or(defaults.sentry_auto_attach),
        graph_watches: match &file.graph.watches {
            Some(watches) => parse_watches(watches)?,
            None => defaults.graph_watches,
        },
        graph_prepare: file.graph.prepare.unwrap_or(defaults.graph_prepare),
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

        assert_eq!(watches("[graph]\nwatches = 0\n").unwrap(), Some(0));
        assert_eq!(watches("[graph]\nwatches = 5000\n").unwrap(), Some(5000));
        assert_eq!(watches("[graph]\nwatches = \"none\"\n").unwrap(), None);
        assert!(watches("[graph]\nwatches = \"many\"\n").is_err());
    }

    #[test]
    fn how_far_graphs_are_prepared_is_one_of_three_points() {
        let prepare = |text: &str| parse(text).map(|config| config.graph_prepare);

        assert_eq!(prepare("").unwrap(), Prepare::Graph);
        assert_eq!(
            prepare("[graph]\nprepare = \"index\"\n").unwrap(),
            Prepare::Index
        );
        assert_eq!(
            prepare("[graph]\nprepare = \"off\"\n").unwrap(),
            Prepare::Off
        );
        assert_eq!(
            prepare("[graph]\nprepare = \"graph\"\n").unwrap(),
            Prepare::Graph
        );
        assert!(prepare("[graph]\nprepare = \"everything\"\n").is_err());
    }

    #[test]
    fn unless_set_graphs_may_take_a_quarter_of_the_systems_watches() {
        assert_eq!(default_graph_watches(Some(8192)), Some(2048));
        assert_eq!(default_graph_watches(None), None);
    }

    #[test]
    fn an_empty_file_gives_the_defaults() {
        assert_eq!(parse("").unwrap(), Config::default());
    }

    #[test]
    fn every_setting_can_be_changed() {
        let config = parse(
            "[daemon]\nidle_exit = \"30m\"\n\n[servers]\npause_after = \"never\"\nstop_after = \"1d\"\nmemory_budget = \"2GB\"\nwarm_on_activity = false\n",
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
