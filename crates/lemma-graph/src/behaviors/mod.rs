//! Behaviors in `lemma check`: the entry module's `.lm` program is checked
//! at the head, the behaviors its `check main` selects run (all of them
//! without one), and checkers run in their repositories for the evidence
//! they demand (spec 13, sections 9 to 11).
//!
//! With a change, the base program runs too, but only on evidence already
//! kept: a violated case counts unless the base shows the same violation.
//! The requirements and behaviors added, removed or changed between the
//! two are listed, so a deleted one shows up instead of disappearing.

mod encode;
mod evidence;

use std::collections::BTreeMap;
use std::fmt::Write as _;
use std::path::Path;

use lemma_lang::Checked;
use lemma_lang::eval::{self, BehaviorResult, Demand, Facts, Outcome, StatedFacts, World};
use lemma_lang::model::{Check, Model, Predicate, Run};
use lemma_lang::syntax::ast::Snapshot;
use lemma_lang::types::Address;

use self::encode::Encoder;
use self::evidence::{Place, Request, Store};
use crate::GraphError;
use crate::check::{BASE, CheckOptions, Finding, HEAD, Repo, SELF, Severity};
use crate::config::Level;
use crate::lm::CheckoutSources;
use crate::repos::Checkout;

/// The standard library's HTTP contracts, whose requirements `lemma check`
/// always loads, as it ran the built-in grammar before modules.
const HTTP: &str = "lemma.http";

/// A behavior's result, as a report lists it.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize)]
pub struct BehaviorReport {
    /// The behavior, as `module::name`.
    pub behavior: String,
    /// The behavior with its arguments, for a parameterised one.
    pub identity: String,
    /// `error`, `warning` or `info`.
    pub severity: &'static str,
    /// `satisfied`, `violated`, `unknown` or `stale`.
    pub outcome: &'static str,
    /// Every case, the root first.
    pub cases: Vec<CaseReport>,
    /// Why the outcome is worse than its cases'.
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub notes: Vec<String>,
    /// The evidence its cases used.
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub evidence: Vec<EvidenceReport>,
}

/// One case of a behavior.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize)]
pub struct CaseReport {
    /// The behavior's identity and its loop values.
    pub identity: String,
    /// How it ended.
    pub outcome: &'static str,
    /// What ended it, when it did not hold.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub problem: Option<ProblemReport>,
    /// Whether the change brought it about; always true without a change.
    pub introduced: bool,
    /// Whether the change made it hold: it ended violated at the base and
    /// holds at the head. Known only when the base ran its checkers or
    /// kept the evidence it needs.
    pub fixed: bool,
    /// The reason of the allowance covering it, if one does.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub allowed: Option<String>,
}

/// Why a case did not hold.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize)]
pub struct ProblemReport {
    /// The repository of the module with the statement.
    pub repository: String,
    /// The module's file, relative to its repository.
    pub file: String,
    /// The statement's line, from 1.
    pub line: usize,
    /// Its column, from 1.
    pub column: usize,
    /// The expression as written.
    pub expression: String,
    /// What happened.
    pub message: String,
    /// Each operand of a failed comparison, and its value.
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub operands: Vec<(String, String)>,
    /// The `because` text.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub because: Option<String>,
}

/// Evidence a case used.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize)]
pub struct EvidenceReport {
    /// The checker, as `module::name`.
    pub checker: String,
    /// The case that demanded it.
    pub case: String,
    /// Its key, when there was evidence.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub key: Option<String>,
    /// Whether kept evidence was reused instead of running the checker.
    pub reused: bool,
    /// Why there was none.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub unavailable: Option<String>,
}

/// A requirement or behavior a change added, removed or changed.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, serde::Serialize)]
pub struct RequirementChange {
    /// It, as `module::name`.
    pub requirement: String,
    /// `added`, `removed` or `changed`.
    pub change: &'static str,
}

