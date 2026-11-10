//! What a definition is made of: a digest of its syntax and the names it
//! mentions (spec 17). The digest ignores comments and positions, so
//! moving a function or rewording a comment leaves it unchanged.

use std::collections::BTreeSet;

use lemma_model::{Position, Symbol};
use tree_sitter::{Node, Point, TreeCursor};

/// A definition with its digest and the names it mentions.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct OutlinedSymbol {
    /// The definition.
    pub symbol: Symbol,
    /// A digest of its syntax tree, without comments or positions.
    pub digest: [u8; 32],
    /// Every identifier in its code, sorted. This over-approximates what
    /// it refers to, which is the safe direction for finding what a change
    /// can reach.
    pub mentions: Vec<String>,
}

/// Reads the symbol out of the tree it was found in.
pub(crate) fn read_symbol(root: Node<'_>, source: &str, symbol: Symbol) -> OutlinedSymbol {
    let start = point_of(symbol.span.start);
    let end = point_of(symbol.span.end);
    let node = root.descendant_for_point_range(start, end).unwrap_or(root);
    let mut reading = Reading::new(source);
    reading.walk(node);
    OutlinedSymbol {
        symbol,
        digest: *reading.hasher.finalize().as_bytes(),
        mentions: reading.mentions.into_iter().map(str::to_owned).collect(),
    }
}

fn point_of(position: Position) -> Point {
    Point {
        row: position.line as usize,
        column: position.column as usize,
    }
}

struct Reading<'source> {
    source: &'source str,
    hasher: blake3::Hasher,
    mentions: BTreeSet<&'source str>,
    /// How many string nodes enclose the current node. A string is hashed
    /// whole, because some grammars leave parts of its text out of any
    /// token, but it is still walked: a template can mention names.
    strings_open: usize,
}

impl<'source> Reading<'source> {
    fn new(source: &'source str) -> Self {
        Self {
            source,
            hasher: blake3::Hasher::new(),
            mentions: BTreeSet::new(),
            strings_open: 0,
        }
    }

    fn walk(&mut self, node: Node<'_>) {
        let mut cursor = node.walk();
        loop {
            let current = cursor.node();
            self.enter(current);
            if !is_comment(current) && cursor.goto_first_child() {
                continue;
            }
            if !self.advance(&mut cursor) {
                return;
            }
        }
    }

    /// Closes finished nodes until there is a next sibling to visit;
    /// `false` when the walk is back at its start.
    fn advance(&mut self, cursor: &mut TreeCursor<'_>) -> bool {
        loop {
            self.exit(cursor.node());
            if cursor.goto_next_sibling() {
                return true;
            }
            if !cursor.goto_parent() {
                return false;
            }
        }
    }

    fn enter(&mut self, node: Node<'_>) {
        if is_comment(node) {
            return;
        }
        let text = self.source.get(node.byte_range()).unwrap_or("");
        if node.child_count() == 0 && node.kind().contains("identifier") {
            self.mentions.insert(text);
        }
        if self.strings_open > 0 {
            self.strings_open += usize::from(is_string(node));
            return;
        }
        // The node's kind, opened and closed, keeps the tree's shape in the
        // digest: Python's blocks differ only by tokens with no text.
        self.hasher.update(&node.kind_id().to_le_bytes());
        if is_string(node) {
            self.strings_open = 1;
            self.hash_text(text);
        } else if node.child_count() == 0 {
            self.hash_text(text);
        }
    }

    fn exit(&mut self, node: Node<'_>) {
        if is_comment(node) {
            return;
        }
        self.strings_open -= usize::from(is_string(node));
        if self.strings_open == 0 {
            self.hasher.update(&[0xff]);
        }
    }

    fn hash_text(&mut self, text: &str) {
        self.hasher.update(&(text.len() as u64).to_le_bytes());
        self.hasher.update(text.as_bytes());
    }
}

fn is_comment(node: Node<'_>) -> bool {
    node.kind().contains("comment")
}

fn is_string(node: Node<'_>) -> bool {
    node.kind().contains("string")
}

#[cfg(test)]
mod tests {
    use lemma_model::Language;

    use crate::Extractor;

    fn digest_of(language: Language, source: &str, name: &str) -> [u8; 32] {
        let outline = Extractor::new().unwrap().outline(language, source).unwrap();
        let found = outline.symbols.iter().find(|s| s.symbol.name == name);
        found.unwrap().digest
    }

    #[test]
    fn moving_a_function_leaves_its_digest() {
        let before = digest_of(Language::Python, "def f():\n    return 1\n", "f");
        let after = digest_of(Language::Python, "\n\n\ndef f():\n    return 1\n", "f");
        assert_eq!(before, after);
    }

    #[test]
    fn rewording_a_comment_leaves_its_digest() {
        let before = digest_of(Language::Go, "package a\nfunc f() { // one\n}\n", "f");
        let after = digest_of(Language::Go, "package a\nfunc f() { // two\n}\n", "f");
        assert_eq!(before, after);
    }

    #[test]
    fn a_changed_body_changes_its_digest() {
        let before = digest_of(Language::Python, "def f():\n    return 1\n", "f");
        let after = digest_of(Language::Python, "def f():\n    return 2\n", "f");
        assert_ne!(before, after);
    }

    #[test]
    fn python_indentation_that_moves_a_statement_changes_its_digest() {
        let inside = "def f(a):\n    if a:\n        g()\n        h()\n";
        let outside = "def f(a):\n    if a:\n        g()\n    h()\n";
        assert_ne!(
            digest_of(Language::Python, inside, "f"),
            digest_of(Language::Python, outside, "f")
        );
    }

    #[test]
    fn text_between_escapes_in_a_string_changes_its_digest() {
        let before = digest_of(Language::Python, "def f():\n    return 'a\\nb'\n", "f");
        let after = digest_of(Language::Python, "def f():\n    return 'c\\nb'\n", "f");
        assert_ne!(before, after);
    }

    #[test]
    fn a_docstring_is_part_of_the_digest() {
        let before = digest_of(Language::Python, "class A:\n    'one'\n", "A");
        let after = digest_of(Language::Python, "class A:\n    'two'\n", "A");
        assert_ne!(before, after);
    }

    #[test]
    fn names_inside_a_template_are_mentioned() {
        let source = "function f() { return `${helper()}`; }\n";
        let outline = Extractor::new()
            .unwrap()
            .outline(Language::TypeScript, source)
            .unwrap();
        assert!(outline.symbols[0].mentions.contains(&"helper".to_owned()));
    }
}
