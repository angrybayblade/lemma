//! `lemma check`: checks the links declared in composable lemma.yaml files.

use std::io::Write;
use std::process::ExitCode;

use lemma_graph::config::Level;
use lemma_graph::{Change, CheckOptions, GraphError, Impact, Report, Severity};
use lemma_logic::ReferenceEngine;
use lemma_souffle::SouffleEngine;

use crate::cli::CheckArgs;

/// Exit code when a finding is an error.
const EXIT_VIOLATED: u8 = 4;

pub(crate) fn run(args: &CheckArgs, json: bool) -> ExitCode {
    let change = args.change.as_ref().map(|repository| Change {
        repository: repository.clone(),
        base: args.base.clone().unwrap_or_default(),
        head: args.head.clone().unwrap_or_default(),
    });
    let options = CheckOptions {
        change,
        level: args.level.as_deref().map(|level| match level {
            "check" => Level::Check,
            _ => Level::Audit,
        }),
        cache: lemma_graph::repos::cache_directory(),
    };
    let report = match check(args, &options) {
        Ok(report) => report,
        Err(error) => {
            eprintln!("error: {error:#}");
            return ExitCode::FAILURE;
        }
    };
    let mut stdout = std::io::stdout().lock();
    let printed = if json {
        serde_json::to_writer_pretty(&mut stdout, &report)
            .map_err(std::io::Error::other)
            .and_then(|()| writeln!(stdout))
    } else {
        print_text(&mut stdout, &report, args.all)
    };
    if printed.is_err() {
        return ExitCode::FAILURE;
    }
    let violated = report
        .findings
        .iter()
        .any(|finding| finding.severity == Severity::Error);
    ExitCode::from(if violated { EXIT_VIOLATED } else { 0 })
}

fn check(args: &CheckArgs, options: &CheckOptions) -> anyhow::Result<Report> {
    let manifest = &args.manifest;
    let report: Result<Report, GraphError> = match args.engine.as_deref() {
        Some("reference") => lemma_graph::check(manifest, options, &ReferenceEngine),
        Some("souffle") => lemma_graph::check(manifest, options, &SouffleEngine::find()?),
        Some(other) => anyhow::bail!("unknown engine `{other}`; use `souffle` or `reference`"),
        None => match SouffleEngine::find() {
            Ok(souffle) => lemma_graph::check(manifest, options, &souffle),
            Err(_) => lemma_graph::check(manifest, options, &ReferenceEngine),
        },
    };
    Ok(report?)
}

fn print_text(out: &mut dyn Write, report: &Report, all: bool) -> std::io::Result<()> {
    let level = match report.level {
        Level::Audit => "audit",
        Level::Check => "check",
    };
    let inferred = report
        .inferred_links
        .map_or_else(String::new, |count| format!(", {count} inferred"));
    writeln!(out, "{} links{inferred}, level {level}", report.links)?;
    print_repositories(out, report)?;
    let count = |severity: Severity| {
        report
            .findings
            .iter()
            .filter(|finding| finding.severity == severity)
            .count()
    };
    for finding in &report.findings {
        if finding.severity == Severity::Info && !all {
            continue;
        }
        let label = match finding.severity {
            Severity::Error => "error",
            Severity::Warning => "warning",
            Severity::Info => "info",
        };
        let origin = if finding.introduced {
            "introduced"
        } else {
            "already there"
        };
        writeln!(out, "{label:<7} {}", finding.message)?;
        writeln!(
            out,
            "        [{}] {}  ({origin})",
            finding.check, finding.link
        )?;
        print_affects(out, &finding.affects)?;
    }
    let infos = count(Severity::Info);
    let hidden = if all || infos == 0 {
        ""
    } else {
        " (--all lists them)"
    };
    writeln!(
        out,
        "{} errors, {} warnings, {infos} informational{hidden}",
        count(Severity::Error),
        count(Severity::Warning)
    )
}

/// How many affected items to list per service or repository.
const LISTED_PER_OWNER: usize = 3;

/// What a problem affects, by service or repository, nearest first.
fn print_affects(out: &mut dyn Write, affects: &[Impact]) -> std::io::Result<()> {
    if affects.is_empty() {
        return Ok(());
    }
    writeln!(out, "        affects (traced by name):")?;
    let mut owners: Vec<(&str, Vec<&Impact>)> = Vec::new();
    for impact in affects {
        match owners.iter_mut().find(|(owner, _)| *owner == impact.owner) {
            Some((_, items)) => items.push(impact),
            None => owners.push((&impact.owner, vec![impact])),
        }
    }
    for (owner, items) in owners {
        let listed: Vec<String> = items
            .iter()
            .take(LISTED_PER_OWNER)
            .map(|impact| match &impact.calls {
                Some(operation) => format!("{} -> {operation}", impact.at),
                None => impact.at.clone(),
            })
            .collect();
        let more = items.len().saturating_sub(LISTED_PER_OWNER);
        let more = if more > 0 {
            format!(", and {more} more")
        } else {
            String::new()
        };
        writeln!(
            out,
            "          {owner} ({}): {}{more}",
            items.len(),
            listed.join("; ")
        )?;
    }
    Ok(())
}

fn print_repositories(out: &mut dyn Write, report: &Report) -> std::io::Result<()> {
    let at = |commit: &Option<String>| {
        commit.as_deref().map_or_else(
            || "working directory".to_owned(),
            |commit| commit[..commit.len().min(10)].to_owned(),
        )
    };
    for repository in &report.repositories {
        let commits = if repository.base == repository.head {
            at(&repository.head)
        } else {
            format!("{}..{}", at(&repository.base), at(&repository.head))
        };
        let note = if repository.manifest_from_working_directory {
            "  (lemma.yaml from the working directory)"
        } else {
            ""
        };
        writeln!(out, "  {:<14} {commits}{note}", repository.name)?;
    }
    Ok(())
}