/// Behaviors' results at the head, and what the change did to them.
pub(crate) struct Evaluated {
    /// The head's checked program, for allowances and locations.
    pub(crate) head: Checked,
    /// The behaviors' results.
    pub(crate) results: Vec<BehaviorResult>,
    /// The same results, as reported.
    pub(crate) reports: Vec<BehaviorReport>,
    /// The requirements and behaviors the change added, removed or changed.
    pub(crate) changes: Vec<RequirementChange>,
}

/// The behaviors of the program whose entry module is `manifest` in the
/// parent repository; `None` when the parent's manifest is not a module.
pub(crate) fn run(
    manifest: &str,
    repos: &BTreeMap<String, Repo>,
    options: &CheckOptions,
) -> Result<Option<Evaluated>, GraphError> {
    let (cache, has_change) = (options.cache.as_path(), options.change.is_some());
    let Some(manifest) = parent_module(manifest, repos)? else {
        return Ok(None);
    };
    let head = Program::load(repos, &manifest, HEAD, cache)?;
    // A base that does not load (one from before the repository had
    // modules, or with errors since fixed) only means nothing is compared.
    let base = has_change
        .then(|| Program::load(repos, &manifest, BASE, cache))
        .transpose()
        .unwrap_or_else(|error| {
            log::warn!(
                "behaviors: the base program does not load, so nothing is compared with it: {error}"
            );
            None
        });
    let changes = base
        .as_ref()
        .map(|base| requirement_changes(&base.checked.model, &head.checked.model))
        .unwrap_or_default();
    if head.checked.model.behaviors.is_empty() {
        return Ok(Some(Evaluated {
            head: head.checked,
            results: Vec::new(),
            reports: Vec::new(),
            changes,
        }));
    }
    let base_model = base.as_ref().map(|base| &base.checked.model);
    let selected = options.select.as_deref();
    let results = head.run(
        &Store::new(cache, options.run_checkers),
        base_model,
        selected,
    )?;
    // The base may lack the check the head selects; it then runs them all.
    let base_store = Store::new(cache, options.run_checkers && options.runs_base_checkers);
    let base_results = base.as_ref().map(|base| {
        base.run(&base_store, None, selected)
            .or_else(|_| base.run(&base_store, None, None))
            .unwrap_or_default()
    });
    let reports = results
        .iter()
        .map(|result| report(result, base_results.as_deref(), has_change, &head.checked))
        .collect();
    Ok(Some(Evaluated {
        head: head.checked,
        results,
        reports,
        changes,
    }))
}

/// The grammar the parent's `.lm` program lowers to: the relations, rules
/// and requirements of every module it loads, `lemma.http` always among
/// them, as its `check main` selects them. `None` when the parent's
/// manifest is not a module, and the built-in grammar runs.
// TODO(release): lemma check checks the parent's program up to four times:
// lowering each repository at both snapshots, here, and for behaviors at
// head and base. Check it once per snapshot and share it.
pub(crate) fn program_grammar(
    root_file: &Path,
    repos: &BTreeMap<String, Repo>,
    options: &CheckOptions,
) -> Result<Option<lemma_grammar::GrammarFile>, GraphError> {
    let cache = options.cache.as_path();
    let manifest = root_file.file_name().map_or_else(
        || crate::config::LEMMA_FILE.to_owned(),
        |name| name.to_string_lossy().into_owned(),
    );
    let Some(manifest) = parent_module(&manifest, repos)? else {
        return Ok(None);
    };
    let program = Program::load(repos, &manifest, HEAD, cache)?;
    let checked = &program.checked;
    let main = selected_check(checked, options.select.as_deref())?;
    let runs = main.as_ref().map(|check| check.runs.as_slice());
    lemma_grammar::lm::grammar_file(&checked.model, &checked.loaded.entry, runs)
        .map(Some)
        .map_err(|source| GraphError::Grammar {
            path: repos[SELF].checkouts[HEAD].root.join(&manifest),
            source,
        })
}

