//! Following a symbol's callers out of its repository: from the functions
//! that reach it, to the operations whose handlers are among them, to the
//! code in other repositories that calls those operations, and on up.
//!
//! The first step is given: the functions that reach the symbol in its own
//! repository, found by whoever asks (a language server, say). The rest is
//! inference over the system the workspace description composes, as
//! `lemma check` traces what a break affects.

use std::collections::{BTreeMap, BTreeSet};
use std::path::Path;
use std::sync::Arc;

use lemma_logic::Engine;
use lemma_model::{GraphNode, Use};

use crate::GraphError;
use crate::affects::{self, Code, Walkable};
use crate::check::{self, CheckOptions, HEAD, Repo};
use crate::config::LemmaFile;
#[cfg(test)]
use crate::impact::LinkKind;
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

impl SystemTrace {
    /// The trace cut after `calls` calls from other code: following uses
    /// outward from `starts`, the reached functions, a `call` relationship
    /// (code calling an operation) is the only one that crosses into other
    /// code. With 1, the trace ends at the code calling the operations the
    /// starts' handlers serve, instead of climbing through that code to
    /// its own operations and their callers, and on.
    #[must_use]
    pub fn within_calls(self, starts: &BTreeSet<String>, calls: usize) -> SystemTrace {
        let mut crossed: BTreeMap<String, usize> =
            starts.iter().map(|start| (start.clone(), 0)).collect();
        let mut pending: Vec<String> = starts.iter().cloned().collect();
        let mut kept = Vec::new();
        while let Some(used) = pending.pop() {
            let at = crossed[&used];
            let incoming = self
                .relationships
                .iter()
                .filter(|relationship| relationship.to == used)
                .filter_map(|relationship| Some((relationship, step(relationship, at, calls)?)));
            for (relationship, next) in incoming {
                kept.push(relationship.clone());
                reach(&mut crossed, &mut pending, &relationship.from, next);
            }
        }
        lemma_model::dedup_uses(&mut kept);
        let used: BTreeSet<&str> = kept
            .iter()
            .flat_map(|relationship| [relationship.from.as_str(), relationship.to.as_str()])
            .collect();
        let nodes = self
            .nodes
            .into_iter()
            .filter(|node| used.contains(node.id.as_str()))
            .collect();
        SystemTrace {
            nodes,
            relationships: kept,
            notes: self.notes,
        }
    }
}

/// Records that `node` is reached having crossed `crossed_calls` calls,
/// and queues it to follow, when no shorter way to it is known.
fn reach(
    crossed: &mut BTreeMap<String, usize>,
    pending: &mut Vec<String>,
    node: &str,
    crossed_calls: usize,
) {
    let is_closer = crossed.get(node).is_none_or(|known| crossed_calls < *known);
    if is_closer {
        crossed.insert(node.to_owned(), crossed_calls);
        pending.push(node.to_owned());
    }
}

/// How many calls from other code a trace has crossed after following
/// `relationship` from a node `at` that many, or `None` past `calls`. Code
/// reached through its call at the last allowed hop is shown, but nothing
/// is followed through it.
fn step(relationship: &Use, at: usize, calls: usize) -> Option<usize> {
    if relationship.source == "call" {
        return (at < calls).then_some(at + 1);
    }
    (at < calls).then_some(at)
}

/// Where a trace keeps what it reads between runs.
pub struct TraceCaches<'a> {
    /// Clones of `git` sources and worktrees.
    pub clones: &'a Path,
    /// Facts read from code, as `lemma check` keeps them; read afresh
    /// without.
    pub facts: Option<&'a lemma_verify::FactsCache>,
}

/// How a trace reads the system: at the head, all consumer code, no change.
fn trace_options(cache: &Path) -> CheckOptions {
    CheckOptions {
        change: None,
        level: None,
        cache: cache.to_path_buf(),
        // A trace answers a query, which must not start a clone.
        cloning: crate::repos::Cloning::Forbidden,
        explain: false,
        relations: Vec::new(),
        facts_cache: None,
        evaluation: lemma_verify::Evaluation::Full,
        list_links: false,
        run_checkers: true,
        runs_base_checkers: false,
        select: None,
        warm: None,
        environment: None,
        runtime: None,
    }
}

