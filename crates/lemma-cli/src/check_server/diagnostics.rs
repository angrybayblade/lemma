//! What a change's report says about each file, for `lemma check --lsp`.
//!
//! Pure: a report and the files the change touched go in, diagnostics by
//! file come out. Reading files to place a diagnostic within its line, and
//! speaking LSP, happen in the server.

use std::collections::{BTreeMap, BTreeSet};
use std::path::{Path, PathBuf};

use lemma_graph::{Finding, Report, Severity};

/// How many places a summary links to before it only counts the rest.
const RELATED_LIMIT: usize = 50;

/// A diagnostic about one line of a file.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord)]
pub(crate) struct LineDiagnostic {
    /// The line, counted from zero.
    pub line: u32,
    pub severity: Severity,
    /// The requirement or check that found it.
    pub code: String,
    pub message: String,
    /// Places elsewhere it is about, such as each caller a change breaks.
    pub related: Vec<Place>,
}

/// A line of a file, and what is there.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord)]
pub(crate) struct Place {
    pub path: PathBuf,
    /// Counted from zero.
    pub line: u32,
    pub message: String,
}

/// Diagnostics by absolute file path, each file's sorted.
pub(crate) type FileDiagnostics = BTreeMap<PathBuf, Vec<LineDiagnostic>>;

/// The diagnostics one repository's change gives: each finding the change
/// introduced, on the line of code it names, and on each file in `touched`
/// a summary linking to every such line outside those files, together with
/// the introduced findings that name no line. Findings that were there
/// before the change are left out: they are not the change's to fix.
pub(crate) fn diagnose(report: &Report, touched: &[PathBuf]) -> FileDiagnostics {
    let roots = repository_roots(report);
    let introduced: Vec<&Finding> = report
        .findings
        .iter()
        .filter(|finding| finding.introduced)
        .collect();
    let mut files = FileDiagnostics::new();
    let mut elsewhere: Vec<(&str, Severity, Place)> = Vec::new();
    let mut unlocated = Vec::new();
    for finding in introduced {
        let Some(location) = finding_location(finding, &roots) else {
            unlocated.push(finding);
            continue;
        };
        let place = Place {
            path: location.path,
            line: location.line,
            message: short_message(finding),
        };
        files
            .entry(place.path.clone())
            .or_default()
            .push(LineDiagnostic {
                line: place.line,
                severity: finding.severity,
                code: finding.check.clone(),
                message: place.message.clone(),
                related: changed_files(touched, &place.path),
            });
        if !touched.contains(&place.path) {
            elsewhere.push((location.repository, finding.severity, place));
        }
    }
    elsewhere.sort();
    elsewhere.dedup();
    for path in touched {
        let diagnostics = files.entry(path.clone()).or_default();
        if !elsewhere.is_empty() {
            diagnostics.push(summary(&elsewhere));
        }
        for finding in &unlocated {
            diagnostics.push(LineDiagnostic {
                line: 0,
                severity: finding.severity,
                code: finding.check.clone(),
                message: finding.message.clone(),
                related: Vec::new(),
            });
        }
    }
    files.retain(|_, diagnostics| !diagnostics.is_empty());
    for diagnostics in files.values_mut() {
        diagnostics.sort();
        diagnostics.dedup();
    }
    files
}

/// Each repository's name as findings spell it (`#billing`, `self`), with
/// its directory.
fn repository_roots(report: &Report) -> BTreeMap<&str, &Path> {
    report
        .repositories
        .iter()
        .map(|repository| (repository.name.as_str(), Path::new(&repository.root)))
        .collect()
}

/// Where a finding's code is.
struct Location<'a> {
    /// The repository, as findings spell it.
    repository: &'a str,
    path: PathBuf,
    /// Counted from zero.
    line: u32,
}

/// Where a finding's code is, from the `f` and `l` its requirement binds:
/// the standard library's requirements name the calling code's file `f`,
/// as `#repository:service:path` with the path relative to the repository,
/// and its line `l`, counted from one.
fn finding_location<'a>(
    finding: &Finding,
    roots: &BTreeMap<&'a str, &Path>,
) -> Option<Location<'a>> {
    let file = finding.values.get("f")?;
    let line: u32 = finding.values.get("l")?.parse().ok()?;
    let (repository, root, rest) = roots.iter().find_map(|(name, root)| {
        let rest = file.strip_prefix(name)?.strip_prefix(':')?;
        Some((*name, *root, rest))
    })?;
    let relative = rest.split_once(':').map_or(rest, |(_, path)| path);
    Some(Location {
        repository,
        path: root.join(relative),
        line: line.saturating_sub(1),
    })
}

/// A finding's message without the `(file:line)` it ends with, which the
/// diagnostic's place already says.
fn short_message(finding: &Finding) -> String {
    let (Some(file), Some(line)) = (finding.values.get("f"), finding.values.get("l")) else {
        return finding.message.clone();
    };
    let suffix = format!(" ({file}:{line})");
    finding
        .message
        .strip_suffix(&suffix)
        .unwrap_or(&finding.message)
        .to_owned()
}

/// The files a change touched other than `own`, as places a diagnostic in
/// `own` links back to.
fn changed_files(touched: &[PathBuf], own: &Path) -> Vec<Place> {
    touched
        .iter()
        .filter(|path| path.as_path() != own)
        .take(RELATED_LIMIT)
        .map(|path| Place {
            path: path.clone(),
            line: 0,
            message: "changed here".to_owned(),
        })
        .collect()
}

