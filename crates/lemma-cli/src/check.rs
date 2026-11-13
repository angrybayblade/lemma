//! `lemma check`: checks the links a composed description declares and infers.

use std::collections::BTreeMap;
use std::io::Write;
use std::path::{Path, PathBuf};
use std::process::ExitCode;
use std::sync::Arc;

use lemma_graph::config::Level;
use lemma_graph::{
    Change, CheckOptions, Finding, GraphError, Impact, InferredLink, LinkList, Report, Severity,
};
use lemma_logic::ReferenceEngine;
use lemma_souffle::SouffleEngine;

use crate::cli::CheckArgs;

/// Exit code when a finding is an error.
const EXIT_VIOLATED: u8 = 4;
/// Exit code when the check finished without covering every repository.
const EXIT_INCOMPLETE: u8 = 5;

pub(crate) fn run(args: &CheckArgs, asks_daemon: bool, json: bool) -> ExitCode {
    #[cfg(unix)]
    if asks_daemon {
        if let Some(answered) = crate::sentry::ask(asked_arguments(args)) {
            let _ = std::io::stdout().write_all(answered.stdout.as_bytes());
            let _ = std::io::stderr().write_all(answered.stderr.as_bytes());
            return ExitCode::from(answered.exit_code);
        }
    }
    let mut stdout = std::io::stdout().lock();
    // Not locked for the whole check, as stdout is: log lines go to stderr
    // from any thread, and one waiting on a lock the check holds while the
    // check waits on that thread would never finish.
    let mut stderr = std::io::stderr();
    let mut streams = Streams {
        out: &mut stdout,
        err: &mut stderr,
    };
    let exit_code = answer(args, json, None, &mut streams);
    if args.since.is_some() {
        let _ = writeln!(
            streams.err,
            "--since needs the daemon's sentry attached to this workspace \
             (`lemma daemon sentry start`); the whole report is above"
        );
    }
    ExitCode::from(exit_code)
}

/// Where a check prints its report and its errors.
pub(crate) struct Streams<'a> {
    pub out: &'a mut dyn Write,
    pub err: &'a mut dyn Write,
}

/// What a check sentry answers brings from it: the manifest, resolved
/// against the asking process's directory, the reads sentry keeps, and the
/// asking process's environment, for the commands the check runs.
pub(crate) struct Served<'a> {
    pub manifest: &'a Path,
    pub warm: Arc<lemma_graph::Warm>,
    pub environment: Arc<BTreeMap<String, String>>,
}

/// Checks as `args` ask, printing the report to `streams`; the exit code.
pub(crate) fn answer(
    args: &CheckArgs,
    json: bool,
    served: Option<&Served<'_>>,
    streams: &mut Streams<'_>,
) -> u8 {
    match evaluate(args, served) {
        Ok(report) => print(&report, args, json, None, streams),
        Err(error) => {
            let _ = writeln!(streams.err, "error: {error:#}");
            1
        }
    }
}

/// The report of the check `args` ask for.
pub(crate) fn evaluate(args: &CheckArgs, served: Option<&Served<'_>>) -> anyhow::Result<Report> {
    let manifest = served.map_or_else(
        || args.manifest.clone(),
        |served| served.manifest.to_path_buf(),
    );
    let directory = manifest
        .parent()
        .filter(|directory| !directory.as_os_str().is_empty())
        .unwrap_or(Path::new("."));
    let cloning = crate::cloning::permitted(args.no_clone, directory)?;
    let options = check_options(args, cloning, served);
    if args.evaluation.as_deref() == Some("both") {
        return check_both(&manifest, args, &options);
    }
    check(&manifest, args, &options)
}

/// The command line sentry is asked with: as given, with `--no-clone` when
/// the person declined cloning at the prompt, which only `args` records.
#[cfg(unix)]
fn asked_arguments(args: &CheckArgs) -> Vec<String> {
    let mut asked: Vec<String> = std::env::args().skip(1).collect();
    if args.no_clone && !asked.iter().any(|argument| argument == "--no-clone") {
        asked.push("--no-clone".to_owned());
    }
    asked
}

