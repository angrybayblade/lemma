//! The Lemma store (spec 17): one `index.ls` file per repository, holding
//! each source's files and symbols as a tree of immutable nodes.
//!
//! The file only ever grows. A commit appends the nodes it changed, then a
//! footer naming the new roots, so a reader takes the last footer whose
//! digest checks and never needs a lock. Sources that share files share
//! the nodes for them, and comparing two sources descends only where their
//! digests differ.

mod codec;
mod read;
mod scan;
mod write;

#[cfg(test)]
mod tests;

use std::path::PathBuf;

use lemma_model::{Language, Span, SymbolKind};

pub use read::{
    FileChange, Index, ReadMode, Root, SymbolChange, SymbolChangeKind, changed_symbols,
};
pub use scan::{ScanError, changes_on_disk, read_file};
pub use write::{Commit, Writer};

/// A BLAKE3 digest.
pub type Digest = [u8; 32];

/// Why a store could not be used.
#[derive(Debug, thiserror::Error)]
pub enum StoreError {
    /// The file could not be read or written.
    #[error("cannot use store {path}")]
    Io {
        /// The store.
        path: PathBuf,
        /// What went wrong.
        #[source]
        source: std::io::Error,
    },
    /// The file is not a store.
    #[error("{path} is not a Lemma store")]
    NotAStore {
        /// The file.
        path: PathBuf,
    },
    /// The store was written in a layout this build does not read.
    #[error("store layout {found} is not supported (expected {expected})")]
    UnsupportedVersion {
        /// The layout written.
        found: u16,
        /// The layout this build reads.
        expected: u16,
    },
    /// Stored bytes fail a check.
    #[error("store is damaged: {what}")]
    Damaged {
        /// What failed.
        what: &'static str,
    },
    /// No source has this name.
    #[error("store has no root named {name}")]
    UnknownRoot {
        /// The name asked for.
        name: String,
    },
    /// Another process is writing.
    #[error("store {path} is being written by another process")]
    Busy {
        /// The store.
        path: PathBuf,
    },
}

/// What a source is.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SourceRecord {
    /// The repository's name.
    pub repository: String,
    /// The commit, or a name for a working directory.
    pub revision: String,
    /// A digest of everything that determines how files were read.
    pub profile: Digest,
}

/// One symbol as stored.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SymbolRecord {
    /// The identifier as written.
    pub name: String,
    /// The name prefixed by its containers.
    pub qualified_name: String,
    /// What kind of definition it is.
    pub kind: SymbolKind,
    /// The whole definition.
    pub span: Span,
    /// Just the identifier.
    pub name_span: Span,
    /// Its position among definitions in the file with the same name and
    /// kind, so that repeated names have distinct identities.
    pub discriminator: u32,
    /// A digest of its syntax, without comments or positions.
    pub digest: Digest,
    /// The identifiers in its code, sorted.
    pub mentions: Vec<String>,
}

/// One file as stored.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FileRecord {
    /// A digest of the file's contents.
    pub blob: Digest,
    /// The language it was parsed as.
    pub language: Language,
    /// Whether the parser recovered from syntax errors.
    pub has_syntax_errors: bool,
    /// Its definitions in source order.
    pub symbols: Vec<SymbolRecord>,
}

/// A difference to apply to a source's files.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Change {
    /// The file at this path now holds this.
    Put {
        /// Relative to the repository, with `/` separators.
        path: String,
        /// Its contents.
        record: FileRecord,
    },
    /// The file at this path is gone.
    Remove {
        /// Relative to the repository, with `/` separators.
        path: String,
    },
}

impl Change {
    fn path(&self) -> &str {
        match self {
            Self::Put { path, .. } | Self::Remove { path } => path,
        }
    }
}
