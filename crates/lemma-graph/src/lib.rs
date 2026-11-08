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

pub mod adapters;
pub mod address;
mod affects;
mod allowances;
mod anchors;
mod behaviors;
mod check;
pub mod compare;
pub mod compose;
pub mod config;
pub mod exports;
mod functions;
pub mod handlers;
mod impact;
mod infer;
pub mod lm;
pub mod openapi;
pub mod repos;
pub mod schema;
mod trace;
mod warm;

use std::path::{Path, PathBuf};

pub use anchors::{References, Uses};
pub use check::{
    Change, CheckOptions, Finding, Impact, InferredLink, LinkList, Report, ResolvedRepository,
    Severity, check, repository_roots,
};
pub use trace::{Reached, SystemTrace, TraceCaches, trace_through_system};
pub use warm::Warm;

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
    /// A `.lm` manifest does not check.
    #[error("{} has errors:\n  {}", path.display(), errors.join("\n  "))]
    Language {
        /// The entry module's file.
        path: PathBuf,
        /// Each error, as `module:line:column: message`.
        errors: Vec<String>,
    },
    /// A `.lm` manifest says something `lemma check` cannot express yet.
    #[error("cannot use {}", path.display())]
    Lower {
        /// The entry module's file.
        path: PathBuf,
        /// What it cannot express.
        #[source]
        source: lm::LowerError,
    },
    /// A `.lm` program's relations, rules or requirements cannot be lowered
    /// to the grammar inference evaluates.
    #[error("cannot use the requirements of {}", path.display())]
    Grammar {
        /// The entry module's file.
        path: PathBuf,
        /// What it cannot express.
        #[source]
        source: lemma_grammar::lm::LowerError,
    },
    /// `--select` names a check the parent module does not declare, or the
    /// parent is a `lemma.yaml`, which declares none.
    #[error("{module} declares no check `{name}`, and no behavior or requirement is named so")]
    UnknownCheck {
        /// The parent's entry module.
        module: String,
        /// The check asked for.
        name: String,
    },
    /// `--select` names a behavior or requirement by a name several share.
    #[error("`{name}` names several behaviors and requirements: {}; select one by its address", candidates.join(", "))]
    AmbiguousSelection {
        /// The name asked for.
        name: String,
        /// The addresses it could mean.
        candidates: Vec<String>,
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
    /// Nothing tells where a clean checkout left its default branch, so a
    /// change without a base would be empty and check nothing.
    #[error(
        "could not tell where {} left its default branch: no remote's HEAD, main, master, develop or trunk shares its history (a shallow clone has too little); pass --base",
        directory.display()
    )]
    NoForkPoint {
        /// The repository.
        directory: PathBuf,
    },
    /// A commit, branch or tag is not in a repository.
    #[error("`{reference}` is not a commit, branch or tag in {}", directory.display())]
    UnknownReference {
        /// The repository.
        directory: PathBuf,
        /// What was asked for.
        reference: String,
    },
    /// Another Lemma is building an adapter's document in the same
    /// worktree.
    #[error("another Lemma is building in {}; try again when it finishes", directory.display())]
    BuildInProgress {
        /// The worktree.
        directory: PathBuf,
    },
    /// An adapter's configuration cannot be used.
    #[error("operations_from: {message}")]
    Adapter {
        /// What is wrong.
        message: String,
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
    /// An adapter failed on this code. Its output is shortened here and
    /// kept whole in `log`.
    #[error("{summary}\n(full output in {})", log.display())]
    AdapterFailed {
        /// The start and end of what it printed.
        summary: String,
        /// The file holding all of it.
        log: PathBuf,
    },
    /// An adapter failed on this code in an earlier run and was not run
    /// again, since the same code and adapter would fail the same way.
    #[error(
        "{summary}\n(failed the same way on this code before, so not run again; full output in {}; delete it to try again)",
        log.display()
    )]
    AdapterFailedBefore {
        /// The start and end of what it printed then.
        summary: String,
        /// The file holding all of it.
        log: PathBuf,
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
    /// A repository has no `lemma.yaml` where it should, at the commit
    /// read nor in its working directory.
    #[error("{}", missing_manifest(name, path, root, commit.as_deref()))]
    MissingManifest {
        /// The repository's name.
        name: String,
        /// Where its `lemma.yaml` was looked for, relative to its root.
        path: String,
        /// The repository's clone.
        root: PathBuf,
        /// The commit read, if one was; its working directory was read too.
        commit: Option<String>,
    },
    /// The change names a repository the workspace does not compose.
    #[error("the change names `{name}`, which the workspace does not compose")]
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
    /// A file's functions, just parsed, could not be read back.
    #[error("could not read back the functions parsed from {path}")]
    FunctionRecord {
        /// The file.
        path: String,
        /// Why.
        #[source]
        source: lemma_store::RecordError,
    },
}

/// What a missing `lemma.yaml` error says: where it was looked for, and
/// what to do about it.
fn missing_manifest(name: &str, path: &str, root: &Path, commit: Option<&str>) -> String {
    let file = root.join(path);
    let looked = match commit {
        Some(commit) => format!(
            "{name} has no {path} at {}, nor in its working directory ({})",
            commit.chars().take(10).collect::<String>(),
            file.display()
        ),
        None => format!("{name} has no {path} ({})", file.display()),
    };
    format!(
        "{looked}. Add one there saying what the repository serves and calls; \
         a commit from before it existed reads the working directory's (see \
         https://github.com/angrybayblade/lemma/blob/main/docs/check.md)"
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_missing_manifest_says_where_it_was_looked_for_and_what_to_do() {
        let error = GraphError::MissingManifest {
            name: "#billing".to_owned(),
            path: "lemma.yaml".to_owned(),
            root: PathBuf::from("/work/billing"),
            commit: Some("5a4d5fff35cfb1481505".to_owned()),
        };

        let message = error.to_string();

        assert!(message.starts_with(
            "#billing has no lemma.yaml at 5a4d5fff35, nor in its working directory \
             (/work/billing/lemma.yaml). Add one there"
        ));
    }
}
