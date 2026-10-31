//! Answers questions about an indexed snapshot: the operations agents and
//! people use, independent of how they are asked or where facts are kept.
//!
//! Semantic operations go through [`lookup`]: stored answers first, then
//! the language's providers.

mod lookup;
mod references;
mod resolve;
mod search;
mod sessions;

pub use lookup::{Answered, LookupContext, LookupOptions};
pub use references::{ReferenceReport, SymbolReport, find_references};
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
