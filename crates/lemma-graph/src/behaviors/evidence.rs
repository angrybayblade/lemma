//! Evidence: running checkers and keeping what they observed, keyed by
//! everything a result depends on, so it is reused while those inputs hold
//! (spec 13, section 10).
//!
//! A key covers the checker's address and declaration, the arguments, the
//! state of the code in every repository it reads, and its environment.
//! Code is named by its Git tree, plus a digest of changed files for a
//! working directory, as adapters' documents are. Results are kept under
//! `evidence/` in Lemma's cache, one JSON file per key, with the key each
//! case last ran with, so a run that may not start checkers can tell stale
//! evidence from none.

use std::collections::BTreeMap;
use std::path::{Component, Path, PathBuf};
use std::sync::Arc;

use lemma_lang::eval::Evidence;
use lemma_lang::model::Checker;
use serde_json::{Value as Json, json};

use super::encode::{Decoded, decode_output};
use crate::repos::{self, Checkout};

/// Bumped when what checkers are given, or how their output is read,
/// changes.
const PROTOCOL: &str = "1";

/// How long a checker may run when it does not say.
const CHECKER_TIMEOUT_MS: u64 = 600_000;

/// How long a checker's `setup` may run.
const SETUP_TIMEOUT_MS: u64 = 1_200_000;

/// Where evidence is kept, and whether checkers may run.
pub(crate) struct Store {
    directory: PathBuf,
    cache: PathBuf,
    may_run: bool,
    /// The environment checkers start from instead of this process's own,
    /// for a check answered on behalf of another process.
    environment: Option<Arc<BTreeMap<String, String>>>,
}

/// Where a repository's code is read, and the state of that code: `None`
/// outside Git, where nothing can be reused.
#[derive(Debug, Clone)]
pub(crate) struct Place {
    pub(crate) checkout: Checkout,
    pub(crate) state: Option<String>,
}

impl Place {
    /// The checkout and the state of its code.
    pub(crate) fn of(checkout: &Checkout) -> Place {
        let state = match &checkout.commit {
            Some(commit) => crate::adapters::commit_state(&checkout.root, commit).ok(),
            None => crate::adapters::working_state(&checkout.root)
                .ok()
                .flatten(),
        };
        Place {
            checkout: checkout.clone(),
            state,
        }
    }
}

/// One demand for evidence, with its arguments encoded.
pub(crate) struct Request<'r> {
    pub(crate) checker: &'r Checker,
    pub(crate) case: &'r str,
    pub(crate) arguments: Json,
    /// The state of each repository the checker reads.
    pub(crate) reads: BTreeMap<String, Option<String>>,
    /// The checker's own repository, where it runs.
    pub(crate) place: &'r Place,
    /// Where each repository the checker reads has its files at the
    /// snapshot checked, which the checker is told but the key leaves out,
    /// so evidence holds on any machine.
    pub(crate) roots: &'r BTreeMap<String, String>,
}

impl Store {
    /// Evidence kept under `cache`; checkers run only if `may_run`.
    pub(crate) fn new(cache: &Path, may_run: bool) -> Self {
        Self {
            directory: cache.join("evidence"),
            cache: cache.to_path_buf(),
            may_run,
            environment: None,
        }
    }

    /// The store, with checkers run from `environment` rather than this
    /// process's own.
    pub(crate) fn with_environment(
        mut self,
        environment: Option<Arc<BTreeMap<String, String>>>,
    ) -> Self {
        self.environment = environment;
        self
    }

    /// Where a checkout's files are: the clone for a working directory, a
    /// worktree in the cache for a commit.
    pub(crate) fn files_of(&self, checkout: &Checkout) -> Result<PathBuf, String> {
        checkout
            .worktree(&self.cache)
            .map_err(|error| error.to_string())
    }

    /// The evidence for `request`: stored evidence with an equal key, or
    /// the checker's answer after running it.
    /// Stored evidence for `request`, if its key holds some; asks nothing
    /// of the request's roots, which only running needs.
    pub(crate) fn reusable(&self, request: &Request<'_>) -> Option<Evidence> {
        let key = evidence_key(request);
        let is_reusable = request.checker.is_cached && request.reads.values().all(Option::is_some);
        if !is_reusable {
            return None;
        }
        let stored = self.stored(&key)?;
        log::info!(
            "evidence: {} reused for {}",
            request.checker.address,
            request.case
        );
        Some(stored.into_evidence(&key, true))
    }

