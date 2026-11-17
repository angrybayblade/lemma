//! Explicit, ignored probes against a user-selected source corpus. No source is executed.
#![allow(
    clippy::unwrap_used,
    reason = "test setup and malformed audit manifests should fail loudly"
)]

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::process::Command;

use lemma_model::{Language, SymbolKind};
use serde::Deserialize;
use serde_json::{Value as Json, json};

#[derive(Deserialize)]
struct Manifest {
    roots: BTreeMap<String, PathBuf>,
    output: PathBuf,
    #[serde(default)]
    cases: Vec<Case>,
    #[serde(default)]
    recheck: Vec<Probe>,
}

#[derive(Deserialize)]
struct Probe {
    repository: String,
    file: String,
    symbol: String,
}

#[derive(Deserialize)]
struct Case {
    name: String,
    root: PathBuf,
    file: String,
    symbol: String,
    contract: Json,
    expected: String,
}

/// Deliberately separate from the public-fixture Z3 lane: a private corpus is
/// opt-in and is never silently reported as tested when its manifest is absent.
#[test]
#[ignore = "requires LEMMA_PROOF_CORPUS_MANIFEST; surveys selected repositories without executing them"]
fn external_corpus_coverage_and_contracts() {
    let path = std::env::var("LEMMA_PROOF_CORPUS_MANIFEST").unwrap();
    let manifest: Manifest = serde_json::from_slice(&std::fs::read(path).unwrap()).unwrap();
    let mut extractor = lemma_syntax::Extractor::new().unwrap();
    let mut rows = Vec::new();
    let mut files = Vec::new();
    for (repository, root) in &manifest.roots {
        survey_repository(repository, root, &mut extractor, &mut files, &mut rows);
    }
    std::fs::create_dir_all(&manifest.output).unwrap();
    let cases: Vec<_> = manifest
        .cases
        .iter()
        .enumerate()
        .map(|(index, case)| run_case(index, case, &manifest.output))
        .collect();
    let failures: Vec<_> = cases
        .iter()
        .filter(|case| case["actual"] != case["expected"])
        .collect();
    let report = json!({
        "scope": "named function/method source-reader coverage; readable does not mean proved",
        "files": files,
        "definitions": rows,
        "cases": cases,
    });
    std::fs::write(
        manifest.output.join("report.json"),
        serde_json::to_vec_pretty(&report).unwrap(),
    )
    .unwrap();
    assert!(
        !report["files"].as_array().unwrap().is_empty(),
        "empty corpus is not coverage"
    );
    assert!(
        failures.is_empty(),
        "unexpected contract results: {failures:#?}"
    );
}

#[test]
#[ignore = "requires LEMMA_PROOF_CORPUS_MANIFEST and native Z3; checks explicit contracts only"]
fn external_contract_cases() {
    let path = std::env::var("LEMMA_PROOF_CORPUS_MANIFEST").unwrap();
    let manifest: Manifest = serde_json::from_slice(&std::fs::read(path).unwrap()).unwrap();
    assert!(
        !manifest.cases.is_empty(),
        "no contract cases were selected"
    );
    std::fs::create_dir_all(&manifest.output).unwrap();
    let cases: Vec<_> = manifest
        .cases
        .iter()
        .enumerate()
        .map(|(index, case)| run_case(index, case, &manifest.output))
        .collect();
    std::fs::write(
        manifest.output.join("cases.json"),
        serde_json::to_vec_pretty(&cases).unwrap(),
    )
    .unwrap();
    let failures: Vec<_> = cases
        .iter()
        .filter(|case| case["actual"] != case["expected"])
        .collect();
    assert!(
        failures.is_empty(),
        "unexpected contract results: {failures:#?}"
    );
}

