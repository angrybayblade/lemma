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

#[test]
fn a_command_starts_no_daemon_when_only_clients_may() {
    let space = tempfile::tempdir().unwrap();
    let runtime = space.path().canonicalize().unwrap();
    let _daemons = Daemons(&runtime);
    let app = runtime.join("app");
    checkout(&app);
    fs::write(app.join("m.py"), "def greet():\n    return 1\n").unwrap();
    let config = runtime.join("config.toml");
    fs::write(&config, "[daemon]\nautostart = \"clients\"\n").unwrap();
    let asked = |args: &[&str]| {
        Command::new(env!("CARGO_BIN_EXE_lemma"))
            .args(args)
            .current_dir(&app)
            .env("XDG_RUNTIME_DIR", &runtime)
            .env("LEMMA_CONFIG", &config)
            .env_remove("LEMMA_DAEMON_SOCKET")
            .env_remove("LEMMA_NO_DAEMON")
            .output()
            .unwrap()
    };

    asked(&["index", "."]);
    let found = said(&asked(&["symbols", "greet"]));

    assert!(found.contains("greet"), "{found}");
    assert_eq!(served_root(&runtime, &app), None);
    let started = said(&asked(&["daemon", "start"]));
    assert!(started.contains("started a lemma daemon"), "{started}");
}

fn git(directory: &Path, args: &[&str]) {
    let done = Command::new("git")
        .arg("-C")
        .arg(directory)
        .args(["-c", "user.name=t", "-c", "user.email=t@example.com"])
        .args(args)
        .output()
        .unwrap();
    assert!(done.status.success(), "git {args:?}: {}", said(&done));
}

/// A workspace of a service and the code calling it, where the service's
/// last commit removed a response field the caller reads.
fn shop_with_a_break(shop: &Path) {
    let document = |fields: &str| {
        format!(
            r#"{{"openapi":"3.0.0","info":{{"title":"Billing","version":"1"}},"paths":{{"/v1/charges":{{"post":{{"operationId":"createCharge","responses":{{"200":{{"content":{{"application/json":{{"schema":{{"type":"object","properties":{{{fields}}}}}}}}}}}}}}}}}}}}}"#
        )
    };
    let (billing, web) = (shop.join("billing"), shop.join("web"));
    fs::create_dir_all(web.join("src")).unwrap();
    fs::create_dir_all(&billing).unwrap();
    fs::write(
        shop.join("mod.lm"),
        "module shop\n\nworkspace {\n    repo billing = local(\"billing\")\n    repo web = local(\"web\")\n    config: { level: check }\n}\n",
    )
    .unwrap();
    fs::write(
        billing.join("mod.lm"),
        "module billing\n\nimport * as http from lemma.http\n\nexport service api {\n    operations: http.openapi(\"openapi.json\")\n}\n",
    )
    .unwrap();
    fs::write(
        web.join("mod.lm"),
        "module web\n\nexport service web {\n    index: { include: [\"src\"] }\n}\n",
    )
    .unwrap();
    fs::write(
        web.join("src/charge.ts"),
        "export async function charge(): Promise<string> {\n  const response = await fetch(\"/v1/charges\", { method: \"POST\" });\n  const body = await response.json();\n  return body.receipt_url;\n}\n",
    )
    .unwrap();
    let both = r#""id":{"type":"string"},"receipt_url":{"type":"string"}"#;
    fs::write(billing.join("openapi.json"), document(both)).unwrap();
    for repository in [&billing, &web] {
        git(repository, &["init", "-q"]);
        git(repository, &["add", "."]);
        git(repository, &["commit", "-q", "-m", "first"]);
    }
    fs::write(
        billing.join("openapi.json"),
        document(r#""id":{"type":"string"}"#),
    )
    .unwrap();
    git(&billing, &["commit", "-q", "-a", "-m", "drop receipt_url"]);
}

#[test]
fn a_check_the_daemon_answers_prints_what_one_in_its_own_process_prints() {
    let space = tempfile::tempdir().unwrap();
    let runtime = space.path().canonicalize().unwrap();
    let _daemons = Daemons(&runtime);
    let shop = runtime.join("shop");
    shop_with_a_break(&shop);
    let check = [
        "check", "--change", "billing", "--base", "HEAD~1", "--head", "HEAD",
    ];
    let alone: Vec<&str> = check.iter().copied().chain(["--no-daemon"]).collect();

    let from_daemon = lemma(&runtime, &shop, &check);
    let own_process = lemma(&runtime, &shop, &alone);

    assert!(
        said(&own_process).contains("receipt_url"),
        "{}",
        said(&own_process)
    );
    assert_eq!(said(&from_daemon), said(&own_process));
    assert_eq!(from_daemon.status.code(), own_process.status.code());
    assert_eq!(
        served_root(&runtime, &shop),
        Some(shop.display().to_string()),
        "the check started the workspace's daemon"
    );
}

#[test]
fn a_verify_the_daemon_answers_prints_what_one_in_its_own_process_prints() {
    let space = tempfile::tempdir().unwrap();
    let runtime = space.path().canonicalize().unwrap();
    let _daemons = Daemons(&runtime);
    let shop = runtime.join("shop");
    shop_with_a_break(&shop);

    for verify in [&["verify", "--links"][..], &["verify", "--json"][..]] {
        let alone: Vec<&str> = verify.iter().copied().chain(["--no-daemon"]).collect();
        let from_daemon = lemma(&runtime, &shop, verify);
        let own_process = lemma(&runtime, &shop, &alone);

        assert!(!said(&own_process).is_empty(), "{verify:?} printed nothing");
        assert_eq!(said(&from_daemon), said(&own_process), "{verify:?}");
        assert_eq!(from_daemon.status.code(), own_process.status.code());
    }
    assert_eq!(
        served_root(&runtime, &shop),
        Some(shop.display().to_string())
    );
}
