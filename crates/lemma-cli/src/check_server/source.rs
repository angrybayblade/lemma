//! Where `lemma lsp` gets its checks.
//!
//! Sentry first: the daemon's attachment to the workspace
//! (`.specs/14-sentry.md`), shared with `lemma check` and the MCP `check`
//! tool, so the workspace is watched and read once for all of them and an
//! answer one client asked for is given again to the next. Its journal says
//! which repositories changed, and each check is an ask, as from `lemma
//! check --change`. When sentry cannot serve the workspace, as when it is
//! attached to another, the server watches the workspace itself.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::mpsc::{self, Receiver, Sender};
use std::time::{Duration, Instant};

use crate::check::Served;
use crate::cli::{CheckArgs, LanguageServerArgs};
use crate::watched::{BATCH_INTERVAL, WatchedWorkspace};

use super::diagnostics::CheckedChange;

/// How long a repository's changes must be quiet before it is checked.
const SETTLE: Duration = Duration::from_millis(300);
/// How long one wait for sentry's events lasts before it is asked again.
const EVENTS_WAIT: Duration = Duration::from_secs(30);

/// A source of checks for the server.
pub(crate) trait Source {
    /// The repositories, by the directory checks name them by, whose own
    /// files changed and have settled since this was last asked.
    fn settled(&mut self) -> Vec<PathBuf>;

    /// What checking the change to the repository named `name` found.
    fn check(&mut self, name: &str) -> Result<Checked, CheckFailure>;

    /// Whether it can no longer serve, so the server should use another.
    fn is_lost(&self) -> bool;

    /// How it checks, for the editor's log.
    fn describe(&self) -> String;
}

/// A check's findings, and how long evaluating them took where the source
/// says: sentry does, so the time spent asking is the rest.
pub(crate) struct Checked {
    pub change: CheckedChange,
    pub evaluation: Option<Duration>,
}

/// Why a check gave no findings.
#[derive(Debug, thiserror::Error)]
pub(crate) enum CheckFailure {
    /// The source cannot serve the workspace any more; another should.
    #[error("sentry no longer serves the workspace")]
    NotServed,
    /// The check itself failed.
    #[error("{0}")]
    Failed(String),
}

/// The source to check `manifest`'s workspace with: sentry, if `asks_sentry`,
/// asked of the workspace's daemon and kept for as long as this server
/// runs, unless `LEMMA_SENTRY=off` or no daemon may serve it; this process
/// otherwise.
pub(crate) fn open(
    args: &LanguageServerArgs,
    manifest: &Path,
    repositories: &[PathBuf],
    asks_sentry: bool,
) -> anyhow::Result<Box<dyn Source>> {
    let may_ask = crate::config::sentry_autostart() != crate::config::Autostart::Never;
    if asks_sentry && may_ask && !crate::sentry::is_turned_off() {
        match crate::daemon::hold_sentry(manifest) {
            Ok(Some(hold)) => {
                let followed = SentrySource::follow(args, manifest, repositories, hold);
                return Ok(Box::new(followed));
            }
            Ok(None) => {}
            Err(error) => log::info!("sentry is not used: {error:#}"),
        }
    }
    Ok(Box::new(Local::start(args, manifest, repositories)?))
}

/// The daemon's sentry, followed through its journal.
struct SentrySource {
    args: LanguageServerArgs,
    manifest: PathBuf,
    /// What the thread following the journal reports.
    events: Receiver<Followed>,
    /// Each repository's directory as the journal names it (canonical), with
    /// the directory checks name it by.
    spelled: BTreeMap<PathBuf, PathBuf>,
    /// Repositories that changed, with when they last did.
    changing: BTreeMap<PathBuf, Instant>,
    /// The journal token of the last change followed: an ask is about the
    /// changes up to it.
    as_of: Option<String>,
    is_lost: bool,
    /// Keeps sentry attached while this server runs.
    _hold: crate::daemon::SentryHold,
}

/// What following sentry's journal reports.
enum Followed {
    /// A repository's own files changed: its directory, canonical, and the
    /// journal token of the change.
    Changed(PathBuf, String),
    /// Sentry, or its daemon, went away.
    Lost,
}

impl SentrySource {
    fn follow(
        args: &LanguageServerArgs,
        manifest: &Path,
        repositories: &[PathBuf],
        hold: crate::daemon::SentryHold,
    ) -> Self {
        let (sender, events) = mpsc::channel();
        let workspace = manifest.parent().unwrap_or(manifest).to_path_buf();
        if let Err(error) = std::thread::Builder::new()
            .name("lemma-lsp-sentry".to_owned())
            .spawn(move || follow_journal(&workspace, &sender))
        {
            log::warn!("cannot follow sentry's journal: {error}");
        }
        let spelled = repositories
            .iter()
            .map(|spelled| {
                let canonical = spelled.canonicalize().unwrap_or_else(|_| spelled.clone());
                (canonical, spelled.clone())
            })
            .collect();
        Self {
            args: args.clone(),
            manifest: manifest.to_path_buf(),
            events,
            spelled,
            changing: BTreeMap::new(),
            as_of: None,
            is_lost: false,
            _hold: hold,
        }
    }

