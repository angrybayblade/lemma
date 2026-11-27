//! `lemma check --change <repository> --last N`: checks the repository's
//! last contract changes one by one, each against the other repositories
//! as they stood then, and says what each would have been told and whether
//! the code it was told about was changed afterwards. A team reads it
//! before letting findings fail a build; it fails none itself.

use std::collections::BTreeMap;
use std::io::Write;
use std::path::Path;
use std::process::ExitCode;

use lemma_graph::config::Level;
use lemma_graph::history::{
    Commit, LaterChange, PastChange, is_named_at, later_change, past_changes, rests_on,
};
use lemma_graph::{Change, CheckOptions, Finding, Report, Severity};

use crate::cli::CheckArgs;

/// Seconds in a day, for saying how long after a change its code moved.
const DAY_SECONDS: i64 = 86_400;

/// One past change and what checking it found.
#[derive(serde::Serialize)]
struct CheckedChange {
    #[serde(flatten)]
    change: PastChange,
    /// Why it was not checked, when it was not.
    #[serde(skip_serializing_if = "Option::is_none")]
    not_checked: Option<String>,
    /// The errors and warnings it introduced.
    findings: Vec<FollowedFinding>,
}

/// A finding on a past change, and what happened to its code afterwards.
#[derive(serde::Serialize)]
struct FollowedFinding {
    #[serde(flatten)]
    finding: Finding,
    /// Present for a finding that names code Lemma can follow.
    #[serde(skip_serializing_if = "Option::is_none")]
    afterwards: Option<Afterwards>,
}

/// What happened, after a change, to the code one of its findings rests
/// on.
#[derive(serde::Serialize)]
struct Afterwards {
    /// The file, from its repository's root.
    path: String,
    /// The names looked for in its lines.
    names: Vec<String>,
    /// The first later commit that changed the lines naming them; `None`
    /// when none has.
    changed_in: Option<LaterChange>,
    /// How many whole days after the change that commit was made.
    #[serde(skip_serializing_if = "Option::is_none")]
    days_after: Option<i64>,
}

/// Checks the last `last` changes of the repository `args` name and prints
/// each as it is checked, or all of them as JSON at the end.
pub(crate) fn run(args: &CheckArgs, last: usize, json: bool) -> ExitCode {
    let mut out = std::io::stdout().lock();
    match checked(args, last, json, &mut out) {
        Ok(could_check_all) if could_check_all => ExitCode::SUCCESS,
        Ok(_) => ExitCode::from(1),
        Err(error) => {
            eprintln!("error: {error:#}");
            ExitCode::from(1)
        }
    }
}

/// [`run`], returning whether every change that could be checked was.
fn checked(args: &CheckArgs, last: usize, json: bool, out: &mut dyn Write) -> anyhow::Result<bool> {
    let (manifest, options) = crate::check::options_for(args, None)?;
    let options = for_history(options, args);
    let changes = past_changes(&manifest, &options, last)?;
    let mut all = Vec::new();
    for change in changes {
        let checked = check_change(&manifest, args, &options, change);
        if !json {
            print_change(out, &checked)?;
        }
        all.push(checked);
    }
    if json {
        serde_json::to_writer_pretty(&mut *out, &all)?;
        writeln!(out)?;
    } else {
        print_counts(out, &all)?;
    }
    Ok(all
        .iter()
        .all(|change| change.not_checked.is_none() || !change.change.not_yet_there.is_empty()))
}

/// `options` as history is checked: at the level that fails a build unless
/// another was asked for, since the question is what would have failed,
/// and with no checker run, since a past commit's tests are not this
/// reading's to start.
fn for_history(mut options: CheckOptions, args: &CheckArgs) -> CheckOptions {
    if args.level.is_none() {
        options.level = Some(Level::Check);
    }
    options.run_checkers = false;
    options.runs_base_checkers = false;
    options
}

/// Checks one past change against the system of its time.
fn check_change(
    manifest: &Path,
    args: &CheckArgs,
    options: &CheckOptions,
    change: PastChange,
) -> CheckedChange {
    if !change.not_yet_there.is_empty() {
        let missing = change.not_yet_there.join(", ");
        return CheckedChange {
            not_checked: Some(format!("{missing} had no commit yet")),
            change,
            findings: Vec::new(),
        };
    }
    let mut options = options.clone();
    options.pins.clone_from(&change.pins);
    if let Some(asked) = &mut options.change {
        *asked = Change {
            repository: asked.repository.clone(),
            base: Some(format!("{}^", change.commit.hash)),
            head: Some(change.commit.hash.clone()),
        };
    }
    match crate::check::check(manifest, args, &options) {
        Ok(report) => CheckedChange {
            findings: followed(&report, &change.commit),
            change,
            not_checked: None,
        },
        Err(error) => CheckedChange {
            change,
            not_checked: Some(format!("{error:#}")),
            findings: Vec::new(),
        },
    }
}