/// What sentry adds to a report it answers with: the token naming the
/// answer, and, when the check asked `--since` an earlier one, what changed.
pub(crate) struct SentryNote<'a> {
    pub token: &'a str,
    pub delta: Option<Delta<'a>>,
}

/// The findings that came and went since an earlier answer with the same
/// options, or `None` in `changes` when that answer is not kept.
pub(crate) struct Delta<'a> {
    pub since: &'a str,
    pub changes: Option<(Vec<&'a Finding>, Vec<&'a Finding>)>,
}

/// Prints `report` to `streams` as text or JSON, with sentry's note when it
/// answered; the exit code: 4 when a finding is an error.
pub(crate) fn print(
    report: &Report,
    args: &CheckArgs,
    json: bool,
    note: Option<&SentryNote<'_>>,
    streams: &mut Streams<'_>,
) -> u8 {
    let out = &mut *streams.out;
    let printed = match (json, note) {
        (true, None) => serde_json::to_writer_pretty(&mut *out, report)
            .map_err(std::io::Error::other)
            .and_then(|()| writeln!(out)),
        (true, Some(note)) => print_json_noted(out, report, note),
        (
            false,
            Some(SentryNote {
                token,
                delta: Some(delta),
            }),
        ) if delta.changes.is_some() => print_delta(out, report, delta, args.all)
            .and_then(|()| writeln!(out, "sentry token: {token}")),
        (false, note) => print_text(out, report, args.all).and_then(|()| match note {
            Some(note) => print_note(out, note),
            None => Ok(()),
        }),
    };
    if printed.is_err() {
        return 1;
    }
    exit_code(report)
}

/// The token line under a whole report, saying first when `--since` asked
/// for an answer that is not kept.
fn print_note(out: &mut dyn Write, note: &SentryNote<'_>) -> std::io::Result<()> {
    if let Some(delta) = &note.delta {
        writeln!(
            out,
            "since {}: not kept, so the whole report is above",
            delta.since
        )?;
    }
    writeln!(out, "sentry token: {}", note.token)
}

/// What changed since an earlier answer: the findings that came and went,
/// then the counts of the whole report.
fn print_delta(
    out: &mut dyn Write,
    report: &Report,
    delta: &Delta<'_>,
    all: bool,
) -> std::io::Result<()> {
    let Some((new, gone)) = &delta.changes else {
        return Ok(());
    };
    writeln!(
        out,
        "since {}: {} new, {} gone",
        delta.since,
        new.len(),
        gone.len()
    )?;
    for finding in new {
        write!(out, "new  ")?;
        print_finding(out, finding, all)?;
    }
    for finding in gone {
        write!(out, "gone ")?;
        print_finding(out, finding, all)?;
    }
    print_counts(out, report, all)
}

/// The report as JSON, with a `sentry` object: the token, and what changed
/// since the answer `--since` named.
fn print_json_noted(
    out: &mut dyn Write,
    report: &Report,
    note: &SentryNote<'_>,
) -> std::io::Result<()> {
    let mut value = serde_json::to_value(report).map_err(std::io::Error::other)?;
    let mut sentry = serde_json::json!({ "token": note.token });
    if let Some(delta) = &note.delta {
        sentry["since"] = match &delta.changes {
            Some((new, gone)) => {
                serde_json::json!({ "token": delta.since, "kept": true, "new": new, "gone": gone })
            }
            None => serde_json::json!({ "token": delta.since, "kept": false }),
        };
    }
    if let Some(object) = value.as_object_mut() {
        object.insert("sentry".to_owned(), sentry);
    }
    serde_json::to_writer_pretty(&mut *out, &value).map_err(std::io::Error::other)?;
    writeln!(out)
}

/// The command's exit code: findings first, then whether it covered all.
fn exit_code(report: &Report) -> u8 {
    let violated = report
        .findings
        .iter()
        .any(|finding| finding.severity == Severity::Error);
    if violated {
        return EXIT_VIOLATED;
    }
    if report.not_checked.is_empty() {
        return 0;
    }
    EXIT_INCOMPLETE
}

