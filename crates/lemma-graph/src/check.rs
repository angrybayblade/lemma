//! Loading a system of descriptions and checking its links.
//!
//! The system starts at a parent description. Every external service it or
//! a repository it reaches declares (`#name`) is resolved to a checkout and
//! its description read there; the first declaration of a name wins, so a
//! parent's pin overrides a repository's. The changed repository is read at
//! the change's two commits, every other at its pin. Links come from every
//! `graph` at head.

use std::collections::{BTreeMap, BTreeSet, VecDeque};
use std::path::{Path, PathBuf};
use std::sync::Arc;

use lemma_logic::Engine;
use lemma_model::Language;
use lemma_syntax::Extractor;
use rayon::prelude::*;

use crate::GraphError;
use crate::adapters;
use crate::address::{Address, Link, parse_link};
use crate::affects::{self, Walkable};
use crate::behaviors::{BehaviorReport, RequirementChange};
use crate::compare::{Difference, compare};
use lemma_lang::load::MODULE_FILE;

use crate::config::{LemmaFile, Level, Operation, SchemaSource, Service};
use crate::infer;
use crate::repos::{
    Checkout, Cloning, UnavailableRepository, clone_remote, fork_point, is_cloned, resolve_commit,
};
use crate::schema::{self, Context, Role};

/// The name of the repository holding the parent description.
pub(crate) const SELF: &str = "self";
pub(crate) const BASE: usize = 0;
pub(crate) const HEAD: usize = 1;

/// The change being checked: a repository and its commits before and with
/// the change.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Change {
    /// The external service's name (`#billing`), or `self` for the
    /// repository holding the parent description.
    pub repository: String,
    /// The commit before the change; when absent, where the repository's
    /// checked-out commit left its default branch (see
    /// [`crate::repos::fork_point`]), so a branch's work is checked as a whole.
    pub base: Option<String>,
    /// The commit with the change; the working directory, uncommitted
    /// changes included, when absent.
    pub head: Option<String>,
}

/// What to check, and where to cache clones and worktrees.
#[derive(Debug, Clone, PartialEq, Eq)]
#[allow(
    clippy::struct_excessive_bools,
    reason = "each switch is independent of the others"
)]
pub struct CheckOptions {
    /// The change; without one, every repository is read once and links are
    /// checked as they stand.
    pub change: Option<Change>,
    /// Overrides the `graph`'s level.
    pub level: Option<Level>,
    /// Whether each inferred finding says why it holds.
    pub explain: bool,
    /// Derived relations whose rows to include in the report.
    pub relations: Vec<String>,
    /// Lemma's cache directory.
    pub cache: PathBuf,
    /// Whether a repository not in the cache may be cloned.
    pub cloning: Cloning,
    /// Where facts read from code are kept between runs; `None` keeps
    /// nothing.
    pub facts_cache: Option<lemma_verify::FactsCache>,
    /// How much consumer code inference reads: what the change's findings
    /// rest on (the default), or everything.
    pub evaluation: lemma_verify::Evaluation,
    /// Also list every link found: those declared, and every call from code
    /// to a documented operation. Inference then reads all consumer code,
    /// and runs at head when there is no change.
    pub list_links: bool,
    /// Whether behaviors may run checkers for evidence not kept yet;
    /// without, kept evidence is used and the rest is unknown or stale.
    pub run_checkers: bool,
    /// Whether, with a change, behaviors may also run checkers at the base,
    /// so a case the change fixed is reported as fixed. Without, the base
    /// uses kept evidence only.
    pub runs_base_checkers: bool,
    /// The check of a parent module to run instead of its `check main`.
    pub select: Option<String>,
    /// Reads kept from earlier checks of the same workspace, by a
    /// long-lived process that drops what changes; `None` reads everything.
    pub warm: Option<Arc<crate::warm::Warm>>,
    /// The environment resolvers, adapters and checkers start from instead
    /// of this process's own: the asking process's, for a check the daemon
    /// answers on its behalf. `None` uses this process's.
    pub environment: Option<Arc<BTreeMap<String, String>>>,
}

impl CheckOptions {
    /// These options with the changed repository named as loading names
    /// it: a module's workspace writes `repo billing`, a description writes
    /// `#billing`, and either is the external service `#billing`.
    fn with_change_named_as_loaded(&self) -> CheckOptions {
        let mut options = self.clone();
        if let Some(change) = &mut options.change {
            let is_loaded_name = change.repository == SELF || change.repository.starts_with('#');
            if !is_loaded_name {
                change.repository = format!("#{}", change.repository);
            }
        }
        options
    }
}

impl CheckOptions {
    /// These options with the changed repository named as `repos` names it,
    /// if the change names none of them but is the directory of exactly one:
    /// a repository is `web_app` to a module and `web-app` to
    /// the people and tools working in it.
    fn with_change_found_by_directory(
        &self,
        repos: &BTreeMap<String, Repo>,
    ) -> Option<CheckOptions> {
        let change = self.change.as_ref()?;
        if repos.contains_key(&change.repository) {
            return None;
        }
        let directory = change.repository.trim_start_matches('#');
        let mut found = repos.iter().filter(|(name, repo)| {
            name.as_str() != SELF
                && repo.checkouts[HEAD]
                    .root
                    .file_name()
                    .is_some_and(|name| name == directory)
        });
        let (name, _) = found.next()?;
        if found.next().is_some() {
            return None;
        }
        let mut options = self.clone();
        options.change.as_mut()?.repository.clone_from(name);
        Some(options)
    }
}

/// Every link a check found, as [`CheckOptions::list_links`] asks.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize)]
pub struct LinkList {
    /// The links descriptions declare, as `caller -> provider`.
    pub declared: Vec<String>,
    /// The calls from code to documented operations inference found.
    pub inferred: Vec<InferredLink>,
}

/// A call from code to a documented operation, found by inference.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, serde::Serialize)]
pub struct InferredLink {
    /// The calling repository, as the parent names it (`#dashboard`).
    pub repository: String,
    /// `file::Function`, or the file for a call outside functions.
    pub location: String,
    /// The operation called, as `METHOD /path`.
    pub operation: String,
    /// The service serving it (`#platform:gateway`), when one does.
    pub service: Option<String>,
}

/// How serious a finding is.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, serde::Serialize)]
#[serde(rename_all = "lowercase")]
pub enum Severity {
    /// Fails the check (level `check`).
    Error,
    /// Reported (level `audit`).
    Warning,
    /// Context: differences nothing is expected to break on.
    Info,
}

/// One result of checking a link.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize)]
pub struct Finding {
    /// How serious it is.
    pub severity: Severity,
    /// The link, as `caller -> provider`.
    pub link: String,
    /// Which check found it: `link`, `request`, `response` or `schema` for a
    /// declared link; the grammar requirement's name for an inferred one.
    pub check: String,
    /// What was found.
    pub message: String,
    /// Whether the change introduced it, rather than it being there before.
    pub introduced: bool,
    /// For a problem: the operations whose handlers reach it and the code
    /// elsewhere that calls them, nearest first, traced through the call
    /// graph by name, so approximate.
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub affects: Vec<Impact>,
    /// The reason of the `allow` covering it; an allowed finding is
    /// informational, whatever its requirement's severity.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub allowed: Option<String>,
    /// A grammar requirement's finding: the value of each variable, which
    /// allowances read.
    #[serde(skip)]
    pub values: BTreeMap<String, String>,
    /// Why an inferred finding holds, when [`CheckOptions::explain`] asks.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub derivation: Option<String>,
    /// The name `lemma verify` reports it under, if it is about the
    /// description rather than a contract.
    #[serde(skip)]
    pub description_name: Option<&'static str>,
}

/// Something a problem affects further up.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize)]
pub struct Impact {
    /// How many operations away from the problem: 0 for an operation whose
    /// handler reaches it, 1 for code calling that operation, and so on.
    pub distance: usize,
    /// The service serving an operation, or the repository of calling code.
    pub owner: String,
    /// `METHOD /path` for an operation; `file::Function` for calling code.
    pub at: String,
    /// For calling code, the operation it calls.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub calls: Option<String>,
    /// For an operation, what reaches it: its handler (`file:Function`) or
    /// the calling code one hop nearer the problem.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub via: Option<String>,
    /// For an operation whose handler reaches the problem itself, the
    /// calls from the handler down to the code with the problem, each
    /// `file:Function`.
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub path: Vec<String>,
}

/// A repository as it was read.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize)]
pub struct ResolvedRepository {
    /// Its name.
    pub name: String,
    /// Its local clone.
    pub root: String,
    /// The commit read before the change (`None`: the working directory).
    pub base: Option<String>,
    /// The commit read with the change.
    pub head: Option<String>,
    /// Whether its description came from the working directory because a
    /// commit read had none, as when replaying history from before the
    /// repository adopted Lemma. Its code is still read at the commit.
    pub manifest_from_working_directory: bool,
}

/// The result of checking a system's links.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize)]
pub struct Report {
    /// The level the links were checked at.
    pub level: Level,
    /// How many declared links were checked.
    pub links: usize,
    /// How many calls from code to the operations the change touches were
    /// found and checked (inference reads only the code that may make
    /// them); `None` when inference does not run.
    pub inferred_links: Option<usize>,
    /// Every repository read.
    pub repositories: Vec<ResolvedRepository>,
    /// Findings, most severe first.
    pub findings: Vec<Finding>,
    /// Every link found, when [`CheckOptions::list_links`] asks.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub link_list: Option<LinkList>,
    /// The behaviors the parent's `.lm` program ran, and their cases.
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub behaviors: Vec<BehaviorReport>,
    /// The requirements and behaviors the change added, removed or
    /// changed.
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub requirement_changes: Vec<RequirementChange>,
    /// Repositories that are not here, so this check did not cover them.
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub not_checked: Vec<NotChecked>,
    /// The rows of each derived relation [`CheckOptions::relations`] asks.
    #[serde(skip)]
    pub relations: BTreeMap<String, Vec<Vec<String>>>,
}

/// A repository a check could not read.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize)]
pub struct NotChecked {
    /// Its name, as loading names it: `#billing`.
    pub repository: String,
    /// How many links reach it.
    pub links: usize,
}

/// A repository at both snapshots.
pub(crate) struct Repo {
    pub(crate) checkouts: [Checkout; 2],
    pub(crate) files: [LemmaFile; 2],
    pub(crate) manifest_from_working_directory: bool,
    /// The repositories this one names that are not here.
    pub(crate) unavailable: BTreeSet<UnavailableRepository>,
}

/// The directory of each repository the system whose parent description is
/// `root_file` reads, itself included: what a watcher of
/// its files must watch. Read as [`check`] reads them, at the head.
pub fn repository_roots(
    root_file: &Path,
    options: &CheckOptions,
) -> Result<Vec<PathBuf>, GraphError> {
    let options = &options.with_change_named_as_loaded();
    let repos = load(root_file, options)?;
    let mut roots: Vec<PathBuf> = repos
        .values()
        .map(|repo| repo.checkouts[HEAD].root.clone())
        .collect();
    roots.sort();
    roots.dedup();
    Ok(roots)
}

/// The level a check runs at: the one asked for, or the workspace's.
fn check_level(repos: &BTreeMap<String, Repo>, options: &CheckOptions) -> Level {
    options
        .level
        .or(repos[SELF].files[HEAD].graph.level)
        .unwrap_or_default()
}

