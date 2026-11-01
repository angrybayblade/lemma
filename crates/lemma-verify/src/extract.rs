//! Facts for each repository, by its role, in the grammar's relations.
//!
//! Relations the grammar does not declare are skipped, so a grammar can use
//! a subset. A service's facts go to the base and head snapshots; other
//! repositories are read at one commit, so their facts are the same in both.

use std::collections::{BTreeMap, BTreeSet};
use std::path::Path;

use lemma_grammar::CompiledGrammar;
use lemma_http::{CodeFacts, CodeOptions, Operation, SourceFile};
use lemma_logic::Value;
use lemma_syntax::Extractor;

use crate::VerifyError;
use crate::manifest::{Manifest, Repository, Role};

const CODE_EXTENSIONS: [&str; 11] = [
    "ts", "tsx", "mts", "cts", "js", "jsx", "mjs", "cjs", "py", "go", "rs",
];

/// A value for a fact, before interning.
enum Arg<'a> {
    Text(&'a str),
    Number(i64),
}

/// Writes facts into the grammar's relations, interning text as it goes.
pub struct FactWriter<'g> {
    grammar: &'g mut CompiledGrammar,
    /// Facts written, by relation, for the summary.
    pub counts: BTreeMap<String, usize>,
}

impl<'g> FactWriter<'g> {
    pub fn new(grammar: &'g mut CompiledGrammar) -> Self {
        Self {
            grammar,
            counts: BTreeMap::new(),
        }
    }

    fn add(&mut self, relation: &str, snapshots: &[&str], args: &[Arg<'_>]) {
        for snapshot in snapshots {
            let Some(name) = self.grammar.input_relation(relation, snapshot) else {
                continue;
            };
            let row: Vec<Value> = args
                .iter()
                .map(|arg| match arg {
                    Arg::Text(text) => self.grammar.symbols.intern(text),
                    Arg::Number(number) => Value::Number(*number),
                })
                .collect();
            if self.grammar.facts.insert(&name, row) {
                *self.counts.entry(relation.to_owned()).or_default() += 1;
            }
        }
    }
}

/// Extracts every repository's facts.
pub fn extract_all(manifest: &Manifest, writer: &mut FactWriter<'_>) -> Result<(), VerifyError> {
    let mut extractor = Extractor::new().map_err(|source| VerifyError::Syntax { source })?;
    check_files(manifest)?;
    for (name, repository) in &manifest.repositories {
        for snapshot in snapshots(manifest, repository) {
            match repository.role {
                Role::Service => service(name, repository, &snapshot, writer, &mut extractor)?,
                Role::Client => client(name, repository, &snapshot, writer, &mut extractor)?,
                Role::Consumer => consumer(name, repository, &snapshot, writer, &mut extractor)?,
            }
        }
    }
    Ok(())
}

/// Fails if a spec or table is missing at every commit its repository is
/// read at, which means the manifest names it wrongly. Missing at one of
/// the changed repository's two commits is fine: the change added or
/// removed it.
fn check_files(manifest: &Manifest) -> Result<(), VerifyError> {
    for (name, repository) in &manifest.repositories {
        let commits: Vec<Option<&str>> = if is_changed(manifest, repository) {
            vec![Some(&manifest.change.base), Some(&manifest.change.head)]
        } else {
            vec![repository.commit.as_deref()]
        };
        for file in repository.specs.iter().chain(&repository.tables) {
            if !exists_at_any(&repository.path, &commits, file)? {
                return Err(VerifyError::MissingFile {
                    repository: name.clone(),
                    file: file.clone(),
                    commits: commits
                        .iter()
                        .map(|commit| commit.unwrap_or("the working directory"))
                        .collect::<Vec<_>>()
                        .join(" or "),
                });
            }
        }
    }
    Ok(())
}

fn exists_at_any(
    repository: &Path,
    commits: &[Option<&str>],
    file: &str,
) -> Result<bool, VerifyError> {
    for commit in commits {
        if lemma_http::read_file(repository, *commit, file)?.is_some() {
            return Ok(true);
        }
    }
    Ok(false)
}

/// The commits to read a repository at. The changed repository, and any
/// other entry for the same clone (one repository can hold a service and
/// its consumers), are read before and with the change; every other
/// repository once, for both snapshots.
fn snapshots<'m>(manifest: &'m Manifest, repository: &'m Repository) -> Vec<Snapshot<'m>> {
    if !is_changed(manifest, repository) {
        return vec![Snapshot {
            commit: repository.commit.as_deref(),
            names: &BOTH,
        }];
    }
    vec![
        Snapshot {
            commit: Some(&manifest.change.base),
            names: &["base"],
        },
        Snapshot {
            commit: Some(&manifest.change.head),
            names: &["head"],
        },
    ]
}

fn is_changed(manifest: &Manifest, repository: &Repository) -> bool {
    manifest
        .repositories
        .get(&manifest.change.repository)
        .is_some_and(|changed| changed.path == repository.path)
}

/// A commit to read and the snapshots its facts belong to.
struct Snapshot<'a> {
    commit: Option<&'a str>,
    names: &'a [&'a str],
}

