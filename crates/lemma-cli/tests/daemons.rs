//! Each workspace is served by a daemon of its own
//! (`.specs/21-workspace-daemons.md`): these run real daemons, in a runtime
//! directory of the test's own.
#![cfg(unix)]
#![allow(clippy::unwrap_used, reason = "test setup should fail loudly")]

use std::fs;
use std::path::Path;
use std::process::{Command, Output};

/// A directory for a test's checkouts and its daemons' sockets. It is made
/// in `/tmp`, not the system's temporary directory, whose path on macOS is
/// too long for a socket: the daemons would then all listen in one shared
/// directory, and one test stopping its own would stop another's.
fn space() -> tempfile::TempDir {
    tempfile::Builder::new()
        .prefix("lm")
        .tempdir_in("/tmp")
        .unwrap()
}

/// `lemma` to run in `directory`, with its daemons' sockets under
/// `runtime` and nothing of the machine the test runs on: no settings file,
/// and none of the variables that turn the daemon or sentry off, `CI`
/// among them, which a build server sets.
fn command(runtime: &Path, directory: &Path) -> Command {
    let mut command = Command::new(env!("CARGO_BIN_EXE_lemma"));
    command
        .current_dir(directory)
        .env("XDG_RUNTIME_DIR", runtime)
        .env("LEMMA_CONFIG", runtime.join("no-settings.toml"))
        .env_remove("LEMMA_DAEMON_SOCKET")
        .env_remove("LEMMA_NO_DAEMON")
        .env_remove("LEMMA_SENTRY")
        .env_remove("CI");
    command
}

/// Runs `lemma` in `directory`, with its daemons' sockets under `runtime`.
fn lemma(runtime: &Path, directory: &Path, args: &[&str]) -> Output {
    command(runtime, directory).args(args).output().unwrap()
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
    let space = space();
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
    let space = space();
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
    let space = space();
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
    let space = space();
    let runtime = space.path().canonicalize().unwrap();

    let status = said(&lemma(&runtime, &runtime, &["daemon", "status"]));

    assert!(
        status.contains("no lemma daemon serves this directory"),
        "{status}"
    );
    let starts = format!(
        "`lemma daemon start` starts one for the workspace {}",
        runtime.display()
    );
    assert!(status.contains(&starts), "{status}");
}

