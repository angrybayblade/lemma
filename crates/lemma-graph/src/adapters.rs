//! Adapters: reading a service's operations from its code rather than from
//! a committed document. An adapter imports the code in a separate process
//! and writes the `OpenAPI` document the framework itself describes for
//! it, each operation marked with `x-lemma-handler` (the file and line
//! that handles it). The document is generated in the checkout being read,
//! so at a past commit it reflects the code as it was then.

use std::collections::BTreeMap;
use std::hash::{DefaultHasher, Hash, Hasher};
use std::path::{Path, PathBuf};

use crate::GraphError;
use crate::config::{LemmaFile, OperationsFrom, Runtime};
use crate::repos::{self, Checkout};
use crate::schema::absolute_interpreter;

/// The built-in adapters: a name, its script's file name, and the script.
const BUILT_IN: [(&str, &str, &str); 2] = [
    (
        "hono-zod-openapi",
        "hono-zod-openapi.ts",
        include_str!("../adapters/hono-zod-openapi.ts"),
    ),
    (
        "fastapi",
        "fastapi_openapi.py",
        include_str!("../adapters/fastapi_openapi.py"),
    ),
];

/// The document `from` generates for `checkout`, as a file in `cache`. A
/// commit's document is generated once and kept; the working directory's
/// is generated every time, since its code can change.
pub fn generate(
    from: &OperationsFrom,
    runtime: &Runtime,
    checkout: &Checkout,
    cache: &Path,
) -> Result<PathBuf, GraphError> {
    let root = checkout.worktree(cache)?;
    let directory = from
        .directory
        .as_deref()
        .map_or_else(|| root.clone(), |directory| root.join(directory));
    let adapters = cache.join("adapters");
    create_directory(&adapters)?;
    let output = adapters.join(format!(
        "{:016x}.json",
        fingerprint(&(&root, &checkout.commit, from))
    ));
    if checkout.commit.is_some() && output.exists() {
        return Ok(output);
    }
    let mut env = runtime.env.clone();
    env.extend(from.env.clone());
    env.insert("LEMMA_OUTPUT".to_owned(), output.display().to_string());
    env.insert("LEMMA_ENTRY".to_owned(), from.entry.join(":"));
    env.insert("LEMMA_ROOT".to_owned(), root.display().to_string());
    if checkout.commit.is_some() {
        set_up(from, &root, &directory, &env)?;
    }
    let command = command(from, runtime, &checkout.root, &adapters)?;
    let _ = std::fs::remove_file(&output);
    repos::run(&directory, &command, &env)?;
    if !output.exists() {
        return Err(GraphError::Resolver {
            command,
            directory,
            message: "the adapter wrote no document to $LEMMA_OUTPUT".to_owned(),
        });
    }
    Ok(output)
}

/// Runs `setup` once in a fresh checkout, remembered by a marker file in it.
fn set_up(
    from: &OperationsFrom,
    root: &Path,
    directory: &Path,
    env: &BTreeMap<String, String>,
) -> Result<(), GraphError> {
    let Some(setup) = &from.setup else {
        return Ok(());
    };
    let marker = root.join(format!(".lemma-setup-{:016x}", fingerprint(setup)));
    if marker.exists() {
        return Ok(());
    }
    repos::run(directory, setup, env)?;
    std::fs::write(&marker, setup).map_err(|source| GraphError::Io {
        path: marker,
        source,
    })
}

/// The command to run: a built-in adapter's script, written to `adapters`,
/// with the runtime it needs, or the configured command.
fn command(
    from: &OperationsFrom,
    runtime: &Runtime,
    repository: &Path,
    adapters: &Path,
) -> Result<String, GraphError> {
    let (Some(name), None) = (&from.adapter, &from.command) else {
        return from.command.clone().ok_or_else(|| GraphError::Schema {
            message: "operations_from needs an `adapter` or a `command`".to_owned(),
        });
    };
    let Some((_, file, script)) = BUILT_IN.iter().find(|(known, _, _)| known == name) else {
        return Err(GraphError::Schema {
            message: format!(
                "no built-in adapter `{name}`; use `hono-zod-openapi` or `fastapi`, or a `command`"
            ),
        });
    };
    let path = adapters.join(file);
    if std::fs::read_to_string(&path).ok().as_deref() != Some(*script) {
        std::fs::write(&path, script).map_err(|source| GraphError::Io {
            path: path.clone(),
            source,
        })?;
    }
    let script = path.display();
    Ok(match *file {
        "fastapi_openapi.py" => {
            let python =
                absolute_interpreter(runtime.python.as_deref().unwrap_or("python3"), repository);
            format!("{python} {script}")
        }
        _ => format!("{} run {script}", runtime.bun.as_deref().unwrap_or("bun")),
    })
}

