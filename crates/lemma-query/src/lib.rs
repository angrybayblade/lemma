//! Answers questions about an indexed snapshot: the operations agents and
//! people use, independent of how they are asked or where facts are kept.
//!
//! Syntax-only operations read the store. Semantic ones go through
//! [`lookup`]: stored answers first, then the language's providers.

mod lookup;
mod navigation;
mod references;
mod resolve;
mod search;
mod sessions;
mod signature;

pub use lookup::{Answered, LookupContext, LookupOptions};
pub use navigation::{
    DefinitionReport, ImplementationReport, Target, find_definitions, find_implementations,
};
pub use references::{ReferenceReport, SymbolReport, find_references};
pub use resolve::{SymbolResolution, SymbolSelector, resolve_symbol, symbols_in_file};
pub use search::SymbolSearch;
pub use sessions::{IdlePolicy, ServerState, SessionPool, SessionStatus, WarmUp};
pub use signature::{SignatureReport, signature_of};

/// Why a query could not be answered.
#[derive(Debug, thiserror::Error)]
pub enum QueryError {
    /// Reading or writing the index failed.
    #[error("could not access the index")]
    Store(#[from] lemma_store::StoreError),
    /// A file's language is not one Lemma supports.
    #[error("{path} is not a TypeScript, Python, or Go file")]
    UnsupportedFile {
        /// The file, relative to the snapshot root.
        path: String,
    },
    /// The syntax parser could not be set up or gave up on a file.
    #[error("could not parse the source")]
    Syntax(#[from] lemma_syntax::SyntaxError),
}