/// Checks the links of the system whose parent description is `root_file`,
/// evaluating inferred links' grammar with `engine`. With `references`,
/// also checks that each link's caller is anchored at code that is called.
pub fn check<E: Engine>(
    root_file: &Path,
    options: &CheckOptions,
    engine: &E,
    references: Option<&mut dyn crate::anchors::References>,
) -> Result<Report, GraphError> {
    let started = std::time::Instant::now();
    let (repos, mut options) = load_with_change_named(root_file, options)?;
    // Tracing verifies the same repositories again; without a long-lived
    // process's reads, this check keeps its own for those later runs.
    options.warm.get_or_insert_with(Arc::default);
    let options = &options;
    check_change_names_a_repository(&repos, options)?;
    log_repositories(&repos);
    let level = check_level(&repos, options);
    let (links, not_checked) = split_unchecked_links(collect_links(&repos)?, &repos);
    // Verification and tracing read the same repositories; each is listed
    // once.
    let listings = options
        .warm
        .as_ref()
        .map_or_else(Arc::default, |warm| warm.verify().listings());
    let mut shared = Shared::new(options, &listings, root_file, &repos)?;
    let behaviors = Behaviors {
        manifest: manifest_name(root_file),
        repos: &repos,
        options,
    };
    let (mut linked, evaluated) = std::thread::scope(|scope| {
        // Without a change no walk keys evidence, so behaviors need nothing
        // the links' checks find, and their checkers run beside them.
        let early = options.change.is_none().then(|| {
            let program = shared.program.take();
            scope.spawn(|| behaviors.run(program, None))
        });
        let mut linked = check_links(&repos, &links, &mut shared, engine, references)?;
        let evaluated = match early {
            Some(running) => running
                .join()
                .unwrap_or_else(|panic| std::panic::resume_unwind(panic))?,
            None => behaviors.run(shared.program.take(), linked.reach.take().as_ref())?,
        };
        Ok::<_, GraphError>((linked, evaluated))
    })?;
    let (behaviors, requirement_changes) =
        apply_behaviors(evaluated, &repos, level, &mut linked.findings);
    let mut findings = linked.findings;
    sort_findings(&mut findings);
    log_outcome(&findings, started);
    let walkable = linked.walkable;
    Ok(Report {
        level,
        links: links.len(),
        inferred_links: linked.inferred_links,
        repositories: resolved(&repos),
        findings,
        link_list: options.list_links.then(|| link_list(&links, &walkable)),
        behaviors,
        requirement_changes,
        not_checked,
        relations: walkable.relations,
    })
}

/// What checking a system's links found: findings, the number of calls
/// inference found, if it ran, what tracing walks, and what the change
/// reaches.
struct Linked {
    findings: Vec<Finding>,
    inferred_links: Option<usize>,
    walkable: Walkable,
    reach: Option<crate::reach::ChangeReach>,
}

/// Checks the declared links, infers links from code and traces what both
/// found; with `references`, also checks that each link's caller is
/// anchored at code that is called.
fn check_links<E: Engine>(
    repos: &BTreeMap<String, Repo>,
    links: &BTreeSet<Link>,
    shared: &mut Shared<'_>,
    engine: &E,
    references: Option<&mut dyn crate::anchors::References>,
) -> Result<Linked, GraphError> {
    let options = shared.options;
    let level = check_level(repos, options);
    let (mut findings, inference) = match options.change {
        Some(_) => declared_beside_inference(repos, links, shared, engine, level)?,
        None => (
            check_declared_beside_adapters(repos, links, level, options)?,
            None,
        ),
    };
    if options.change.is_some() {
        findings.extend(crate::description::introduced_by_change(repos, level));
    }
    log::info!(
        "checked {} declared links: {} findings",
        links.len(),
        findings.len()
    );
    let mut seeds = declared_seeds(&findings, links, repos);
    // After the seeds: a doubtful anchor is no problem to trace from.
    if let Some(references) = references {
        let anchored = crate::anchors::check_anchors(repos, links, &options.cache, references)?;
        findings.extend(anchored);
    }
    let (inferred_links, walkable) = match inference {
        Some(inferred) => inferred.append_to(&mut findings),
        None => infer(repos, shared, engine, level, &mut findings)?,
    };
    fail_unread_contracts(level, &mut findings);
    seeds.extend(walkable.seeds.iter().cloned());
    let tracing = Tracing {
        repos,
        walkable: &walkable,
        listings: shared.listings,
        facts: options.facts_cache.as_ref(),
        warm: options.warm.as_ref().map(|warm| warm.verify()),
    };
    // The change's reach needs only the changed repository's functions,
    // nothing tracing finds, so the two run side by side.
    let (attached, reach) = std::thread::scope(|scope| {
        let reach = scope.spawn(|| tracing.change_reach());
        let attached = tracing.attach(&mut findings, &seeds, engine);
        let reach = reach
            .join()
            .unwrap_or_else(|panic| std::panic::resume_unwind(panic));
        (attached, reach)
    });
    attached?;
    // After tracing, whose seeds point into the findings by index.
    shared.keep_selected(&mut findings);
    Ok(Linked {
        findings,
        inferred_links,
        walkable,
        reach,
    })
}

/// The repositories at `root_file` and the options, with the change named as
/// loading names its repository: by that name, or else by its directory.
fn load_with_change_named(
    root_file: &Path,
    options: &CheckOptions,
) -> Result<(BTreeMap<String, Repo>, CheckOptions), GraphError> {
    let options = options.with_change_named_as_loaded();
    let repos = load(root_file, &options)?;
    let Some(found) = options.with_change_found_by_directory(&repos) else {
        return Ok((repos, options));
    };
    Ok((load(root_file, &found)?, found))
}

/// What tracing what problems affect reads.
struct Tracing<'a> {
    repos: &'a BTreeMap<String, Repo>,
    walkable: &'a Walkable,
    listings: &'a Arc<lemma_http::Listings>,
    facts: Option<&'a lemma_verify::FactsCache>,
    warm: Option<Arc<lemma_verify::Warm>>,
}

impl Tracing<'_> {
    /// Adds to each problem among `findings` with a seed what it affects,
    /// asking `engine` for the calls inference did not read.
    fn attach<E: Engine>(
        &self,
        findings: &mut [Finding],
        seeds: &[(usize, crate::impact::Seed)],
        engine: &E,
    ) -> Result<(), GraphError> {
        let reading = affects::CodeReading {
            facts: self.facts,
            listings: self.listings,
        };
        let reads = VerificationReads {
            facts: self.facts,
            listings: self.listings,
            warm: self.warm.as_ref(),
        };
        let mut fetch = |operations: &BTreeSet<String>| {
            fetch_calls(self.walkable.partial.as_ref(), operations, engine, &reads)
        };
        let mut source = affects::CallSource {
            walkable: self.walkable,
            fetch: &mut fetch,
        };
        affects::attach(findings, seeds, &mut source, self.repos, &reading)
    }

    /// What a change to a repository's working directory reaches, with its
    /// documented operations' route files, when the change is one.
    fn change_reach(&self) -> Option<crate::reach::ChangeReach> {
        let reach = change_reach_of(self.repos, self.listings, self.facts)?;
        Some(with_route_files(
            reach,
            (self.repos, self.facts),
            self.walkable,
        ))
    }
}

/// What a change to a repository's working directory reaches, by name,
/// when the change is one: read from the repository's functions as tracing
/// reads them.
fn change_reach_of(
    repos: &BTreeMap<String, Repo>,
    listings: &lemma_http::Listings,
    facts: Option<&lemma_verify::FactsCache>,
) -> Option<crate::reach::ChangeReach> {
    let started = std::time::Instant::now();
    let (name, repo) = repos.iter().find(|(_, repo)| {
        repo.checkouts[HEAD].commit.is_none() && repo.checkouts[BASE].commit.is_some()
    })?;
    let base = repo.checkouts[BASE].commit.clone()?;
    let reading = affects::CodeReading { facts, listings };
    let functions = affects::Code::new(repos, &reading).functions(name);
    let root = &repo.checkouts[HEAD].root;
    // The listing tracing read named the untracked files already.
    let untracked = listings.untracked(root).ok();
    let reach = crate::reach::change_reach(
        name,
        (root, &base),
        untracked.as_deref().map(Vec::as_slice),
        &functions,
    );
    log::info!(
        "reach: the change to {name} reaches {} of {} functions' names{} in {:.1}s",
        reach.len(),
        functions.len(),
        if reach.everything {
            " (and anything: it changed more than functions)"
        } else {
            ""
        },
        started.elapsed().as_secs_f64()
    );
    Some(reach)
}

/// `reach` with the route files of the changed repository's documented
/// operations, by `service.operationId`, read from its documents at head
/// and the routes inference found.
fn with_route_files(
    mut reach: crate::reach::ChangeReach,
    (repos, facts): (&BTreeMap<String, Repo>, Option<&lemma_verify::FactsCache>),
    walkable: &Walkable,
) -> crate::reach::ChangeReach {
    let Some(repo) = repos.get(&reach.repository) else {
        return reach;
    };
    let checkout = &repo.checkouts[HEAD];
    // Verification kept each document's operations by its contents.
    let kept = facts.map(lemma_verify::FactsCache::documents);
    for (service, declared) in &repo.files[HEAD].services {
        for document_file in &declared.exports.openapi {
            let Ok(Some(text)) = checkout.read(document_file) else {
                continue;
            };
            let Ok(operations) =
                lemma_verify::document_operations(document_file, &text, kept.as_deref())
            else {
                continue;
            };
            let found = route_files_of(&operations, &reach.repository, walkable);
            reach.route_files.extend(
                found
                    .into_iter()
                    .map(|(id, files)| (format!("{service}.{id}"), files)),
            );
        }
    }
    reach
}

/// For each operation of `document` with an `operationId`, the files
/// `repository`'s routes for it are declared in, when inference found any.
fn route_files_of(
    operations: &[lemma_http::Operation],
    repository: &str,
    walkable: &Walkable,
) -> Vec<(String, Vec<String>)> {
    let mut found = Vec::new();
    for operation in operations {
        let Some(id) = &operation.operation_id else {
            continue;
        };
        let key = format!("{} {}", operation.method, operation.path);
        let files: Vec<String> = walkable
            .routes
            .iter()
            .filter(|site| site.repository == repository && site.operation == key)
            .map(|site| site.file.clone())
            .collect();
        if !files.is_empty() {
            found.push((id.clone(), files));
        }
    }
    found
}

/// The file name of the parent's manifest, which behaviors load its
/// program by.
fn manifest_name(root_file: &Path) -> String {
    root_file.file_name().map_or_else(
        || MODULE_FILE.to_owned(),
        |name| name.to_string_lossy().into_owned(),
    )
}

/// What running the parent's behaviors reads: its manifest, the
/// repositories and the options.
struct Behaviors<'a> {
    manifest: String,
    repos: &'a BTreeMap<String, Repo>,
    options: &'a CheckOptions,
}

impl Behaviors<'_> {
    /// Runs the behaviors of the parent's `.lm` program, with the head's
    /// program if inference already checked it and what the change
    /// reaches, which decides what kept evidence still holds.
    fn run(
        &self,
        head: Option<crate::behaviors::HeadProgram>,
        reach: Option<&crate::reach::ChangeReach>,
    ) -> Result<Option<crate::behaviors::Evaluated>, GraphError> {
        crate::behaviors::run(&self.manifest, self.repos, self.options, head, reach)
    }
}

/// Adds what the behaviors found, with the allowances that cover it and
/// every other finding; their reports and what the change did to
/// requirements.
fn apply_behaviors(
    evaluated: Option<crate::behaviors::Evaluated>,
    repos: &BTreeMap<String, Repo>,
    level: Level,
    findings: &mut Vec<Finding>,
) -> (Vec<BehaviorReport>, Vec<RequirementChange>) {
    let Some(mut evaluated) = evaluated else {
        return (Vec::new(), Vec::new());
    };
    // The newest commit read dates the check: a change's head, or, without
    // a change, the newest of the repositories' commits, whether or not the
    // workspace's own directory is a Git repository. Only allowances read
    // the date, and finding it asks Git once per repository, so it is found
    // only when there are allowances, asking for every repository at once.
    let date = if evaluated.head.model.allowances.is_empty() {
        None
    } else {
        repos
            .par_iter()
            .filter_map(|(_, repo)| crate::allowances::committer_date(&repo.checkouts[HEAD]))
            .max()
    };
    let mut allowances = crate::allowances::Allowed::new(&evaluated.head.model, date);
    allowances.cover_cases(&evaluated.results, &mut evaluated.reports);
    findings.extend(crate::behaviors::findings(&evaluated.reports, level));
    allowances.cover_findings(findings);
    let ran = evaluated
        .results
        .iter()
        .map(|result| result.address.clone())
        .collect();
    findings.extend(allowances.findings(&ran));
    (evaluated.reports, evaluated.changes)
}

