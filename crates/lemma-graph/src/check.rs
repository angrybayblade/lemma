//! Loading a system of `lemma.yaml` files and checking its links.
//!
//! The system starts at a parent `lemma.yaml`. Every external service it or
//! a repository it reaches declares (`#name`) is resolved to a checkout and
//! its `lemma.yaml` read there; the first declaration of a name wins, so a
//! parent's pin overrides a repository's. The changed repository is read at
//! the change's two commits, every other at its pin. Links come from every
//! `graph` at head.

use std::collections::{BTreeMap, BTreeSet, VecDeque};
use std::path::{Path, PathBuf};
use std::sync::Arc;

use lemma_logic::Engine;
use lemma_model::Language;
use lemma_syntax::Extractor;

use crate::GraphError;
use crate::adapters;
use crate::address::{Address, Link, parse_link};
use crate::affects::{self, Walkable};
use crate::compare::{Difference, compare};
use crate::config::{LEMMA_FILE, LemmaFile, Level, Operation, SchemaSource, Service, parse};
use crate::infer;
use crate::repos::{Checkout, clone_remote, resolve_commit};
use crate::schema::{self, Context, Role};

/// The name of the repository holding the parent `lemma.yaml`.
pub(crate) const SELF: &str = "self";
const BASE: usize = 0;
pub(crate) const HEAD: usize = 1;

/// The change being checked: a repository and its commits before and with
/// the change.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Change {
    /// The external service's name (`#billing`), or `self` for the
    /// repository holding the parent `lemma.yaml`.
    pub repository: String,
    /// The commit before the change.
    pub base: String,
    /// The commit with the change; the working directory, uncommitted
    /// changes included, when absent.
    pub head: Option<String>,
}

/// What to check, and where to cache clones and worktrees.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CheckOptions {
    /// The change; without one, every repository is read once and links are
    /// checked as they stand.
    pub change: Option<Change>,
    /// Overrides the `graph`'s level.
    pub level: Option<Level>,
    /// Lemma's cache directory.
    pub cache: PathBuf,
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
}

/// Every link a check found, as [`CheckOptions::list_links`] asks.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize)]
pub struct LinkList {
    /// The links `lemma.yaml` files declare, as `caller -> provider`.
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
    /// The service serving it (`#platform:apollo`), when one does.
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
    /// Whether its `lemma.yaml` came from the working directory because a
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
}

/// A repository at both snapshots.
pub(crate) struct Repo {
    pub(crate) checkouts: [Checkout; 2],
    pub(crate) files: [LemmaFile; 2],
    pub(crate) manifest_from_working_directory: bool,
}

