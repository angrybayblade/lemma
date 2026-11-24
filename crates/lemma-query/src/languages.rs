//! What the query layer knows of each language's text: how its comments
//! and string literals are written, for telling code from prose on a line
//! without parsing it.

use lemma_model::Language;

/// How one language writes comments and strings.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct Lexicon {
    /// What starts a comment that runs to the end of its line.
    pub line_comment: &'static str,
    /// What opens a comment or string that can run over several lines,
    /// each with what closes it.
    pub blocks: &'static [(&'static str, &'static str)],
    /// Whether `'` quotes a string. In Rust it also starts a lifetime, so
    /// it is not taken as a quote there.
    pub quotes_with_apostrophe: bool,
}

/// How `language` writes comments and strings.
pub(crate) fn lexicon(language: Language) -> Lexicon {
    match language {
        Language::Python => Lexicon {
            line_comment: "#",
            blocks: &[("\"\"\"", "\"\"\""), ("'''", "'''")],
            quotes_with_apostrophe: true,
        },
        Language::Rust => Lexicon {
            line_comment: "//",
            blocks: &[("/*", "*/")],
            quotes_with_apostrophe: false,
        },
        Language::TypeScript | Language::Tsx | Language::Go => Lexicon {
            line_comment: "//",
            blocks: &[("/*", "*/")],
            quotes_with_apostrophe: true,
        },
    }
}
