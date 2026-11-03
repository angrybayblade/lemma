//! What `lemma index` reads: the code each service in a directory's
//! `lemma.yaml` says it is made of.

use std::path::Path;

use anyhow::{Context, bail};
use lemma_graph::config::{LEMMA_FILE, LemmaFile, Service};
use lemma_index::{RuleSpec, Selection};
use lemma_model::Language;

/// A `lemma.yaml` as read: its text and what it says.
pub(crate) struct LemmaSource {
    pub text: String,
    pub file: LemmaFile,
}

/// The `lemma.yaml` in `root`, read and checked.
pub(crate) fn read_lemma_file(root: &Path) -> anyhow::Result<LemmaSource> {
    let path = root.join(LEMMA_FILE);
    if !path.is_file() {
        bail!(
            "{} has no {LEMMA_FILE}; describe its services and the code each is made of \
             (`index.include`) before indexing it (see docs/check.md)",
            root.display()
        );
    }
    let text = std::fs::read_to_string(&path)
        .with_context(|| format!("could not read {}", path.display()))?;
    let file =
        lemma_graph::config::parse(&text).with_context(|| format!("invalid {}", path.display()))?;
    file.validate()
        .with_context(|| format!("invalid {}", path.display()))?;
    Ok(LemmaSource { text, file })
}

/// The files a repository's index covers: those any of its services
/// includes.
pub(crate) fn repository_selection(file: &LemmaFile) -> anyhow::Result<Selection> {
    let local: Vec<(&String, &Service)> = file
        .services
        .iter()
        .filter(|(name, _)| !name.starts_with('#'))
        .collect();
    if local.is_empty() {
        bail!("{LEMMA_FILE} defines no services of its own, so there is nothing to index");
    }
    let mut rules = Vec::new();
    for (name, service) in &local {
        if service.index.include.is_empty() {
            continue;
        }
        rules.push(rule(name, service)?);
    }
    if rules.is_empty() {
        let names: Vec<&str> = local.iter().map(|(name, _)| name.as_str()).collect();
        bail!(
            "no service in {LEMMA_FILE} says what it is made of; add `index.include` to {}",
            names.join(", ")
        );
    }
    Ok(Selection::from_rules(&rules)?)
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
    fn services_without_code_are_named_in_the_error() {
        let file = lemma_graph::config::parse("services:\n  api: {}\n  web: {}\n").unwrap();

        let error = repository_selection(&file).unwrap_err().to_string();

        assert!(error.contains("api, web"), "{error}");
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