#[test]
#[ignore = "requires LEMMA_PROOF_CORPUS_MANIFEST; rechecks explicit source-reader candidates"]
fn external_source_readability_recheck() {
    let path = std::env::var("LEMMA_PROOF_CORPUS_MANIFEST").unwrap();
    let manifest: Manifest = serde_json::from_slice(&std::fs::read(path).unwrap()).unwrap();
    assert!(!manifest.recheck.is_empty());
    let mut sources = BTreeMap::new();
    let mut rows = Vec::new();
    for probe in &manifest.recheck {
        let path = manifest.roots[&probe.repository].join(&probe.file);
        let text = sources
            .entry(path.clone())
            .or_insert_with(|| std::fs::read_to_string(path).unwrap());
        let answer = super::source::read(&probe.file, text, &probe.symbol);
        rows.push(json!({"repository": probe.repository, "file": probe.file, "symbol": probe.symbol, "readable": answer.is_ok(), "reason": answer.err().map(|why| why.chars().take(240).collect::<String>())}));
    }
    std::fs::create_dir_all(&manifest.output).unwrap();
    std::fs::write(
        manifest.output.join("recheck.json"),
        serde_json::to_vec_pretty(&rows).unwrap(),
    )
    .unwrap();
}

fn paths(root: &Path) -> Vec<PathBuf> {
    let output = Command::new("git")
        .arg("-C")
        .arg(root)
        .args([
            "ls-files",
            "--cached",
            "--others",
            "--exclude-standard",
            "-z",
            "--",
            "*.ts",
            "*.tsx",
            "*.py",
            "*.go",
        ])
        .output()
        .unwrap();
    assert!(output.status.success());
    let mut paths: Vec<_> = output
        .stdout
        .split(|byte| *byte == 0)
        .filter(|part| !part.is_empty())
        .map(|part| PathBuf::from(String::from_utf8(part.to_vec()).unwrap()))
        .collect();
    paths.sort();
    paths.dedup();
    paths
}

fn survey_repository(
    repository: &str,
    root: &Path,
    extractor: &mut lemma_syntax::Extractor,
    files: &mut Vec<Json>,
    rows: &mut Vec<Json>,
) {
    for path in paths(root) {
        // Explicitly exclude installed/vendored code; generated first-party clients
        // and test files stay in the denominator and can be stratified in the report.
        if path.components().any(|part| {
            matches!(
                part.as_os_str().to_str(),
                Some("node_modules" | "vendor" | ".venv" | "dist")
            )
        }) {
            continue;
        }
        let Ok(text) = std::fs::read_to_string(root.join(&path)) else {
            continue;
        };
        let Some(language) = path
            .extension()
            .and_then(|part| part.to_str())
            .and_then(Language::from_extension)
        else {
            continue;
        };
        let extraction = extractor.extract(language, &text).unwrap();
        files.push(json!({"repository": repository, "file": path, "language": language, "source_hash": lemma_model::content_hash(text.as_bytes()), "extractor_recovered": extraction.has_syntax_errors}));
        for symbol in extraction
            .symbols
            .iter()
            .filter(|symbol| matches!(symbol.kind, SymbolKind::Function | SymbolKind::Method))
        {
            let answer = if symbol.kind == SymbolKind::Method
                || symbol.qualified_name != symbol.name
            {
                Err("entry: method or nested function is outside the current backend".to_owned())
            } else {
                super::source::read(&path.to_string_lossy(), &text, &symbol.name).map(|_| ())
            };
            rows.push(json!({
                "repository": repository, "file": path, "symbol": symbol.qualified_name,
                "line": symbol.name_span.start.line + 1, "language": language,
                "readable": answer.is_ok(), "reason": answer.err().map(|why| why.chars().take(240).collect::<String>()),
            }));
        }
    }
}

fn run_case(index: usize, case: &Case, output: &Path) -> Json {
    let directory = output.join(format!("case-{index:04}"));
    std::fs::create_dir_all(&directory).unwrap();
    let mut handler = json!({"repository": "subject", "implementation": {"file": case.file, "symbol": case.symbol}});
    super::bind_source(&case.root, &mut handler).unwrap();
    let given = json!({"roots": {"subject": case.root}, "arguments": {"handler": handler, "contract": case.contract}});
    let evidence = super::run(
        &given,
        &directory,
        crate::repos::Environment {
            base: None,
            added: &BTreeMap::new(),
        },
        30000,
    );
    let actual = evidence["observed"]["status"]
        .as_str()
        .unwrap_or("unknown")
        .to_owned();
    std::fs::write(
        directory.join("evidence.json"),
        serde_json::to_vec_pretty(&evidence).unwrap(),
    )
    .unwrap();
    json!({"name": case.name, "source": handler, "expected": case.expected, "actual": actual, "evidence": evidence})
}
