//! A workspace: the directory one daemon serves, and the checkouts in it.
//!
//! A workspace is a Git checkout, or the directory of a root module that
//! declares a `workspace` with the repositories it names
//! (`.specs/21-workspace-daemons.md`). A daemon serves a directory only when
//! the Git checkout that directory is in is one of the workspace's own, so
//! a worktree placed inside a checkout is never answered about the files of
//! the checkout around it.

use std::path::{Path, PathBuf};

/// The file a directory's module is written in.
pub(crate) const ROOT_MODULE: &str = "mod.lm";

/// One workspace: its root, and the working directories of the
/// repositories its root module names.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct Workspace {
    root: PathBuf,
    members: Vec<PathBuf>,
}

impl Workspace {
    /// The workspace rooted at `root`, with `members` the repositories its
    /// root module names. Paths are compared as given, so pass them
    /// canonical.
    pub(crate) fn at(root: PathBuf, members: Vec<PathBuf>) -> Self {
        Self { root, members }
    }

    /// The workspace `directory` belongs to, where `members_of` lists the
    /// repositories a root module names.
    ///
    /// It is the nearest directory above with a root module that declares
    /// a workspace serving `directory`; otherwise `directory`'s Git
    /// checkout; otherwise `directory` itself. A workspace above that does
    /// not name the checkout `directory` is in does not count, which keeps
    /// a nested worktree out of the workspace around it.
    pub(crate) fn of(directory: &Path, members_of: &dyn Fn(&Path) -> Vec<PathBuf>) -> Self {
        for above in directory.ancestors() {
            let manifest = above.join(ROOT_MODULE);
            if !module_declares_workspace(&manifest) {
                continue;
            }
            let workspace = Self::at(above.to_path_buf(), members_of(&manifest));
            if workspace.serves(directory) {
                return workspace;
            }
        }
        let root = checkout_of(directory).unwrap_or(directory);
        Self::at(root.to_path_buf(), Vec::new())
    }

    /// The workspace rooted at `root`, whatever is above it: with the
    /// repositories its root module names, if it declares a workspace.
    pub(crate) fn rooted_at(root: PathBuf, members_of: &dyn Fn(&Path) -> Vec<PathBuf>) -> Self {
        let manifest = root.join(ROOT_MODULE);
        let members = if module_declares_workspace(&manifest) {
            members_of(&manifest)
        } else {
            Vec::new()
        };
        Self::at(root, members)
    }

    /// The directory the workspace's daemon is started in.
    pub(crate) fn root(&self) -> &Path {
        &self.root
    }

    /// Whether `directory` is the workspace's to answer about: it is in a
    /// repository the root module names, or under the root in the root's
    /// own checkout.
    pub(crate) fn serves(&self, directory: &Path) -> bool {
        let checkout = checkout_of(directory);
        if checkout.is_some_and(|checkout| self.members.iter().any(|member| member == checkout)) {
            return true;
        }
        directory.starts_with(&self.root) && checkout == checkout_of(&self.root)
    }

    /// The repositories the root module names that are not under the root.
    /// Commands run in them walk up without reaching the workspace's
    /// daemon.
    pub(crate) fn members_outside(&self) -> Vec<&Path> {
        self.members
            .iter()
            .filter(|member| !member.starts_with(&self.root))
            .map(PathBuf::as_path)
            .collect()
    }
}

/// The working directories of the repositories the workspace module
/// `manifest` names, as far as they are here: nothing is cloned to find
/// out. A module that does not load names none, so its workspace serves
/// its own checkout alone until it does.
pub(crate) fn named_repositories(manifest: &Path) -> Vec<PathBuf> {
    let cache = lemma_graph::repos::cache_directory();
    let cloning = lemma_graph::repos::Cloning::Forbidden;
    match lemma_graph::compose::composition(manifest, &cache, cloning) {
        Ok(composition) => composition
            .members
            .into_iter()
            .filter(|member| member.commit.is_none())
            .map(|member| member.root.canonicalize().unwrap_or(member.root))
            .collect(),
        Err(error) => {
            log::warn!(
                "{} does not load, so its repositories are not known: {error}",
                manifest.display()
            );
            Vec::new()
        }
    }
}

/// The Git checkout `directory` is in: the nearest directory at or above it
/// that has a `.git`, which is a file in a worktree or a submodule.
pub(crate) fn checkout_of(directory: &Path) -> Option<&Path> {
    directory
        .ancestors()
        .find(|above| above.join(".git").exists())
}

/// The module a check of the code in `folder` starts from: the nearest
/// root module at or above it that declares a workspace, or else the
/// nearest root module.
pub(crate) fn entry_module_above(folder: &Path) -> Option<PathBuf> {
    let found: Vec<PathBuf> = folder
        .ancestors()
        .map(|directory| directory.join(ROOT_MODULE))
        .filter(|manifest| manifest.is_file())
        .collect();
    found
        .iter()
        .find(|manifest| module_declares_workspace(manifest))
        .or_else(|| found.first())
        .cloned()
}