/// What `args` ask to check.
fn check_options(
    args: &CheckArgs,
    cloning: lemma_graph::repos::Cloning,
    served: Option<&Served<'_>>,
) -> CheckOptions {
    let change = args.change.as_ref().map(|repository| Change {
        repository: repository.clone(),
        base: args.base.clone(),
        head: args.head.clone(),
    });
    CheckOptions {
        change,
        level: args.level.as_deref().map(|level| match level {
            "check" => Level::Check,
            "strict" => Level::Strict,
            _ => Level::Audit,
        }),
        cache: lemma_graph::repos::cache_directory(),
        cloning,
        explain: args.explain,
        relations: Vec::new(),
        facts_cache: crate::verify::facts_cache(),
        evaluation: match args.evaluation.as_deref() {
            Some("full") => lemma_verify::Evaluation::Full,
            _ => lemma_verify::Evaluation::Demand,
        },
        list_links: false,
        run_checkers: !args.no_run,
        runs_base_checkers: args.run_base,
        select: args.select.clone(),
        warm: served.map(|served| Arc::clone(&served.warm)),
        environment: served.map(|served| Arc::clone(&served.environment)),
        runtime: None,
    }
}

/// Where `lemma verify` gets language servers from.
pub(crate) struct LanguageServers {
    /// Whether to ask the running daemon's warm servers first.
    pub asks_daemon: bool,
    /// Provider configuration given with `--providers`.
    pub providers_file: Option<PathBuf>,
}

/// The description a check asked from `cwd` starts from, made absolute:
/// what sentry is attached to.
pub(crate) fn entry_manifest_in(cwd: &Path, manifest: &Path) -> PathBuf {
    cwd.join(manifest)
}

fn check(manifest: &Path, args: &CheckArgs, options: &CheckOptions) -> anyhow::Result<Report> {
    if let Some(cache) = &options.facts_cache {
        log::debug!("facts cache: {}", cache.directory.display());
    }
    let report: Result<Report, GraphError> = match args.engine.as_deref() {
        Some("reference") => {
            log::info!("rule engine: reference (--engine reference)");
            lemma_graph::check(manifest, options, &ReferenceEngine, None)
        }
        Some("souffle") => {
            log::info!("rule engine: souffle (--engine souffle)");
            lemma_graph::check(manifest, options, &SouffleEngine::find()?, None)
        }
        Some(other) => anyhow::bail!("unknown engine `{other}`; use `souffle` or `reference`"),
        None => {
            if let Ok(souffle) = SouffleEngine::find() {
                log::info!("rule engine: souffle");
                lemma_graph::check(manifest, options, &souffle, None)
            } else {
                log::info!("rule engine: reference (souffle is not installed)");
                lemma_graph::check(manifest, options, &ReferenceEngine, None)
            }
        }
    };
    Ok(report?)
}

/// Checks with full and with change-driven evaluation, and fails if they
/// found different things: a selection that missed a file a finding rests
/// on. The change-driven report, otherwise.
fn check_both(manifest: &Path, args: &CheckArgs, options: &CheckOptions) -> anyhow::Result<Report> {
    let evaluated = |evaluation| CheckOptions {
        evaluation,
        ..options.clone()
    };
    let full = check(manifest, args, &evaluated(lemma_verify::Evaluation::Full))?;
    let demand = check(manifest, args, &evaluated(lemma_verify::Evaluation::Demand))?;
    let differences = findings_only_in_one(&full, &demand);
    anyhow::ensure!(
        differences.is_empty(),
        "change-driven evaluation found other findings than full evaluation:{differences}"
    );
    Ok(demand)
}

/// The findings of one report and not the other, a line each.
fn findings_only_in_one(full: &Report, demand: &Report) -> String {
    let described = |report: &Report| -> std::collections::BTreeSet<String> {
        report
            .findings
            .iter()
            .map(|finding| {
                let origin = if finding.introduced {
                    ""
                } else {
                    " (already there)"
                };
                format!(
                    "{:?} {} {} {}{origin}",
                    finding.severity, finding.check, finding.link, finding.message
                )
            })
            .collect()
    };
    let (full_findings, demand_findings) = (described(full), described(demand));
    let missed = full_findings
        .difference(&demand_findings)
        .map(|finding| format!("\n  only in full: {finding}"));
    let extra = demand_findings
        .difference(&full_findings)
        .map(|finding| format!("\n  only in demand: {finding}"));
    missed.chain(extra).collect()
}