/// The parent's entry module, if its manifest is one.
fn parent_module(
    manifest: &str,
    repos: &BTreeMap<String, Repo>,
) -> Result<Option<String>, GraphError> {
    let Some(parent) = repos.get(SELF) else {
        return Ok(None);
    };
    let manifest = crate::check::language_manifest(&parent.checkouts[HEAD], manifest)?;
    let is_module = Path::new(&manifest)
        .extension()
        .is_some_and(|extension| extension.eq_ignore_ascii_case("lm"));
    Ok(is_module.then_some(manifest))
}

/// A program checked at one snapshot, with where its repositories are.
struct Program {
    checked: Checked,
    /// Where each repository of the program is read.
    checkouts: BTreeMap<String, Checkout>,
}

impl Program {
    /// The program at `snapshot`: the parent's checkout and every other
    /// repository where the composition reads it.
    fn load(
        repos: &BTreeMap<String, Repo>,
        manifest: &str,
        snapshot: usize,
        cache: &Path,
    ) -> Result<Program, GraphError> {
        let pins: BTreeMap<String, Checkout> = repos
            .iter()
            .filter_map(|(name, repo)| {
                let pinned = name.strip_prefix('#')?;
                Some((
                    crate::lm::repository_name(pinned),
                    repo.checkouts[snapshot].clone(),
                ))
            })
            .collect();
        let parent = &repos[SELF].checkouts[snapshot];
        let mut sources = CheckoutSources::new(parent.clone(), cache).with_pins(pins.clone());
        let checked = lemma_lang::check_program_including(&mut sources, manifest, &[HTTP]);
        let errors = checked.error_lines();
        if !errors.is_empty() {
            return Err(GraphError::Language {
                path: parent.root.join(manifest),
                errors,
            });
        }
        let entry = entry_repository(&checked);
        let roots = sources.roots(&entry);
        let checkouts = roots
            .into_iter()
            .map(|(name, root)| {
                let checkout = if name == entry {
                    parent.clone()
                } else {
                    pins.get(&crate::lm::repository_name(&name))
                        .cloned()
                        .unwrap_or(Checkout { root, commit: None })
                };
                (name, checkout)
            })
            .collect();
        Ok(Program { checked, checkouts })
    }

    /// Runs the behaviors the selected check (or `check main`) selects;
    /// `@base` reads `other`'s stated facts.
    fn run(
        &self,
        store: &Store,
        other: Option<&Model>,
        selected: Option<&str>,
    ) -> Result<Vec<BehaviorResult>, GraphError> {
        let model = &self.checked.model;
        let repositories = self
            .checked
            .loaded
            .modules
            .values()
            .map(|module| (module.name.clone(), module.repository.clone()))
            .collect();
        let mut world = CheckWorld {
            facts: StatedFacts::new(model, other),
            encoder: Encoder::new(model, repositories),
            store,
            checkouts: &self.checkouts,
            places: BTreeMap::new(),
        };
        let main = selected_check(&self.checked, selected)?;
        Ok(eval::run_check(model, main.as_ref(), &mut world))
    }
}

/// Where behaviors get facts and evidence in `lemma check`.
struct CheckWorld<'w> {
    facts: StatedFacts<'w>,
    encoder: Encoder<'w>,
    store: &'w Store,
    checkouts: &'w BTreeMap<String, Checkout>,
    /// The state of each repository's code, found when a checker first
    /// needs it: a working directory's costs a `git status` and a digest
    /// of each changed file, which a check without checkers never pays.
    places: BTreeMap<String, Place>,
}