/// The links inferred from code, as the options ask: for the change, at
/// head to list them, or where a service's code and committed documents
/// may differ; their count, if inference ran, and what tracing walks.
fn infer<E: Engine>(
    repos: &BTreeMap<String, Repo>,
    shared: &Shared<'_>,
    engine: &E,
    level: Level,
    findings: &mut Vec<Finding>,
) -> Result<(Option<usize>, Walkable), GraphError> {
    let options = shared.options;
    let inference = match &options.change {
        Some(change) => infer_change(repos, change, shared, engine, findings)?,
        None if options.list_links => infer_at_head(repos, shared, engine)?,
        None => infer_drift(repos, level, engine, findings, shared)?,
    };
    let Some((count, walkable)) = inference else {
        log::info!("no inference: nothing changed that inferred links could rest on");
        return Ok((None, Walkable::default()));
    };
    Ok((Some(count), walkable))
}

/// A commit or reference as logs show it: its first ten characters.
fn short(commit: &str) -> String {
    commit.chars().take(10).collect()
}

/// Logs each repository read and the commits it was read at.
fn log_repositories(repos: &BTreeMap<String, Repo>) {
    for repository in resolved(repos) {
        let at = |commit: &Option<String>| {
            commit.as_deref().map_or_else(
                || "working directory".to_owned(),
                |commit| commit.chars().take(10).collect(),
            )
        };
        let commits = if repository.base == repository.head {
            at(&repository.head)
        } else {
            format!("{}..{}", at(&repository.base), at(&repository.head))
        };
        let manifest = if repository.manifest_from_working_directory {
            ", the description from the working directory"
        } else {
            ""
        };
        log::info!(
            "repository {} at {} ({commits}{manifest})",
            repository.name,
            repository.root
        );
    }
}

/// Logs how many findings of each severity a check ended with, and how
/// long it took.
fn log_outcome(findings: &[Finding], started: std::time::Instant) {
    let count = |severity: Severity| {
        findings
            .iter()
            .filter(|finding| finding.severity == severity)
            .count()
    };
    log::info!(
        "check done in {:.1}s: {} errors, {} warnings, {} informational",
        started.elapsed().as_secs_f64(),
        count(Severity::Error),
        count(Severity::Warning),
        count(Severity::Info)
    );
}

/// Fails when the change names a repository no description declares.
fn check_change_names_a_repository(
    repos: &BTreeMap<String, Repo>,
    options: &CheckOptions,
) -> Result<(), GraphError> {
    let Some(change) = &options.change else {
        return Ok(());
    };
    if repos.contains_key(&change.repository) {
        return Ok(());
    }
    if unavailable_repositories(repos)
        .iter()
        .any(|missing| missing.name == change.repository)
    {
        return Err(GraphError::UnavailableRepository {
            name: change.repository.clone(),
        });
    }
    Err(GraphError::UnknownRepository {
        name: change.repository.clone(),
    })
}

/// The links declared, and the calls inference found, each call with the
/// service serving its operation.
fn link_list(links: &BTreeSet<Link>, walkable: &Walkable) -> LinkList {
    let declared = links
        .iter()
        .map(|link| format!("{} -> {}", link.caller, link.provider))
        .collect();
    let inferred: BTreeSet<InferredLink> = walkable
        .calls
        .iter()
        .map(|call| InferredLink {
            repository: call.repository.clone(),
            location: match &call.function {
                Some(function) => format!("{}::{function}", call.file),
                None => call.file.clone(),
            },
            operation: call.operation.clone(),
            service: walkable.services.get(&call.operation).cloned(),
        })
        .collect();
    LinkList {
        declared,
        inferred: inferred.into_iter().collect(),
    }
}

/// Without a change, for a list of links: one inference over every
/// repository at head, its findings left out, since there is nothing to
/// compare head with. The first repository inference reads stands as the
/// changed one, at `HEAD` on both sides.
fn infer_at_head<E: Engine>(
    repos: &BTreeMap<String, Repo>,
    shared: &Shared<'_>,
    engine: &E,
) -> Result<Option<(usize, Walkable)>, GraphError> {
    let options = shared.options;
    for name in repos.keys() {
        let run = Inference {
            changed: name,
            grammar: shared.grammar.as_ref(),
            base: "HEAD",
            head: Some("HEAD"),
            is_drift: false,
            cache: &options.cache,
            facts_cache: options.facts_cache.as_ref(),
            listings: shared.listings,
            evaluation: lemma_verify::Evaluation::Full,
            warm: options.warm.as_ref().map(|warm| warm.verify()),
            environment: options.environment.clone(),
            explain: false,
            relations: &options.relations,
        };
        let mut ignored = Vec::new();
        if let Some(found) = infer_links(repos, &run, Level::Audit, engine, &mut ignored)? {
            return Ok(Some((
                found.0,
                Walkable {
                    seeds: Vec::new(),
                    ..found.1
                },
            )));
        }
    }
    Ok(None)
}

/// When inference read only some consumer code: the operations whose calls
/// it holds completely, and what to ask for more with.
fn partial_calls(
    report: &lemma_verify::Report,
    inferred: &infer::Inferred,
) -> Option<affects::PartialCalls> {
    let covered = report.covered.as_ref()?;
    Some(affects::PartialCalls {
        covered: covered.iter().cloned().collect(),
        manifest: inferred.manifest.clone(),
        owners: inferred.repositories.clone(),
    })
}

/// Sorts findings by severity, link, check and message, so a report reads
/// the same every run.
fn sort_findings(findings: &mut [Finding]) {
    findings.sort_by(|left, right| {
        (left.severity, &left.link, &left.check, &left.message).cmp(&(
            right.severity,
            &right.link,
            &right.check,
            &right.message,
        ))
    });
}

/// What a verification run reads with besides its manifest: facts kept
/// between runs, the repositories' listings, and the reads of earlier runs
/// of the same process.
pub(crate) struct VerificationReads<'a> {
    pub(crate) facts: Option<&'a lemma_verify::FactsCache>,
    pub(crate) listings: &'a Arc<lemma_http::Listings>,
    pub(crate) warm: Option<&'a Arc<lemma_verify::Warm>>,
}

/// The calls of `operations` the inference behind `partial` did not read:
/// verified again, reading only the code that may make them.
pub(crate) fn fetch_calls<E: Engine>(
    partial: Option<&affects::PartialCalls>,
    operations: &BTreeSet<String>,
    engine: &E,
    reads: &VerificationReads<'_>,
) -> Result<Vec<crate::impact::Call>, GraphError> {
    let Some(partial) = partial else {
        return Ok(Vec::new());
    };
    let options = lemma_verify::Options {
        explain: false,
        relations: affects::TRACED_RELATIONS.map(str::to_owned).to_vec(),
        facts_cache: reads.facts.cloned(),
        evaluation: lemma_verify::Evaluation::Demand,
        warm: reads.warm.cloned(),
        also_keep: None,
    };
    let report = lemma_verify::calls_of(
        &partial.manifest,
        engine,
        &options,
        reads.listings,
        operations,
    )
    .map_err(|source| GraphError::Inference { source })?;
    Ok(affects::walkable(&report.relations, &partial.owners).calls)
}

/// The findings of the declared links, and of documents Lemma cannot read.
/// What inference found apart from the declared links: the number of calls
/// it found, if it ran, what tracing walks, and its findings, which its
/// seeds point into from 0.
struct InferredApart {
    links: Option<usize>,
    walkable: Walkable,
    findings: Vec<Finding>,
}

impl InferredApart {
    /// Adds the findings after `findings`, moving the seeds with them.
    fn append_to(mut self, findings: &mut Vec<Finding>) -> (Option<usize>, Walkable) {
        let offset = findings.len();
        for (index, _) in &mut self.walkable.seeds {
            *index += offset;
        }
        findings.append(&mut self.findings);
        (self.links, self.walkable)
    }
}

/// [`check_declared`] and, for a change, inference, at once: inference
/// reads nothing declared links find unless there is no change, when what
/// they found decides whether it traces. Inference runs its adapters
/// itself, so none are run ahead.
fn declared_beside_inference<E: Engine>(
    repos: &BTreeMap<String, Repo>,
    links: &BTreeSet<Link>,
    shared: &Shared<'_>,
    engine: &E,
    level: Level,
) -> Result<(Vec<Finding>, Option<InferredApart>), GraphError> {
    let options = shared.options;
    std::thread::scope(|scope| {
        let declared = scope.spawn(|| check_declared(repos, links, level, options));
        let mut findings = Vec::new();
        let inferred = infer(repos, shared, engine, level, &mut findings);
        let declared = declared
            .join()
            .unwrap_or_else(|panic| std::panic::resume_unwind(panic))?;
        let (links, walkable) = inferred?;
        Ok((
            declared,
            Some(InferredApart {
                links,
                walkable,
                findings,
            }),
        ))
    })
}

/// [`check_declared`], with the adapters inference will ask for run beside
/// it: their commands are the slowest part of a cold check and read nothing
/// declared links decide.
fn check_declared_beside_adapters(
    repos: &BTreeMap<String, Repo>,
    links: &BTreeSet<Link>,
    level: Level,
    options: &CheckOptions,
) -> Result<Vec<Finding>, GraphError> {
    std::thread::scope(|scope| {
        scope.spawn(|| prefetch_adapters(repos, options));
        check_declared(repos, links, level, options)
    })
}

fn check_declared(
    repos: &BTreeMap<String, Repo>,
    links: &BTreeSet<Link>,
    level: Level,
    options: &CheckOptions,
) -> Result<Vec<Finding>, GraphError> {
    let (reads, outputs) = options
        .warm
        .as_ref()
        .map(|warm| warm.take())
        .unwrap_or_default();
    let mut checker = Checker {
        repos,
        level,
        cache: &options.cache,
        extractor: Extractor::new().map_err(|source| GraphError::Syntax { source })?,
        outputs,
        reads,
        findings: Vec::new(),
        has_change: options.change.is_some(),
        environment: options.environment.clone(),
        unread: BTreeMap::new(),
    };
    let links_read = links.iter().try_for_each(|link| checker.check_link(link));
    // Adapters build in the same shared worktree resolvers ran in.
    checker.reads.release();
    if let Some(warm) = &options.warm {
        warm.put(
            std::mem::take(&mut checker.reads),
            std::mem::take(&mut checker.outputs),
        );
    }
    links_read?;
    let mut findings = checker.findings;
    findings.extend(checker.unread.into_values().map(Unread::into_finding));
    findings.extend(unsupported_documents(repos)?);
    Ok(findings)
}

/// Where each declared link's problems start a trace: the link's caller.
fn declared_seeds(
    findings: &[Finding],
    links: &BTreeSet<Link>,
    repos: &BTreeMap<String, Repo>,
) -> Vec<(usize, crate::impact::Seed)> {
    let by_label: BTreeMap<String, &Link> = links
        .iter()
        .map(|link| (format!("{} -> {}", link.caller, link.provider), link))
        .collect();
    findings
        .iter()
        .enumerate()
        .filter(|(_, finding)| finding.check != "schema")
        .filter_map(|(index, finding)| {
            let link = by_label.get(&finding.link)?;
            Some((index, affects::declared_seed(link, repos)?))
        })
        .collect()
}

