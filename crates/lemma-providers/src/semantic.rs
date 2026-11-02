//! The questions Lemma asks semantic providers, independent of how a
//! provider answers them.
//!
//! A language server is the first kind of provider. Others, such as a
//! batch indexer or a resolver used as a library, can answer the same
//! questions, so the query layer only ever talks to this trait.

use lemma_model::{Language, Location, LocationQuery, Position, ProviderAnswer};

use crate::ProviderError;

/// A position in a source file a provider is asked about.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct SourcePoint<'a> {
    /// Path relative to the provider's root.
    pub path: &'a str,
    /// Language of the file.
    pub language: Language,
    /// The position, in Lemma's UTF-8 columns.
    pub position: Position,
}

/// A source of semantic facts about code.
///
/// Answers describe the files as they are on disk now; callers check that
/// a file has not changed since it was indexed before asking about it.
pub trait SemanticProvider {
    /// The version the provider reported about itself, if any.
    fn version(&self) -> Option<String>;

    /// Answers a question whose answer is a list of locations, sorted and
    /// without duplicates. References always include the declaration.
    fn locations(
        &mut self,
        query: LocationQuery,
        at: SourcePoint<'_>,
    ) -> Result<ProviderAnswer<Vec<Location>>, ProviderError>;

    /// What the provider shows for the symbol at a position, such as its
    /// signature and documentation. This is display text, often Markdown,
    /// not a structured type.
    fn hover(&mut self, at: SourcePoint<'_>) -> Result<ProviderAnswer<String>, ProviderError>;

    /// Whether the provider only searches projects it has loaded, so a
    /// project-wide question needs [`SemanticProvider::open_projects`] first.
    fn is_project_scoped(&self) -> bool {
        false
    }

    /// Makes the provider load the projects these files belong to, so a
    /// following project-wide question covers them. Paths are relative to
    /// the provider's root.
    fn open_projects(&mut self, _paths: &[&str]) -> Result<(), ProviderError> {
        Ok(())
    }
}
