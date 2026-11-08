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

/// Reads kept from one check for the next. One check uses it at a time.
#[derive(Default)]
pub struct Warm {
    verify: Arc<lemma_verify::Warm>,
    kept: Mutex<Option<(schema::Reads, Outputs)>>,
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

    /// Drops what was read from the working directory of the repository at
    /// `repository`, after its files or its Git state changed.
    pub fn forget_working(&self, repository: &Path) {
        self.verify.forget_working(repository);
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
        if let Some((reads, outputs)) = self.lock().as_mut() {
            reads.forget_working(repository);
            outputs.retain(|(root, commit, _), _| {
                !commit.is_empty() || !schema::same_path(Path::new(root), repository)
            });
        }
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