fn create_directory(path: &Path) -> Result<(), GraphError> {
    std::fs::create_dir_all(path).map_err(|source| GraphError::Io {
        path: path.to_owned(),
        source,
    })
}

/// A stable fingerprint of a value, for naming cached files.
fn fingerprint(value: &impl Hash) -> u64 {
    let mut hasher = DefaultHasher::new();
    value.hash(&mut hasher);
    hasher.finish()
}

/// The document each local service with `operations_from` in `file`
/// generates from the code at `root` as it is on disk, by service name.
/// A service whose adapter fails keeps its error, so the caller can fall
/// back to the committed documents and say why.
pub fn from_working_directory(
    file: &LemmaFile,
    root: &Path,
    cache: &Path,
) -> BTreeMap<String, Result<serde_json::Value, GraphError>> {
    let checkout = Checkout {
        root: root.to_owned(),
        commit: None,
    };
    let mut documents = BTreeMap::new();
    for (name, service) in &file.services {
        let Some(from) = &service.exports.operations_from else {
            continue;
        };
        if name.starts_with('#') {
            continue;
        }
        let document = generate(from, &service.runtime, &checkout, cache)
            .and_then(|generated| read_document(&generated));
        documents.insert(name.clone(), document);
    }
    documents
}

fn read_document(path: &Path) -> Result<serde_json::Value, GraphError> {
    let text = std::fs::read_to_string(path).map_err(|source| GraphError::Io {
        path: path.to_owned(),
        source,
    })?;
    serde_json::from_str(&text).map_err(|source| GraphError::ResolverOutput {
        command: format!("reading {}", path.display()),
        source,
    })
}

/// Where the code registers each operation of `document`, as an adapter
/// marked it with `x-lemma-handler` (`file:line`), by method and canonical
/// path.
pub fn handler_sites(document: &serde_json::Value) -> BTreeMap<(String, String), (String, u32)> {
    let mut sites = BTreeMap::new();
    let Some(paths) = document.get("paths").and_then(serde_json::Value::as_object) else {
        return sites;
    };
    for (template, item) in paths {
        let (Some(path), Some(methods)) = (lemma_http::canonical_route(template), item.as_object())
        else {
            continue;
        };
        for (method, operation) in methods {
            let Some(tag) = operation
                .get("x-lemma-handler")
                .and_then(serde_json::Value::as_str)
            else {
                continue;
            };
            let Some((file, line)) = tag.rsplit_once(':') else {
                continue;
            };
            let Ok(line) = line.parse() else {
                continue;
            };
            let key = (method.to_uppercase(), path.clone());
            sites.insert(key, (file.to_owned(), line));
        }
    }
    sites
}

/// A service's contract as read from its code at one checkout.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Contract {
    /// The documents to check against: committed ones with the code's
    /// operations laid over them.
    pub documents: Vec<PathBuf>,
    /// What the adapter could not read.
    pub unread: Vec<String>,
}

/// The contract `from` reads from a service's code at `checkout`, laid over
/// its committed `documents`.
pub fn contract(
    from: &OperationsFrom,
    runtime: &Runtime,
    checkout: &Checkout,
    cache: &Path,
    documents: &[String],
) -> Result<Contract, GraphError> {
    let generated = generate(from, runtime, checkout, cache)?;
    Ok(Contract {
        unread: unread(&generated),
        documents: contract_documents(&generated, documents, checkout)?,
    })
}

/// What an adapter could not read, as it listed in `x-lemma-unread`: modules
/// it could not import, routes it could not describe.
pub fn unread(generated: &Path) -> Vec<String> {
    let Ok(text) = std::fs::read_to_string(generated) else {
        return Vec::new();
    };
    serde_json::from_str::<serde_json::Value>(&text)
        .map(|document| unread_in(&document))
        .unwrap_or_default()
}

