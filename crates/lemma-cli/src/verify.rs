//! `lemma verify`: whether a description still matches the code. See
//! `.specs/16-verifying-descriptions.md`.

use std::collections::BTreeMap;
use std::io::Write;
use std::path::Path;
use std::process::ExitCode;

use lemma_graph::description::{Candidate, DescriptionReport, LookedAt, verify_description};
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

/// Verifies as `args` ask and prints the report: in a process the
/// workspace's daemon runs for it, when a daemon may serve it and it is to
/// be asked (`asks_daemon_to`), and in this process otherwise.
pub(crate) fn run(
    args: &VerifyArgs,
    lsp: &LanguageServers,
    asks_daemon_to: bool,
    json: bool,
) -> ExitCode {
    #[cfg(unix)]
    if lsp.asks_daemon && asks_daemon_to {
        if let Some(code) = crate::check::answered_by_daemon(std::env::args().skip(1).collect()) {
            return code;
        }
    }
    log::info!("verifying in this process");
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
        // Behaviors are `lemma check`'s to report; verify reports none, so
        // running their checkers here only cost time.
        run_checkers: false,
        runs_base_checkers: false,
        select: None,
        warm: None,
        environment: None,
        runtime: crate::daemon::warm_runtime(directory),
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
    print_candidates(out, &report.candidates, args.all)?;
    if let Some(list) = report.link_list.as_ref().filter(|_| args.links) {
        crate::check::print_link_list(out, list, args.all)?;
    }
    if let Some(looked_at) = &report.looked_at {
        print_looked_at(out, looked_at, args.all)?;
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
    print_totals(out, report)
}

/// The closing lines: how far a result with no findings reaches, then how
/// many findings of each severity there are.
fn print_totals(out: &mut dyn Write, report: &DescriptionReport) -> std::io::Result<()> {
    let count = |severity: Severity| {
        report
            .findings
            .iter()
            .filter(|finding| finding.severity == severity)
            .count()
    };
    let candidates = match report.candidates.len() {
        0 => String::new(),
        found => format!(", {found} candidates"),
    };
    if let Some(looked_at) = &report.looked_at {
        writeln!(
            out,
            "{} of {} operations have a call read, {} requests not read{candidates}",
            looked_at.called_operations,
            looked_at.operations,
            looked_at.unread_requests.len()
        )?;
    }
    writeln!(
        out,
        "{} errors, {} warnings, {} informational{candidates}",
        count(Severity::Error),
        count(Severity::Warning),
        count(Severity::Info)
    )
}

/// The candidate for a client none of whose functions has a call read,
/// which is about the client and has no places to show.
const CLIENT_WITHOUT_CALLERS: &str = "client_without_callers";

/// The first ten candidates, or with `all` every one: where each is, what
/// was found there, and the first place that shows it.
fn print_candidates(
    out: &mut dyn Write,
    candidates: &[Candidate],
    all: bool,
) -> std::io::Result<()> {
    let shown = if all {
        candidates.len()
    } else {
        SHOWN_WITHOUT_ALL
    };
    for candidate in candidates.iter().take(shown) {
        if candidate.name == CLIENT_WITHOUT_CALLERS {
            writeln!(
                out,
                "candidate {}  its functions request {} operations, and no call of one was read",
                candidate.location, candidate.operations
            )?;
        } else {
            writeln!(
                out,
                "candidate {}  {} places in {} files write the paths of {} described operations, and no service includes it",
                candidate.location,
                candidate.places.len(),
                candidate.files,
                candidate.operations
            )?;
        }
        if let Some(first) = candidate.places.first() {
            writeln!(
                out,
                "        first: {}:{}  {} {}",
                first.file, first.line, first.method, first.path
            )?;
        }
        writeln!(out, "        [{}]", candidate.name)?;
    }
    if candidates.len() > shown {
        let more = candidates.len() - shown;
        writeln!(out, "and {more} more candidates (--all lists them)")?;
    }
    Ok(())
}

/// What the run read, then the requests it did not read: how many for
/// each reason, and the first ten, or with `all` every one.
fn print_looked_at(out: &mut dyn Write, looked_at: &LookedAt, all: bool) -> std::io::Result<()> {
    writeln!(out, "looked at")?;
    writeln!(
        out,
        "        {} repositories, {} operations, {} calls of them read from code",
        looked_at.repositories, looked_at.operations, looked_at.calls
    )?;
    print_calls_in_tests(out, looked_at)?;
    print_unread_files(out, looked_at)?;
    let unread = &looked_at.unread_requests;
    if unread.is_empty() {
        return Ok(());
    }
    let mut reasons: BTreeMap<&str, usize> = BTreeMap::new();
    for request in unread {
        *reasons.entry(request.reason.as_str()).or_default() += 1;
    }
    let reasons: Vec<String> = reasons
        .iter()
        .map(|(reason, count)| format!("{count} {reason}"))
        .collect();
    writeln!(
        out,
        "        {} requests not read: {}",
        unread.len(),
        reasons.join(", ")
    )?;
    let shown = if all { unread.len() } else { SHOWN_WITHOUT_ALL };
    for request in unread.iter().take(shown) {
        writeln!(
            out,
            "          {}:{}  {}  {}",
            request.file, request.line, request.reason, request.written
        )?;
    }
    if unread.len() > shown {
        let more = unread.len() - shown;
        writeln!(out, "          and {more} more (--all lists them)")?;
    }
    Ok(())
}

/// The places in tests that write a described operation's path, which no
/// run reads: how many, and in which repositories.
fn print_calls_in_tests(out: &mut dyn Write, looked_at: &LookedAt) -> std::io::Result<()> {
    let tests = &looked_at.calls_in_tests;
    if tests.is_empty() {
        return Ok(());
    }
    let places: usize = tests.iter().map(|repository| repository.places).sum();
    let files: usize = tests.iter().map(|repository| repository.files).sum();
    let repositories: Vec<String> = tests
        .iter()
        .map(|repository| format!("{} {}", repository.repository, repository.places))
        .collect();
    writeln!(
        out,
        "        tests are not read: {places} places in {files} test files write a described operation's path ({})",
        repositories.join(", ")
    )
}

/// The files the services include in languages Lemma has no reader for:
/// how many, and of which language in which repository.
fn print_unread_files(out: &mut dyn Write, looked_at: &LookedAt) -> std::io::Result<()> {
    let unread = &looked_at.unread_files;
    if unread.is_empty() {
        return Ok(());
    }
    let files: usize = unread.iter().map(|found| found.files).sum();
    let each: Vec<String> = unread
        .iter()
        .map(|found| format!("{} {} {}", found.repository, found.language, found.files))
        .collect();
    writeln!(
        out,
        "        {files} included files are in languages Lemma does not read ({})",
        each.join(", ")
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
