//! Evidence: running checkers and keeping what they observed, keyed by
//! everything a result depends on, so it is reused while those inputs hold
//! (spec 13, section 10).
//!
//! A key covers the checker's address and declaration, the arguments, its
//! environment, and the code its result rests on. Where a run can be
//! watched, that code is what the run read: each file, listing and missing
//! path a recorder saw, with its state, so a change to anything else
//! leaves the evidence standing. Elsewhere it is the whole state of every
//! repository the checker reads: its Git tree, plus a digest of changed
//! files for a working directory, as adapters' documents are.
//!
//! Results are kept under `evidence/` in Lemma's cache, one JSON file per
//! key, with the key each case last ran with, so a run that may not start
//! checkers can tell stale evidence from none. `evidence/reads/` holds,
//! for each checker and arguments, the read sets of its latest runs: the
//! key of a run's evidence is not known until what it read is.

use std::collections::BTreeMap;
use std::path::{Component, Path, PathBuf};
use std::sync::{Arc, Mutex, OnceLock, PoisonError};
use std::time::SystemTime;

use lemma_lang::eval::Evidence;
use lemma_lang::model::{Checker, CheckerRunner};
use serde_json::{Value as Json, json};

use super::encode::{Decoded, decode_output};
use super::reads::{self, KeptReads, Parts, ReadSet, Roots};
use super::states::Current;
use crate::recorder::{self, Recorded, Recorder};
use crate::repos::{self, Checkout};

/// Bumped when what checkers are given, how their output is read, or
/// what a read set holds changes.
const PROTOCOL: &str = "3";

/// How many read sets are kept for one checker and arguments: enough to
/// come back to a state left a few changes ago, as after a branch switch.
const KEPT_READ_SETS: usize = 8;

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
    /// The state of working directories' code, which evidence is kept by.
    states: Arc<crate::adapters::States>,
    /// Held while a `setup` runs: demands running at once share one, which
    /// runs once and before any of them.
    setting_up: Mutex<()>,
    /// Digests of the proof verifier and solvers, read once per check.
    digests: crate::proof::Digests,
    /// What watches a checker run, looked for when the first one starts;
    /// `None` on a machine with nothing that can.
    recorder: OnceLock<Option<Box<dyn Recorder>>>,
    /// The state now of the paths kept evidence rests on.
    current: Current,
}

impl Drop for Store {
    /// Digests taken during the check are written down for the next one.
    fn drop(&mut self) {
        self.current.remember();
    }
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
    fn of(checkout: &Checkout, states: &crate::adapters::States) -> Place {
        let state = match &checkout.commit {
            Some(commit) => crate::adapters::commit_state(&checkout.root, commit).ok(),
            None => states.working_state(&checkout.root).ok().flatten(),
        };
        Place {
            checkout: checkout.clone(),
            state,
        }
    }
}

/// A demand recorded to run beside others, owning what a [`Request`]
/// borrows.
pub(crate) struct RecordedDemand {
    checker: Checker,
    case: String,
    arguments: Json,
    reads: BTreeMap<String, Option<String>>,
    place: Place,
    roots: BTreeMap<String, String>,
    is_at_work: bool,
    engine_key: Option<String>,
}

impl RecordedDemand {
    /// `request`, with a copy of what it borrows.
    pub(crate) fn of(request: &Request<'_>) -> Self {
        Self {
            checker: request.checker.clone(),
            case: request.case.to_owned(),
            arguments: request.arguments.clone(),
            reads: request.reads.clone(),
            place: request.place.clone(),
            roots: request.roots.clone(),
            is_at_work: request.is_at_work,
            engine_key: request.engine_key.clone(),
        }
    }

    fn as_request(&self) -> Request<'_> {
        Request {
            checker: &self.checker,
            case: &self.case,
            arguments: self.arguments.clone(),
            reads: self.reads.clone(),
            place: &self.place,
            roots: &self.roots,
            is_at_work: self.is_at_work,
            engine_key: self.engine_key.clone(),
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
    /// Whether every repository the checker reads is read at its working
    /// directory, with `roots` saying where: there, what a run reads can
    /// be recorded and compared with what the files hold now.
    pub(crate) is_at_work: bool,
    /// Native verifier and solver identity; absence disables proof reuse.
    pub(crate) engine_key: Option<String>,
}

impl Request<'_> {
    /// The key of this demand within one check: equal demands have equal
    /// keys. Evidence kept by whole repositories is kept under it.
    pub(crate) fn key(&self) -> String {
        evidence_key(self)
    }

