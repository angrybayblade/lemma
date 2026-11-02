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

fn verify(args: &VerifyArgs) -> anyhow::Result<Report> {
    let mut manifest = Manifest::load(&args.manifest)?;
    if let Some(base) = &args.base {
        manifest.change.base.clone_from(base);
    }
    if let Some(head) = &args.head {
        manifest.change.head.clone_from(head);
    }
    let options = Options {
        explain: args.explain,
        relations: args.relations.clone(),
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
