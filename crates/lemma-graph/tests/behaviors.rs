//! Behaviors in a whole check, offline: spec 13's worked example, a worker
//! that should pass on the `retriable` flag its upstream sets, with a
//! replay checker that runs the worker's handler against an injected
//! upstream error. The behavior is violated on a worker that drops the
//! flag, satisfied on one that forwards it, and unknown when the checker
//! cannot run.

#![allow(clippy::unwrap_used, reason = "test setup should fail loudly")]

use std::path::Path;
use std::process::Command;

use lemma_graph::config::Level;
use lemma_graph::{CheckOptions, Report, Severity, check};
use lemma_logic::ReferenceEngine;

fn git(directory: &Path, args: &[&str]) {
    let status = Command::new("git")
        .arg("-C")
        .arg(directory)
        .args([
            "-c",
            "user.name=t",
            "-c",
            "user.email=t@t",
            "-c",
            "commit.gpgsign=false",
        ])
        .args(args)
        .status()
        .unwrap();
    assert!(status.success(), "git {args:?}");
}

fn write(path: &Path, text: &str) {
    std::fs::create_dir_all(path.parent().unwrap()).unwrap();
    std::fs::write(path, text).unwrap();
}

const FUNCTIONS: &str = r#"module functions

import * as python from lemma.python

let serverless = python.file("serverless.py")

export service Functions {
    export operation setup_webhook {
        implementation: serverless.symbol("setup_webhook")
    }
}
"#;

const PLATFORM: &str = r#"module platform

import * as python from lemma.python
import { Link } from lemma.links
import { Functions } from functions

repo functions = local("../functions")

let client = python.file("worker/client.py")
let handlers = python.file("worker/handlers.py")

export service Worker {
    client operation call_setup_webhook {
        implementation: client.symbol("setup_webhook")
    }

    export operation setup_monitor {
        implementation: handlers.symbol("setup_monitor")
    }
}

export graph links : Link {
    Worker.call_setup_webhook -> Functions.setup_webhook
}

export checker replay_setup_error(
    handler: Operation,
    through: Fact<Link>,
    upstream_retriable: Bool
): Evidence<Json> {
    command: ["python3", "tools/replay.py"]
    timeout_ms: 60000
}

export behavior forwards_upstream_retriable {
    let link = links[Worker.call_setup_webhook, Functions.setup_webhook]
    expect link.exists

    for flag in [true, false] {
        let trace = need replay_setup_error(
            handler: Worker.setup_monitor,
            through: link,
            upstream_retriable: flag
        )
        expect trace.upstream.retriable == flag
        expect trace.http.status == 200
        expect trace.http.body.retriable == trace.upstream.retriable
            because "the worker passes on the retriable flag the functions service set"
    }
}
"#;

const CLIENT: &str =
    "def setup_webhook(request):\n    raise RuntimeError(\"calls the functions service\")\n";

const FORWARDING_HANDLER: &str = r#"from worker import client


def setup_monitor(request):
    result = client.setup_webhook(request)
    if result.get("error") is not None:
        return {"status": 200, "body": {"error": result["error"], "retriable": result["retriable"]}}
    return {"status": 200, "body": {"ok": True}}
"#;

const DROPPING_HANDLER: &str = r#"from worker import client


def setup_monitor(request):
    result = client.setup_webhook(request)
    if result.get("error") is not None:
        return {"status": 200, "body": {"error": result["error"]}}
    return {"status": 200, "body": {"ok": True}}
"#;

/// Exercises the bound handler with the selected outbound call replaced by
/// an upstream application error, and reports both sides of the case.
const REPLAY: &str = r#"import importlib
import json
import os
import sys

given = json.load(open(os.environ["LEMMA_INPUT"]))
arguments = given["arguments"]
handler = arguments["handler"]["implementation"]
call = arguments["through"]["values"]["caller"]["implementation"]
sys.path.insert(0, given["root"])

flag = arguments["upstream_retriable"]
upstream = {"error": "upstream failed", "retriable": flag}
client = importlib.import_module(call["file"][:-3].replace("/", "."))
setattr(client, call["symbol"], lambda request: upstream)
handlers = importlib.import_module(handler["file"][:-3].replace("/", "."))
response = getattr(handlers, handler["symbol"])({})

with open(os.environ["LEMMA_OUTPUT"], "w") as output:
    json.dump({"observed": {"upstream": upstream, "http": response}}, output)
"#;