fn service(
    name: &str,
    repository: &Repository,
    snapshot: &Snapshot<'_>,
    writer: &mut FactWriter<'_>,
    extractor: &mut Extractor,
) -> Result<(), VerifyError> {
    let (commit, snapshots) = (snapshot.commit, snapshot.names);
    let service_name = repository.service.as_deref().unwrap_or(name);
    if repository.rejects_unknown_fields {
        writer.add(
            "rejects_unknown_fields",
            snapshots,
            &[Arg::Text(service_name)],
        );
    }
    for spec in &repository.specs {
        for operation in spec_operations(repository, commit, spec)? {
            operation_facts(service_name, &operation, snapshots, writer);
        }
    }
    // Without route directories there is no route code to read; an empty
    // list would otherwise mean the whole repository.
    if !snapshots.contains(&"head") || repository.routes.is_empty() {
        return Ok(());
    }
    let files = lemma_http::read_files(
        &repository.path,
        commit,
        &repository.routes,
        &CODE_EXTENSIONS,
    )?;
    for file in files.iter().filter(|file| !is_test_file(&file.path)) {
        let facts = code_facts(file, &CodeOptions::default(), extractor)?;
        for route in facts.method_paths.iter().filter(|item| item.method != "*") {
            let line = i64::from(route.location.line);
            let args = [
                Arg::Text(service_name),
                Arg::Text(&route.method),
                Arg::Text(&route.path),
                Arg::Text(&file.path),
                Arg::Number(line),
            ];
            writer.add("route", &["head"], &args);
        }
    }
    Ok(())
}

fn operation_facts(
    service: &str,
    operation: &Operation,
    snapshots: &[&str],
    writer: &mut FactWriter<'_>,
) {
    let key = operation_key(&operation.method, &operation.path);
    let op = key.as_str();
    writer.add(
        "operation",
        snapshots,
        &[
            Arg::Text(service),
            Arg::Text(op),
            Arg::Text(&operation.method),
            Arg::Text(&operation.path),
        ],
    );
    add_path_family(&operation.path, snapshots, writer);
    if operation.rejects_unknown_fields {
        writer.add("strict_request", snapshots, &[Arg::Text(op)]);
    }
    for parameter in &operation.parameters {
        let args = [
            Arg::Text(op),
            Arg::Text(&parameter.name),
            Arg::Text(&parameter.location),
        ];
        writer.add("parameter", snapshots, &args);
        if parameter.required {
            writer.add("required_parameter", snapshots, &args);
        }
    }
    for field in &operation.request_fields {
        let args = [
            Arg::Text(op),
            Arg::Text(&field.path),
            Arg::Text(leaf(&field.path)),
        ];
        writer.add("request_field", snapshots, &args);
        if field.required {
            writer.add("required_request_field", snapshots, &args);
        }
    }
    for field in &operation.response_fields {
        writer.add(
            "response_field",
            snapshots,
            &[Arg::Text(op), Arg::Text(field), Arg::Text(leaf(field))],
        );
    }
}