/// Whether the module in the file `manifest` declares a workspace; `false`
/// when the file cannot be read.
fn module_declares_workspace(manifest: &Path) -> bool {
    std::fs::read_to_string(manifest).is_ok_and(|text| declares_workspace(&text))
}

/// Whether a module's text declares a `workspace { ... }` block.
pub(crate) fn declares_workspace(text: &str) -> bool {
    text.lines().any(|line| {
        let line = line.trim_start();
        line.strip_prefix("workspace")
            .is_some_and(|rest| rest.trim_start().starts_with('{'))
    })
}

#[cfg(test)]
mod tests {
    use std::fs;

    use super::*;

    /// A directory tree for a test: `checkouts` get a `.git`, and
    /// `workspaces` a root module that declares a workspace.
    fn tree(checkouts: &[&str], workspaces: &[&str]) -> tempfile::TempDir {
        let directory = tempfile::tempdir().unwrap();
        for checkout in checkouts {
            fs::create_dir_all(directory.path().join(checkout).join(".git")).unwrap();
        }
        for workspace in workspaces {
            let inside = directory.path().join(workspace);
            fs::create_dir_all(&inside).unwrap();
            fs::write(inside.join(ROOT_MODULE), "module w\n\nworkspace {\n}\n").unwrap();
        }
        directory
    }

    fn no_members(_manifest: &Path) -> Vec<PathBuf> {
        Vec::new()
    }

    #[test]
    fn a_directory_in_a_checkout_belongs_to_the_checkout() {
        let tree = tree(&["app"], &[]);
        let inside = tree.path().join("app/src/api");
        fs::create_dir_all(&inside).unwrap();

        let workspace = Workspace::of(&inside, &no_members);

        assert_eq!(workspace.root(), tree.path().join("app"));
    }

    #[test]
    fn a_directory_in_no_checkout_is_its_own_workspace() {
        let tree = tree(&[], &[]);

        let workspace = Workspace::of(tree.path(), &no_members);

        assert_eq!(workspace.root(), tree.path());
    }

    #[test]
    fn a_repository_the_root_module_names_belongs_to_its_workspace() {
        let tree = tree(&["system", "system/repos/billing"], &["system"]);
        let billing = tree.path().join("system/repos/billing");
        let named = billing.clone();
        let members = move |_: &Path| vec![named.clone()];

        let workspace = Workspace::of(&billing.join("src"), &members);

        assert_eq!(workspace.root(), tree.path().join("system"));
    }

    #[test]
    fn a_checkout_the_root_module_does_not_name_is_its_own_workspace() {
        let tree = tree(&["system", "system/vendor/tool"], &["system"]);
        let tool = tree.path().join("system/vendor/tool");

        let workspace = Workspace::of(&tool, &no_members);

        assert_eq!(workspace.root(), tool);
    }

    #[test]
    fn a_worktree_inside_a_checkout_is_not_served_by_the_checkouts_workspace() {
        let tree = tree(&["app"], &[]);
        let app = tree.path().join("app");
        let worktree = app.join(".claude/worktrees/task");
        fs::create_dir_all(&worktree).unwrap();
        fs::write(worktree.join(".git"), "gitdir: elsewhere\n").unwrap();
        let workspace = Workspace::at(app.clone(), Vec::new());

        assert!(workspace.serves(&app.join("src")));
        assert!(!workspace.serves(&worktree));
        assert_eq!(Workspace::of(&worktree, &no_members).root(), worktree);
    }

    #[test]
    fn a_workspace_does_not_serve_a_directory_outside_it() {
        let tree = tree(&["app", "other"], &[]);
        let workspace = Workspace::at(tree.path().join("app"), Vec::new());

        assert!(!workspace.serves(&tree.path().join("other")));
        assert!(!workspace.serves(tree.path()));
    }

    #[test]
    fn a_repository_named_from_outside_the_root_is_served_and_listed() {
        let tree = tree(&["system", "billing"], &["system"]);
        let billing = tree.path().join("billing");
        let workspace = Workspace::at(tree.path().join("system"), vec![billing.clone()]);

        assert!(workspace.serves(&billing));
        assert_eq!(workspace.members_outside(), [billing.as_path()]);
    }

    #[test]
    fn only_a_workspace_block_declares_a_workspace() {
        assert!(declares_workspace("module shop\n\nworkspace {\n}\n"));
        assert!(declares_workspace("  workspace{"));
        assert!(!declares_workspace("module shop\n// a workspace of one\n"));
        assert!(!declares_workspace("export service workspaces {\n}\n"));
    }
}
