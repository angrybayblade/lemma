//! TypeScript the Tree-sitter grammar does not accept yet, though the
//! compiler does. A file using it parses with an error that can swallow
//! the definitions after it, so symbols, code facts and schemas went
//! missing from valid code.
//!
//! When a TypeScript file parses with errors, those constructs are blanked
//! (replaced with spaces, or with `any` for a type), keeping every byte
//! offset, line and column, and the file is parsed again; the second tree
//! is kept only if it has fewer errors. Text is always read from the
//! original source, so nothing downstream sees the blanking. A file that
//! parses cleanly is parsed once, as before.

use std::sync::LazyLock;

use lemma_model::Language;
use regex::Regex;
use tree_sitter::{Parser, Tree, TreeCursor};

// The patterns are constant and the tests below compile each; one that
// failed to compile would only mean that construct is not blanked.

/// `typeof import('module')` used as a type, with any members after it, as
/// in `vi.importActual<typeof import('./x')>()` or
/// `typeof import('./x').run`.
static TYPEOF_IMPORT: LazyLock<Option<Regex>> = LazyLock::new(|| {
    Regex::new(r#"typeof\s+import\s*\(\s*(?:'[^'\n]*'|"[^"\n]*")\s*\)(?:\.[\w$]+|\[[^\]\n]*\])*"#)
        .ok()
});

/// Type arguments on a tagged template, as in ``sql<Row[]>`select 1` `` or
/// ``sql<{ id: string }[]>`...` ``: the arguments, between the tag and the
/// backtick, on one line.
static TAGGED_TYPE_ARGUMENTS: LazyLock<Option<Regex>> =
    LazyLock::new(|| Regex::new(r"[\w$\])](<[^<>`;\n]*(?:<[^<>`;\n]*>[^<>`;\n]*)*>)[ \t]*`").ok());

/// `import('module')` used as a type, after `:`, `<`, `|`, `&` or `,`, as in
/// `(policy: import('./schema').Policy) => void`; elsewhere `import(...)`
/// is a dynamic import, which code facts read, and is left alone.
static IMPORT_TYPE: LazyLock<Option<Regex>> = LazyLock::new(|| {
    Regex::new(
        r#"[:<|&,][ \t]*(import\s*\(\s*(?:'[^'\n]*'|"[^"\n]*")\s*\)(?:\.[\w$]+|\[[^\]\n]*\])*)"#,
    )
    .ok()
});

/// A lone `&` in JSX text, as in `<h2>Version & events</h2>` or a line of
/// text between tags: React accepts it, the TSX grammar does not. Either
/// between a tag's `>` and the next `<` on one line, or on a line holding
/// only text.
static JSX_TEXT_AMPERSAND: LazyLock<Option<Regex>> =
    LazyLock::new(|| Regex::new(r#"(?m)(?:>[^<>{}\n]*?|^[ \t]*[^<>{}=;()'"`\n]*?)\s(&)\s"#).ok());

/// A member named `in` starting a line, as in an object type
/// `{ name: string⏎ in: 'query' | 'header' }`, which the grammar reads as
/// the `in` operator.
static MEMBER_NAMED_IN: LazyLock<Option<Regex>> =
    LazyLock::new(|| Regex::new(r"(?m)^[ \t]+(in)\??:").ok());

/// The `type` of `export type * from './x'`.
static EXPORT_TYPE_STAR: LazyLock<Option<Regex>> =
    LazyLock::new(|| Regex::new(r"\bexport\s+(type)\s+\*").ok());

/// Parses `source`, retrying a TypeScript or TSX file that has syntax
/// errors with the constructs the grammar does not accept blanked, and
/// keeping whichever tree has fewer errors. `None` only if the parser
/// gives up, as [`Parser::parse`] does.
pub fn parse(parser: &mut Parser, language: Language, source: &str) -> Option<Tree> {
    let tree = parser.parse(source, None)?;
    let is_typescript = matches!(language, Language::TypeScript | Language::Tsx);
    if !is_typescript || !tree.root_node().has_error() {
        return Some(tree);
    }
    let Some(blanked) = blank_unsupported(source, language == Language::Tsx) else {
        return Some(tree);
    };
    let Some(retried) = parser.parse(&blanked, None) else {
        return Some(tree);
    };
    if error_count(&retried) < error_count(&tree) {
        return Some(retried);
    }
    Some(tree)
}

/// `source` with the TypeScript the grammar does not accept blanked, the
/// same length and with the same line breaks; `None` if it has none.
/// `is_tsx` adds what only JSX needs.
pub fn blank_unsupported(source: &str, is_tsx: bool) -> Option<String> {
    let mut ranges: Vec<(usize, usize, &str)> = Vec::new();
    for found in TYPEOF_IMPORT
        .iter()
        .flat_map(|pattern| pattern.find_iter(source))
    {
        ranges.push((found.start(), found.end(), "any"));
    }
    let mut groups = vec![
        (&*IMPORT_TYPE, "any"),
        (&*TAGGED_TYPE_ARGUMENTS, ""),
        (&*EXPORT_TYPE_STAR, ""),
        // Renamed for the parse only: names are read from the original.
        (&*MEMBER_NAMED_IN, "__"),
    ];
    if is_tsx {
        groups.push((&*JSX_TEXT_AMPERSAND, ""));
    }
    for (pattern, replacement) in groups {
        let captured = pattern
            .iter()
            .flat_map(|pattern| pattern.captures_iter(source))
            .filter_map(|captures| captures.get(1));
        for group in captured {
            ranges.push((group.start(), group.end(), replacement));
        }
    }
    if ranges.is_empty() {
        return None;
    }
    let mut blanked = source.as_bytes().to_vec();
    for (start, end, replacement) in ranges {
        for (offset, byte) in blanked[start..end].iter_mut().enumerate() {
            let kept = matches!(*byte, b'\n' | b'\r');
            *byte = match replacement.as_bytes().get(offset) {
                Some(letter) if !kept => *letter,
                _ if kept => *byte,
                _ => b' ',
            };
        }
    }
    // Only ASCII inside the matched ranges is replaced, and only with
    // ASCII, so the text stays valid UTF-8 unless a range held other
    // characters, in which case the original stands.
    String::from_utf8(blanked).ok()
}

/// How many error and missing nodes a tree holds.
fn error_count(tree: &Tree) -> usize {
    let mut cursor = tree.walk();
    let mut count = 0;
    count_errors(&mut cursor, &mut count);
    count
}

fn count_errors(cursor: &mut TreeCursor<'_>, count: &mut usize) {
    loop {
        let node = cursor.node();
        if node.is_error() || node.is_missing() {
            *count += 1;
        }
        if node.has_error() && cursor.goto_first_child() {
            count_errors(cursor, count);
            cursor.goto_parent();
        }
        if !cursor.goto_next_sibling() {
            return;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn parser(language: Language) -> Parser {
        let mut parser = Parser::new();
        parser
            .set_language(&crate::tree_sitter_language(language))
            .unwrap();
        parser
    }

    #[test]
    fn a_typeof_import_type_argument_parses_once_blanked() {
        let source = "const real = await vi.importActual<typeof import('./x')>('./x')\nexport function after() {}\n";

        let tree = parse(
            &mut parser(Language::TypeScript),
            Language::TypeScript,
            source,
        )
        .unwrap();

        assert!(!tree.root_node().has_error());
    }

    #[test]
    fn a_tagged_template_with_type_arguments_parses_once_blanked() {
        let source =
            "const rows = await db.$queryRaw<Row[]>`select 1`\nexport function after() {}\n";

        let tree = parse(
            &mut parser(Language::TypeScript),
            Language::TypeScript,
            source,
        )
        .unwrap();

        assert!(!tree.root_node().has_error());
    }

    #[test]
    fn a_member_of_an_imported_type_parses_once_blanked() {
        let source = "type Run = typeof import('./x').run\nexport function after() {}\n";

        let tree = parse(
            &mut parser(Language::TypeScript),
            Language::TypeScript,
            source,
        )
        .unwrap();

        assert!(!tree.root_node().has_error());
    }

    #[test]
    fn a_tagged_template_typed_with_an_object_parses_once_blanked() {
        let source = "const rows = await db.$queryRaw<{ id: string }[]>`select id`\nexport function after() {}\n";

        let tree = parse(
            &mut parser(Language::TypeScript),
            Language::TypeScript,
            source,
        )
        .unwrap();

        assert!(!tree.root_node().has_error());
    }

    #[test]
    fn a_member_named_in_parses_and_keeps_its_name() {
        let source = "export function f(p: {\n  name: string\n  in: 'query' | 'header'\n}) {}\n";

        let tree = parse(
            &mut parser(Language::TypeScript),
            Language::TypeScript,
            source,
        )
        .unwrap();

        assert!(!tree.root_node().has_error());
        let named_in = source.find("in:").unwrap();
        let node = tree
            .root_node()
            .descendant_for_byte_range(named_in, named_in + 2)
            .unwrap();
        assert_eq!(&source[node.byte_range()], "in");
    }

    #[test]
    fn an_imported_type_without_typeof_parses_once_blanked() {
        let source = "type Check = (policy: import('./schema').Policy) => boolean\nexport function after() {}\n";

        let tree = parse(
            &mut parser(Language::TypeScript),
            Language::TypeScript,
            source,
        )
        .unwrap();

        assert!(!tree.root_node().has_error());
    }

    #[test]
    fn a_dynamic_import_is_not_blanked() {
        let source = "const module = await import('./x')\n";

        assert_eq!(blank_unsupported(source, false), None);
    }

    #[test]
    fn an_ampersand_in_jsx_text_parses_once_blanked() {
        let source = "export const Title = () => (\n  <h2>\n    Version & events\n  </h2>\n)\n";

        let tree = parse(&mut parser(Language::Tsx), Language::Tsx, source).unwrap();

        assert!(!tree.root_node().has_error());
    }

    #[test]
    fn blanking_keeps_every_offset_and_line() {
        let source = "export type * from './types'\nf<typeof import(\"m\")>()\n";

        let blanked = blank_unsupported(source, false).unwrap();

        assert_eq!(blanked.len(), source.len());
        assert_eq!(blanked.lines().count(), source.lines().count());
        assert_eq!(
            blanked,
            "export      * from './types'\nf<any               >()\n"
        );
    }

    #[test]
    fn source_without_those_constructs_is_not_blanked() {
        assert_eq!(blank_unsupported("const x = f<string>(y)\n", true), None);
    }

    #[test]
    fn the_patterns_compile_and_match_what_they_name() {
        let matches =
            |pattern: &Option<Regex>, text: &str| pattern.as_ref().unwrap().is_match(text);

        assert!(matches(&TYPEOF_IMPORT, "typeof import('x')"));
        assert!(matches(&TAGGED_TYPE_ARGUMENTS, "q<T>`x`"));
        assert!(matches(&EXPORT_TYPE_STAR, "export type * from 'x'"));
        assert!(matches(&MEMBER_NAMED_IN, "  in: 'header'"));
        assert!(matches(&IMPORT_TYPE, ": import('x').T"));
        assert!(matches(&JSX_TEXT_AMPERSAND, "<b>a & b</b>"));
    }
}