fn client(
    name: &str,
    repository: &Repository,
    snapshot: &Snapshot<'_>,
    writer: &mut FactWriter<'_>,
    extractor: &mut Extractor,
) -> Result<(), VerifyError> {
    let (commit, both) = (snapshot.commit, snapshot.names);
    for spec in &repository.generated_from {
        for operation in spec_operations(repository, commit, spec)? {
            let Some(function) = operation.operation_id.as_deref().map(camel_case) else {
                continue;
            };
            let args = [
                Arg::Text(name),
                Arg::Text(&function),
                Arg::Text(&operation.method),
                Arg::Text(&operation.path),
                Arg::Text(spec),
                Arg::Number(0),
            ];
            writer.add("client_operation", both, &args);
        }
    }
    for table in &repository.tables {
        let Some(source) = lemma_http::read_file(&repository.path, commit, table)? else {
            continue;
        };
        let file = SourceFile {
            path: table.clone(),
            contents: source,
        };
        let facts = code_facts(&file, &CodeOptions::default(), extractor)?;
        for item in &facts.method_paths {
            let Some(chain) = &item.chain else { continue };
            let line = i64::from(item.location.line);
            let args = [
                Arg::Text(name),
                Arg::Text(chain),
                Arg::Text(&item.method),
                Arg::Text(&item.path),
                Arg::Text(&file.path),
                Arg::Number(line),
            ];
            writer.add("client_operation", both, &args);
        }
    }
    for spec in &repository.specs {
        for operation in spec_operations(repository, commit, spec)? {
            for parameter in &operation.parameters {
                let args = [
                    Arg::Text(name),
                    Arg::Text(&operation.method),
                    Arg::Text(&operation.path),
                    Arg::Text(&parameter.name),
                    Arg::Text(&parameter.location),
                ];
                writer.add("client_parameter", both, &args);
            }
        }
    }
    Ok(())
}

/// What every file of one consumer shares.
struct ConsumerScope<'a> {
    name: &'a str,
    /// The snapshots this reading of the repository belongs to.
    snapshots: &'a [&'a str],
    known: BTreeSet<String>,
    options: CodeOptions,
}

const BOTH: [&str; 2] = ["base", "head"];

/// Records a path's family: its first two segments, such as `/api/v3.1`.
/// Rules use it to tell a call to a missing operation of a service from a
/// call to some other service, such as a consumer's own `/api/trpc`.
fn add_path_family(path: &str, snapshots: &[&str], writer: &mut FactWriter<'_>) {
    let family: Vec<&str> = path
        .split('/')
        .filter(|segment| !segment.is_empty())
        .take(2)
        .collect();
    let family = format!("/{}", family.join("/"));
    writer.add(
        "path_family",
        snapshots,
        &[Arg::Text(path), Arg::Text(&family)],
    );
}

fn consumer(
    name: &str,
    repository: &Repository,
    snapshot: &Snapshot<'_>,
    writer: &mut FactWriter<'_>,
    extractor: &mut Extractor,
) -> Result<(), VerifyError> {
    let files = lemma_http::read_files(
        &repository.path,
        snapshot.commit,
        &repository.include,
        &CODE_EXTENSIONS,
    )?;
    let scope = ConsumerScope {
        name,
        snapshots: snapshot.names,
        known: files.iter().map(|file| file.path.clone()).collect(),
        options: CodeOptions {
            client_receivers: repository.client_receivers.clone(),
            client_modules: repository.client_modules.clone(),
        },
    };
    for file in files.iter().filter(|file| !is_test_file(&file.path)) {
        let facts = code_facts(file, &scope.options, extractor)?;
        consumer_file(&scope, &file.path, &facts, writer);
    }
    Ok(())
}

