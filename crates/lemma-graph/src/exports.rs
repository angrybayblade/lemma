//! What each service in a repository offers, and the code implementing it:
//! the entry points an index stores and queries start from.
//!
//! Declared operations sit at their code anchors. An operation an `OpenAPI`
//! document describes sits at its route's handler: the function the route's
//! line names (`r.POST("/x", h.execute)`) or, for a route declared inside
//! its handler, the function around it. A public module's top-level
//! definitions are exports too.

use std::collections::{BTreeMap, HashMap};
use std::path::{Path, PathBuf};
use std::sync::{Mutex, PoisonError};

use lemma_model::{Export, ExportKind, ExportLink, FoundBy};
use lemma_syntax::ExtractorPool;
use rayon::prelude::*;

use crate::GraphError;
use crate::adapters;
use crate::address::parse_link;
use crate::affects::CODE_EXTENSIONS;
use crate::config::{LemmaFile, Service};
use crate::handlers::{
    Conventions, Lookup, RouteSite, decorated, defined_at, passed_to_helper, registered_call,
    resolve, resolve_declared,
};

/// Bumped whenever how exports are found changes, so stored ones built the
/// old way are rebuilt.
pub const VERSION: &str = "11";

/// What exports need to know about the indexed code.
pub struct IndexedSymbols<'a> {
    /// Definitions by name and by place, to find route handlers.
    pub lookup: Lookup<'a>,
    /// The qualified names of a file's top-level definitions.
    pub top_level: &'a dyn Fn(&str) -> Vec<String>,
}

/// The routes each file was found to write, kept by a process that stays:
/// exports are built again after every edit, and reading a service's code
/// for its routes is most of that, so a file is parsed again only when its
/// contents change.
#[derive(Default)]
pub struct KeptRoutes {
    /// By repository and path.
    read: Mutex<HashMap<(PathBuf, String), ReadRoutes>>,
}

/// One file's routes, and the contents they were read from.
struct ReadRoutes {
    digest: String,
    routes: Vec<Written>,
}

impl KeptRoutes {
    /// The routes kept for the file at `path` of the repository at `root`,
    /// if they were read from contents with this `digest`.
    fn of(&self, root: &Path, path: &str, digest: &str) -> Option<Vec<Written>> {
        let read = self.read.lock().unwrap_or_else(PoisonError::into_inner);
        let kept = read.get(&(root.to_path_buf(), path.to_owned()))?;
        (kept.digest == digest).then(|| kept.routes.clone())
    }

    fn keep(&self, root: &Path, path: &str, digest: String, routes: Vec<Written>) {
        // A poisoned lock means a thread panicked while reading or
        // replacing one entry; the others are whole.
        let mut read = self.read.lock().unwrap_or_else(PoisonError::into_inner);
        let key = (root.to_path_buf(), path.to_owned());
        read.insert(key, ReadRoutes { digest, routes });
    }
}

/// How a service's files are read for routes: a parser for each thread,
/// and the routes kept from earlier reads, if any are.
#[derive(Clone, Copy)]
struct RouteReading<'a> {
    extractors: &'a ExtractorPool,
    kept: Option<&'a KeptRoutes>,
}

