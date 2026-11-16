//! Each workspace is served by a daemon of its own
//! (`.specs/21-workspace-daemons.md`): these run real daemons, in a runtime
//! directory of the test's own.
#![cfg(unix)]
#![allow(clippy::unwrap_used, reason = "test setup should fail loudly")]

use std::fs;
use std::path::Path;
use std::process::{Command, Output};

/// Runs `lemma` in `directory`, with its daemons' sockets under `runtime`.
fn lemma(runtime: &Path, directory: &Path, args: &[&str]) -> Output {
    Command::new(env!("CARGO_BIN_EXE_lemma"))
        .args(args)
        .current_dir(directory)
        .env("XDG_RUNTIME_DIR", runtime)
        .env_remove("LEMMA_DAEMON_SOCKET")
        .env_remove("LEMMA_NO_DAEMON")
        .output()
        .unwrap()
}

fn said(output: &Output) -> String {
    format!(
        "{}{}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    )
}

/// The workspace root that `lemma daemon status` run in `directory` names.
fn served_root(runtime: &Path, directory: &Path) -> Option<String> {
    let status = said(&lemma(runtime, directory, &["daemon", "status"]));
    let root = status
        .lines()
        .find_map(|line| line.strip_prefix("workspace: "))?;
    Some(root.to_owned())
}

/// A directory that is a Git checkout as far as a `.git` in it says.
fn checkout(directory: &Path) {
    fs::create_dir_all(directory.join(".git")).unwrap();
}

/// Stops every daemon a test started, whatever it asserted.
struct Daemons<'a>(&'a Path);

impl Drop for Daemons<'_> {
    fn drop(&mut self) {
        lemma(self.0, self.0, &["daemon", "stop", "--all"]);
    }
}

#[test]
fn a_daemon_started_from_a_subdirectory_serves_the_whole_checkout() {
    let space = tempfile::tempdir().unwrap();
    let runtime = space.path().canonicalize().unwrap();
    let _daemons = Daemons(&runtime);
    let app = runtime.join("app");
    let deep = app.join("src/deep");
    checkout(&app);
    fs::create_dir_all(&deep).unwrap();

    let started = said(&lemma(&runtime, &deep, &["daemon", "start"]));

    let root = app.display().to_string();
    assert!(started.contains(&format!("serves {root}")), "{started}");
    assert_eq!(served_root(&runtime, &app), Some(root.clone()));
    assert_eq!(served_root(&runtime, &deep), Some(root));
}

#[test]
fn two_checkouts_and_a_worktree_inside_one_each_have_a_daemon() {
    let space = tempfile::tempdir().unwrap();
    let runtime = space.path().canonicalize().unwrap();
    let _daemons = Daemons(&runtime);
    let app = runtime.join("app");
    let other = runtime.join("other");
    let worktree = app.join(".claude/worktrees/task");
    checkout(&app);
    checkout(&other);
    fs::create_dir_all(&worktree).unwrap();
    fs::write(worktree.join(".git"), "gitdir: elsewhere\n").unwrap();

    for directory in [&app, &other, &worktree] {
        lemma(&runtime, directory, &["daemon", "start"]);
    }

    assert_eq!(served_root(&runtime, &app), Some(app.display().to_string()));
    assert_eq!(
        served_root(&runtime, &other),
        Some(other.display().to_string())
    );
    assert_eq!(
        served_root(&runtime, &worktree),
        Some(worktree.display().to_string())
    );
    let all = said(&lemma(&runtime, &runtime, &["daemon", "status", "--all"]));
    assert_eq!(all.matches("workspace: ").count(), 3, "{all}");
}

#[test]
fn stopping_a_workspaces_daemon_leaves_the_others_running() {
    let space = tempfile::tempdir().unwrap();
    let runtime = space.path().canonicalize().unwrap();
    let _daemons = Daemons(&runtime);
    let app = runtime.join("app");
    let other = runtime.join("other");
    checkout(&app);
    checkout(&other);
    lemma(&runtime, &app, &["daemon", "start"]);
    lemma(&runtime, &other, &["daemon", "start"]);

    lemma(&runtime, &app, &["daemon", "stop"]);

    assert_eq!(served_root(&runtime, &app), None);
    assert_eq!(
        served_root(&runtime, &other),
        Some(other.display().to_string())
    );
}

#[test]
fn a_directory_no_daemon_serves_is_told_how_to_start_one() {
    let space = tempfile::tempdir().unwrap();
    let runtime = space.path().canonicalize().unwrap();

    let status = said(&lemma(&runtime, &runtime, &["daemon", "status"]));

    assert!(
        status.contains("no lemma daemon serves this directory"),
        "{status}"
    );
    assert!(status.contains("lemma daemon start"), "{status}");
}
