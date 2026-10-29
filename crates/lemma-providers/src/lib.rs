//! Pluggable language servers that supply semantic facts.
//!
//! Providers are declared as data: a command, the languages it serves, and the
//! settings it needs. Adding a server means adding a configuration entry, not
//! code. This crate is also the boundary where LSP shapes become Lemma types;
//! nothing beyond it sees `lsp_types`.

mod registry;
mod session;

use std::path::PathBuf;

pub use registry::{InstalledProvider, ProviderDefinition, ProviderRegistry, resolve_executable};
pub use session::{ProviderSession, ReferenceAnswer};

/// Why a provider could not be configured or could not answer.
#[derive(Debug, thiserror::Error)]
pub enum ProviderError {
    /// A provider configuration file could not be read.
    #[error("could not read provider configuration {}", path.display())]
    ReadConfig {
        /// The configuration file.
        path: PathBuf,
        /// The underlying failure.
        #[source]
        source: std::io::Error,
    },
    /// A provider configuration file is not valid.
    #[error("invalid provider configuration in {origin}")]
    ParseConfig {
        /// Where the configuration came from.
        origin: String,
        /// What was wrong with it.
        #[source]
        source: toml::de::Error,
    },
    /// A source file needed for a request could not be read.
    #[error("could not read {}", path.display())]
    ReadSource {
        /// The file.
        path: PathBuf,
        /// The underlying failure.
        #[source]
        source: std::io::Error,
    },
    /// A path cannot be expressed as a `file://` URI.
    #[error("{} cannot be expressed as a file URI", path.display())]
    InvalidPath {
        /// The offending path.
        path: PathBuf,
    },
    /// The language server failed.
    #[error("provider `{provider}` failed")]
    Server {
        /// The provider id.
        provider: String,
        /// What went wrong in the conversation with the server.
        #[source]
        source: lemma_lsp::LspError,
    },
}