#[test]
fn a_command_starts_no_daemon_when_only_clients_may() {
    let space = space();
    let runtime = space.path().canonicalize().unwrap();
    let _daemons = Daemons(&runtime);
    let app = runtime.join("app");
    checkout(&app);
    fs::write(app.join("m.py"), "def greet():\n    return 1\n").unwrap();
    let config = runtime.join("config.toml");
    fs::write(&config, "[daemon]\nautostart = \"clients\"\n").unwrap();
    let asked = |args: &[&str]| {
        command(&runtime, &app)
            .args(args)
            .env("LEMMA_CONFIG", &config)
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
    let space = space();
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
    // Any command asks for sentry unless told otherwise, and sentry names
    // each answer in a last line of its own.
    let report = said(&from_daemon);
    let (findings, token) = report.rsplit_once("sentry token: ").unwrap();
    assert_eq!(findings, said(&own_process));
    assert!(!token.trim().is_empty(), "{report}");
    assert_eq!(from_daemon.status.code(), own_process.status.code());
    assert_eq!(
        served_root(&runtime, &shop),
        Some(shop.display().to_string()),
        "the check started the workspace's daemon"
    );
    assert_eq!(
        sentry_said(&runtime, &shop, |said| !said.is_empty()),
        "sentry is kept for commands"
    );
}

#[test]
fn a_verify_the_daemon_answers_prints_what_one_in_its_own_process_prints() {
    let space = space();
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

/// What `lemma daemon status` in `directory` says about sentry, once it
/// says what `wanted` accepts; what it said last if it never does.
fn sentry_said(runtime: &Path, directory: &Path, wanted: impl Fn(&str) -> bool) -> String {
    let mut said_last = String::new();
    for _ in 0..100 {
        let status = said(&lemma(runtime, directory, &["daemon", "status"]));
        said_last = status
            .lines()
            .filter(|line| line.starts_with("sentry is "))
            .collect();
        if wanted(&said_last) {
            break;
        }
        std::thread::sleep(std::time::Duration::from_millis(50));
    }
    said_last
}

#[test]
fn sentry_asked_for_by_hand_starts_the_daemon_and_stays() {
    let space = space();
    let runtime = space.path().canonicalize().unwrap();
    let _daemons = Daemons(&runtime);
    let shop = runtime.join("shop");
    shop_with_a_break(&shop);

    let started = said(&lemma(&runtime, &shop, &["daemon", "sentry", "start"]));

    assert!(started.contains("attached to"), "{started}");
    assert_eq!(
        served_root(&runtime, &shop),
        Some(shop.display().to_string())
    );
    assert_eq!(
        sentry_said(&runtime, &shop, |said| !said.is_empty()),
        "sentry is kept for commands"
    );
}

#[test]
fn sentry_an_mcp_server_asked_for_is_let_go_when_the_server_leaves() {
    let space = space();
    let runtime = space.path().canonicalize().unwrap();
    let _daemons = Daemons(&runtime);
    let shop = runtime.join("shop");
    shop_with_a_break(&shop);
    let mut server = command(&runtime, &shop)
        .arg("mcp")
        .stdin(std::process::Stdio::piped())
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .spawn()
        .unwrap();

    let held = sentry_said(&runtime, &shop, |said| !said.is_empty());
    drop(server.stdin.take());
    server.wait().unwrap();
    let after = sentry_said(&runtime, &shop, str::is_empty);

    assert_eq!(held, "sentry is kept by 1 connected client");
    assert_eq!(after, "");
}

#[test]
fn a_check_without_sentry_prints_what_one_in_its_own_process_prints() {
    let space = space();
    let runtime = space.path().canonicalize().unwrap();
    let _daemons = Daemons(&runtime);
    let shop = runtime.join("shop");
    shop_with_a_break(&shop);
    let check = [
        "check", "--change", "billing", "--base", "HEAD~1", "--head", "HEAD",
    ];
    let alone: Vec<&str> = check.iter().copied().chain(["--no-daemon"]).collect();
    let without_sentry = command(&runtime, &shop)
        .args(check)
        .env("LEMMA_SENTRY", "off")
        .output()
        .unwrap();

    let own_process = lemma(&runtime, &shop, &alone);

    assert_eq!(said(&without_sentry), said(&own_process));
    assert_eq!(sentry_said(&runtime, &shop, str::is_empty), "");
}

/// A checkout holding one Python function, indexed.
fn checkout_defining(runtime: &Path, directory: &Path, function: &str) {
    fs::create_dir_all(directory.join("src")).unwrap();
    if !directory.join(".git").exists() {
        checkout(directory);
    }
    let source = format!("def {function}():\n    return 1\n");
    fs::write(directory.join("src/m.py"), source).unwrap();
    let indexed = lemma(runtime, directory, &["index", ".", "--no-daemon"]);
    assert!(indexed.status.success(), "{}", said(&indexed));
}

#[test]
fn every_place_is_answered_about_its_own_code_whichever_daemons_run() {
    let space = space();
    let runtime = space.path().canonicalize().unwrap();
    let _daemons = Daemons(&runtime);
    let app = runtime.join("app");
    let sibling = runtime.join("sibling");
    let worktree = app.join(".claude/worktrees/task");
    fs::create_dir_all(&worktree).unwrap();
    fs::write(worktree.join(".git"), "gitdir: elsewhere\n").unwrap();
    checkout_defining(&runtime, &app, "in_the_app");
    checkout_defining(&runtime, &sibling, "in_the_sibling");
    checkout_defining(&runtime, &worktree, "in_the_worktree");
    // The app's daemon runs first, above the worktree and beside the
    // sibling: the one each would be given by a client that only walked up.
    lemma(&runtime, &app, &["daemon", "start"]);
    // Where each command is asked from, the file it outlines from there,
    // and the function only that checkout defines.
    let places = [
        (app.clone(), "src/m.py", "in_the_app"),
        (app.join("src"), "m.py", "in_the_app"),
        (sibling.clone(), "src/m.py", "in_the_sibling"),
        (worktree.clone(), "src/m.py", "in_the_worktree"),
        (worktree.join("src"), "m.py", "in_the_worktree"),
    ];

    for (place, file, function) in &places {
        for asked in [["symbols", "in_the"], ["outline", file]] {
            let alone: Vec<&str> = asked.iter().copied().chain(["--no-daemon"]).collect();

            let from_daemon = said(&lemma(&runtime, place, &asked));
            let own_process = said(&lemma(&runtime, place, &alone));

            let asked_in = format!("{asked:?} in {}", place.display());
            assert!(own_process.contains(function), "{asked_in}: {own_process}");
            assert_eq!(from_daemon, own_process, "{asked_in}");
        }
    }
    let all = said(&lemma(&runtime, &runtime, &["daemon", "status", "--all"]));
    assert_eq!(all.matches("workspace: ").count(), 3, "{all}");
}

#[test]
fn a_file_in_another_checkout_is_refused_and_never_answered_from_this_one() {
    let space = space();
    let runtime = space.path().canonicalize().unwrap();
    let _daemons = Daemons(&runtime);
    let app = runtime.join("app");
    let sibling = runtime.join("sibling");
    checkout_defining(&runtime, &app, "in_the_app");
    checkout_defining(&runtime, &sibling, "in_the_sibling");
    let elsewhere = sibling.join("src/m.py");

    let asked = lemma(&runtime, &app, &["outline", elsewhere.to_str().unwrap()]);

    let answer = said(&asked);
    assert!(!asked.status.success(), "{answer}");
    assert!(
        answer.contains("is outside the indexed directory"),
        "{answer}"
    );
    assert!(
        answer.contains("run the command in the repository"),
        "{answer}"
    );
    assert!(!answer.contains("in_the_app"), "{answer}");
}

/// A language server that answers `initialize`, the first request a client
/// sends, and nothing after it. It is `/bin/sh`, so that the tests need
/// nothing installed.
const SILENT_SERVER: &str = r#"#!/bin/sh
read -r header
body='{"jsonrpc":"2.0","id":1,"result":{"capabilities":{"referencesProvider":true}}}'
printf 'Content-Length: %s\r\n\r\n%s' "${#body}" "$body"
exec cat > /dev/null
"#;

/// A checkout defining `price` in Python, whose language server starts and
/// then never answers.
fn checkout_with_a_silent_server(runtime: &Path, directory: &Path) {
    use std::os::unix::fs::PermissionsExt;

    checkout_defining(runtime, directory, "price");
    let server = directory.join(".lemma/silent-server");
    fs::write(&server, SILENT_SERVER).unwrap();
    fs::set_permissions(&server, fs::Permissions::from_mode(0o755)).unwrap();
    let providers = format!(
        "[[provider]]\nid = \"pyright\"\nlanguages = [\"python\"]\ncommand = \"{}\"\n",
        server.display()
    );
    fs::write(directory.join(".lemma/providers.toml"), providers).unwrap();
}

#[test]
fn a_lookup_waits_for_its_server_as_long_as_it_was_told_and_no_longer() {
    let space = space();
    let runtime = space.path().canonicalize().unwrap();
    let _daemons = Daemons(&runtime);
    let app = runtime.join("app");
    checkout_with_a_silent_server(&runtime, &app);
    // Indexing through the daemon starts the server ahead of any lookup,
    // with the long wait that starting is allowed.
    let indexed = lemma(&runtime, &app, &["index", "."]);
    assert!(indexed.status.success(), "{}", said(&indexed));

    let asked = lemma(
        &runtime,
        &app,
        &["refs", "price", "--refresh", "--timeout-secs", "2"],
    );

    let answer = said(&asked);
    assert!(!asked.status.success(), "{answer}");
    let waited = answer
        .split("timed out after ")
        .nth(1)
        .and_then(|rest| rest.split(' ').next())
        .and_then(|seconds| seconds.parse::<u64>().ok());
    assert!(waited.is_some_and(|seconds| seconds <= 2), "{answer}");
}
