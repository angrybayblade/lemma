//! What verification reads: which repositories make up the system, at which
//! commits, what role each plays, and which change is being verified.
//! `lemma check` builds one from a description.

use std::collections::BTreeMap;
use std::path::PathBuf;

/// What a repository is to the system.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Role {
    /// Serves operations, described by specs and route code.
    Service,
    /// A generated client: functions generated from a service's spec.
    Client,
    /// Calls operations, directly or through a client.
    Consumer,
}

/// One repository.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Repository {
    /// Its clone.
    pub path: PathBuf,
    /// The commit to read, unless it is the changed repository. Without
    /// one, the working directory is read.
    pub commit: Option<String>,
    /// What it is to the system.
    pub role: Role,
    /// A service's name, used in findings; the repository's name if absent.
    /// On a consumer, the service its code belongs to: its references to
    /// that service's own operations, such as route declarations, are not
    /// calls.
    pub service: Option<String>,
    /// `OpenAPI` documents: a service's contract, or a client's copy of it.
    pub specs: Vec<String>,
    /// Documents built from a service's code by an adapter, replacing
    /// `specs` where a snapshot is read: by `base`, `head`, or `both` for a
    /// repository read once.
    pub generated: BTreeMap<String, Vec<GeneratedDocument>>,
    /// Whether a service rejects requests with fields it does not know, as
    /// a strict schema does, even where its spec does not say so.
    pub rejects_unknown_fields: bool,
    /// Directories with a service's route code.
    pub routes: Vec<String>,
    /// Further directories with schemas a service's routes import, such as
    /// shared zod schemas, read to tell which reject unknown keys.
    pub schemas: Vec<String>,
    /// `OpenAPI` documents a client is generated from, with one function per
    /// operation named after its `operationId` (see `call_names`).
    pub generated_from: Vec<String>,
    /// How a generated client names an operation's function, from its
    /// `operationId`: templates with `{operationId}`, `{camel}`, `{pascal}`
    /// or `{snake}`, such as `{camel}` or `{pascal}.Invoke`. The
    /// `operationId` as written if empty.
    pub call_names: Vec<String>,
    /// Directories with a consumer's code; all of it if empty.
    pub include: Vec<String>,
}

/// The change being verified: one repository at two commits.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Change {
    /// The changed repository.
    pub repository: String,
    /// The commit before the change.
    pub base: String,
    /// The commit with the change; the working directory, uncommitted
    /// changes included, when absent.
    pub head: Option<String>,
}

impl Change {
    /// The head commit, or the working directory's name for it, for
    /// showing to a person.
    pub fn head_label(&self) -> &str {
        self.head.as_deref().unwrap_or("working directory")
    }
}

/// A document built from a service's code, standing in for one of its
/// committed documents.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct GeneratedDocument {
    /// The committed document it stands for, so both snapshots of a change
    /// name the same document whichever file each was written to.
    pub name: String,
    /// Where it was written.
    pub path: PathBuf,
}

/// What to verify.
#[derive(Debug, Clone, PartialEq)]
pub struct Manifest {
    /// The change.
    pub change: Change,
    /// Every repository, by name.
    pub repositories: BTreeMap<String, Repository>,
    /// The grammar a description's `.lm` program lowers to; the built-in
    /// `lemma.http` when `None`.
    pub lowered: Option<lemma_grammar::GrammarFile>,
}