/// Checks the links of the system whose parent `lemma.yaml` is `root_file`,
/// evaluating inferred links' grammar with `engine`. With `references`,
/// also checks that each link's caller is anchored at code that is called.
pub fn check<E: Engine>(
    root_file: &Path,
    options: &CheckOptions,
    engine: &E,
    references: Option<&mut dyn crate::anchors::References>,
) -> Result<Report, GraphError> {
    let started = std::time::Instant::now();
    let repos = load(root_file, options)?;
    check_change_names_a_repository(&repos, options)?;
    log_repositories(&repos);
    let level = options
        .level
        .or(repos[SELF].files[HEAD].graph.level)
        .unwrap_or_default();
    let links = collect_links(&repos)?;
    let mut findings = check_declared(&repos, &links, level, options)?;
    log::info!(
        "checked {} declared links: {} findings",
        links.len(),
        findings.len()
    );
    let mut seeds = declared_seeds(&findings, &links, &repos);
    // After the seeds: a doubtful anchor is no problem to trace from.
    if let Some(references) = references {
        let anchored = crate::anchors::check_anchors(&repos, &links, &options.cache, references)?;
        findings.extend(anchored);
    }
    // Verification and tracing read the same repositories; each is listed
    // once.
    let listings = Arc::new(lemma_http::Listings::default());
    let shared = Shared {
        options,
        listings: &listings,
    };
    let (inferred_links, walkable) = infer(&repos, &shared, engine, level, &mut findings)?;
    seeds.extend(walkable.seeds.iter().cloned());
    let facts = options.facts_cache.as_ref();
    let reading = affects::CodeReading {
        facts,
        listings: &listings,
    };
    let mut fetch = |operations: &BTreeSet<String>| {
        fetch_calls(
            walkable.partial.as_ref(),
            operations,
            engine,
            facts,
            &listings,
        )
    };
    let mut source = affects::CallSource {
        walkable: &walkable,
        fetch: &mut fetch,
    };
    affects::attach(&mut findings, &seeds, &mut source, &repos, &reading)?;
    sort_findings(&mut findings);
    log_outcome(&findings, started);
    let link_list = options.list_links.then(|| link_list(&links, &walkable));
    Ok(Report {
        level,
        links: links.len(),
        inferred_links,
        repositories: resolved(&repos),
        findings,
        link_list,
    })
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
            ", lemma.yaml from the working directory"
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

/// Fails when the change names a repository no `lemma.yaml` declares.
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
            base: "HEAD",
            head: Some("HEAD"),
            is_drift: false,
            cache: &options.cache,
            facts_cache: options.facts_cache.as_ref(),
            listings: shared.listings,
            evaluation: lemma_verify::Evaluation::Full,
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

/// The calls of `operations` the inference behind `partial` did not read:
/// verified again, reading only the code that may make them.
fn fetch_calls<E: Engine>(
    partial: Option<&affects::PartialCalls>,
    operations: &BTreeSet<String>,
    engine: &E,
    facts: Option<&lemma_verify::FactsCache>,
    listings: &Arc<lemma_http::Listings>,
) -> Result<Vec<crate::impact::Call>, GraphError> {
    let Some(partial) = partial else {
        return Ok(Vec::new());
    };
    let options = lemma_verify::Options {
        explain: false,
        relations: affects::TRACED_RELATIONS.map(str::to_owned).to_vec(),
        facts_cache: facts.cloned(),
        evaluation: lemma_verify::Evaluation::Demand,
    };
    let report = lemma_verify::calls_of(&partial.manifest, engine, &options, listings, operations)
        .map_err(|source| GraphError::Inference { source })?;
    Ok(affects::walkable(&report.relations, &partial.owners).calls)
}

/// The findings of the declared links, and of documents Lemma cannot read.
fn check_declared(
    repos: &BTreeMap<String, Repo>,
    links: &BTreeSet<Link>,
    level: Level,
    options: &CheckOptions,
) -> Result<Vec<Finding>, GraphError> {
    let mut checker = Checker {
        repos,
        level,
        cache: &options.cache,
        extractor: Extractor::new().map_err(|source| GraphError::Syntax { source })?,
        outputs: BTreeMap::new(),
        reads: schema::Reads::default(),
        findings: Vec::new(),
        has_change: options.change.is_some(),
    };
    for link in links {
        checker.check_link(link)?;
    }
    // Adapters build in the same shared worktree resolvers ran in.
    checker.reads.release();
    let mut findings = checker.findings;
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
    base: &'a str,
    /// `None` for the working directory.
    head: Option<&'a str>,
    is_drift: bool,
    cache: &'a Path,
    facts_cache: Option<&'a lemma_verify::FactsCache>,
    listings: &'a Arc<lemma_http::Listings>,
    evaluation: lemma_verify::Evaluation,
}

/// What every inference in one check shares: its options, and the
/// listings verification and tracing both read.
struct Shared<'a> {
    options: &'a CheckOptions,
    listings: &'a Arc<lemma_http::Listings>,
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
        base,
        head,
        is_drift: false,
        cache: &options.cache,
        facts_cache: options.facts_cache.as_ref(),
        listings,
        evaluation: evaluation_of(options),
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
            base: "HEAD",
            head: Some("HEAD"),
            is_drift: true,
            cache: &options.cache,
            facts_cache: options.facts_cache.as_ref(),
            listings,
            evaluation: options.evaluation,
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
    let Some(mut inferred) = infer::manifest(&sources, &changed) else {
        return Ok(None);
    };
    // With nothing to compare and no declared problem to trace, the
    // inference would find what the committed documents already say.
    let needs_trace = findings
        .iter()
        .any(|finding| finding.severity != Severity::Info);
    let has_drift = apply_adapters(&mut inferred, repos, run, findings);
    if run.is_drift && !has_drift && !needs_trace {
        return Ok(None);
    }
    let report = verify_inferred(&inferred, run, engine)?;
    let mut walkable = affects::walkable(&report.relations, &inferred.repositories);
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
        explain: false,
        relations: affects::TRACED_RELATIONS.map(str::to_owned).to_vec(),
        facts_cache: run.facts_cache.cloned(),
        // Tracing asks for the calls of further operations as it reaches
        // them (see `affects::CallSource`).
        evaluation: run.evaluation,
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
fn apply_adapters(
    inferred: &mut infer::Inferred,
    repos: &BTreeMap<String, Repo>,
    run: &Inference<'_>,
    findings: &mut Vec<Finding>,
) -> bool {
    let jobs = adapter_jobs(inferred, repos, run);
    let results = run_adapter_jobs(&jobs, run.cache);
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
    runtime: &'r crate::config::Runtime,
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
        for (role, checkout, build) in adapter_snapshots(repo, repo_name, run) {
            jobs.push(AdapterJob {
                entry: entry.clone(),
                service: service_name.to_owned(),
                role,
                is_changed: repo_name == run.changed,
                checkout,
                build,
                from,
                runtime: &service.runtime,
                documents: repository.specs.clone(),
            });
        }
    }
    jobs
}

