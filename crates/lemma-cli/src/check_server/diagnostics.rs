//! What a change's report says about each file, for `lemma lsp`.
//!
//! Pure: a report and the lines the change touched go in, diagnostics by
//! file come out. Asking Git for those lines, reading files to place a
//! diagnostic within its line, and speaking LSP happen in the server.

use std::collections::{BTreeMap, BTreeSet};
use std::path::{Path, PathBuf};

use lemma_graph::{Report, Severity};

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

/// What checking a change found, as the server reads it: from a report in
/// this process, or from the JSON of one sentry answered.
#[derive(Debug, Clone, PartialEq, Eq, serde::Deserialize)]
pub(crate) struct CheckedChange {
    pub repositories: Vec<CheckedRepository>,
    pub findings: Vec<CheckedFinding>,
}

/// A repository a check read.
#[derive(Debug, Clone, PartialEq, Eq, serde::Deserialize)]
pub(crate) struct CheckedRepository {
    /// As findings spell it: `#billing`, or `self`.
    pub name: String,
    pub root: String,
    /// The commit the change was compared against, for the changed one.
    pub base: Option<String>,
}

/// One finding of a check.
#[derive(Debug, Clone, PartialEq, Eq, serde::Deserialize)]
pub(crate) struct CheckedFinding {
    pub severity: Severity,
    /// The requirement or check that found it.
    pub check: String,
    pub message: String,
    pub introduced: bool,
    /// The value of each variable its requirement binds.
    #[serde(default)]
    pub values: BTreeMap<String, String>,
}

impl From<&Report> for CheckedChange {
    fn from(report: &Report) -> Self {
        let repositories = report
            .repositories
            .iter()
            .map(|repository| CheckedRepository {
                name: repository.name.clone(),
                root: repository.root.clone(),
                base: repository.base.clone(),
            })
            .collect();
        let findings = report
            .findings
            .iter()
            .map(|finding| CheckedFinding {
                severity: finding.severity,
                check: finding.check.clone(),
                message: finding.message.clone(),
                introduced: finding.introduced,
                values: finding.values.clone(),
            })
            .collect();
        Self {
            repositories,
            findings,
        }
    }
}

/// Diagnostics by absolute file path, each file's sorted.
pub(crate) type FileDiagnostics = BTreeMap<PathBuf, Vec<LineDiagnostic>>;

/// A file a change touched, and the lines it changed: none for a file Git
/// does not know, which is new as a whole.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct ChangedFile {
    pub path: PathBuf,
    pub lines: Vec<ChangedLine>,
}

/// A line a change added, or the line where it removed some, with the text
/// added or removed.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct ChangedLine {
    /// Counted from zero, in the file as it is now.
    pub line: u32,
    pub text: String,
}

/// Where a change answers for findings: a changed line of a file.
type Anchor = (PathBuf, u32);

/// What one changed line answers for: the places elsewhere its findings
/// are at, each with its repository and severity, and the findings that
/// name no place.
#[derive(Default)]
struct Answerable<'a> {
    elsewhere: Vec<(&'a str, Severity, Place)>,
    unlocated: Vec<&'a CheckedFinding>,
}

/// The diagnostics one repository's change gives: each finding the change
/// introduced, on the line of code it names, and on the changed lines it
/// comes from (see [`anchors`]) a summary linking to every such line
/// outside the changed files, together with the introduced findings that
/// name no line. Findings that were there before the change are left out:
/// they are not the change's to fix.
pub(crate) fn diagnose(report: &CheckedChange, changed: &[ChangedFile]) -> FileDiagnostics {
    let roots = repository_roots(report);
    let introduced = report.findings.iter().filter(|finding| finding.introduced);
    let mut files = FileDiagnostics::new();
    let mut answerable: BTreeMap<Anchor, Answerable<'_>> = BTreeMap::new();
    for finding in introduced {
        let anchors = anchors(finding, changed);
        let Some(location) = finding_location(finding, &roots) else {
            for anchor in anchors {
                answerable
                    .entry(anchor)
                    .or_default()
                    .unlocated
                    .push(finding);
            }
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
                related: changed_here(&anchors, &place.path),
            });
        if changed.iter().any(|file| file.path == place.path) {
            continue;
        }
        for anchor in anchors {
            let entry = (location.repository, finding.severity, place.clone());
            answerable.entry(anchor).or_default().elsewhere.push(entry);
        }
    }
    add_answers(&mut files, answerable);
    files.retain(|_, diagnostics| !diagnostics.is_empty());
    for diagnostics in files.values_mut() {
        diagnostics.sort();
        diagnostics.dedup();
    }
    files
}

