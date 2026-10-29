//! Recording which Git commit a snapshot was taken from.

use std::path::Path;
use std::process::Command;

/// The repository state at indexing time.
pub(crate) struct GitState {
    /// `HEAD`, or `None` outside a repository or before the first commit.
    pub revision: Option<String>,
    /// Whether tracked or untracked files differ from `HEAD`.
    pub is_dirty: bool,
}

/// Reads the current commit and whether the tree is dirty. Directories that
/// are not Git repositories report no revision and a clean tree.
pub(crate) fn git_state(root: &Path) -> GitState {
    let revision = git_output(root, &["rev-parse", "HEAD"]);
    // Lemma's own index directory is not a change to the code.
    let changes = git_output(root, &["status", "--porcelain", "--", ".", ":!.lemma"]);
    GitState {
        revision,
        is_dirty: changes.is_some_and(|changes| !changes.is_empty()),
    }
}

fn git_output(root: &Path, args: &[&str]) -> Option<String> {
    let output = Command::new("git")
        .arg("-C")
        .arg(root)
        .args(args)
        .output()
        .ok()?;
    if !output.status.success() {
        return None;
    }
    Some(String::from_utf8_lossy(&output.stdout).trim().to_owned())
}
