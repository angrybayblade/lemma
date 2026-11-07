//! `lemma mcp` answering tool calls end to end, through the real binary.

#![allow(clippy::unwrap_used, reason = "test setup should fail loudly")]

use std::io::Write;
use std::path::Path;
use std::process::{Command, Stdio};

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

const SPEC: &str = r#"{ "openapi": "3.0.0", "info": { "title": "B", "version": "1" }, "paths": { "/v1/charges": { "post": { "operationId": "createCharge", "responses": { "200": { "description": "ok" } } } } } }"#;

/// Billing and web as in the quickstart; then billing removes the
/// operation web calls, uncommitted.
fn broken_shop(root: &Path) {
    let billing = root.join("billing");
    write(&billing.join("openapi.json"), SPEC);
    write(
        &billing.join("mod.lm"),
        "module billing\n\nimport * as http from lemma.http\n\nexport service api {\n    operations: http.openapi(\"openapi.json\")\n}\n",
    );
    git(&billing, &["init", "-q"]);
    git(&billing, &["add", "."]);
    git(&billing, &["commit", "-q", "-m", "billing"]);
    let web = root.join("web");
    write(
        &web.join("src/charge.ts"),
        "export async function charge() {\n  return fetch(\"/v1/charges\", { method: \"POST\" });\n}\n",
    );
    write(
        &web.join("mod.lm"),
        "module web\n\nexport service web {\n    index: { include: [\"src\"] }\n}\n",
    );
    git(&web, &["init", "-q"]);
    git(&web, &["add", "."]);
    git(&web, &["commit", "-q", "-m", "web"]);
    write(
        &root.join("mod.lm"),
        "module shop\n\nworkspace {\n    repo billing = local(\"billing\")\n    repo web = local(\"web\")\n    config: { level: check }\n}\n",
    );
    let removed = SPEC.replace(r#""/v1/charges""#, r#""/v1/payments""#);
    write(&billing.join("openapi.json"), &removed);
}

/// The replies to `requests`, sent to `lemma mcp` in `root`.
fn serve(root: &Path, requests: &[serde_json::Value]) -> Vec<serde_json::Value> {
    let mut child = Command::new(env!("CARGO_BIN_EXE_lemma"))
        .arg("mcp")
        .current_dir(root)
        .env("HOME", root)
        .env("XDG_CACHE_HOME", root.join(".cache"))
        .env("LEMMA_NO_DAEMON", "1")
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .spawn()
        .unwrap();
    {
        let mut stdin = child.stdin.take().unwrap();
        for request in requests {
            writeln!(stdin, "{request}").unwrap();
        }
    }
    let output = child.wait_with_output().unwrap();
    String::from_utf8(output.stdout)
        .unwrap()
        .lines()
        .map(|line| serde_json::from_str(line).unwrap())
        .collect()
}

#[test]
fn the_check_tool_runs_a_check_and_reports_its_findings() {
    let scratch = tempfile::tempdir().unwrap();
    broken_shop(scratch.path());
    let call = serde_json::json!({
        "jsonrpc": "2.0", "id": 1, "method": "tools/call",
        "params": { "name": "check", "arguments": { "change": "billing", "base": "HEAD" } }
    });

    let replies = serve(scratch.path(), &[call]);

    let result = &replies[0]["result"];
    let text = result["content"][0]["text"].as_str().unwrap();
    assert_eq!(result["isError"], false, "{text}");
    assert!(text.contains("no longer exists"), "{text}");
    assert!(text.contains("1 errors"), "{text}");
}
