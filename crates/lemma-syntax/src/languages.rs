//! What the syntax layer knows of each language: its grammar, the query
//! that finds its definitions, and how to retry a file its grammar rejects.
//!
//! No other module, and no other crate, names a grammar: they ask for a
//! [`parser`](crate::parser) by language. That keeps a language's syntax in
//! one place, from which it can later be supplied from outside Lemma's core.

use lemma_model::Language;

use crate::repair;

/// The Tree-sitter grammar that parses `language`.
pub(crate) fn grammar(language: Language) -> tree_sitter::Language {
    match language {
        Language::TypeScript => tree_sitter_typescript::LANGUAGE_TYPESCRIPT.into(),
        Language::Tsx => tree_sitter_typescript::LANGUAGE_TSX.into(),
        Language::Python => tree_sitter_python::LANGUAGE.into(),
        Language::Go => tree_sitter_go::LANGUAGE.into(),
        Language::Rust => tree_sitter_rust::LANGUAGE.into(),
    }
}

/// The query that captures `language`'s definitions and their names.
pub(crate) fn definition_query(language: Language) -> &'static str {
    match language {
        Language::TypeScript | Language::Tsx => include_str!("../queries/typescript.scm"),
        Language::Python => include_str!("../queries/python.scm"),
        Language::Go => include_str!("../queries/go.scm"),
        Language::Rust => include_str!("../queries/rust.scm"),
    }
}

/// Blanks, in a file's source, what a grammar does not accept though the
/// language's own compiler does. The result has the same length and line
/// breaks; `None` when the source has nothing to blank.
pub(crate) type Blanking = fn(&str) -> Option<String>;

/// How to blank what `language`'s grammar rejects, for a second parse of a
/// file with syntax errors; `None` for a language whose grammar is taken at
/// its word.
pub(crate) fn blanking(language: Language) -> Option<Blanking> {
    match language {
        Language::TypeScript => Some(repair::blank_typescript),
        Language::Tsx => Some(repair::blank_tsx),
        Language::Python | Language::Go | Language::Rust => None,
    }
}