/// One inference run: the repository read at two commits, and whether it
/// compares a service's committed documents with what its code describes
/// (both commits are then `HEAD`, the code read from the working
/// directory).
struct Inference<'a> {
    changed: &'a str,
    /// The grammar the parent's `.lm` program lowers to; the built-in one
    /// when `None`.
    grammar: Option<&'a lemma_grammar::GrammarFile>,
    base: &'a str,
    /// `None` for the working directory.
    head: Option<&'a str>,
    is_drift: bool,
    cache: &'a Path,
    facts_cache: Option<&'a lemma_verify::FactsCache>,
    listings: &'a Arc<lemma_http::Listings>,
    evaluation: lemma_verify::Evaluation,
    /// What verification keeps between checks, in a long-lived process.
    warm: Option<Arc<lemma_verify::Warm>>,
    /// The environment adapters start from, when not this process's own.
    environment: Option<Arc<BTreeMap<String, String>>>,
    explain: bool,
    relations: &'a [String],
}

/// What every inference in one check shares: its options, and the
/// listings verification and tracing both read.
struct Shared<'a> {
    options: &'a CheckOptions,
    listings: &'a Arc<lemma_http::Listings>,
    /// The grammar the parent's `.lm` program lowers to, if it is one.
    grammar: Option<lemma_grammar::GrammarFile>,
    /// The program the grammar was lowered from, checked at the head, for
    /// its behaviors to run without checking it again.
    program: Option<crate::behaviors::HeadProgram>,
    /// Without a program: the built-in requirement a selection names, whose
    /// findings alone are kept.
    only: Option<String>,
}

impl<'a> Shared<'a> {
    /// What inferences share, with the grammar of the program at
    /// `root_file` when it is a module.
    fn new(
        options: &'a CheckOptions,
        listings: &'a Arc<lemma_http::Listings>,
        root_file: &Path,
        repos: &BTreeMap<String, Repo>,
    ) -> Result<Self, GraphError> {
        let (grammar, program) =
            crate::behaviors::program_grammar(root_file, repos, options)?.unzip();
        let only = match (&grammar, &options.select) {
            (None, Some(name)) => Some(built_in_requirement(root_file, name)?),
            _ => None,
        };
        Ok(Self {
            options,
            listings,
            grammar,
            program,
            only,
        })
    }

    /// Keeps only the selected requirement's findings, and what says a
    /// repository could not be read, which every pass rests on.
    fn keep_selected(&self, findings: &mut Vec<Finding>) {
        let Some(only) = &self.only else {
            return;
        };
        findings.retain(|finding| {
            finding.check == *only || matches!(finding.check.as_str(), "adapter" | "resolver")
        });
    }
}

/// The built-in grammar's requirement `selected` names, bare
/// (`operation_without_consumer`) or with its module
/// (`lemma.http.operation_without_consumer`): a description with no
/// program has no checks or behaviors a selection could name.
fn built_in_requirement(root_file: &Path, selected: &str) -> Result<String, GraphError> {
    let name = selected
        .strip_prefix("lemma.http.")
        .unwrap_or(selected)
        .to_owned();
    if !lemma_verify::is_built_in_requirement(&name) {
        return Err(GraphError::UnknownCheck {
            module: root_file.display().to_string(),
            name: selected.to_owned(),
        });
    }
    Ok(name)
}

/// What a change to one repository breaks, read at its two commits.
fn infer_change<E: Engine>(
    repos: &BTreeMap<String, Repo>,
    change: &Change,
    shared: &Shared<'_>,
    engine: &E,
    findings: &mut Vec<Finding>,
) -> Result<Option<(usize, Walkable)>, GraphError> {
    let (options, listings) = (shared.options, shared.listings);
    let level = options
        .level
        .or(repos[SELF].files[HEAD].graph.level)
        .unwrap_or_default();
    let changed = &repos[&change.repository];
    let Some(base) = changed.checkouts[BASE].commit.as_deref() else {
        return Ok(None);
    };
    let head = changed.checkouts[HEAD].commit.as_deref();
    let run = Inference {
        changed: &change.repository,
        grammar: shared.grammar.as_ref(),
        base,
        head,
        is_drift: false,
        cache: &options.cache,
        facts_cache: options.facts_cache.as_ref(),
        listings,
        evaluation: evaluation_of(options),
        warm: options.warm.as_ref().map(|warm| warm.verify()),
        environment: options.environment.clone(),
        explain: options.explain,
        relations: &options.relations,
    };
    infer_links(repos, &run, level, engine, findings)
}

/// How much consumer code inference reads: everything when every link is
/// to be listed.
fn evaluation_of(options: &CheckOptions) -> lemma_verify::Evaluation {
    if options.list_links {
        return lemma_verify::Evaluation::Full;
    }
    options.evaluation
}

/// Without a change: for each repository with a service whose operations
/// come from its code, what the difference between its committed documents
/// and its code breaks. Only one such repository's calls and routes are
/// kept for tracing.
fn infer_drift<E: Engine>(
    repos: &BTreeMap<String, Repo>,
    level: Level,
    engine: &E,
    findings: &mut Vec<Finding>,
    shared: &Shared<'_>,
) -> Result<Option<(usize, Walkable)>, GraphError> {
    let (options, listings) = (shared.options, shared.listings);
    let mut inferred = None;
    for (name, repo) in repos {
        let has_adapter = repo.files[HEAD]
            .services
            .values()
            .any(|service| service.exports.operations_from.is_some());
        if !has_adapter {
            continue;
        }
        let run = Inference {
            changed: name,
            grammar: shared.grammar.as_ref(),
            base: "HEAD",
            head: Some("HEAD"),
            is_drift: true,
            cache: &options.cache,
            facts_cache: options.facts_cache.as_ref(),
            listings,
            evaluation: options.evaluation,
            warm: options.warm.as_ref().map(|warm| warm.verify()),
            environment: options.environment.clone(),
            explain: options.explain,
            relations: &options.relations,
        };
        if let Some(found) = infer_links(repos, &run, level, engine, findings)? {
            inferred = Some(found);
        }
    }
    Ok(inferred)
}

/// Finds and checks calls from code to documented operations, adding their
/// findings; the number of distinct calls found, and the calls and routes a
/// trace can walk.
fn infer_links<E: Engine>(
    repos: &BTreeMap<String, Repo>,
    run: &Inference<'_>,
    level: Level,
    engine: &E,
    findings: &mut Vec<Finding>,
) -> Result<Option<(usize, Walkable)>, GraphError> {
    let (base, head) = (run.base, run.head);
    let files = repos
        .values()
        .map(inference_file)
        .collect::<Result<Vec<LemmaFile>, GraphError>>()?;
    let sources: Vec<infer::Source<'_>> = repos
        .iter()
        .zip(&files)
        .map(|((name, repo), file)| infer::Source {
            name,
            root: &repo.checkouts[HEAD].root,
            commit: repo.checkouts[HEAD].commit.as_deref(),
            file,
        })
        .collect();
    let changed = infer::Changed {
        name: run.changed,
        base,
        head,
    };
    let started = std::time::Instant::now();
    let Some(mut inferred) = infer::manifest(&sources, &changed) else {
        return Ok(None);
    };
    log::debug!("inference: manifest inferred in {:.0?}", started.elapsed());
    // With nothing to compare and no declared problem to trace, the
    // inference would find what the committed documents already say.
    let needs_trace = findings
        .iter()
        .any(|finding| finding.severity != Severity::Info);
    let started = std::time::Instant::now();
    let snapshots = snapshots_to_list(&inferred.manifest, run.is_drift && !needs_trace);
    let has_drift = adapters_beside_listings(&mut inferred, repos, run, findings, &snapshots);
    log::debug!("inference: adapters applied in {:.0?}", started.elapsed());
    if run.is_drift && !has_drift && !needs_trace {
        return Ok(None);
    }
    inferred.manifest.lowered = run.grammar.cloned();
    let report = verify_inferred(&inferred, run, engine)?;
    let mut walkable = affects::walkable(&report.relations, &inferred.repositories);
    walkable.relations = run
        .relations
        .iter()
        .filter_map(|name| Some((name.clone(), report.relations.get(name)?.clone())))
        .collect();
    walkable.partial = partial_calls(&report, &inferred);
    for found in &report.findings {
        if let Some(seed) = affects::inferred_seed(&found.values, &inferred.repositories) {
            walkable.seeds.push((findings.len(), seed));
        }
        let mut finding = inferred_finding(found, level, !run.is_drift);
        if run.is_drift {
            finding
                .message
                .push_str(" (in the code, not yet in its committed document)");
        }
        findings.push(finding);
    }
    Ok(Some((distinct_calls(&report), walkable)))
}

/// Verifies the manifest inference made for `run`: the calls code makes to
/// documented operations, and what the change breaks in them.
fn verify_inferred<E: Engine>(
    inferred: &infer::Inferred,
    run: &Inference<'_>,
    engine: &E,
) -> Result<lemma_verify::Report, GraphError> {
    let (base, head) = (run.base, run.head);
    let options = lemma_verify::Options {
        explain: run.explain,
        relations: affects::TRACED_RELATIONS
            .map(str::to_owned)
            .into_iter()
            .chain(run.relations.iter().cloned())
            .collect(),
        facts_cache: run.facts_cache.cloned(),
        // Tracing asks for the calls of further operations as it reaches
        // them (see `affects::CallSource`).
        evaluation: run.evaluation,
        warm: run.warm.clone(),
        also_keep: run
            .facts_cache
            .map(|cache| affects::keep_functions_too(cache.build.clone())),
    };
    log::info!(
        "inferring links from code: {} changed {}..{}, {} evaluation",
        run.changed,
        short(base),
        head.map_or_else(|| "working directory".to_owned(), short),
        match run.evaluation {
            lemma_verify::Evaluation::Full => "full",
            lemma_verify::Evaluation::Demand => "change-driven",
        }
    );
    let started = std::time::Instant::now();
    let report =
        lemma_verify::verify_with_listings(&inferred.manifest, engine, &options, run.listings)
            .map_err(|source| GraphError::Inference { source })?;
    log::info!(
        "inference done in {:.1}s: {} findings",
        started.elapsed().as_secs_f64(),
        report.findings.len()
    );
    Ok(report)
}

/// How many distinct caller and operation pairs inference found, of the
/// operations it read every call of: calls read only as context (from a
/// file a caller imports, say) are not counted.
fn distinct_calls(report: &lemma_verify::Report) -> usize {
    let covered: Option<BTreeSet<&String>> = report
        .covered
        .as_ref()
        .map(|covered| covered.iter().collect());
    let calls: BTreeSet<(&String, &String)> = report
        .relations
        .get("calls")
        .into_iter()
        .flatten()
        .filter_map(|row| Some((row.first()?, row.get(1)?)))
        .filter(|(_, operation)| {
            covered
                .as_ref()
                .is_none_or(|covered| covered.contains(operation))
        })
        .collect();
    calls.len()
}

/// Gives each service whose operations come from its code the documents its
/// adapter generates, laid over its committed documents: at the change's
/// two snapshots for the changed repository (only the head, the working
/// directory, when comparing against committed documents), at its one
/// checkout for any other. An adapter that fails, or reads the code only in
/// part, leaves the committed documents where it could not read and adds a
/// warning, so a check never passes for want of a contract. Whether the
/// changed repository's code describes anything its committed documents do
/// not: a changed or removed operation.
/// The snapshots verification of `manifest` will list; none when the
/// inference may stop after its adapters, as a check without a change
/// does when they find nothing.
fn snapshots_to_list(
    manifest: &lemma_verify::Manifest,
    may_stop: bool,
) -> Vec<(PathBuf, Option<String>)> {
    if may_stop {
        return Vec::new();
    }
    lemma_verify::snapshot_listings(manifest)
}

