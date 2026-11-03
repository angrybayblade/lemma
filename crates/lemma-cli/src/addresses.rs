//! Exports as entry points: resolving an address (`services.api.charge`,
//! `#billing:services.api.charge`) to the symbol implementing it, and
//! adding declared links to a graph.

use anyhow::bail;
use lemma_graph::address::{Address, parse_address};
use lemma_model::{Export, ExportLink, GraphNode, Use, UseGraph};
use lemma_query::SymbolResolution;
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

/// Adds to `graph` the links declared from its symbols' exports: each
/// linked operation becomes a node its caller uses, named by address. The
/// other end is resolved in a workspace; here it stays an address.
pub(crate) fn add_links(graph: &mut UseGraph, exports: &[Export], links: &[ExportLink]) {
    let mut added = Vec::new();
    for node in &graph.nodes {
        let (Some(path), Some(_)) = (&node.path, node.line) else {
            continue;
        };
        let implemented = exports.iter().filter(|export| {
            export.path.as_deref() == Some(path.as_str())
                && export.symbol.as_deref().is_some_and(|symbol| {
                    node.label == symbol || node.label.ends_with(&format!(".{symbol}"))
                })
        });
        for export in implemented {
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
    use lemma_model::ExportKind;

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
                label: "ExecuteAction.Invoke".to_owned(),
                kind: "method".to_owned(),
                path: Some("gen/execute_action.go".to_owned()),
                line: Some(40),
            }],
            ..UseGraph::default()
        };
        let exports = [Export {
            service: "worker".to_owned(),
            name: "execute_action".to_owned(),
            kind: ExportKind::Operation,
            path: Some("gen/execute_action.go".to_owned()),
            symbol: Some("ExecuteAction.Invoke".to_owned()),
            detail: None,
        }];
        let links = [ExportLink {
            caller: "services.worker.execute_action".to_owned(),
            provider: "#functions:services.serverless.execute_action".to_owned(),
        }];

        add_links(&mut graph, &exports, &links);

        assert_eq!(
            graph.relationships,
            [Use {
                from: "symbol:7".to_owned(),
                to: "export:#functions:services.serverless.execute_action".to_owned(),
            }]
        );
        assert_eq!(graph.nodes[1].kind, "linked operation");
    }
}
