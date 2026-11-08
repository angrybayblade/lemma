//! What `lemma index` reads: the code each service in a directory's
//! description says it is made of.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use anyhow::{Context, bail};
use lemma_graph::config::{LEMMA_FILE, LemmaFile, Service};
use lemma_graph::repos::{Checkout, Cloning, cache_directory, is_cloned};
use lemma_index::{RuleSpec, Selection};
use lemma_model::Language;

/// Where `lemma index` reads from: `path` itself if it is a file, else the
/// description in it if there is one; and the directory indexed.
pub(crate) fn locate(path: &Path) -> anyhow::Result<(PathBuf, Option<PathBuf>)> {
    let path = path
        .canonicalize()
        .with_context(|| format!("cannot index {}", path.display()))?;
    if path.is_file() {
        let root = path.parent().map(Path::to_path_buf).unwrap_or_default();
        return Ok((root, Some(path)));
    }
    let description = description_in(&path);
    Ok((path, description))
}

/// The description in `directory`, if it has one. `lemma.yaml` comes first
/// because `lemma check` reads it first, and the two must agree.
pub(crate) fn description_in(directory: &Path) -> Option<PathBuf> {
    [LEMMA_FILE, lemma_lang::load::MODULE_FILE]
        .into_iter()
        .map(|name| directory.join(name))
        .find(|file| file.is_file())
}

/// What `lemma index` says when a directory has no description.
pub(crate) const MISSING_DESCRIPTION: &str = "no mod.lm here, so every supported file was \
    indexed; describe the services and the code each is made of (`index.include`) to index only \
    that, with their exports and links (see docs/check.md)";

/// Whether a description is of a workspace: repositories it composes
/// (`#name` services) and no code of its own.
pub(crate) fn is_workspace(file: &LemmaFile) -> bool {
    let mut names = file.services.keys();
    file.services.keys().any(|name| name.starts_with('#'))
        && names.all(|name| name.starts_with('#'))
}

/// The description at `path`, read and checked.
pub(crate) fn read_description(path: &Path, cloning: Cloning) -> anyhow::Result<LemmaFile> {
    if path.extension().is_some_and(|extension| extension == "lm") {
        return read_module(path, cloning);
    }
    let text = std::fs::read_to_string(path)
        .with_context(|| format!("could not read {}", path.display()))?;
    let file =
        lemma_graph::config::parse(&text).with_context(|| format!("invalid {}", path.display()))?;
    file.validate()
        .with_context(|| format!("invalid {}", path.display()))?;
    Ok(file)
}

/// The repository a root module at `path` describes, loaded and type-checked
/// as `lemma check` loads it, so that the two cannot disagree about it.
fn read_module(path: &Path, cloning: Cloning) -> anyhow::Result<LemmaFile> {
    let (Some(root), Some(name)) = (path.parent(), path.file_name()) else {
        bail!("{} is not a module's file", path.display());
    };
    let checkout = Checkout {
        root: root.to_path_buf(),
        commit: None,
    };
    let name = name.to_string_lossy();
    // Its errors already name the file.
    Ok(lemma_graph::lm::read_manifest(
        &checkout,
        &name,
        &cache_directory(),
        &BTreeMap::new(),
        cloning,
    )?)
}

/// The files a repository's index covers: those any of its services
/// includes.
pub(crate) fn repository_selection(file: &LemmaFile) -> anyhow::Result<Selection> {
    let mut rules = Vec::new();
    for (name, service) in local_services(file) {
        if service.index.include.is_empty() {
            continue;
        }
        rules.push(rule(name, service)?);
    }
    // A description can be only requirements, and a service only an API
    // document. Neither is a reason to leave the repository unindexed, or to
    // lose the operations a document exports.
    if rules.is_empty() {
        return Ok(Selection::everything());
    }
    Ok(Selection::from_rules(&rules)?)
}

