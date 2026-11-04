//! Exports as entry points: resolving an address (`services.api.charge`,
//! `#billing:services.api.charge`) to the symbol implementing it, and
//! adding declared links to a graph.

use anyhow::bail;
use lemma_graph::address::{Address, parse_address};
use lemma_model::{Export, ExportLink, GraphNode, Use, UseGraph};

use crate::output::Linked;
use lemma_query::{Direction, SymbolResolution};
use lemma_store::{FactStore, Store, StoredSnapshot, StoredSymbol};

/// The address `text` names, if it is one rather than a symbol name or id:
/// `services.<service>.<name>` or `#<repository>:services.<service>.<name>`.
pub(crate) fn as_address(text: &str) -> Option<Address> {
    let is_local = text.starts_with("services.");
    let is_external = text.starts_with('#') && text.contains(":services.");
    if !is_local && !is_external {
        return None;
    }
    parse_address(text).ok()
}

/// The symbol an export in this index is implemented by.
pub(crate) fn resolve_export(
    store: &Store,
    snapshot: &StoredSnapshot,
    address: &Address,
) -> anyhow::Result<SymbolResolution> {
    if let Some(repository) = &address.repository {
        bail!(
            "`{address}` is an export of {repository}, another repository; index a workspace \
             lemma.yaml that composes both to resolve it"
        );
    }
    let exports = store.exports(snapshot.id)?;
    let Some(export) = exports
        .iter()
        .find(|export| export.service == address.service && export.name == address.operation)
    else {
        bail!(
            "no service in this index exports `{}` as `{}`; `lemma index` stores what lemma.yaml declares",
            address.operation,
            address.service
        );
    };
    let (Some(path), Some(symbol)) = (&export.path, &export.symbol) else {
        bail!(
            "`{address}` is exported{}, but the code implementing it was not found",
            export
                .detail
                .as_deref()
                .map_or_else(String::new, |detail| format!(" as {detail}"))
        );
    };
    Ok(symbol_in(store, snapshot, path, symbol)?
        .map_or(SymbolResolution::Unresolved, SymbolResolution::Resolved))
}

/// The indexed symbol `qualified` in `path`.
fn symbol_in(
    store: &Store,
    snapshot: &StoredSnapshot,
    path: &str,
    qualified: &str,
) -> anyhow::Result<Option<StoredSymbol>> {
    let symbols = store.symbols_in_file(snapshot.id, path)?;
    let suffix = format!(".{qualified}");
    Ok(symbols.into_iter().find(|symbol| {
        symbol.symbol.qualified_name == qualified || symbol.symbol.qualified_name.ends_with(&suffix)
    }))
}

/// The exports a symbol implements.
fn implemented_by<'a>(exports: &'a [Export], path: &str, qualified: &str) -> Vec<&'a Export> {
    exports
        .iter()
        .filter(|export| {
            export.path.as_deref() == Some(path)
                && export.symbol.as_deref().is_some_and(|symbol| {
                    qualified == symbol || qualified.ends_with(&format!(".{symbol}"))
                })
        })
        .collect()
}

/// What a repository's own index says a symbol is linked to: the links
/// declared from or to the exports it implements. Another repository's end
/// is named by address only; a workspace index resolves it.
pub(crate) fn repository_links(
    store: &Store,
    snapshot: &StoredSnapshot,
    symbol: &StoredSymbol,
) -> anyhow::Result<Vec<Linked>> {
    let exports = store.exports(snapshot.id)?;
    let links = store.export_links(snapshot.id)?;
    let mut linked = Vec::new();
    for export in implemented_by(&exports, &symbol.path, &symbol.symbol.qualified_name) {
        let address = export.address();
        for link in &links {
            if link.caller == address {
                linked.push(local_end("to", &link.provider, &exports));
            }
            if link.provider == address {
                linked.push(local_end("from", &link.caller, &exports));
            }
        }
    }
    Ok(linked)
}

/// A link's other end as this repository knows it: resolved if it is one
/// of its own exports.
fn local_end(direction: &'static str, address: &str, exports: &[Export]) -> Linked {
    let export = as_address(address)
        .filter(|address| address.repository.is_none())
        .and_then(|address| {
            exports.iter().find(|export| {
                export.service == address.service && export.name == address.operation
            })
        });
    Linked {
        direction,
        address: address.to_owned(),
        path: export.and_then(|export| export.path.clone()),
        symbol: export.and_then(|export| export.symbol.clone()),
    }
}

/// What a workspace index says the export at `address` (absolute) is linked
/// to, each end resolved to the code implementing it.
pub(crate) fn workspace_links(
    address: &str,
    exports: &[(String, Export)],
    links: &[ExportLink],
) -> Vec<Linked> {
    let mut linked = Vec::new();
    for link in links {
        if link.caller == address {
            linked.push(member_end("to", &link.provider, exports));
        }
        if link.provider == address {
            linked.push(member_end("from", &link.caller, exports));
        }
    }
    linked
}

