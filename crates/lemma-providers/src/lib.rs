//! Pluggable language servers that supply semantic facts.
//!
//! Providers are declared as data: a command, the languages it serves, and the
//! settings it needs. Adding a server means adding a configuration entry, not
//! code. This crate is also the boundary where LSP shapes become Lemma types;
//! nothing beyond it sees `lsp_types`.

mod languages;
mod registry;
mod semantic;
mod session;

use std::path::PathBuf;

pub use lemma_lsp::Interrupt;
pub use registry::{InstalledProvider, ProviderDefinition, ProviderRegistry};
pub use semantic::{SemanticProvider, SourcePoint};
pub use session::{FileChange, ProviderSession};

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
    /// The provider could not start a moment ago and was not started
    /// again: what was wrong then, in the words it was reported in.
    #[error("{message}")]
    CouldNotStart {
        /// The earlier failure and its causes.
        message: String,
    },
}

impl ProviderError {
    /// Whether a request was given up on because the caller was told to
    /// stop waiting, rather than because the server failed.
    pub fn is_interrupted(&self) -> bool {
        matches!(
            self,
            Self::Server {
                source: lemma_lsp::LspError::Interrupted { .. },
                ..
            }
        )
    }

    /// Whether the server was asked and did not answer in the time the
    /// caller allowed: it may be well, and too slow for this question.
    pub fn is_timeout(&self) -> bool {
        matches!(
            self,
            Self::Server {
                source: lemma_lsp::LspError::Timeout { .. },
                ..
            }
        )
    }
}

#[cfg(test)]
mod tests {
    use std::time::Duration;

    use lemma_lsp::LspError;

    use super::*;

    fn failed(source: LspError) -> ProviderError {
        ProviderError::Server {
            provider: "pyright".to_owned(),
            source,
        }
    }

    #[test]
    fn only_a_server_that_did_not_answer_in_time_timed_out() {
        let slow = failed(LspError::Timeout {
            method: "textDocument/references",
            timeout: Duration::from_secs(60),
        });
        let gone = failed(LspError::Closed);
        let told_to_stop = failed(LspError::Interrupted {
            method: "textDocument/references",
        });

        assert!(slow.is_timeout());
        assert!(!gone.is_timeout());
        assert!(!told_to_stop.is_timeout());
    }
}