/// The functions repository, and the platform with `handler` as its
/// worker's handler, committed.
fn system(handler: &str) -> tempfile::TempDir {
    let workspace = tempfile::tempdir().unwrap();
    let functions = workspace.path().join("functions");
    write(&functions.join("mod.lm"), FUNCTIONS);
    write(
        &functions.join("serverless.py"),
        "def setup_webhook(request):\n    pass\n",
    );
    git(&functions, &["init", "-q"]);
    git(&functions, &["add", "."]);
    git(&functions, &["commit", "-q", "-m", "functions"]);
    let platform = workspace.path().join("platform");
    write(&platform.join("mod.lm"), PLATFORM);
    write(&platform.join("worker/__init__.py"), "");
    write(&platform.join("worker/client.py"), CLIENT);
    write(&platform.join("worker/handlers.py"), handler);
    write(&platform.join("tools/replay.py"), REPLAY);
    // What running Python leaves behind is ignored, or the working
    // directory's state would change with every run.
    write(&platform.join(".gitignore"), "__pycache__/\n");
    git(&platform, &["init", "-q"]);
    git(&platform, &["add", "."]);
    git(&platform, &["commit", "-q", "-m", "platform"]);
    workspace
}

fn run(workspace: &Path, level: Level, run_checkers: bool) -> Report {
    let options = CheckOptions {
        change: None,
        level: Some(level),
        cache: workspace.join("cache"),
        facts_cache: None,
        evaluation: lemma_verify::Evaluation::Full,
        list_links: false,
        run_checkers,
        runs_base_checkers: false,
        select: None,
    };
    check(
        &workspace.join("platform/mod.lm"),
        &options,
        &ReferenceEngine,
        None,
    )
    .unwrap()
}

fn behavior_findings(report: &Report) -> Vec<(Severity, String)> {
    report
        .findings
        .iter()
        .filter(|finding| finding.check == "platform::forwards_upstream_retriable")
        .map(|finding| (finding.severity, finding.message.clone()))
        .collect()
}

#[test]
fn a_worker_that_forwards_the_flag_satisfies_the_behavior() {
    let workspace = system(FORWARDING_HANDLER);

    let report = run(workspace.path(), Level::Check, true);

    let behavior = &report.behaviors[0];
    assert_eq!(behavior.outcome, "satisfied", "{behavior:#?}");
    assert_eq!(behavior.cases.len(), 3);
    assert_eq!(behavior.evidence.len(), 2);
    assert_eq!(behavior_findings(&report), []);
}

#[test]
fn a_worker_that_drops_the_flag_violates_it() {
    let workspace = system(DROPPING_HANDLER);

    let report = run(workspace.path(), Level::Check, true);

    assert_eq!(report.behaviors[0].outcome, "violated");
    let found = behavior_findings(&report);
    assert_eq!(found.len(), 2, "{found:#?}");
    assert_eq!(found[0].0, Severity::Error);
    assert!(
        found[0].1.contains("`retriable` is missing"),
        "{}",
        found[0].1
    );
    assert!(
        found[0].1.contains("passes on the retriable flag"),
        "{}",
        found[0].1
    );
}

#[test]
fn a_checker_that_cannot_run_leaves_it_unknown_which_only_strict_fails() {
    let workspace = system(FORWARDING_HANDLER);
    std::fs::remove_file(workspace.path().join("platform/tools/replay.py")).unwrap();
    git(
        &workspace.path().join("platform"),
        &["commit", "-q", "-am", "no replay"],
    );

    let checked = run(workspace.path(), Level::Check, true);
    let strict = run(workspace.path(), Level::Strict, true);

    assert_eq!(checked.behaviors[0].outcome, "unknown");
    let severities = |report: &Report| -> Vec<Severity> {
        behavior_findings(report)
            .into_iter()
            .map(|(severity, _)| severity)
            .collect()
    };
    assert_eq!(severities(&checked), [Severity::Warning, Severity::Warning]);
    assert_eq!(severities(&strict), [Severity::Error, Severity::Error]);
}

#[test]
fn kept_evidence_is_reused_until_the_code_changes_and_is_then_stale_without_running() {
    let workspace = system(FORWARDING_HANDLER);
    run(workspace.path(), Level::Check, true);

    let again = run(workspace.path(), Level::Check, false);
    let platform = workspace.path().join("platform");
    write(&platform.join("worker/handlers.py"), DROPPING_HANDLER);
    let changed = run(workspace.path(), Level::Check, false);

    assert_eq!(again.behaviors[0].outcome, "satisfied");
    assert!(again.behaviors[0].evidence.iter().all(|used| used.reused));
    assert_eq!(
        changed.behaviors[0].outcome, "stale",
        "{:#?}",
        changed.behaviors[0]
    );
}

