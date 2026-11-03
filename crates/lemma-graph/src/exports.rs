//! What each service in a repository offers, and the code implementing it:
//! the entry points an index stores and queries start from.
//!
//! Declared operations sit at their code anchors. An operation an `OpenAPI`
//! document describes sits at its route's handler: the function the route's
//! line names (`r.POST("/x", h.execute)`) or, for a route declared inside
//! its handler, the function around it. A public module's top-level
//! definitions are exports too.

use std::collections::BTreeMap;
use std::path::Path;

use lemma_model::{Export, ExportKind, ExportLink};
use lemma_syntax::Extractor;

use crate::GraphError;
use crate::address::parse_link;
use crate::affects::CODE_EXTENSIONS;
use crate::config::{LemmaFile, Service};

/// Bumped whenever how exports are found changes, so stored ones built the
/// old way are rebuilt.
pub const VERSION: &str = "1";

/// What exports need to know about the indexed code.
pub struct IndexedSymbols<'a> {
    /// The function, method or type a simple name means, as its file and
    /// qualified name, if exactly one definition has that name.
    pub unique: &'a dyn Fn(&str) -> Option<(String, String)>,
    /// The qualified names of a file's top-level definitions.
    pub top_level: &'a dyn Fn(&str) -> Vec<String>,
}

/// Every local service's exports in `file`, read from the repository at
/// `root` as it is on disk, and the links `file` declares.
pub fn exports(
    file: &LemmaFile,
    root: &Path,
    symbols: &IndexedSymbols<'_>,
) -> Result<(Vec<Export>, Vec<ExportLink>), GraphError> {
    let mut found = Vec::new();
    let mut extractor = Extractor::new().map_err(|source| GraphError::Syntax { source })?;
    for (name, service) in file
        .services
        .iter()
        .filter(|(name, _)| !name.starts_with('#'))
    {
        found.extend(operations(name, service));
        found.extend(endpoints(name, service, root, symbols, &mut extractor)?);
        for path in &service.exports.symbols {
            found.extend(
                (symbols.top_level)(path)
                    .into_iter()
                    .map(|qualified| Export {
                        service: name.clone(),
                        name: qualified.clone(),
                        kind: ExportKind::Symbol,
                        path: Some(path.clone()),
                        symbol: Some(qualified),
                        detail: None,
                    }),
            );
        }
    }
    found.sort();
    found.dedup();
    let mut links = file
        .graph
        .links
        .iter()
        .map(|text| {
            let link = parse_link(text)?;
            Ok(ExportLink {
                caller: link.caller.to_string(),
                provider: link.provider.to_string(),
            })
        })
        .collect::<Result<Vec<_>, GraphError>>()?;
    links.sort();
    links.dedup();
    Ok((found, links))
}

/// Declared operations, at their anchors (`file:Symbol`).
fn operations(name: &str, service: &Service) -> Vec<Export> {
    service
        .exports
        .operations
        .iter()
        .map(|(operation, declared)| {
            let (path, symbol) = match declared.path.rsplit_once(':') {
                Some((path, symbol)) => (path.to_owned(), Some(symbol.replace("()", ""))),
                None => (declared.path.clone(), None),
            };
            Export {
                service: name.to_owned(),
                name: operation.clone(),
                kind: ExportKind::Operation,
                path: Some(path),
                symbol,
                detail: None,
            }
        })
        .collect()
}

/// A route declared in a service's code.
#[derive(Debug, Clone, PartialEq, Eq)]
struct Route {
    method: String,
    path: String,
    file: String,
    /// The definition around the route, by qualified name.
    enclosing: Option<String>,
    /// The names on the route's line that could be its handler, in order:
    /// identifiers other than method calls on an object (`router.Handle(`),
    /// which are the routing library's.
    line_names: Vec<String>,
}

/// The operations a service's documents describe, each at its route's
/// handler when one is found.
fn endpoints(
    name: &str,
    service: &Service,
    root: &Path,
    symbols: &IndexedSymbols<'_>,
    extractor: &mut Extractor,
) -> Result<Vec<Export>, GraphError> {
    if service.exports.openapi.is_empty() {
        return Ok(Vec::new());
    }
    let routes = routes(service, root, extractor)?;
    let mut found = Vec::new();
    for document in &service.exports.openapi {
        let Some(text) = lemma_http::read_file(root, None, document)
            .map_err(|source| GraphError::Source { source })?
        else {
            continue;
        };
        let json: serde_json::Value =
            serde_json::from_str(&text).map_err(|source| GraphError::ResolverOutput {
                command: format!("reading {document}"),
                source,
            })?;
        for operation in lemma_http::operations(&json) {
            let Some(id) = operation.operation_id else {
                continue;
            };
            let route = routes.get(&(operation.method.clone(), operation.path.clone()));
            let (path, symbol) = route.map_or((None, None), |route| handler(route, symbols));
            found.push(Export {
                service: name.to_owned(),
                name: id,
                kind: ExportKind::Endpoint,
                path,
                symbol,
                detail: Some(format!("{} {}", operation.method, operation.path)),
            });
        }
    }
    Ok(found)
}

