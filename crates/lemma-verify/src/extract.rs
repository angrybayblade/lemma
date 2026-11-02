//! Facts for each repository, by its role, in the grammar's relations.
//!
//! Relations the grammar does not declare are skipped, so a grammar can use
//! a subset. A service's facts go to the base and head snapshots; other
//! repositories are read at one commit, so their facts are the same in both.

use std::collections::{BTreeMap, BTreeSet};
use std::path::Path;

use lemma_grammar::CompiledGrammar;
use lemma_http::{CodeFacts, CodeOptions, Operation, PathAliases, SourceFile};
use lemma_logic::Value;
use lemma_syntax::Extractor;

use crate::VerifyError;
use crate::manifest::{Manifest, Repository, Role};
use crate::system::System;

const CODE_EXTENSIONS: [&str; 11] = [
    "ts", "tsx", "mts", "cts", "js", "jsx", "mjs", "cjs", "py", "go", "rs",
];

/// A value for a fact, before interning.
#[derive(Clone, Copy)]
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
    let system = read_system(manifest)?;
    for (name, repository) in &manifest.repositories {
        for snapshot in snapshots(manifest, repository, &system) {
            match repository.role {
                Role::Service => service(name, repository, &snapshot, writer, &mut extractor)?,
                Role::Client => client(name, repository, &snapshot, writer)?,
                Role::Consumer => consumer(name, repository, &snapshot, writer, &mut extractor)?,
            }
        }
    }
    Ok(())
}

/// What the system's services serve, from their specs at every commit they
/// are read at: the markers that make a string in code a path, and the path
/// prefixes each service owns.
fn read_system(manifest: &Manifest) -> Result<System, VerifyError> {
    let unknown = System::default();
    let mut paths: BTreeMap<String, BTreeSet<String>> = BTreeMap::new();
    let mut declared = BTreeMap::new();
    let services = manifest
        .repositories
        .iter()
        .filter(|(_, repository)| repository.role == Role::Service);
    for (name, repository) in services {
        let service = repository.service.clone().unwrap_or_else(|| name.clone());
        let served = paths.entry(service.clone()).or_default();
        for snapshot in snapshots(manifest, repository, &unknown) {
            for spec in &repository.specs {
                served.extend(
                    spec_operations(repository, &snapshot, spec)?
                        .into_iter()
                        .map(|operation| operation.path),
                );
            }
        }
        declared.insert(service, repository.path_prefixes.clone());
    }
    Ok(System::new(&paths, &declared))
}

/// Fails if a spec is missing at every commit its repository is
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
        for file in repository.specs.iter().chain(&repository.generated_from) {
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
/// other entry for the same clone without a commit of its own (one
/// repository can hold a service and its consumers), are read before and
/// with the change; every other repository once, for both snapshots.
fn snapshots<'m>(
    manifest: &'m Manifest,
    repository: &'m Repository,
    system: &'m System,
) -> Vec<Snapshot<'m>> {
    if !is_changed(manifest, repository) {
        return vec![Snapshot {
            commit: repository.commit.as_deref(),
            names: &BOTH,
            other: None,
            system,
        }];
    }
    vec![
        Snapshot {
            commit: Some(&manifest.change.base),
            names: &["base"],
            other: Some(&manifest.change.head),
            system,
        },
        Snapshot {
            commit: Some(&manifest.change.head),
            names: &["head"],
            other: Some(&manifest.change.base),
            system,
        },
    ]
}

/// Whether a repository entry is read at the change's commits: the changed
/// one, and other entries for its clone unless they pin their own commit
/// (to ask how the change meets code from another time).
fn is_changed(manifest: &Manifest, repository: &Repository) -> bool {
    let Some(changed) = manifest.repositories.get(&manifest.change.repository) else {
        return false;
    };
    if std::ptr::eq(changed, repository) {
        return true;
    }
    changed.path == repository.path && repository.commit.is_none()
}