/// The platform with an allowance appended to its module, committed.
fn allowing(handler: &str, allowance: &str) -> tempfile::TempDir {
    let workspace = system(handler);
    let platform = workspace.path().join("platform");
    write(
        &platform.join("mod.lm"),
        &format!("{PLATFORM}\n{allowance}"),
    );
    git(&platform, &["commit", "-q", "-am", "allow"]);
    workspace
}

#[test]
fn an_allowance_covers_the_cases_its_condition_selects() {
    let workspace = allowing(
        DROPPING_HANDLER,
        "allow forwards_upstream_retriable {\n    when: flag == false\n    reason: \"false is the default\"\n}\n",
    );

    let report = run(workspace.path(), Level::Check, true);

    let found = behavior_findings(&report);
    assert_eq!(found.len(), 2, "{found:#?}");
    assert_eq!(found[0].0, Severity::Error);
    assert_eq!(found[1].0, Severity::Info);
    assert!(
        found[1].1.ends_with("(allowed: false is the default)"),
        "{}",
        found[1].1
    );
    let cases = &report.behaviors[0].cases;
    assert_eq!(cases[2].allowed.as_deref(), Some("false is the default"));
}

#[test]
fn an_expired_allowance_covers_nothing_and_is_reported() {
    let workspace = allowing(
        DROPPING_HANDLER,
        "allow forwards_upstream_retriable {\n    when: flag == false\n    until: \"2000-01-01\"\n}\n",
    );

    let report = run(workspace.path(), Level::Check, true);

    let severities: Vec<Severity> = behavior_findings(&report)
        .into_iter()
        .map(|(severity, _)| severity)
        .collect();
    assert_eq!(severities, [Severity::Error, Severity::Error]);
    let expired = report
        .findings
        .iter()
        .find(|finding| finding.check == "allow")
        .unwrap();
    assert!(
        expired.message.contains("expired on 2000-01-01"),
        "{}",
        expired.message
    );
}

#[test]
fn an_allowance_that_covers_nothing_is_reported() {
    let workspace = allowing(
        FORWARDING_HANDLER,
        "allow forwards_upstream_retriable {\n    reason: \"it used to drop the flag\"\n}\n",
    );

    let report = run(workspace.path(), Level::Check, true);

    let unused = report
        .findings
        .iter()
        .find(|finding| finding.check == "allow")
        .unwrap();
    assert_eq!(unused.severity, Severity::Warning);
    assert!(
        unused.message.contains("covers no finding"),
        "{}",
        unused.message
    );
}

#[test]
fn an_allowance_for_a_requirement_no_check_evaluated_is_not_called_stale() {
    let workspace = allowing(
        FORWARDING_HANDLER,
        "import lemma.http\n\nallow lemma.http.consumer_calls_removed_operation {\n    when: f == \"legacy.py\"\n}\n",
    );

    let report = run(workspace.path(), Level::Check, true);

    let about_allowances: Vec<&str> = report
        .findings
        .iter()
        .filter(|finding| finding.check == "allow")
        .map(|finding| finding.message.as_str())
        .collect();
    assert_eq!(about_allowances, Vec::<&str>::new());
}

#[test]
fn a_change_that_breaks_the_worker_introduces_its_violations_and_lists_new_behaviors() {
    let workspace = system(FORWARDING_HANDLER);
    let platform = workspace.path().join("platform");
    write(&platform.join("worker/handlers.py"), DROPPING_HANDLER);
    let module = format!("{PLATFORM}\nbehavior always {{\n    expect true\n}}\n");
    write(&platform.join("mod.lm"), &module);
    git(&platform, &["commit", "-q", "-am", "drop the flag"]);
    let options = CheckOptions {
        change: Some(lemma_graph::Change {
            repository: "self".to_owned(),
            base: Some("HEAD~1".to_owned()),
            head: Some("HEAD".to_owned()),
        }),
        level: Some(Level::Check),
        cache: workspace.path().join("cache"),
        facts_cache: None,
        evaluation: lemma_verify::Evaluation::Full,
        list_links: false,
        run_checkers: true,
        runs_base_checkers: false,
        select: None,
    };

    let report = check(&platform.join("mod.lm"), &options, &ReferenceEngine, None).unwrap();

    let forwards = &report.behaviors[0];
    assert_eq!(forwards.outcome, "violated");
    assert!(
        forwards.cases[1..].iter().all(|case| case.introduced),
        "{forwards:#?}"
    );
    let changes: Vec<(&str, &str)> = report
        .requirement_changes
        .iter()
        .map(|change| (change.requirement.as_str(), change.change))
        .collect();
    assert_eq!(changes, [("platform::always", "added")]);
}