fn consumer_file(
    scope: &ConsumerScope<'_>,
    path: &str,
    facts: &CodeFacts,
    writer: &mut FactWriter<'_>,
) {
    let file_id = format!("{}:{path}", scope.name);
    let consumer_id = |symbol: Option<&String>| {
        symbol.map_or_else(|| file_id.clone(), |symbol| format!("{file_id}::{symbol}"))
    };
    let calls = facts
        .path_references
        .iter()
        .map(|reference| (&reference.method, &reference.path, &reference.location));
    let descriptors = facts
        .method_paths
        .iter()
        .filter(|item| item.chain.is_none())
        .map(|item| (&item.method, &item.path, &item.location));
    for (method, call_path, location) in calls.chain(descriptors) {
        let consumer = consumer_id(location.symbol.as_ref());
        let args = [
            Arg::Text(&consumer),
            Arg::Text(method),
            Arg::Text(call_path),
            Arg::Text(&file_id),
            Arg::Number(i64::from(location.line)),
        ];
        writer.add("calls_path", scope.snapshots, &args);
        add_path_family(call_path, scope.snapshots, writer);
    }
    for call in &facts.client_calls {
        let consumer = consumer_id(call.location.symbol.as_ref());
        let args = [
            Arg::Text(&consumer),
            Arg::Text(&call.chain),
            Arg::Text(&file_id),
            Arg::Number(i64::from(call.location.line)),
        ];
        writer.add("calls_client", scope.snapshots, &args);
    }
    for mention in &facts.mentions {
        writer.add(
            "mentions",
            scope.snapshots,
            &[Arg::Text(&file_id), Arg::Text(mention)],
        );
    }
    let package_files = same_package(path, &scope.known);
    let imported = facts
        .imports
        .iter()
        .filter_map(|specifier| resolve_import(path, specifier, &scope.known))
        .chain(package_files);
    for target in imported {
        let target_id = format!("{}:{target}", scope.name);
        writer.add(
            "imports",
            scope.snapshots,
            &[Arg::Text(&file_id), Arg::Text(&target_id)],
        );
    }
}

fn spec_operations(
    repository: &Repository,
    commit: Option<&str>,
    spec: &str,
) -> Result<Vec<Operation>, VerifyError> {
    let Some(text) = lemma_http::read_file(&repository.path, commit, spec)? else {
        return Ok(Vec::new());
    };
    let document: serde_json::Value =
        serde_json::from_str(&text).map_err(|source| VerifyError::Spec {
            file: spec.to_owned(),
            source,
        })?;
    Ok(lemma_http::operations(&document))
}

fn code_facts(
    file: &SourceFile,
    options: &CodeOptions,
    extractor: &mut Extractor,
) -> Result<CodeFacts, VerifyError> {
    Ok(lemma_http::extract(
        &file.path,
        &file.contents,
        options,
        extractor,
    )?)
}

/// `GET /api/v3.1/session_configs/{}`: how rules name an operation.
pub fn operation_key(method: &str, path: &str) -> String {
    format!("{method} {path}")
}

/// The last name in a field path: `hooks[].handler.action` → `action`.
fn leaf(field: &str) -> &str {
    field
        .rsplit('.')
        .next()
        .unwrap_or(field)
        .trim_end_matches("[]")
}

/// The function a client generated from a spec names an operation with:
/// its `operationId` in camel case, as `openapi-ts` and most TypeScript
/// generators write it (`get_pollRevokeJob` is `getPollRevokeJob`).
fn camel_case(operation_id: &str) -> String {
    let mut name = String::new();
    for (index, word) in operation_id
        .split(['_', '-', ' ', '.'])
        .filter(|word| !word.is_empty())
        .enumerate()
    {
        let mut characters = word.chars();
        let Some(first) = characters.next() else {
            continue;
        };
        if index == 0 {
            name.extend(first.to_lowercase());
        } else {
            name.extend(first.to_uppercase());
        }
        name.push_str(characters.as_str());
    }
    name
}

