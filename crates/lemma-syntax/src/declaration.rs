//! A definition's declaration as written: its header without the body,
//! such as `def delete(self, id: str) -> None:`.

use lemma_model::{Language, Span};
use tree_sitter::{Node, Parser, Point};

use crate::{SyntaxError, tree_sitter_language};

/// Longer declarations, such as a Go struct with many fields, are cut here.
const MAX_LINES: usize = 12;

/// How far below the definition node to look for its body. A TypeScript
/// arrow function assigned to a constant keeps its body two levels down.
const BODY_SEARCH_DEPTH: usize = 3;

/// The source text of the definition spanning `span`, up to where its body
/// starts. Definitions without a body, such as constants, are returned
/// whole. `None` if no node spans exactly that range, as when the file
/// changed since the span was recorded.
pub fn declaration_text(
    language: Language,
    source: &str,
    span: Span,
) -> Result<Option<String>, SyntaxError> {
    let mut parser = Parser::new();
    parser
        .set_language(&tree_sitter_language(language))
        .map_err(|source| SyntaxError::Grammar { language, source })?;
    let tree = parser
        .parse(source, None)
        .ok_or(SyntaxError::ParseAborted { language })?;
    let Some(definition) = node_spanning(tree.root_node(), span) else {
        return Ok(None);
    };
    let end = find_body(definition, BODY_SEARCH_DEPTH)
        .map_or(definition.end_byte(), |body| body.start_byte());
    let header = &source[declaration_start(definition)..end];
    Ok(Some(tidy(header)))
}

/// The node whose range is exactly `span`, outermost first.
fn node_spanning(root: Node<'_>, span: Span) -> Option<Node<'_>> {
    let start = point(span.start);
    let end = point(span.end);
    let mut node = root.named_descendant_for_point_range(start, end)?;
    while node.start_position() != start || node.end_position() != end {
        node = node.parent()?;
    }
    while let Some(parent) = node.parent() {
        if parent.start_position() != start || parent.end_position() != end {
            break;
        }
        node = parent;
    }
    Some(node)
}

/// Where the declaration starts: a TypeScript variable's definition is its
/// declarator, so include the `const` or `let` before it.
fn declaration_start(definition: Node<'_>) -> usize {
    let keyword_parent = definition.parent().filter(|parent| {
        matches!(
            parent.kind(),
            "lexical_declaration" | "variable_declaration"
        )
    });
    match (definition.kind(), keyword_parent) {
        ("variable_declarator", Some(parent)) => parent.start_byte(),
        _ => definition.start_byte(),
    }
}

fn find_body(node: Node<'_>, depth: usize) -> Option<Node<'_>> {
    if let Some(body) = node.child_by_field_name("body") {
        return Some(body);
    }
    if depth == 0 {
        return None;
    }
    let mut cursor = node.walk();
    let children: Vec<_> = node.named_children(&mut cursor).collect();
    children
        .into_iter()
        .find_map(|child| find_body(child, depth - 1))
}

/// Trims the header, drops an opening brace left before the body, and cuts
/// very long declarations.
fn tidy(header: &str) -> String {
    let trimmed = header.trim_end();
    let trimmed = trimmed.strip_suffix('{').unwrap_or(trimmed).trim_end();
    let lines: Vec<&str> = trimmed.lines().collect();
    if lines.len() <= MAX_LINES {
        return trimmed.to_owned();
    }
    let mut cut = lines[..MAX_LINES].join("\n");
    cut.push_str("\n…");
    cut
}

fn point(position: lemma_model::Position) -> Point {
    Point {
        row: position.line as usize,
        column: position.column as usize,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::Extractor;

    /// The declaration of the first extracted symbol named `name`.
    fn declaration_of(language: Language, source: &str, name: &str) -> Option<String> {
        let extraction = Extractor::new().unwrap().extract(language, source).unwrap();
        let symbol = extraction
            .symbols
            .iter()
            .find(|symbol| symbol.name == name)
            .unwrap();
        declaration_text(language, source, symbol.span).unwrap()
    }

    #[test]
    fn python_functions_stop_before_the_body() {
        let source = "class Router:\n    def delete(self, id: str) -> None:\n        pass\n";

        let declaration = declaration_of(Language::Python, source, "delete");

        assert_eq!(
            declaration.as_deref(),
            Some("def delete(self, id: str) -> None:")
        );
    }

    #[test]
    fn go_functions_drop_the_opening_brace() {
        let source = "package main\n\nfunc Delete(id string) error {\n\treturn nil\n}\n";

        let declaration = declaration_of(Language::Go, source, "Delete");

        assert_eq!(declaration.as_deref(), Some("func Delete(id string) error"));
    }

    #[test]
    fn typescript_arrow_functions_stop_before_the_body() {
        let source =
            "export const remove = async (id: string): Promise<void> => {\n  return;\n};\n";

        let declaration = declaration_of(Language::TypeScript, source, "remove");

        assert_eq!(
            declaration.as_deref(),
            Some("const remove = async (id: string): Promise<void> =>")
        );
    }

    #[test]
    fn constants_are_returned_whole() {
        let source = "MAX_RETRIES = 3\n";

        let declaration = declaration_of(Language::Python, source, "MAX_RETRIES");

        assert_eq!(declaration.as_deref(), Some("MAX_RETRIES = 3"));
    }

    #[test]
    fn a_span_matching_no_node_gives_nothing() {
        let span = Span {
            start: lemma_model::Position { line: 0, column: 2 },
            end: lemma_model::Position { line: 0, column: 3 },
        };

        let declaration = declaration_text(Language::Python, "x = 1\n", span).unwrap();

        assert_eq!(declaration, None);
    }

    #[test]
    fn long_declarations_are_cut() {
        let header = (0..20)
            .map(|line| format!("line{line}"))
            .collect::<Vec<_>>();

        let tidied = tidy(&header.join("\n"));

        assert_eq!(tidied.lines().count(), MAX_LINES + 1);
        assert!(tidied.ends_with('…'));
    }
}