/// [`apply_adapters`], listing `snapshots` while they run: verification
/// reads those listings first, and they need nothing adapters produce.
fn adapters_beside_listings(
    inferred: &mut infer::Inferred,
    repos: &BTreeMap<String, Repo>,
    run: &Inference<'_>,
    findings: &mut Vec<Finding>,
    snapshots: &[(PathBuf, Option<String>)],
) -> bool {
    std::thread::scope(|scope| {
        scope.spawn(|| run.listings.prefetch(snapshots));
        apply_adapters(inferred, repos, run, findings)
    })
}

fn apply_adapters(
    inferred: &mut infer::Inferred,
    repos: &BTreeMap<String, Repo>,
    run: &Inference<'_>,
    findings: &mut Vec<Finding>,
) -> bool {
    let jobs = adapter_jobs(inferred, repos, run);
    let operations = run.facts_cache.map(lemma_verify::FactsCache::documents);
    let results = run_adapter_jobs(&jobs, run.cache, operations.as_deref());
    let mut has_drift = false;
    for (job, result) in jobs.iter().zip(results) {
        let Some(repository) = inferred.manifest.repositories.get_mut(&job.entry) else {
            continue;
        };
        match result {
            Ok(Some(contract)) => {
                let coverage = &contract.coverage;
                if job.is_changed && (!coverage.changed.is_empty() || !coverage.removed.is_empty())
                {
                    has_drift = true;
                }
                for warning in contract_warnings(job, &contract) {
                    // The same gap at both snapshots is one warning.
                    push_once(findings, warning);
                }
                repository
                    .generated
                    .insert(job.role.to_owned(), contract.documents);
            }
            // A pinned repository nobody has read the code of: its
            // committed documents are its contract.
            Ok(None) => {}
            Err(error) => {
                let at = job
                    .checkout
                    .commit
                    .as_deref()
                    .unwrap_or("working directory");
                findings.push(adapter_warning(
                    &job.entry,
                    format!(
                        "could not read {}'s operations from its code ({at}), so its committed documents were checked: {error}",
                        job.service
                    ),
                ));
            }
        }
    }
    has_drift
}

/// One adapter run an inference needs: a service's code at one snapshot.
struct AdapterJob<'r> {
    entry: String,
    service: String,
    role: &'static str,
    is_changed: bool,
    checkout: &'r Checkout,
    build: adapters::Build,
    from: &'r crate::config::OperationsFrom,
    /// The service's runtime, with the asking process's environment under
    /// its own variables when a check is answered on its behalf.
    runtime: std::borrow::Cow<'r, crate::config::Runtime>,
    documents: Vec<String>,
}

fn adapter_jobs<'r>(
    inferred: &infer::Inferred,
    repos: &'r BTreeMap<String, Repo>,
    run: &Inference<'_>,
) -> Vec<AdapterJob<'r>> {
    let mut jobs = Vec::new();
    for (entry, repository) in &inferred.manifest.repositories {
        if repository.role != lemma_verify::Role::Service {
            continue;
        }
        let Some(repo_name) = inferred.repositories.get(entry) else {
            continue;
        };
        let service_name = entry
            .rsplit_once(':')
            .map_or(entry.as_str(), |(_, service)| service);
        let repo = &repos[repo_name];
        let Some(service) = repo.files[HEAD].services.get(service_name) else {
            continue;
        };
        let Some(from) = &service.exports.operations_from else {
            continue;
        };
        for (role, checkout, build) in adapter_snapshots(repo, repo_name, run.changed, run.is_drift)
        {
            jobs.push(AdapterJob {
                entry: entry.clone(),
                service: service_name.to_owned(),
                role,
                is_changed: repo_name == run.changed,
                checkout,
                build,
                from,
                runtime: runtime_for(&service.runtime, run.environment.as_deref()),
                documents: repository.specs.clone(),
            });
        }
    }
    jobs
}

/// Runs ahead of inference the adapters it will ask for, so that their
/// commands overlap other work and inference finds their documents and
/// contracts kept. Their outcomes are dropped on purpose: inference asks
/// again, reads what was kept, and reports a failure itself.
fn prefetch_adapters(repos: &BTreeMap<String, Repo>, options: &CheckOptions) {
    let jobs = prefetch_jobs(repos, options);
    if jobs.is_empty() {
        return;
    }
    let started = std::time::Instant::now();
    let operations = options
        .facts_cache
        .as_ref()
        .map(lemma_verify::FactsCache::documents);
    let ran = run_adapter_jobs(&jobs, &options.cache, operations.as_deref());
    log::debug!(
        "adapter: {} runs prefetched in {:.0?}",
        ran.len(),
        started.elapsed()
    );
}

/// The adapter runs inference will ask for, found from the descriptions
/// alone: what [`adapter_jobs`] lists once inference names the services.
/// None for a list of links, which reads every repository at head anyway,
/// or for a change whose repository has no base commit to compare.
fn prefetch_jobs<'r>(
    repos: &'r BTreeMap<String, Repo>,
    options: &'r CheckOptions,
) -> Vec<AdapterJob<'r>> {
    let (changed, is_drift) = match &options.change {
        Some(change) => {
            let has_base = repos
                .get(&change.repository)
                .is_some_and(|repo| repo.checkouts[BASE].commit.is_some());
            if !has_base {
                return Vec::new();
            }
            (change.repository.as_str(), false)
        }
        None if options.list_links => return Vec::new(),
        // Without a change every adapter reads its repository at head.
        None => ("", true),
    };
    let mut jobs = Vec::new();
    for (repo_name, repo) in repos {
        for (service_name, service) in &repo.files[HEAD].services {
            let Some(from) = &service.exports.operations_from else {
                continue;
            };
            for (role, checkout, build) in adapter_snapshots(repo, repo_name, changed, is_drift) {
                jobs.push(AdapterJob {
                    entry: crate::infer::entry_name(repo_name, service_name),
                    service: service_name.clone(),
                    role,
                    is_changed: repo_name == changed,
                    checkout,
                    build,
                    from,
                    runtime: runtime_for(&service.runtime, options.environment.as_deref()),
                    documents: service.exports.openapi.clone(),
                });
            }
        }
    }
    jobs
}

/// `runtime`, or, given the environment of the process a check is answered
/// for, a copy whose variables lie over that environment's: adapters then
/// run as they would have in that process.
fn runtime_for<'r>(
    runtime: &'r crate::config::Runtime,
    environment: Option<&BTreeMap<String, String>>,
) -> std::borrow::Cow<'r, crate::config::Runtime> {
    let Some(environment) = environment else {
        return std::borrow::Cow::Borrowed(runtime);
    };
    let mut env = environment.clone();
    env.extend(runtime.env.clone());
    std::borrow::Cow::Owned(crate::config::Runtime {
        env,
        ..runtime.clone()
    })
}

/// Runs every job, the jobs of one repository in turn (they share its
/// build worktree) and repositories side by side; results in job order.
/// `operations` is where verification keeps documents' operations.
fn run_adapter_jobs(
    jobs: &[AdapterJob<'_>],
    cache: &Path,
    operations: Option<&Path>,
) -> Vec<Result<Option<adapters::Contract>, GraphError>> {
    let mut by_repository: BTreeMap<&Path, Vec<usize>> = BTreeMap::new();
    for (index, job) in jobs.iter().enumerate() {
        by_repository
            .entry(job.checkout.root.as_path())
            .or_default()
            .push(index);
    }
    let mut results: Vec<Option<Result<Option<adapters::Contract>, GraphError>>> =
        jobs.iter().map(|_| None).collect();
    std::thread::scope(|scope| {
        let handles: Vec<_> = by_repository
            .values()
            .map(|indices| {
                scope.spawn(move || {
                    indices
                        .iter()
                        .map(|&index| (index, adapter_contract(&jobs[index], cache, operations)))
                        .collect::<Vec<_>>()
                })
            })
            .collect();
        for handle in handles {
            let Ok(done) = handle.join() else {
                continue;
            };
            for (index, result) in done {
                results[index] = Some(result);
            }
        }
    });
    results
        .into_iter()
        .map(|result| {
            result.unwrap_or_else(|| {
                Err(GraphError::Adapter {
                    message: "the adapter's run stopped unexpectedly".to_owned(),
                })
            })
        })
        .collect()
}

fn adapter_contract(
    job: &AdapterJob<'_>,
    cache: &Path,
    operations: Option<&Path>,
) -> Result<Option<adapters::Contract>, GraphError> {
    log::info!(
        "adapter: reading the operations of {} ({}) from its code",
        job.entry,
        job.role
    );
    let Some(generated) =
        adapters::generate(job.from, &job.runtime, job.checkout, cache, job.build)?
    else {
        return Ok(None);
    };
    // A contract is laid from the adapter's document and the committed
    // ones, so the same inputs give the contract already kept; a document
    // patched for a change gives the base's contract patched the same way.
    let digests = adapters::committed_digests(job.checkout, &job.documents, cache)?;
    let key = adapters::contract_key(&job.service, &generated.digest, &digests);
    if let Some(kept) = adapters::kept_contract(cache, &key) {
        log::info!(
            "adapter: {} ({}): contract kept for these documents",
            job.entry,
            job.role
        );
        return Ok(Some(kept));
    }
    let patched = adapters::patched_contract(&job.service, &generated, &digests, cache, operations);
    if let Some(patched) = patched {
        adapters::keep_contract(cache, &key, &patched);
        log::info!(
            "adapter: {} ({}): contract patched from the base's",
            job.entry,
            job.role
        );
        return Ok(Some(patched));
    }
    let texts = adapters::committed_texts(job.checkout, &job.documents)?;
    let committed = adapters::parse_committed(&texts)?;
    let contract = adapters::contract(&job.service, &generated, &committed, cache)?;
    adapters::keep_contract(cache, &key, &contract);
    Ok(Some(contract))
}

/// What a contract's reading leaves for a person to know: code it could
/// not read, and code that matched too few committed operations to trust.
fn contract_warnings(job: &AdapterJob<'_>, contract: &adapters::Contract) -> Vec<Finding> {
    let service = &job.service;
    let mut warnings = Vec::new();
    if let Some(first) = contract.unread.first() {
        let count = contract.unread.len();
        warnings.push(adapter_warning(
            &job.entry,
            format!(
                "{service}'s code was not read whole: {count} modules or routes could not be, so their operations were checked as committed; first: {first}"
            ),
        ));
    }
    let coverage = &contract.coverage;
    if !coverage.is_trusted && coverage.committed > 0 {
        warnings.push(adapter_warning(
            &job.entry,
            format!(
                "{service}'s code describes {} operations, matching only {} of its {} committed ones, so only those were checked as the code has them; is `entry` right, or are routes served under a base path the adapter does not see?",
                coverage.described, coverage.matched, coverage.committed
            ),
        ));
    }
    warnings
}

/// The snapshots an adapter reads a repository's code at, by the role they
/// play, and whether a commit may be built: the change's two snapshots for
/// the changed repository (only the head, the working directory, when
/// comparing against committed documents), and its one checkout for any
/// other. Only the changed repository is built; another repository's code
/// is read where it is checked out, or from a document kept for its commit,
/// and never runs its `setup`.
fn adapter_snapshots<'r>(
    repo: &'r Repo,
    repo_name: &str,
    changed: &str,
    is_drift: bool,
) -> Vec<(&'static str, &'r Checkout, adapters::Build)> {
    if repo_name != changed {
        return vec![("both", &repo.checkouts[HEAD], adapters::Build::KeptOnly)];
    }
    if is_drift {
        return vec![("head", &repo.checkouts[HEAD], adapters::Build::KeptOnly)];
    }
    vec![
        ("base", &repo.checkouts[BASE], adapters::Build::Allowed),
        ("head", &repo.checkouts[HEAD], adapters::Build::Allowed),
    ]
}

