//! Printing a use graph: as trees for reading in a terminal, as Graphviz DOT
//! or Mermaid for drawing, or as JSON.
//!
//! Every scope (a symbol now; modules, repositories and services later)
//! produces the same graph shape, so these work for all of them.

use std::collections::{BTreeMap, BTreeSet};
use std::fmt::Write as _;
use std::io::{self, Write};

use lemma_model::{GraphNode, UseGraph};
use serde::Serialize;

use super::{Format, print_json};

/// How to draw a graph in text.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Drawing {
    /// Trees of what uses the root and what it uses.
    Tree,
    /// Graphviz DOT.
    Dot,
    /// A Mermaid flowchart.
    Mermaid,
}

#[derive(Serialize)]
struct GraphView<'a> {
    root: Option<&'a str>,
    source: &'a str,
    nodes: &'a [GraphNode],
    relationships: Vec<RelationshipView<'a>>,
    notes: &'a [String],
    truncated: bool,
}

#[derive(Serialize)]
struct RelationshipView<'a> {
    from: &'a str,
    to: &'a str,
    source: &'a str,
}

/// Prints `graph`: as JSON when asked, otherwise drawn as `drawing` says.
pub(crate) fn use_graph(
    out: &mut dyn Write,
    graph: &UseGraph,
    drawing: Drawing,
    format: Format,
) -> io::Result<()> {
    if format == Format::Json {
        let view = GraphView {
            root: graph.root.as_deref(),
            source: &graph.source,
            nodes: &graph.nodes,
            relationships: graph
                .relationships
                .iter()
                .map(|relationship| RelationshipView {
                    from: &relationship.from,
                    to: &relationship.to,
                    source: &relationship.source,
                })
                .collect(),
            notes: &graph.notes,
            truncated: graph.truncated,
        };
        return print_json(out, &view);
    }
    let text = match drawing {
        Drawing::Tree => tree(graph),
        Drawing::Dot => dot(graph),
        Drawing::Mermaid => mermaid(graph),
    };
    out.write_all(text.as_bytes())
}

/// The root, what uses it and what it uses, as indented trees, then a
/// summary and any notes.
fn tree(graph: &UseGraph) -> String {
    let Some(root) = graph.root.as_deref().and_then(|root| node(graph, root)) else {
        return summary(graph);
    };
    let mut text = String::new();
    let _ = writeln!(text, "{}", describe(root));
    for (heading, upwards) in [("used by", true), ("uses", false)] {
        // Only what reaches the root (or what it reaches) belongs in this
        // tree, not relationships found while following the other way.
        let allowed = reachable(graph, &root.id, upwards);
        let mut printer = TreePrinter {
            graph,
            upwards,
            allowed,
            shown: BTreeSet::from([root.id.clone()]),
            text,
        };
        let children = printer.children(&root.id);
        if !children.is_empty() {
            let _ = writeln!(printer.text, "  {heading}");
            printer.branch(&root.id, &children, "  ");
        }
        text = printer.text;
    }
    text.push_str(&summary(graph));
    text
}

/// Prints one tree, each node once: a node reached again is marked.
struct TreePrinter<'a> {
    graph: &'a UseGraph,
    upwards: bool,
    allowed: BTreeSet<String>,
    shown: BTreeSet<String>,
    text: String,
}

