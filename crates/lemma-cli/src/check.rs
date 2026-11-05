//! `lemma check`: checks the links declared in composable lemma.yaml files.

use std::io::Write;
use std::path::Path;
use std::process::ExitCode;

use lemma_graph::config::Level;
use lemma_graph::{Change, CheckOptions, GraphError, Impact, Report, Severity};
use lemma_logic::ReferenceEngine;
use lemma_providers::ProviderRegistry;
use lemma_souffle::SouffleEngine;

use crate::anchors::ProviderReferences;
use crate::cli::CheckArgs;

/// Exit code when a finding is an error.
const EXIT_VIOLATED: u8 = 4;

pub(crate) fn run(args: &CheckArgs, providers: Option<&Path>, json: bool) -> ExitCode {
    let change = args.change.as_ref().map(|repository| Change {
        repository: repository.clone(),
        base: args.base.clone().unwrap_or_default(),
        head: args.head.clone(),
    });
    let options = CheckOptions {
        change,
        level: args.level.as_deref().map(|level| match level {
            "check" => Level::Check,
            _ => Level::Audit,
        }),
        cache: lemma_graph::repos::cache_directory(),
        facts_cache: crate::verify::facts_cache(),
        evaluation: match args.evaluation.as_deref() {
            Some("full") => lemma_verify::Evaluation::Full,
            _ => lemma_verify::Evaluation::Demand,
        },
    };
    let report = match check(args, &options, providers) {
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

fn check(
    args: &CheckArgs,
    options: &CheckOptions,
    providers: Option<&Path>,
) -> anyhow::Result<Report> {
    let manifest = &args.manifest;
    let mut references = if args.anchors {
        Some(ProviderReferences::new(registry(providers)?))
    } else {
        None
    };
    let references = references
        .as_mut()
        .map(|references| references as &mut dyn lemma_graph::References);
    let report: Result<Report, GraphError> = match args.engine.as_deref() {
        Some("reference") => lemma_graph::check(manifest, options, &ReferenceEngine, references),
        Some("souffle") => {
            lemma_graph::check(manifest, options, &SouffleEngine::find()?, references)
        }
        Some(other) => anyhow::bail!("unknown engine `{other}`; use `souffle` or `reference`"),
        None => match SouffleEngine::find() {
            Ok(souffle) => lemma_graph::check(manifest, options, &souffle, references),
            Err(_) => lemma_graph::check(manifest, options, &ReferenceEngine, references),
        },
    };
    Ok(report?)
}

/// The built-in providers, plus those in `providers` when given.
fn registry(providers: Option<&Path>) -> anyhow::Result<ProviderRegistry> {
    let Some(file) = providers else {
        return Ok(ProviderRegistry::builtin()?);
    };
    anyhow::ensure!(
        file.is_file(),
        "provider configuration {} does not exist",
        file.display()
    );
    Ok(ProviderRegistry::load(file)?)
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
        print_affects(out, &finding.affects, all)?;
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
fn plural(count: usize, noun: &str) -> String {
    if count == 1 {
        format!("1 {noun}")
    } else {
        format!("{count} {noun}s")
    }
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

#[cfg(test)]
mod tests {
    use super::*;

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