/// A warning for each service document Lemma cannot read operations from:
/// Swagger 2.0, which describes no operation Lemma would check, so calls to
/// it would pass unchecked.
fn unsupported_documents(repos: &BTreeMap<String, Repo>) -> Result<Vec<Finding>, GraphError> {
    let documents = repos.iter().flat_map(|(name, repo)| {
        repo.files[HEAD]
            .services
            .iter()
            .flat_map(move |(service, found)| {
                found
                    .exports
                    .openapi
                    .iter()
                    .map(move |document| (name, repo, service, document))
            })
    });
    let mut warnings = Vec::new();
    for (name, repo, service, document) in documents {
        if !is_swagger(&repo.checkouts[HEAD], document)? {
            continue;
        }
        warnings.push(Finding {
            severity: Severity::Warning,
            link: format!("{name}:{service}"),
            check: "document".to_owned(),
            message: format!(
                "{document} is Swagger 2.0, which Lemma does not read, so calls to its operations are not checked; convert it to OpenAPI 3"
            ),
            introduced: false,
            affects: Vec::new(),
            allowed: None,
            values: BTreeMap::new(),
            derivation: None,
            description_name: None,
        });
    }
    Ok(warnings)
}

/// Whether a document is Swagger 2.0; its start is looked at before the
/// whole is parsed.
fn is_swagger(checkout: &Checkout, document: &str) -> Result<bool, GraphError> {
    let Some(text) = checkout.read(document)? else {
        return Ok(false);
    };
    let start: String = text.chars().take(400).collect();
    if !start.contains("swagger") {
        return Ok(false);
    }
    Ok(adapters::parse_document(document, &text)
        .is_ok_and(|parsed| parsed.get("swagger").is_some()))
}

/// Adds `finding` unless an identical one is there.
fn push_once(findings: &mut Vec<Finding>, finding: Finding) {
    if !findings.contains(&finding) {
        findings.push(finding);
    }
}

/// At level `strict`, code an adapter or schema resolver could not read,
/// whole or in part, is an error: its committed documents stood in for it, so a change only the
/// code shows (in CI without the dependencies the adapter needs, say)
/// would otherwise pass.
fn fail_unread_contracts(level: Level, findings: &mut [Finding]) {
    if !matches!(level, Level::Strict) {
        return;
    }
    for finding in findings
        .iter_mut()
        .filter(|finding| matches!(finding.check.as_str(), "adapter" | "resolver"))
    {
        finding.severity = Severity::Error;
        finding.introduced = true;
    }
}

/// A warning that a service's contract was not fully read from its code.
fn adapter_warning(entry: &str, message: String) -> Finding {
    Finding {
        severity: Severity::Warning,
        link: entry.to_owned(),
        check: "adapter".to_owned(),
        message,
        introduced: false,
        affects: Vec::new(),
        allowed: None,
        values: BTreeMap::new(),

        derivation: None,

        description_name: None,
    }
}

/// A repository's the description as inference reads it. One borrowed from the
/// working directory for a commit that had none names documents as they are
/// today; those missing at both of the repository's checkouts are dropped
/// rather than failing the check. A description from the commit itself is
/// taken as written, so a misspelt document is still an error.
pub(crate) fn inference_file(repo: &Repo) -> Result<LemmaFile, GraphError> {
    let mut file = repo.files[HEAD].clone();
    if !repo.manifest_from_working_directory {
        return Ok(file);
    }
    for service in file.services.values_mut() {
        retain_existing(&mut service.exports.openapi, &repo.checkouts)?;
        if let Some(client) = &mut service.exports.client {
            retain_existing(&mut client.generated_from, &repo.checkouts)?;
            retain_existing(&mut client.specs, &repo.checkouts)?;
        }
    }
    Ok(file)
}

fn retain_existing(
    documents: &mut Vec<String>,
    checkouts: &[Checkout; 2],
) -> Result<(), GraphError> {
    let mut kept = Vec::new();
    for document in documents.drain(..) {
        let mut exists = false;
        for checkout in checkouts {
            exists = exists || checkout.has(&document)?;
        }
        if exists {
            kept.push(document);
        }
    }
    *documents = kept;
    Ok(())
}

/// A grammar finding as a finding: its errors are problems, which the level
/// makes warnings or errors. With a change, as for declared links, only
/// what the change introduced counts; what was already there is
/// informational.
fn inferred_finding(found: &lemma_verify::Finding, level: Level, has_change: bool) -> Finding {
    let counts = found.introduced || !has_change;
    let severity = match (found.severity, level) {
        _ if !counts => Severity::Info,
        ("error", Level::Check | Level::Strict) => Severity::Error,
        ("error" | "warning", _) => Severity::Warning,
        _ => Severity::Info,
    };
    let link = match (found.values.get("c"), found.values.get("op")) {
        (Some(caller), Some(operation)) => format!("{caller} -> {operation}"),
        _ => found.requirement.clone(),
    };
    Finding {
        severity,
        link,
        check: found.requirement.clone(),
        message: found.message.clone(),
        introduced: found.introduced,
        affects: Vec::new(),
        allowed: None,
        values: found.values.clone(),
        derivation: found.derivation.clone(),
        description_name: None,
    }
}

fn resolved(repos: &BTreeMap<String, Repo>) -> Vec<ResolvedRepository> {
    repos
        .iter()
        .map(|(name, repo)| ResolvedRepository {
            name: name.clone(),
            root: repo.checkouts[HEAD].root.to_string_lossy().into_owned(),
            base: repo.checkouts[BASE].commit.clone(),
            head: repo.checkouts[HEAD].commit.clone(),
            manifest_from_working_directory: repo.manifest_from_working_directory,
        })
        .collect()
}

/// The parent and every repository reachable through `#name` services,
/// with the operations their `OpenAPI` documents declare added to their
/// services, so links can name them.
pub(crate) fn load(
    root_file: &Path,
    options: &CheckOptions,
) -> Result<BTreeMap<String, Repo>, GraphError> {
    let mut repos = load_manifests(root_file, options)?;
    let mut documents = Documents {
        cache: &options.cache,
        warm: options.warm.as_deref(),
        declared: BTreeMap::new(),
    };
    for repo in repos.values_mut() {
        for (file, checkout) in repo.files.iter_mut().zip(&repo.checkouts) {
            add_documented_operations(file, checkout, &mut documents)?;
        }
    }
    for (owner, service, route) in routes_named(&repos) {
        if let Some(repo) = repos.get_mut(&owner) {
            add_routed_operation(repo, &service, &route)?;
        }
    }
    Ok(repos)
}

/// The parent and every repository reachable through `#name` services,
/// with their manifests as written. Reads no `OpenAPI` document, which can
/// run to megabytes, so a workspace can list its members on every query.
pub(crate) fn load_manifests(
    root_file: &Path,
    options: &CheckOptions,
) -> Result<BTreeMap<String, Repo>, GraphError> {
    let directory = absolute(root_file.parent().unwrap_or(Path::new(".")));
    let manifest = root_file.file_name().map_or_else(
        || MODULE_FILE.to_owned(),
        |name| name.to_string_lossy().into_owned(),
    );
    let mut repos = BTreeMap::new();
    let mut pending = VecDeque::from([(SELF.to_owned(), directory, None, manifest)]);
    // Where each repository a file named first is, so a `.lm` module that
    // imports it resolves it as the composition pins it.
    let mut pins: BTreeMap<String, Checkout> = BTreeMap::new();
    while let Some((name, root, reference, manifest)) = pending.pop_front() {
        if repos.contains_key(&name) {
            continue;
        }
        let checkouts = checkouts(&name, &root, reference.as_deref(), options)?;
        let reading = Reading {
            name: &name,
            manifest: &manifest,
            cache: &options.cache,
            pins: &pins,
            cloning: options.cloning,
        };
        let (base_file, base_fallback) = read_manifest(&reading, &checkouts[BASE])?;
        // A repository the change is not in is read at one checkout for
        // both snapshots; checking its program again gives the same file.
        let (head_file, head_fallback) = if checkouts[BASE] == checkouts[HEAD] {
            (base_file.clone(), base_fallback)
        } else {
            read_manifest(&reading, &checkouts[HEAD])?
        };
        let files = [base_file, head_file];
        let manifest_from_working_directory = base_fallback || head_fallback;
        let base_directory = root.join(Path::new(&manifest).parent().unwrap_or(Path::new("")));
        let mut unavailable = BTreeSet::new();
        for (external, service) in files.iter().flat_map(external_services) {
            if let Some(missing) = unavailable_external(external, service, options) {
                unavailable.insert(missing);
                continue;
            }
            let source = external_root(service, &base_directory, &options.cache)?;
            let pinned = crate::lm::repository_name(external.trim_start_matches('#'));
            pins.entry(pinned).or_insert_with(|| Checkout {
                root: source.root.clone(),
                commit: source.reference.clone(),
            });
            pending.push_back(source.with_name(external));
        }
        repos.insert(
            name,
            Repo {
                checkouts,
                files,
                manifest_from_working_directory,
                unavailable,
            },
        );
    }
    Ok(repos)
}

/// Every operation a link names by its method and path, as its repository,
/// service and route.
fn routes_named(repos: &BTreeMap<String, Repo>) -> BTreeSet<(String, String, String)> {
    let mut named = BTreeSet::new();
    for (name, repo) in repos {
        // A malformed link is reported where links are collected.
        let ends = repo
            .files
            .iter()
            .flat_map(|file| &file.graph.links)
            .filter_map(|written| parse_link(written).ok())
            .flat_map(|link| [link.caller, link.provider]);
        for end in ends.filter(|end| crate::address::is_route(&end.operation)) {
            let owner = end.repository.unwrap_or_else(|| name.clone());
            named.insert((owner, end.service, end.operation));
        }
    }
    named
}

/// Adds the operation a service's `OpenAPI` documents declare for `route`
/// (`GET /v1/charges/{}`) under that name, at both snapshots, so a link
/// naming it is checked as one naming an `operationId` is.
fn add_routed_operation(repo: &mut Repo, service: &str, route: &str) -> Result<(), GraphError> {
    let Some((method, path)) = route.split_once(' ') else {
        return Ok(());
    };
    for (file, checkout) in repo.files.iter_mut().zip(&repo.checkouts) {
        let Some(declared) = file.services.get_mut(service) else {
            continue;
        };
        if declared.exports.operations.contains_key(route) {
            continue;
        }
        for document_file in declared.exports.openapi.clone() {
            let Some(text) = checkout.read(&document_file)? else {
                continue;
            };
            let document: serde_json::Value =
                serde_json::from_str(&text).map_err(|source| GraphError::ResolverOutput {
                    command: format!("reading {document_file}"),
                    source,
                })?;
            let found = crate::openapi::operation_at(&document_file, &document, method, path);
            if let Some(operation) = found {
                declared
                    .exports
                    .operations
                    .insert(route.to_owned(), operation);
                break;
            }
        }
    }
    Ok(())
}

/// A directory as an absolute path, so commands run elsewhere (in a
/// worktree) still find files given relative to it. Unchanged if it does
/// not exist; reading it then reports the problem.
fn absolute(path: &Path) -> PathBuf {
    let path = if path.as_os_str().is_empty() {
        Path::new(".")
    } else {
        path
    };
    std::fs::canonicalize(path).unwrap_or_else(|_| path.to_path_buf())
}

/// Where an external service is, before its commit is resolved.
struct ExternalSource {
    root: PathBuf,
    reference: Option<String>,
    manifest: String,
}

impl ExternalSource {
    fn with_name(self, name: &str) -> (String, PathBuf, Option<String>, String) {
        (name.to_owned(), self.root, self.reference, self.manifest)
    }
}

