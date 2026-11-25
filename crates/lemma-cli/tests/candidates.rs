//! Code a description leaves out that writes the path of a described
//! operation, through the real binary: `lemma verify` reports the
//! directory as a candidate, never fails for it, and says how much test
//! code writes such paths, since tests are never read, and how many included
//! files are in a language it has no reader for.

#![allow(clippy::unwrap_used, reason = "test setup should fail loudly")]

use std::path::Path;
use std::process::{Command, Output};

fn write(path: &Path, text: &str) {
    std::fs::create_dir_all(path.parent().unwrap()).unwrap();
    std::fs::write(path, text).unwrap();
}

fn git(directory: &Path, args: &[&str]) {
    let status = Command::new("git")
        .arg("-C")
        .arg(directory)
        .args(["-c", "user.name=t", "-c", "user.email=t@t"])
        .args(["-c", "commit.gpgsign=false"])
        .args(args)
        .status()
        .unwrap();
    assert!(status.success(), "git {args:?}");
}

fn commit_all(directory: &Path) {
    git(directory, &["init", "-q"]);
    git(directory, &["add", "."]);
    git(directory, &["commit", "-q", "-m", "first"]);
}

fn lemma(directory: &Path, args: &[&str]) -> Output {
    Command::new(env!("CARGO_BIN_EXE_lemma"))
        .args(args)
        .current_dir(directory)
        .env("HOME", directory)
        .env("XDG_CACHE_HOME", directory.join(".cache"))
        .env("LEMMA_NO_DAEMON", "1")
        .output()
        .unwrap()
}

fn text(output: &Output) -> String {
    format!(
        "{}{}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    )
}

const OPENAPI: &str = r#"{ "openapi": "3.0.0", "info": { "title": "B", "version": "1" }, "paths": {
  "/v1/invoices": { "get": { "operationId": "listInvoices", "responses": { "200": { "description": "ok" } } } },
  "/v1/invoices/{id}": { "get": { "operationId": "getInvoice",
    "parameters": [{ "name": "id", "in": "path", "required": true, "schema": { "type": "string" } }],
    "responses": { "200": { "description": "ok" } } } } } }"#;

// The service's code, which the description includes.
const API: &str = "export const invoices = () => fetch('/v1/invoices')\n";
// A script beside it, which the description leaves out.
const SEED: &str = "export const show = (id: string) => fetch(`${process.env.API}/v1/invoices/${id}`)\nexport const list = () => fetch(`${process.env.API}/v1/invoices`)\n";
// A test of the service's code.
const API_TEST: &str = "test('lists', () => expect(fetch).toHaveBeenCalledWith('/v1/invoices'))\n";

/// Billing's document, and web with its `src` described, a script and a
/// test beside it, composed in `root`. `web_services` is what web's
/// description says of its code.
fn system(root: &Path, web_services: &str) {
    let billing = root.join("billing");
    write(&billing.join("openapi.json"), OPENAPI);
    write(
        &billing.join("mod.lm"),
        "module billing\n\nimport * as http from lemma.http\n\nexport service api {\n    operations: http.openapi(\"openapi.json\")\n}\n",
    );
    commit_all(&billing);
    let web = root.join("web");
    write(&web.join("src/api.ts"), API);
    write(&web.join("src/api.test.ts"), API_TEST);
    write(&web.join("scripts/seed.ts"), SEED);
    write(
        &web.join("mod.lm"),
        &format!("module web\n\nexport service web {{\n    {web_services}\n}}\n"),
    );
    commit_all(&web);
    write(
        &root.join("mod.lm"),
        "module shop\n\nworkspace {\n    repo billing = local(\"billing\")\n    repo web = local(\"web\")\n    config: { level: check }\n}\n",
    );
}

#[test]
fn verify_reports_a_directory_left_out_that_writes_described_paths() {
    let scratch = tempfile::tempdir().unwrap();
    system(scratch.path(), "index: { include: [\"src\"] }");

    let output = lemma(scratch.path(), &["verify", "--no-daemon"]);

    let printed = text(&output);
    assert_eq!(output.status.code(), Some(0), "{printed}");
    assert!(
        printed.contains(
            "candidate #web:scripts  2 places in 1 files write the paths of 2 described operations, and no service includes it"
        ),
        "{printed}"
    );
    assert!(
        printed.contains("first: scripts/seed.ts:1  GET /v1/invoices/{}"),
        "{printed}"
    );
    assert!(printed.contains("[calls_outside_services]"), "{printed}");
    assert!(
        printed.contains("0 errors, 0 warnings, 0 informational, 1 candidates"),
        "{printed}"
    );
}

