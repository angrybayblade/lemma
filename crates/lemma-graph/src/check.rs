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

use lemma_logic::Engine;
use lemma_model::Language;
use lemma_syntax::Extractor;

use crate::GraphError;
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
    /// The commit with the change.
    pub head: String,
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
    /// How many calls from code to documented operations were found and
    /// checked; `None` without a change, when inference does not run.
    pub inferred_links: Option<usize>,
    /// Every repository read.
    pub repositories: Vec<ResolvedRepository>,
    /// Findings, most severe first.
    pub findings: Vec<Finding>,
}

/// A repository at both snapshots.
pub(crate) struct Repo {
    pub(crate) checkouts: [Checkout; 2],
    pub(crate) files: [LemmaFile; 2],
    pub(crate) manifest_from_working_directory: bool,
}

/// Checks the links of the system whose parent `lemma.yaml` is `root_file`,
/// evaluating inferred links' grammar with `engine`.
pub fn check<E: Engine>(
    root_file: &Path,
    options: &CheckOptions,
    engine: &E,
) -> Result<Report, GraphError> {
    let repos = load(root_file, options)?;
    if let Some(change) = &options.change {
        if !repos.contains_key(&change.repository) {
            return Err(GraphError::UnknownRepository {
                name: change.repository.clone(),
            });
        }
    }
    let level = options
        .level
        .or(repos[SELF].files[HEAD].graph.level)
        .unwrap_or_default();
    let links = collect_links(&repos)?;
    let mut checker = Checker {
        repos: &repos,
        level,
        cache: &options.cache,
        extractor: Extractor::new().map_err(|source| GraphError::Syntax { source })?,
        outputs: BTreeMap::new(),
        findings: Vec::new(),
        has_change: options.change.is_some(),
    };
    for link in &links {
        checker.check_link(link)?;
    }
    let mut findings = checker.findings;
    let mut seeds = declared_seeds(&findings, &links, &repos);
    let inference = match &options.change {
        Some(change) => infer_links(&repos, change, level, engine, &mut findings)?,
        None => None,
    };
    let (inferred_links, walkable) = match inference {
        Some((count, walkable)) => (Some(count), walkable),
        None => (None, Walkable::default()),
    };
    seeds.extend(walkable.seeds.iter().cloned());
    affects::attach(&mut findings, &seeds, &walkable, &repos)?;
    findings.sort_by(|left, right| {
        (left.severity, &left.link, &left.check, &left.message).cmp(&(
            right.severity,
            &right.link,
            &right.check,
            &right.message,
        ))
    });
    Ok(Report {
        level,
        links: links.len(),
        inferred_links,
        repositories: resolved(&repos),
        findings,
    })
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

/// Finds and checks calls from code to documented operations, adding their
/// findings; the number of distinct calls found, and the calls and routes a
/// trace can walk.
fn infer_links<E: Engine>(
    repos: &BTreeMap<String, Repo>,
    change: &Change,
    level: Level,
    engine: &E,
    findings: &mut Vec<Finding>,
) -> Result<Option<(usize, Walkable)>, GraphError> {
    let changed_repo = &repos[&change.repository];
    let (Some(base), Some(head)) = (
        changed_repo.checkouts[BASE].commit.as_deref(),
        changed_repo.checkouts[HEAD].commit.as_deref(),
    ) else {
        return Ok(None);
    };
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
        name: &change.repository,
        base,
        head,
    };
    let Some(inferred) = infer::manifest(&sources, &changed) else {
        return Ok(None);
    };
    let options = lemma_verify::Options {
        explain: false,
        relations: vec!["calls".to_owned(), "route".to_owned()],
    };
    let report = lemma_verify::verify(&inferred.manifest, engine, &options)
        .map_err(|source| GraphError::Inference { source })?;
    let mut walkable = affects::walkable(&report.relations, &inferred.repositories);
    for found in &report.findings {
        if let Some(seed) = affects::inferred_seed(&found.values, &inferred.repositories) {
            walkable.seeds.push((findings.len(), seed));
        }
        findings.push(inferred_finding(found, level));
    }
    let calls: BTreeSet<(&String, &String)> = report
        .relations
        .get("calls")
        .into_iter()
        .flatten()
        .filter_map(|row| Some((row.first()?, row.get(1)?)))
        .collect();
    Ok(Some((calls.len(), walkable)))
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
        retain_existing(&mut service.openapi, &repo.checkouts)?;
        if let Some(client) = &mut service.client {
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
/// makes warnings or errors; everything it reports is about the change.
fn inferred_finding(found: &lemma_verify::Finding, level: Level) -> Finding {
    let severity = match (found.severity, level) {
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
        introduced: true,
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
        let (base_file, base_fallback) = read_manifest(&name, &checkouts[BASE], &manifest)?;
        let (head_file, head_fallback) = read_manifest(&name, &checkouts[HEAD], &manifest)?;
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
        return Ok([
            at(Some(resolve_commit(root, &change.base)?)),
            at(Some(resolve_commit(root, &change.head)?)),
        ]);
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
) -> Result<(LemmaFile, bool), GraphError> {
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
                commit: checkout
                    .commit
                    .clone()
                    .unwrap_or_else(|| "the working directory".to_owned()),
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

/// Adds every operation a local service's `OpenAPI` documents declare, at
/// the checkout, to its operations; one declared by name in `lemma.yaml`
/// wins. A document missing at the checkout adds nothing.
fn add_documented_operations(file: &mut LemmaFile, checkout: &Checkout) -> Result<(), GraphError> {
    for (name, service) in &mut file.services {
        if name.starts_with('#') {
            continue;
        }
        for document_file in &service.openapi {
            let Some(text) = checkout.read(document_file)? else {
                continue;
            };
            let document: serde_json::Value =
                serde_json::from_str(&text).map_err(|source| GraphError::ResolverOutput {
                    command: format!("reading {document_file}"),
                    source,
                })?;
            for (id, operation) in crate::openapi::operations(document_file, &document) {
                service.operations.entry(id).or_insert(operation);
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
        if anchor_resolves(&checkout, &anchor, &mut self.extractor)? {
            return Ok(true);
        }
        // Introduced only if the anchor resolved before the change.
        let resolved_at_base = match self.find(address, BASE) {
            Some(before) => {
                let anchor = before.operation.path.clone();
                let checkout = before.repo.checkouts[BASE].clone();
                anchor_resolves(&checkout, &anchor, &mut self.extractor)?
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
        let operation = service.operations.get(&address.operation)?;
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
) -> Result<bool, GraphError> {
    let (file, symbol) = anchor.rsplit_once(':').unwrap_or((anchor, ""));
    let Some(source) = checkout.read(file)? else {
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