/// What a change breaks, in one diagnostic linking to each place, given
/// with its repository and how serious the finding there is: as serious as
/// the most serious of them.
fn summary(located: &[(&str, Severity, Place)]) -> LineDiagnostic {
    let severity = located
        .iter()
        .map(|(_, severity, _)| *severity)
        .min()
        .unwrap_or(Severity::Info);
    let repositories: BTreeSet<&str> = located
        .iter()
        .map(|(repository, _, _)| *repository)
        .collect();
    let named: Vec<&str> = repositories.into_iter().collect();
    let count = located.len();
    let noun = if count == 1 { "place" } else { "places" };
    let mut message = format!(
        "this change affects {count} {noun} in other code: {}",
        named.join(", ")
    );
    if count > RELATED_LIMIT {
        message.push_str(" (the first ");
        message.push_str(&RELATED_LIMIT.to_string());
        message.push_str(" are linked)");
    }
    LineDiagnostic {
        line: 0,
        severity,
        code: "change".to_owned(),
        message,
        related: located
            .iter()
            .take(RELATED_LIMIT)
            .map(|(_, _, place)| place.clone())
            .collect(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use lemma_graph::config::Level;
    use lemma_graph::{ResolvedRepository, Severity};

    fn repository(name: &str, root: &str) -> ResolvedRepository {
        ResolvedRepository {
            name: name.to_owned(),
            root: root.to_owned(),
            base: None,
            head: None,
            manifest_from_working_directory: false,
        }
    }

    fn finding(introduced: bool, file: Option<(&str, &str)>, message: &str) -> Finding {
        let mut values = BTreeMap::new();
        if let Some((path, line)) = file {
            values.insert("f".to_owned(), path.to_owned());
            values.insert("l".to_owned(), line.to_owned());
        }
        Finding {
            severity: Severity::Warning,
            link: "caller -> POST /link".to_owned(),
            check: "consumer_uses_removed_response_field".to_owned(),
            message: message.to_owned(),
            introduced,
            affects: Vec::new(),
            allowed: None,
            values,
            derivation: None,
            description_name: None,
        }
    }

    fn report(findings: Vec<Finding>) -> Report {
        Report {
            level: Level::Check,
            links: 0,
            inferred_links: None,
            repositories: vec![
                repository("#client", "/work/client"),
                repository("#server", "/work/server"),
            ],
            findings,
            link_list: None,
            behaviors: Vec::new(),
            requirement_changes: Vec::new(),
            not_checked: Vec::new(),
            relations: BTreeMap::new(),
        }
    }

    #[test]
    fn an_introduced_finding_lands_on_the_line_its_code_names() {
        let located = finding(
            true,
            Some(("#client:sdk:src/api.ts", "12")),
            "#client:sdk:src/api.ts::link names `url` (#client:sdk:src/api.ts:12)",
        );

        let files = diagnose(&report(vec![located]), &[]);

        let diagnostics = &files[Path::new("/work/client/src/api.ts")];
        assert_eq!(diagnostics.len(), 1);
        assert_eq!(diagnostics[0].line, 11);
        assert_eq!(
            diagnostics[0].message,
            "#client:sdk:src/api.ts::link names `url`"
        );
    }

    #[test]
    fn a_finding_from_before_the_change_is_left_out() {
        let old = finding(false, Some(("#client:sdk:src/api.ts", "12")), "old");

        let files = diagnose(&report(vec![old]), &[PathBuf::from("/work/server/link.ts")]);

        assert!(files.is_empty());
    }

    #[test]
    fn a_touched_file_summarizes_what_the_change_breaks_with_links() {
        let first = finding(true, Some(("#client:sdk:src/a.ts", "3")), "#client:sdk:a");
        let second = finding(true, Some(("#client:sdk:src/b.ts", "7")), "#client:sdk:b");
        let touched = PathBuf::from("/work/server/link.ts");

        let files = diagnose(&report(vec![first, second]), std::slice::from_ref(&touched));

        let summary = &files[&touched][0];
        assert_eq!(
            summary.message,
            "this change affects 2 places in other code: #client"
        );
        let linked: Vec<(&Path, u32)> = summary
            .related
            .iter()
            .map(|place| (place.path.as_path(), place.line))
            .collect();
        assert_eq!(
            linked,
            [
                (Path::new("/work/client/src/a.ts"), 2),
                (Path::new("/work/client/src/b.ts"), 6)
            ]
        );
    }

    #[test]
    fn a_finding_in_a_touched_file_is_not_summarized_there_again() {
        let own = finding(true, Some(("#server:api:link.ts", "4")), "#server:api:link");
        let touched = PathBuf::from("/work/server/link.ts");

        let files = diagnose(&report(vec![own]), std::slice::from_ref(&touched));

        let diagnostics = &files[&touched];
        assert_eq!(diagnostics.len(), 1);
        assert_eq!(diagnostics[0].line, 3);
        assert_eq!(diagnostics[0].related, Vec::<Place>::new());
    }

    #[test]
    fn a_finding_that_names_no_line_goes_on_each_touched_file() {
        let unlocated = finding(true, None, "the request schemas disagree");
        let touched = PathBuf::from("/work/server/link.ts");

        let files = diagnose(&report(vec![unlocated]), std::slice::from_ref(&touched));

        assert_eq!(files[&touched].len(), 1);
        assert_eq!(files[&touched][0].message, "the request schemas disagree");
        assert_eq!(files[&touched][0].line, 0);
    }

    #[test]
    fn a_file_in_a_repository_the_report_does_not_name_has_no_place() {
        let elsewhere = finding(true, Some(("#gone:sdk:src/a.ts", "3")), "elsewhere");

        let files = diagnose(&report(vec![elsewhere]), &[]);

        assert!(files.is_empty());
    }
}
