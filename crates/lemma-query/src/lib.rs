//! Answers questions about an indexed snapshot: which symbol a name refers
//! to, and where a symbol is referenced.

mod references;
mod resolve;
mod search;
mod sessions;

pub use references::{
    LookupContext, ReferenceOptions, ReferenceReport, answer_without_provider, find_references,
};
pub use resolve::{SymbolResolution, SymbolSelector, resolve_symbol};
pub use search::SymbolSearch;
pub use sessions::{IdlePolicy, ServerState, SessionPool, SessionStatus, WarmUp};

/// Why a query could not be answered.
#[derive(Debug, thiserror::Error)]
pub enum QueryError {
    /// Reading or writing the index failed.
    #[error("could not access the index")]
    Store(#[from] lemma_store::StoreError),
}
