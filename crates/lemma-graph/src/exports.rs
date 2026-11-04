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

use lemma_model::{Export, ExportKind, ExportLink, FoundBy};
use lemma_syntax::Extractor;

use crate::GraphError;
use crate::address::parse_link;
use crate::affects::CODE_EXTENSIONS;
use crate::config::{LemmaFile, Service};
use crate::handlers::{
    Conventions, Lookup, RouteSite, passed_to_helper, resolve, resolve_declared,
};

/// Bumped whenever how exports are found changes, so stored ones built the
/// old way are rebuilt.
pub const VERSION: &str = "10";

/// What exports need to know about the indexed code.
pub struct IndexedSymbols<'a> {
    /// Definitions by name and by place, to find route handlers.
    pub lookup: Lookup<'a>,
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
                        found_by: Some(FoundBy::Declared),
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
                found_by: Some(FoundBy::Declared),
            }
        })
        .collect()
}

/// A route declared in a service's code: where its path is written.
#[derive(Debug, Clone, PartialEq, Eq)]
struct Route {
    file: String,
    line: u32,
    template: String,
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
    let (written, texts) = routes(service, root, extractor)?;
    let documents = read_documents(service, root)?;
    let aliases = route_aliases(&documents, &written);
    let routes = served_routes(written, &aliases);
    let code = ServiceCode {
        texts: &texts,
        symbols,
        conventions: Conventions {
            register: &service.exports.routes.register,
            define: &service.exports.routes.define,
        },
    };
    let mut found = Vec::new();
    for operation in documents.iter().flatten() {
        let Some(id) = operation.operation_id.clone() else {
            continue;
        };
        let templates = unaliased(&operation.template, &aliases);
        let route = templates.iter().find_map(|template| {
            let path = lemma_http::canonical_route(template)?;
            routes.get(&(operation.method.clone(), path))
        });
        let (path, symbol, found_by) = handler_of(route, &operation.method, &templates, &code);
        found.push(Export {
            service: name.to_owned(),
            name: id,
            kind: ExportKind::Endpoint,
            path,
            symbol,
            detail: Some(format!("{} {}", operation.method, operation.path)),
            found_by,
        });
    }
    Ok(found)
}

/// The operations of each of a service's `OpenAPI` documents.
fn read_documents(
    service: &Service,
    root: &Path,
) -> Result<Vec<Vec<lemma_http::Operation>>, GraphError> {
    let mut documents = Vec::new();
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
        documents.push(lemma_http::operations(&json));
    }
    Ok(documents)
}

/// A service's route aliases, as its documents and routes imply them (see
/// [`lemma_http::implied_aliases`]).
fn route_aliases(
    documents: &[Vec<lemma_http::Operation>],
    written: &[(String, String, Route)],
) -> BTreeMap<String, String> {
    let operations: Vec<Vec<(String, String)>> = documents
        .iter()
        .map(|operations| {
            operations
                .iter()
                .map(|operation| (operation.method.clone(), operation.path.clone()))
                .collect()
        })
        .collect();
    let routes = written
        .iter()
        .map(|(method, path, _)| (method.clone(), path.clone()))
        .collect();
    lemma_http::implied_aliases(&operations, &routes)
}

/// Routes by method and path, including the paths aliases also serve them
/// at; the first route written for a path wins.
fn served_routes(
    written: Vec<(String, String, Route)>,
    aliases: &BTreeMap<String, String>,
) -> BTreeMap<(String, String), Route> {
    let mut served = BTreeMap::new();
    for (method, path, route) in written {
        for path in aliased(&path, aliases) {
            served
                .entry((method.clone(), path))
                .or_insert_with(|| route.clone());
        }
    }
    served
}

/// The routes a service's code writes, as method, canonical path and where,
/// and the texts of the files kept for finding handlers.
type Routes = (Vec<(String, String, Route)>, BTreeMap<String, String>);

fn routes(service: &Service, root: &Path, extractor: &mut Extractor) -> Result<Routes, GraphError> {
    let files = lemma_http::read_files(root, None, &service.index.include, &CODE_EXTENSIONS)
        .map_err(|source| GraphError::Source { source })?;
    let mut found = Vec::new();
    let mut texts = BTreeMap::new();
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
        let declared_here: Vec<_> = facts
            .method_paths
            .iter()
            .filter(|item| item.method != "*")
            .collect();
        // Kept for finding handlers: files that declare routes, and files
        // that write paths a helper may register.
        if !declared_here.is_empty()
            || file.contents.contains("'/")
            || file.contents.contains("\"/")
        {
            texts.insert(file.path.clone(), file.contents.clone());
        }
        for declared in declared_here {
            let route = Route {
                file: file.path.clone(),
                line: declared.location.line,
                template: declared.template.clone(),
            };
            found.push((declared.method.clone(), declared.path.clone(), route));
        }
    }
    Ok((found, texts))
}

/// What was found for a documented operation: its handler's file and
/// qualified name, and how they were found.
type Handler = (Option<String>, Option<String>, Option<FoundBy>);

/// What finding a service's handlers reads: the texts of its code, the
/// indexed definitions, and the calls its `routes` directives name.
struct ServiceCode<'a> {
    texts: &'a BTreeMap<String, String>,
    symbols: &'a IndexedSymbols<'a>,
    conventions: Conventions<'a>,
}

/// A documented operation's handler: at its route, by the service's
/// `routes` directives if they decide, else by reading the registration;
/// or, when no route writes its path beside a method, the helper its path
/// is passed to.
fn handler_of(
    route: Option<&Route>,
    method: &str,
    templates: &[String],
    code: &ServiceCode<'_>,
) -> Handler {
    let lookup = &code.symbols.lookup;
    let Some(route) = route else {
        return passed_to_helper(code.texts, method, templates, lookup)
            .map_or((None, None, None), |found| {
                (Some(found.path), Some(found.qualified), Some(FoundBy::Code))
            });
    };
    let site = RouteSite {
        file: &route.file,
        text: code.texts.get(&route.file).map_or("", String::as_str),
        line: route.line,
        template: &route.template,
    };
    if let Some((path, symbol)) = resolve_declared(&site, code.conventions, lookup) {
        return (path, symbol, Some(FoundBy::Directive));
    }
    let (path, symbol) = resolve(&site, lookup);
    let found_by = path.is_some().then_some(FoundBy::Code);
    (path, symbol, found_by)
}

/// A documented path as written, then as each alias's source prefix would
/// have it written (`/v3.1/x` documented, `/v3/x` written and aliased).
fn unaliased(template: &str, aliases: &BTreeMap<String, String>) -> Vec<String> {
    let mut templates = vec![template.to_owned()];
    for (from, to) in aliases {
        if let Some(rest) = template.strip_prefix(to.as_str()) {
            templates.push(format!("{from}{rest}"));
        }
    }
    templates
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
        let unique = |_: &str, _: &str| None;
        let enclosing = |_: &str, _: u32| None;
        let top_level = |_: &str| Vec::new();
        let symbols = IndexedSymbols {
            lookup: Lookup {
                unique: &unique,
                enclosing: &enclosing,
            },
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
                found_by: Some(FoundBy::Declared),
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
}