impl<'a> TreePrinter<'a> {
    /// What to say about how `child` uses (or is used by) `parent` when it
    /// is less certain than a server's answer: found by its name, or only
    /// by naming its type.
    fn edge_note(&self, parent: &str, child: &str) -> &'static str {
        let (from, to) = if self.upwards {
            (child, parent)
        } else {
            (parent, child)
        };
        let sources: BTreeSet<&str> = self
            .graph
            .relationships
            .iter()
            .filter(|relationship| relationship.from == from && relationship.to == to)
            .map(|relationship| relationship.source.as_str())
            .collect();
        if sources.contains("lsp") || sources.contains("route") || sources.contains("call") {
            return "";
        }
        if sources.contains("type") {
            return "  through its type";
        }
        if sources.contains("name") {
            return "  by name";
        }
        ""
    }

    fn children(&self, id: &str) -> Vec<&'a GraphNode> {
        neighbours(self.graph, id, self.upwards)
            .into_iter()
            .filter(|node| self.allowed.contains(&node.id))
            .collect()
    }

    fn branch(&mut self, parent: &str, children: &[&GraphNode], indent: &str) {
        for (index, child) in children.iter().enumerate() {
            let is_last = index + 1 == children.len();
            let (stem, next_indent) = if is_last {
                ("└─ ", format!("{indent}   "))
            } else {
                ("├─ ", format!("{indent}│  "))
            };
            if !self.shown.insert(child.id.clone()) {
                let _ = writeln!(self.text, "{indent}{stem}{} (shown above)", child.label);
                continue;
            }
            let note = self.edge_note(parent, &child.id);
            let _ = writeln!(self.text, "{indent}{stem}{}{note}", describe(child));
            let grandchildren = self.children(&child.id);
            self.branch(&child.id, &grandchildren, &next_indent);
        }
    }
}

/// What uses `id` (`upwards`) or what it uses, in node order.
fn neighbours<'a>(graph: &'a UseGraph, id: &str, upwards: bool) -> Vec<&'a GraphNode> {
    let ids: BTreeSet<&str> = graph
        .relationships
        .iter()
        .filter_map(|relationship| match upwards {
            true if relationship.to == id => Some(relationship.from.as_str()),
            false if relationship.from == id => Some(relationship.to.as_str()),
            _ => None,
        })
        .collect();
    graph
        .nodes
        .iter()
        .filter(|node| ids.contains(node.id.as_str()))
        .collect()
}

/// The nodes that reach `id` through uses (`upwards`), or that it reaches.
fn reachable(graph: &UseGraph, id: &str, upwards: bool) -> BTreeSet<String> {
    let mut found = BTreeSet::new();
    let mut pending = vec![id.to_owned()];
    while let Some(current) = pending.pop() {
        for next in neighbours(graph, &current, upwards) {
            if found.insert(next.id.clone()) {
                pending.push(next.id.clone());
            }
        }
    }
    found
}

fn node<'a>(graph: &'a UseGraph, id: &str) -> Option<&'a GraphNode> {
    graph.nodes.iter().find(|node| node.id == id)
}

/// `label  (kind, path:line)`.
fn describe(node: &GraphNode) -> String {
    let place = match (&node.path, node.line) {
        (Some(path), Some(line)) => format!(", {path}:{line}"),
        (Some(path), None) => format!(", {path}"),
        _ => String::new(),
    };
    format!("{}  ({}{place})", node.label, node.kind)
}

/// How the relationships were found: one source by name (`lsp`), or how
/// many came from each when they were found several ways (`12 lsp, 40
/// name, 3 call`), so a trace by name is not taken for a server's answer.
fn sources(graph: &UseGraph) -> String {
    let mut counts: BTreeMap<&str, usize> = BTreeMap::new();
    for relationship in &graph.relationships {
        *counts.entry(relationship.source.as_str()).or_default() += 1;
    }
    if counts.len() <= 1 {
        return counts
            .into_keys()
            .next()
            .unwrap_or(graph.source.as_str())
            .to_owned();
    }
    let parts: Vec<String> = counts
        .iter()
        .map(|(source, count)| format!("{count} {source}"))
        .collect();
    parts.join(", ")
}

fn summary(graph: &UseGraph) -> String {
    let mut text = format!(
        "{} nodes, {} relationships ({}{})\n",
        graph.nodes.len(),
        graph.relationships.len(),
        sources(graph),
        if graph.truncated {
            "; stopped at the node limit, raise --max-nodes for more"
        } else {
            ""
        }
    );
    for note in &graph.notes {
        let _ = writeln!(text, "note: {note}");
    }
    text
}