/// Runs every job, the jobs of one repository in turn (they share its
/// build worktree) and repositories side by side; results in job order.
fn run_adapter_jobs(
    jobs: &[AdapterJob<'_>],
    cache: &Path,
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
                        .map(|&index| (index, adapter_contract(&jobs[index], cache)))
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
) -> Result<Option<adapters::Contract>, GraphError> {
    log::info!(
        "adapter: reading the operations of {} ({}) from its code",
        job.entry,
        job.role
    );
    let Some(generated) =
        adapters::generate(job.from, job.runtime, job.checkout, cache, job.build)?
    else {
        return Ok(None);
    };
    let committed = adapters::committed_documents(job.checkout, &job.documents)?;
    adapters::contract(&job.service, &generated, &committed, cache).map(Some)
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
    run: &Inference<'_>,
) -> Vec<(&'static str, &'r Checkout, adapters::Build)> {
    if repo_name != run.changed {
        return vec![("both", &repo.checkouts[HEAD], adapters::Build::KeptOnly)];
    }
    if run.is_drift {
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

/// A warning that a service's contract was not fully read from its code.
fn adapter_warning(entry: &str, message: String) -> Finding {
    Finding {
        severity: Severity::Warning,
        link: entry.to_owned(),
        check: "adapter".to_owned(),
        message,
        introduced: false,
        affects: Vec::new(),
    }
}

/// A repository's `lemma.yaml` as inference reads it. One borrowed from the
/// working directory for a commit that had none names documents as they are
/// today; those missing at both of the repository's checkouts are dropped
/// rather than failing the check. A `lemma.yaml` from the commit itself is
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
            exists = exists || checkout.read(&document)?.is_some();
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
        ("error", Level::Check) => Severity::Error,
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

/// The parent and every repository reachable through `#name` services.
pub(crate) fn load(
    root_file: &Path,
    options: &CheckOptions,
) -> Result<BTreeMap<String, Repo>, GraphError> {
    let directory = absolute(root_file.parent().unwrap_or(Path::new(".")));
    let manifest = root_file.file_name().map_or_else(
        || LEMMA_FILE.to_owned(),
        |name| name.to_string_lossy().into_owned(),
    );
    let mut repos = BTreeMap::new();
    let mut pending = VecDeque::from([(SELF.to_owned(), directory, None, manifest)]);
    while let Some((name, root, reference, manifest)) = pending.pop_front() {
        if repos.contains_key(&name) {
            continue;
        }
        let checkouts = checkouts(&name, &root, reference.as_deref(), options)?;
        let (base_file, base_fallback) =
            read_manifest(&name, &checkouts[BASE], &manifest, &options.cache)?;
        let (head_file, head_fallback) =
            read_manifest(&name, &checkouts[HEAD], &manifest, &options.cache)?;
        let mut files = [base_file, head_file];
        for (file, checkout) in files.iter_mut().zip(&checkouts) {
            add_documented_operations(file, checkout)?;
        }
        let manifest_from_working_directory = base_fallback || head_fallback;
        let base_directory = root.join(Path::new(&manifest).parent().unwrap_or(Path::new("")));
        for file in &files {
            for (external, service) in file.services.iter().filter(|(key, _)| key.starts_with('#'))
            {
                let source = external_root(service, &base_directory, &options.cache)?;
                pending.push_back(source.with_name(external));
            }
        }
        repos.insert(
            name,
            Repo {
                checkouts,
                files,
                manifest_from_working_directory,
            },
        );
    }
    Ok(repos)
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
        manifest: service
            .manifest
            .clone()
            .unwrap_or_else(|| LEMMA_FILE.to_owned()),
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
        return Ok([at(Some(resolve_commit(root, &change.base)?)), at(head)]);
    }
    let commit = reference
        .map(|reference| resolve_commit(root, reference))
        .transpose()?;
    Ok([at(commit.clone()), at(commit)])
}

/// A repository's `lemma.yaml` at a checkout, falling back to the working
/// directory's when the commit has none; the flag says whether it did.
fn read_manifest(
    name: &str,
    checkout: &Checkout,
    manifest: &str,
    cache: &Path,
) -> Result<(LemmaFile, bool), GraphError> {
    let manifest = language_manifest(checkout, manifest)?;
    let manifest = manifest.as_str();
    if Path::new(manifest)
        .extension()
        .is_some_and(|extension| extension == "lm")
    {
        return read_language_manifest(name, checkout, manifest, cache);
    }
    let (text, fallback) = if let Some(text) = checkout.read(manifest)? {
        (text, false)
    } else {
        let working = Checkout {
            root: checkout.root.clone(),
            commit: None,
        };
        let text = working
            .read(manifest)?
            .ok_or_else(|| GraphError::MissingManifest {
                name: name.to_owned(),
                path: manifest.to_owned(),
                root: checkout.root.clone(),
                commit: checkout.commit.clone(),
            })?;
        (text, checkout.commit.is_some())
    };
    let path = checkout.root.join(manifest);
    let file = parse(&text).map_err(|source| GraphError::Parse {
        path: path.clone(),
        source,
    })?;
    file.validate()
        .map_err(|source| GraphError::InvalidService { path, source })?;
    Ok((file, fallback))
}

/// The manifest to read: `lemma.yaml` falls back to `lemma.lm` for a
/// repository that has only the latter, at the checkout or in its working
/// directory.
fn language_manifest(checkout: &Checkout, manifest: &str) -> Result<String, GraphError> {
    if manifest != LEMMA_FILE {
        return Ok(manifest.to_owned());
    }
    let working = Checkout {
        root: checkout.root.clone(),
        commit: None,
    };
    // Most repositories have lemma.yaml; look no further for them.
    if checkout.read(manifest)?.is_some() || working.read(manifest)?.is_some() {
        return Ok(manifest.to_owned());
    }
    let has_language = checkout.read(lemma_lang::load::ROOT_FILE)?.is_some()
        || working.read(lemma_lang::load::ROOT_FILE)?.is_some();
    if has_language {
        return Ok(lemma_lang::load::ROOT_FILE.to_owned());
    }
    Ok(manifest.to_owned())
}

/// A repository's `.lm` manifest at a checkout, falling back to the working
/// directory's when the commit has none, as `lemma.yaml` does.
fn read_language_manifest(
    name: &str,
    checkout: &Checkout,
    manifest: &str,
    cache: &Path,
) -> Result<(LemmaFile, bool), GraphError> {
    if checkout.read(manifest)?.is_some() {
        return Ok((crate::lm::read_manifest(checkout, manifest, cache)?, false));
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
    let file = crate::lm::read_manifest(&working, manifest, cache)?;
    Ok((file, checkout.commit.is_some()))
}

/// Adds every operation a local service's `OpenAPI` documents declare, at
/// the checkout, to its operations; one declared by name in `lemma.yaml`
/// wins. A document missing at the checkout adds nothing.
fn add_documented_operations(file: &mut LemmaFile, checkout: &Checkout) -> Result<(), GraphError> {
    for (name, service) in &mut file.services {
        if name.starts_with('#') {
            continue;
        }
        for document_file in &service.exports.openapi {
            let Some(text) = checkout.read(document_file)? else {
                continue;
            };
            let document: serde_json::Value =
                serde_json::from_str(&text).map_err(|source| GraphError::ResolverOutput {
                    command: format!("reading {document_file}"),
                    source,
                })?;
            for (id, operation) in crate::openapi::operations(document_file, &document) {
                service.exports.operations.entry(id).or_insert(operation);
            }
        }
    }
    Ok(())
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
    outputs: BTreeMap<(String, String, String), serde_json::Value>,
    reads: schema::Reads,
    findings: Vec<Finding>,
    /// Whether a change is being checked: then only what it introduces is a
    /// problem, and what was already there is context.
    has_change: bool,
}

/// A finding before its severity is decided.
struct Note {
    check: &'static str,
    message: String,
    introduced: bool,
    /// Whether it is a problem (a warning or error) rather than context.
    problem: bool,
}

impl Note {
    fn problem(check: &'static str, message: String, introduced: bool) -> Self {
        Note {
            check,
            message,
            introduced,
            problem: true,
        }
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
        let label = format!("{} -> {}", link.caller, link.provider);
        let caller_ok = self.persists(&label, "caller", &link.caller)?;
        let provider_ok = self.persists(&label, "provider", &link.provider)?;
        if !(caller_ok && provider_ok) {
            return Ok(());
        }
        for part in [Part::Request, Part::Response] {
            self.compare_part(&label, link, part)?;
        }
        Ok(())
    }

    /// Level 1: the operation is declared at head, and its anchor resolves.
    fn persists(&mut self, label: &str, role: &str, address: &Address) -> Result<bool, GraphError> {
        let at_base = self.find(address, BASE).is_some();
        let Some(found) = self.find(address, HEAD) else {
            let message = if at_base {
                format!("the {role} operation {address} no longer exists")
            } else {
                format!("the {role} operation {address} is not declared in any lemma.yaml")
            };
            self.report(label, Note::problem("link", message, at_base));
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
        self.report(label, Note::problem("link", message, resolved_at_base));
        Ok(false)
    }

    fn find(&self, address: &Address, snapshot: usize) -> Option<Found<'_>> {
        let repo = self.repos.get(address.repository.as_deref()?)?;
        let service = repo.files[snapshot].services.get(&address.service)?;
        let operation = service.exports.operations.get(&address.operation)?;
        Some(Found {
            repo,
            service,
            operation,
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
                let message = format!(
                    "could not resolve {address}'s {} schema: {}",
                    part.name(),
                    describe(&error)
                );
                self.report(label, Note::problem("schema", message, snapshot == HEAD));
                None
            }
        }
    }

    /// Records a finding: a problem is a warning at level `audit` and an
    /// error at level `check`; anything else, including a problem a checked
    /// change did not introduce, is context.
    fn report(&mut self, link: &str, note: Note) {
        let Note {
            check,
            message,
            introduced,
            problem,
        } = note;
        let is_problem = problem && (introduced || !self.has_change);
        let severity = match (is_problem, self.level) {
            (false, _) => Severity::Info,
            (true, Level::Audit) => Severity::Warning,
            (true, Level::Check) => Severity::Error,
        };
        let finding = Finding {
            severity,
            link: link.to_owned(),
            check: check.to_owned(),
            message,
            introduced,
            affects: Vec::new(),
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