fn is_test_file(path: &str) -> bool {
    let lower = path.to_ascii_lowercase();
    lower.ends_with("_test.go")
        || lower.ends_with("_test.py")
        || lower.contains("/test/")
        || lower.contains("/tests/")
        || lower.contains("__tests__")
        || lower.contains(".test.")
        || lower.contains(".spec.")
        || lower.contains("/test_")
}

fn has_extension(path: &str, extension: &str) -> bool {
    Path::new(path)
        .extension()
        .is_some_and(|found| found.eq_ignore_ascii_case(extension))
}

/// For a Go file, the other files of its package: Go code uses names from
/// its whole package without importing them.
fn same_package(path: &str, known: &BTreeSet<String>) -> Vec<String> {
    if !has_extension(path, "go") {
        return Vec::new();
    }
    let directory = Path::new(path).parent().unwrap_or(Path::new(""));
    known
        .iter()
        .filter(|other| {
            other.as_str() != path
                && has_extension(other, "go")
                && !is_test_file(other)
                && Path::new(other.as_str()).parent() == Some(directory)
        })
        .cloned()
        .collect()
}

/// The repository file a relative import names, if it is one of `known`.
fn resolve_import(file: &str, specifier: &str, known: &BTreeSet<String>) -> Option<String> {
    if has_extension(file, "rs") {
        return rust_module(file, specifier, known);
    }
    let directory = Path::new(file).parent().unwrap_or(Path::new(""));
    let is_python = Path::new(file)
        .extension()
        .is_some_and(|extension| extension.eq_ignore_ascii_case("py"));
    let base = if is_python {
        python_module(directory, specifier)?
    } else {
        join(directory, specifier.trim_end_matches(".js"))?
    };
    let candidates = [
        base.clone(),
        format!("{base}.ts"),
        format!("{base}.tsx"),
        format!("{base}.js"),
        format!("{base}.jsx"),
        format!("{base}/index.ts"),
        format!("{base}/index.tsx"),
        format!("{base}/index.js"),
        format!("{base}.py"),
        format!("{base}/__init__.py"),
    ];
    candidates
        .into_iter()
        .find(|candidate| known.contains(candidate))
}

/// The file of the module a Rust `use` path names: `crate::models::Tool`
/// is `src/models.rs` or `src/models/mod.rs` (the last segments may be
/// items, so shorter paths are tried too).
fn rust_module(file: &str, specifier: &str, known: &BTreeSet<String>) -> Option<String> {
    let path = Path::new(file);
    let directory = path.parent().unwrap_or(Path::new(""));
    let is_module_root = matches!(
        path.file_name().and_then(|name| name.to_str()),
        Some("mod.rs" | "lib.rs" | "main.rs")
    );
    let mut segments = specifier.split("::");
    let base = match segments.next()? {
        "crate" => {
            let source_root = file.rfind("src/").map_or("", |index| &file[..index + 4]);
            Path::new(source_root).to_path_buf()
        }
        "self" if is_module_root => directory.to_path_buf(),
        "self" => directory.join(path.file_stem()?),
        "super" if is_module_root => directory.parent()?.to_path_buf(),
        "super" => directory.to_path_buf(),
        _ => return None,
    };
    let segments: Vec<&str> = segments.collect();
    for length in (1..=segments.len()).rev() {
        let module = segments[..length]
            .iter()
            .fold(base.clone(), |module, segment| module.join(segment));
        let module = module.to_string_lossy();
        let candidates = [format!("{module}.rs"), format!("{module}/mod.rs")];
        if let Some(found) = candidates
            .into_iter()
            .find(|candidate| known.contains(candidate))
        {
            return Some(found);
        }
    }
    None
}