/// The errors and warnings `report`'s change introduced, each with what
/// happened afterwards to the code it rests on.
fn followed(report: &Report, change: &Commit) -> Vec<FollowedFinding> {
    report
        .findings
        .iter()
        .filter(|finding| finding.introduced && finding.severity != Severity::Info)
        .map(|finding| FollowedFinding {
            afterwards: afterwards(finding, report, change),
            finding: finding.clone(),
        })
        .collect()
}

/// What happened after `change` to the code `finding` rests on; `None` for
/// a finding that names no code, code read from a working directory, which
/// has no later commit, or a file that does not write the name itself. A history that cannot be read says nothing
/// rather than failing the change's report.
fn afterwards(finding: &Finding, report: &Report, change: &Commit) -> Option<Afterwards> {
    let (repository, path) = rests_on(finding, &report.repositories)?;
    let read_at = repository.head.as_deref()?;
    let names = shown_names(finding);
    let root = Path::new(&repository.root);
    if !is_named_at(root, read_at, &path, &names) {
        return None;
    }
    let changed_in = later_change(root, read_at, &path, &names).ok()?;
    let days_after = changed_in.as_ref().map(|later| {
        (later.commit.committed_at_seconds - change.committed_at_seconds).div_euclid(DAY_SECONDS)
    });
    Some(Afterwards {
        path,
        names: names.into_iter().map(str::to_owned).collect(),
        changed_in,
        days_after,
    })
}

/// The names in code `finding` is about that its message shows the reader
/// in backticks: a removed field or parameter, and not a word a rule also
/// binds, such as where a parameter is sent.
fn shown_names(finding: &Finding) -> Vec<&str> {
    lemma_graph::names::named_in(&finding.values)
        .into_iter()
        .filter(|name| finding.message.contains(&format!("`{name}`")))
        .collect()
}

/// A commit's hash as people write it.
fn short(hash: &str) -> &str {
    hash.get(..10).unwrap_or(hash)
}

fn print_change(out: &mut dyn Write, checked: &CheckedChange) -> std::io::Result<()> {
    let commit = &checked.change.commit;
    writeln!(
        out,
        "{} {} {}",
        short(&commit.hash),
        commit.committed_on,
        commit.subject
    )?;
    if let Some(reason) = &checked.not_checked {
        return writeln!(out, "  not checked: {reason}");
    }
    if checked.findings.is_empty() {
        return writeln!(out, "  introduced no error or warning");
    }
    for followed in &checked.findings {
        print_followed(out, followed)?;
    }
    Ok(())
}

fn print_followed(out: &mut dyn Write, followed: &FollowedFinding) -> std::io::Result<()> {
    let finding = &followed.finding;
    let label = match finding.severity {
        Severity::Error => "error",
        Severity::Warning => "warning",
        Severity::Info => "info",
    };
    let mut lines = finding.message.lines();
    writeln!(out, "  {label:<7} {}", lines.next().unwrap_or_default())?;
    for line in lines {
        writeln!(out, "          {line}")?;
    }
    writeln!(out, "          [{}] {}", finding.check, finding.link)?;
    let Some(afterwards) = &followed.afterwards else {
        return Ok(());
    };
    let names = afterwards
        .names
        .iter()
        .map(|name| format!("`{name}`"))
        .collect::<Vec<_>>()
        .join(" or ");
    match (&afterwards.changed_in, afterwards.days_after) {
        (Some(later), Some(days)) => writeln!(
            out,
            "          afterwards: {} changed the lines naming {names} in {} ({} removed, {} added), {} ({})",
            short(&later.commit.hash),
            afterwards.path,
            later.lines_removed,
            later.lines_added,
            days_later(days),
            later.commit.subject
        ),
        _ => writeln!(
            out,
            "          afterwards: no later commit changed a line naming {names} in {}",
            afterwards.path
        ),
    }
}

fn days_later(days: i64) -> String {
    match days {
        0 => "the same day".to_owned(),
        1 => "1 day later".to_owned(),
        days => format!("{days} days later"),
    }
}

fn print_counts(out: &mut dyn Write, all: &[CheckedChange]) -> std::io::Result<()> {
    let mut counts: BTreeMap<&str, usize> = BTreeMap::new();
    for checked in all {
        let has = |severity| {
            checked
                .findings
                .iter()
                .any(|followed| followed.finding.severity == severity)
        };
        let kind = if checked.not_checked.is_some() {
            "not checked"
        } else if has(Severity::Error) {
            "introduced errors"
        } else if has(Severity::Warning) {
            "introduced warnings only"
        } else {
            "introduced neither"
        };
        *counts.entry(kind).or_default() += 1;
    }
    let count = |kind| counts.get(kind).copied().unwrap_or_default();
    writeln!(
        out,
        "\n{} {}: {} introduced errors, {} introduced warnings only, {} introduced neither, {} not checked",
        all.len(),
        if all.len() == 1 { "change" } else { "changes" },
        count("introduced errors"),
        count("introduced warnings only"),
        count("introduced neither"),
        count("not checked"),
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_later_commit_is_dated_in_whole_days_after_the_change() {
        assert_eq!(days_later(0), "the same day");
        assert_eq!(days_later(1), "1 day later");
        assert_eq!(days_later(7), "7 days later");
    }
}
