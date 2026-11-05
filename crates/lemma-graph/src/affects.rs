//! Attaching to each problem what it affects further up (see `impact`):
//! where each trace starts, and reading the code and routes it walks.

use std::collections::{BTreeMap, BTreeSet};
use std::sync::Arc;

use lemma_http::ListedFile;
use lemma_model::{Language, SymbolKind};
use lemma_syntax::ExtractorPool;

use crate::GraphError;
use crate::address::Link;
use crate::check::{Finding, HEAD, Impact, Repo, Severity};
use crate::impact::{self, Affected, Call, Function, Route, Seed};

/// Code files the call graph is read from.
pub(crate) const CODE_EXTENSIONS: [&str; 11] = [
    "ts", "tsx", "mts", "cts", "js", "jsx", "mjs", "cjs", "py", "go", "rs",
];

/// A route as inference read it, before its line is read.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RouteSite {
    /// The repository.
    pub repository: String,
    /// The service's name in findings.
    pub service: String,
    /// `METHOD /path`.
    pub operation: String,
    /// The declaring file.
    pub file: String,
    /// The declaring line, counting from 1.
    pub line: u32,
    /// Whether the line passes the path to a helper that registers it,
    /// rather than declaring the route.
    pub passed_to_helper: bool,
}

/// The inference relations tracing reads.
pub const TRACED_RELATIONS: [&str; 5] = [
    "calls",
    "route",
    "references_operation",
    "operation",
    "part_of_service",
];

/// What inference found that tracing walks: calls and routes, and where
/// its own findings' traces start.
#[derive(Debug, Default)]
pub struct Walkable {
    /// Calls from code to documented operations.
    pub calls: Vec<Call>,
    /// Routes services declare.
    pub routes: Vec<RouteSite>,
    /// Trace starts for inferred findings, by their index in the findings.
    pub seeds: Vec<(usize, Seed)>,
}

/// Reads inference's `calls` and `route` rows, naming repositories by
/// `owners` (entry name to repository).
pub fn walkable(
    relations: &BTreeMap<String, Vec<Vec<String>>>,
    owners: &BTreeMap<String, String>,
) -> Walkable {
    let rows = |name: &str| relations.get(name).into_iter().flatten();
    let calls = rows("calls")
        .filter_map(|row| call(row, owners))
        .collect::<BTreeSet<_>>();
    let mut routes: Vec<RouteSite> = rows("route")
        .filter_map(|row| route_site(row, owners))
        .collect();
    routes.extend(helper_sites(relations, &routes, owners));
    Walkable {
        calls: calls
            .into_iter()
            .map(|(repository, file, function, operation)| Call {
                repository,
                file,
                function,
                operation,
            })
            .collect(),
        routes,
        seeds: Vec::new(),
    }
}

/// A `calls(c, op, f, l)` row: `f` is `entry:path`, `c` is `f::Function`
/// or `f`.
fn call(
    row: &[String],
    owners: &BTreeMap<String, String>,
) -> Option<(String, String, Option<String>, String)> {
    let [scope, operation, file_id, ..] = row else {
        return None;
    };
    let (repository, file) = split_file_id(file_id, owners)?;
    let function = scope
        .strip_prefix(file_id.as_str())
        .and_then(|rest| rest.strip_prefix("::"))
        .map(str::to_owned);
    Some((repository, file, function, operation.clone()))
}

/// Where documented operations without a declared route are registered by
/// a helper: their service's own code writes their path, passed to the
/// helper (`references_operation` rows in files `part_of_service`).
fn helper_sites(
    relations: &BTreeMap<String, Vec<Vec<String>>>,
    routes: &[RouteSite],
    owners: &BTreeMap<String, String>,
) -> Vec<RouteSite> {
    let rows = |name: &str| relations.get(name).into_iter().flatten();
    let declared: BTreeSet<(&str, &str)> = routes
        .iter()
        .map(|route| (route.service.as_str(), route.operation.as_str()))
        .collect();
    let mut own_files: BTreeMap<&str, BTreeSet<&str>> = BTreeMap::new();
    for row in rows("part_of_service") {
        if let [file_id, service] = &row[..] {
            own_files.entry(service).or_default().insert(file_id);
        }
    }
    let mut sites = BTreeSet::new();
    for row in rows("operation") {
        let [service, operation, ..] = &row[..] else {
            continue;
        };
        if declared.contains(&(service.as_str(), operation.as_str())) {
            continue;
        }
        let Some(files) = own_files.get(service.as_str()) else {
            continue;
        };
        let referencing = rows("references_operation").filter(|reference| {
            reference.get(1) == Some(operation)
                && reference
                    .get(2)
                    .is_some_and(|file| files.contains(file.as_str()))
        });
        for reference in referencing {
            let (Some(file_id), Some(line)) = (reference.get(2), reference.get(3)) else {
                continue;
            };
            let Some((repository, file)) = split_file_id(file_id, owners) else {
                continue;
            };
            let Ok(line) = line.parse() else {
                continue;
            };
            sites.insert((repository, service.clone(), operation.clone(), file, line));
        }
    }
    sites
        .into_iter()
        .map(|(repository, service, operation, file, line)| RouteSite {
            repository,
            service,
            operation,
            file,
            line,
            passed_to_helper: true,
        })
        .collect()
}

