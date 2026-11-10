//! What a symbol looks like from outside: its declaration and what its
//! provider says about it.

use std::fs;
use std::path::Path;

use lemma_legacy_store::{FactStore, StoredSymbol};

use crate::QueryError;
use crate::lookup::{Answered, Hover, LookupContext, ask_providers, is_stale};
use crate::references::name_of;
use crate::sessions::SessionPool;

/// A symbol's signature, from two sources kept apart: the declaration as
/// written, and the provider's display text, which may add inferred types
/// and documentation.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SignatureReport {
    /// The symbol asked about.
    pub symbol: StoredSymbol,
    /// The definition's header as written in the source, without its body.
    /// `None` when the file changed since it was indexed.
    pub declaration: Option<String>,
    /// The provider's hover text, usually Markdown. Display text only; it
    /// is not parsed into types.
    pub hover: Answered<String>,
}

/// Describes a symbol's signature.
pub fn signature_of(
    store: &mut dyn FactStore,
    context: &LookupContext<'_>,
    symbol: StoredSymbol,
    pool: &mut SessionPool,
) -> Result<SignatureReport, QueryError> {
    let declaration = declaration(&*store, context, &symbol)?;
    let hover = ask_providers(store, context, name_of(&symbol), pool, &Hover)?;
    Ok(SignatureReport {
        symbol,
        declaration,
        hover,
    })
}

/// The declaration as written, needing no provider.
fn declaration(
    store: &dyn FactStore,
    context: &LookupContext<'_>,
    symbol: &StoredSymbol,
) -> Result<Option<String>, QueryError> {
    let root = Path::new(&context.snapshot.snapshot.root);
    if is_stale(store, context.snapshot.id, root, &symbol.path)? {
        return Ok(None);
    }
    let Ok(source) = fs::read_to_string(root.join(&symbol.path)) else {
        return Ok(None);
    };
    Ok(lemma_syntax::declaration_text(
        symbol.language,
        &source,
        symbol.symbol.span,
    )?)
}
