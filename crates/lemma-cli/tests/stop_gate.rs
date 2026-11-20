//! The stop gate (`skills/lemma-verify/hooks/stop-check.py`) sends an agent
//! back when its uncommitted edits introduce errors, wherever the workspace
//! keeps its repositories, and says so when it could not check. A gate that
//! passes without having looked is worse than none: these tests run it.

#![allow(
    clippy::unwrap_used,
    clippy::panic,
    reason = "test setup should fail loudly"
)]

use std::io::Write;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};

/// Billing's document: the operation takes `customer` and answers with `id`
/// and `receipt_url`.
const DOCUMENT: &str = r#"{
  "openapi": "3.0.0",
  "info": { "title": "Billing", "version": "1" },
  "paths": {
    "/v1/charges": {
      "post": {
        "operationId": "createCharge",
        "requestBody": { "content": { "application/json": { "schema": {
          "type": "object",
          "properties": { "customer": { "type": "string" } },
          "required": ["customer"]
        } } } },
        "responses": { "200": { "content": { "application/json": { "schema": {
          "type": "object",
          "properties": { "id": { "type": "string" }, "receipt_url": { "type": "string" } }
        } } } } }
      }
    }
  }
}
"#;

/// Web's caller: it sends `customer` and reads `receipt_url`.
const CALLER: &str = r#"export async function charge(customer: string): Promise<string> {
  const response = await fetch("/v1/charges", {
    method: "POST",
    body: JSON.stringify({ customer }),
  });
  const body = await response.json();
  return body.receipt_url;
}
"#;

const BILLING: &str = r#"module billing

import * as http from lemma.http

export service api {
    operations: http.openapi("openapi.json")
}
"#;

const WEB: &str = r#"module web

export service web {
    index: { include: ["src"] }
}
"#;

/// Where a workspace keeps its repositories.
#[derive(Clone, Copy)]
enum Layout {
    /// Each a directory inside the workspace.
    Inside,
    /// Each a directory beside the workspace, as the onboarding guide lays
    /// them out.
    Beside,
    /// Billing is a Git worktree, as an orchestrator gives each task.
    Worktree,
}

/// A two-repository system, committed, and the directory an agent runs in.
struct Shop {
    root: tempfile::TempDir,
    workspace: PathBuf,
    billing: PathBuf,
}

impl Shop {
    fn new(layout: Layout) -> Self {
        let root = tempfile::tempdir().unwrap();
        let workspace = root.path().join("workspace");
        let (billing, web, prefix) = match layout {
            Layout::Inside | Layout::Worktree => {
                (workspace.join("billing"), workspace.join("web"), "")
            }
            Layout::Beside => (root.path().join("billing"), root.path().join("web"), "../"),
        };
        std::fs::create_dir_all(&workspace).unwrap();
        let module = format!(
            "module shop\n\nworkspace {{\n    repo billing = local(\"{prefix}billing\")\n    \
             repo web = local(\"{prefix}web\")\n    config: {{ level: check }}\n}}\n"
        );
        std::fs::write(workspace.join("mod.lm"), module).unwrap();
        let shop = Self {
            root,
            workspace,
            billing,
        };
        let source = match layout {
            Layout::Worktree => shop.root.path().join("billing-source"),
            Layout::Inside | Layout::Beside => shop.billing.clone(),
        };
        shop.repository(&source, &[("openapi.json", DOCUMENT), ("mod.lm", BILLING)]);
        if matches!(layout, Layout::Worktree) {
            let worktree = shop.billing.display().to_string();
            shop.git(&source, &["worktree", "add", "-q", &worktree, "-b", "task"]);
        }
        shop.repository(&web, &[("src/charge.ts", CALLER), ("mod.lm", WEB)]);
        shop
    }

    fn repository(&self, directory: &Path, files: &[(&str, &str)]) {
        for (name, text) in files {
            let path = directory.join(name);
            std::fs::create_dir_all(path.parent().unwrap()).unwrap();
            std::fs::write(path, text).unwrap();
        }
        self.git(directory, &["init", "-q"]);
        self.git(directory, &["add", "."]);
        self.git(directory, &["commit", "-q", "-m", "first"]);
    }

