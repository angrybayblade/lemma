//! `lemma index` and the queries after it, reading a description in `.lm`,
//! through the real binary.

#![allow(clippy::unwrap_used, reason = "test setup should fail loudly")]

use std::path::Path;
use std::process::{Command, Output};

use lemma_model::{Export, ExportLink};
use lemma_store::Store;

const SPEC: &str = r#"{ "openapi": "3.0.0", "info": { "title": "B", "version": "1" }, "paths": { "/v1/charges": { "post": { "operationId": "createCharge", "responses": { "200": { "description": "ok" } } } } } }"#;

const MODULE: &str = "module billing\n\nimport * as http from lemma.http\n\nexport service api {\n    index: { include: [\"src\"] }\n    operations: http.openapi(\"openapi.json\")\n}\n";

fn write(path: &Path, text: &str) {
    std::fs::create_dir_all(path.parent().unwrap()).unwrap();
    std::fs::write(path, text).unwrap();
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

/// A repository with code the description includes and code it leaves out.
fn code(root: &Path) {
    write(&root.join("openapi.json"), SPEC);
    write(
        &root.join("src/charges.ts"),
        "export function createCharge() {}\n",
    );
    write(&root.join("scripts/seed.ts"), "export function seed() {}\n");
}

/// What the index in `root` holds: its files, exports and links.
fn indexed(root: &Path) -> (Vec<String>, Vec<Export>, Vec<ExportLink>) {
    let store = Store::open(&root.join(".lemma/index.ls")).unwrap();
    let snapshots = store.snapshots().unwrap();
    let id = snapshots[0].id;
    let mut files: Vec<String> = store
        .file_keys(id)
        .unwrap()
        .into_iter()
        .map(|file| file.path)
        .collect();
    files.sort();
    (
        files,
        store.exports(id).unwrap(),
        store.export_links(id).unwrap(),
    )
}

#[test]
fn a_root_module_selects_the_code_indexed_without_a_warning() {
    let scratch = tempfile::tempdir().unwrap();
    code(scratch.path());
    write(&scratch.path().join("mod.lm"), MODULE);

    let output = lemma(scratch.path(), &["index"]);

    assert!(output.status.success(), "{}", text(&output));
    assert!(
        !text(&output).contains("no mod.lm here"),
        "{}",
        text(&output)
    );
    let (files, _, _) = indexed(scratch.path());
    assert_eq!(files, ["src/charges.ts"]);
}

#[test]
fn a_directory_without_a_description_is_indexed_whole_and_says_so() {
    let scratch = tempfile::tempdir().unwrap();
    code(scratch.path());

    let output = lemma(scratch.path(), &["index"]);

    assert!(output.status.success(), "{}", text(&output));
    assert!(
        text(&output).contains("no mod.lm here"),
        "{}",
        text(&output)
    );
    let (files, _, _) = indexed(scratch.path());
    assert!(files.contains(&"scripts/seed.ts".to_owned()), "{files:?}");
}

/// A service that is only an API document, as in the quickstart.
const DOCUMENT_ONLY: &str = "module billing\n\nimport * as http from lemma.http\n\nexport service api {\n    operations: http.openapi(\"openapi.json\")\n}\n";

#[test]
fn a_service_that_is_only_a_document_indexes_every_file_and_keeps_its_exports() {
    let scratch = tempfile::tempdir().unwrap();
    code(scratch.path());
    write(&scratch.path().join("mod.lm"), DOCUMENT_ONLY);

    let output = lemma(scratch.path(), &["index"]);

    assert!(output.status.success(), "{}", text(&output));
    assert!(
        text(&output).contains("add `index.include` to api"),
        "{}",
        text(&output)
    );
    let (files, exports, _) = indexed(scratch.path());
    assert!(files.contains(&"scripts/seed.ts".to_owned()), "{files:?}");
    assert_ne!(exports, []);
}

#[test]
fn a_workspace_says_which_member_was_indexed_whole() {
    let scratch = tempfile::tempdir().unwrap();
    let billing = scratch.path().join("billing");
    code(&billing);
    write(&billing.join("mod.lm"), DOCUMENT_ONLY);
    write(
        &scratch.path().join("mod.lm"),
        "module shop\n\nworkspace {\n    repo billing = local(\"billing\")\n}\n",
    );
    for directory in [scratch.path(), billing.as_path()] {
        git_init(directory);
    }

    let output = lemma(scratch.path(), &["index"]);

    assert!(output.status.success(), "{}", text(&output));
    assert!(
        text(&output).contains("1 of 1 repositories indexed"),
        "{}",
        text(&output)
    );
    assert!(
        text(&output).contains("add `index.include` to api"),
        "{}",
        text(&output)
    );
    assert_ne!(indexed(&billing).1, []);
}

#[test]
fn a_workspace_module_indexes_each_repository_it_composes() {
    let scratch = tempfile::tempdir().unwrap();
    let billing = scratch.path().join("billing");
    code(&billing);
    write(&billing.join("mod.lm"), MODULE);
    write(
        &scratch.path().join("mod.lm"),
        "module shop\n\nworkspace {\n    repo billing = local(\"billing\")\n}\n",
    );
    for directory in [scratch.path(), billing.as_path()] {
        git_init(directory);
    }

    let output = lemma(scratch.path(), &["index"]);

    assert!(output.status.success(), "{}", text(&output));
    let (files, exports, _) = indexed(&billing);
    assert_eq!(files, ["src/charges.ts"]);
    assert_ne!(exports, []);
    let store = Store::open(&scratch.path().join(".lemma/index.ls")).unwrap();
    assert_eq!(store.workspace_members().unwrap().len(), 1);
}

fn git_init(directory: &Path) {
    let status = Command::new("git")
        .args(["init", "-q"])
        .current_dir(directory)
        .status()
        .unwrap();
    assert!(status.success());
}

#[test]
fn an_edit_to_an_imported_module_rebuilds_the_exports() {
    let scratch = tempfile::tempdir().unwrap();
    code(scratch.path());
    write(
        &scratch.path().join("openapi2.json"),
        &SPEC
            .replace("createCharge", "refundCharge")
            .replace("/v1/charges", "/v1/refunds"),
    );
    write(
        &scratch.path().join("documents.lm"),
        "module billing.documents\n\nexport let main = \"openapi.json\"\n",
    );
    write(
        &scratch.path().join("mod.lm"),
        "module billing\n\nimport * as http from lemma.http\nimport { main } from billing.documents\n\nexport service api {\n    index: { include: [\"src\"] }\n    operations: http.openapi(main)\n}\n",
    );
    let first = lemma(scratch.path(), &["index"]);
    assert!(first.status.success(), "{}", text(&first));
    let names = |root: &Path| -> Vec<String> {
        indexed(root)
            .1
            .into_iter()
            .map(|export| export.name)
            .collect()
    };
    assert_eq!(names(scratch.path()), ["createCharge"]);

    write(
        &scratch.path().join("documents.lm"),
        "module billing.documents\n\nexport let main = \"openapi2.json\"\n",
    );
    let second = lemma(scratch.path(), &["index"]);

    assert!(second.status.success(), "{}", text(&second));
    assert_eq!(names(scratch.path()), ["refundCharge"]);
}

#[test]
fn a_query_during_a_type_error_answers_from_the_last_index_and_says_so() {
    let scratch = tempfile::tempdir().unwrap();
    code(scratch.path());
    write(&scratch.path().join("mod.lm"), MODULE);
    let first = lemma(scratch.path(), &["index"]);
    assert!(first.status.success(), "{}", text(&first));
    write(
        &scratch.path().join("mod.lm"),
        &MODULE.replace("http.openapi", "http.no_such_function"),
    );

    let output = lemma(scratch.path(), &["symbols", "createCharge"]);

    assert!(output.status.success(), "{}", text(&output));
    let printed = text(&output);
    assert!(printed.contains("could not be refreshed"), "{printed}");
    assert!(printed.contains("src/charges.ts"), "{printed}");
}

#[test]
fn a_type_error_keeps_the_last_exports() {
    let scratch = tempfile::tempdir().unwrap();
    code(scratch.path());
    write(&scratch.path().join("mod.lm"), MODULE);
    let first = lemma(scratch.path(), &["index"]);
    assert!(first.status.success(), "{}", text(&first));
    let before = indexed(scratch.path());
    write(
        &scratch.path().join("mod.lm"),
        &MODULE.replace("http.openapi", "http.no_such_function"),
    );

    let output = lemma(scratch.path(), &["index"]);

    assert!(!output.status.success(), "{}", text(&output));
    assert!(text(&output).contains("mod.lm"), "{}", text(&output));
    assert_eq!(indexed(scratch.path()), before);
}

/// A workspace of one repository, indexed, whose description then stops
/// type-checking; the workspace's directory.
fn workspace_with_a_type_error(root: &Path) {
    let billing = root.join("billing");
    code(&billing);
    write(&billing.join("mod.lm"), MODULE);
    write(
        &root.join("mod.lm"),
        "module shop\n\nworkspace {\n    repo billing = local(\"billing\")\n}\n",
    );
    for directory in [root, billing.as_path()] {
        git_init(directory);
    }
    let first = lemma(root, &["index"]);
    assert!(first.status.success(), "{}", text(&first));
    write(
        &billing.join("mod.lm"),
        &MODULE.replace("http.openapi", "http.no_such_function"),
    );
}

// The two graph tests hold with or without a language server on the machine:
// without one the command stops earlier, printing neither thing they forbid.
#[test]
fn a_graph_during_a_type_error_does_not_fail_on_the_workspace_trace() {
    let scratch = tempfile::tempdir().unwrap();
    workspace_with_a_type_error(scratch.path());

    let output = lemma(
        scratch.path(),
        &["graph", "createCharge", "--direction", "callers"],
    );

    assert!(
        !String::from_utf8_lossy(&output.stderr).contains("has errors"),
        "{}",
        text(&output)
    );
}

#[test]
fn a_drawing_stays_free_of_the_refresh_warning() {
    let scratch = tempfile::tempdir().unwrap();
    workspace_with_a_type_error(scratch.path());

    let output = lemma(
        scratch.path(),
        &[
            "graph",
            "createCharge",
            "--direction",
            "callers",
            "--format",
            "dot",
        ],
    );

    assert!(
        !String::from_utf8_lossy(&output.stdout).contains("could not be refreshed"),
        "{}",
        text(&output)
    );
}

#[test]
fn a_description_without_services_indexes_every_file_and_says_so() {
    let scratch = tempfile::tempdir().unwrap();
    code(scratch.path());
    write(&scratch.path().join("mod.lm"), "module billing\n");

    let output = lemma(scratch.path(), &["index"]);

    assert!(output.status.success(), "{}", text(&output));
    assert!(
        text(&output).contains("defines no services"),
        "{}",
        text(&output)
    );
    assert!(
        indexed(scratch.path())
            .0
            .contains(&"scripts/seed.ts".to_owned())
    );
}

#[test]
fn a_workspaces_json_carries_each_members_warnings() {
    let scratch = tempfile::tempdir().unwrap();
    let billing = scratch.path().join("billing");
    code(&billing);
    write(&billing.join("mod.lm"), DOCUMENT_ONLY);
    write(
        &scratch.path().join("mod.lm"),
        "module shop\n\nworkspace {\n    repo billing = local(\"billing\")\n}\n",
    );
    for directory in [scratch.path(), billing.as_path()] {
        git_init(directory);
    }

    let output = lemma(scratch.path(), &["--json", "index"]);

    let report: serde_json::Value = serde_json::from_slice(&output.stdout).unwrap();
    let warning = report["members"][0]["warnings"][0].as_str().unwrap();
    assert!(warning.contains("add `index.include` to api"), "{warning}");
}
