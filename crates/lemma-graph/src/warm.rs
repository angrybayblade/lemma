//! What a long-lived process, such as the daemon's sentry, keeps between
//! checks of the same workspace: what was read from repositories, resolver
//! outputs, and the reads verification keeps (see [`lemma_verify::Warm`]).
//!
//! What was read at a commit never goes stale. What was read from a
//! working directory is dropped with [`Warm::forget_working`] when that
//! directory changes, which the caller, watching the files, decides. A check
//! without a `Warm` reads everything and gives the same answer.

use std::collections::BTreeMap;
use std::path::Path;
use std::sync::{Arc, Mutex, PoisonError};

use crate::schema;

/// Resolver outputs by repository, commit (empty for the working
/// directory) and command.
pub(crate) type Outputs = BTreeMap<(String, String, String), Arc<serde_json::Value>>;

/// The operations one document declares.
pub(crate) type Documented = Arc<BTreeMap<String, crate::config::Operation>>;

/// How many documents' operations are kept.
const DOCUMENTS_KEPT: usize = 64;

/// Reads kept from one check for the next. One check uses it at a time.
#[derive(Default)]
pub struct Warm {
    verify: Arc<lemma_verify::Warm>,
    states: Arc<crate::adapters::States>,
    kept: Mutex<Option<(schema::Reads, Outputs)>>,
    /// The operations of documents, by name and contents: never stale.
    documented: Mutex<BTreeMap<(String, String), Documented>>,
}

/// Two are equal only if they are the same: what they hold is a cache.
impl PartialEq for Warm {
    fn eq(&self, other: &Self) -> bool {
        std::ptr::eq(self, other)
    }
}

impl Eq for Warm {}

impl std::fmt::Debug for Warm {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.write_str("Warm")
    }
}

impl Warm {
    /// What verification keeps.
    pub fn verify(&self) -> Arc<lemma_verify::Warm> {
        Arc::clone(&self.verify)
    }

    /// The state of working directories' code, asked once per repository
    /// by everything a check keys by it.
    pub fn states(&self) -> Arc<crate::adapters::States> {
        Arc::clone(&self.states)
    }

    /// The state of the code in the working directory of the repository at
    /// `repository`, asked of Git now and kept for whoever asks next:
    /// Git's own account of whether a repository nobody watches changed.
    /// `None` outside a Git repository, before its first commit, or when
    /// Git fails.
    pub fn working_state_now(&self, repository: &Path) -> Option<String> {
        self.states.forget(repository);
        self.states.working_state(repository).ok().flatten()
    }

    /// The files kept reads hold by name from the working directory of the
    /// repository at `repository`, sorted: documents a description names
    /// and files resolvers read. Git may ignore any of them, and then its
    /// state does not say whether one changed.
    pub fn files_read_by_name(&self, repository: &Path) -> Vec<String> {
        let mut files = self.verify.documents_read(repository);
        if let Some((reads, _)) = self.lock().as_ref() {
            files.extend(reads.working_files(repository));
        }
        files.sort();
        files.dedup();
        files
    }

    /// Forgets the state of the working directory of the repository at
    /// `repository` alone, after files no check reads changed there: what
    /// checks read of it stays, but a checker's evidence is kept by every
    /// file of the repositories it reads.
    pub fn forget_state(&self, repository: &Path) {
        self.states.forget(repository);
    }

    /// Drops what was read from the working directory of the repository at
    /// `repository`, after its files or its Git state changed.
    pub fn forget_working(&self, repository: &Path) {
        self.verify.forget_working(repository);
        self.states.forget(repository);
        if let Some((reads, outputs)) = self.lock().as_mut() {
            reads.forget_working(repository);
            outputs.retain(|(root, commit, _), _| {
                !commit.is_empty() || !schema::same_path(Path::new(root), repository)
            });
        }
    }

    /// [`Warm::forget_working`] for a change to `changed` alone (relative
    /// to the repository). Only files Git lists are kept past the change:
    /// what else a check read may be ignored by Git and so never reported.
    pub fn forget_paths(&self, repository: &Path, changed: &[String]) {
        self.verify.forget_paths(repository, changed);
        self.states.forget(repository);
        if let Some((reads, outputs)) = self.lock().as_mut() {
            reads.forget_working(repository);
            outputs.retain(|(root, commit, _), _| {
                !commit.is_empty() || !schema::same_path(Path::new(root), repository)
            });
        }
    }

    /// Whether a check can depend on `path` in the repository at
    /// `repository`. Test code and prose are read by no check unless a
    /// description names them, and then the last check has read them; a
    /// schema or adapter command is taken not to depend on them.
    pub fn can_depend_on(&self, repository: &Path, path: &str) -> bool {
        let is_test_code = lemma_http::is_test_file(path) && lemma_verify::is_code_path(path);
        let is_prose = [".md", ".mdx", ".rst"]
            .iter()
            .any(|extension| path.ends_with(extension));
        if !is_test_code && !is_prose {
            return true;
        }
        self.verify.has_read(repository, path)
            || self
                .lock()
                .as_ref()
                .is_some_and(|(reads, _)| reads.has_text(repository, path))
    }

    pub(crate) fn documented(&self, key: &(String, String)) -> Option<Documented> {
        self.documents().get(key).cloned()
    }

    pub(crate) fn keep_documented(&self, key: (String, String), operations: Documented) {
        let mut documents = self.documents();
        if documents.len() >= DOCUMENTS_KEPT {
            documents.clear();
        }
        documents.insert(key, operations);
    }

    fn documents(&self) -> std::sync::MutexGuard<'_, BTreeMap<(String, String), Documented>> {
        self.documented
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
    }

    /// Each interpreter the kept reads asked, with the stamp it had then:
    /// what an answer rests on outside the files watched, since a package
    /// installs under a directory Git ignores.
    pub fn interpreter_stamps(&self) -> BTreeMap<String, String> {
        self.lock()
            .as_ref()
            .map(|(reads, _)| reads.interpreter_stamps())
            .unwrap_or_default()
    }

    /// Whether every interpreter in `stamps` still has its stamp.
    pub fn stamps_hold(stamps: &BTreeMap<String, String>) -> bool {
        stamps
            .iter()
            .all(|(python, seen)| schema::interpreter_stamp(Path::new(python)) == *seen)
    }

    pub(crate) fn take(&self) -> (schema::Reads, Outputs) {
        self.lock().take().unwrap_or_default()
    }

    pub(crate) fn put(&self, reads: schema::Reads, outputs: Outputs) {
        *self.lock() = Some((reads, outputs));
    }

    fn lock(&self) -> std::sync::MutexGuard<'_, Option<(schema::Reads, Outputs)>> {
        self.kept.lock().unwrap_or_else(PoisonError::into_inner)
    }
}
