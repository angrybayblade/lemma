//! The repositories a workspace `lemma.yaml` composes: where each is, which
//! commit it is pinned to, and its own `lemma.yaml`.

use std::path::{Path, PathBuf};

use crate::GraphError;
use crate::check::{self, CheckOptions, HEAD, SELF};
use crate::config::LemmaFile;

/// One repository a workspace composes.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Member {
    /// Its name in the workspace, such as `#billing`.
    pub name: String,
    /// Its local clone.
    pub root: PathBuf,
    /// The commit it is pinned to, or `None` for its working directory.
    pub commit: Option<String>,
    /// Its `lemma.yaml`, at the pin (or the working directory's, for a
    /// commit that has none).
    pub file: LemmaFile,
}

/// A loaded `lemma.yaml` as written: loading adds each documented operation
/// to its service's operations (so links can name it), at its document;
/// those are taken out again.
fn as_written(file: &LemmaFile) -> LemmaFile {
    let mut file = file.clone();
    for service in file.services.values_mut() {
        let documents = service.exports.openapi.clone();
        service
            .exports
            .operations
            .retain(|_, operation| !documents.contains(&operation.path));
    }
    file
}

/// Every repository `workspace` composes, directly or through others, in
/// name order, without the workspace itself. Clones of `git` sources and
/// worktrees go in `cache`.
pub fn members(workspace: &Path, cache: &Path) -> Result<Vec<Member>, GraphError> {
    let options = CheckOptions {
        change: None,
        level: None,
        cache: cache.to_path_buf(),
        facts_cache: None,
        evaluation: lemma_verify::Evaluation::Demand,
        list_links: false,
        run_checkers: true,
        select: None,
    };
    let repos = check::load(workspace, &options)?;
    Ok(repos
        .into_iter()
        .filter(|(name, _)| name != SELF)
        .map(|(name, repo)| {
            let checkout = &repo.checkouts[HEAD];
            Member {
                name,
                root: checkout.root.clone(),
                commit: checkout.commit.clone(),
                file: as_written(&repo.files[HEAD]),
            }
        })
        .collect())
}
