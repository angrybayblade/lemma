//! Reading `.lm` modules at the checkouts `lemma check` reads: the entry
//! repository at the commit being checked, and the repositories it pins
//! from their working trees or Lemma's clone cache.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use lemma_lang::load::{Repository, RepositorySource, SourceError, Sources};
use lemma_lang::{Checked, Severity, check_program};

use crate::GraphError;
use crate::config::LemmaFile;
use crate::repos::{Checkout, clone_remote};

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
        }
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
        checkout.read(path).map_err(|error| source_error(&error))
    }
}

fn source_error(error: &GraphError) -> SourceError {
    SourceError {
        message: error.to_string(),
    }
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

/// A repository's `lemma.lm` at a checkout, checked and lowered to the
/// `lemma.yaml` structures for that repository.
pub fn read_manifest(
    checkout: &Checkout,
    manifest: &str,
    cache: &Path,
) -> Result<LemmaFile, GraphError> {
    let mut sources = CheckoutSources::new(checkout.clone(), cache);
    let checked = check_program(&mut sources, manifest);
    let path = checkout.root.join(manifest);
    let errors = render_errors(&checked);
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

/// Error diagnostics as `module:line:column: message`.
pub fn render_errors(checked: &Checked) -> Vec<String> {
    checked
        .diagnostics
        .iter()
        .filter(|diagnostic| diagnostic.severity == Severity::Error)
        .map(|diagnostic| {
            let position = checked
                .loaded
                .modules
                .get(&diagnostic.module)
                .map(|module| module.lines.position(diagnostic.span.start));
            match position {
                Some(position) => format!(
                    "{}:{}:{}: {}",
                    diagnostic.module,
                    position.line + 1,
                    position.column + 1,
                    diagnostic.message
                ),
                None => diagnostic.message.clone(),
            }
        })
        .collect()
}
