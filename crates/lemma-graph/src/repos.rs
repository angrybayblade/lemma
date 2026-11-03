//! Repositories: where each one is, which commit is read, and the files and
//! worktrees at that commit.
//!
//! A `path` source is a local clone; a `git` source is cloned into Lemma's
//! cache on first use. A commit is read from Git without a checkout; a
//! worktree is created only when a resolver has to run code at a commit.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::process::Command;

use crate::GraphError;

/// A repository as read for one snapshot.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Checkout {
    /// The local clone.
    pub root: PathBuf,
    /// The commit read, or `None` for the working directory.
    pub commit: Option<String>,
}

impl Checkout {
    /// A file's text at this checkout, or `None` if it does not exist.
    pub fn read(&self, path: &str) -> Result<Option<String>, GraphError> {
        lemma_http::read_file(&self.root, self.commit.as_deref(), path)
            .map_err(|source| GraphError::Source { source })
    }

    /// A directory holding the repository's files at this checkout: the
    /// clone itself for the working directory, otherwise a worktree in
    /// `cache`, created once per commit.
    pub fn worktree(&self, cache: &Path) -> Result<PathBuf, GraphError> {
        let Some(commit) = &self.commit else {
            return Ok(self.root.clone());
        };
        let name = format!("{}-{commit}", directory_name(&self.root));
        let directory = cache.join("worktrees").join(name);
        if directory.exists() {
            return Ok(directory);
        }
        std::fs::create_dir_all(cache.join("worktrees")).map_err(|source| GraphError::Io {
            path: cache.to_owned(),
            source,
        })?;
        let target = directory.to_string_lossy().into_owned();
        git(
            &self.root,
            &["worktree", "add", "--detach", "--quiet", &target, commit],
        )?;
        Ok(directory)
    }
}

/// Removes a worktree of the clone at `root`, made by [`Checkout::worktree`].
pub fn remove_worktree(root: &Path, worktree: &Path) -> Result<(), GraphError> {
    let target = worktree.to_string_lossy().into_owned();
    git(root, &["worktree", "remove", "--force", &target])?;
    Ok(())
}

/// The commit a reference names in a clone.
pub fn resolve_commit(root: &Path, reference: &str) -> Result<String, GraphError> {
    let spec = format!("{reference}^{{commit}}");
    let output = git(root, &["rev-parse", "--verify", "--quiet", &spec]).map_err(|_| {
        GraphError::UnknownReference {
            directory: root.to_owned(),
            reference: reference.to_owned(),
        }
    })?;
    Ok(output.trim().to_owned())
}

/// The local clone of a `git` source, cloned into `cache` on first use and
/// fetched when `reference` is not there yet.
pub fn clone_remote(
    url: &str,
    reference: Option<&str>,
    cache: &Path,
) -> Result<PathBuf, GraphError> {
    let directory = cache.join("repos").join(sanitize(url));
    if !directory.exists() {
        std::fs::create_dir_all(cache.join("repos")).map_err(|source| GraphError::Io {
            path: cache.to_owned(),
            source,
        })?;
        let target = directory.to_string_lossy().into_owned();
        git(cache, &["clone", "--quiet", "--no-checkout", url, &target])?;
    }
    let wanted = reference.unwrap_or("HEAD");
    if resolve_commit(&directory, wanted).is_err() {
        git(&directory, &["fetch", "--quiet", "origin", wanted])?;
    }
    Ok(directory)
}

/// Runs a command in `directory` with `sh -c`, returning its standard output.
pub fn run(
    directory: &Path,
    command: &str,
    env: &BTreeMap<String, String>,
) -> Result<String, GraphError> {
    let output = Command::new("sh")
        .arg("-c")
        .arg(command)
        .current_dir(directory)
        .envs(env)
        .output()
        .map_err(|source| GraphError::Io {
            path: directory.to_owned(),
            source,
        })?;
    if !output.status.success() {
        return Err(GraphError::Resolver {
            command: command.to_owned(),
            directory: directory.to_owned(),
            message: String::from_utf8_lossy(&output.stderr).trim().to_owned(),
        });
    }
    Ok(String::from_utf8_lossy(&output.stdout).into_owned())
}

/// Lemma's cache directory: `$XDG_CACHE_HOME/lemma` or `~/.cache/lemma`.
pub fn cache_directory() -> PathBuf {
    if let Some(cache) = std::env::var_os("XDG_CACHE_HOME") {
        return PathBuf::from(cache).join("lemma");
    }
    let home = std::env::var_os("HOME").map_or_else(|| PathBuf::from("."), PathBuf::from);
    home.join(".cache").join("lemma")
}

fn git(directory: &Path, args: &[&str]) -> Result<String, GraphError> {
    let output = Command::new("git")
        .arg("-C")
        .arg(directory)
        .args(args)
        .output()
        .map_err(|source| GraphError::Io {
            path: directory.to_owned(),
            source,
        })?;
    if !output.status.success() {
        return Err(GraphError::Git {
            directory: directory.to_owned(),
            message: String::from_utf8_lossy(&output.stderr).trim().to_owned(),
        });
    }
    Ok(String::from_utf8_lossy(&output.stdout).into_owned())
}

fn directory_name(path: &Path) -> String {
    sanitize(&path.to_string_lossy())
}

/// A string as a safe directory name.
fn sanitize(text: &str) -> String {
    text.chars()
        .map(|c| {
            if c.is_ascii_alphanumeric() || c == '-' || c == '.' {
                c
            } else {
                '_'
            }
        })
        .collect::<String>()
        .trim_matches('_')
        .to_owned()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn urls_become_directory_names() {
        assert_eq!(
            sanitize("git@github.com:org/repo.git"),
            "git_github.com_org_repo.git"
        );
    }
}
