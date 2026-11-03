//! Attaching to each problem what it affects further up (see `impact`):
//! where each trace starts, and reading the code and routes it walks.

use std::collections::{BTreeMap, BTreeSet};

use lemma_model::{Language, SymbolKind};
use lemma_syntax::Extractor;

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
}

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
    let routes = rows("route")
        .filter_map(|row| route_site(row, owners))
        .collect();
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
) -> Result<(), GraphError> {
    let problems: Vec<&(usize, Seed)> = seeds
        .iter()
        .filter(|(index, _)| findings[*index].severity != Severity::Info)
        .collect();
    if problems.is_empty() {
        return Ok(());
    }
    let routes = read_routes(&walkable.routes, repos)?;
    let mut code = Code::new(repos)?;
    let mut traces: BTreeMap<&Seed, Vec<Impact>> = BTreeMap::new();
    for (index, seed) in problems {
        if !traces.contains_key(seed) {
            let mut functions = |repository: &str| code.functions(repository);
            let affected = impact::trace(seed, &mut functions, &routes, &walkable.calls);
            traces.insert(seed, describe(&affected));
        }
        findings[*index].affects.clone_from(&traces[seed]);
    }
    code.into_failure().map_or(Ok(()), Err)
}

/// Repositories' functions, read once each. A repository that cannot be
/// read gives none, and the first such error is kept to report.
pub(crate) struct Code<'a> {
    repos: &'a BTreeMap<String, Repo>,
    extractor: Extractor,
    functions: BTreeMap<String, Vec<Function>>,
    failure: Option<GraphError>,
}

impl<'a> Code<'a> {
    pub(crate) fn new(repos: &'a BTreeMap<String, Repo>) -> Result<Self, GraphError> {
        Ok(Self {
            repos,
            extractor: Extractor::new().map_err(|source| GraphError::Syntax { source })?,
            functions: BTreeMap::new(),
            failure: None,
        })
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
            Some(repo) => functions_of(repo, &mut self.extractor),
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

fn describe(affected: &[(usize, Affected)]) -> Vec<Impact> {
    affected
        .iter()
        .map(|(distance, item)| match item {
            Affected::Operation { service, operation } => Impact {
                distance: *distance,
                owner: service.clone(),
                at: operation.clone(),
                calls: None,
            },
            Affected::Caller {
                repository,
                location,
                operation,
            } => Impact {
                distance: *distance,
                owner: repository.clone(),
                at: location.clone(),
                calls: Some(operation.clone()),
            },
        })
        .collect()
}

/// Routes with the identifiers on their declaring lines, read at head.
pub(crate) fn read_routes(
    sites: &[RouteSite],
    repos: &BTreeMap<String, Repo>,
) -> Result<Vec<Route>, GraphError> {
    let mut sources: BTreeMap<(&str, &str), Option<String>> = BTreeMap::new();
    let mut routes = Vec::new();
    for site in sites {
        let key = (site.repository.as_str(), site.file.as_str());
        if let std::collections::btree_map::Entry::Vacant(entry) = sources.entry(key) {
            let text = match repos.get(&site.repository) {
                Some(repo) => repo.checkouts[HEAD].read(&site.file)?,
                None => None,
            };
            entry.insert(text);
        }
        let line_text = sources[&key]
            .as_deref()
            .and_then(|text| text.lines().nth(site.line.saturating_sub(1) as usize))
            .unwrap_or_default();
        routes.push(Route {
            repository: site.repository.clone(),
            operation: site.operation.clone(),
            service: site.service.clone(),
            file: site.file.clone(),
            line: site.line,
            line_mentions: impact::identifiers(line_text),
        });
    }
    Ok(routes)
}

/// A repository's functions and methods at head, from the directories its
/// services' code, routes and operations are in.
pub(crate) fn functions_of(
    repo: &Repo,
    extractor: &mut Extractor,
) -> Result<Vec<Function>, GraphError> {
    let Some(prefixes) = code_directories(repo) else {
        return Ok(Vec::new());
    };
    let checkout = &repo.checkouts[HEAD];
    let files = lemma_http::read_files(
        &checkout.root,
        checkout.commit.as_deref(),
        &prefixes,
        &CODE_EXTENSIONS,
    )
    .map_err(|source| GraphError::Source { source })?;
    let mut functions = Vec::new();
    for file in files
        .into_iter()
        .filter(|file| !lemma_http::is_test_file(&file.path))
    {
        let extension = file
            .path
            .rsplit_once('.')
            .map_or("", |(_, extension)| extension);
        let Some(language) = Language::from_extension(extension) else {
            continue;
        };
        let extraction = extractor
            .extract(language, &file.contents)
            .map_err(|source| GraphError::Syntax { source })?;
        let lines: Vec<&str> = file.contents.lines().collect();
        let mut covered = vec![false; lines.len()];
        for symbol in extraction.symbols {
            if !matches!(symbol.kind, SymbolKind::Function | SymbolKind::Method) {
                continue;
            }
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
                file: file.path.clone(),
                name: symbol.name,
                qualified_name: symbol.qualified_name,
                lines: (
                    u32::try_from(first).unwrap_or(u32::MAX) + 1,
                    u32::try_from(last).unwrap_or(u32::MAX) + 1,
                ),
                mentions: impact::identifiers(&body),
            });
        }
        functions.push(top_level(&file.path, &lines, &covered));
    }
    Ok(functions)
}

/// A file's code outside its functions, as a function without a name: it
/// can be reached (a route registered with an anonymous handler) but is
/// never called by name.
fn top_level(file: &str, lines: &[&str], covered: &[bool]) -> Function {
    let text: Vec<&str> = lines
        .iter()
        .zip(covered)
        .filter(|(_, covered)| !**covered)
        .map(|(line, _)| *line)
        .collect();
    Function {
        file: file.to_owned(),
        name: String::new(),
        qualified_name: String::new(),
        lines: (0, 0),
        mentions: impact::identifiers(&text.join("\n")),
    }
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
    let whole = local.len() == 1
        && local[0].index.include.is_empty()
        && local[0].exports.is_empty()
        && local[0].calls.clients.is_empty();
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
}