    pub(crate) fn demand(&self, request: &Request<'_>) -> Evidence {
        let key = evidence_key(request);
        let case = case_key(request);
        if let Some(stored) = self.reusable(request) {
            return stored;
        }
        let is_reusable = request.checker.is_cached && request.reads.values().all(Option::is_some);
        if !self.may_run {
            return self.not_run(&case, &key);
        }
        log::info!(
            "evidence: running {} for {}",
            request.checker.address,
            request.case
        );
        match self.run(request, &key) {
            Ok((decoded, output)) => {
                if is_reusable {
                    self.keep(request, &key, &case, &output);
                }
                decoded.into_evidence(&key, false)
            }
            Err(why) => Evidence::Unavailable(why),
        }
    }

    fn stored(&self, key: &str) -> Option<Decoded> {
        let text = std::fs::read_to_string(self.record(key)).ok()?;
        let record: Json = serde_json::from_str(&text).ok()?;
        decode_output(&record)
    }

    /// Why there is no evidence when checkers may not run: stale when the
    /// case last ran with other inputs, otherwise none was ever kept.
    fn not_run(&self, case: &str, key: &str) -> Evidence {
        let latest = std::fs::read_to_string(self.latest(case)).ok();
        match latest {
            Some(previous) if previous.trim() != key => Evidence::Stale(
                "its inputs changed since the evidence was kept, and checkers may not run"
                    .to_owned(),
            ),
            _ => Evidence::Unavailable(
                "checkers may not run, and no evidence is kept for these inputs".to_owned(),
            ),
        }
    }

    /// Runs the checker in its repository at the snapshot; its decoded
    /// output and the output as written.
    fn run(&self, request: &Request<'_>, key: &str) -> Result<(Decoded, Json), String> {
        let checker = request.checker;
        let root = request
            .place
            .checkout
            .worktree(&self.cache)
            .map_err(|error| error.to_string())?;
        let directory = match &checker.directory {
            Some(directory) => inside(&root, directory)?,
            None => root.clone(),
        };
        self.set_up(request, &directory)?;
        let run = self
            .directory
            .join("runs")
            .join(key.get(..16).unwrap_or(key));
        std::fs::create_dir_all(&run).map_err(|error| format!("{}: {error}", run.display()))?;
        let input = run.join("input.json");
        let output = run.join("output.json");
        let _ = std::fs::remove_file(&output);
        let given = json!({
            "checker": checker.address.to_string(),
            "case": request.case,
            "repository": checker.repository,
            "root": root.display().to_string(),
            "roots": request.roots,
            "arguments": request.arguments,
        });
        write_json(&input, &given)?;
        let mut env = checker.env.clone();
        env.insert("LEMMA_INPUT".to_owned(), input.display().to_string());
        env.insert("LEMMA_OUTPUT".to_owned(), output.display().to_string());
        env.insert("LEMMA_ROOT".to_owned(), root.display().to_string());
        env.insert("LEMMA_CASE".to_owned(), request.case.to_owned());
        let timeout_ms = checker.timeout_ms.unwrap_or(CHECKER_TIMEOUT_MS);
        let environment = repos::Environment {
            base: self.environment.as_deref(),
            added: &env,
        };
        repos::run_program(&directory, &checker.command, environment, Some(timeout_ms))
            .map_err(|error| format!("the checker failed: {}", describe(&error)))?;
        read_output(&output, checker)
    }

    /// Runs the checker's `setup` once per state of its repository's code.
    fn set_up(&self, request: &Request<'_>, directory: &Path) -> Result<(), String> {
        let Some(setup) = &request.checker.setup else {
            return Ok(());
        };
        let state = request.place.state.as_deref().unwrap_or("untracked");
        let marker = self.directory.join("setup").join(lemma_model::content_hash(
            format!("{}\n{state}", request.checker.text).as_bytes(),
        ));
        if marker.exists() {
            return Ok(());
        }
        log::info!("evidence: setting up {}", request.checker.address);
        repos::run_program(
            directory,
            setup,
            repos::Environment {
                base: self.environment.as_deref(),
                added: &request.checker.env,
            },
            Some(SETUP_TIMEOUT_MS),
        )
        .map_err(|error| format!("the checker's setup failed: {}", describe(&error)))?;
        if let Some(parent) = marker.parent() {
            std::fs::create_dir_all(parent).map_err(|error| error.to_string())?;
        }
        std::fs::write(&marker, state).map_err(|error| error.to_string())
    }

