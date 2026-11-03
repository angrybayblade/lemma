//! The user's settings for the daemon: when it exits, and how it treats
//! language servers nobody is using.
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