    /// The command line of an ask: `lemma check` of the workspace, for one
    /// repository's change, as JSON, never cloning.
    fn asked(&self, name: &str) -> Vec<String> {
        let mut asked = vec![
            "check".to_owned(),
            self.manifest.display().to_string(),
            "--change".to_owned(),
            name.to_owned(),
            "--json".to_owned(),
            "--no-clone".to_owned(),
        ];
        if let Some(level) = &self.args.level {
            asked.extend(["--level".to_owned(), level.clone()]);
        }
        if let Some(engine) = &self.args.engine {
            asked.extend(["--engine".to_owned(), engine.clone()]);
        }
        if self.args.no_run {
            asked.push("--no-run".to_owned());
        }
        asked
    }
}

impl Source for SentrySource {
    fn settled(&mut self) -> Vec<PathBuf> {
        let now = Instant::now();
        while let Ok(followed) = self.events.try_recv() {
            match followed {
                Followed::Changed(repository, token) => {
                    self.changing.insert(repository, now);
                    self.as_of = Some(token);
                }
                Followed::Lost => self.is_lost = true,
            }
        }
        let settled: Vec<PathBuf> = self
            .changing
            .iter()
            .filter(|(_, last)| now.duration_since(**last) >= SETTLE)
            .map(|(repository, _)| repository.clone())
            .collect();
        settled
            .into_iter()
            .filter_map(|repository| {
                self.changing.remove(&repository);
                self.spelled.get(&repository).cloned()
            })
            .collect()
    }

    fn check(&mut self, name: &str) -> Result<Checked, CheckFailure> {
        let directory = self
            .manifest
            .parent()
            .unwrap_or(Path::new("/"))
            .to_path_buf();
        let environment = std::env::vars().collect();
        let asked = self.asked(name);
        let as_of = self.as_of.clone();
        let asked = crate::daemon::AskedCheck {
            args: asked,
            cwd: directory,
            environment,
            as_of,
        };
        let Some(answer) = crate::daemon::ask_for_check_as_of(asked, crate::daemon::Asker::Server)
        else {
            self.is_lost = true;
            return Err(CheckFailure::NotServed);
        };
        if !matches!(answer.exit_code, 0 | 4 | 5) {
            return Err(CheckFailure::Failed(answer.stderr.trim().to_owned()));
        }
        read_answer(&answer.stdout).map_err(|error| CheckFailure::Failed(error.to_string()))
    }

    fn is_lost(&self) -> bool {
        self.is_lost
    }

    fn describe(&self) -> String {
        "checking through the daemon's sentry, shared with `lemma check` and MCP".to_owned()
    }
}

/// A sentry answer's JSON: the report, and how long sentry spent
/// evaluating it.
fn read_answer(stdout: &str) -> serde_json::Result<Checked> {
    #[derive(serde::Deserialize)]
    struct Note {
        evaluation_ms: f64,
    }
    #[derive(serde::Deserialize)]
    struct Answered {
        #[serde(flatten)]
        change: CheckedChange,
        sentry: Option<Note>,
    }
    let answered: Answered = serde_json::from_str(stdout)?;
    let evaluation = answered
        .sentry
        .map(|note| Duration::from_secs_f64(note.evaluation_ms / 1000.0));
    Ok(Checked {
        change: answered.change,
        evaluation,
    })
}

/// Follows the journal of the sentry attached to the workspace in
/// `workspace`, reporting each repository whose own files changed, until
/// the server stops listening or sentry goes away. Batches that touched
/// only Git's files, which a check's own `git status` rewrites, are not
/// changes to check.
fn follow_journal(workspace: &Path, sender: &Sender<Followed>) {
    // The journal before now is history: the server checks what has
    // uncommitted changes when it starts.
    let first = read_events(workspace, None, Duration::ZERO);
    let Some(mut token) = first.map(|events| events.token) else {
        let _ = sender.send(Followed::Lost);
        return;
    };
    loop {
        let Some(events) = read_events(workspace, Some(token.clone()), EVENTS_WAIT) else {
            let _ = sender.send(Followed::Lost);
            return;
        };
        for event in events.events {
            let is_change = event.kind == "batch" && (event.paths > 0 || event.bulk);
            let Some(repository) = event.repository.filter(|_| is_change) else {
                continue;
            };
            let changed = Followed::Changed(PathBuf::from(repository), event.token);
            if sender.send(changed).is_err() {
                return;
            }
        }
        token = events.token;
    }
}