/// A `route(s, m, p, f, l)` row.
fn route_site(row: &[String], owners: &BTreeMap<String, String>) -> Option<RouteSite> {
    let [service, method, path, file, line] = row else {
        return None;
    };
    Some(RouteSite {
        repository: owners.get(service)?.clone(),
        service: service.clone(),
        operation: lemma_verify::operation_key(method, path),
        file: file.clone(),
        line: line.parse().ok()?,
        passed_to_helper: false,
    })
}

/// `entry:path` → (the entry's repository, path). Entry names can contain
/// `:`, so the longest entry the id starts with wins.
fn split_file_id(file_id: &str, owners: &BTreeMap<String, String>) -> Option<(String, String)> {
    owners
        .iter()
        .filter_map(|(entry, repository)| {
            let path = file_id.strip_prefix(entry.as_str())?.strip_prefix(':')?;
            Some((entry.len(), repository, path))
        })
        .max_by_key(|(length, _, _)| *length)
        .map(|(_, repository, path)| (repository.clone(), path.to_owned()))
}

/// Where an inferred finding's trace starts: the calling function.
pub fn inferred_seed(
    values: &BTreeMap<String, String>,
    owners: &BTreeMap<String, String>,
) -> Option<Seed> {
    let file_id = values.get("f")?;
    let scope = values.get("c")?;
    let (repository, _) = split_file_id(file_id, owners)?;
    let function = scope.strip_prefix(file_id.as_str())?.strip_prefix("::")?;
    let name = function.rsplit('.').next().unwrap_or(function);
    Some(Seed {
        repository,
        names: vec![name.to_owned()],
    })
}

/// Where a declared link's trace starts: its caller's code anchor. For a
/// method (`Charges.Create`) that is its type, since callers may reach it
/// through other methods or fields of that type.
pub fn declared_seed(link: &Link, repos: &BTreeMap<String, Repo>) -> Option<Seed> {
    let caller = &link.caller;
    let repository = caller.repository.clone()?;
    let operation = repos.get(&repository)?.files[HEAD]
        .services
        .get(&caller.service)?
        .exports
        .operations
        .get(&caller.operation)?;
    let symbol = operation.path.rsplit_once(':')?.1.replace("()", "");
    let name = symbol.split('.').next().unwrap_or(&symbol).to_owned();
    Some(Seed {
        repository,
        names: vec![name],
    })
}

/// Adds to each problem among `findings` that has a seed what it affects.
pub fn attach(
    findings: &mut [Finding],
    seeds: &[(usize, Seed)],
    walkable: &Walkable,
    repos: &BTreeMap<String, Repo>,
    reading: &CodeReading<'_>,
) -> Result<(), GraphError> {
    let problems: Vec<&(usize, Seed)> = seeds
        .iter()
        .filter(|(index, _)| findings[*index].severity != Severity::Info)
        .collect();
    if problems.is_empty() {
        return Ok(());
    }
    let routes = read_routes(&walkable.routes, repos)?;
    let mut code = Code::new(repos, reading);
    let mut traces: BTreeMap<&Seed, Vec<Impact>> = BTreeMap::new();
    let mut tracing = impact::Tracing::default();
    for (index, seed) in problems {
        if !traces.contains_key(seed) {
            let mut functions = |repository: &str| code.functions(repository);
            let start = impact::Start::Names(seed.clone());
            let (affected, links) = impact::trace_links(
                &mut tracing,
                &start,
                &mut functions,
                &routes,
                &walkable.calls,
            );
            traces.insert(seed, describe(&affected, &links));
        }
        findings[*index].affects.clone_from(&traces[seed]);
    }
    code.into_failure().map_or(Ok(()), Err)
}

