//! Evidence kept by what its checker's run read (spec 13, section 10): a
//! change to a file the run did not read leaves it standing, a change to
//! one it read ends it, and it stands again when the files are as they
//! were. These need something that can watch a run, and pass without
//! looking where there is none.

#![allow(clippy::unwrap_used, reason = "test setup should fail loudly")]

use std::path::Path;
use std::process::Command;

use lemma_graph::config::Level;
use lemma_graph::{CheckOptions, Report, check};
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

/// Whether this machine can watch a run: on Linux, with `strace`.
fn can_watch_runs() -> bool {
    let traced = Command::new("strace")
        .args(["-qq", "-e", "trace=chdir", "-o", "/dev/null", "true"])
        .status();
    cfg!(target_os = "linux") && traced.is_ok_and(|status| status.success())
}

const MODULE: &str = r#"module shelf

export checker contents(name: Text): Evidence<Json> {
    command: ["python3", "-B", "tools/read.py"]
    timeout_ms: 60000
}

export behavior every_book_has_text {
    for name in ["a", "b"] {
        let book = need contents(name: name)
        expect book.text != ""
    }
}
"#;

/// Reports the text of `books/<name>.txt`, or of `books/<name>.new` when
/// there is one.
const READ: &str = r#"import json
import os

given = json.load(open(os.environ["LEMMA_INPUT"]))
name = given["arguments"]["name"]
path = os.path.join(given["root"], "books", name + ".new")
if not os.path.exists(path):
    path = os.path.join(given["root"], "books", name + ".txt")
with open(os.environ["LEMMA_OUTPUT"], "w") as output:
    json.dump({"observed": {"text": open(path).read()}}, output)
"#;

/// A repository with two books and a checker that reads one of them for
/// each case, committed.
fn shelf() -> tempfile::TempDir {
    let workspace = tempfile::tempdir().unwrap();
    let root = workspace.path().join("shelf");
    write(&root.join("mod.lm"), MODULE);
    write(&root.join("tools/read.py"), READ);
    write(&root.join("books/a.txt"), "first\n");
    write(&root.join("books/b.txt"), "second\n");
    git(&root, &["init", "-q"]);
    git(&root, &["add", "."]);
    git(&root, &["commit", "-q", "-m", "shelf"]);
    workspace
}

fn run(workspace: &Path, run_checkers: bool) -> Report {
    let options = CheckOptions {
        change: None,
        level: Some(Level::Check),
        cache: workspace.join("cache"),
        cloning: lemma_graph::repos::Cloning::Allowed,
        explain: false,
        relations: Vec::new(),
        facts_cache: None,
        evaluation: lemma_verify::Evaluation::Full,
        list_links: false,
        run_checkers,
        runs_base_checkers: false,
        select: None,
        warm: None,
        environment: None,
        runtime: None,
    };
    check(
        &workspace.join("shelf/mod.lm"),
        &options,
        &ReferenceEngine,
        None,
    )
    .unwrap()
}

/// Each case's outcome, by the book it reads.
fn outcomes(report: &Report) -> Vec<(&str, &str)> {
    let cases = report.behaviors[0]
        .cases
        .iter()
        .filter(|case| case.identity.contains('['));
    cases
        .map(|case| {
            let book = if case.identity.contains("\"a\"") {
                "a"
            } else {
                "b"
            };
            (book, case.outcome)
        })
        .collect()
}

#[test]
fn evidence_outlives_a_change_to_a_file_its_run_did_not_read() {
    if !can_watch_runs() {
        return;
    }
    let workspace = shelf();
    run(workspace.path(), true);

    write(
        &workspace.path().join("shelf/books/b.txt"),
        "second, revised\n",
    );
    let changed = run(workspace.path(), false);

    assert_eq!(
        outcomes(&changed),
        [("a", "satisfied"), ("b", "stale")],
        "{:#?}",
        changed.behaviors[0]
    );
}

#[test]
fn evidence_stands_again_when_the_files_are_as_they_were() {
    if !can_watch_runs() {
        return;
    }
    let workspace = shelf();
    run(workspace.path(), true);
    write(
        &workspace.path().join("shelf/books/a.txt"),
        "first, revised\n",
    );
    run(workspace.path(), true);

    write(&workspace.path().join("shelf/books/a.txt"), "first\n");
    let restored = run(workspace.path(), false);

    let behavior = &restored.behaviors[0];
    assert_eq!(behavior.outcome, "satisfied", "{behavior:#?}");
    assert!(behavior.evidence.iter().all(|used| used.reused));
}

#[test]
fn evidence_ends_when_a_path_its_run_looked_for_appears() {
    if !can_watch_runs() {
        return;
    }
    let workspace = shelf();
    run(workspace.path(), true);

    write(&workspace.path().join("shelf/books/a.new"), "rewritten\n");
    let appeared = run(workspace.path(), false);

    assert_eq!(
        outcomes(&appeared),
        [("a", "stale"), ("b", "satisfied")],
        "{:#?}",
        appeared.behaviors[0]
    );
}

#[test]
fn a_run_that_changes_a_file_it_read_keeps_no_evidence() {
    if !can_watch_runs() {
        return;
    }
    let workspace = shelf();
    // A checker that appends to the book it reads: no state of the book is
    // the one its answer holds for.
    let appending = READ.replace(
        "with open(os.environ[\"LEMMA_OUTPUT\"]",
        "open(path, \"a\").write(\"more\\n\")\nwith open(os.environ[\"LEMMA_OUTPUT\"]",
    );
    write(&workspace.path().join("shelf/tools/read.py"), &appending);
    run(workspace.path(), true);

    let again = run(workspace.path(), false);

    let behavior = &again.behaviors[0];
    assert_eq!(behavior.outcome, "unknown", "{behavior:#?}");
    assert!(behavior.evidence.iter().all(|used| !used.reused));
}

#[test]
fn a_state_come_back_to_keeps_its_evidence_through_many_others() {
    if !can_watch_runs() {
        return;
    }
    let workspace = shelf();
    let book = workspace.path().join("shelf/books/a.txt");
    run(workspace.path(), true);

    // More states than read sets are kept for one case, with the first
    // come back to after each.
    for edit in 0..10 {
        write(&book, &format!("edit {edit}\n"));
        run(workspace.path(), true);
        write(&book, "first\n");
        run(workspace.path(), false);
    }
    let restored = run(workspace.path(), false);

    let behavior = &restored.behaviors[0];
    assert_eq!(behavior.outcome, "satisfied", "{behavior:#?}");
    assert!(behavior.evidence.iter().all(|used| used.reused));
}