impl CheckWorld<'_> {
    /// Where each repository a checker reads, and its own, has its files at
    /// the snapshot checked: a worktree for one read at a commit, so the
    /// checker sees the code it is checking, not the clone's working tree.
    fn roots(&mut self, read: &[String], own: &str) -> Result<BTreeMap<String, String>, String> {
        let mut roots = BTreeMap::new();
        for name in read.iter().map(String::as_str).chain([own]) {
            let Some(checkout) = self.checkouts.get(name) else {
                return Err(format!(
                    "repository `{name}`, which the checker reads, is not loaded"
                ));
            };
            let files = self.store.files_of(checkout)?;
            roots.insert(name.to_owned(), files.display().to_string());
        }
        Ok(roots)
    }

    /// Where repository `name` is read, and the state of its code.
    fn place(&mut self, name: &str) -> Option<Place> {
        if let Some(found) = self.places.get(name) {
            return Some(found.clone());
        }
        let place = Place::of(self.checkouts.get(name)?);
        self.places.insert(name.to_owned(), place.clone());
        Some(place)
    }
}

impl World for CheckWorld<'_> {
    fn facts(&mut self, predicate: &Predicate, snapshot: Snapshot) -> Facts {
        self.facts.facts(predicate, snapshot)
    }

    // TODO(release): a checker declared in any loaded module runs, including
    // modules of repositories the workspace only pins, so a pinned
    // repository can add a command that runs on the machine checking it.
    // Decide whether checkers outside the entry repository need the
    // workspace's consent; until then, `--no-run` is the guard.
    fn evidence(&mut self, demand: &Demand<'_>) -> eval::Evidence {
        let checker = demand.checker;
        let Some(place) = self.place(&checker.repository) else {
            return eval::Evidence::Unavailable(format!(
                "repository `{}`, where the checker runs, is not checked out",
                checker.repository
            ));
        };
        let arguments: serde_json::Map<String, serde_json::Value> = demand
            .arguments
            .iter()
            .map(|(name, value)| (name.clone(), self.encoder.encode(value)))
            .collect();
        let read: Vec<String> = if checker.reads.is_empty() {
            vec![checker.repository.clone()]
        } else {
            checker.reads.clone()
        };
        let reads = read
            .iter()
            .map(|name| {
                let state = self.place(name).and_then(|place| place.state);
                (name.clone(), state)
            })
            .collect();
        let roots = match self.roots(&read, &checker.repository) {
            Ok(roots) => roots,
            Err(why) => return eval::Evidence::Unavailable(why),
        };
        self.store.demand(&Request {
            checker,
            case: demand.case,
            arguments: serde_json::Value::Object(arguments),
            reads,
            place: &place,
            roots: &roots,
        })
    }
}

/// The entry module's check named `selected`, or else a check running just
/// the behavior or requirement it names; without `selected`, its `check
/// main` if it has one.
fn selected_check(checked: &Checked, selected: Option<&str>) -> Result<Option<Check>, GraphError> {
    let name = selected.unwrap_or("main");
    let found =
        checked.model.checks.iter().find(|check| {
            check.address.module == checked.loaded.entry && check.address.name() == name
        });
    if let Some(found) = found {
        return Ok(Some(found.clone()));
    }
    let Some(selected) = selected else {
        return Ok(None);
    };
    let address = named_runnable(&checked.model, selected).map_err(|candidates| {
        if candidates.is_empty() {
            return GraphError::UnknownCheck {
                module: checked.loaded.entry.clone(),
                name: selected.to_owned(),
            };
        }
        GraphError::AmbiguousSelection {
            name: selected.to_owned(),
            candidates,
        }
    })?;
    Ok(Some(Check {
        address: Address::top(&checked.loaded.entry, selected),
        runs: vec![Run {
            requirement: address,
            severity: None,
            arguments: Vec::new(),
        }],
    }))
}

