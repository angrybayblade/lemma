//! Answers questions about an indexed snapshot: which symbol a name refers
//! to, and where a symbol is referenced.

mod references;
mod resolve;

pub use references::{ReferenceOptions, ReferenceReport, find_references};
pub use resolve::{SymbolResolution, SymbolSelector, resolve_symbol};

/// Why a query could not be answered.
#[derive(Debug, thiserror::Error)]
pub enum QueryError {
    /// Reading or writing the index failed.
    #[error("could not access the index")]
    Store(#[from] lemma_store::StoreError),
}
