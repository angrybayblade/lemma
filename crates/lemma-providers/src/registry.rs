//! The set of configured providers and choosing one for a language.

use std::ffi::OsString;
use std::fs;
use std::path::{Path, PathBuf};

use lemma_model::Language;
use serde::{Deserialize, Serialize};
use serde_json::Value;

use crate::ProviderError;

const BUILTIN_PROVIDERS: &str = include_str!("builtin.toml");

/// One language server Lemma can use.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ProviderDefinition {
    /// Unique name, used in output and cache keys.
    pub id: String,
    /// Languages this server answers for.
    pub languages: Vec<Language>,
    /// Executable name on `PATH`, or a path.
    pub command: String,
    /// Command arguments.
    #[serde(default)]
    pub args: Vec<String>,
    /// Sent as `initializationOptions` during the handshake.
    #[serde(default)]
    pub initialization_options: Option<Value>,
    /// Returned when the server asks for `workspace/configuration`.
    #[serde(default)]
    pub settings: Option<Value>,
    /// Log messages, or titles of progress reports, that the server finishes
    /// once it has loaded the workspace (`*` matches anything). Lemma waits
    /// for one before asking anything.
    #[serde(default)]
    pub ready_messages: Vec<String>,
}

impl ProviderDefinition {
    /// Hash of everything that could change this provider's answers, so
    /// cached results are not reused after its configuration changes.
    pub fn fingerprint(&self) -> String {
        let mut hasher = blake3::Hasher::new();
        let parts = [&self.command]
            .into_iter()
            .chain(&self.args)
            .chain(&self.ready_messages);
        for part in parts {
            hasher.update(part.as_bytes());
            hasher.update(b"\0");
        }
        // JSON maps serialize with sorted keys, so this is deterministic.
        for value in [&self.initialization_options, &self.settings] {
            hasher.update(
                value
                    .as_ref()
                    .map(Value::to_string)
                    .unwrap_or_default()
                    .as_bytes(),
            );
            hasher.update(b"\0");
        }
        hasher.finalize().to_hex().to_string()
    }
}

/// A configured provider whose executable was found.
#[derive(Debug, Clone, PartialEq)]
pub struct InstalledProvider {
    /// The provider's configuration.
    pub definition: ProviderDefinition,
    /// Where its executable lives.
    pub executable: PathBuf,
}

/// Every configured provider, in the order they are tried.
#[derive(Debug, Clone, PartialEq)]
pub struct ProviderRegistry {
    definitions: Vec<ProviderDefinition>,
    /// Where to look for executables, if not this process's own `PATH`.
    search: Option<ExecutableSearch>,
}

/// A `PATH` and working directory to resolve executables against, such as
/// those of the command a long-running daemon is answering.
#[derive(Debug, Clone, PartialEq)]
struct ExecutableSearch {
    path: OsString,
    cwd: PathBuf,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct ProviderFile {
    #[serde(default)]
    provider: Vec<ProviderDefinition>,
}

impl ProviderRegistry {
    /// The providers that ship with Lemma.
    pub fn builtin() -> Result<Self, ProviderError> {
        let definitions = parse_definitions("built-in providers", BUILTIN_PROVIDERS)?;
        Ok(Self {
            definitions,
            search: None,
        })
    }

    /// The built-in providers plus a project's own, if its file exists.
    pub fn load(project_file: &Path) -> Result<Self, ProviderError> {
        let registry = Self::builtin()?;
        if !project_file.exists() {
            return Ok(registry);
        }
        let text =
            fs::read_to_string(project_file).map_err(|source| ProviderError::ReadConfig {
                path: project_file.to_owned(),
                source,
            })?;
        let overrides = parse_definitions(&project_file.display().to_string(), &text)?;
        Ok(registry.with_overrides(overrides))
    }

    /// Puts project providers first and drops built-ins they replace by id.
    #[must_use]
    pub fn with_overrides(self, overrides: Vec<ProviderDefinition>) -> Self {
        let replaced = |definition: &ProviderDefinition| {
            overrides
                .iter()
                .any(|override_| override_.id == definition.id)
        };
        let builtins: Vec<_> = self
            .definitions
            .into_iter()
            .filter(|d| !replaced(d))
            .collect();
        Self {
            definitions: overrides.into_iter().chain(builtins).collect(),
            search: self.search,
        }
    }

    /// Resolves executables against `path` and `cwd` instead of this
    /// process's environment.
    #[must_use]
    pub fn with_search_path(self, path: OsString, cwd: PathBuf) -> Self {
        Self {
            search: Some(ExecutableSearch { path, cwd }),
            ..self
        }
    }

    /// Finds a provider's executable by name on the search path, or checks a
    /// given path.
    pub fn resolve_executable(&self, command: &str) -> Option<PathBuf> {
        match &self.search {
            Some(search) => which::which_in(command, Some(&search.path), &search.cwd).ok(),
            None => which::which(command).ok(),
        }
    }

    /// Every provider, in the order they are tried.
    pub fn definitions(&self) -> &[ProviderDefinition] {
        &self.definitions
    }