/// `.models` or `..core.models` relative to a Python package directory.
fn python_module(directory: &Path, specifier: &str) -> Option<String> {
    let dots = specifier.chars().take_while(|c| *c == '.').count();
    let mut base = directory.to_path_buf();
    for _ in 1..dots {
        base = base.parent()?.to_path_buf();
    }
    let module = specifier[dots..].replace('.', "/");
    let joined = if module.is_empty() {
        base
    } else {
        base.join(module)
    };
    Some(joined.to_string_lossy().replace('\\', "/"))
}

/// Joins a relative specifier to a directory, resolving `.` and `..`.
fn join(directory: &Path, specifier: &str) -> Option<String> {
    let mut parts: Vec<String> = directory
        .iter()
        .map(|part| part.to_string_lossy().into_owned())
        .collect();
    for part in specifier.split('/') {
        match part {
            "." | "" => {}
            ".." => {
                parts.pop()?;
            }
            name => parts.push(name.to_owned()),
        }
    }
    Some(parts.join("/"))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn http_contracts() -> CompiledGrammar {
        let grammar = lemma_grammar::parse_grammar(crate::HTTP_CONTRACTS).unwrap();
        lemma_grammar::compile(&grammar).unwrap()
    }

    fn derive(grammar: &CompiledGrammar, relation: &str) -> Vec<Vec<String>> {
        let request = lemma_logic::Request::default();
        let derived = lemma_logic::evaluate(
            &lemma_logic::ReferenceEngine,
            &grammar.program,
            &grammar.facts,
            &request,
        )
        .unwrap();
        derived
            .rows(relation)
            .map(|row| {
                row.iter()
                    .map(|value| grammar.symbols.display(*value))
                    .collect()
            })
            .collect()
    }

    #[test]
    fn a_removed_operation_does_not_also_report_its_fields() {
        let mut grammar = http_contracts();
        let mut writer = FactWriter::new(&mut grammar);
        let kept = [
            Arg::Text("gateway"),
            Arg::Text("GET /api/a"),
            Arg::Text("GET"),
            Arg::Text("/api/a"),
        ];
        let gone = [
            Arg::Text("gateway"),
            Arg::Text("POST /api/b"),
            Arg::Text("POST"),
            Arg::Text("/api/b"),
        ];
        writer.add("operation", &["base", "head"], &kept);
        writer.add("operation", &["base"], &gone);
        for op in ["GET /api/a", "POST /api/b"] {
            let field = [Arg::Text(op), Arg::Text("name"), Arg::Text("name")];
            writer.add("request_field", &["base"], &field);
        }

        let removed = derive(&grammar, "removed_request_field");

        assert_eq!(removed, [["GET /api/a", "name", "name"]]);
        assert_eq!(derive(&grammar, "removed_operation"), [["POST /api/b"]]);
    }

    #[test]
    fn sending_a_removed_field_is_an_error_only_where_unknown_fields_fail() {
        let mut grammar = http_contracts();
        let mut writer = FactWriter::new(&mut grammar);
        for op in ["PATCH /api/strict", "PATCH /api/lenient"] {
            let path = op.split(' ').nth(1).unwrap();
            let operation = [
                Arg::Text("gateway"),
                Arg::Text(op),
                Arg::Text("PATCH"),
                Arg::Text(path),
            ];
            writer.add("operation", &BOTH, &operation);
            let field = [Arg::Text(op), Arg::Text("old"), Arg::Text("old")];
            writer.add("request_field", &["base"], &field);
            let call = [
                Arg::Text("d:f::c"),
                Arg::Text("PATCH"),
                Arg::Text(path),
                Arg::Text("d:f"),
                Arg::Number(1),
            ];
            writer.add("calls_path", &BOTH, &call);
        }
        writer.add(
            "strict_request",
            &["head"],
            &[Arg::Text("PATCH /api/strict")],
        );
        writer.add("mentions", &BOTH, &[Arg::Text("d:f"), Arg::Text("old")]);

        let errors = derive(&grammar, "requirement:consumer_sends_rejected_field");
        let warnings = derive(&grammar, "requirement:consumer_uses_removed_request_field");

        assert_eq!(errors.len(), 1);
        assert_eq!(errors[0][1], "PATCH /api/strict");
        assert_eq!(warnings.len(), 1);
        assert_eq!(warnings[0][1], "PATCH /api/lenient");
    }

    #[test]
    fn only_new_calls_to_a_served_family_are_unserved() {
        let mut grammar = http_contracts();
        let mut writer = FactWriter::new(&mut grammar);
        let operation = [
            Arg::Text("gateway"),
            Arg::Text("GET /api/v3.1/a"),
            Arg::Text("GET"),
            Arg::Text("/api/v3.1/a"),
        ];
        writer.add("operation", &BOTH, &operation);
        add_path_family("/api/v3.1/a", &BOTH, &mut writer);
        let call = |path| {
            [
                Arg::Text("d:f::c"),
                Arg::Text("GET"),
                Arg::Text(path),
                Arg::Text("d:f"),
                Arg::Number(3),
            ]
        };
        writer.add("calls_path", &BOTH, &call("/api/v3.1/old"));
        for path in ["/api/v3.1/old", "/api/v3.1/new", "/api/trpc/x"] {
            writer.add("calls_path", &["head"], &call(path));
            add_path_family(path, &BOTH, &mut writer);
        }

        let unserved = derive(&grammar, "unserved_call");

        assert_eq!(unserved, [["d:f::c", "GET", "/api/v3.1/new", "d:f", "3"]]);
    }

    fn known(paths: &[&str]) -> BTreeSet<String> {
        paths.iter().map(|path| (*path).to_owned()).collect()
    }

    #[test]
    fn typescript_imports_resolve_with_extensions_and_index_files() {
        let files = known(&["src/types/configs.ts", "src/models/index.ts"]);

        assert_eq!(
            resolve_import("src/models/a.ts", "../types/configs.js", &files).as_deref(),
            Some("src/types/configs.ts")
        );
        assert_eq!(
            resolve_import("src/b.ts", "./models", &files).as_deref(),
            Some("src/models/index.ts")
        );
    }

    #[test]
    fn rust_use_paths_resolve_to_module_files() {
        let files = known(&[
            "svc/src/models.rs",
            "svc/src/api/mod.rs",
            "svc/src/api/types.rs",
        ]);

        assert_eq!(
            resolve_import("svc/src/api/client.rs", "crate::models::Tool", &files).as_deref(),
            Some("svc/src/models.rs")
        );
        assert_eq!(
            resolve_import("svc/src/api/client.rs", "super::types", &files).as_deref(),
            Some("svc/src/api/types.rs")
        );
        assert_eq!(
            resolve_import("svc/src/api/mod.rs", "self::types", &files).as_deref(),
            Some("svc/src/api/types.rs")
        );
    }

    #[test]
    fn camel_case_follows_generated_client_names() {
        assert_eq!(camel_case("get_pollRevokeJob"), "getPollRevokeJob");
        assert_eq!(camel_case("ListCategories"), "listCategories");
        assert_eq!(camel_case("list-toolkits-post"), "listToolkitsPost");
    }

    #[test]
    fn python_relative_imports_resolve_to_modules_and_packages() {
        let files = known(&["pkg/core/models.py", "pkg/types/__init__.py"]);

        assert_eq!(
            resolve_import("pkg/core/a.py", ".models", &files).as_deref(),
            Some("pkg/core/models.py")
        );
        assert_eq!(
            resolve_import("pkg/core/a.py", "..types", &files).as_deref(),
            Some("pkg/types/__init__.py")
        );
    }

    #[test]
    fn field_leaves_drop_containers_and_array_marks() {
        assert_eq!(leaf("hooks[].handler.action"), "action");
        assert_eq!(leaf("items[]"), "items");
    }
}