/// The behavior (one taking no arguments) or requirement `selected` names:
/// by its address (`checks::retries`, `checks.retries`), or by its name
/// alone when one thing has it. Otherwise the addresses it could mean,
/// empty when none.
fn named_runnable(model: &Model, selected: &str) -> Result<Address, Vec<String>> {
    let behaviors = model
        .behaviors
        .iter()
        .filter(|behavior| behavior.parameters.is_empty())
        .map(|behavior| &behavior.address);
    let requirements = model
        .requirements
        .iter()
        .map(|requirement| &requirement.address);
    let runnable: Vec<&Address> = behaviors.chain(requirements).collect();
    let written = |address: &Address| {
        address.to_string() == selected
            || format!("{}.{}", address.module, address.dotted_path()) == selected
    };
    if let Some(address) = runnable.iter().find(|address| written(address)) {
        return Ok((*address).clone());
    }
    let named: Vec<&&Address> = runnable
        .iter()
        .filter(|address| address.dotted_path() == selected)
        .collect();
    match named[..] {
        [address] => Ok((*address).clone()),
        _ => Err(named.iter().map(ToString::to_string).collect()),
    }
}

fn entry_repository(checked: &Checked) -> String {
    checked
        .loaded
        .modules
        .get(&checked.loaded.entry)
        .map(|module| module.repository.clone())
        .unwrap_or_default()
}

/// A result as reported; with a base, a case is introduced unless the base
/// ended the same case the same way.
fn report(
    result: &BehaviorResult,
    base: Option<&[BehaviorResult]>,
    has_change: bool,
    checked: &Checked,
) -> BehaviorReport {
    let base_cases: BTreeMap<&str, &eval::CaseResult> = base
        .into_iter()
        .flatten()
        .filter(|other| other.identity == result.identity)
        .flat_map(|other| &other.cases)
        .map(|case| (case.identity.as_str(), case))
        .collect();
    let cases = result
        .cases
        .iter()
        .map(|case| {
            let before = base_cases.get(case.identity.as_str());
            let fixed = has_change
                && case.outcome == Outcome::Satisfied
                && before.is_some_and(|before| before.outcome == Outcome::Violated);
            let was_there = before.is_some_and(|before| {
                before.outcome == case.outcome
                    && before.problem.as_ref().map(|problem| &problem.message)
                        == case.problem.as_ref().map(|problem| &problem.message)
            });
            CaseReport {
                identity: case.identity.clone(),
                outcome: case.outcome.as_str(),
                problem: case
                    .problem
                    .as_ref()
                    .map(|problem| problem_report(problem, checked)),
                introduced: !has_change || (case.outcome != Outcome::Satisfied && !was_there),
                fixed,
                allowed: None,
            }
        })
        .collect();
    BehaviorReport {
        behavior: result.address.to_string(),
        identity: result.identity.clone(),
        severity: severity_name(result.severity),
        outcome: result.outcome.as_str(),
        cases,
        notes: result.notes.clone(),
        evidence: result
            .evidence
            .iter()
            .map(|used| EvidenceReport {
                checker: used.checker.to_string(),
                case: used.case.clone(),
                key: used.key.clone(),
                reused: used.was_reused,
                unavailable: used.unavailable.clone(),
            })
            .collect(),
    }
}

fn problem_report(problem: &eval::Problem, checked: &Checked) -> ProblemReport {
    let module = checked.loaded.modules.get(&problem.module);
    let position = module.map(|module| module.lines.position(problem.span.start));
    ProblemReport {
        repository: module
            .map(|module| module.repository.clone())
            .unwrap_or_default(),
        file: module.map(|module| module.path.clone()).unwrap_or_default(),
        line: position.map_or(0, |position| position.line as usize + 1),
        column: position.map_or(0, |position| position.column as usize + 1),
        expression: problem.text.clone(),
        message: problem.message.clone(),
        operands: problem.operands.clone(),
        because: problem.because.clone(),
    }
}

fn severity_name(severity: lemma_lang::model::Severity) -> &'static str {
    match severity {
        lemma_lang::model::Severity::Error => "error",
        lemma_lang::model::Severity::Warning => "warning",
        lemma_lang::model::Severity::Info => "info",
    }
}