    /// Ids of every provider configured for a language, installed or not.
    pub fn candidate_ids(&self, language: Language) -> Vec<String> {
        self.serving(language)
            .map(|definition| definition.id.clone())
            .collect()
    }

    /// Providers for a language whose executables are on `PATH`, in the order
    /// they should be tried.
    pub fn installed(&self, language: Language) -> Vec<InstalledProvider> {
        self.installed_with(language, |command| self.resolve_executable(command))
    }

    /// Like [`ProviderRegistry::installed`], with a custom way to find executables.
    pub fn installed_with(
        &self,
        language: Language,
        resolve: impl Fn(&str) -> Option<PathBuf>,
    ) -> Vec<InstalledProvider> {
        self.serving(language)
            .filter_map(|definition| {
                let executable = resolve(&definition.command)?;
                Some(InstalledProvider {
                    definition: definition.clone(),
                    executable,
                })
            })
            .collect()
    }

    fn serving(&self, language: Language) -> impl Iterator<Item = &ProviderDefinition> {
        self.definitions
            .iter()
            .filter(move |definition| definition.languages.contains(&language))
    }
}

fn parse_definitions(origin: &str, text: &str) -> Result<Vec<ProviderDefinition>, ProviderError> {
    let file: ProviderFile = toml::from_str(text).map_err(|source| ProviderError::ParseConfig {
        origin: origin.to_owned(),
        source,
    })?;
    Ok(file.provider)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn definition(id: &str, language: Language, command: &str) -> ProviderDefinition {
        ProviderDefinition {
            id: id.to_owned(),
            languages: vec![language],
            command: command.to_owned(),
            args: Vec::new(),
            initialization_options: None,
            settings: None,
            ready_messages: Vec::new(),
        }
    }

    fn installed(commands: &'static [&'static str]) -> impl Fn(&str) -> Option<PathBuf> {
        move |command| {
            commands
                .contains(&command)
                .then(|| PathBuf::from("/bin").join(command))
        }
    }

    #[test]
    fn builtin_providers_cover_every_language() {
        let registry = ProviderRegistry::builtin().unwrap();
        for language in Language::ALL {
            let serves = |d: &&ProviderDefinition| d.languages.contains(&language);
            assert!(
                registry.definitions().iter().any(|d| serves(&d)),
                "no provider for {language}"
            );
        }
    }

    fn ids(providers: &[InstalledProvider]) -> Vec<&str> {
        providers
            .iter()
            .map(|provider| provider.definition.id.as_str())
            .collect()
    }

    #[test]
    fn only_installed_providers_are_returned_in_order() {
        let registry = ProviderRegistry::builtin().unwrap();

        let providers = registry.installed_with(
            Language::TypeScript,
            installed(&["tsc", "typescript-language-server"]),
        );

        assert_eq!(
            ids(&providers),
            ["typescript-language-server", "typescript-native"]
        );
        assert_eq!(providers[1].executable, PathBuf::from("/bin/tsc"));
    }

    #[test]
    fn candidates_include_providers_that_are_not_installed() {
        let registry = ProviderRegistry::builtin().unwrap();

        assert_eq!(
            registry.installed_with(Language::Python, installed(&[])),
            Vec::new()
        );
        assert_eq!(
            registry.candidate_ids(Language::Python),
            ["ty", "pyright", "basedpyright"]
        );
    }

    #[test]
    fn project_providers_come_first_and_replace_builtins_by_id() {
        let registry = ProviderRegistry::builtin().unwrap().with_overrides(vec![
            definition("custom-python", Language::Python, "custom"),
            definition("pyright", Language::Python, "/opt/pyright"),
        ]);

        let python_ids: Vec<_> = registry
            .definitions()
            .iter()
            .filter(|d| d.languages.contains(&Language::Python))
            .map(|d| (d.id.as_str(), d.command.as_str()))
            .collect();

        assert_eq!(
            python_ids,
            [
                ("custom-python", "custom"),
                ("pyright", "/opt/pyright"),
                ("ty", "ty"),
                ("basedpyright", "basedpyright-langserver")
            ]
        );
    }

    #[test]
    fn project_files_use_the_builtin_format() {
        let text = r#"
[[provider]]
id = "jedi"
languages = ["python"]
command = "jedi-language-server"

[provider.initialization_options]
diagnostics = { enable = false }
"#;
        let definitions = parse_definitions("test", text).unwrap();

        assert_eq!(definitions[0].id, "jedi");
        assert_eq!(
            definitions[0].initialization_options,
            Some(serde_json::json!({ "diagnostics": { "enable": false } }))
        );
    }

    #[test]
    fn unknown_fields_are_rejected() {
        let text = "[[provider]]\nid = \"x\"\nlanguages = []\ncommand = \"x\"\ncomand = \"typo\"\n";
        assert!(parse_definitions("test", text).is_err());
    }

    #[test]
    fn fingerprint_changes_with_settings() {
        let plain = definition("pyright", Language::Python, "pyright-langserver");
        let mut configured = plain.clone();
        configured.settings = Some(serde_json::json!({ "python": {} }));

        assert_eq!(plain.fingerprint(), plain.clone().fingerprint());
        assert_ne!(plain.fingerprint(), configured.fingerprint());
    }
}
