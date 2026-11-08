//! What a long-lived process, such as the daemon's sentry, keeps between
//! verifications of the same repositories, so a run reads again only what
//! changed: listings, documents parsed, files read and facts decoded.
//!
//! What was read at a commit, and facts keyed by their file's contents,
//! never go stale. What was read from a working directory is dropped with
//! [`Warm::forget_working`] when that directory changes; the caller, which
//! watches the files, decides when. A run without a `Warm` reads everything,
//! as before, and gives the same answer: that is the comparison the warm
//! path is tested against.

use std::path::Path;
use std::sync::{Arc, Mutex, PoisonError};

use crate::extract::Memo;

/// Reads kept from one verification for the next. One run uses it at a
/// time: a run takes what is kept and puts back what it read.
#[derive(Default)]
pub struct Warm {
    listings: Arc<lemma_http::Listings>,
    memo: Mutex<Option<Memo>>,
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
    /// The repositories' file listings, shared by every run.
    pub fn listings(&self) -> Arc<lemma_http::Listings> {
        Arc::clone(&self.listings)
    }

    /// Drops what was read from the working directory of the repository at
    /// `repository`, after its files changed.
    pub fn forget_working(&self, repository: &Path) {
        self.listings.forget_working(repository);
        if let Some(memo) = self.lock().as_mut() {
            memo.forget_working(repository);
        }
    }

    /// [`Warm::forget_working`] for a change to `changed` alone (relative
    /// to the repository, files or directories): what was read of its
    /// other files stays.
    pub fn forget_paths(&self, repository: &Path, changed: &[String]) {
        self.listings.forget_working(repository);
        if let Some(memo) = self.lock().as_mut() {
            memo.forget_paths(repository, changed);
        }
    }

    /// Whether the last run read `file` from the working directory of the
    /// repository at `repository`.
    pub fn has_read(&self, repository: &Path, file: &str) -> bool {
        self.lock()
            .as_ref()
            .is_some_and(|memo| memo.has_read(repository, file))
    }

    /// Gets the repository at `repository` ready for the next run, after
    /// `changed` changed: lists its working directory and parses the code
    /// among `changed` as the last run read it. Nothing is evaluated. How
    /// many files' facts are now known.
    pub fn prepare(&self, repository: &Path, changed: &[String]) -> usize {
        let Ok(listing) = self.listings.get(repository, None) else {
            return 0;
        };
        let Some(mut memo) = self.lock().take() else {
            return 0;
        };
        let files: Vec<lemma_http::ListedFile> = listing
            .iter()
            .filter(|file| crate::extract::is_code_path(&file.path))
            .filter(|file| {
                changed
                    .iter()
                    .any(|one| crate::extract::is_at_or_below(&file.path, one))
            })
            .cloned()
            .collect();
        let parsed = memo.parse_changed(repository, &files);
        self.put(memo);
        parsed
    }

    pub(crate) fn take(&self) -> Memo {
        self.lock().take().unwrap_or_default()
    }

    pub(crate) fn put(&self, memo: Memo) {
        *self.lock() = Some(memo);
    }

    fn lock(&self) -> std::sync::MutexGuard<'_, Option<Memo>> {
        self.memo.lock().unwrap_or_else(PoisonError::into_inner)
    }
}

/// Whether two paths name the same directory, comparing them as written
/// and, failing that, resolved: a manifest and a watcher may spell one
/// repository differently.
pub(crate) fn same_path(left: &Path, right: &Path) -> bool {
    left == right
        || left
            .canonicalize()
            .ok()
            .zip(right.canonicalize().ok())
            .is_some_and(|(left, right)| left == right)
}