#[test]
fn verify_gives_the_places_in_json() {
    let scratch = tempfile::tempdir().unwrap();
    system(scratch.path(), "index: { include: [\"src\"] }");

    let output = lemma(scratch.path(), &["verify", "--no-daemon", "--json"]);

    let report: serde_json::Value = serde_json::from_slice(&output.stdout).unwrap();
    let candidates = report["candidates"].as_array().unwrap();
    assert_eq!(candidates.len(), 1, "{report:#}");
    assert_eq!(
        candidates[0]["name"], "calls_outside_services",
        "{report:#}"
    );
    assert_eq!(candidates[0]["location"], "#web:scripts", "{report:#}");
    assert_eq!(candidates[0]["operations"], 2, "{report:#}");
    let places = candidates[0]["places"].as_array().unwrap();
    assert_eq!(places.len(), 2, "{report:#}");
    assert_eq!(places[1]["file"], "scripts/seed.ts", "{report:#}");
    assert_eq!(places[1]["line"], 2, "{report:#}");
    assert_eq!(places[1]["method"], "GET", "{report:#}");
    assert_eq!(places[1]["path"], "/v1/invoices", "{report:#}");
}

#[test]
fn a_directory_the_description_includes_or_excludes_is_no_candidate() {
    let included = tempfile::tempdir().unwrap();
    system(
        included.path(),
        "index: { include: [\"src\", \"scripts\"] }",
    );
    let excluded = tempfile::tempdir().unwrap();
    system(
        excluded.path(),
        "index: { include: [\"src\"], exclude: [\"scripts/**\"] }",
    );

    let with_include = text(&lemma(included.path(), &["verify", "--no-daemon"]));
    let with_exclude = text(&lemma(excluded.path(), &["verify", "--no-daemon"]));

    assert!(!with_include.contains("candidate"), "{with_include}");
    assert!(
        with_include.contains("3 calls of them read from code"),
        "{with_include}"
    );
    assert!(!with_exclude.contains("candidate"), "{with_exclude}");
}

#[test]
fn verify_counts_the_tests_that_write_described_paths() {
    let scratch = tempfile::tempdir().unwrap();
    system(scratch.path(), "index: { include: [\"src\"] }");

    let output = lemma(scratch.path(), &["verify", "--no-daemon", "--json"]);

    let printed = text(&lemma(scratch.path(), &["verify", "--no-daemon"]));
    let report: serde_json::Value = serde_json::from_slice(&output.stdout).unwrap();
    assert!(
        printed.contains(
            "tests are not read: 1 places in 1 test files write a described operation's path (#web 1)"
        ),
        "{printed}"
    );
    let tests = report["looked_at"]["calls_in_tests"].as_array().unwrap();
    assert_eq!(tests.len(), 1, "{report:#}");
    assert_eq!(tests[0]["repository"], "#web", "{report:#}");
    assert_eq!(tests[0]["files"], 1, "{report:#}");
    assert_eq!(tests[0]["places"], 1, "{report:#}");
}

#[test]
fn verify_counts_the_included_files_in_languages_it_does_not_read() {
    let scratch = tempfile::tempdir().unwrap();
    system(scratch.path(), "index: { include: [\"src\"] }");
    let web = scratch.path().join("web");
    let component = "<script>\nfetch('/v1/invoices')\n</script>\n";
    write(&web.join("src/Invoices.vue"), component);
    write(&web.join("src/pages/Invoice.vue"), component);
    write(&web.join("src/Invoices.test.vue"), component);
    write(&web.join("scripts/Seed.vue"), component);
    git(&web, &["add", "."]);
    git(&web, &["commit", "-q", "-m", "components"]);

    let output = lemma(scratch.path(), &["verify", "--no-daemon", "--json"]);

    let printed = text(&lemma(scratch.path(), &["verify", "--no-daemon"]));
    let report: serde_json::Value = serde_json::from_slice(&output.stdout).unwrap();
    assert!(
        printed.contains("2 included files are in languages Lemma does not read (#web Vue 2)"),
        "{printed}"
    );
    let unread = report["looked_at"]["unread_files"].as_array().unwrap();
    assert_eq!(unread.len(), 1, "{report:#}");
    assert_eq!(unread[0]["repository"], "#web", "{report:#}");
    assert_eq!(unread[0]["language"], "Vue", "{report:#}");
    assert_eq!(unread[0]["files"], 2, "{report:#}");
}