/// The routes in a service's code, by method and path, including the paths
/// its route aliases also serve them at.
fn routes(
    service: &Service,
    root: &Path,
    extractor: &mut Extractor,
) -> Result<BTreeMap<(String, String), Route>, GraphError> {
    let files = lemma_http::read_files(root, None, &service.index.include, &CODE_EXTENSIONS)
        .map_err(|source| GraphError::Source { source })?;
    let mut found = BTreeMap::new();
    for file in files
        .iter()
        .filter(|file| !lemma_http::is_test_file(&file.path))
    {
        let facts = lemma_http::extract(
            &file.path,
            &file.contents,
            &lemma_http::CodeOptions::default(),
            extractor,
        )
        .map_err(|source| GraphError::Schema {
            message: format!("could not read routes in {}: {source}", file.path),
        })?;
        let lines: Vec<&str> = file.contents.lines().collect();
        for declared in facts.method_paths.iter().filter(|item| item.method != "*") {
            let line = lines
                .get(declared.location.line.saturating_sub(1) as usize)
                .copied()
                .unwrap_or_default();
            let route = Route {
                method: declared.method.clone(),
                path: declared.path.clone(),
                file: file.path.clone(),
                enclosing: declared.location.symbol.clone(),
                line_names: handler_candidates(line),
            };
            for path in aliased(&declared.path, &service.route_aliases) {
                found
                    .entry((declared.method.clone(), path))
                    .or_insert_with(|| route.clone());
            }
        }
    }
    Ok(found)
}

/// A route's path and the paths its prefixes are aliased to.
fn aliased(path: &str, aliases: &BTreeMap<String, String>) -> Vec<String> {
    let mut paths = vec![path.to_owned()];
    for (from, to) in aliases {
        if let Some(rest) = path.strip_prefix(from.as_str()) {
            paths.push(format!("{to}{rest}"));
        }
    }
    paths
}

/// The function handling a route: the last name on its line that only one
/// definition has, other than the function around it (handlers come after
/// paths and middleware); otherwise the function around it.
fn handler(route: &Route, symbols: &IndexedSymbols<'_>) -> (Option<String>, Option<String>) {
    let enclosing_name = route
        .enclosing
        .as_deref()
        .map(|qualified| qualified.rsplit('.').next().unwrap_or(qualified));
    let named = route
        .line_names
        .iter()
        .rev()
        .filter(|name| Some(name.as_str()) != enclosing_name)
        .find_map(|name| (symbols.unique)(name));
    if let Some((path, qualified)) = named {
        return (Some(path), Some(qualified));
    }
    (Some(route.file.clone()), route.enclosing.clone())
}

/// The identifiers on a line that could name a handler, in order: all but
/// method calls on an object (`.Handle(`), which belong to the routing
/// library. A method value (`h.execute`) or a call (`NewHandler(`) stays.
fn handler_candidates(line: &str) -> Vec<String> {
    let mut found = Vec::new();
    let mut start = None;
    for (index, character) in line.char_indices().chain([(line.len(), ' ')]) {
        let is_part = character.is_alphanumeric() || character == '_';
        match (start, is_part) {
            (None, true) => start = Some(index),
            (Some(begin), false) => {
                start = None;
                let word = &line[begin..index];
                if word.chars().next().is_none_or(char::is_numeric) {
                    continue;
                }
                let is_member = line[..begin].ends_with('.');
                let is_called = line[index..].trim_start().starts_with('(');
                if !(is_member && is_called) {
                    found.push(word.to_owned());
                }
            }
            _ => {}
        }
    }
    found
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::parse;

    #[test]
    fn declared_operations_and_links_become_exports_and_links() {
        let file = parse(
            "services:\n  web:\n    exports:\n      operations:\n        pay: { path: \"src/pay.go:Charges.Create\" }\ngraph:\n  links:\n    - services.web.pay -> \"#billing:services.api.charge\"\n",
        )
        .unwrap();
        let unique = |_: &str| None;
        let top_level = |_: &str| Vec::new();
        let symbols = IndexedSymbols {
            unique: &unique,
            top_level: &top_level,
        };

        let (exports, links) = exports(&file, Path::new("/nowhere"), &symbols).unwrap();

        assert_eq!(
            exports,
            [Export {
                service: "web".to_owned(),
                name: "pay".to_owned(),
                kind: ExportKind::Operation,
                path: Some("src/pay.go".to_owned()),
                symbol: Some("Charges.Create".to_owned()),
                detail: None,
            }]
        );
        assert_eq!(
            links,
            [ExportLink {
                caller: "services.web.pay".to_owned(),
                provider: "#billing:services.api.charge".to_owned(),
            }]
        );
    }

    #[test]
    fn a_route_line_naming_a_unique_function_points_at_it() {
        let route = Route {
            method: "POST".to_owned(),
            path: "/x".to_owned(),
            file: "routes.go".to_owned(),
            enclosing: Some("SetupRoutes".to_owned()),
            line_names: handler_candidates(
                r#"router.Handle("/{slug}/execute", NewExecuteHandler(client)).Methods("POST")"#,
            ),
        };
        // `Handle` is defined once too, by an unrelated type.
        let unique = |name: &str| match name {
            "NewExecuteHandler" => Some((
                "handlers/execute.go".to_owned(),
                "NewExecuteHandler".to_owned(),
            )),
            "Handle" => Some(("lib/notify.go".to_owned(), "Notifier.Handle".to_owned())),
            _ => None,
        };
        let top_level = |_: &str| Vec::new();
        let symbols = IndexedSymbols {
            unique: &unique,
            top_level: &top_level,
        };

        let found = handler(&route, &symbols);

        assert_eq!(
            found,
            (
                Some("handlers/execute.go".to_owned()),
                Some("NewExecuteHandler".to_owned())
            )
        );
    }
}
