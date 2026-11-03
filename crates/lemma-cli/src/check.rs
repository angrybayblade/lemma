//! `lemma check`: checks the links declared in composable lemma.yaml files.

use std::io::Write;
use std::process::ExitCode;

use lemma_graph::config::Level;
use lemma_graph::{Change, CheckOptions, Report, Severity};

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
    let report = match lemma_graph::check(&args.manifest, &options) {
        Ok(report) => report,
        Err(error) => {
            eprintln!("error: {error:#}");
            let mut source = std::error::Error::source(&error);
            while let Some(cause) = source {
                eprintln!("  caused by: {cause}");
                source = cause.source();
            }
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

fn print_text(out: &mut dyn Write, report: &Report, all: bool) -> std::io::Result<()> {
    let level = match report.level {
        Level::Audit => "audit",
        Level::Check => "check",
    };
    writeln!(out, "{} links, level {level}", report.links)?;
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