    /// Whether the evidence can be kept by what a run reads: a command,
    /// run where its files can be watched.
    fn is_kept_by_reads(&self) -> bool {
        self.is_at_work
            && !self.roots.is_empty()
            && matches!(self.checker.runner, CheckerRunner::Command(_))
    }

    /// Where each repository the checker reads has its files.
    fn root_paths(&self) -> Roots {
        self.roots
            .iter()
            .map(|(name, root)| {
                let root = PathBuf::from(root);
                (name.clone(), root.canonicalize().unwrap_or(root))
            })
            .collect()
    }
}

impl Store {
    /// Evidence kept under `cache`; checkers run only if `may_run`.
    pub(crate) fn new(cache: &Path, may_run: bool) -> Self {
        Self {
            directory: cache.join("evidence"),
            cache: cache.to_path_buf(),
            may_run,
            environment: None,
            states: Arc::default(),
            setting_up: Mutex::new(()),
            digests: crate::proof::Digests::default(),
            recorder: OnceLock::new(),
            current: Current::remembering(cache.join("evidence").join("states")),
        }
    }

    /// Whether checkers may run.
    pub(crate) fn may_run(&self) -> bool {
        self.may_run
    }

    /// The evidence for each of `demands`, by its key, with those that
    /// have to run running at once, as many as the machine has processors.
    /// Equal demands run once.
    pub(crate) fn demand_at_once(
        &self,
        demands: Vec<RecordedDemand>,
    ) -> BTreeMap<String, Evidence> {
        let distinct: BTreeMap<String, RecordedDemand> = demands
            .into_iter()
            .map(|demand| (demand.as_request().key(), demand))
            .collect();
        let workers = std::thread::available_parallelism()
            .map_or(1, std::num::NonZero::get)
            .min(distinct.len());
        let waiting = Mutex::new(distinct.iter());
        let answers = Mutex::new(BTreeMap::new());
        let answer_waiting = || {
            let next = || {
                waiting
                    .lock()
                    .unwrap_or_else(PoisonError::into_inner)
                    .next()
            };
            while let Some((key, demand)) = next() {
                let evidence = self.demand(&demand.as_request());
                answers
                    .lock()
                    .unwrap_or_else(PoisonError::into_inner)
                    .insert(key.clone(), evidence);
            }
        };
        std::thread::scope(|scope| {
            for _ in 0..workers {
                scope.spawn(answer_waiting);
            }
        });
        answers.into_inner().unwrap_or_else(PoisonError::into_inner)
    }

    /// The store, asking `states` for the state of working directories'
    /// code, as the rest of a check does.
    pub(crate) fn with_states(mut self, states: Arc<crate::adapters::States>) -> Self {
        self.states = states;
        self
    }