/// The requirements and behaviors in one model and not the other, or in
/// both but different, by address.
fn requirement_changes(base: &Model, head: &Model) -> Vec<RequirementChange> {
    let declared = |model: &Model| -> BTreeMap<String, String> {
        let requirements = model
            .requirements
            .iter()
            .map(|requirement| (requirement.address.to_string(), format!("{requirement:?}")));
        let behaviors = model
            .behaviors
            .iter()
            .map(|behavior| (behavior.address.to_string(), behavior.text.clone()));
        requirements.chain(behaviors).collect()
    };
    let (before, after) = (declared(base), declared(head));
    let mut changes = Vec::new();
    for (address, body) in &after {
        let change = match before.get(address) {
            None => "added",
            Some(previous) if previous != body => "changed",
            Some(_) => continue,
        };
        changes.push(RequirementChange {
            requirement: address.clone(),
            change,
        });
    }
    for address in before
        .keys()
        .filter(|address| !after.contains_key(*address))
    {
        changes.push(RequirementChange {
            requirement: address.clone(),
            change: "removed",
        });
    }
    changes.sort();
    changes
}

/// The findings behaviors' cases make: one per case that did not hold,
/// and one for a behavior whose outcome is worse than its cases', as one
/// that asserted nothing is.
pub(crate) fn findings(reports: &[BehaviorReport], level: Level) -> Vec<Finding> {
    let mut found = Vec::new();
    for report in reports {
        for case in report
            .cases
            .iter()
            .filter(|case| case.outcome != "satisfied")
        {
            let mut message = case
                .problem
                .as_ref()
                .map_or_else(|| format!("the case is {}", case.outcome), problem_message);
            if let Some(reason) = &case.allowed {
                let _ = write!(message, " (allowed: {reason})");
            }
            let counts = case.introduced && case.allowed.is_none();
            found.push(Finding {
                severity: severity_at(report.severity, case.outcome, level, counts),
                link: case.identity.clone(),
                check: report.behavior.clone(),
                message,
                introduced: case.introduced,
                affects: Vec::new(),
                allowed: None,
                values: std::collections::BTreeMap::new(),
            });
        }
        let worst_case = report.cases.iter().map(|case| rank(case.outcome)).max();
        if worst_case.unwrap_or(0) < rank(report.outcome) {
            found.push(Finding {
                severity: severity_at(report.severity, report.outcome, level, true),
                link: report.identity.clone(),
                check: report.behavior.clone(),
                message: format!(
                    "{} is {}: {}",
                    report.identity,
                    report.outcome,
                    report.notes.join("; ")
                ),
                introduced: true,
                affects: Vec::new(),
                allowed: None,
                values: std::collections::BTreeMap::new(),
            });
        }
    }
    found
}

/// A problem in a sentence, with where it is and what it compared.
fn problem_message(problem: &ProblemReport) -> String {
    let mut message = format!(
        "{} ({}/{}:{})",
        problem.message, problem.repository, problem.file, problem.line
    );
    if !problem.operands.is_empty() {
        let operands: Vec<String> = problem
            .operands
            .iter()
            .map(|(expression, value)| format!("{expression} = {value}"))
            .collect();
        let _ = write!(message, "; {}", operands.join(", "));
    }
    if let Some(because) = &problem.because {
        let _ = write!(message, "; {because}");
    }
    message
}

/// How bad an outcome is, worst highest, as spec 13 section 9 orders them.
fn rank(outcome: &str) -> u8 {
    match outcome {
        "violated" => 3,
        "unknown" => 2,
        "stale" => 1,
        _ => 0,
    }
}

/// A case's severity at `level` (spec 13, section 11): a violated
/// error-severity behavior fails at `check` and `strict`, an unknown or
/// stale one only at `strict`; what does not count is context.
fn severity_at(severity: &str, outcome: &str, level: Level, counts: bool) -> Severity {
    if !counts {
        return Severity::Info;
    }
    match (severity, outcome, level) {
        ("error", "violated", Level::Check | Level::Strict)
        | ("error", "unknown" | "stale", Level::Strict) => Severity::Error,
        ("error" | "warning", _, _) => Severity::Warning,
        _ => Severity::Info,
    }
}
