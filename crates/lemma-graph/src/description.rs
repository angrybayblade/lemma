//! Whether a description still matches the code: one snapshot, compared with
//! what its repositories hold. See `.specs/16-verifying-descriptions.md`.
//!
//! `lemma verify` reports everything here. `lemma check --change` runs the
//! part a change can cause at both snapshots, and reports what it introduced.

use std::collections::{BTreeMap, BTreeSet};
use std::path::Path;

use globset::Glob;
use lemma_logic::Engine;

use crate::GraphError;
use crate::check::{
    self, BASE, CheckOptions, Finding, HEAD, InferredLink, LinkList, NotChecked, Repo, Report,
    Severity,
};
use crate::config::{Level, Runtime, Service};
use crate::repos::{Checkout, tracked_files};

/// One way a description does not match the code.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, serde::Serialize)]
pub struct DescriptionFinding {
    /// How serious it is.
    pub severity: Severity,
    /// What is described wrongly: a repository's service or link.
    pub location: String,
    /// The kind of finding, such as `document_missing`.
    pub name: String,
    /// What was found.
    pub message: String,
}

/// What `lemma verify` found.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize)]
pub struct DescriptionReport {
    /// Findings, most severe first.
    pub findings: Vec<DescriptionFinding>,
    /// Every link, declared and inferred.
    pub link_list: Option<LinkList>,
    /// The rows of each derived relation asked for.
    #[serde(skip_serializing_if = "BTreeMap::is_empty")]
    pub relations: BTreeMap<String, Vec<Vec<String>>>,
    /// Repositories that are not here, so were not looked at.
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub not_checked: Vec<NotChecked>,
}

/// Compares the description at `entry` with the code it describes.
/// Language servers, through `references`, say which anchors nothing calls.
pub fn verify_description<E: Engine>(
    entry: &Path,
    options: &CheckOptions,
    engine: &E,
    references: &mut dyn crate::anchors::References,
) -> Result<DescriptionReport, GraphError> {
    let repos = check::load_manifests(entry, options)?;
    let mut findings = configuration_findings(&repos);
    findings.extend(renames_still_present(&repos));
    let listed = CheckOptions {
        list_links: true,
        level: Some(Level::Check),
        ..options.clone()
    };
    // A description this wrong can stop inference from reading the code at
    // all; what is wrong with it is then the answer.
    let report = match check::check(entry, &listed, engine, Some(references)) {
        Ok(report) => Some(report),
        Err(_) if has_error(&findings) => None,
        Err(error) => return Err(error),
    };
    if let Some(report) = &report {
        findings.extend(named_findings(report));
        findings.extend(report.link_list.iter().flat_map(redundant_links));
    }
    findings.sort();
    findings.dedup();
    let (link_list, relations, not_checked) = report.map_or_else(
        || (None, BTreeMap::new(), Vec::new()),
        |report| (report.link_list, report.relations, report.not_checked),
    );
    Ok(DescriptionReport {
        findings,
        link_list,
        relations,
        not_checked,
    })
}

fn has_error(findings: &[DescriptionFinding]) -> bool {
    findings
        .iter()
        .any(|finding| finding.severity == Severity::Error)
}

/// The findings a check made that are about the description, which it marks
/// with their names. Contract disagreements have none: they are for
/// `lemma check`.
fn named_findings(report: &Report) -> Vec<DescriptionFinding> {
    report
        .findings
        .iter()
        .filter_map(|finding| {
            Some(DescriptionFinding {
                severity: finding.severity,
                location: finding.link.clone(),
                name: finding.description_name?.to_owned(),
                message: finding.message.clone(),
            })
        })
        .collect()
}

/// Every problem with what `repos` say about themselves at head.
fn configuration_findings(repos: &BTreeMap<String, Repo>) -> Vec<DescriptionFinding> {
    let mut findings = Vec::new();
    for (name, repo) in repos {
        let checkout = &repo.checkouts[HEAD];
        let tracked = tracked_files(checkout);
        let tracked: Option<Vec<&str>> = tracked
            .as_ref()
            .map(|files| files.iter().map(String::as_str).collect());
        for (service_name, service) in &repo.files[HEAD].services {
            let location = format!("{name}:{service_name}");
            if service_name.starts_with('#') {
                findings.extend(unpinned(&location, service));
                continue;
            }
            findings.extend(empty_service(&location, service));
            findings.extend(selection_findings(&location, service, tracked.as_deref()));
            findings.extend(document_findings(&location, service, checkout));
            findings.extend(missing_runtimes(&location, &service.runtime, checkout));
        }
    }
    findings
}

fn finding(severity: Severity, location: &str, name: &str, message: String) -> DescriptionFinding {
    DescriptionFinding {
        severity,
        location: location.to_owned(),
        name: name.to_owned(),
        message,
    }
}

