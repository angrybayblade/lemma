//! Links between operations declared in composable `lemma.yaml` files, and
//! the checks that run over them.
//!
//! Each repository's `lemma.yaml` says what it is (`services`, with their
//! operations and where each operation's request and response schemas come
//! from) and how it relates to others (`graph`). A parent `lemma.yaml`
//! composes repositories as external services (`#name`). For every link
//! (`caller -> provider`), Lemma checks that the link persists, then that
//! the two sides' schemas, resolved to JSON Schema, agree in the direction
//! each message travels.

pub mod address;
mod affects;
mod check;
pub mod compare;
pub mod config;
mod impact;
mod infer;
pub mod openapi;
pub mod repos;
pub mod schema;

use std::path::PathBuf;

pub use check::{
    Change, CheckOptions, Finding, Impact, Report, ResolvedRepository, Severity, check,
};

/// Why a check could not run.
#[derive(Debug, thiserror::Error)]
pub enum GraphError {
    /// A file could not be read.
    #[error("could not read {}", path.display())]
    Io {
        /// The file or directory.
        path: PathBuf,
        /// Why.
        #[source]
        source: std::io::Error,
    },
    /// A `lemma.yaml` is malformed.
    #[error("invalid {}", path.display())]
    Parse {
        /// The file.
        path: PathBuf,
        /// Why.
        #[source]
        source: yaml_serde::Error,
    },
    /// A service in a `lemma.yaml` contradicts its name.
    #[error("invalid {}", path.display())]
    InvalidService {
        /// The file.
        path: PathBuf,
        /// Which service, and why.
        #[source]
        source: config::InvalidService,
    },
    /// A link or address is malformed.
    #[error(transparent)]
    Address(#[from] address::AddressError),
    /// A repository's files could not be read from Git.
    #[error("could not read a repository's files")]
    Source {
        /// Why.
        #[source]
        source: lemma_http::SourceError,
    },
    /// A Git command failed.
    #[error("git failed in {}: {message}", directory.display())]
    Git {
        /// Where.
        directory: PathBuf,
        /// What Git printed.
        message: String,
    },
    /// A commit, branch or tag is not in a repository.
    #[error("`{reference}` is not a commit, branch or tag in {}", directory.display())]
    UnknownReference {
        /// The repository.
        directory: PathBuf,
        /// What was asked for.
        reference: String,
    },
    /// A schema resolver command failed.
    #[error("resolver `{command}` failed in {}: {message}", directory.display())]
    Resolver {
        /// The command.
        command: String,
        /// Where it ran.
        directory: PathBuf,
        /// What it printed.
        message: String,
    },
    /// A resolver's output is not JSON.
    #[error("`{command}` did not produce JSON")]
    ResolverOutput {
        /// The command.
        command: String,
        /// Why.
        #[source]
        source: serde_json::Error,
    },
    /// A schema could not be found where its `lemma.yaml` says.
    #[error("{message}")]
    Schema {
        /// What is missing.
        message: String,
    },
    /// Two schemas could not be compared.
    #[error("could not compare schemas: {message}")]
    Compare {
        /// Why.
        message: String,
    },
    /// A repository has no `lemma.yaml` where it should.
    #[error("{name} has no {path} at {commit}")]
    MissingManifest {
        /// The repository's name.
        name: String,
        /// Where its `lemma.yaml` was looked for.
        path: String,
        /// The commit, or the working directory.
        commit: String,
    },
    /// The change names a repository no `lemma.yaml` declares.
    #[error("the change names `{name}`, which no lemma.yaml declares")]
    UnknownRepository {
        /// The name.
        name: String,
    },
    /// Inferred links could not be found or checked.
    #[error("could not infer links")]
    Inference {
        /// Why.
        #[source]
        source: lemma_verify::VerifyError,
    },
    /// The syntax extractor could not be set up.
    #[error("could not set up the syntax extractor")]
    Syntax {
        /// Why.
        #[source]
        source: lemma_syntax::SyntaxError,
    },
}