    fn git(&self, directory: &Path, args: &[&str]) {
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
            .env("HOME", self.root.path())
            .status()
            .unwrap();
        assert!(status.success(), "git {args:?}");
    }

    /// Billing's document with `from` replaced by `to`, left uncommitted.
    fn edit_billing(&self, from: &str, to: &str) {
        assert!(DOCUMENT.contains(from), "{from}");
        std::fs::write(
            self.billing.join("openapi.json"),
            DOCUMENT.replace(from, to),
        )
        .unwrap();
    }

    /// An edit `lemma check` reports as an error: a field web never sends
    /// becomes required.
    fn require_a_new_field(&self) {
        self.edit_billing(
            r#""properties": { "customer": { "type": "string" } },
          "required": ["customer"]"#,
            r#""properties": { "customer": { "type": "string" }, "currency": { "type": "string" } },
          "required": ["customer", "currency"]"#,
        );
    }

    /// An edit `lemma check` reports as a warning: a response field web
    /// reads is gone.
    fn drop_a_response_field(&self) {
        self.edit_billing(r#", "receipt_url": { "type": "string" }"#, "");
    }

    /// Commits whatever billing holds, on the branch it is on.
    fn commit_billing(&self) {
        self.git(&self.billing, &["commit", "-q", "-a", "-m", "the change"]);
    }

    /// What the gate prints when an agent working in `place` tries to stop.
    fn gate_from(&self, place: &Path, settings: &[(&str, &str)]) -> String {
        self.gate_in(place, &self.bin(), settings)
    }

    fn bin(&self) -> PathBuf {
        let bin = self.root.path().join("bin");
        std::fs::create_dir_all(&bin).unwrap();
        let lemma = bin.join("lemma");
        if !lemma.exists() {
            std::os::unix::fs::symlink(env!("CARGO_BIN_EXE_lemma"), lemma).unwrap();
        }
        bin
    }

    /// What the gate prints when the agent tries to stop, run with `lemma`
    /// from `bin` first on `PATH`.
    fn gate_with(&self, bin: &Path, settings: &[(&str, &str)]) -> String {
        self.gate_in(&self.workspace, bin, settings)
    }

    fn gate_in(&self, place: &Path, bin: &Path, settings: &[(&str, &str)]) -> String {
        let hook = Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("../../skills/lemma-verify/hooks/stop-check.py");
        let path = format!(
            "{}:{}",
            bin.display(),
            std::env::var("PATH").unwrap_or_default()
        );
        let mut child = Command::new("python3")
            .arg(hook)
            .current_dir(place)
            .env("PATH", path)
            .env("HOME", self.root.path())
            .env("XDG_CACHE_HOME", self.root.path().join(".cache"))
            .env("LEMMA_NO_DAEMON", "1")
            .env_remove("LEMMA_GATE_LEVEL")
            .env_remove("LEMMA_WORKSPACE")
            .envs(settings.iter().copied())
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .spawn()
            .unwrap();
        let input = serde_json::json!({ "cwd": place, "stop_hook_active": false });
        child
            .stdin
            .take()
            .unwrap()
            .write_all(input.to_string().as_bytes())
            .unwrap();
        let output = child.wait_with_output().unwrap();
        assert!(output.status.success(), "the hook itself failed");
        String::from_utf8(output.stdout).unwrap()
    }

    fn gate(&self, settings: &[(&str, &str)]) -> String {
        self.gate_with(&self.bin(), settings)
    }
}

/// The reason the gate gave for sending the agent back.
fn blocked(printed: &str) -> String {
    let answer: serde_json::Value = serde_json::from_str(printed)
        .unwrap_or_else(|_| panic!("the gate let it pass: {printed:?}"));
    assert_eq!(answer["decision"], "block", "{printed}");
    answer["reason"].as_str().unwrap().to_owned()
}

#[test]
fn an_introduced_error_sends_the_agent_back() {
    let shop = Shop::new(Layout::Inside);
    shop.require_a_new_field();

    let reason = blocked(&shop.gate(&[]));

    assert!(reason.contains("currency"), "{reason}");
    assert!(reason.contains("billing"), "{reason}");
}

#[test]
fn an_agent_that_edited_nothing_may_finish() {
    let shop = Shop::new(Layout::Inside);

    assert_eq!(shop.gate(&[]), "");
}

#[test]
fn a_repository_that_is_a_git_worktree_is_checked() {
    let shop = Shop::new(Layout::Worktree);
    shop.require_a_new_field();

    let reason = blocked(&shop.gate(&[]));

    assert!(reason.contains("currency"), "{reason}");
}

#[test]
fn repositories_beside_the_workspace_are_checked() {
    let shop = Shop::new(Layout::Beside);
    shop.require_a_new_field();

    let reason = blocked(&shop.gate(&[]));

    assert!(reason.contains("currency"), "{reason}");
}

#[test]
fn an_introduced_warning_sends_the_agent_back_unless_the_gate_is_set_to_errors() {
    let shop = Shop::new(Layout::Inside);
    shop.drop_a_response_field();

    let reason = blocked(&shop.gate(&[]));
    let errors_only = shop.gate(&[("LEMMA_GATE_LEVEL", "error")]);

    assert!(reason.contains("receipt_url"), "{reason}");
    assert!(reason.contains("- warning ["), "{reason}");
    assert_eq!(errors_only, "");
}

#[test]
fn a_level_the_gate_does_not_know_is_said_and_not_guessed_at() {
    let shop = Shop::new(Layout::Inside);

    let reason = blocked(&shop.gate(&[("LEMMA_GATE_LEVEL", "errors")]));

    assert!(reason.contains("must be `warning` or `error`"), "{reason}");
}

#[test]
fn a_check_that_could_not_run_is_said_not_passed() {
    let shop = Shop::new(Layout::Inside);
    shop.require_a_new_field();
    // A `lemma` that fails without an answer, first on `PATH`.
    let broken = shop.root.path().join("broken");
    std::fs::create_dir_all(&broken).unwrap();
    let script = broken.join("lemma");
    std::fs::write(
        &script,
        "#!/bin/sh\necho 'the index is locked' >&2\nexit 1\n",
    )
    .unwrap();
    let mut permissions = std::fs::metadata(&script).unwrap().permissions();
    std::os::unix::fs::PermissionsExt::set_mode(&mut permissions, 0o755);
    std::fs::set_permissions(&script, permissions).unwrap();

    let reason = blocked(&shop.gate_with(&broken, &[]));

    assert!(reason.contains("could not check"), "{reason}");
    assert!(reason.contains("the index is locked"), "{reason}");
}

#[test]
fn a_change_committed_on_a_branch_is_checked() {
    let shop = Shop::new(Layout::Worktree);
    shop.require_a_new_field();
    shop.commit_billing();

    let reason = blocked(&shop.gate(&[]));

    assert!(reason.contains("currency"), "{reason}");
}

#[test]
fn an_agent_inside_a_repository_is_checked_from_the_workspace_above() {
    let shop = Shop::new(Layout::Inside);
    shop.require_a_new_field();

    let reason = blocked(&shop.gate_from(&shop.billing, &[]));

    assert!(reason.contains("currency"), "{reason}");
}

#[test]
fn a_workspace_beside_the_repository_is_found_through_lemma_workspace() {
    let shop = Shop::new(Layout::Beside);
    shop.require_a_new_field();
    let workspace = shop.workspace.display().to_string();

    let reason = blocked(&shop.gate_from(&shop.billing, &[("LEMMA_WORKSPACE", &workspace)]));

    assert!(reason.contains("currency"), "{reason}");
}

#[test]
fn a_workspace_that_is_a_repository_does_not_count_its_repositories_as_edits() {
    let shop = Shop::new(Layout::Inside);
    // The workspace's own repository holds its module; the repositories
    // inside it are their own, untracked here.
    shop.git(&shop.workspace, &["init", "-q"]);
    shop.git(&shop.workspace, &["add", "mod.lm"]);
    shop.git(&shop.workspace, &["commit", "-q", "-m", "workspace"]);

    assert_eq!(shop.gate(&[]), "");
}

#[test]
fn a_project_without_lemma_is_left_alone() {
    let shop = Shop::new(Layout::Inside);
    let elsewhere = shop.root.path().join("elsewhere");
    std::fs::create_dir_all(&elsewhere).unwrap();

    assert_eq!(shop.gate_from(&elsewhere, &[]), "");
}