/// What a generated `document` lists in `x-lemma-unread`.
pub fn unread_in(document: &serde_json::Value) -> Vec<String> {
    document
        .get("x-lemma-unread")
        .and_then(|unread| serde_json::from_value(unread.clone()).ok())
        .unwrap_or_default()
}

/// The documents a service is checked against at `checkout`: its committed
/// `documents` with the operations `generated` describes laid over them
/// (see [`overlay`]), each written beside the generated document.
pub fn contract_documents(
    generated: &Path,
    documents: &[String],
    checkout: &Checkout,
) -> Result<Vec<PathBuf>, GraphError> {
    let read_json = |text: &str, file: &str| {
        serde_json::from_str::<serde_json::Value>(text).map_err(|source| {
            GraphError::ResolverOutput {
                command: format!("reading {file}"),
                source,
            }
        })
    };
    let generated_text = std::fs::read_to_string(generated).map_err(|source| GraphError::Io {
        path: generated.to_owned(),
        source,
    })?;
    let from_code = read_json(&generated_text, &generated.display().to_string())?;
    let mut committed = Vec::new();
    for document in documents {
        if let Some(text) = checkout.read(document)? {
            committed.push(read_json(&text, document)?);
        }
    }
    let mut written = Vec::new();
    for (index, document) in overlay(&committed, &from_code).iter().enumerate() {
        let path = generated.with_extension(format!("contract-{index}.json"));
        let text =
            serde_json::to_string(document).map_err(|source| GraphError::ResolverOutput {
                command: format!("writing {}", path.display()),
                source,
            })?;
        std::fs::write(&path, text).map_err(|source| GraphError::Io {
            path: path.clone(),
            source,
        })?;
        written.push(path);
    }
    Ok(written)
}

/// The committed documents with each operation the code describes taken
/// from `generated`: at the same path, or at the same path under another
/// committed document's prefix (`/api/v3.1/x` served by the `/api/v3/x`
/// route). Operations the code does not describe keep their committed
/// definition: one adapter run that missed a route is not its removal.
/// The generated document's schemas are added to each, so its operations'
/// references resolve. Without committed documents, the generated one.
pub fn overlay(
    committed: &[serde_json::Value],
    generated: &serde_json::Value,
) -> Vec<serde_json::Value> {
    if committed.is_empty() {
        return vec![generated.clone()];
    }
    let prefixes: Vec<String> = committed.iter().map(shared_prefix).collect();
    committed
        .iter()
        .zip(&prefixes)
        .map(|(document, prefix)| overlay_one(document, prefix, &prefixes, generated))
        .collect()
}

fn overlay_one(
    document: &serde_json::Value,
    prefix: &str,
    prefixes: &[String],
    generated: &serde_json::Value,
) -> serde_json::Value {
    let mut result = document.clone();
    let Some(paths) = result
        .get_mut("paths")
        .and_then(serde_json::Value::as_object_mut)
    else {
        return result;
    };
    for (path, item) in paths.iter_mut() {
        let Some(item) = item.as_object_mut() else {
            continue;
        };
        let candidates = sibling_paths(path, prefix, prefixes);
        for (method, operation) in item.iter_mut() {
            let from_code = candidates
                .iter()
                .enumerate()
                .find_map(|(index, candidate)| {
                    let found = generated.get("paths")?.get(candidate)?.get(method)?;
                    Some((index == 0, found))
                });
            if let Some((is_same_path, from_code)) = from_code {
                *operation = with_committed_id(from_code, operation, is_same_path);
            }
        }
    }
    let schemas = generated
        .pointer("/components/schemas")
        .and_then(serde_json::Value::as_object);
    if let Some(schemas) = schemas {
        let components = result.as_object_mut().map(|object| {
            object
                .entry("components")
                .or_insert_with(|| serde_json::json!({}))
        });
        if let Some(target) = components
            .and_then(|components| components.as_object_mut())
            .map(|components| {
                components
                    .entry("schemas")
                    .or_insert_with(|| serde_json::json!({}))
            })
            .and_then(serde_json::Value::as_object_mut)
        {
            target.extend(schemas.clone());
        }
    }
    result
}