fn print_text(out: &mut dyn Write, report: &Report, all: bool) -> std::io::Result<()> {
    let level = match report.level {
        Level::Audit => "audit",
        Level::Check => "check",
        Level::Strict => "strict",
    };
    let inferred = report
        .inferred_links
        .map_or_else(String::new, |count| format!(", {count} inferred"));
    writeln!(out, "{} links{inferred}, level {level}", report.links)?;
    print_repositories(out, report)?;
    print_behaviors(out, report)?;
    for finding in &report.findings {
        if finding.severity == Severity::Info && !all {
            continue;
        }
        print_finding(out, finding, all)?;
    }
    print_counts(out, report, all)
}

/// One finding: its severity and message, then its requirement, link and
/// whether the change introduced it, then what it affects.
fn print_finding(out: &mut dyn Write, finding: &Finding, all: bool) -> std::io::Result<()> {
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
    for line in finding.derivation.iter().flat_map(|text| text.lines()) {
        writeln!(out, "        {line}")?;
    }
    print_affects(out, &finding.affects, all)
}

/// The counts of errors, warnings and informational findings.
fn print_counts(out: &mut dyn Write, report: &Report, all: bool) -> std::io::Result<()> {
    let count = |severity: Severity| {
        report
            .findings
            .iter()
            .filter(|finding| finding.severity == severity)
            .count()
    };
    print_not_checked(out, report)?;
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

/// Each behavior's outcome and how many of its cases held, then what the
/// change did to requirements.
fn print_behaviors(out: &mut dyn Write, report: &Report) -> std::io::Result<()> {
    for behavior in &report.behaviors {
        let held = behavior
            .cases
            .iter()
            .filter(|case| case.outcome == "satisfied")
            .count();
        let reused = behavior.evidence.iter().filter(|used| used.reused).count();
        let evidence = if behavior.evidence.is_empty() {
            String::new()
        } else {
            format!(", {reused} of {} evidence reused", behavior.evidence.len())
        };
        let fixed = behavior.cases.iter().filter(|case| case.fixed).count();
        let fixed = if fixed == 0 {
            String::new()
        } else {
            format!(", {fixed} fixed by this change")
        };
        writeln!(
            out,
            "behavior {}: {} ({held} of {} cases held{evidence}{fixed})",
            behavior.identity,
            behavior.outcome,
            behavior.cases.len()
        )?;
        for case in &behavior.cases {
            if let Some(at_base) = &case.at_base {
                writeln!(
                    out,
                    "        fixed {}: at the base, {at_base}",
                    case.identity
                )?;
            }
        }
    }
    for change in &report.requirement_changes {
        writeln!(out, "requirement {} {}", change.requirement, change.change)?;
    }
    Ok(())
}

/// How many affected items to list per service or repository, unless
/// `--all` asks for every one.
const LISTED_PER_OWNER: usize = 3;

/// What a problem affects: a count, then by distance (operations whose
/// handlers reach it first, then the calls into them, and so on), by
/// service or repository, each operation reached directly with the calls
/// from its handler down to the problem.
fn print_affects(out: &mut dyn Write, affects: &[Impact], all: bool) -> std::io::Result<()> {
    if affects.is_empty() {
        return Ok(());
    }
    let operations = affects
        .iter()
        .filter(|impact| impact.calls.is_none())
        .count();
    let callers = affects.len() - operations;
    writeln!(
        out,
        "        affects {} and {} (traced by name):",
        plural(operations, "operation"),
        plural(callers, "caller")
    )?;
    let farthest = affects
        .iter()
        .map(|impact| impact.distance)
        .max()
        .unwrap_or(0);
    for distance in 0..=farthest {
        let here: Vec<&Impact> = affects
            .iter()
            .filter(|impact| impact.distance == distance)
            .collect();
        if here.is_empty() {
            continue;
        }
        let heading = match distance {
            0 => "reached directly".to_owned(),
            1 => "one call away".to_owned(),
            hops => format!("{hops} calls away"),
        };
        writeln!(out, "          {heading}:")?;
        // Callers first: the operations at this distance are reached
        // through them.
        let mut owners = by_owner(&here);
        owners.sort_by_key(|(_, items)| items.iter().any(|impact| impact.calls.is_none()));
        for (owner, items) in owners {
            print_owner(out, owner, &items, all)?;
        }
    }
    Ok(())
}

/// Items grouped by their service or repository, in first-seen order.
fn by_owner<'a>(items: &[&'a Impact]) -> Vec<(&'a str, Vec<&'a Impact>)> {
    let mut owners: Vec<(&str, Vec<&Impact>)> = Vec::new();
    for impact in items {
        match owners.iter_mut().find(|(owner, _)| *owner == impact.owner) {
            Some((_, grouped)) => grouped.push(impact),
            None => owners.push((&impact.owner, vec![impact])),
        }
    }
    owners
}

