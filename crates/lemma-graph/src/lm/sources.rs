//! Reading `.lm` modules at the checkouts `lemma check` reads: the entry
//! repository at the commit being checked, and the repositories it pins
//! from their working trees or Lemma's clone cache.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use lemma_lang::check_program;
use lemma_lang::load::{Repository, RepositorySource, SourceError, Sources};

use crate::GraphError;
use crate::config::LemmaFile;
use crate::repos::{Checkout, Cloning, clone_remote, is_cloned};

/// Module texts from checkouts: the entry repository's, and each pinned
/// repository's, found from its `repo` declaration.
pub struct CheckoutSources<'c> {
    entry: Checkout,
    cache: &'c Path,
    /// Repositories read so far, by name, for paths declared relative to
    /// them.
    checkouts: BTreeMap<String, Checkout>,
    /// Texts that win over the files they stand for, such as an editor's
    /// unsaved buffers, by absolute path.
    overlay: BTreeMap<PathBuf, String>,
    /// Repositories a composition already pinned, which win over the pins
    /// the modules declare, as a parent's do in `lemma.yaml`.
    pins: BTreeMap<String, Checkout>,
    cloning: Cloning,
}

impl<'c> CheckoutSources<'c> {
    /// Sources whose entry repository is read at `entry`; clones of git
    /// repositories go in `cache`.
    pub fn new(entry: Checkout, cache: &'c Path) -> Self {
        Self {
            entry,
            cache,
            checkouts: BTreeMap::new(),
            overlay: BTreeMap::new(),
            pins: BTreeMap::new(),
            cloning: Cloning::Allowed,
        }
    }

    /// The same sources, cloning repositories only as `cloning` says.
    #[must_use]
    pub fn with_cloning(mut self, cloning: Cloning) -> Self {
        self.cloning = cloning;
        self
    }

    /// The same sources, reading the repositories in `pins` where a
    /// composition put them rather than where modules pin them.
    #[must_use]
    pub fn with_pins(mut self, pins: BTreeMap<String, Checkout>) -> Self {
        self.pins = pins;
        self
    }

    /// The same sources, with `texts` read instead of the files at those
    /// absolute paths in working trees.
    #[must_use]
    pub fn with_overlay(mut self, texts: BTreeMap<PathBuf, String>) -> Self {
        self.overlay = texts;
        self
    }

    /// Where each repository read so far is, the entry's under `entry`.
    pub fn roots(&self, entry: &str) -> BTreeMap<String, PathBuf> {
        let mut roots: BTreeMap<String, PathBuf> = self
            .checkouts
            .iter()
            .map(|(name, checkout)| (name.clone(), checkout.root.clone()))
            .collect();
        roots.insert(entry.to_owned(), self.entry.root.clone());
        roots
    }

    fn checkout(&mut self, repository: &Repository) -> Result<Checkout, SourceError> {
        let Some(source) = &repository.source else {
            return Ok(self.entry.clone());
        };
        if let Some(found) = self.checkouts.get(&repository.name) {
            return Ok(found.clone());
        }
        if let Some(pinned) = self.pins.get(&repository.name) {
            self.checkouts
                .insert(repository.name.clone(), pinned.clone());
            return Ok(pinned.clone());
        }
        let checkout = match source {
            RepositorySource::Local {
                path,
                declared_in,
                declaring_file,
            } => {
                let base = self.root_of(declared_in);
                let directory = Path::new(declaring_file).parent().unwrap_or(Path::new(""));
                Checkout {
                    root: normalize(&base.join(directory).join(path)),
                    commit: None,
                }
            }
            RepositorySource::Git { url, rev, .. } => {
                if self.cloning == Cloning::Forbidden && !is_cloned(url, rev.as_deref(), self.cache)
                {
                    return Err(SourceError::unavailable(format!(
                        "repository `{}` is not cloned",
                        repository.name
                    )));
                }
                let root = clone_remote(url, rev.as_deref(), self.cache)
                    .map_err(|error| source_error(&error))?;
                Checkout {
                    root,
                    commit: Some(rev.clone().unwrap_or_else(|| "HEAD".to_owned())),
                }
            }
        };
        self.checkouts
            .insert(repository.name.clone(), checkout.clone());
        Ok(checkout)
    }