/// How tracing reads code: the facts kept between runs, if any, and the
/// repositories' listings, shared with verification.
pub struct CodeReading<'a> {
    /// Where functions are kept between runs.
    pub facts: Option<&'a lemma_verify::FactsCache>,
    /// Repositories' files, listed once per run.
    pub listings: &'a lemma_http::Listings,
}

/// Repositories' functions, read once each. A repository that cannot be
/// read gives none, and the first such error is kept to report.
pub(crate) struct Code<'a> {
    repos: &'a BTreeMap<String, Repo>,
    /// The build of Lemma and the units each file's functions are kept in
    /// between runs, when they are kept.
    kept: Option<(String, lemma_store::UnitCache)>,
    /// Repositories' files, listed once per run with verification.
    listings: &'a lemma_http::Listings,
    extractors: ExtractorPool,
    functions: BTreeMap<String, Vec<Function>>,
    failure: Option<GraphError>,
}

impl<'a> Code<'a> {
    pub(crate) fn new(repos: &'a BTreeMap<String, Repo>, reading: &CodeReading<'a>) -> Self {
        // A cache that cannot be opened only means every file is parsed.
        let kept = reading.facts.and_then(|cache| {
            std::fs::create_dir_all(&cache.directory).ok()?;
            let units = lemma_store::UnitCache::open(&cache.directory.join("units.db")).ok()?;
            Some((cache.build.clone(), units))
        });
        Self {
            repos,
            kept,
            listings: reading.listings,
            extractors: ExtractorPool::new(),
            functions: BTreeMap::new(),
            failure: None,
        }
    }

    /// The first repository that could not be read, if one could not.
    pub(crate) fn into_failure(self) -> Option<GraphError> {
        self.failure
    }

    pub(crate) fn functions(&mut self, repository: &str) -> Vec<Function> {
        if let Some(functions) = self.functions.get(repository) {
            return functions.clone();
        }
        let read = match self.repos.get(repository) {
            Some(repo) => functions_of(repo, &mut self.kept, self.listings, &self.extractors),
            None => Ok(Vec::new()),
        };
        let functions = match read {
            Ok(functions) => functions,
            Err(error) => {
                self.failure.get_or_insert(error);
                Vec::new()
            }
        };
        self.functions
            .insert(repository.to_owned(), functions.clone());
        functions
    }
}

fn describe(affected: &[(usize, Affected)], links: &[impact::Link]) -> Vec<Impact> {
    affected
        .iter()
        .map(|(distance, item)| match item {
            Affected::Operation { service, operation } => {
                let reached_by = links
                    .iter()
                    .find(|link| link.user == impact::Node::Affected(item.clone()))
                    .map(|link| &link.used);
                Impact {
                    distance: *distance,
                    owner: service.clone(),
                    at: operation.clone(),
                    calls: None,
                    via: reached_by.map(node_label),
                    path: reached_by.map_or_else(Vec::new, |node| calls_below(node, links)),
                }
            }
            Affected::Caller {
                repository,
                location,
                operation,
            } => Impact {
                distance: *distance,
                owner: repository.clone(),
                at: location.clone(),
                calls: Some(operation.clone()),
                via: None,
                path: Vec::new(),
            },
        })
        .collect()
}

/// A node as a reader names it: `file:Function`, or what was affected.
fn node_label(node: &impact::Node) -> String {
    match node {
        impact::Node::Function { file, name, .. } => format!("{file}:{name}"),
        impact::Node::Affected(Affected::Caller { location, .. }) => location.clone(),
        impact::Node::Affected(Affected::Operation { operation, .. }) => operation.clone(),
    }
}

/// The functions `node` calls on the way down to the problem, nearest
/// first, while the links go from function to function.
fn calls_below(node: &impact::Node, links: &[impact::Link]) -> Vec<String> {
    let mut path = Vec::new();
    let mut current = node;
    while let Some(link) = links
        .iter()
        .find(|link| &link.user == current && matches!(link.used, impact::Node::Function { .. }))
    {
        if path.len() > 64 {
            break;
        }
        path.push(node_label(&link.used));
        current = &link.used;
    }
    path
}

