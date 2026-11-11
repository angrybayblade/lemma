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
        warm: None,
        environment: None,
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
    let cache = lemma_graph::repos::cache_directory();
    let build = build_digest(&executable, &cache.join("build-digests"))?;
    let facts = cache.join("facts");
    let directory = facts.join(&build[..16]);
    // Marks this build as the latest used, for `remove_older_builds`.
    let _ = std::fs::create_dir_all(&directory);
    let _ = std::fs::File::create(directory.join(".used"));
    remove_older_builds(&facts, KEPT_BUILDS);
    Some(lemma_verify::FactsCache { directory, build })
}

/// The digest of the executable at `executable`, remembered in `digests`
/// under the file's identity (size, change times, device and inode), so
/// it is read and hashed only after it changes: reading tens of megabytes
/// is most of a quick command's start. A digest that cannot be remembered
/// is only computed again next time.
fn build_digest(executable: &std::path::Path, digests: &std::path::Path) -> Option<String> {
    let metadata = std::fs::metadata(executable).ok()?;
    let identity = format!("{}\0{}", executable.display(), file_identity(&metadata));
    let remembered = digests.join(&lemma_model::content_hash(identity.as_bytes())[..32]);
    if let Ok(digest) = std::fs::read_to_string(&remembered)
        && digest.len() == 64
    {
        return Some(digest);
    }
    let digest = lemma_model::content_hash(&std::fs::read(executable).ok()?);
    // Written aside and renamed, so a reader never sees half a digest.
    let aside = remembered.with_extension(format!("{}.new", std::process::id()));
    let written = std::fs::create_dir_all(digests)
        .and_then(|()| std::fs::write(&aside, &digest))
        .and_then(|()| std::fs::rename(&aside, &remembered));
    if written.is_err() {
        let _ = std::fs::remove_file(&aside);
    }
    Some(digest)
}

/// What changes whenever a file's contents could have: its size and
/// modification time, and on Unix its change time, device and inode,
/// which writing it always moves.
fn file_identity(metadata: &std::fs::Metadata) -> String {
    let modified = metadata
        .modified()
        .ok()
        .and_then(|time| time.duration_since(std::time::UNIX_EPOCH).ok())
        .map_or(0, |since| since.as_nanos());
    #[cfg(unix)]
    {
        use std::os::unix::fs::MetadataExt;
        format!(
            "{} {modified} {}.{} {} {}",
            metadata.len(),
            metadata.ctime(),
            metadata.ctime_nsec(),
            metadata.dev(),
            metadata.ino()
        )
    }
    #[cfg(not(unix))]
    {
        format!("{} {modified}", metadata.len())
    }
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