    /// Where a repository named in a declaration is: the entry's root when
    /// it is the entry repository or not read yet.
    fn root_of(&self, repository: &str) -> PathBuf {
        self.checkouts
            .get(repository)
            .map_or_else(|| self.entry.root.clone(), |checkout| checkout.root.clone())
    }
}

impl Sources for CheckoutSources<'_> {
    fn read(&mut self, repository: &Repository, path: &str) -> Result<Option<String>, SourceError> {
        let checkout = self.checkout(repository)?;
        if checkout.commit.is_none() {
            if let Some(text) = self.overlay.get(&checkout.root.join(path)) {
                return Ok(Some(text.clone()));
            }
        }
        let found = checkout.read(path).map_err(|error| source_error(&error))?;
        if found.is_some() || checkout.commit.is_none() {
            return Ok(found);
        }
        // A commit from before the repository had modules: its working
        // directory's, as a lemma.yaml missing at a commit is read.
        let working = Checkout {
            root: checkout.root.clone(),
            commit: None,
        };
        working.read(path).map_err(|error| source_error(&error))
    }
}

fn source_error(error: &GraphError) -> SourceError {
    SourceError::new(error.to_string())
}

/// `a/b/../c` as `a/c`, without touching the file system.
fn normalize(path: &Path) -> PathBuf {
    let mut normalized = PathBuf::new();
    for component in path.components() {
        match component {
            std::path::Component::ParentDir if normalized.file_name().is_some() => {
                normalized.pop();
            }
            std::path::Component::CurDir => {}
            other => normalized.push(other),
        }
    }
    normalized
}

/// A repository's `mod.lm` at a checkout, checked and lowered to the
/// `lemma.yaml` structures for that repository.
pub fn read_manifest(
    checkout: &Checkout,
    manifest: &str,
    cache: &Path,
    pins: &BTreeMap<String, Checkout>,
    cloning: Cloning,
) -> Result<LemmaFile, GraphError> {
    let mut sources = CheckoutSources::new(checkout.clone(), cache)
        .with_pins(pins.clone())
        .with_cloning(cloning);
    let checked = check_program(&mut sources, manifest);
    let path = checkout.root.join(manifest);
    let errors = checked.stopping_error_lines();
    if !errors.is_empty() {
        return Err(GraphError::Language { path, errors });
    }
    let repository = checked
        .loaded
        .modules
        .get(&checked.loaded.entry)
        .map(|module| module.repository.clone())
        .unwrap_or_default();
    super::repository_file(&checked, &repository)
        .map_err(|source| GraphError::Lower { path, source })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_composition_pin_wins_over_the_modules_own() {
        let directory = tempfile::tempdir().unwrap();
        let platform = directory.path().join("platform");
        let functions = directory.path().join("checkouts/functions");
        std::fs::create_dir_all(&platform).unwrap();
        std::fs::create_dir_all(&functions).unwrap();
        std::fs::write(
            platform.join("mod.lm"),
            "module platform\nimport functions\nrepo functions = local(\"../missing\")\n",
        )
        .unwrap();
        std::fs::write(functions.join("mod.lm"), "module functions\n").unwrap();
        let entry = Checkout {
            root: platform,
            commit: None,
        };
        let pins = BTreeMap::from([(
            "functions".to_owned(),
            Checkout {
                root: functions,
                commit: None,
            },
        )]);

        let file = read_manifest(&entry, "mod.lm", directory.path(), &pins, Cloning::Allowed);

        assert!(file.is_ok(), "{file:?}");
    }
}