/// Routes with the identifiers on their declaring lines, read at head.
pub(crate) fn read_routes(
    sites: &[RouteSite],
    repos: &BTreeMap<String, Repo>,
) -> Result<Vec<Route>, GraphError> {
    let sources = route_sources(sites, repos)?;
    let mut routes = Vec::new();
    for site in sites {
        let key = (site.repository.clone(), site.file.clone());
        // A route whose file is gone (deleted but not committed) is kept,
        // with no text, as one whose handler cannot be read.
        let (source, lines): (Arc<str>, &[(usize, usize)]) = match sources.get(&key) {
            Some((source, lines)) => (Arc::clone(source), lines),
            None => (Arc::from(""), &[]),
        };
        let index = site.line.saturating_sub(1) as usize;
        let line_text = lines
            .get(index)
            .map_or("", |&(start, end)| &source[start..end]);
        routes.push(Route {
            repository: site.repository.clone(),
            operation: site.operation.clone(),
            service: site.service.clone(),
            file: site.file.clone(),
            line: site.line,
            handler_names: crate::handlers::handler_names(line_text),
            template: path_literal(line_text),
            inline_mentions: inline_handler(&source, lines, index),
            passed_to_helper: site.passed_to_helper,
            source,
        });
    }
    Ok(routes)
}

/// Each file routes are declared in, by repository and path, with the byte
/// spans of its lines: read in one batch per repository, and split into
/// lines once rather than once per route.
type RouteSources = BTreeMap<(String, String), (Arc<str>, Vec<(usize, usize)>)>;

fn route_sources(
    sites: &[RouteSite],
    repos: &BTreeMap<String, Repo>,
) -> Result<RouteSources, GraphError> {
    let mut wanted: BTreeMap<&str, BTreeSet<String>> = BTreeMap::new();
    for site in sites {
        wanted
            .entry(site.repository.as_str())
            .or_default()
            .insert(site.file.clone());
    }
    let mut sources = BTreeMap::new();
    for (repository, files) in wanted {
        let Some(repo) = repos.get(repository) else {
            continue;
        };
        let checkout = &repo.checkouts[HEAD];
        let paths: Vec<String> = files.into_iter().collect();
        let read = match &checkout.commit {
            Some(commit) => lemma_http::read_paths(&checkout.root, commit, &paths),
            None => lemma_http::read_from_disk(&checkout.root, &paths),
        }
        .map_err(|source| GraphError::Source { source })?;
        for file in read {
            let lines = line_spans(&file.contents);
            let key = (repository.to_owned(), file.path);
            sources.insert(key, (Arc::from(file.contents), lines));
        }
    }
    Ok(sources)
}

/// The byte span of each line of `text`, without its line ending.
fn line_spans(text: &str) -> Vec<(usize, usize)> {
    let mut spans = Vec::new();
    let mut start = 0;
    for line in text.split_inclusive('\n') {
        let content = line.trim_end_matches('\n').trim_end_matches('\r');
        spans.push((start, start + content.len()));
        start += line.len();
    }
    spans
}

/// The path literal a route's line writes: the longest string starting
/// with `/`, or an empty one (a router mounted at its prefix).
fn path_literal(line: &str) -> Option<String> {
    let mut best: Option<&str> = None;
    for quote in ['"', '\'', '`'] {
        let mut rest = 0;
        while let Some(open) = line[rest..].find(quote).map(|at| rest + at) {
            let Some(close) = line[open + 1..].find(quote).map(|at| open + 1 + at) else {
                break;
            };
            let literal = &line[open + 1..close];
            let is_path = literal.is_empty() || literal.starts_with('/');
            if is_path && best.is_none_or(|found| literal.len() > found.len()) {
                best = Some(literal);
            }
            rest = close + 1;
        }
    }
    best.map(str::to_owned)
}

/// The identifiers an inline handler's code mentions, when the route on
/// line `index` (from 0) is registered with one: an anonymous function
/// after its path (`func(w, r) {`, `(c) => {`, `function (req) {`,
/// `lambda`). Its code runs from that line to where the registration's
/// parentheses close.
fn inline_handler(
    source: &str,
    spans: &[(usize, usize)],
    index: usize,
) -> Option<BTreeSet<String>> {
    // The registration's statement: its line and up to 400 after it.
    let statement_lines: Vec<&str> = spans
        .get(index..)?
        .iter()
        .take(400)
        .map(|&(start, end)| &source[start..end])
        .collect();
    let line = statement_lines.first()?;
    let path_end = ['"', '\'', '`']
        .iter()
        .filter_map(|quote| line.rfind(*quote))
        .max()
        .map_or(0, |at| at + 1);
    let after = &line[path_end..];
    let is_inline = ["func(", "func (", "=>", "function(", "function (", "lambda"]
        .iter()
        .any(|marker| after.contains(marker));
    if !is_inline {
        return None;
    }
    let mut depth = 0i64;
    let mut statement = Vec::new();
    for text in statement_lines {
        statement.push(text);
        depth += bracket_change(text);
        if depth <= 0 {
            break;
        }
    }
    Some(impact::identifiers(&statement.join("\n")))
}