/// The code's operation under the name the committed one has. The
/// `operationId` is how links and generated clients name an operation, and
/// code often leaves it to the step that writes the committed document; an
/// operation borrowed from a sibling prefix keeps its own document's name.
fn with_committed_id(
    from_code: &serde_json::Value,
    committed: &serde_json::Value,
    is_same_path: bool,
) -> serde_json::Value {
    let mut operation = from_code.clone();
    let has_own_id = is_same_path && from_code.get("operationId").is_some();
    let committed_id = committed.get("operationId").filter(|_| !has_own_id);
    if let (Some(id), Some(object)) = (committed_id, operation.as_object_mut()) {
        object.insert("operationId".to_owned(), id.clone());
    }
    operation
}

/// `path`, then `path` with its document's prefix replaced by each other
/// document's.
fn sibling_paths(path: &str, prefix: &str, prefixes: &[String]) -> Vec<String> {
    let mut candidates = vec![path.to_owned()];
    let rest = path
        .strip_prefix(prefix)
        .filter(|rest| !prefix.is_empty() && rest.starts_with('/'));
    if let Some(rest) = rest {
        candidates.extend(
            prefixes
                .iter()
                .filter(|other| !other.is_empty() && other.as_str() != prefix)
                .map(|other| format!("{other}{rest}")),
        );
    }
    candidates
}

/// The static prefix every path of a document shares, as written (`/api/v3.1`).
fn shared_prefix(document: &serde_json::Value) -> String {
    let mut paths = document
        .get("paths")
        .and_then(serde_json::Value::as_object)
        .into_iter()
        .flat_map(|paths| paths.keys())
        .map(|path| path.split('/').collect::<Vec<_>>());
    let Some(mut prefix) = paths.next() else {
        return String::new();
    };
    for path in paths {
        let shared = prefix
            .iter()
            .zip(&path)
            .take_while(|(left, right)| left == right)
            .count();
        prefix.truncate(shared);
    }
    let static_length = prefix
        .iter()
        .take_while(|segment| !segment.contains('{'))
        .count();
    prefix.truncate(static_length);
    prefix.join("/")
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn code_replaces_committed_operations_at_their_path_or_a_sibling_prefix() {
        let v3 = json!({"paths": {"/api/v3/a": {"get": {"summary": "old a", "operationId": "getA"}}, "/api/v3/b": {"get": {"summary": "old b"}}}});
        let v31 = json!({"paths": {"/api/v3.1/a": {"get": {"summary": "old a", "operationId": "getA31"}}, "/api/v3.1/c": {"get": {"summary": "old c"}}}});
        let generated = json!({
            "paths": {"/api/v3/a": {"get": {"summary": "new a"}}},
            "components": {"schemas": {"A": {"type": "object"}}}
        });

        let documents = overlay(&[v3, v31], &generated);

        assert_eq!(
            documents[0]["paths"]["/api/v3/a"]["get"]["summary"],
            "new a"
        );
        assert_eq!(
            documents[0]["paths"]["/api/v3/b"]["get"]["summary"],
            "old b"
        );
        assert_eq!(
            documents[1]["paths"]["/api/v3.1/a"]["get"]["summary"],
            "new a"
        );
        assert_eq!(
            documents[1]["paths"]["/api/v3.1/c"]["get"]["summary"],
            "old c"
        );
        assert_eq!(documents[1]["components"]["schemas"]["A"]["type"], "object");
    }

    #[test]
    fn code_operations_keep_the_committed_operation_id() {
        let v3 = json!({"paths": {"/api/v3/a": {"get": {"operationId": "getA"}}}});
        let v31 = json!({"paths": {"/api/v3.1/a": {"get": {"operationId": "getA31"}}}});
        let generated = json!({"paths": {"/api/v3/a": {"get": {"summary": "new a"}}}});

        let documents = overlay(&[v3, v31], &generated);

        assert_eq!(
            documents[0]["paths"]["/api/v3/a"]["get"]["operationId"],
            "getA"
        );
        assert_eq!(
            documents[1]["paths"]["/api/v3.1/a"]["get"]["operationId"],
            "getA31"
        );
    }
}