    /// Where `checkout`'s code is read, and its state.
    pub(crate) fn place(&self, checkout: &Checkout) -> Place {
        Place::of(checkout, &self.states)
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

    /// Identity of the native proof toolchain, when it can be resolved.
    pub(crate) fn engine_key(&self, checker: &Checker) -> Option<String> {
        if checker.runner != CheckerRunner::Proof {
            return None;
        }
        crate::proof::identity(
            repos::Environment {
                base: self.environment.as_deref(),
                added: &checker.env,
            },
            &self.digests,
        )
    }

    /// Where a checkout's files are: the clone for a working directory, a
    /// worktree in the cache for a commit.
    pub(crate) fn files_of(&self, checkout: &Checkout) -> Result<PathBuf, String> {
        checkout
            .worktree(&self.cache)
            .map_err(|error| error.to_string())
    }

    /// Stored evidence for `request`, if some is kept that still holds;
    /// asks nothing of where a commit's files are, which only running
    /// needs.
    pub(crate) fn reusable(&self, request: &Request<'_>) -> Option<Evidence> {
        self.kept(request).ok()
    }

    /// Stored evidence for `request` that still holds; failing that, the
    /// first path its latest run read that has changed since, if the
    /// evidence was kept by what the run read.
    fn kept(&self, request: &Request<'_>) -> Result<Evidence, Option<String>> {
        if !request.checker.is_cached {
            return Err(None);
        }
        let (key, stored) = match self.kept_by_reads(request) {
            Ok(found) => found,
            Err(difference) => self.kept_by_state(request).ok_or(difference)?,
        };
        log::info!(
            "evidence: {} reused for {}",
            request.checker.address,
            request.case
        );
        Ok(stored.into_evidence(&key, true, None))
    }

    /// Evidence of a run whose every read still finds what it found;
    /// failing that, the first path the latest run read that differs. A
    /// read set found again that is not the latest becomes the latest:
    /// the oldest is dropped when another is kept, and one that keeps
    /// being come back to, such as the state before an edit, must not be.
    fn kept_by_reads(&self, request: &Request<'_>) -> Result<(String, Decoded), Option<String>> {
        if !request.is_kept_by_reads() {
            return Err(None);
        }
        let roots = request.root_paths();
        let part = |digest: &str| std::fs::read_to_string(self.part(digest)).ok();
        let invariant = invariant_key(request);
        let mut read_sets = self.read_sets(&invariant);
        let mut latest_difference = None;
        let found = read_sets.iter().position(|(key, reads)| {
            let difference = reads.difference(&roots, &self.current, &part);
            let holds = difference.is_none() && self.record(key).exists();
            latest_difference.get_or_insert(difference);
            holds
        });
        let Some(found) = found else {
            return Err(latest_difference.flatten());
        };
        let stored = self.stored(&read_sets[found].0).ok_or(None)?;
        if found > 0 {
            read_sets[..=found].rotate_right(1);
            if let Err(why) = self.write_read_sets(&invariant, &read_sets) {
                log::warn!("what {} read was not kept: {why}", request.case);
            }
        }
        Ok((read_sets[0].0.clone(), stored))
    }

    /// Evidence kept for the state every repository read is in.
    fn kept_by_state(&self, request: &Request<'_>) -> Option<(String, Decoded)> {
        if !is_kept_by_state(request) {
            return None;
        }
        let key = evidence_key(request);
        let stored = self.stored(&key)?;
        Some((key, stored))
    }

    /// The evidence for `request`: kept evidence that still holds, or the
    /// checker's answer after running it.
    pub(crate) fn demand(&self, request: &Request<'_>) -> Evidence {
        let key = evidence_key(request);
        let case = case_key(request);
        let difference = match self.kept(request) {
            Ok(stored) => return stored,
            Err(difference) => difference,
        };
        if !self.may_run {
            return self.not_run(request, &case, &key, difference.as_deref());
        }
        match &difference {
            Some(difference) => log::info!(
                "evidence: running {} for {} again: {difference}",
                request.checker.address,
                request.case
            ),
            None => log::info!(
                "evidence: running {} for {}",
                request.checker.address,
                request.case
            ),
        }
        let recorder = self.recorder_for(request);
        let started = SystemTime::now();
        let ran = self.run(request, &key, recorder);
        // Whatever the run wrote, kept evidence is compared with afresh.
        self.current.forget();
        let (decoded, output, seen) = match ran {
            Ok(ran) => ran,
            Err(why) => return Evidence::Unavailable(why),
        };
        let Some(seen) = seen else {
            let not_kept = self.keep_by_state(request, &key, &case, &output);
            return decoded.into_evidence(&key, false, not_kept);
        };
        let roots = request.root_paths();
        let seen = reads::inside_repositories(&seen, &roots);
        let reads = match changed_input(&seen, &roots) {
            Some(path) => Err(format!("the checker changed {path}, which it read")),
            None => ReadSet::of(&seen, &roots, started, &self.current),
        };
        match reads {
            Ok(reads) => {
                let (kept, parts) = reads.kept();
                let key = reads_key(request, &kept);
                log::info!(
                    "evidence: {} is kept by the {} paths its run read",
                    request.case,
                    reads.len()
                );
                self.keep(request, &key, &case, &output);
                self.keep_read_set(request, &key, &kept, &parts);
                decoded.into_evidence(&key, false, None)
            }
            Err(why) => {
                log::info!("evidence for {} is not kept: {why}", request.case);
                decoded.into_evidence(&key, false, Some(why))
            }
        }
    }

    /// Keeps evidence by the state of the repositories its checker reads,
    /// or says why it cannot be kept.
    fn keep_by_state(
        &self,
        request: &Request<'_>,
        key: &str,
        case: &str,
        output: &Json,
    ) -> Option<String> {
        if is_kept_by_state(request) {
            self.keep(request, key, case, output);
            return None;
        }
        let why = why_not_kept_by_state(request);
        if let Some(why) = &why {
            log::info!("evidence for {} is not kept: {why}", request.case);
        }
        why
    }

    /// What watches this request's run, when its evidence is to be kept
    /// by what the run reads.
    fn recorder_for(&self, request: &Request<'_>) -> Option<&dyn Recorder> {
        if !request.checker.is_cached || !request.is_kept_by_reads() {
            return None;
        }
        let found = self.recorder.get_or_init(|| {
            let none = BTreeMap::new();
            let found = recorder::find(repos::Environment {
                base: self.environment.as_deref(),
                added: &none,
            });
            match &found {
                Some(recorder) => log::info!("evidence: runs are watched by {}", recorder.name()),
                None => {
                    log::info!("evidence: no run can be watched here; kept by whole repositories");
                }
            }
            found
        });
        found.as_deref()
    }

    /// The read sets kept for a checker and arguments, latest first, each
    /// with the key of its evidence.
    fn read_sets(&self, invariant: &str) -> Vec<(String, KeptReads)> {
        let read = || {
            let text = std::fs::read_to_string(self.read_sets_of(invariant)).ok()?;
            let kept: Json = serde_json::from_str(&text).ok()?;
            let entries = kept["entries"].as_array()?;
            let sets = entries.iter().filter_map(|entry| {
                let reads = KeptReads::from_json(&entry["reads"])?;
                Some((entry["evidence"].as_str()?.to_owned(), reads))
            });
            Some(sets.collect())
        };
        read().unwrap_or_default()
    }

    fn stored(&self, key: &str) -> Option<Decoded> {
        let text = std::fs::read_to_string(self.record(key)).ok()?;
        let record: Json = serde_json::from_str(&text).ok()?;
        decode_output(&record)
    }

    /// Why there is no evidence when checkers may not run: stale when the
    /// case last ran with other inputs, naming the `difference` if one is
    /// known, otherwise none was ever kept.
    fn not_run(
        &self,
        request: &Request<'_>,
        case: &str,
        key: &str,
        difference: Option<&str>,
    ) -> Evidence {
        let latest = std::fs::read_to_string(self.latest(case)).ok();
        // Evidence kept by what a run read has no key until it runs; had
        // the last run's still held, it would have been reused.
        let is_other = |previous: &str| request.is_kept_by_reads() || previous.trim() != key;
        let changed = difference.unwrap_or("its inputs changed");
        match latest {
            Some(previous) if is_other(&previous) => Evidence::Stale(format!(
                "{changed} since the evidence was kept, and checkers may not run"
            )),
            _ => Evidence::Unavailable(
                "checkers may not run, and no evidence is kept for these inputs".to_owned(),
            ),
        }
    }

    /// Runs the checker in its repository at the snapshot, watched by
    /// `recorder` if there is one; its decoded output, the output as
    /// written, and what the run was seen to do.
    fn run(
        &self,
        request: &Request<'_>,
        key: &str,
        recorder: Option<&dyn Recorder>,
    ) -> Result<(Decoded, Json, Option<Recorded>), String> {
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
            "engine_key": request.engine_key,
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
        let mut seen = None;
        match &checker.runner {
            CheckerRunner::Command(command) => {
                let watched = Watched::start(recorder, command, &run)?;
                let ran = repos::run_program(
                    &directory,
                    watched.command(),
                    environment,
                    Some(timeout_ms),
                );
                seen = watched.finish(&directory, ran.is_ok());
                ran.map_err(|error| format!("the checker failed: {}", describe(&error)))?;
            }
            CheckerRunner::Proof => {
                let result =
                    crate::proof::run(&given, &run, environment, &self.digests, timeout_ms);
                write_json(&output, &result)?;
            }
        }
        let (decoded, written) = read_output(&output, checker)?;
        Ok((decoded, written, seen))
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
        let _one_at_a_time = self
            .setting_up
            .lock()
            .unwrap_or_else(PoisonError::into_inner);
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

    /// Puts a run's read set first among those kept for its checker and
    /// arguments, dropping the oldest past [`KEPT_READ_SETS`], with the
    /// parts not kept yet. Without it the evidence is only run again, so a
    /// failure to write is logged.
    fn keep_read_set(&self, request: &Request<'_>, key: &str, reads: &KeptReads, parts: &Parts) {
        let invariant = invariant_key(request);
        let mut read_sets = vec![(key.to_owned(), reads.clone())];
        let earlier = self.read_sets(&invariant);
        let others = earlier.into_iter().filter(|(other, _)| other != key);
        read_sets.extend(others.take(KEPT_READ_SETS - 1));
        let mut new_parts = parts
            .iter()
            .filter(|(digest, _)| !self.part(digest).exists());
        let kept = new_parts
            .try_for_each(|(digest, text)| write_whole(&self.part(digest), text))
            .and_then(|()| self.write_read_sets(&invariant, &read_sets));
        if let Err(why) = kept {
            log::warn!("what {} read was not kept: {why}", request.case);
        }
    }

    /// Keeps the read sets of a checker and arguments, latest first.
    fn write_read_sets(
        &self,
        invariant: &str,
        read_sets: &[(String, KeptReads)],
    ) -> Result<(), String> {
        let entries: Vec<Json> = read_sets
            .iter()
            .map(|(key, reads)| json!({ "evidence": key, "reads": reads.to_json() }))
            .collect();
        let text = json!({ "entries": entries }).to_string();
        write_whole(&self.read_sets_of(invariant), &text)
    }

    /// Where the part of a read set named `digest` is kept.
    fn part(&self, digest: &str) -> PathBuf {
        self.directory.join("parts").join(digest)
    }

    fn read_sets_of(&self, invariant: &str) -> PathBuf {
        self.directory
            .join("reads")
            .join(format!("{invariant}.json"))
    }

    fn record(&self, key: &str) -> PathBuf {
        self.directory.join(format!("{key}.json"))
    }

    fn latest(&self, case: &str) -> PathBuf {
        self.directory.join("cases").join(case)
    }
}

/// A command to run, watched if there is a recorder.
struct Watched<'r> {
    recorder: Option<&'r dyn Recorder>,
    command: Vec<String>,
    scratch: PathBuf,
}

impl<'r> Watched<'r> {
    /// `command`, to run under `recorder` with what it sees left in a
    /// fresh directory of the run's.
    fn start(
        recorder: Option<&'r dyn Recorder>,
        command: &[String],
        run: &Path,
    ) -> Result<Self, String> {
        let scratch = run.join("watched");
        let Some(recorder) = recorder else {
            return Ok(Self {
                recorder: None,
                command: command.to_vec(),
                scratch,
            });
        };
        let _ = std::fs::remove_dir_all(&scratch);
        std::fs::create_dir_all(&scratch)
            .map_err(|error| format!("{}: {error}", scratch.display()))?;
        Ok(Self {
            recorder: Some(recorder),
            command: recorder.watching(command, &scratch),
            scratch,
        })
    }