/// One service's affected operations, or one repository's callers.
fn print_owner(
    out: &mut dyn Write,
    owner: &str,
    items: &[&Impact],
    all: bool,
) -> std::io::Result<()> {
    let is_callers = items.iter().all(|impact| impact.calls.is_some());
    let shown = if all {
        items.len()
    } else {
        items.len().min(LISTED_PER_OWNER)
    };
    let summary = if is_callers {
        let called: std::collections::BTreeSet<&str> = items
            .iter()
            .filter_map(|impact| impact.calls.as_deref())
            .collect();
        format!(
            "{} of {}",
            plural(items.len(), "caller"),
            plural(called.len(), "operation")
        )
    } else {
        plural(items.len(), "operation")
    };
    writeln!(out, "            {owner}: {summary}")?;
    for impact in &items[..shown] {
        match (&impact.calls, &impact.via) {
            (Some(operation), _) => writeln!(out, "              {} -> {operation}", impact.at)?,
            (None, Some(via)) if impact.distance > 0 => {
                let caller = via.rsplit("::").next().unwrap_or(via);
                writeln!(out, "              {}  (through {caller})", impact.at)?;
            }
            (None, _) => writeln!(out, "              {}", impact.at)?,
        }
        if impact.distance == 0 && !impact.path.is_empty() {
            let steps: Vec<&str> = impact
                .via
                .iter()
                .chain(&impact.path)
                .map(String::as_str)
                .collect();
            writeln!(out, "                through {}", call_path(&steps))?;
        }
    }
    let more = items.len() - shown;
    if more > 0 {
        writeln!(out, "              and {more} more (--all lists them)")?;
    }
    Ok(())
}

/// Calls as `file:Function -> Other -> other/file:Third`, each file written
/// where it changes.
fn call_path(steps: &[&str]) -> String {
    let mut written = Vec::new();
    let mut last_file = "";
    for step in steps {
        let (file, function) = step.rsplit_once(':').unwrap_or(("", step));
        if file == last_file {
            written.push(function.to_owned());
        } else {
            written.push((*step).to_owned());
            last_file = file;
        }
    }
    written.join(" -> ")
}

/// `1 operation`, `3 operations`.
/// How many calls to list per caller and service, unless `--all` asks for
/// every one.
const LISTED_CALLS_PER_GROUP: usize = 10;

/// Every link: those declared, then the inferred calls by calling
/// repository and the service called, each group's count first.
pub(crate) fn print_link_list(
    out: &mut dyn Write,
    list: &LinkList,
    all: bool,
) -> std::io::Result<()> {
    writeln!(out, "declared links: {}", list.declared.len())?;
    for link in &list.declared {
        writeln!(out, "  {link}")?;
    }
    writeln!(out, "inferred links: {}", list.inferred.len())?;
    for ((repository, service), calls) in by_caller_and_service(&list.inferred) {
        let operations: std::collections::BTreeSet<&str> =
            calls.iter().map(|call| call.operation.as_str()).collect();
        writeln!(
            out,
            "  {repository} -> {service}: {} to {}",
            plural(calls.len(), "call"),
            plural(operations.len(), "operation")
        )?;
        let shown = if all {
            calls.len()
        } else {
            LISTED_CALLS_PER_GROUP
        };
        for call in calls.iter().take(shown) {
            writeln!(out, "    {} -> {}", call.location, call.operation)?;
        }
        if calls.len() > shown {
            writeln!(
                out,
                "    ... {} more (--all lists them)",
                calls.len() - shown
            )?;
        }
    }
    Ok(())
}