/// What `lemma index` says when no service in `file` names its code, so that
/// every supported file was indexed.
pub(crate) fn whole_repository_warning(file: &LemmaFile) -> Option<String> {
    let services: Vec<(&String, &Service)> = local_services(file).collect();
    if services.is_empty() {
        return Some(
            "the description defines no services, so every supported file was indexed; describe \
             each service and the code it is made of (`index.include`) to index only that"
                .to_owned(),
        );
    }
    if services
        .iter()
        .any(|(_, service)| !service.index.include.is_empty())
    {
        return None;
    }
    let names: Vec<&str> = services.iter().map(|(name, _)| name.as_str()).collect();
    Some(format!(
        "no service in the description says what it is made of, so every supported file was \
         indexed; add `index.include` to {}",
        names.join(", ")
    ))
}

/// What `lemma index` says about each repository `file` names that is not
/// cloned here, so links reaching it are not known.
pub(crate) fn not_cloned_warnings(file: &LemmaFile) -> Vec<String> {
    let cache = cache_directory();
    file.services
        .iter()
        .filter_map(|(name, service)| Some((name, service.git.as_ref()?, service)))
        .filter(|(_, url, service)| !is_cloned(url, service.reference.as_deref(), &cache))
        .map(|(name, url, _)| format!("{name} is not cloned, so nothing about it is known ({url})"))
        .collect()
}

/// The services `file` defines itself, without the repositories it names.
fn local_services(file: &LemmaFile) -> impl Iterator<Item = (&String, &Service)> {
    file.services
        .iter()
        .filter(|(name, _)| !name.starts_with('#'))
}

fn rule(name: &str, service: &Service) -> anyhow::Result<RuleSpec> {
    let languages = service
        .index
        .languages
        .iter()
        .map(|language| {
            language
                .parse::<Language>()
                .map_err(|_| anyhow::anyhow!("service `{name}`: unknown language `{language}`"))
        })
        .collect::<anyhow::Result<Vec<_>>>()?;
    Ok(RuleSpec {
        include: service.index.include.clone(),
        exclude: service.index.exclude.clone(),
        languages,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_directory_with_only_a_root_module_is_described_by_it() {
        let directory = tempfile::tempdir().unwrap();
        let module = directory.path().join("mod.lm");
        std::fs::write(&module, "module example\n").unwrap();

        assert_eq!(description_in(directory.path()), Some(module));
    }

    #[test]
    fn lemma_yaml_is_read_before_a_root_module_as_lemma_check_does() {
        let directory = tempfile::tempdir().unwrap();
        let yaml = directory.path().join(LEMMA_FILE);
        std::fs::write(&yaml, "services: {}\n").unwrap();
        std::fs::write(directory.path().join("mod.lm"), "module example\n").unwrap();

        assert_eq!(description_in(directory.path()), Some(yaml));
    }

    #[test]
    fn services_without_code_select_every_file_and_are_named_in_the_warning() {
        let file = lemma_graph::config::parse("services:\n  api: {}\n  web: {}\n").unwrap();

        let selection = repository_selection(&file).unwrap();
        let warning = whole_repository_warning(&file).unwrap();

        assert_eq!(
            selection.description(),
            Selection::everything().description()
        );
        assert!(warning.contains("api, web"), "{warning}");
    }

    #[test]
    fn a_description_without_services_selects_every_file_and_says_so() {
        let file = LemmaFile::default();

        let selection = repository_selection(&file).unwrap();
        let warning = whole_repository_warning(&file).unwrap();

        assert_eq!(
            selection.description(),
            Selection::everything().description()
        );
        assert!(warning.contains("defines no services"), "{warning}");
    }

    #[test]
    fn a_service_naming_its_code_needs_no_warning() {
        let file = lemma_graph::config::parse(
            "services:\n  api: {}\n  web:\n    index:\n      include: [src]\n",
        )
        .unwrap();

        assert_eq!(whole_repository_warning(&file), None);
    }

    #[test]
    fn each_service_contributes_its_own_rule() {
        let file = lemma_graph::config::parse(
            "services:\n  api:\n    index: { include: [api], exclude: [\"**/gen/**\"] }\n  web:\n    index: { include: [web], languages: [typescript] }\n",
        )
        .unwrap();

        let selection = repository_selection(&file).unwrap();

        assert!(selection.covers("api/main.go", Language::Go));
        assert!(!selection.covers("api/gen/x.go", Language::Go));
        assert!(selection.covers("web/app.ts", Language::TypeScript));
        assert!(!selection.covers("web/app.py", Language::Python));
    }
}