/// Traces from `reached`, functions in the repository indexed at
/// `index_root`, through the system `workspace` composes. Inference rules
/// are evaluated with `engine`; clones, worktrees and facts read from code
/// are kept in `caches`.
pub fn trace_through_system<E: Engine>(
    workspace: &Path,
    index_root: &Path,
    reached: &[Reached],
    caches: &TraceCaches<'_>,
    engine: &E,
) -> Result<SystemTrace, GraphError> {
    let options = trace_options(caches.clones);
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
    // Calls fetched as the trace goes share inference's reads.
    let warm = Arc::new(lemma_verify::Warm::default());
    let listings = warm.listings();
    let reads = check::VerificationReads {
        facts: caches.facts,
        listings: &listings,
        warm: Some(&warm),
    };
    let walkable = current_inference(&repos, engine, &reads)?;
    let routes = affects::read_routes(&walkable.routes, &repos)?;
    let in_repository = |file: &str| join(&offset, file);
    let start = Start::Reached {
        repository: repository.clone(),
        functions: reached
            .iter()
            .map(|function| (in_repository(&function.file), function.line))
            .collect(),
    };
    let reading = affects::CodeReading {
        facts: caches.facts,
        listings: &listings,
    };
    let mut code = Code::new(&repos, &reading);
    let mut functions = |name: &str| code.functions(name);
    let mut fetch = |operations: &BTreeSet<String>| {
        check::fetch_calls(walkable.partial.as_ref(), operations, engine, &reads)
    };
    let mut source = affects::CallSource {
        walkable: &walkable,
        fetch: &mut fetch,
    };
    let mut known = affects::KnownCalls::default();
    let mut calls = |operations: &BTreeSet<String>| known.of(operations, &mut source);
    let mut tracing = impact::Tracing::default();
    let (_, links) = impact::trace_links(&mut tracing, &start, &mut functions, &routes, &mut calls);
    if let Some(error) = known.failure.take().or_else(|| code.into_failure()) {
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

/// Inference over the system as it is now: the routes every repository's
/// code shows, and how to ask for the calls of each operation a trace
/// reaches, reading only the code that may make them. One repository is
/// named as changed, at the same commit before and after, because
/// inference reads a change; a pinned one is preferred, so working
/// directories are read as they are.
fn current_inference<E: Engine>(
    repos: &BTreeMap<String, Repo>,
    engine: &E,
    reads: &check::VerificationReads<'_>,
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
            facts_cache: reads.facts.cloned(),
            evaluation: lemma_verify::Evaluation::Demand,
            warm: reads.warm.cloned(),
            also_keep: None,
        };
        // The calls of no operation: routes and operations, with calls
        // fetched as the trace reaches their operations.
        let report = lemma_verify::calls_of(
            &inferred.manifest,
            engine,
            &options,
            reads.listings,
            &BTreeSet::new(),
        )
        .map_err(|source| GraphError::Inference { source })?;
        let mut walkable = affects::walkable(&report.relations, &inferred.repositories);
        walkable.partial = report.covered.map(|covered| affects::PartialCalls {
            covered: covered.into_iter().collect(),
            manifest: inferred.manifest.clone(),
            owners: inferred.repositories.clone(),
        });
        return Ok(walkable);
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
        trace.relationships.push(Use {
            from,
            to,
            source: link.kind.as_str().to_owned(),
        });
    }
    lemma_model::dedup_uses(&mut trace.relationships);
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
    let Node::Function {
        file,
        line,
        name,
        kind,
    } = node
    else {
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
        kind: if kind.is_empty() { "code" } else { kind }.to_owned(),
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
                // Code found by the requests it sends: what kind of
                // definition it is was not read.
                kind: if function.is_some() { "caller" } else { "file" }.to_owned(),
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
                    kind: "method".to_owned(),
                },
                kind: LinkKind::Route,
            },
            Link {
                user: Node::Affected(caller),
                used: Node::Affected(operation),
                kind: LinkKind::Call,
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

    fn uses(pairs: &[(&str, &str, &str)]) -> Vec<Use> {
        pairs
            .iter()
            .map(|(from, to, source)| Use {
                from: (*from).to_owned(),
                to: (*to).to_owned(),
                source: (*source).to_owned(),
            })
            .collect()
    }

    #[test]
    fn a_trace_ends_at_the_code_calling_the_first_operations() {
        let relationships = uses(&[
            ("function:handler", "symbol:1", "name"),
            ("operation:execute", "function:handler", "route"),
            ("code:gateway:run", "operation:execute", "call"),
            ("function:gateway:route_handler", "code:gateway:run", "name"),
            (
                "operation:gateway route",
                "function:gateway:route_handler",
                "route",
            ),
            ("code:dashboard:page", "operation:gateway route", "call"),
        ]);
        let ids = [
            "function:handler",
            "operation:execute",
            "code:gateway:run",
            "function:gateway:route_handler",
            "operation:gateway route",
            "code:dashboard:page",
        ];
        let trace = SystemTrace {
            nodes: ids
                .iter()
                .map(|id| GraphNode {
                    id: (*id).to_owned(),
                    label: (*id).to_owned(),
                    kind: "code".to_owned(),
                    path: None,
                    line: None,
                })
                .collect(),
            relationships,
            notes: Vec::new(),
        };
        let starts = BTreeSet::from(["symbol:1".to_owned()]);

        let first = trace.clone().within_calls(&starts, 1);
        let second = trace.within_calls(&starts, 2);

        let froms = |trace: &SystemTrace| -> Vec<String> {
            trace
                .relationships
                .iter()
                .map(|relationship| relationship.from.clone())
                .collect()
        };
        assert_eq!(
            froms(&first),
            ["code:gateway:run", "function:handler", "operation:execute"]
        );
        assert_eq!(first.nodes.len(), 3);
        assert_eq!(froms(&second).len(), 6);
    }

    #[test]
    fn files_in_an_index_below_the_repository_root_are_prefixed() {
        assert_eq!(join("", "src/a.rs"), "src/a.rs");
        assert_eq!(join("crates", "lemma/src/a.rs"), "crates/lemma/src/a.rs");
    }
}