fn external_root(
    service: &Service,
    directory: &Path,
    cache: &Path,
) -> Result<ExternalSource, GraphError> {
    let root = match (&service.git, &service.path) {
        (Some(url), _) => clone_remote(url, service.reference.as_deref(), cache)?,
        (None, Some(path)) => absolute(&directory.join(path)),
        (None, None) => directory.to_path_buf(),
    };
    let reference = match (&service.git, &service.reference) {
        (_, Some(reference)) => Some(reference.clone()),
        (Some(_), None) => Some("HEAD".to_owned()),
        (None, None) => None,
    };
    Ok(ExternalSource {
        root,
        reference,
        manifest: MODULE_FILE.to_owned(),
    })
}

/// The checkouts a repository is read at: the change's commits if it is the
/// changed repository, otherwise its pin (or working directory) for both.
fn checkouts(
    name: &str,
    root: &Path,
    reference: Option<&str>,
    options: &CheckOptions,
) -> Result<[Checkout; 2], GraphError> {
    let at = |commit: Option<String>| Checkout {
        root: root.to_path_buf(),
        commit,
    };
    if let Some(change) = options
        .change
        .as_ref()
        .filter(|change| change.repository == name)
    {
        let head = change
            .head
            .as_deref()
            .map(|head| resolve_commit(root, head))
            .transpose()?;
        let base = match &change.base {
            Some(base) => resolve_commit(root, base)?,
            None => fork_point(root)?,
        };
        return Ok([at(Some(base)), at(head)]);
    }
    let commit = reference
        .map(|reference| resolve_commit(root, reference))
        .transpose()?;
    Ok([at(commit.clone()), at(commit)])
}

/// A repository's the description at a checkout, falling back to the working
/// directory's when the commit has none; the flag says whether it did.
fn read_manifest(reading: &Reading, checkout: &Checkout) -> Result<(LemmaFile, bool), GraphError> {
    read_language_manifest(reading, checkout, reading.manifest)
}

/// What reading one repository's manifest needs: its name, the manifest's
/// file, the cache, and the repositories the composition pinned so far.
struct Reading<'r> {
    name: &'r str,
    manifest: &'r str,
    cache: &'r Path,
    pins: &'r BTreeMap<String, Checkout>,
    cloning: Cloning,
}

/// A repository's `.lm` manifest at a checkout, falling back to the working
/// directory's when the commit has none.
fn read_language_manifest(
    reading: &Reading,
    checkout: &Checkout,
    manifest: &str,
) -> Result<(LemmaFile, bool), GraphError> {
    let (name, cache, pins, cloning) = (reading.name, reading.cache, reading.pins, reading.cloning);
    if checkout.read(manifest)?.is_some() {
        return Ok((
            crate::lm::read_manifest(checkout, manifest, cache, pins, cloning)?,
            false,
        ));
    }
    let working = Checkout {
        root: checkout.root.clone(),
        commit: None,
    };
    if working.read(manifest)?.is_none() {
        return Err(GraphError::MissingManifest {
            name: name.to_owned(),
            path: manifest.to_owned(),
            root: checkout.root.clone(),
            commit: checkout.commit.clone(),
        });
    }
    let file = crate::lm::read_manifest(&working, manifest, cache, pins, cloning)?;
    Ok((file, checkout.commit.is_some()))
}

/// Adds every operation a local service's `OpenAPI` documents declare, at
/// the checkout, to its operations; one declared by name in a description
/// wins. A document missing at the checkout adds nothing.
fn add_documented_operations(
    file: &mut LemmaFile,
    checkout: &Checkout,
    documents: &mut Documents<'_>,
) -> Result<(), GraphError> {
    for (name, service) in &mut file.services {
        if name.starts_with('#') {
            continue;
        }
        let digests =
            adapters::committed_digests(checkout, &service.exports.openapi, documents.cache)?;
        for (document_file, digest) in digests {
            let documented = documents.operations(checkout, &document_file, &digest)?;
            for (id, operation) in documented.iter() {
                service
                    .exports
                    .operations
                    .entry(id.clone())
                    .or_insert_with(|| operation.clone());
            }
        }
    }
    Ok(())
}

/// Where loading finds what services' documents declare, by each
/// document's contents: documents run to megabytes, every repository is
/// read at both of a change's snapshots, and documents seldom change, so a
/// document is parsed once and its declarations kept for later runs.
struct Documents<'a> {
    cache: &'a Path,
    warm: Option<&'a crate::Warm>,
    /// Declarations met in this load, by the document's digest.
    declared: BTreeMap<String, Arc<Vec<crate::openapi::Declaration>>>,
}

/// Where declarations are kept between runs; named for their format.
const DECLARATIONS: &str = "declarations-1";

impl Documents<'_> {
    /// The operations the document `document_file`, whose contents have
    /// `digest`, declares at `checkout`.
    fn operations(
        &mut self,
        checkout: &Checkout,
        document_file: &str,
        digest: &str,
    ) -> Result<crate::warm::Documented, GraphError> {
        let key = (document_file.to_owned(), digest.to_owned());
        if let Some(found) = self.warm.and_then(|warm| warm.documented(&key)) {
            return Ok(found);
        }
        let declarations = self.declarations(checkout, document_file, digest)?;
        let operations = Arc::new(crate::openapi::declared_operations(
            document_file,
            &declarations,
        ));
        if let Some(warm) = self.warm {
            warm.keep_documented(key, Arc::clone(&operations));
        }
        Ok(operations)
    }

    /// What the document declares: met already, kept by an earlier run, or
    /// read and parsed now and kept.
    fn declarations(
        &mut self,
        checkout: &Checkout,
        document_file: &str,
        digest: &str,
    ) -> Result<Arc<Vec<crate::openapi::Declaration>>, GraphError> {
        if let Some(found) = self.declared.get(digest) {
            return Ok(Arc::clone(found));
        }
        let kept = self.cache.join(DECLARATIONS).join(format!("{digest}.json"));
        let found = if let Some(found) = read_kept_declarations(&kept) {
            found
        } else {
            let parsed = parse_declarations(checkout, document_file)?;
            keep_declarations(&kept, &parsed);
            parsed
        };
        let found = Arc::new(found);
        self.declared.insert(digest.to_owned(), Arc::clone(&found));
        Ok(found)
    }
}

/// Declarations an earlier run kept at `path`, if it kept them.
fn read_kept_declarations(path: &Path) -> Option<Vec<crate::openapi::Declaration>> {
    let text = std::fs::read_to_string(path).ok()?;
    serde_json::from_str(&text).ok()
}

/// Keeps `declarations` at `path`; a failure only means parsing again.
fn keep_declarations(path: &Path, declarations: &[crate::openapi::Declaration]) {
    let Ok(text) = serde_json::to_string(declarations) else {
        return;
    };
    let partial = path.with_extension(format!("{}.partial", std::process::id()));
    let _ = path
        .parent()
        .map_or(Ok(()), std::fs::create_dir_all)
        .and_then(|()| std::fs::write(&partial, text))
        .and_then(|()| std::fs::rename(&partial, path));
}

/// What the document `document_file` at `checkout` declares, parsed; none
/// if the checkout has no such file.
fn parse_declarations(
    checkout: &Checkout,
    document_file: &str,
) -> Result<Vec<crate::openapi::Declaration>, GraphError> {
    let Some(text) = checkout.read(document_file)? else {
        return Ok(Vec::new());
    };
    let document: serde_json::Value =
        serde_json::from_str(&text).map_err(|source| GraphError::ResolverOutput {
            command: format!("reading {document_file}"),
            source,
        })?;
    Ok(crate::openapi::declarations(&document))
}

fn external_services(file: &LemmaFile) -> impl Iterator<Item = (&String, &Service)> {
    file.services.iter().filter(|(key, _)| key.starts_with('#'))
}

/// Every repository named somewhere in `repos` that is not here.
pub(crate) fn unavailable_repositories(
    repos: &BTreeMap<String, Repo>,
) -> BTreeSet<UnavailableRepository> {
    repos
        .values()
        .flat_map(|repo| repo.unavailable.iter().cloned())
        .collect()
}

/// `service`, an external service called `name`, if it is a `git` repository
/// that is not cloned and may not be.
fn unavailable_external(
    name: &str,
    service: &Service,
    options: &CheckOptions,
) -> Option<UnavailableRepository> {
    let url = service.git.as_ref()?;
    if options.cloning == Cloning::Allowed
        || is_cloned(url, service.reference.as_deref(), &options.cache)
    {
        return None;
    }
    Some(UnavailableRepository {
        name: name.to_owned(),
        url: url.clone(),
        reference: service.reference.clone(),
    })
}

/// `links` split into those that can be checked, and how many reach each
/// repository that is not here.
fn split_unchecked_links(
    links: BTreeSet<Link>,
    repos: &BTreeMap<String, Repo>,
) -> (BTreeSet<Link>, Vec<NotChecked>) {
    let unavailable = unavailable_repositories(repos);
    let reaches = |link: &Link, name: &str| {
        [&link.caller, &link.provider]
            .iter()
            .any(|end| end.repository.as_deref() == Some(name))
    };
    let not_checked = unavailable
        .iter()
        .map(|missing| NotChecked {
            repository: missing.name.clone(),
            links: links
                .iter()
                .filter(|link| reaches(link, &missing.name))
                .count(),
        })
        .collect();
    let checkable = links
        .into_iter()
        .filter(|link| {
            !unavailable
                .iter()
                .any(|missing| reaches(link, &missing.name))
        })
        .collect();
    (checkable, not_checked)
}

/// Every link in every repository's `graph` at head, with addresses made
/// absolute (each names its repository).
fn collect_links(repos: &BTreeMap<String, Repo>) -> Result<BTreeSet<Link>, GraphError> {
    let mut links = BTreeSet::new();
    for (name, repo) in repos {
        for text in &repo.files[HEAD].graph.links {
            let mut link = parse_link(text)?;
            for address in [&mut link.caller, &mut link.provider] {
                address.repository.get_or_insert_with(|| name.clone());
            }
            links.insert(link);
        }
    }
    Ok(links)
}

/// Which way a message travels.
#[derive(Debug, Clone, Copy)]
enum Part {
    Request,
    Response,
}

impl Part {
    fn name(self) -> &'static str {
        match self {
            Part::Request => "request",
            Part::Response => "response",
        }
    }
}

/// An operation as found at one snapshot.
struct Found<'a> {
    repo: &'a Repo,
    service: &'a Service,
    operation: &'a Operation,
}

struct Checker<'a> {
    repos: &'a BTreeMap<String, Repo>,
    level: Level,
    cache: &'a Path,
    extractor: Extractor,
    outputs: crate::warm::Outputs,
    reads: schema::Reads,
    findings: Vec<Finding>,
    /// Whether a change is being checked: then only what it introduces is a
    /// problem, and what was already there is context.
    has_change: bool,
    /// The environment commands start from instead of this process's own.
    environment: Option<Arc<BTreeMap<String, String>>>,
    /// Schemas a resolver could not produce, by its command and directory:
    /// one resolver that cannot run here leaves every schema it serves
    /// unread, and that is one thing to fix, so one warning.
    unread: BTreeMap<(String, PathBuf), Unread>,
}

/// What one resolver that could not run left unread.
struct Unread {
    /// The first link whose schema it was asked for.
    link: String,
    /// Why it failed.
    error: String,
    /// Each schema it left unread, such as `#billing:services.api.charge's request`.
    schemas: Vec<String>,
}

