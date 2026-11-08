//! `lemma verify`: whether a description still matches the code. See
//! `.specs/16-verifying-descriptions.md`.

use std::io::Write;
use std::path::Path;
use std::process::ExitCode;

use lemma_graph::description::{DescriptionReport, verify_description};
use lemma_graph::{CheckOptions, Severity};
use lemma_logic::ReferenceEngine;
use lemma_souffle::SouffleEngine;

use crate::anchors::CheckReferences;
use crate::check::LanguageServers;
use crate::cli::VerifyArgs;

/// Exit code when a finding is an error.
const EXIT_VIOLATED: u8 = 4;
/// How many informational findings are listed without `--all`.
const SHOWN_WITHOUT_ALL: usize = 10;

pub(crate) fn run(args: &VerifyArgs, lsp: &LanguageServers, json: bool) -> ExitCode {
    let report = match verify(args, lsp) {
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
        print_text(&mut stdout, &report, args)
    };
    if printed.is_err() {
        return ExitCode::FAILURE;
    }
    let has_error = report
        .findings
        .iter()
        .any(|finding| finding.severity == Severity::Error);
    ExitCode::from(if has_error { EXIT_VIOLATED } else { 0 })
}

fn verify(args: &VerifyArgs, lsp: &LanguageServers) -> anyhow::Result<DescriptionReport> {
    let directory = entry_directory(&args.module);
    let options = CheckOptions {
        change: None,
        level: None,
        cache: lemma_graph::repos::cache_directory(),
        cloning: crate::cloning::permitted(false, directory)?,
        explain: false,
        relations: args.relations.clone(),
        facts_cache: facts_cache(),
        evaluation: lemma_verify::Evaluation::Full,
        list_links: true,
        run_checkers: true,
        runs_base_checkers: false,
        select: None,
    };
    let mut references = CheckReferences::new(lsp.asks_daemon, lsp.providers_file.clone());
    let report = if let Ok(souffle) = SouffleEngine::find() {
        verify_description(&args.module, &options, &souffle, &mut references)?
    } else {
        verify_description(&args.module, &options, &ReferenceEngine, &mut references)?
    };
    Ok(report)
}

fn entry_directory(module: &Path) -> &Path {
    match module.parent() {
        Some(directory) if !directory.as_os_str().is_empty() => directory,
        _ => Path::new("."),
    }
}

fn print_text(
    out: &mut dyn Write,
    report: &DescriptionReport,
    args: &VerifyArgs,
) -> std::io::Result<()> {
    let label = |severity: Severity| match severity {
        Severity::Error => "error",
        Severity::Warning => "warning",
        Severity::Info => "info",
    };
    let count = |severity: Severity| {
        report
            .findings
            .iter()
            .filter(|finding| finding.severity == severity)
            .count()
    };
    let mut infos_shown = 0;
    for finding in &report.findings {
        if finding.severity == Severity::Info {
            infos_shown += 1;
            if infos_shown > SHOWN_WITHOUT_ALL && !args.all {
                continue;
            }
        }
        writeln!(
            out,
            "{:<7} {}  {}",
            label(finding.severity),
            finding.location,
            finding.message
        )?;
        writeln!(out, "        [{}]", finding.name)?;
    }
    if let Some(list) = report.link_list.as_ref().filter(|_| args.links) {
        crate::check::print_link_list(out, list, args.all)?;
    }
    for skipped in &report.not_checked {
        writeln!(out, "not looked at: {} (not cloned)", skipped.repository)?;
    }
    for (name, rows) in &report.relations {
        writeln!(out, "{name}: {} rows", rows.len())?;
        for row in rows {
            writeln!(out, "  {}", row.join("\t"))?;
        }
    }
    writeln!(
        out,
        "{} errors, {} warnings, {} informational",
        count(Severity::Error),
        count(Severity::Warning),
        count(Severity::Info)
    )
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
    let directory = facts.join(&build[..16]);
    // Marks this build as the latest used, for `remove_older_builds`.
    let _ = std::fs::create_dir_all(&directory);
    let _ = std::fs::File::create(directory.join(".used"));
    remove_older_builds(&facts, KEPT_BUILDS);
    Some(lemma_verify::FactsCache { directory, build })
}

/// How many builds' facts are kept: an installed Lemma and a development
/// build used in turn keep theirs, older releases' go.
const KEPT_BUILDS: usize = 3;

/// Removes the facts of all but the `kept` builds used most recently: an
/// older build's facts are not read again, and each is the size of every
/// repository's facts. Removing is tidying, so a failure only leaves them
/// for the next run.
fn remove_older_builds(facts: &std::path::Path, kept: usize) {
    let Ok(entries) = std::fs::read_dir(facts) else {
        return;
    };
    let mut builds: Vec<(std::time::SystemTime, std::path::PathBuf)> = entries
        .filter_map(Result::ok)
        .map(|entry| {
            let used = std::fs::metadata(entry.path().join(".used"))
                .and_then(|metadata| metadata.modified())
                .unwrap_or(std::time::UNIX_EPOCH);
            (used, entry.path())
        })
        .collect();
    builds.sort_by(|left, right| right.cmp(left));
    for (_, path) in builds.into_iter().skip(kept) {
        let _ = std::fs::remove_dir_all(path);
    }
}