/// A repository's functions and methods at head, from the directories its
/// services' code, routes and operations are in. A file is read only when
/// its functions are not kept under its blob, so an unchanged repository
/// is listed but not read.
pub(crate) fn functions_of(
    repo: &Repo,
    kept: &mut Option<(String, lemma_store::UnitCache)>,
    listings: &lemma_http::Listings,
    extractors: &ExtractorPool,
) -> Result<Vec<Function>, GraphError> {
    let Some(prefixes) = code_directories(repo) else {
        return Ok(Vec::new());
    };
    let checkout = &repo.checkouts[HEAD];
    let (root, commit) = (checkout.root.as_path(), checkout.commit.as_deref());
    let source_error = |source| GraphError::Source { source };
    if commit.is_none() {
        lemma_http::require_prefixes(root, &prefixes).map_err(source_error)?;
    }
    let listed = listings.get(root, commit).map_err(source_error)?;
    let code: Vec<&ListedFile> = listed
        .iter()
        .filter(|file| is_traced_code(&file.path, &prefixes))
        .collect();
    // A file Git has no blob for is named by its contents, so those are
    // read first.
    let unvouched: Vec<String> = code
        .iter()
        .filter(|file| file.blob.is_none())
        .map(|file| file.path.clone())
        .collect();
    let mut contents = read_by_path(root, commit, &unvouched)?;
    let build = kept.as_ref().map_or("", |(build, _)| build.as_str());
    let keyed: Vec<(&ListedFile, String)> = code
        .into_iter()
        .filter_map(|file| {
            let named = lemma_http::contents_key(file, contents.get(&file.path))?;
            Some((file, functions_key(build, &file.path, &named)))
        })
        .collect();
    let keys: Vec<String> = keyed.iter().map(|(_, key)| key.clone()).collect();
    let mut by_key = kept_functions(kept.as_ref(), &keys);
    let to_read: Vec<String> = keyed
        .iter()
        .filter(|(file, key)| !by_key.contains_key(key) && !contents.contains_key(&file.path))
        .map(|(file, _)| file.path.clone())
        .collect();
    contents.extend(read_by_path(root, commit, &to_read)?);
    let to_parse: Vec<(&lemma_http::SourceFile, &String)> = keyed
        .iter()
        .filter(|(_, key)| !by_key.contains_key(key))
        .filter_map(|(file, key)| Some((contents.get(&file.path)?, key)))
        .collect();
    by_key.extend(parse_functions(&to_parse, kept, extractors)?);
    Ok(keyed
        .iter()
        .filter_map(|(_, key)| by_key.remove(key))
        .flatten()
        .collect())
}

/// Whether tracing reads the file at `path`: code under one of `prefixes`
/// that is not a test.
fn is_traced_code(path: &str, prefixes: &[String]) -> bool {
    let extension = path.rsplit_once('.').map_or("", |(_, extension)| extension);
    CODE_EXTENSIONS.contains(&extension)
        && lemma_http::is_under_any(path, prefixes)
        && !lemma_http::is_test_file(path)
}

/// The files at `paths` at `commit` or in the working directory, by path.
fn read_by_path(
    root: &std::path::Path,
    commit: Option<&str>,
    paths: &[String],
) -> Result<BTreeMap<String, lemma_http::SourceFile>, GraphError> {
    let read = lemma_http::read_listed(root, commit, paths)
        .map_err(|source| GraphError::Source { source })?;
    Ok(read
        .into_iter()
        .map(|file| (file.path.clone(), file))
        .collect())
}

/// The functions `kept` holds under `keys`, decoded in parallel. A cache
/// that cannot be read, or an entry that does not decode, only means
/// parsing.
fn kept_functions(
    kept: Option<&(String, lemma_store::UnitCache)>,
    keys: &[String],
) -> BTreeMap<String, Vec<Function>> {
    let Some((_, units)) = kept else {
        return BTreeMap::new();
    };
    units
        .get_decoded(keys, |bytes| serde_json::from_slice(bytes).ok())
        .unwrap_or_default()
}