/// Inferred calls grouped by calling repository and the service called,
/// in that order; a call to an operation no service serves is grouped
/// under `unknown service`.
fn by_caller_and_service(
    calls: &[InferredLink],
) -> std::collections::BTreeMap<(&str, &str), Vec<&InferredLink>> {
    let mut groups: std::collections::BTreeMap<(&str, &str), Vec<&InferredLink>> =
        std::collections::BTreeMap::new();
    for call in calls {
        let service = call.service.as_deref().unwrap_or("unknown service");
        groups
            .entry((call.repository.as_str(), service))
            .or_default()
            .push(call);
    }
    groups
}

fn plural(count: usize, noun: &str) -> String {
    if count == 1 {
        format!("1 {noun}")
    } else {
        format!("{count} {noun}s")
    }
}

fn print_not_checked(out: &mut dyn Write, report: &Report) -> std::io::Result<()> {
    for skipped in &report.not_checked {
        writeln!(
            out,
            "not checked: {} links to {} (not cloned)",
            skipped.links, skipped.repository
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
            "  (the description from the working directory)"
        } else {
            ""
        };
        writeln!(out, "  {:<14} {commits}{note}", repository.name)?;
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn call(repository: &str, location: &str, operation: &str) -> InferredLink {
        InferredLink {
            repository: repository.to_owned(),
            location: location.to_owned(),
            operation: operation.to_owned(),
            service: Some("#api:api".to_owned()),
        }
    }

    #[test]
    fn links_are_listed_by_caller_and_service_with_counts() {
        let list = LinkList {
            declared: vec!["#web:services.web.pay -> #api:services.api.pay".to_owned()],
            inferred: vec![
                call("#web", "a.ts::one", "GET /a"),
                call("#web", "b.ts::two", "GET /a"),
                call("#worker", "c.py", "POST /b"),
            ],
        };
        let mut printed = Vec::new();

        print_link_list(&mut printed, &list, false).unwrap();

        assert_eq!(
            String::from_utf8(printed).unwrap(),
            "declared links: 1\n  #web:services.web.pay -> #api:services.api.pay\n\
             inferred links: 3\n  #web -> #api:api: 2 calls to 1 operation\n\
             \x20   a.ts::one -> GET /a\n    b.ts::two -> GET /a\n\
             \x20 #worker -> #api:api: 1 call to 1 operation\n    c.py -> POST /b\n"
        );
    }

    fn impact(
        distance: usize,
        owner: &str,
        at: &str,
        calls: Option<&str>,
        via: Option<&str>,
    ) -> Impact {
        Impact {
            distance,
            owner: owner.to_owned(),
            at: at.to_owned(),
            calls: calls.map(str::to_owned),
            via: via.map(str::to_owned),
            path: Vec::new(),
        }
    }

    #[test]
    fn a_trace_reads_nearest_first_with_callers_before_what_they_reach() {
        let mut direct = impact(
            0,
            "#api:tools",
            "POST /tools/run",
            None,
            Some("h/run.go:NewRun"),
        );
        direct.path = vec![
            "h/run.go:Run.handle".to_owned(),
            "lib/run.go:Execute".to_owned(),
        ];
        let affects = [
            direct,
            impact(
                1,
                "#api:gateway",
                "POST /v1/run",
                None,
                Some("src/run.ts::runTool"),
            ),
            impact(
                1,
                "#api",
                "src/run.ts::runTool",
                Some("POST /tools/run"),
                None,
            ),
        ];
        let mut out = Vec::new();

        print_affects(&mut out, &affects, false).unwrap();

        let text = String::from_utf8(out).unwrap();
        let expected = "        affects 2 operations and 1 caller (traced by name):
          reached directly:
            #api:tools: 1 operation
              POST /tools/run
                through h/run.go:NewRun -> Run.handle -> lib/run.go:Execute
          one call away:
            #api: 1 caller of 1 operation
              src/run.ts::runTool -> POST /tools/run
            #api:gateway: 1 operation
              POST /v1/run  (through runTool)
";
        assert_eq!(text, expected);
    }
}