/// A link's end, found among the members' exports.
fn member_end(direction: &'static str, address: &str, exports: &[(String, Export)]) -> Linked {
    let export = as_address(address).and_then(|parsed| {
        exports.iter().find(|(member, export)| {
            Some(member.as_str()) == parsed.repository.as_deref()
                && export.service == parsed.service
                && export.name == parsed.operation
        })
    });
    Linked {
        direction,
        address: address.to_owned(),
        path: export.and_then(|(member, export)| {
            export.path.as_ref().map(|path| format!("{member}:{path}"))
        }),
        symbol: export.and_then(|(_, export)| export.symbol.clone()),
    }
}

/// Adds `linked`, the root's links, to `graph` as nodes the root uses or is
/// used by, following the graph's direction.
pub(crate) fn add_root_links(graph: &mut UseGraph, linked: &[Linked], direction: Direction) {
    let Some(root) = graph.root.clone() else {
        return;
    };
    for link in linked {
        let wanted = match direction {
            Direction::Both => true,
            Direction::Callees => link.direction == "to",
            Direction::Callers => link.direction == "from",
        };
        if !wanted {
            continue;
        }
        let id = format!("export:{}", link.address);
        if !graph.nodes.iter().any(|node| node.id == id) {
            graph.nodes.push(GraphNode {
                id: id.clone(),
                label: link.symbol.clone().unwrap_or_else(|| link.address.clone()),
                kind: "linked operation".to_owned(),
                path: link.path.clone().or_else(|| Some(link.address.clone())),
                line: None,
            });
        }
        let relationship = if link.direction == "to" {
            Use {
                from: root.clone(),
                to: id,
            }
        } else {
            Use {
                from: id,
                to: root.clone(),
            }
        };
        graph.relationships.push(relationship);
    }
    graph.relationships.sort();
    graph.relationships.dedup();
}

/// Adds to `graph` the links declared from its symbols' exports: each
/// linked operation becomes a node its caller uses, named by address. The
/// other end is resolved in a workspace; here it stays an address.
pub(crate) fn add_links(graph: &mut UseGraph, exports: &[Export], links: &[ExportLink]) {
    let mut added = Vec::new();
    for node in &graph.nodes {
        let (Some(path), Some(_)) = (&node.path, node.line) else {
            continue;
        };
        for export in implemented_by(exports, path, &node.label) {
            let address = export.address();
            for link in links.iter().filter(|link| link.caller == address) {
                added.push((node.id.clone(), link.provider.clone()));
            }
        }
    }
    for (from, provider) in added {
        let id = format!("export:{provider}");
        if !graph.nodes.iter().any(|node| node.id == id) {
            graph.nodes.push(GraphNode {
                id: id.clone(),
                label: provider,
                kind: "linked operation".to_owned(),
                path: None,
                line: None,
            });
        }
        graph.relationships.push(Use { from, to: id });
    }
    graph.relationships.sort();
    graph.relationships.dedup();
}

#[cfg(test)]
mod tests {
    use super::*;
    use lemma_model::{ExportKind, FoundBy};

    #[test]
    fn addresses_are_told_apart_from_names_and_ids() {
        assert!(as_address("services.api.charge").is_some());
        assert!(as_address("#billing:services.api.charge").is_some());
        assert!(as_address("#415").is_none());
        assert!(as_address("Charges.Create").is_none());
    }

    #[test]
    fn a_linked_export_adds_its_provider_as_a_used_node() {
        let mut graph = UseGraph {
            nodes: vec![GraphNode {
                id: "symbol:7".to_owned(),
                label: "SubmitTask.Invoke".to_owned(),
                kind: "method".to_owned(),
                path: Some("gen/submit_task.go".to_owned()),
                line: Some(40),
            }],
            ..UseGraph::default()
        };
        let exports = [Export {
            service: "worker".to_owned(),
            name: "submit_task".to_owned(),
            kind: ExportKind::Operation,
            path: Some("gen/submit_task.go".to_owned()),
            symbol: Some("SubmitTask.Invoke".to_owned()),
            detail: None,
            found_by: Some(FoundBy::Declared),
        }];
        let links = [ExportLink {
            caller: "services.worker.submit_task".to_owned(),
            provider: "#functions:services.serverless.submit_task".to_owned(),
        }];

        add_links(&mut graph, &exports, &links);

        assert_eq!(
            graph.relationships,
            [Use {
                from: "symbol:7".to_owned(),
                to: "export:#functions:services.serverless.submit_task".to_owned(),
            }]
        );
        assert_eq!(graph.nodes[1].kind, "linked operation");
    }
}