/// Reads `files`' functions in parallel, by key, and keeps them in `kept`.
/// The results keep the files' order, so the first failure is the one
/// reported.
fn parse_functions(
    files: &[(&lemma_http::SourceFile, &String)],
    kept: &mut Option<(String, lemma_store::UnitCache)>,
    extractors: &ExtractorPool,
) -> Result<Vec<(String, Vec<Function>)>, GraphError> {
    use rayon::prelude::*;
    let is_keeping = kept.is_some();
    let per_file: Vec<Result<ReadFunctions, GraphError>> = files
        .par_iter()
        .with_min_len(8)
        .map(|(file, _)| {
            let functions = read_functions(file, extractors)?;
            let encoded = is_keeping
                .then(|| serde_json::to_vec(&functions).ok())
                .flatten();
            Ok((functions, encoded))
        })
        .collect();
    let mut parsed = Vec::new();
    let mut new_units = Vec::new();
    for ((_, key), result) in files.iter().zip(per_file) {
        let (functions, encoded) = result?;
        if let Some(encoded) = encoded {
            new_units.push(((*key).clone(), encoded));
        }
        parsed.push(((*key).clone(), functions));
    }
    if let Some((_, units)) = kept.as_mut() {
        units.put_later(new_units);
    }
    Ok(parsed)
}

/// A file's functions, and their encoding when they are new and should be
/// kept.
type ReadFunctions = (Vec<Function>, Option<Vec<u8>>);

/// The key a file's functions are kept under: the build of Lemma, the
/// path and what names the contents ([`lemma_http::contents_key`]).
fn functions_key(build: &str, path: &str, contents: &[u8]) -> String {
    let mut described = format!("{build}\0functions\0{path}\0").into_bytes();
    described.extend_from_slice(contents);
    lemma_model::content_hash(&described)
}

/// A file's functions, read with this thread's extractor from
/// `extractors`; none for a file in no language Lemma reads.
fn read_functions(
    file: &lemma_http::SourceFile,
    extractors: &ExtractorPool,
) -> Result<Vec<Function>, GraphError> {
    let extension = file
        .path
        .rsplit_once('.')
        .map_or("", |(_, extension)| extension);
    let Some(language) = Language::from_extension(extension) else {
        return Ok(Vec::new());
    };
    let extraction = extractors
        .with(|extractor| extractor.extract(language, &file.contents))
        .and_then(|extraction| extraction)
        .map_err(|source| GraphError::Syntax { source })?;
    Ok(file_functions(
        &file.path,
        &file.contents,
        extraction.symbols,
    ))
}

/// One file's functions, methods and values, then its top-level code.
fn file_functions(path: &str, contents: &str, symbols: Vec<lemma_model::Symbol>) -> Vec<Function> {
    let lines: Vec<&str> = contents.lines().collect();
    let mut covered = vec![false; lines.len()];
    let mut functions = Vec::new();
    for symbol in symbols {
        let kind = match symbol.kind {
            SymbolKind::Function => "function",
            SymbolKind::Method => "method",
            // A route's handler is often a constant holding its
            // registration (`const route = router.openapi(...)`).
            SymbolKind::Constant => "constant",
            SymbolKind::Variable => "variable",
            _ => continue,
        };
        let (first, last) = (
            symbol.span.start.line as usize,
            symbol.span.end.line as usize,
        );
        let last = last.min(lines.len().saturating_sub(1));
        for line in covered.iter_mut().take(last + 1).skip(first) {
            *line = true;
        }
        let body = lines
            .get(first..=last)
            .map(|body| body.join("\n"))
            .unwrap_or_default();
        functions.push(Function {
            file: path.to_owned(),
            name: symbol.name,
            qualified_name: symbol.qualified_name,
            kind: kind.to_owned(),
            lines: (
                u32::try_from(first).unwrap_or(u32::MAX) + 1,
                u32::try_from(last).unwrap_or(u32::MAX) + 1,
            ),
            mentions: impact::identifiers(&body),
            binds: Vec::new(),
        });
    }
    functions.extend(top_level(path, &lines, &mut covered));
    functions
}