    /// Keeps a result and the key its case ran with. Evidence that cannot
    /// be kept is only run again, so failures to write are logged.
    fn keep(&self, request: &Request<'_>, key: &str, case: &str, output: &Json) {
        let mut record = json!({
            "key": key,
            "checker": request.checker.address.to_string(),
            "case": request.case,
            "arguments": request.arguments,
            "reads": request.reads,
        });
        if let (Some(record), Some(output)) = (record.as_object_mut(), output.as_object()) {
            for (name, value) in output {
                record.insert(name.clone(), value.clone());
            }
        }
        let kept = write_json(&self.record(key), &record).and_then(|()| {
            let latest = self.latest(case);
            if let Some(parent) = latest.parent() {
                std::fs::create_dir_all(parent).map_err(|error| error.to_string())?;
            }
            std::fs::write(&latest, key).map_err(|error| error.to_string())
        });
        if let Err(why) = kept {
            log::warn!("evidence for {} was not kept: {why}", request.case);
        }
    }

    fn record(&self, key: &str) -> PathBuf {
        self.directory.join(format!("{key}.json"))
    }

    fn latest(&self, case: &str) -> PathBuf {
        self.directory.join("cases").join(case)
    }
}

/// The key of a demand's evidence: everything its result depends on.
// TODO(release): the key holds the whole state of each repository in
// `reads`, so any commit makes every checker run again; key on the files a
// checker read (reported, or declared as paths) instead.
fn evidence_key(request: &Request<'_>) -> String {
    let checker = request.checker;
    let described = json!({
        "protocol": PROTOCOL,
        "checker": checker.address.to_string(),
        "declaration": checker.text,
        "arguments": request.arguments,
        "reads": request.reads,
        "env": checker.env,
    });
    lemma_model::content_hash(described.to_string().as_bytes())
}

/// The key of a case: a checker and its arguments, whatever the code.
fn case_key(request: &Request<'_>) -> String {
    let described = json!({
        "checker": request.checker.address.to_string(),
        "arguments": request.arguments,
    });
    lemma_model::content_hash(described.to_string().as_bytes())
}

/// What the checker wrote, decoded and checked against what it says it
/// observes.
fn read_output(output: &Path, checker: &Checker) -> Result<(Decoded, Json), String> {
    let text = std::fs::read_to_string(output)
        .map_err(|_| "the checker wrote nothing to $LEMMA_OUTPUT".to_owned())?;
    let written: Json = serde_json::from_str(&text)
        .map_err(|error| format!("the checker's output is not JSON: {error}"))?;
    let decoded = decode_output(&written).ok_or_else(|| {
        "the checker's output has neither `observed` nor `unavailable`".to_owned()
    })?;
    if let Decoded::Observed(value) = &decoded {
        if !value.fits(&checker.observes) {
            return Err(format!(
                "the checker observed {value}, which is not a `{}`",
                checker.observes
            ));
        }
    }
    Ok((decoded, written))
}

/// `directory` under `root`, refused if it leaves it.
fn inside(root: &Path, directory: &str) -> Result<PathBuf, String> {
    let relative = Path::new(directory);
    let leaves = relative
        .components()
        .any(|component| !matches!(component, Component::Normal(_) | Component::CurDir));
    if leaves {
        return Err(format!(
            "the checker's directory `{directory}` is not inside its repository"
        ));
    }
    Ok(root.join(relative))
}

fn write_json(path: &Path, value: &Json) -> Result<(), String> {
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent).map_err(|error| error.to_string())?;
    }
    let text = serde_json::to_string_pretty(value).map_err(|error| error.to_string())?;
    std::fs::write(path, text).map_err(|error| format!("{}: {error}", path.display()))
}

/// A command's failure, without the command line, which the report has.
fn describe(error: &crate::GraphError) -> String {
    match error {
        crate::GraphError::Resolver { message, .. } => message.clone(),
        other => other.to_string(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_directory_outside_the_repository_is_refused() {
        assert!(inside(Path::new("/repo"), "../elsewhere").is_err());
        assert!(inside(Path::new("/repo"), "/etc").is_err());
        assert_eq!(
            inside(Path::new("/repo"), "apps/worker").unwrap(),
            Path::new("/repo/apps/worker")
        );
    }
}