/// A page of sentry's journal.
#[derive(serde::Deserialize)]
struct Events {
    events: Vec<Event>,
    token: String,
}

/// One entry of sentry's journal, as far as the server reads it.
#[derive(serde::Deserialize)]
struct Event {
    kind: String,
    token: String,
    repository: Option<String>,
    #[serde(default)]
    paths: usize,
    #[serde(default)]
    bulk: bool,
}

/// The journal of the workspace in `workspace` after `since`, waiting up
/// to `wait` for an entry; `None` when no daemon answers or sentry is not
/// attached.
fn read_events(workspace: &Path, since: Option<String>, wait: Duration) -> Option<Events> {
    let action = crate::daemon::SentryAction::Events {
        since,
        wait_ms: u64::try_from(wait.as_millis()).unwrap_or(u64::MAX),
        json: true,
    };
    let (stdout, _, exit_code) = crate::daemon::sentry_control(workspace, action).ok()??;
    if exit_code != 0 {
        return None;
    }
    serde_json::from_str(&stdout).ok()
}

/// The server's own watched workspace, for when sentry cannot serve it.
struct Local {
    args: LanguageServerArgs,
    manifest: PathBuf,
    workspace: WatchedWorkspace,
    repositories: Vec<PathBuf>,
}

impl Local {
    fn start(
        args: &LanguageServerArgs,
        manifest: &Path,
        repositories: &[PathBuf],
    ) -> anyhow::Result<Self> {
        let mut workspace = WatchedWorkspace::start(manifest)?;
        // Takes what the watchers report as they start, which is no edit.
        workspace.collect(true);
        Ok(Self {
            args: args.clone(),
            manifest: manifest.to_path_buf(),
            workspace,
            repositories: repositories.to_vec(),
        })
    }
}

impl Source for Local {
    fn settled(&mut self) -> Vec<PathBuf> {
        self.workspace.collect(false);
        let mut settled = Vec::new();
        for changes in self.workspace.take_settled(Instant::now()) {
            let own = super::own_paths(&changes.repository, &changes.paths, &self.repositories);
            log::debug!(
                "changes settled in {}: {own:?} of {} paths{}",
                changes.repository.display(),
                changes.paths.len(),
                if changes.is_bulk { ", in bulk" } else { "" }
            );
            // Nothing of its own changed: only Git's files, which a check's
            // own `git status` rewrites, or files of a repository inside it.
            if own.is_empty() && !changes.is_bulk {
                continue;
            }
            settled.push(changes.repository);
        }
        settled
    }

    fn check(&mut self, name: &str) -> Result<Checked, CheckFailure> {
        // Drops what is kept of every change reported so far. This does not
        // wait for the watchers' barrier, as an ask to sentry does, since
        // that would forget which paths of other repositories changed: a
        // change reported after this check gets a check of its own.
        self.workspace.collect(false);
        let warm = Arc::clone(self.workspace.warm());
        let report = check_change(&self.args, &self.manifest, name, warm)
            .map_err(|error| CheckFailure::Failed(format!("{error:#}")))?;
        Ok(Checked {
            change: CheckedChange::from(&report),
            evaluation: None,
        })
    }

    fn is_lost(&self) -> bool {
        false
    }

    fn describe(&self) -> String {
        format!(
            "checking in this process (sentry cannot serve this workspace); watching \
             every {} ms",
            BATCH_INTERVAL.as_millis()
        )
    }
}

/// Checks the change to the repository named `name` as `lemma check
/// --change` would, reading through `warm`.
fn check_change(
    args: &LanguageServerArgs,
    manifest: &Path,
    name: &str,
    warm: Arc<lemma_graph::Warm>,
) -> anyhow::Result<lemma_graph::Report> {
    let asked = CheckArgs {
        manifest: manifest.to_path_buf(),
        // An editor's server has no one to ask, and cloning behind its back
        // would surprise.
        no_clone: true,
        change: Some(name.to_owned()),
        base: None,
        head: None,
        level: args.level.clone(),
        no_run: args.no_run,
        run_base: false,
        select: None,
        since: None,
        all: false,
        engine: args.engine.clone(),
        evaluation: None,
        explain: false,
    };
    let served = Served {
        manifest,
        warm,
        environment: Arc::new(std::env::vars().collect()),
    };
    crate::check::evaluate(&asked, Some(&served))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn an_answer_gives_its_report_and_how_long_sentry_evaluated() {
        let stdout = r#"{"level": "check", "links": 0, "repositories": [],
            "findings": [], "sentry": {"token": "a-1", "reused": false, "evaluation_ms": 1500.0}}"#;

        let checked = read_answer(stdout).unwrap();

        assert_eq!(checked.evaluation, Some(Duration::from_millis(1500)));
        assert_eq!(checked.change.findings, Vec::new());
    }
}