/// A file's code outside its functions, as functions without a name: each
/// top-level destructuring (`const { handler } = make({ ...rpc })`), reached
/// when its own code mentions something reached and followed by the names
/// it binds, then the rest, which can be reached (a route registered with
/// an anonymous handler) but is never called. Imports are left out:
/// importing a function does not call it.
fn top_level(file: &str, lines: &[&str], covered: &mut [bool]) -> Vec<Function> {
    let mut found = destructurings(file, lines, covered);
    let mut text = Vec::new();
    let mut in_import = false;
    for (line, covered) in lines.iter().zip(covered.iter()) {
        if *covered {
            continue;
        }
        let trimmed = line.trim_start();
        let starts_import = trimmed.starts_with("import ")
            || (trimmed.starts_with("from ") && trimmed.contains(" import "));
        if starts_import || in_import {
            let ends = trimmed.contains(" from ")
                || trimmed.starts_with("from ")
                || !trimmed.contains('{');
            in_import = !(ends || trimmed.ends_with(';') || trimmed.contains('}'));
            continue;
        }
        text.push(*line);
    }
    found.push(Function {
        file: file.to_owned(),
        name: String::new(),
        qualified_name: String::new(),
        kind: String::new(),
        lines: (0, 0),
        mentions: impact::identifiers(&text.join("\n")),
        binds: Vec::new(),
    });
    found
}

/// Each top-level destructuring statement outside `covered` lines, which it
/// then covers.
fn destructurings(file: &str, lines: &[&str], covered: &mut [bool]) -> Vec<Function> {
    let mut found = Vec::new();
    let mut index = 0;
    while index < lines.len() {
        let trimmed = lines[index].trim_start().trim_start_matches("export ");
        let starts = ["const {", "let {", "var {"]
            .iter()
            .any(|start| trimmed.starts_with(start));
        if covered[index] || !starts {
            index += 1;
            continue;
        }
        let first = index;
        let mut depth = 0i64;
        let mut statement = Vec::new();
        while index < lines.len() && !covered[index] {
            statement.push(lines[index]);
            depth += bracket_change(lines[index]);
            index += 1;
            if depth <= 0 {
                break;
            }
        }
        for line in covered.iter_mut().take(index).skip(first) {
            *line = true;
        }
        let text = statement.join("\n");
        found.push(Function {
            file: file.to_owned(),
            name: String::new(),
            qualified_name: String::new(),
            kind: "destructuring".to_owned(),
            lines: (
                u32::try_from(first).unwrap_or(u32::MAX) + 1,
                u32::try_from(index).unwrap_or(u32::MAX),
            ),
            mentions: impact::identifiers(&text),
            binds: destructured_names(&text),
        });
    }
    found
}

/// How many brackets a line opens, less how many it closes.
fn bracket_change(line: &str) -> i64 {
    line.chars()
        .map(|character| match character {
            '(' | '{' | '[' => 1,
            ')' | '}' | ']' => -1,
            _ => 0,
        })
        .sum()
}

/// The name one part of a destructuring pattern binds: `X` in `def: X`,
/// `handler` in `handler` or `handler = fallback`.
fn bound_name(part: &str) -> Option<String> {
    let name = part.rsplit(':').next().unwrap_or(part);
    let name = name.split('=').next().unwrap_or(name).trim();
    let is_identifier = !name.is_empty()
        && name
            .chars()
            .all(|c| c.is_alphanumeric() || c == '_' || c == '$');
    is_identifier.then(|| name.to_owned())
}

/// The names destructuring declarations bind: `handler` and `def` in
/// `const { def: DEF, handler } = make()` bind `DEF` and `handler`.
fn destructured_names(text: &str) -> Vec<String> {
    let mut names = Vec::new();
    let mut rest = text;
    while let Some(at) = rest.find("const {").or_else(|| rest.find("let {")) {
        let after = &rest[at..];
        let Some(open) = after.find('{') else { break };
        let Some(close) = after.find('}') else { break };
        let is_declaration = after[close..]
            .trim_start_matches('}')
            .trim_start()
            .starts_with('=');
        if close > open && is_declaration {
            names.extend(after[open + 1..close].split(',').filter_map(bound_name));
        }
        rest = &after[close.max(open) + 1..];
    }
    names
}