/// Every local service's exports in `file`, read from the repository at
/// `root` as it is on disk, and the links `file` declares. `from_code`
/// holds, by service, the documents adapters generated from the code (see
/// [`crate::adapters`]); `kept` holds the routes earlier calls read.
pub fn exports(
    file: &LemmaFile,
    root: &Path,
    symbols: &IndexedSymbols<'_>,
    from_code: &BTreeMap<String, adapters::Generated>,
    kept: Option<&KeptRoutes>,
) -> Result<(Vec<Export>, Vec<ExportLink>), GraphError> {
    let mut found = Vec::new();
    let extractors = ExtractorPool::new();
    let reading = RouteReading {
        extractors: &extractors,
        kept,
    };
    for (name, service) in file
        .services
        .iter()
        .filter(|(name, _)| !name.starts_with('#'))
    {
        found.extend(operations(name, service));
        let service_code = ServiceInput {
            name,
            service,
            from_code: from_code.get(name),
        };
        found.extend(endpoints(&service_code, root, symbols, reading)?);
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

/// A local service, and the document its adapter generated from its code,
/// if it has one.
struct ServiceInput<'a> {
    name: &'a str,
    service: &'a Service,
    from_code: Option<&'a adapters::Generated>,
}

/// The operations a service's documents describe, each at its route's
/// handler when one is found. With an adapter, the code's operations are
/// laid over the committed documents, and each sits at the handler its
/// registration names.
fn endpoints(
    input: &ServiceInput<'_>,
    root: &Path,
    symbols: &IndexedSymbols<'_>,
    reading: RouteReading<'_>,
) -> Result<Vec<Export>, GraphError> {
    let service = input.service;
    if service.exports.openapi.is_empty() && input.from_code.is_none() {
        return Ok(Vec::new());
    }
    let (written, mut texts) = routes(service, root, reading)?;
    let documents = service_documents(input, root)?;
    let registered: BTreeMap<(String, String), (String, u32)> =
        documents.iter().flat_map(adapters::handler_sites).collect();
    read_missing(&mut texts, registered.values().map(|(file, _)| file), root)?;
    let documents: Vec<Vec<lemma_http::Operation>> =
        documents.iter().map(lemma_http::operations).collect();
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
        let id = operation
            .operation_id
            .clone()
            .unwrap_or_else(|| unnamed_operation_name(&operation.method, &operation.template));
        let key = (operation.method.clone(), operation.path.clone());
        let at_registration = registered
            .get(&key)
            .and_then(|(file, line)| handler_registered_at(file, *line, operation, &code));
        let (path, symbol, found_by) = at_registration.unwrap_or_else(|| {
            let templates = unaliased(&operation.template, &aliases);
            let route = templates.iter().find_map(|template| {
                let path = lemma_http::canonical_route(template)?;
                routes.get(&(operation.method.clone(), path))
            });
            handler_of(route, &operation.method, &templates, &code)
        });
        found.push(Export {
            service: input.name.to_owned(),
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

/// Adds to `texts` each of `files` it lacks, as on disk under `root`: the
/// files registrations are in, which need not write a path themselves.
fn read_missing<'a>(
    texts: &mut BTreeMap<String, String>,
    files: impl Iterator<Item = &'a String>,
    root: &Path,
) -> Result<(), GraphError> {
    for file in files {
        if texts.contains_key(file) {
            continue;
        }
        if let Some(text) = lemma_http::read_file(root, None, file)
            .map_err(|source| GraphError::Source { source })?
        {
            texts.insert(file.clone(), text);
        }
    }
    Ok(())
}

/// The documents a service's endpoints come from: its committed ones, with
/// the operations its adapter read from the code laid over them.
fn service_documents(
    input: &ServiceInput<'_>,
    root: &Path,
) -> Result<Vec<serde_json::Value>, GraphError> {
    let committed = read_documents(input.service, root)?;
    let Some(generated) = input.from_code else {
        return Ok(committed
            .into_iter()
            .map(|(_, document)| document)
            .collect());
    };
    let from_code_name = format!("{} (from code)", input.name);
    let laid = adapters::overlay::overlay(
        &committed,
        generated.document()?,
        &generated.reading,
        &from_code_name,
    );
    Ok(laid
        .documents
        .into_iter()
        .map(|(_, document)| document)
        .collect())
}

/// The committed `OpenAPI` documents of a service, by name.
fn read_documents(
    service: &Service,
    root: &Path,
) -> Result<Vec<(String, serde_json::Value)>, GraphError> {
    let mut documents = Vec::new();
    for document in &service.exports.openapi {
        let Some(text) = lemma_http::read_file(root, None, document)
            .map_err(|source| GraphError::Source { source })?
        else {
            continue;
        };
        documents.push((document.clone(), adapters::parse_document(document, &text)?));
    }
    Ok(documents)
}

/// A name for an operation its document gives no `operationId`, from its
/// method and path: `get_items_id` for `GET /items/{id}`. Frameworks that
/// leave names to the author (`createRoute` without one) would otherwise
/// leave the operation out.
fn unnamed_operation_name(method: &str, template: &str) -> String {
    let mut name = method.to_lowercase();
    for word in template.split(|c: char| !c.is_ascii_alphanumeric()) {
        if word.is_empty() {
            continue;
        }
        name.push('_');
        name.push_str(word);
    }
    name
}

/// A service's route aliases, as its documents and routes imply them (see
/// [`lemma_http::implied_aliases`]).
fn route_aliases(
    documents: &[Vec<lemma_http::Operation>],
    written: &[Written],
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
    written: Vec<Written>,
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

/// A route as code writes it: method, canonical path and where.
type Written = (String, String, Route);

/// The routes a service's code writes, and the texts of the files kept for
/// finding handlers.
type Routes = (Vec<Written>, BTreeMap<String, String>);

fn routes(service: &Service, root: &Path, reading: RouteReading<'_>) -> Result<Routes, GraphError> {
    let files = lemma_http::read_files(root, None, &service.index.include, &CODE_EXTENSIONS)
        .map_err(|source| GraphError::Source { source })?;
    let code: Vec<&lemma_http::SourceFile> = files
        .iter()
        .filter(|file| !lemma_http::is_test_file(&file.path))
        .collect();
    // Parsed on every core, and taken in the files' order, so the routes
    // and the first error are those reading the files one by one gives.
    let read: Vec<Result<Vec<Written>, GraphError>> = code
        .par_iter()
        .map(|file| written_routes(file, root, reading))
        .collect();
    let mut found = Vec::new();
    let mut texts = BTreeMap::new();
    for (file, declared) in code.iter().zip(read) {
        let declared = declared?;
        // Kept for finding handlers: files that declare routes, and files
        // that write paths a helper may register.
        if !declared.is_empty() || file.contents.contains("'/") || file.contents.contains("\"/") {
            texts.insert(file.path.clone(), file.contents.clone());
        }
        found.extend(declared);
    }
    Ok((found, texts))
}

/// The routes the file `file` of the repository at `root` writes with a
/// method: those kept for its contents, or read from them now.
fn written_routes(
    file: &lemma_http::SourceFile,
    root: &Path,
    reading: RouteReading<'_>,
) -> Result<Vec<Written>, GraphError> {
    let Some(kept) = reading.kept else {
        return parsed_routes(file, reading.extractors);
    };
    let digest = lemma_model::content_hash(file.contents.as_bytes());
    if let Some(routes) = kept.of(root, &file.path, &digest) {
        return Ok(routes);
    }
    let routes = parsed_routes(file, reading.extractors)?;
    kept.keep(root, &file.path, digest, routes.clone());
    Ok(routes)
}

/// The routes `file`'s code writes with a method.
fn parsed_routes(
    file: &lemma_http::SourceFile,
    extractors: &ExtractorPool,
) -> Result<Vec<Written>, GraphError> {
    let facts = extractors
        .with(|extractor| {
            lemma_http::extract(
                &file.path,
                &file.contents,
                &lemma_http::CodeOptions::default(),
                extractor,
            )
        })
        .map_err(|source| GraphError::Syntax { source })?
        .map_err(|source| GraphError::Schema {
            message: format!("could not read routes in {}: {source}", file.path),
        })?;
    let routes = facts
        .method_paths
        .iter()
        .filter(|item| item.method != "*")
        .map(|declared| {
            let route = Route {
                file: file.path.clone(),
                line: declared.location.line,
                template: declared.template.clone(),
            };
            (declared.method.clone(), declared.path.clone(), route)
        })
        .collect();
    Ok(routes)
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

/// The handler of an operation an adapter saw registered at `file:line`,
/// read from that registration as a route's would be; `None` when the
/// registration names nothing found.
fn handler_registered_at(
    file: &str,
    line: u32,
    operation: &lemma_http::Operation,
    code: &ServiceCode<'_>,
) -> Option<Handler> {
    let lookup = &code.symbols.lookup;
    let site = RouteSite {
        file,
        text: code.texts.get(file).map_or("", String::as_str),
        line,
        template: &operation.template,
    };
    if let Some((path, symbol)) = resolve_declared(&site, code.conventions, lookup) {
        return Some((path, symbol, Some(FoundBy::Adapter)));
    }
    let found = defined_at(&site, lookup)
        .or_else(|| decorated(&site, lookup))
        .or_else(|| registered_call(&site, lookup));
    if let Some(found) = found {
        return Some((
            Some(found.path),
            Some(found.qualified),
            Some(FoundBy::Adapter),
        ));
    }
    let (path, symbol) = resolve(&site, lookup);
    path.is_some()
        .then_some((path, symbol, Some(FoundBy::Adapter)))
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
    use crate::config::{Graph, LemmaFile, Operation, Service};

    #[test]
    fn declared_operations_and_links_become_exports_and_links() {
        let mut web = Service::default();
        web.exports.operations.insert(
            "pay".to_owned(),
            Operation {
                path: "src/pay.go:Charges.Create".to_owned(),
                ..Operation::default()
            },
        );
        let file = LemmaFile {
            services: [("web".to_owned(), web)].into(),
            graph: Graph {
                links: vec!["services.web.pay -> #billing:services.api.charge".to_owned()],
                ..Graph::default()
            },
        };
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

        let (exports, links) = exports(
            &file,
            Path::new("/nowhere"),
            &symbols,
            &BTreeMap::new(),
            None,
        )
        .unwrap();

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

    fn server(path: &str) -> lemma_http::SourceFile {
        lemma_http::SourceFile {
            path: "src/server.ts".to_owned(),
            contents: format!(
                "import express from \"express\";\nconst app = express();\napp.post(\"{path}\", (req, res) => res.json({{}}));\n"
            ),
        }
    }

    #[test]
    fn kept_routes_are_those_of_the_contents_last_read() {
        let extractors = ExtractorPool::new();
        let kept = KeptRoutes::default();
        let reading = RouteReading {
            extractors: &extractors,
            kept: Some(&kept),
        };
        let root = Path::new("/repo");
        let (first, edited) = (server("/v1/charges"), server("/v1/refunds"));
        let digest =
            |file: &lemma_http::SourceFile| lemma_model::content_hash(file.contents.as_bytes());

        let read = written_routes(&first, root, reading).unwrap();
        let kept_after_first = kept.of(root, &first.path, &digest(&first));
        let after_edit = written_routes(&edited, root, reading).unwrap();

        assert_eq!(read, parsed_routes(&first, &extractors).unwrap());
        assert_eq!(kept_after_first, Some(read.clone()));
        assert_eq!(after_edit, parsed_routes(&edited, &extractors).unwrap());
        assert_ne!(after_edit, read);
        assert_eq!(kept.of(root, &first.path, &digest(&first)), None);
    }
}