fn unpinned(location: &str, service: &Service) -> Option<DescriptionFinding> {
    let url = service.git.as_ref()?;
    service.reference.is_none().then(|| {
        finding(
            Severity::Warning,
            location,
            "external_unpinned",
            format!("{url} has no pinned commit, so what it is read at can change"),
        )
    })
}

fn empty_service(location: &str, service: &Service) -> Option<DescriptionFinding> {
    (!service.defines_anything()).then(|| {
        finding(
            Severity::Error,
            location,
            "service_empty",
            "reads no files and offers nothing".to_owned(),
        )
    })
}

/// `pattern`'s matches among `files`: a directory or file, or a glob.
fn matches_any(pattern: &str, files: &[&str]) -> bool {
    let directory = format!("{}/", pattern.trim_end_matches('/'));
    if files
        .iter()
        .any(|file| *file == pattern || file.starts_with(&directory))
    {
        return true;
    }
    Glob::new(pattern).is_ok_and(|glob| {
        let matcher = glob.compile_matcher();
        files.iter().any(|file| matcher.is_match(file))
    })
}

fn selection_findings(
    location: &str,
    service: &Service,
    tracked: Option<&[&str]>,
) -> Vec<DescriptionFinding> {
    let Some(tracked) = tracked else {
        return Vec::new();
    };
    let index = &service.index;
    index
        .include
        .iter()
        .chain(&index.exclude)
        .filter(|pattern| !matches_any(pattern, tracked))
        .map(|pattern| {
            finding(
                Severity::Error,
                location,
                "include_matches_nothing",
                format!("`{pattern}` matches no tracked file"),
            )
        })
        .collect()
}

fn document_findings(
    location: &str,
    service: &Service,
    checkout: &Checkout,
) -> Vec<DescriptionFinding> {
    let mut findings = Vec::new();
    for document in &service.exports.openapi {
        let Ok(Some(text)) = checkout.read(document) else {
            findings.push(missing_document(location, document));
            continue;
        };
        if !describes_operations(document, &text) {
            findings.push(finding(
                Severity::Error,
                location,
                "document_unreadable",
                format!("{document} does not parse, or describes no operations"),
            ));
        }
    }
    findings
}

fn missing_document(location: &str, document: &str) -> DescriptionFinding {
    finding(
        Severity::Error,
        location,
        "document_missing",
        format!("exports `{document}`, which does not exist"),
    )
}

fn describes_operations(name: &str, text: &str) -> bool {
    crate::adapters::parse_document(name, text).is_ok_and(|document| {
        document
            .get("paths")
            .and_then(serde_json::Value::as_object)
            .is_some_and(|paths| !paths.is_empty())
    })
}

fn missing_runtimes(
    location: &str,
    runtime: &Runtime,
    checkout: &Checkout,
) -> Vec<DescriptionFinding> {
    [
        &runtime.python,
        &runtime.node,
        &runtime.bun,
        &runtime.go,
        &runtime.cargo,
    ]
    .into_iter()
    .flatten()
    .filter(|program| !program_exists(program, &checkout.root))
    .map(|program| {
        finding(
            Severity::Error,
            location,
            "runtime_missing",
            format!("`{program}` does not exist"),
        )
    })
    .collect()
}

/// Whether `program`, a path from `root` or a name on `PATH`, exists.
fn program_exists(program: &str, root: &Path) -> bool {
    if program.contains('/') {
        return root.join(program).exists();
    }
    let path = std::env::var_os("PATH").unwrap_or_default();
    std::env::split_paths(&path).any(|directory| directory.join(program).is_file())
}

/// Renames whose old name still exists, so the claim of a rename is false.
fn renames_still_present(repos: &BTreeMap<String, Repo>) -> Vec<DescriptionFinding> {
    let mut findings = Vec::new();
    for (name, repo) in repos {
        let services = &repo.files[HEAD].services;
        let operations = services.iter().flat_map(|(service_name, service)| {
            service
                .exports
                .operations
                .iter()
                .map(move |(operation, found)| (service_name, operation, found))
        });
        for (service_name, operation_name, operation) in operations {
            let stale = operation
                .previously
                .iter()
                .filter(|old| still_declared(old, services));
            findings.extend(stale.map(|old| {
                finding(
                    Severity::Warning,
                    &format!("{name}:services.{service_name}.{operation_name}"),
                    "previously_still_present",
                    format!("says it was `{old}`, which still exists"),
                )
            }));
        }
    }
    findings
}

/// Whether `old`, written `service.operation`, is still declared.
fn still_declared(old: &str, services: &BTreeMap<String, Service>) -> bool {
    old.rsplit_once('.').is_some_and(|(service, operation)| {
        services
            .get(service)
            .is_some_and(|found| found.exports.operations.contains_key(operation))
    })
}

