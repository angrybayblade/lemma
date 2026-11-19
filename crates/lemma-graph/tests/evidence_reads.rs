//! Evidence kept by what its checker's run read (spec 13, section 10): a
//! change to a file the run did not read leaves it standing, a change to
//! one it read ends it, and it stands again when the files are as they
//! were. These need something that can watch a run: they fail on Linux
//! without `strace`, and are ignored, not passed, where Lemma has no
//! recorder yet.

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

/// Fails unless this machine can watch a run: on Linux, with `strace`.
/// A test that cannot watch would otherwise pass without testing anything.
fn require_recorder() {
    let traced = Command::new("strace")
        .args(["-qq", "-e", "trace=chdir", "-o", "/dev/null", "true"])
        .status();
    assert!(
        traced.is_ok_and(|status| status.success()),
        "these tests watch runs with `strace`; install it"
    );
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
#[cfg_attr(
    not(target_os = "linux"),
    ignore = "no recorder watches runs on this platform yet (spec 13, section 10)"
)]
fn evidence_outlives_a_change_to_a_file_its_run_did_not_read() {
    require_recorder();
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
    let stale = changed.behaviors[0].cases.iter().find_map(|case| {
        let problem = case.problem.as_ref()?;
        Some(problem.message.as_str())
    });
    assert_eq!(
        stale,
        Some(
            "the stored evidence is stale: books/b.txt changed since the evidence was kept, and checkers may not run"
        )
    );
}

#[test]
#[cfg_attr(
    not(target_os = "linux"),
    ignore = "no recorder watches runs on this platform yet (spec 13, section 10)"
)]
fn evidence_stands_again_when_the_files_are_as_they_were() {
    require_recorder();
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
#[cfg_attr(
    not(target_os = "linux"),
    ignore = "no recorder watches runs on this platform yet (spec 13, section 10)"
)]
fn evidence_ends_when_a_path_its_run_looked_for_appears() {
    require_recorder();
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
#[cfg_attr(
    not(target_os = "linux"),
    ignore = "no recorder watches runs on this platform yet (spec 13, section 10)"
)]
fn a_run_that_changes_a_file_it_read_keeps_no_evidence() {
    require_recorder();
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
#[cfg_attr(
    not(target_os = "linux"),
    ignore = "no recorder watches runs on this platform yet (spec 13, section 10)"
)]
fn a_state_come_back_to_keeps_its_evidence_through_many_others() {
    require_recorder();
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

/// The shelf with `books` a link to one of two editions, committed.
fn shelf_of_editions() -> tempfile::TempDir {
    let workspace = shelf();
    let root = workspace.path().join("shelf");
    std::fs::create_dir_all(root.join("editions")).unwrap();
    std::fs::rename(root.join("books"), root.join("editions/first")).unwrap();
    write(&root.join("editions/second/a.txt"), "first, again\n");
    write(&root.join("editions/second/b.txt"), "second, again\n");
    std::os::unix::fs::symlink("editions/first", root.join("books")).unwrap();
    git(&root, &["add", "."]);
    git(&root, &["commit", "-q", "-m", "editions"]);
    workspace
}

fn point_books_at(workspace: &Path, edition: &str) {
    let books = workspace.join("shelf/books");
    std::fs::remove_file(&books).unwrap();
    std::os::unix::fs::symlink(format!("editions/{edition}"), books).unwrap();
}

#[test]
#[cfg_attr(
    not(target_os = "linux"),
    ignore = "no recorder watches runs on this platform yet (spec 13, section 10)"
)]
fn evidence_ends_when_a_path_looked_for_through_a_link_appears() {
    require_recorder();
    let workspace = shelf();
    // A checker that looks for the new book by another name of the
    // repository: a link beside it.
    std::os::unix::fs::symlink("shelf", workspace.path().join("linked")).unwrap();
    let by_another_name = READ.replace(
        "path = os.path.join(given[\"root\"], \"books\", name + \".new\")",
        "path = os.path.join(given[\"root\"], \"..\", \"linked\", \"books\", name + \".new\")",
    );
    assert_ne!(by_another_name, READ);
    write(
        &workspace.path().join("shelf/tools/read.py"),
        &by_another_name,
    );
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
#[cfg_attr(
    not(target_os = "linux"),
    ignore = "no recorder watches runs on this platform yet (spec 13, section 10)"
)]
fn evidence_ends_when_a_link_on_the_way_to_a_file_points_elsewhere() {
    require_recorder();
    let workspace = shelf_of_editions();
    run(workspace.path(), true);

    point_books_at(workspace.path(), "second");
    let moved = run(workspace.path(), false);

    assert_eq!(
        outcomes(&moved),
        [("a", "stale"), ("b", "stale")],
        "{:#?}",
        moved.behaviors[0]
    );
}

#[test]
#[cfg_attr(
    not(target_os = "linux"),
    ignore = "no recorder watches runs on this platform yet (spec 13, section 10)"
)]
fn evidence_ends_when_a_link_the_run_resolved_points_elsewhere() {
    require_recorder();
    let workspace = shelf_of_editions();
    // A checker that resolves the link itself and opens the file where it
    // led, as a module loader does.
    let resolving = READ.replace(
        "given[\"root\"], \"books\", name + \".txt\")",
        "os.path.realpath(os.path.join(given[\"root\"], \"books\")), name + \".txt\")",
    );
    assert_ne!(resolving, READ);
    write(&workspace.path().join("shelf/tools/read.py"), &resolving);
    run(workspace.path(), true);

    point_books_at(workspace.path(), "second");
    let moved = run(workspace.path(), false);

    assert_eq!(
        outcomes(&moved),
        [("a", "stale"), ("b", "stale")],
        "{:#?}",
        moved.behaviors[0]
    );
}
