//! Following a symbol's callers out of its repository: from the functions
//! that reach it, to the operations whose handlers are among them, to the
//! code in other repositories that calls those operations, and on up.
//!
//! The first step is given: the functions that reach the symbol in its own
//! repository, found by whoever asks (a language server, say). The rest is
//! inference over the system the workspace `lemma.yaml` composes, as
//! `lemma check` traces what a break affects.

use std::collections::BTreeMap;
use std::path::Path;

use lemma_logic::Engine;
use lemma_model::{GraphNode, Use};

use crate::GraphError;
use crate::affects::{self, Code, Walkable};
use crate::check::{self, CheckOptions, HEAD, Repo};
use crate::config::LemmaFile;
use crate::impact::{self, Affected, Link, Node, Start};
use crate::infer;
use crate::repos::resolve_commit;

/// A function already known to reach the traced symbol.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Reached {
    /// Its node id in the caller's graph.
    pub node: String,
    /// Its file, relative to the root of the index it came from.
    pub file: String,
    /// Its first line, counting from 1.
    pub line: u32,
}

/// What a trace through the system adds to a graph: operations and code in
/// other repositories, and who uses whom among them and the reached
/// functions.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct SystemTrace {
    /// New nodes: operations and calling code.
    pub nodes: Vec<GraphNode>,
    /// Use relationships, from the reached functions' node ids and the new
    /// nodes' ids.
    pub relationships: Vec<Use>,
    /// What could not be traced, and why.
    pub notes: Vec<String>,
}

/// Traces from `reached`, functions in the repository indexed at
/// `index_root`, through the system `workspace` composes. Inference rules
/// are evaluated with `engine`; clones and worktrees go in `cache`.
pub fn trace_through_system<E: Engine>(
    workspace: &Path,
    index_root: &Path,
    reached: &[Reached],
    cache: &Path,
    engine: &E,
) -> Result<SystemTrace, GraphError> {
    let options = CheckOptions {
        change: None,
        level: None,
        cache: cache.to_path_buf(),
        facts_cache: None,
    };
    let repos = check::load(workspace, &options)?;
    let Some((repository, offset)) = owning_repository(&repos, index_root) else {
        let note = format!(
            "{} is not inside any repository {} composes",
            index_root.display(),
            workspace.display()
        );
        return Ok(SystemTrace {
            notes: vec![note],
            ..SystemTrace::default()
        });
    };
    let walkable = current_inference(&repos, engine)?;
    let routes = affects::read_routes(&walkable.routes, &repos)?;
    let in_repository = |file: &str| join(&offset, file);
    let start = Start::Reached {
        repository: repository.clone(),
        functions: reached
            .iter()
            .map(|function| (in_repository(&function.file), function.line))
            .collect(),
    };
    let listings = lemma_http::Listings::default();
    let reading = affects::CodeReading {
        facts: None,
        listings: &listings,
    };
    let mut code = Code::new(&repos, &reading);
    let mut functions = |name: &str| code.functions(name);
    let mut tracing = impact::Tracing::default();
    let (_, links) = impact::trace_links(
        &mut tracing,
        &start,
        &mut functions,
        &routes,
        &walkable.calls,
    );
    if let Some(error) = code.into_failure() {
        return Err(error);
    }
    let reached_ids: BTreeMap<(String, u32), &str> = reached
        .iter()
        .map(|function| {
            (
                (in_repository(&function.file), function.line),
                function.node.as_str(),
            )
        })
        .collect();
    Ok(to_graph(&links, &reached_ids, &offset))
}

/// The repository whose clone contains `index_root`, and where the index
/// root is inside it (`""` for the root itself). The deepest clone wins.
fn owning_repository(
    repos: &BTreeMap<String, Repo>,
    index_root: &Path,
) -> Option<(String, String)> {
    let index_root = std::fs::canonicalize(index_root).unwrap_or_else(|_| index_root.to_path_buf());
    repos
        .iter()
        .filter_map(|(name, repo)| {
            let root = &repo.checkouts[HEAD].root;
            let inside = index_root.strip_prefix(root).ok()?;
            Some((root.components().count(), name, inside.to_path_buf()))
        })
        .max_by_key(|(depth, _, _)| *depth)
        .map(|(_, name, inside)| (name.clone(), path_text(&inside)))
}

fn path_text(path: &Path) -> String {
    path.components()
        .map(|part| part.as_os_str().to_string_lossy().into_owned())
        .collect::<Vec<_>>()
        .join("/")
}

/// `file` (relative to the index root) relative to the repository root.
fn join(offset: &str, file: &str) -> String {
    if offset.is_empty() {
        return file.to_owned();
    }
    format!("{offset}/{file}")
}

