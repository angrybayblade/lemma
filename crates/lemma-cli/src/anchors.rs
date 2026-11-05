//! Counting a symbol's uses through language servers, for `lemma check
//! --anchors`.

use std::collections::BTreeMap;
use std::error::Error;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::time::Duration;

use lemma_model::{Language, LocationQuery, Position, ProviderAnswer};
use lemma_providers::{ProviderRegistry, ProviderSession, SemanticProvider, SourcePoint};

/// How long a server may take to start and to answer.
const REQUEST_TIMEOUT: Duration = Duration::from_secs(120);

/// Language servers started for the check, one per repository root and
/// provider, stopped when dropped.
pub(crate) struct ProviderReferences {
    registry: ProviderRegistry,
    sessions: BTreeMap<(PathBuf, String), ProviderSession>,
}

impl ProviderReferences {
    pub(crate) fn new(registry: ProviderRegistry) -> Self {
        Self {
            registry,
            sessions: BTreeMap::new(),
        }
    }

    /// The session of the first installed provider for `language` at
    /// `root`, started and warmed up on `file` the first time.
    fn session(
        &mut self,
        root: &Path,
        file: &str,
        language: Language,
    ) -> Result<Option<&mut ProviderSession>, Box<dyn Error + Send + Sync>> {
        let Some(provider) = self.registry.installed(language).into_iter().next() else {
            return Ok(None);
        };
        let key = (root.to_owned(), provider.definition.id.clone());
        if !self.sessions.contains_key(&key) {
            let mut session = ProviderSession::start(
                &provider.definition,
                &provider.executable,
                root,
                REQUEST_TIMEOUT,
            )?;
            session.warm_up(file, language)?;
            self.sessions.insert(key.clone(), session);
        }
        Ok(self.sessions.get_mut(&key))
    }
}

impl lemma_graph::References for ProviderReferences {
    fn uses(
        &mut self,
        root: &Path,
        file: &str,
        language: Language,
        position: Position,
    ) -> lemma_graph::Uses {
        let Some(session) = self.session(root, file, language)? else {
            return Ok(None);
        };
        if session.is_project_scoped() {
            let name = name_at(root, file, position);
            let mut paths = vec![file.to_owned()];
            paths.extend(mentioning(root, &name));
            let paths: Vec<&str> = paths.iter().map(String::as_str).collect();
            session.open_projects(&paths)?;
        }
        let at = SourcePoint {
            path: file,
            language,
            position,
        };
        match session.locations(LocationQuery::References, at)? {
            ProviderAnswer::Found(locations) => Ok(Some(locations)),
            ProviderAnswer::Empty | ProviderAnswer::Unsupported => Ok(Some(Vec::new())),
        }
    }
}

impl Drop for ProviderReferences {
    fn drop(&mut self) {
        let sessions = std::mem::take(&mut self.sessions);
        for session in sessions.into_values() {
            session.shutdown();
        }
    }
}

/// The identifier starting at `position` in `file`, or empty if unreadable.
fn name_at(root: &Path, file: &str, position: Position) -> String {
    let Ok(source) = std::fs::read_to_string(root.join(file)) else {
        return String::new();
    };
    let Some(line) = source.lines().nth(position.line as usize) else {
        return String::new();
    };
    line.get(position.column as usize..)
        .unwrap_or("")
        .chars()
        .take_while(|character| character.is_alphanumeric() || *character == '_')
        .collect()
}

/// Files under `root` that mention `name` as a word, so a server that only
/// searches loaded projects loads theirs. Best effort: none if Git cannot
/// search.
fn mentioning(root: &Path, name: &str) -> Vec<String> {
    if name.is_empty() {
        return Vec::new();
    }
    let Ok(output) = Command::new("git")
        .arg("-C")
        .arg(root)
        .args(["grep", "-l", "-w", "-F", "-e", name])
        .output()
    else {
        return Vec::new();
    };
    let mut paths: Vec<String> = String::from_utf8_lossy(&output.stdout)
        .lines()
        .map(str::to_owned)
        .collect();
    paths.sort();
    paths
}
