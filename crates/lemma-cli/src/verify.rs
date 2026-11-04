//! `lemma verify`: checks a change across the repositories in a manifest.

use std::io::Write;
use std::process::ExitCode;

use lemma_logic::ReferenceEngine;
use lemma_souffle::SouffleEngine;
use lemma_verify::{Manifest, Options, Report, VerifyError};

use crate::cli::VerifyArgs;

/// Exit code when a finding is an error.
const EXIT_VIOLATED: u8 = 4;

pub(crate) fn run(args: &VerifyArgs, json: bool) -> ExitCode {
    match verify(args) {
        Ok(report) => {
            let mut stdout = std::io::stdout().lock();
            let printed = if json {
                print_json(&mut stdout, &report)
            } else {
                print_text(&mut stdout, &report, args.all)
            };
            if printed.is_err() {
                return ExitCode::FAILURE;
            }
            let violated = report
                .findings
                .iter()
                .any(|finding| finding.severity == "error");
            ExitCode::from(if violated { EXIT_VIOLATED } else { 0 })
        }
        Err(error) => {
            eprintln!("error: {error:#}");
            ExitCode::FAILURE
        }
    }
}

/// Lemma's cache of facts read from code, for this build of Lemma: the
/// build is named by a digest of its executable, so a rebuild never reads
/// facts an older extractor wrote. `None` when the executable cannot be
/// read, which only means nothing is kept.
pub(crate) fn facts_cache() -> Option<lemma_verify::FactsCache> {
    let executable = std::env::current_exe().ok()?;
    let bytes = std::fs::read(executable).ok()?;
    let build = lemma_model::content_hash(&bytes);
    let facts = lemma_graph::repos::cache_directory().join("facts");
    remove_other_builds(&facts, &build[..16]);
    Some(lemma_verify::FactsCache {
        directory: facts.join(&build[..16]),
        build,
    })
}

/// Removes the facts other builds of Lemma kept: they are never read
/// again, and each is the size of every repository's facts. Removing is
/// tidying, so a failure only leaves them for the next run.
fn remove_other_builds(facts: &std::path::Path, build: &str) {
    let Ok(entries) = std::fs::read_dir(facts) else {
        return;
    };
    for entry in entries.filter_map(Result::ok) {
        if entry.file_name() != build {
            let _ = std::fs::remove_dir_all(entry.path());
        }
    }
}

fn verify(args: &VerifyArgs) -> anyhow::Result<Report> {
    let mut manifest = Manifest::load(&args.manifest)?;
    if let Some(base) = &args.base {
        manifest.change.base.clone_from(base);
    }
    if let Some(head) = &args.head {
        manifest.change.head = Some(head.clone());
    }
    let options = Options {
        explain: args.explain,
        relations: args.relations.clone(),
        facts_cache: facts_cache(),
    };
    let report: Result<Report, VerifyError> = match args.engine.as_deref() {
        Some("reference") => lemma_verify::verify(&manifest, &ReferenceEngine, &options),
        Some("souffle") => lemma_verify::verify(&manifest, &SouffleEngine::find()?, &options),
        Some(other) => anyhow::bail!("unknown engine `{other}`; use `souffle` or `reference`"),
        None => {
            if let Ok(souffle) = SouffleEngine::find() {
                lemma_verify::verify(&manifest, &souffle, &options)
            } else {
                eprintln!("souffle is not installed; using the reference engine");
                lemma_verify::verify(&manifest, &ReferenceEngine, &options)
            }
        }
    };
    Ok(report?)
}

fn print_json(out: &mut dyn Write, report: &Report) -> std::io::Result<()> {
    serde_json::to_writer_pretty(&mut *out, report)?;
    writeln!(out)
}

fn print_text(out: &mut dyn Write, report: &Report, all: bool) -> std::io::Result<()> {
    let facts: usize = report.facts.values().sum();
    writeln!(
        out,
        "change {}  ({facts} facts, {} engine)",
        report.change, report.engine
    )?;
    let count = |severity: &str| {
        report
            .findings
            .iter()
            .filter(|finding| finding.severity == severity)
            .count()
    };
    for finding in &report.findings {
        if finding.severity == "info" && !all {
            continue;
        }
        writeln!(out, "{:<7} {}", finding.severity, finding.message)?;
        writeln!(out, "        [{}]", finding.requirement)?;
        if let Some(derivation) = &finding.derivation {
            for line in derivation.lines() {
                writeln!(out, "          {line}")?;
            }
        }
    }
    let infos = count("info");
    let hidden = if all || infos == 0 {
        String::new()
    } else {
        " (--all lists them)".to_owned()
    };
    writeln!(
        out,
        "{} errors, {} warnings, {infos} informational{hidden}",
        count("error"),
        count("warning")
    )?;
    for (name, rows) in &report.relations {
        writeln!(out, "{name}: {} rows", rows.len())?;
        for row in rows {
            writeln!(out, "  {}", row.join("\t"))?;
        }
    }
    Ok(())
}