/// The worker's fix at HEAD, after a commit that dropped the flag.
fn fixing_change(runs_base_checkers: bool) -> Report {
    let workspace = system(DROPPING_HANDLER);
    let platform = workspace.path().join("platform");
    write(&platform.join("worker/handlers.py"), FORWARDING_HANDLER);
    git(&platform, &["commit", "-q", "-am", "forward the flag"]);
    let options = CheckOptions {
        change: Some(lemma_graph::Change {
            repository: "self".to_owned(),
            base: Some("HEAD~1".to_owned()),
            head: Some("HEAD".to_owned()),
        }),
        level: Some(Level::Check),
        cache: workspace.path().join("cache"),
        facts_cache: None,
        evaluation: lemma_verify::Evaluation::Full,
        list_links: false,
        run_checkers: true,
        runs_base_checkers,
        select: None,
    };
    check(&platform.join("mod.lm"), &options, &ReferenceEngine, None).unwrap()
}

#[test]
fn a_change_that_fixes_the_worker_reports_its_cases_fixed_when_the_base_runs() {
    let report = fixing_change(true);

    let forwards = &report.behaviors[0];
    assert_eq!(forwards.outcome, "satisfied");
    assert!(
        forwards.cases[1..].iter().all(|case| case.fixed),
        "{forwards:#?}"
    );
    assert!(
        forwards.cases[1..].iter().all(|case| case
            .at_base
            .as_deref()
            .is_some_and(|ended| ended.contains("retriable"))),
        "{forwards:#?}"
    );
}

#[test]
fn without_running_the_base_a_fix_is_not_known() {
    let report = fixing_change(false);

    let forwards = &report.behaviors[0];
    assert!(
        forwards.cases.iter().all(|case| !case.fixed),
        "{forwards:#?}"
    );
}

#[test]
fn a_checker_reads_other_repositories_at_the_commit_checked() {
    let workspace = system(FORWARDING_HANDLER);
    let functions = workspace.path().join("functions");
    let marked = |text: &str| {
        write(&functions.join("marker.txt"), text);
        git(&functions, &["add", "."]);
        git(&functions, &["commit", "-q", "-m", text.trim()]);
    };
    marked("base\n");
    marked("head\n");
    write(&functions.join("marker.txt"), "working tree\n");
    let platform = workspace.path().join("platform");
    write(
        &platform.join("tools/marker.py"),
        "import json, os\ngiven = json.load(open(os.environ[\"LEMMA_INPUT\"]))\nmarker = open(os.path.join(given[\"roots\"][\"functions\"], \"marker.txt\")).read()\njson.dump({\"observed\": marker}, open(os.environ[\"LEMMA_OUTPUT\"], \"w\"))\n",
    );
    let module = format!(
        "{PLATFORM}\nchecker marker(): Evidence<Text> {{\n    command: [\"python3\", \"tools/marker.py\"]\n    reads: [\"functions\"]\n}}\n\nbehavior reads_the_head {{\n    expect need marker() == \"head\\n\"\n}}\n"
    );
    write(&platform.join("mod.lm"), &module);
    git(&platform, &["add", "."]);
    git(&platform, &["commit", "-q", "-m", "marker"]);
    let options = CheckOptions {
        change: Some(lemma_graph::Change {
            repository: "functions".to_owned(),
            base: Some("HEAD~1".to_owned()),
            head: Some("HEAD".to_owned()),
        }),
        level: Some(Level::Check),
        cache: workspace.path().join("cache"),
        facts_cache: None,
        evaluation: lemma_verify::Evaluation::Full,
        list_links: false,
        run_checkers: true,
        runs_base_checkers: false,
        select: None,
    };

    let report = check(&platform.join("mod.lm"), &options, &ReferenceEngine, None).unwrap();

    let reads = report
        .behaviors
        .iter()
        .find(|behavior| behavior.behavior == "platform::reads_the_head")
        .unwrap();
    assert_eq!(reads.outcome, "satisfied", "{reads:#?}");
}