    fn command(&self) -> &[String] {
        &self.command
    }

    /// What the run, started in `directory` and now ended, was seen to do,
    /// if it was watched and `succeeded`. A record that cannot be read
    /// leaves the checker's answer standing: its evidence is then kept by
    /// whole repositories, as with no recorder.
    fn finish(self, directory: &Path, succeeded: bool) -> Option<Recorded> {
        let recorder = self.recorder?;
        let seen = succeeded.then(|| recorder.recorded(&self.scratch, directory));
        let _ = std::fs::remove_dir_all(&self.scratch);
        match seen? {
            Ok(seen) => Some(seen),
            Err(why) => {
                log::warn!("what the checker read is not known: {why}");
                None
            }
        }
    }
}

/// A file the run read and then wrote that its repository keeps in Git:
/// the run changed what it rests on, so no state of that file is the one
/// its result holds for. What Git does not keep, such as a cache, is the
/// run's product.
fn changed_input(seen: &Recorded, roots: &Roots) -> Option<String> {
    reads::rewritten(seen, roots)
        .into_iter()
        .find_map(|(root, paths)| {
            let mut arguments = vec!["ls-files", "--"];
            arguments.extend(paths.iter().map(String::as_str));
            // Outside Git nothing is kept, and everything is a product.
            let tracked = repos::git(&root, &arguments).ok()?;
            tracked.lines().next().map(str::to_owned)
        })
}

/// Whether evidence may be kept for the state of the repositories read:
/// each has one to name, and a proof's engine is known.
fn is_kept_by_state(request: &Request<'_>) -> bool {
    request.checker.is_cached
        && request.reads.values().all(Option::is_some)
        && (request.checker.runner != CheckerRunner::Proof || request.engine_key.is_some())
}

/// Why evidence kept by the state of the repositories a checker reads
/// cannot be kept, said so the report can tell a reader why the checker
/// runs again every time. `None` for a checker that asked not to be kept
/// (`cache: false`): that is what it asked for.
fn why_not_kept_by_state(request: &Request<'_>) -> Option<String> {
    if !request.checker.is_cached {
        return None;
    }
    let outside: Vec<&str> = request
        .reads
        .iter()
        .filter(|(_, state)| state.is_none())
        .map(|(name, _)| name.as_str())
        .collect();
    if !outside.is_empty() {
        return Some(format!(
            "{} is not a Git repository, so its code has no state to keep the evidence by",
            outside
                .iter()
                .map(|name| format!("`{name}`"))
                .collect::<Vec<_>>()
                .join(", ")
        ));
    }
    Some("the proof's solver could not be named, so its answer has no key".to_owned())
}

/// What a checker's evidence depends on apart from code: the checker as
/// declared, the arguments and its environment.
fn invariant_key(request: &Request<'_>) -> String {
    let checker = request.checker;
    let described = json!({
        "protocol": PROTOCOL,
        "checker": checker.address.to_string(),
        "declaration": checker.text,
        "arguments": request.arguments,
        "env": checker.env,
    });
    lemma_model::content_hash(described.to_string().as_bytes())
}

/// Writes a file whole beside its place and moves it there, so a check
/// reading it at the same moment sees the old text or the new.
fn write_whole(path: &Path, text: &str) -> Result<(), String> {
    let failed = |error: std::io::Error| format!("{}: {error}", path.display());
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent).map_err(failed)?;
    }
    // Named by process and thread: cases run at once may keep one part.
    let writer = format!(
        "{}.{:?}.tmp",
        std::process::id(),
        std::thread::current().id()
    );
    let beside = path.with_extension(writer);
    std::fs::write(&beside, text).map_err(failed)?;
    std::fs::rename(&beside, path).map_err(failed)
}