/// A commit to read and the snapshots its facts belong to.
struct Snapshot<'a> {
    commit: Option<&'a str>,
    names: &'a [&'a str],
    /// The change's other commit, when reading the changed repository. A
    /// spec or table that exists at only one of the two commits is read
    /// from that one for both: a file added or deleted by the change, such
    /// as a spec moved elsewhere, does not mean its operations appeared or
    /// disappeared.
    other: Option<&'a str>,
    /// What the system's services serve.
    system: &'a System,
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
        for operation in spec_operations(repository, snapshot, spec)? {
            operation_facts(service_name, &operation, snapshots, writer);
        }
    }
    // Without route directories there is no route code to read; an empty
    // list would otherwise mean the whole repository.
    if !snapshots.contains(&"head") || repository.routes.is_empty() {
        return Ok(());
    }
    let directories: Vec<String> = repository
        .routes
        .iter()
        .chain(&repository.schemas)
        .cloned()
        .collect();
    let files = lemma_http::read_files(&repository.path, commit, &directories, &CODE_EXTENSIONS)?;
    let scope = ServiceScope {
        name,
        service: service_name,
        known: files.iter().map(|file| file.path.clone()).collect(),
        route_aliases: &repository.route_aliases,
        imports: read_import_aliases(repository, commit)?,
    };
    for file in files.iter().filter(|file| !is_test_file(&file.path)) {
        let facts = code_facts(file, &CodeOptions::default(), extractor)?;
        schema_facts(&scope, &file.path, &facts, writer);
        let is_route_file = repository
            .routes
            .iter()
            .any(|directory| file.path.starts_with(directory.as_str()));
        if !is_route_file {
            continue;
        }
        for route in facts.method_paths.iter().filter(|item| item.method != "*") {
            route_facts(&scope, &file.path, route, writer);
        }
    }
    Ok(())
}

/// What every route file of one service shares.
struct ServiceScope<'a> {
    name: &'a str,
    service: &'a str,
    known: BTreeSet<String>,
    route_aliases: &'a BTreeMap<String, String>,
    imports: PathAliases,
}

impl ServiceScope<'_> {
    /// A route's path and the paths its prefixes are aliased to.
    fn route_paths(&self, path: &str) -> Vec<String> {
        let mut paths = vec![path.to_owned()];
        for (from, to) in self.route_aliases {
            if let Some(rest) = path.strip_prefix(from.as_str()) {
                paths.push(format!("{to}{rest}"));
            }
        }
        paths
    }
}

/// Which schemas a route file's routes use, which schemas it defines strict
/// or as unions, and the files it imports, where its schemas may be defined.
fn schema_facts(
    scope: &ServiceScope<'_>,
    path: &str,
    facts: &CodeFacts,
    writer: &mut FactWriter<'_>,
) {
    let file_id = format!("{}:{path}", scope.name);
    for used in &facts.schema_uses {
        for path in scope.route_paths(&used.path) {
            schema_use_facts(scope, &file_id, used, &path, writer);
        }
    }
    for schema in &facts.strict_schemas {
        writer.add(
            "strict_schema",
            &["head"],
            &[Arg::Text(&file_id), Arg::Text(schema)],
        );
    }
    for union in &facts.schema_unions {
        for member in &union.members {
            let args = [
                Arg::Text(&file_id),
                Arg::Text(&union.name),
                Arg::Text(member),
            ];
            writer.add("union_member", &["head"], &args);
        }
    }
    for specifier in &facts.imports {
        if let Some(target) = resolve_import(path, specifier, &scope.known, &scope.imports) {
            let target_id = format!("{}:{target}", scope.name);
            writer.add(
                "imports",
                &["head"],
                &[Arg::Text(&file_id), Arg::Text(&target_id)],
            );
        }
    }
}