/// Declared links that inference finds too, and say nothing more.
fn redundant_links(list: &LinkList) -> Vec<DescriptionFinding> {
    let inferred: BTreeSet<(&str, &str)> = list
        .inferred
        .iter()
        .map(
            |InferredLink {
                 repository,
                 operation,
                 ..
             }| { (repository.as_str(), operation.as_str()) },
        )
        .collect();
    list.declared
        .iter()
        .filter(|declared| is_inferred(declared, &inferred))
        .map(|declared| {
            finding(
                Severity::Info,
                declared,
                "link_redundant",
                "inference finds this call too, so the link says nothing more".to_owned(),
            )
        })
        .collect()
}

/// Whether a declared link, `caller -> provider`, names by method and path a
/// call inference found in the caller's repository.
fn is_inferred(declared: &str, inferred: &BTreeSet<(&str, &str)>) -> bool {
    let Ok(link) = crate::address::parse_link(declared) else {
        return false;
    };
    let Some(repository) = link.caller.repository.as_deref() else {
        return false;
    };
    crate::address::is_route(&link.provider.operation)
        && inferred.contains(&(repository, link.provider.operation.as_str()))
}

/// What a change did to the description that `lemma check` must report:
/// documents it deleted and `include`s it emptied.
pub(crate) fn introduced_by_change(
    repos: &BTreeMap<String, Repo>,
    listings: &lemma_http::Listings,
    level: Level,
) -> Vec<Finding> {
    let severity = match level {
        Level::Audit => Severity::Warning,
        Level::Check | Level::Strict => Severity::Error,
    };
    // A repository outside the change reads the same at both snapshots, so
    // it can introduce nothing.
    let changed: Vec<(&String, &Repo)> = repos
        .iter()
        .filter(|(_, repo)| repo.checkouts[BASE] != repo.checkouts[HEAD])
        .collect();
    let at = |snapshot: usize| -> BTreeSet<DescriptionFinding> {
        changed
            .iter()
            .flat_map(|&(name, repo)| change_findings(name, repo, snapshot, listings))
            .collect()
    };
    let after = at(HEAD);
    if after.is_empty() {
        return Vec::new();
    }
    let before = at(BASE);
    after
        .into_iter()
        .filter(|found| !before.contains(found))
        .map(|found| Finding {
            severity,
            link: found.location,
            check: found.name,
            message: found.message,
            introduced: true,
            affects: Vec::new(),
            allowed: None,
            values: BTreeMap::new(),
            derivation: None,
            description_name: None,
        })
        .collect()
}

/// The findings of `repo` at `snapshot` a change to it can cause. Its
/// tracked files come from `listings`, where the check's other passes have
/// listed them already: a large repository takes Git long to list again.
fn change_findings(
    name: &str,
    repo: &Repo,
    snapshot: usize,
    listings: &lemma_http::Listings,
) -> Vec<DescriptionFinding> {
    let checkout = &repo.checkouts[snapshot];
    let listed = listed_tracked(checkout, listings);
    let tracked: Option<Vec<&str>> = listed.as_ref().map(ListedTracked::paths);
    let mut findings = Vec::new();
    for (service_name, service) in &repo.files[snapshot].services {
        if service_name.starts_with('#') {
            continue;
        }
        let location = format!("{name}:{service_name}");
        findings.extend(selection_findings(&location, service, tracked.as_deref()));
        for document in &service.exports.openapi {
            if matches!(checkout.has(document), Ok(false)) {
                findings.push(missing_document(&location, document));
            }
        }
    }
    findings
}

/// A checkout's listing, and the untracked files in it.
struct ListedTracked {
    files: std::sync::Arc<Vec<lemma_http::ListedFile>>,
    untracked: std::sync::Arc<Vec<String>>,
}

impl ListedTracked {
    /// The files Git tracks among those listed.
    fn paths(&self) -> Vec<&str> {
        let untracked: BTreeSet<&str> = self.untracked.iter().map(String::as_str).collect();
        self.files
            .iter()
            .map(|file| file.path.as_str())
            .filter(|path| !untracked.contains(path))
            .collect()
    }
}

/// The listing of `checkout`, with what in it Git does not track; `None`
/// where it cannot be listed.
fn listed_tracked(checkout: &Checkout, listings: &lemma_http::Listings) -> Option<ListedTracked> {
    let commit = checkout.commit.as_deref();
    let files = listings.get(&checkout.root, commit).ok()?;
    let untracked = match commit {
        Some(_) => std::sync::Arc::default(),
        None => listings.untracked(&checkout.root).ok()?,
    };
    Some(ListedTracked { files, untracked })
}