/// The key of evidence kept by what its run read.
fn reads_key(request: &Request<'_>, reads: &KeptReads) -> String {
    let described = json!({ "invariant": invariant_key(request), "reads": reads.to_json() });
    lemma_model::content_hash(described.to_string().as_bytes())
}

/// The key of a demand's evidence kept by whole repositories: everything
/// its result depends on, with the state of each repository read.
fn evidence_key(request: &Request<'_>) -> String {
    let checker = request.checker;
    let mut described = json!({
        "protocol": PROTOCOL,
        "checker": checker.address.to_string(),
        "declaration": checker.text,
        "arguments": request.arguments,
        "reads": request.reads,
        "env": checker.env,
    });
    if checker.runner == CheckerRunner::Proof {
        described["engine"] = json!(request.engine_key);
    }
    lemma_model::content_hash(described.to_string().as_bytes())
}

/// The key of a case: a checker and its arguments, whatever the code.
fn case_key(request: &Request<'_>) -> String {
    let mut arguments = request.arguments.clone();
    if request.checker.runner == CheckerRunner::Proof {
        if let Some(implementation) = arguments["handler"]["implementation"].as_object_mut() {
            implementation.remove("source_hash");
        }
    }
    let described = json!({
        "checker": request.checker.address.to_string(),
        "arguments": arguments,
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