/// The directories a repository's call graph is read from: `Some(vec![])`
/// for the whole repository, `None` for nothing.
fn code_directories(repo: &Repo) -> Option<Vec<String>> {
    let file = &repo.files[HEAD];
    let local: Vec<_> = file
        .services
        .iter()
        .filter(|(name, _)| !name.starts_with('#'))
        .map(|(_, service)| service)
        .collect();
    let whole =
        local.len() == 1 && local[0].index.include.is_empty() && local[0].exports.is_empty();
    if whole {
        return Some(Vec::new());
    }
    let mut directories = BTreeSet::new();
    for service in local {
        directories.extend(service.index.include.iter().cloned());
        for operation in service.exports.operations.values() {
            let file = operation.path.split(':').next().unwrap_or(&operation.path);
            if let Some((directory, _)) = file.rsplit_once('/') {
                directories.insert(directory.to_owned());
            }
        }
    }
    // A directory inside another listed one would be read twice.
    let outermost: Vec<String> = directories
        .iter()
        .filter(|directory| {
            !directories
                .iter()
                .any(|other| other != *directory && directory.starts_with(&format!("{other}/")))
        })
        .cloned()
        .collect();
    (!outermost.is_empty()).then_some(outermost)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn rows_are_named_by_their_repositories() {
        let owners = BTreeMap::from([
            ("#web:web".to_owned(), "#web".to_owned()),
            ("#billing:api".to_owned(), "#billing".to_owned()),
        ]);
        let relations = BTreeMap::from([
            (
                "calls".to_owned(),
                vec![vec![
                    "#web:web:src/pay.ts::Checkout.pay".to_owned(),
                    "POST /v1/charges".to_owned(),
                    "#web:web:src/pay.ts".to_owned(),
                    "7".to_owned(),
                ]],
            ),
            (
                "route".to_owned(),
                vec![vec![
                    "#billing:api".to_owned(),
                    "POST".to_owned(),
                    "/v1/charges".to_owned(),
                    "routes.go".to_owned(),
                    "12".to_owned(),
                ]],
            ),
        ]);

        let found = walkable(&relations, &owners);

        assert_eq!(
            found.calls,
            [Call {
                repository: "#web".to_owned(),
                file: "src/pay.ts".to_owned(),
                function: Some("Checkout.pay".to_owned()),
                operation: "POST /v1/charges".to_owned(),
            }]
        );
        assert_eq!(found.routes[0].repository, "#billing");
        assert_eq!(found.routes[0].operation, "POST /v1/charges");
        let values = BTreeMap::from([
            (
                "c".to_owned(),
                "#web:web:src/pay.ts::Checkout.pay".to_owned(),
            ),
            ("f".to_owned(), "#web:web:src/pay.ts".to_owned()),
        ]);
        assert_eq!(
            inferred_seed(&values, &owners),
            Some(Seed {
                repository: "#web".to_owned(),
                names: vec!["pay".to_owned()],
            })
        );
    }

    #[test]
    fn a_top_level_destructuring_binds_names_and_imports_are_not_mentions() {
        let text = "import { executeRPC } from './rpc'\nconst { def: RUN_DEF, handler: runWrapper } = makeRoute({\n  ...executeRPC,\n})\nexport default handler(router)\n";
        let lines: Vec<&str> = text.lines().collect();
        let mut covered = vec![false; lines.len()];

        let found = top_level("run.ts", &lines, &mut covered);

        assert_eq!(found[0].kind, "destructuring");
        assert_eq!(found[0].binds, ["RUN_DEF", "runWrapper"]);
        assert!(found[0].mentions.contains("executeRPC"));
        let rest = &found[1];
        assert_eq!(rest.kind, "");
        assert!(!rest.mentions.contains("executeRPC"));
        assert!(rest.mentions.contains("handler"));
    }

    #[test]
    fn an_operation_without_a_route_is_registered_where_its_service_writes_its_path() {
        let owners = BTreeMap::from([("#api:api".to_owned(), "#api".to_owned())]);
        let row = |values: &[&str]| {
            values
                .iter()
                .map(|value| (*value).to_owned())
                .collect::<Vec<_>>()
        };
        let relations = BTreeMap::from([
            (
                "operation".to_owned(),
                vec![row(&["#api:api", "POST /v1/run", "POST", "/v1/run"])],
            ),
            (
                "part_of_service".to_owned(),
                vec![row(&["#api:api:src/run.ts", "#api:api"])],
            ),
            (
                "references_operation".to_owned(),
                vec![row(&[
                    "#api:api:src/run.ts",
                    "POST /v1/run",
                    "#api:api:src/run.ts",
                    "4",
                ])],
            ),
        ]);

        let found = walkable(&relations, &owners);

        assert_eq!(found.routes.len(), 1);
        assert!(found.routes[0].passed_to_helper);
        assert_eq!(
            (found.routes[0].file.as_str(), found.routes[0].line),
            ("src/run.ts", 4)
        );
    }
}