/// Inference over the system as it is now: the calls and routes every
/// repository's code shows. One repository is named as changed, at the
/// same commit before and after, because inference reads a change; a
/// pinned one is preferred, so working directories are read as they are.
fn current_inference<E: Engine>(
    repos: &BTreeMap<String, Repo>,
    engine: &E,
) -> Result<Walkable, GraphError> {
    let files = repos
        .values()
        .map(check::inference_file)
        .collect::<Result<Vec<LemmaFile>, GraphError>>()?;
    let sources: Vec<infer::Source<'_>> = repos
        .iter()
        .zip(&files)
        .map(|((name, repo), file)| infer::Source {
            name,
            root: &repo.checkouts[HEAD].root,
            commit: repo.checkouts[HEAD].commit.as_deref(),
            file,
        })
        .collect();
    let mut candidates: Vec<(&String, &Repo)> = repos.iter().collect();
    candidates.sort_by_key(|(_, repo)| repo.checkouts[HEAD].commit.is_none());
    for (name, repo) in candidates {
        let checkout = &repo.checkouts[HEAD];
        let commit = match &checkout.commit {
            Some(commit) => commit.clone(),
            None => match resolve_commit(&checkout.root, "HEAD") {
                Ok(commit) => commit,
                Err(_) => continue,
            },
        };
        let changed = infer::Changed {
            name,
            base: &commit,
            head: Some(&commit),
        };
        let Some(inferred) = infer::manifest(&sources, &changed) else {
            continue;
        };
        let options = lemma_verify::Options {
            explain: false,
            relations: affects::TRACED_RELATIONS.map(str::to_owned).to_vec(),
            facts_cache: None,
        };
        let report = lemma_verify::verify(&inferred.manifest, engine, &options)
            .map_err(|source| GraphError::Inference { source })?;
        return Ok(affects::walkable(&report.relations, &inferred.repositories));
    }
    Ok(Walkable::default())
}

/// The links as graph nodes and relationships. A reached function keeps
/// the id it has in the caller's graph.
fn to_graph(links: &[Link], reached: &BTreeMap<(String, u32), &str>, offset: &str) -> SystemTrace {
    let mut trace = SystemTrace::default();
    for link in links {
        let from = node_id(&link.user, reached, offset, &mut trace);
        let to = node_id(&link.used, reached, offset, &mut trace);
        trace.relationships.push(Use { from, to });
    }
    trace.relationships.sort();
    trace.relationships.dedup();
    trace
}

/// A link's node as a graph node id, adding the node if it is new.
fn node_id(
    node: &Node,
    reached: &BTreeMap<(String, u32), &str>,
    offset: &str,
    trace: &mut SystemTrace,
) -> String {
    let graph_node = match node {
        Node::Function { file, line, .. } => {
            if let Some(id) = reached.get(&(file.clone(), *line)) {
                return (*id).to_owned();
            }
            function_node(node, offset)
        }
        Node::Affected(affected) => affected_node(affected),
    };
    let id = graph_node.id.clone();
    if !trace.nodes.iter().any(|existing| existing.id == id) {
        trace.nodes.push(graph_node);
    }
    id
}

/// A function found on the way to a route's handler, with its path
/// relative to the index root, as the graph's other functions are.
fn function_node(node: &Node, offset: &str) -> GraphNode {
    let Node::Function { file, line, name } = node else {
        unreachable!("function_node is called with functions only");
    };
    let relative = file
        .strip_prefix(offset)
        .and_then(|rest| rest.strip_prefix('/'))
        .filter(|_| !offset.is_empty())
        .unwrap_or(file);
    GraphNode {
        id: format!("function:{file}:{line}"),
        label: name.clone(),
        kind: "function".to_owned(),
        path: Some(relative.to_owned()),
        line: Some(*line),
    }
}

fn affected_node(affected: &Affected) -> GraphNode {
    match affected {
        Affected::Operation { service, operation } => GraphNode {
            id: format!("operation:{service} {operation}"),
            label: operation.clone(),
            kind: "operation".to_owned(),
            path: Some(service.clone()),
            line: None,
        },
        Affected::Caller {
            repository,
            location,
            ..
        } => {
            let (file, function) = location
                .split_once("::")
                .map_or((location.as_str(), None), |(file, function)| {
                    (file, Some(function))
                });
            GraphNode {
                id: format!("code:{repository}:{location}"),
                label: function.unwrap_or(file).to_owned(),
                kind: if function.is_some() {
                    "function"
                } else {
                    "file"
                }
                .to_owned(),
                path: Some(format!("{repository}:{file}")),
                line: None,
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn links_become_relationships_between_reached_and_new_nodes() {
        let operation = Affected::Operation {
            service: "#billing:api".to_owned(),
            operation: "POST /v1/charges".to_owned(),
        };
        let caller = Affected::Caller {
            repository: "#web".to_owned(),
            location: "src/pay.ts::Checkout.pay".to_owned(),
            operation: "POST /v1/charges".to_owned(),
        };
        let links = [
            Link {
                user: Node::Affected(operation.clone()),
                used: Node::Function {
                    file: "api/charges.go".to_owned(),
                    line: 12,
                    name: "Charges.Create".to_owned(),
                },
            },
            Link {
                user: Node::Affected(caller),
                used: Node::Affected(operation),
            },
        ];
        let reached = BTreeMap::from([(("api/charges.go".to_owned(), 12), "symbol:7")]);

        let trace = to_graph(&links, &reached, "");

        let pairs: Vec<(&str, &str)> = trace
            .relationships
            .iter()
            .map(|relationship| (relationship.from.as_str(), relationship.to.as_str()))
            .collect();
        assert_eq!(
            pairs,
            [
                (
                    "code:#web:src/pay.ts::Checkout.pay",
                    "operation:#billing:api POST /v1/charges"
                ),
                ("operation:#billing:api POST /v1/charges", "symbol:7"),
            ]
        );
        assert_eq!(trace.nodes[1].label, "Checkout.pay");
        assert_eq!(trace.nodes[1].path.as_deref(), Some("#web:src/pay.ts"));
    }

    #[test]
    fn files_in_an_index_below_the_repository_root_are_prefixed() {
        assert_eq!(join("", "src/a.rs"), "src/a.rs");
        assert_eq!(join("crates", "lemma/src/a.rs"), "crates/lemma/src/a.rs");
    }
}
