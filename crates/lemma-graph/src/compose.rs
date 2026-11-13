//! The repositories a workspace description composes: where each is, which
//! commit it is pinned to, and its own the description.

use std::path::{Path, PathBuf};

use crate::GraphError;
use crate::check::{self, CheckOptions, HEAD, SELF};
use crate::config::LemmaFile;
use crate::repos::{Cloning, UnavailableRepository};

/// One repository a workspace composes.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Member {
    /// Its name in the workspace, such as `#billing`.
    pub name: String,
    /// Its local clone.
    pub root: PathBuf,
    /// The commit it is pinned to, or `None` for its working directory.
    pub commit: Option<String>,
    /// Its description, at the pin (or the working directory's, for a
    /// commit that has none).
    pub file: LemmaFile,
}

/// What a workspace composes, as far as it can be read here.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Composition {
    /// The repositories that are here, in name order.
    pub members: Vec<Member>,
    /// The repositories named but not cloned, which `cloning` kept out.
    pub unavailable: Vec<UnavailableRepository>,
}

/// Every repository `workspace` composes, directly or through others, in
/// name order, without the workspace itself. Clones of `git` sources and
/// worktrees go in `cache`, as `cloning` allows.
pub fn composition(
    workspace: &Path,
    cache: &Path,
    cloning: Cloning,
) -> Result<Composition, GraphError> {
    let options = CheckOptions {
        change: None,
        level: None,
        cache: cache.to_path_buf(),
        cloning,
        explain: false,
        relations: Vec::new(),
        facts_cache: None,
        evaluation: lemma_verify::Evaluation::Demand,
        list_links: false,
        run_checkers: true,
        runs_base_checkers: false,
        select: None,
        warm: None,
        environment: None,
        runtime: None,
    };
    let repos = check::load_manifests(workspace, &options)?;
    let unavailable = check::unavailable_repositories(&repos)
        .into_iter()
        .collect();
    let members = repos
        .into_iter()
        .filter(|(name, _)| name != SELF)
        .map(|(name, repo)| {
            let checkout = &repo.checkouts[HEAD];
            Member {
                name,
                root: checkout.root.clone(),
                commit: checkout.commit.clone(),
                file: repo.files[HEAD].clone(),
            }
        })
        .collect();
    Ok(Composition {
        members,
        unavailable,
    })
}
