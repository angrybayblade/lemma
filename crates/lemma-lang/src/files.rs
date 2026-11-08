//! Module texts from working trees on disk, for callers without commits or
//! clones to read, such as a grammar module named by a `lemma verify`
//! manifest. The rest of the crate reads nothing itself.

use std::collections::BTreeMap;
use std::path::{Component, Path, PathBuf};

use crate::load::{Repository, RepositorySource, SourceError, Sources};

/// Working trees: the entry repository at `root`, and repositories pinned
/// with `local(...)` relative to the file declaring them.
pub struct WorkingTrees {
    root: PathBuf,
    roots: BTreeMap<String, PathBuf>,
}

impl WorkingTrees {
    /// Working trees whose entry repository is at `root`.
    pub fn new(root: &Path) -> Self {
        Self {
            root: root.to_path_buf(),
            roots: BTreeMap::new(),
        }
    }

    fn root_of(&mut self, repository: &Repository) -> Result<PathBuf, SourceError> {
        let Some(source) = &repository.source else {
            return Ok(self.root.clone());
        };
        let RepositorySource::Local {
            path,
            declared_in,
            declaring_file,
            ..
        } = source
        else {
            return Err(SourceError::new(format!(
                "repository `{}` comes from git, which only `lemma check` clones",
                repository.name
            )));
        };
        let base = self.roots.get(declared_in).unwrap_or(&self.root);
        let directory = Path::new(declaring_file).parent().unwrap_or(Path::new(""));
        let root = normalize(&base.join(directory).join(path));
        self.roots.insert(repository.name.clone(), root.clone());
        Ok(root)
    }
}

impl Sources for WorkingTrees {
    fn read(&mut self, repository: &Repository, path: &str) -> Result<Option<String>, SourceError> {
        let file = self.root_of(repository)?.join(path);
        match std::fs::read_to_string(&file) {
            Ok(text) => Ok(Some(text)),
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(None),
            Err(error) => Err(SourceError::new(format!(
                "cannot read {}: {error}",
                file.display()
            ))),
        }
    }
}

/// `a/b/../c` as `a/c`, without touching the file system.
fn normalize(path: &Path) -> PathBuf {
    let mut normalized = PathBuf::new();
    for component in path.components() {
        match component {
            Component::ParentDir if normalized.file_name().is_some() => {
                normalized.pop();
            }
            Component::CurDir => {}
            other => normalized.push(other),
        }
    }
    normalized
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::check_program;

    #[test]
    fn a_grammar_module_loads_its_imports_from_disk() {
        let temporary = tempfile::tempdir().unwrap();
        let directory = temporary.path();
        std::fs::create_dir_all(directory.join("rules")).unwrap();
        std::fs::write(
            directory.join("contracts.lm"),
            "module contracts\nimport contracts.rules.extra\n",
        )
        .unwrap();
        std::fs::write(
            directory.join("rules/extra.lm"),
            "module contracts.rules.extra\nrelation R(a: Text)\n",
        )
        .unwrap();

        let checked = check_program(&mut WorkingTrees::new(directory), "contracts.lm");

        assert_eq!(checked.diagnostics, []);
        assert_eq!(checked.model.relations.len(), 1);
    }
}