impl Unread {
    /// The warning: a resolver that cannot run here fails a strict check
    /// only (see `fail_unread_contracts`).
    fn into_finding(self) -> Finding {
        let Self {
            link,
            error,
            schemas,
        } = self;
        let first = schemas.first().map_or("", String::as_str);
        let message = match schemas.len() {
            1 => format!("could not resolve {first} schema: {error}"),
            count => format!("could not resolve {count} schemas, such as {first} schema: {error}"),
        };
        Finding {
            severity: Severity::Warning,
            link,
            check: "resolver".to_owned(),
            message,
            introduced: false,
            affects: Vec::new(),
            allowed: None,
            values: BTreeMap::new(),
            derivation: None,
            description_name: None,
        }
    }
}

/// A finding before its severity is decided.
struct Note {
    description_name: Option<&'static str>,
    check: &'static str,
    message: String,
    introduced: bool,
    /// Whether it is a problem (a warning or error) rather than context.
    problem: bool,
}

impl Note {
    fn problem(check: &'static str, message: String, introduced: bool) -> Self {
        Note {
            description_name: None,
            check,
            message,
            introduced,
            problem: true,
        }
    }

    /// The same note, reported by `lemma verify` under `name`.
    fn named(mut self, name: &'static str) -> Self {
        self.description_name = Some(name);
        self
    }
}

/// One schema to resolve: which part, at which snapshot, for which side.
#[derive(Debug, Clone, Copy)]
struct Want {
    part: Part,
    snapshot: usize,
    role: Role,
}

impl Checker<'_> {
    fn check_link(&mut self, link: &Link) -> Result<(), GraphError> {
        let started = std::time::Instant::now();
        let label = format!("{} -> {}", link.caller, link.provider);
        let caller_ok = self.persists(&label, "caller", &link.caller)?;
        let provider_ok = self.persists(&label, "provider", &link.provider)?;
        let anchored = started.elapsed();
        if caller_ok && provider_ok {
            for part in [Part::Request, Part::Response] {
                self.compare_part(&label, link, part)?;
            }
        }
        log::debug!(
            "link {label}: anchors {:.0?}, schemas {:.0?}",
            anchored,
            started.elapsed().saturating_sub(anchored)
        );
        Ok(())
    }

    /// Level 1: the operation is declared at head, and its anchor resolves.
    fn persists(&mut self, label: &str, role: &str, address: &Address) -> Result<bool, GraphError> {
        let at_base = self.find(address, BASE).is_some();
        let Some(found) = self.find(address, HEAD) else {
            let message = if at_base {
                format!("the {role} operation {address} no longer exists")
            } else {
                format!("the {role} operation {address} is not declared in any description")
            };
            let name = "operation_not_documented";
            self.report(label, Note::problem("link", message, at_base).named(name));
            return Ok(false);
        };
        let anchor = found.operation.path.clone();
        let checkout = found.repo.checkouts[HEAD].clone();
        if anchor_resolves(&checkout, &anchor, &mut self.extractor, &mut self.reads)? {
            return Ok(true);
        }
        // Introduced only if the anchor resolved before the change.
        let resolved_at_base = match self.find(address, BASE) {
            Some(before) => {
                let anchor = before.operation.path.clone();
                let checkout = before.repo.checkouts[BASE].clone();
                anchor_resolves(&checkout, &anchor, &mut self.extractor, &mut self.reads)?
            }
            None => false,
        };
        let message =
            format!("the {role} operation {address}'s code anchor `{anchor}` does not resolve");
        let note = Note::problem("link", message, resolved_at_base);
        self.report(label, note.named("anchor_unresolved"));
        Ok(false)
    }

    fn find(&self, address: &Address, snapshot: usize) -> Option<Found<'_>> {
        let repo = self.repos.get(address.repository.as_deref()?)?;
        let service = repo.files[snapshot].services.get(&address.service)?;
        if let Some(operation) = service.exports.operations.get(&address.operation) {
            return Some(Found {
                repo,
                service,
                operation,
            });
        }
        if snapshot == HEAD {
            return None;
        }
        // Renamed by the change: the head's declaration says where it was.
        let head = repo.files[HEAD].services.get(&address.service)?;
        let renamed = head.exports.operations.get(&address.operation)?;
        renamed.previously.iter().find_map(|old| {
            let (service_name, operation_name) = old.rsplit_once('.')?;
            let service = repo.files[BASE].services.get(service_name)?;
            let operation = service.exports.operations.get(operation_name)?;
            Some(Found {
                repo,
                service,
                operation,
            })
        })
    }

    /// Level 2: the sender's schema against the receiver's, at head; a
    /// difference already there at base is reported as not introduced.
    fn compare_part(&mut self, label: &str, link: &Link, part: Part) -> Result<(), GraphError> {
        let Some(head) = self.differences(link, part, HEAD, label)? else {
            return Ok(());
        };
        let changed = self
            .repos
            .values()
            .any(|repo| repo.checkouts[BASE] != repo.checkouts[HEAD]);
        let base = if changed {
            self.differences(link, part, BASE, label)?
                .unwrap_or_default()
        } else {
            head.clone()
        };
        for difference in head {
            let introduced = !base.contains(&difference);
            let message = format!("{}: {}", part.name(), difference.description);
            self.report(
                label,
                Note {
                    description_name: None,
                    check: part.name(),
                    message,
                    introduced,
                    problem: difference.breaking,
                },
            );
        }
        Ok(())
    }

    /// The differences between the sender's and receiver's schemas for one
    /// part at one snapshot; `None` if either side declares no schema or it
    /// could not be resolved (then reported).
    fn differences(
        &mut self,
        link: &Link,
        part: Part,
        snapshot: usize,
        label: &str,
    ) -> Result<Option<Vec<Difference>>, GraphError> {
        // A request travels from caller to provider; a response back.
        let (caller_role, provider_role) = match part {
            Part::Request => (Role::Sender, Role::Receiver),
            Part::Response => (Role::Receiver, Role::Sender),
        };
        let caller_want = Want {
            part,
            snapshot,
            role: caller_role,
        };
        let provider_want = Want {
            part,
            snapshot,
            role: provider_role,
        };
        let (Some(caller), Some(provider)) = (
            self.schema(&link.caller, caller_want, label),
            self.schema(&link.provider, provider_want, label),
        ) else {
            return Ok(None);
        };
        let (sender, receiver, expected_fields) = match part {
            Part::Request => (caller, provider, false),
            Part::Response => (provider, caller, true),
        };
        Ok(Some(compare(&sender, &receiver, expected_fields)?))
    }

    fn schema(&mut self, address: &Address, want: Want, label: &str) -> Option<serde_json::Value> {
        let Want {
            part,
            snapshot,
            role,
        } = want;
        let found = self.find(address, snapshot)?;
        let Some(schemas) = &found.operation.schema else {
            return None;
        };
        let source: Option<&SchemaSource> = match part {
            Part::Request => schemas.request.as_ref(),
            Part::Response => schemas.response.as_ref(),
        };
        let source = source.cloned()?;
        let checkout = found.repo.checkouts[snapshot].clone();
        let operation_resolver = schemas.resolver.clone();
        let runtime = found.service.runtime.clone();
        let mut context = Context {
            role,
            checkout: &checkout,
            resolver: operation_resolver.as_ref(),
            runtime: &runtime,
            cache: self.cache,
            outputs: &mut self.outputs,
            reads: &mut self.reads,
            environment: self.environment.as_deref(),
        };
        match schema::resolve(&source, &mut context) {
            Ok(resolved_schema) => {
                if snapshot == HEAD && !resolved_schema.unresolved.is_empty() {
                    let message = format!(
                        "{address}'s {} schema ({}) uses types it could not follow: {}",
                        part.name(),
                        resolved_schema.source,
                        resolved_schema.unresolved.join(", ")
                    );
                    self.report(
                        label,
                        Note {
                            description_name: Some("schema_open"),
                            check: "schema",
                            message,
                            introduced: false,
                            problem: false,
                        },
                    );
                }
                Some(resolved_schema.schema)
            }
            Err(error) => {
                self.report_unresolved(
                    label,
                    &format!("{address}'s {}", part.name()),
                    &error,
                    snapshot,
                );
                None
            }
        }
    }

    /// Reports a schema that could not be resolved. A resolver that could
    /// not run (its interpreter or dependencies missing here) says nothing
    /// about the change, as an adapter that could not run does not: a
    /// warning, an error at level strict.
    fn report_unresolved(
        &mut self,
        label: &str,
        schema: &str,
        error: &GraphError,
        snapshot: usize,
    ) {
        if let GraphError::Resolver {
            command, directory, ..
        } = error
        {
            let unread = self
                .unread
                .entry((command.clone(), directory.clone()))
                .or_insert_with(|| Unread {
                    link: label.to_owned(),
                    error: describe(error),
                    schemas: Vec::new(),
                });
            if !unread.schemas.iter().any(|known| known == schema) {
                unread.schemas.push(schema.to_owned());
            }
            return;
        }
        let message = format!("could not resolve {schema} schema: {}", describe(error));
        let note = Note::problem("schema", message, snapshot == HEAD);
        self.report(label, note.named("schema_unresolved"));
    }

    /// Records a finding: a problem is a warning at level `audit` and an
    /// error at level `check`; anything else, including a problem a checked
    /// change did not introduce, is context.
    fn report(&mut self, link: &str, note: Note) {
        let Note {
            description_name,
            check,
            message,
            introduced,
            problem,
        } = note;
        let is_problem = problem && (introduced || !self.has_change);
        let severity = match (is_problem, self.level) {
            (false, _) => Severity::Info,
            (true, Level::Audit) => Severity::Warning,
            (true, Level::Check | Level::Strict) => Severity::Error,
        };
        let finding = Finding {
            severity,
            link: link.to_owned(),
            check: check.to_owned(),
            message,
            introduced,
            affects: Vec::new(),
            allowed: None,
            values: BTreeMap::new(),
            derivation: None,
            description_name,
        };
        if !self.findings.contains(&finding) {
            self.findings.push(finding);
        }
    }
}

/// Whether `file:Symbol` names a definition in the file at the checkout.
/// `Symbol` is a qualified name (`Charges.Create`); `()` is ignored.
/// Files in languages without a symbol extractor only need to exist.
fn anchor_resolves(
    checkout: &Checkout,
    anchor: &str,
    extractor: &mut Extractor,
    reads: &mut schema::Reads,
) -> Result<bool, GraphError> {
    let (file, symbol) = anchor.rsplit_once(':').unwrap_or((anchor, ""));
    let Some(source) = reads.text(checkout, file)? else {
        return Ok(false);
    };
    let extension = file.rsplit_once('.').map_or("", |(_, extension)| extension);
    let Some(language) = Language::from_extension(extension) else {
        return Ok(true);
    };
    if symbol.is_empty() {
        return Ok(true);
    }
    let symbol = symbol.replace("()", "");
    let extraction = extractor
        .extract(language, &source)
        .map_err(|source| GraphError::Syntax { source })?;
    Ok(extraction.symbols.iter().any(|found| {
        found.qualified_name == symbol || found.qualified_name.ends_with(&format!(".{symbol}"))
    }))
}

fn describe(error: &GraphError) -> String {
    let mut message = error.to_string();
    let mut source = std::error::Error::source(error);
    while let Some(cause) = source {
        message.push_str(": ");
        message.push_str(&cause.to_string());
        source = cause.source();
    }
    message
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn at_level_strict_unread_code_is_an_error_the_change_counts() {
        let mut findings = vec![
            adapter_warning("#billing:api", "could not read api's operations".to_owned()),
            Finding {
                severity: Severity::Warning,
                link: "#web:web".to_owned(),
                check: "request".to_owned(),
                message: "a field".to_owned(),
                introduced: true,
                affects: Vec::new(),
                allowed: None,
                values: BTreeMap::new(),
                derivation: None,
                description_name: None,
            },
        ];

        fail_unread_contracts(Level::Strict, &mut findings);

        assert_eq!(findings[0].severity, Severity::Error);
        assert!(findings[0].introduced);
        assert_eq!(findings[1].severity, Severity::Warning);
    }
}