/// Adds to `files`, on each changed line, what it answers for: a summary of
/// the places elsewhere its findings are at, and each finding that names no
/// place.
fn add_answers(files: &mut FileDiagnostics, answerable: BTreeMap<Anchor, Answerable<'_>>) {
    for ((path, line), mut answered) in answerable {
        let diagnostics = files.entry(path).or_default();
        answered.elsewhere.sort();
        answered.elsewhere.dedup();
        if !answered.elsewhere.is_empty() {
            diagnostics.push(summary(line, &answered.elsewhere));
        }
        for finding in answered.unlocated {
            diagnostics.push(LineDiagnostic {
                line,
                severity: finding.severity,
                code: finding.check.clone(),
                message: finding.message.clone(),
                related: Vec::new(),
            });
        }
    }
}

/// Each repository's name as findings spell it (`#billing`, `self`), with
/// its directory.
fn repository_roots(report: &CheckedChange) -> BTreeMap<&str, &Path> {
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
    finding: &CheckedFinding,
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
fn short_message(finding: &CheckedFinding) -> String {
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

/// The changed lines a finding comes from. A finding names what it is about
/// in the values its requirement binds, such as a field or a parameter:
/// the changed lines that name the longest such value, one per file, as a
/// change to `connection_slug` is the line that added or removed it. A
/// finding that names nothing the change touched comes from the first
/// changed line of each changed file; a file Git does not know, from its
/// top.
fn anchors(finding: &CheckedFinding, changed: &[ChangedFile]) -> Vec<Anchor> {
    let mut names: Vec<&str> = finding
        .values
        .iter()
        .filter(|(variable, _)| !["f", "l"].contains(&variable.as_str()))
        .map(|(_, value)| value.as_str())
        .filter(|value| is_identifier(value))
        .collect();
    names.sort_by_key(|name| std::cmp::Reverse(name.len()));
    for name in names {
        let naming: Vec<Anchor> = changed
            .iter()
            .filter_map(|file| {
                let line = file.lines.iter().find(|line| mentions(&line.text, name))?;
                Some((file.path.clone(), line.line))
            })
            .collect();
        if !naming.is_empty() {
            return naming;
        }
    }
    changed
        .iter()
        .map(|file| {
            (
                file.path.clone(),
                file.lines.first().map_or(0, |line| line.line),
            )
        })
        .collect()
}

/// Whether `value` reads as a name in code: a field, a parameter or a
/// function, rather than a path, an operation or a number.
fn is_identifier(value: &str) -> bool {
    let mut characters = value.chars();
    characters
        .next()
        .is_some_and(|first| first.is_ascii_alphabetic() || first == '_')
        && characters.all(|character| character.is_ascii_alphanumeric() || character == '_')
}

/// Whether `text` holds `name` as a word of its own.
fn mentions(text: &str, name: &str) -> bool {
    let is_word = |character: char| character.is_ascii_alphanumeric() || character == '_';
    text.match_indices(name).any(|(start, _)| {
        let before = text[..start].chars().next_back();
        let after = text[start + name.len()..].chars().next();
        !before.is_some_and(is_word) && !after.is_some_and(is_word)
    })
}

/// The changed lines a finding in `own` comes from, outside `own`, as
/// places its diagnostic links back to.
fn changed_here(anchors: &[Anchor], own: &Path) -> Vec<Place> {
    anchors
        .iter()
        .filter(|(path, _)| path.as_path() != own)
        .take(RELATED_LIMIT)
        .map(|(path, line)| Place {
            path: path.clone(),
            line: *line,
            message: "changed here".to_owned(),
        })
        .collect()
}

/// What a change breaks, in one diagnostic linking to each place, given
/// with its repository and how serious the finding there is: as serious as
/// the most serious of them.
fn summary(line: u32, located: &[(&str, Severity, Place)]) -> LineDiagnostic {
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
        line,
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
    use lemma_graph::{Finding, ResolvedRepository, Severity};

    fn repository(name: &str, root: &str) -> ResolvedRepository {
        ResolvedRepository {
            name: name.to_owned(),
            root: root.to_owned(),
            base: None,
            head: None,
            manifest_from_working_directory: false,
        }
    }

    /// A changed file whose changed lines are `lines`, each a zero-based
    /// line and its text.
    fn changed(path: &str, lines: &[(u32, &str)]) -> ChangedFile {
        ChangedFile {
            path: PathBuf::from(path),
            lines: lines
                .iter()
                .map(|(line, text)| ChangedLine {
                    line: *line,
                    text: (*text).to_owned(),
                })
                .collect(),
        }
    }

    /// `finding` binding `value` to `variable` too.
    fn binding(mut finding: Finding, variable: &str, value: &str) -> Finding {
        finding.values.insert(variable.to_owned(), value.to_owned());
        finding
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

    fn report(findings: Vec<Finding>) -> CheckedChange {
        CheckedChange::from(&whole_report(findings))
    }

    fn whole_report(findings: Vec<Finding>) -> Report {
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
    fn a_report_read_back_from_its_json_is_the_one_checked_here() {
        let located = finding(true, Some(("#client:sdk:src/api.ts", "12")), "names `url`");
        let whole = whole_report(vec![binding(located, "leaf", "url")]);

        let json = serde_json::to_string(&whole).unwrap();
        let read: CheckedChange = serde_json::from_str(&json).unwrap();

        assert_eq!(read, CheckedChange::from(&whole));
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

        let files = diagnose(&report(vec![old]), &[changed("/work/server/link.ts", &[])]);

        assert!(files.is_empty());
    }

    #[test]
    fn a_touched_file_summarizes_what_the_change_breaks_with_links() {
        let first = finding(true, Some(("#client:sdk:src/a.ts", "3")), "#client:sdk:a");
        let second = finding(true, Some(("#client:sdk:src/b.ts", "7")), "#client:sdk:b");
        let touched = PathBuf::from("/work/server/link.ts");
        let change = changed("/work/server/link.ts", &[(9, "  url: z.string(),")]);

        let files = diagnose(&report(vec![first, second]), &[change]);

        let summary = &files[&touched][0];
        assert_eq!(summary.line, 9);
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
        let change = changed("/work/server/link.ts", &[(3, "  link: z.string(),")]);

        let files = diagnose(&report(vec![own]), &[change]);

        let diagnostics = &files[&touched];
        assert_eq!(diagnostics.len(), 1);
        assert_eq!(diagnostics[0].line, 3);
        assert_eq!(diagnostics[0].related, Vec::<Place>::new());
    }

    #[test]
    fn a_summary_sits_on_the_changed_line_naming_what_it_is_about() {
        let caller = finding(true, Some(("#client:sdk:src/a.ts", "3")), "#client:sdk:a");
        let caller = binding(binding(caller, "leaf", "connection_slug"), "loc", "body");
        let touched = PathBuf::from("/work/server/link.ts");
        let change = changed(
            "/work/server/link.ts",
            &[
                (4, "  body: z.object({"),
                (24, "  connection_slug: z.string(),"),
            ],
        );

        let files = diagnose(&report(vec![caller]), &[change]);

        assert_eq!(files[&touched][0].line, 24);
        let back = &files[Path::new("/work/client/src/a.ts")][0].related;
        assert_eq!(
            (back[0].path.as_path(), back[0].line),
            (touched.as_path(), 24)
        );
    }

    #[test]
    fn a_finding_naming_nothing_changed_goes_on_the_first_changed_line() {
        let unlocated = finding(true, None, "the request schemas disagree");
        let touched = PathBuf::from("/work/server/link.ts");
        let change = changed("/work/server/link.ts", &[(7, "  a: 1,"), (30, "  b: 2,")]);

        let files = diagnose(&report(vec![unlocated]), &[change]);

        assert_eq!(files[&touched].len(), 1);
        assert_eq!(files[&touched][0].message, "the request schemas disagree");
        assert_eq!(files[&touched][0].line, 7);
    }

    #[test]
    fn a_name_inside_a_longer_word_is_not_a_mention() {
        assert!(mentions("  user_id: z.string(),", "user_id"));
        assert!(!mentions("  owner_user_id: z.string(),", "user_id"));
    }

    #[test]
    fn a_file_in_a_repository_the_report_does_not_name_has_no_place() {
        let elsewhere = finding(true, Some(("#gone:sdk:src/a.ts", "3")), "elsewhere");

        let files = diagnose(&report(vec![elsewhere]), &[]);

        assert!(files.is_empty());
    }
}
