//! `lemma verify` on a description that declares a client operation and
//! links it to nothing: the declaration checks nothing, and saying so is
//! the only sign of it.

#![allow(clippy::unwrap_used, reason = "test setup should fail loudly")]

use std::path::Path;
use std::process::{Command, Output};

fn write(path: &Path, contents: &str) {
    std::fs::create_dir_all(path.parent().unwrap()).unwrap();
    std::fs::write(path, contents).unwrap();
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

fn verify(directory: &Path) -> Output {
    Command::new(env!("CARGO_BIN_EXE_lemma"))
        .arg("verify")
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

const BILLING: &str = "module billing\n\nimport * as typescript from lemma.typescript\n\nlet charges = typescript.file(\"src/charges.ts\")\n\nexport service api {\n    index: { include: [\"src\"] }\n\n    export operation create_charge {\n        implementation: charges.symbol(\"createCharge\")\n    }\n\n    export operation refund {\n        implementation: charges.symbol(\"refund\")\n    }\n}\n";

/// A caller declaring two client operations, `call` and `undo`, with
/// `links` as its graph of links.
fn web(links: &str) -> String {
    format!(
        "module web\n\nimport * as typescript from lemma.typescript\nimport {{ Link }} from lemma.links\nimport {{ api }} from billing\n\nrepo billing = local(\"../billing\")\n\nlet calls = typescript.file(\"src/calls.ts\")\n\nexport service client {{\n    index: {{ include: [\"src\"] }}\n\n    export client operation call {{\n        implementation: calls.symbol(\"call\")\n    }}\n\n    export client operation undo {{\n        implementation: calls.symbol(\"undo\")\n    }}\n}}\n\nexport graph links : Link {{\n{links}}}\n"
    )
}

/// Billing and its caller, composed by a parent module ending in `parent`.
fn system(root: &Path, links: &str, parent: &str) {
    let billing = root.join("billing");
    write(
        &billing.join("src/charges.ts"),
        "export function createCharge() {}\nexport function refund() {}\n",
    );
    write(&billing.join("mod.lm"), BILLING);
    commit_all(&billing);
    let caller = root.join("web");
    write(
        &caller.join("src/calls.ts"),
        "export function call() {}\nexport function undo() {}\n",
    );
    write(&caller.join("mod.lm"), &web(links));
    commit_all(&caller);
    write(
        &root.join("mod.lm"),
        &format!(
            "module shop\n\nimport {{ Link }} from lemma.links\nimport {{ client }} from web\nimport {{ api }} from billing\n\nworkspace {{\n    repo billing = local(\"billing\")\n    repo web = local(\"web\")\n}}\n{parent}"
        ),
    );
}

#[test]
fn a_client_operation_no_link_starts_from_is_reported() {
    let root = tempfile::tempdir().unwrap();
    system(root.path(), "    client.call -> api.create_charge\n", "");

    let output = verify(root.path());

    let printed = text(&output);
    assert!(output.status.success(), "{printed}");
    assert!(
        printed.contains("warning #web:services.client.undo"),
        "{printed}"
    );
    assert!(
        printed.contains("[client_operation_without_link]"),
        "{printed}"
    );
    assert!(!printed.contains("services.client.call"), "{printed}");
}

#[test]
fn client_operations_that_are_all_linked_are_not_reported() {
    let root = tempfile::tempdir().unwrap();
    let links = "    client.call -> api.create_charge\n    client.undo -> api.refund\n";
    system(root.path(), links, "");

    let output = verify(root.path());

    let printed = text(&output);
    assert!(output.status.success(), "{printed}");
    assert!(
        !printed.contains("client_operation_without_link"),
        "{printed}"
    );
}

#[test]
fn a_link_in_the_parent_module_counts() {
    let root = tempfile::tempdir().unwrap();
    let parent = "\nexport graph links : Link {\n    client.undo -> api.refund\n}\n";
    system(
        root.path(),
        "    client.call -> api.create_charge\n",
        parent,
    );

    let output = verify(root.path());

    let printed = text(&output);
    assert!(output.status.success(), "{printed}");
    assert!(
        !printed.contains("client_operation_without_link"),
        "{printed}"
    );
}
