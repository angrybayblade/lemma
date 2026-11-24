//! What the provider layer knows of each language: the name language
//! servers know it by.

use lemma_model::Language;

/// The identifier the Language Server Protocol expects for `language` when
/// a file is opened (`languageId`).
pub(crate) fn language_id(language: Language) -> &'static str {
    match language {
        Language::TypeScript => "typescript",
        Language::Tsx => "typescriptreact",
        Language::Python => "python",
        Language::Go => "go",
        Language::Rust => "rust",
    }
}