fn dot(graph: &UseGraph) -> String {
    let mut text = String::from(
        "digraph uses {\n  rankdir=LR;\n  node [shape=box, fontname=\"Helvetica\"];\n",
    );
    for node in &graph.nodes {
        let place = node.path.as_deref().unwrap_or_default();
        let style = if graph.root.as_deref() == Some(node.id.as_str()) {
            ", style=bold"
        } else {
            ""
        };
        let _ = writeln!(
            text,
            "  {} [label={}{style}];",
            quoted(&node.id),
            quoted(&format!("{}\n{} {place}", node.label, node.kind))
        );
    }
    for relationship in &graph.relationships {
        let _ = writeln!(
            text,
            "  {} -> {};",
            quoted(&relationship.from),
            quoted(&relationship.to)
        );
    }
    text.push_str("}\n");
    text
}

fn quoted(text: &str) -> String {
    format!(
        "\"{}\"",
        text.replace('\\', "\\\\")
            .replace('"', "\\\"")
            .replace('\n', "\\n")
    )
}

fn mermaid(graph: &UseGraph) -> String {
    let mut text = String::from("flowchart LR\n");
    let position = |id: &str| graph.nodes.iter().position(|node| node.id == id);
    for (index, node) in graph.nodes.iter().enumerate() {
        let label = node.label.replace('"', "'");
        let _ = writeln!(text, "  n{index}[\"{label}\"]");
    }
    for relationship in &graph.relationships {
        if let (Some(from), Some(to)) = (position(&relationship.from), position(&relationship.to)) {
            let _ = writeln!(text, "  n{from} --> n{to}");
        }
    }
    if let Some(root) = graph.root.as_deref().and_then(position) {
        let _ = writeln!(text, "  style n{root} stroke-width:3px");
    }
    text
}

#[cfg(test)]
mod tests {
    use super::*;
    use lemma_model::Use;

    fn node(id: &str, label: &str) -> GraphNode {
        GraphNode {
            id: id.to_owned(),
            label: label.to_owned(),
            kind: "function".to_owned(),
            path: Some(format!("{label}.py")),
            line: Some(1),
        }
    }

    fn uses(from: &str, to: &str) -> Use {
        Use {
            from: from.to_owned(),
            to: to.to_owned(),
            source: "lsp".to_owned(),
        }
    }

    fn sample() -> UseGraph {
        UseGraph {
            nodes: vec![
                node("a", "charge"),
                node("b", "checkout"),
                node("c", "main"),
                node("d", "send"),
            ],
            relationships: vec![uses("a", "d"), uses("b", "a"), uses("c", "b")],
            root: Some("a".to_owned()),
            source: "lsp".to_owned(),
            notes: Vec::new(),
            truncated: false,
        }
    }

    #[test]
    fn a_graph_found_several_ways_counts_each_source() {
        let mut graph = sample();
        graph.relationships[0].source = "name".to_owned();

        let footer = sources(&graph);

        assert_eq!(footer, "2 lsp, 1 name");
    }

    #[test]
    fn a_use_found_by_name_says_so_in_the_tree() {
        let mut graph = sample();
        graph.relationships[2].source = "name".to_owned();

        let text = tree(&graph);

        assert!(
            text.contains("main  (function, main.py:1)  by name"),
            "{text}"
        );
        assert!(
            text.contains("checkout  (function, checkout.py:1)\n"),
            "{text}"
        );
    }

    #[test]
    fn trees_show_what_uses_the_root_and_what_it_uses() {
        let text = tree(&sample());

        assert_eq!(
            text,
            "charge  (function, charge.py:1)\n  used by\n  └─ checkout  (function, checkout.py:1)\n     └─ main  (function, main.py:1)\n  uses\n  └─ send  (function, send.py:1)\n4 nodes, 3 relationships (lsp)\n"
        );
    }

    #[test]
    fn mermaid_and_dot_draw_every_relationship() {
        let mermaid = mermaid(&sample());
        let dot = dot(&sample());

        assert!(mermaid.contains("  n1 --> n0\n"));
        assert!(mermaid.contains("style n0 stroke-width:3px"));
        assert!(dot.contains("  \"b\" -> \"a\";\n"));
        assert!(dot.contains("\"a\" [label=\"charge\\nfunction charge.py\", style=bold]"));
    }
}