/// The schema one route part uses, at one of the route's paths.
fn schema_use_facts(
    scope: &ServiceScope<'_>,
    file_id: &str,
    used: &lemma_http::SchemaUse,
    path: &str,
    writer: &mut FactWriter<'_>,
) {
    let route = [
        Arg::Text(scope.service),
        Arg::Text(&used.method),
        Arg::Text(path),
        Arg::Text(&used.part),
    ];
    if let Some(schema) = &used.schema {
        let mut args = route.to_vec();
        args.extend([Arg::Text(file_id), Arg::Text(schema)]);
        writer.add("route_schema", &["head"], &args);
    } else if used.is_inline_strict {
        writer.add("inline_strict_route", &["head"], &route);
    }
}

/// A route a service serves, at its path and any aliased path.
fn route_facts(
    scope: &ServiceScope<'_>,
    file: &str,
    route: &lemma_http::MethodPath,
    writer: &mut FactWriter<'_>,
) {
    let line = i64::from(route.location.line);
    for path in scope.route_paths(&route.path) {
        let args = [
            Arg::Text(scope.service),
            Arg::Text(&route.method),
            Arg::Text(&path),
            Arg::Text(file),
            Arg::Number(line),
        ];
        writer.add("route", &["head"], &args);
    }
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
) -> Result<(), VerifyError> {
    let both = snapshot.names;
    let default_names = [DEFAULT_CALL_NAME.to_owned()];
    let templates = if repository.call_names.is_empty() {
        &default_names[..]
    } else {
        &repository.call_names[..]
    };
    for spec in &repository.generated_from {
        for operation in spec_operations(repository, snapshot, spec)? {
            let Some(operation_id) = operation.operation_id.as_deref() else {
                continue;
            };
            for template in templates {
                let function = call_name(template, operation_id);
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
    }
    for spec in &repository.specs {
        for operation in spec_operations(repository, snapshot, spec)? {
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
    /// The service this consumer's code belongs to, whose own operations
    /// it does not consume.
    service: Option<&'a str>,
    /// The snapshots this reading of the repository belongs to.
    snapshots: &'a [&'a str],
    known: BTreeSet<String>,
    options: CodeOptions,
    /// What the system's services serve.
    system: &'a System,
    /// Path aliases from the repository's `tsconfig` files.
    imports: PathAliases,
}

const BOTH: [&str; 2] = ["base", "head"];

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
        service: repository.service.as_deref(),
        snapshots: snapshot.names,
        known: files.iter().map(|file| file.path.clone()).collect(),
        options: CodeOptions {
            client_receivers: repository.client_receivers.clone(),
            client_modules: repository.client_modules.clone(),
            path_markers: snapshot.system.markers.clone(),
        },
        system: snapshot.system,
        imports: read_import_aliases(repository, snapshot.commit)?,
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
    let mut scopes = BTreeSet::from([file_id.clone()]);
    write_calls(scope, &file_id, facts, writer, &mut scopes);
    for mention in &facts.mentions {
        let mention_scope = consumer_id(&file_id, mention.function.as_ref());
        writer.add(
            "mentions",
            scope.snapshots,
            &[Arg::Text(&mention_scope), Arg::Text(&mention.name)],
        );
        scopes.insert(mention_scope);
    }
    for function_scope in &scopes {
        writer.add(
            "in_file",
            scope.snapshots,
            &[Arg::Text(function_scope), Arg::Text(&file_id)],
        );
    }
    write_imports(scope, path, &file_id, facts, writer);
    if let Some(service) = scope.service {
        writer.add(
            "part_of_service",
            scope.snapshots,
            &[Arg::Text(&file_id), Arg::Text(service)],
        );
    }
}

/// A consumer's id: the function a fact is in, or the file outside functions.
fn consumer_id(file_id: &str, symbol: Option<&String>) -> String {
    symbol.map_or_else(
        || file_id.to_owned(),
        |symbol| format!("{file_id}::{symbol}"),
    )
}

/// Calls by path and through clients, adding each caller to `scopes`.
fn write_calls(
    scope: &ConsumerScope<'_>,
    file_id: &str,
    facts: &CodeFacts,
    writer: &mut FactWriter<'_>,
    scopes: &mut BTreeSet<String>,
) {
    let calls = facts
        .path_references
        .iter()
        .map(|reference| (&reference.method, &reference.path, &reference.location));
    let descriptors = facts
        .method_paths
        .iter()
        .map(|item| (&item.method, &item.path, &item.location));
    for (method, call_path, location) in calls.chain(descriptors) {
        let consumer = consumer_id(file_id, location.symbol.as_ref());
        let args = [
            Arg::Text(&consumer),
            Arg::Text(method),
            Arg::Text(call_path),
            Arg::Text(file_id),
            Arg::Number(i64::from(location.line)),
        ];
        writer.add("calls_path", scope.snapshots, &args);
        for service in scope.system.addressed_to(call_path) {
            writer.add(
                "addressed_to",
                scope.snapshots,
                &[Arg::Text(call_path), Arg::Text(service)],
            );
        }
        scopes.insert(consumer);
    }
    for call in &facts.client_calls {
        let consumer = consumer_id(file_id, call.location.symbol.as_ref());
        let args = [
            Arg::Text(&consumer),
            Arg::Text(&call.chain),
            Arg::Text(file_id),
            Arg::Number(i64::from(call.location.line)),
        ];
        writer.add("calls_client", scope.snapshots, &args);
        scopes.insert(consumer);
    }
}

/// Path aliases from every `tsconfig*.json` in a repository.
fn read_import_aliases(
    repository: &Repository,
    commit: Option<&str>,
) -> Result<PathAliases, VerifyError> {
    let is_tsconfig = |path: &str| {
        let name = path.rsplit('/').next().unwrap_or(path);
        name.starts_with("tsconfig") && has_extension(name, "json")
    };
    let files = lemma_http::read_files_where(&repository.path, commit, &is_tsconfig)?;
    Ok(PathAliases::from_files(&files))
}

/// The files a file imports, and for Go the other files of its package.
fn write_imports(
    scope: &ConsumerScope<'_>,
    path: &str,
    file_id: &str,
    facts: &CodeFacts,
    writer: &mut FactWriter<'_>,
) {
    let package_files = same_package(path, &scope.known);
    let imported = facts
        .imports
        .iter()
        .filter_map(|specifier| resolve_import(path, specifier, &scope.known, &scope.imports))
        .chain(package_files);
    for target in imported {
        let target_id = format!("{}:{target}", scope.name);
        writer.add(
            "imports",
            scope.snapshots,
            &[Arg::Text(file_id), Arg::Text(&target_id)],
        );
    }
}

fn spec_operations(
    repository: &Repository,
    snapshot: &Snapshot<'_>,
    spec: &str,
) -> Result<Vec<Operation>, VerifyError> {
    let Some(text) = read_at_snapshot(repository, snapshot, spec)? else {
        return Ok(Vec::new());
    };
    let document: serde_json::Value =
        serde_json::from_str(&text).map_err(|source| VerifyError::Spec {
            file: spec.to_owned(),
            source,
        })?;
    Ok(lemma_http::operations(&document))
}

/// A file at the snapshot's commit, or at the change's other commit if the
/// change added or deleted it.
fn read_at_snapshot(
    repository: &Repository,
    snapshot: &Snapshot<'_>,
    file: &str,
) -> Result<Option<String>, VerifyError> {
    if let Some(text) = lemma_http::read_file(&repository.path, snapshot.commit, file)? {
        return Ok(Some(text));
    }
    let Some(other) = snapshot.other else {
        return Ok(None);
    };
    Ok(lemma_http::read_file(&repository.path, Some(other), file)?)
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

/// `GET /v1/users/{}`: how rules name an operation.
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

/// The call name a client is assumed to use when the manifest gives none:
/// the `operationId` as written.
const DEFAULT_CALL_NAME: &str = "{operationId}";

/// A generated client's name for an operation, from a template naming its
/// `operationId` as written (`{operationId}`) or recased: `{camel}`
/// (`listUsers`), `{pascal}` (`ListUsers`) or `{snake}` (`list_users`).
/// Generators differ, so the manifest says which (`call_names`).
fn call_name(template: &str, operation_id: &str) -> String {
    let words = words(operation_id);
    let capitalized: Vec<String> = words.iter().map(|word| capitalize(word)).collect();
    let camel = match capitalized.split_first() {
        Some((first, rest)) => first.to_lowercase() + &rest.concat(),
        None => String::new(),
    };
    template
        .replace("{operationId}", operation_id)
        .replace("{camel}", &camel)
        .replace("{pascal}", &capitalized.concat())
        .replace("{snake}", &words.join("_"))
}

/// An identifier's words, split at `_`, `-`, `.`, spaces and lower-to-upper
/// case changes, in lower case: `get_pollExportJob` is `get poll export job`.
fn words(identifier: &str) -> Vec<String> {
    let mut words = Vec::new();
    let mut current = String::new();
    let mut previous_lower = false;
    for character in identifier.chars() {
        if matches!(character, '_' | '-' | '.' | ' ') {
            words.extend((!current.is_empty()).then(|| std::mem::take(&mut current)));
            previous_lower = false;
            continue;
        }
        if character.is_uppercase() && previous_lower {
            words.push(std::mem::take(&mut current));
        }
        previous_lower = character.is_lowercase() || character.is_ascii_digit();
        current.extend(character.to_lowercase());
    }
    words.extend((!current.is_empty()).then_some(current));
    words
}

fn capitalize(word: &str) -> String {
    let mut characters = word.chars();
    characters
        .next()
        .map(|first| first.to_uppercase().chain(characters).collect())
        .unwrap_or_default()
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
fn resolve_import(
    file: &str,
    specifier: &str,
    known: &BTreeSet<String>,
    aliases: &PathAliases,
) -> Option<String> {
    if has_extension(file, "rs") {
        return rust_module(file, specifier, known);
    }
    if !specifier.starts_with('.') {
        return aliases
            .candidates(file, specifier)
            .iter()
            .find_map(|base| module_file(base, known));
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
    module_file(&base, known)
}

/// The file a module path names, trying source extensions and index files.
fn module_file(base: &str, known: &BTreeSet<String>) -> Option<String> {
    let candidates = [
        base.to_owned(),
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
            Arg::Text("billing"),
            Arg::Text("GET /api/a"),
            Arg::Text("GET"),
            Arg::Text("/api/a"),
        ];
        let gone = [
            Arg::Text("billing"),
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
                Arg::Text("billing"),
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
        writer.add("in_file", &BOTH, &[Arg::Text("d:f::c"), Arg::Text("d:f")]);

        let errors = derive(&grammar, "requirement:consumer_sends_rejected_field");
        let warnings = derive(&grammar, "requirement:consumer_uses_removed_request_field");

        assert_eq!(errors.len(), 1);
        assert_eq!(errors[0][1], "PATCH /api/strict");
        assert_eq!(warnings.len(), 1);
        assert_eq!(warnings[0][1], "PATCH /api/lenient");
    }

    #[test]
    fn strict_schemas_reach_routes_through_imports_and_unions() {
        let mut grammar = http_contracts();
        let mut writer = FactWriter::new(&mut grammar);
        let route = |path, part, schema| {
            [
                Arg::Text("billing"),
                Arg::Text("POST"),
                Arg::Text(path),
                Arg::Text(part),
                Arg::Text("p:routes.ts"),
                Arg::Text(schema),
            ]
        };
        writer.add(
            "route_schema",
            &["head"],
            &route("/api/a", "body", "Imported"),
        );
        writer.add(
            "route_schema",
            &["head"],
            &route("/api/b", "body", "Either"),
        );
        writer.add("route_schema", &["head"], &route("/api/c", "body", "Mixed"));
        writer.add(
            "imports",
            &["head"],
            &[Arg::Text("p:routes.ts"), Arg::Text("p:_schemas.ts")],
        );
        writer.add(
            "strict_schema",
            &["head"],
            &[Arg::Text("p:_schemas.ts"), Arg::Text("Imported")],
        );
        for (union, member) in [
            ("Either", "Imported"),
            ("Mixed", "Imported"),
            ("Mixed", "*"),
        ] {
            writer.add(
                "union_member",
                &["head"],
                &[
                    Arg::Text("p:routes.ts"),
                    Arg::Text(union),
                    Arg::Text(member),
                ],
            );
        }

        let strict = derive(&grammar, "strict_route");

        let paths: Vec<&str> = strict.iter().map(|row| row[2].as_str()).collect();
        assert_eq!(paths, ["/api/a", "/api/b"]);
    }

    #[test]
    fn a_name_in_another_function_of_the_file_is_not_a_use() {
        let mut grammar = http_contracts();
        let mut writer = FactWriter::new(&mut grammar);
        for (scope, name) in [
            ("d:f::caller", "own"),
            ("d:f::other", "elsewhere"),
            ("d:f", "shape"),
        ] {
            writer.add("mentions", &BOTH, &[Arg::Text(scope), Arg::Text(name)]);
            writer.add("in_file", &BOTH, &[Arg::Text(scope), Arg::Text("d:f")]);
        }

        let uses = derive(&grammar, "uses");
        let anywhere = derive(&grammar, "names_anywhere");

        let caller_uses: Vec<&str> = uses
            .iter()
            .filter(|row| row[0] == "d:f::caller")
            .map(|row| row[1].as_str())
            .collect();
        assert_eq!(caller_uses, ["own", "shape"]);
        let in_file = anywhere.iter().filter(|row| row[0] == "d:f").count();
        assert_eq!(in_file, 3);
    }

    #[test]
    fn a_service_does_not_call_its_own_operations() {
        let mut grammar = http_contracts();
        let mut writer = FactWriter::new(&mut grammar);
        for (service, path) in [("billing", "/v1/invoices"), ("users", "/v1/users")] {
            let op = format!("GET {path}");
            writer.add(
                "operation",
                &BOTH,
                &[
                    Arg::Text(service),
                    Arg::Text(&op),
                    Arg::Text("GET"),
                    Arg::Text(path),
                ],
            );
            let call = [
                Arg::Text("a:route.ts"),
                Arg::Text("GET"),
                Arg::Text(path),
                Arg::Text("a:route.ts"),
                Arg::Number(1),
            ];
            writer.add("calls_path", &BOTH, &call);
        }
        writer.add(
            "part_of_service",
            &BOTH,
            &[Arg::Text("a:route.ts"), Arg::Text("billing")],
        );

        let calls = derive(&grammar, "calls");

        let operations: Vec<&str> = calls.iter().map(|row| row[1].as_str()).collect();
        assert_eq!(operations, ["GET /v1/users"]);
    }

    #[test]
    fn only_new_calls_under_a_service_prefix_are_unserved() {
        let mut grammar = http_contracts();
        let mut writer = FactWriter::new(&mut grammar);
        let operation = [
            Arg::Text("billing"),
            Arg::Text("GET /v1/billing/a"),
            Arg::Text("GET"),
            Arg::Text("/v1/billing/a"),
        ];
        writer.add("operation", &BOTH, &operation);
        let call = |path| {
            [
                Arg::Text("d:f::c"),
                Arg::Text("GET"),
                Arg::Text(path),
                Arg::Text("d:f"),
                Arg::Number(3),
            ]
        };
        writer.add("calls_path", &BOTH, &call("/v1/billing/old"));
        for path in ["/v1/billing/old", "/v1/billing/new", "/v1/other/x"] {
            writer.add("calls_path", &["head"], &call(path));
        }
        for path in ["/v1/billing/old", "/v1/billing/new"] {
            writer.add(
                "addressed_to",
                &BOTH,
                &[Arg::Text(path), Arg::Text("billing")],
            );
        }

        let unserved = derive(&grammar, "unserved_call");

        assert_eq!(unserved, [["d:f::c", "GET", "/v1/billing/new", "d:f", "3"]]);
    }

    fn known(paths: &[&str]) -> BTreeSet<String> {
        paths.iter().map(|path| (*path).to_owned()).collect()
    }

    #[test]
    fn typescript_imports_resolve_with_extensions_and_index_files() {
        let files = known(&["src/types/configs.ts", "src/models/index.ts"]);

        assert_eq!(
            resolve_import(
                "src/models/a.ts",
                "../types/configs.js",
                &files,
                &PathAliases::default()
            )
            .as_deref(),
            Some("src/types/configs.ts")
        );
        assert_eq!(
            resolve_import("src/b.ts", "./models", &files, &PathAliases::default()).as_deref(),
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
            resolve_import(
                "svc/src/api/client.rs",
                "crate::models::Tool",
                &files,
                &PathAliases::default()
            )
            .as_deref(),
            Some("svc/src/models.rs")
        );
        assert_eq!(
            resolve_import(
                "svc/src/api/client.rs",
                "super::types",
                &files,
                &PathAliases::default()
            )
            .as_deref(),
            Some("svc/src/api/types.rs")
        );
        assert_eq!(
            resolve_import(
                "svc/src/api/mod.rs",
                "self::types",
                &files,
                &PathAliases::default()
            )
            .as_deref(),
            Some("svc/src/api/types.rs")
        );
    }

    #[test]
    fn path_aliases_resolve_through_tsconfig_paths() {
        let files = known(&[
            "apps/web/src/server/trpc.ts",
            "apps/web/src/server/index.ts",
        ]);
        let aliases = PathAliases::from_files(&[SourceFile {
            path: "apps/web/tsconfig.json".to_owned(),
            contents: r#"{ "compilerOptions": { "paths": { "~/*": ["./src/*"] } } }"#.to_owned(),
        }]);

        assert_eq!(
            resolve_import(
                "apps/web/src/app/page.tsx",
                "~/server/trpc",
                &files,
                &aliases
            )
            .as_deref(),
            Some("apps/web/src/server/trpc.ts")
        );
        assert_eq!(
            resolve_import("apps/web/src/app/page.tsx", "~/server", &files, &aliases).as_deref(),
            Some("apps/web/src/server/index.ts")
        );
        assert_eq!(
            resolve_import("apps/web/src/a.ts", "react", &files, &aliases),
            None
        );
    }

    #[test]
    fn call_names_follow_the_template_a_generator_uses() {
        assert_eq!(
            call_name("{camel}", "get_pollExportJob"),
            "getPollExportJob"
        );
        assert_eq!(
            call_name("{pascal}.Invoke", "list-users"),
            "ListUsers.Invoke"
        );
        assert_eq!(call_name("{snake}", "ListUsersByOrg"), "list_users_by_org");
        assert_eq!(call_name("{operationId}", "users.list"), "users.list");
    }

    #[test]
    fn python_relative_imports_resolve_to_modules_and_packages() {
        let files = known(&["pkg/core/models.py", "pkg/types/__init__.py"]);

        assert_eq!(
            resolve_import("pkg/core/a.py", ".models", &files, &PathAliases::default()).as_deref(),
            Some("pkg/core/models.py")
        );
        assert_eq!(
            resolve_import("pkg/core/a.py", "..types", &files, &PathAliases::default()).as_deref(),
            Some("pkg/types/__init__.py")
        );
    }

    #[test]
    fn field_leaves_drop_containers_and_array_marks() {
        assert_eq!(leaf("hooks[].handler.action"), "action");
        assert_eq!(leaf("items[]"), "items");
    }
}
