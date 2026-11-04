//! Inferred links: calls from code to documented operations, found without
//! being declared.
//!
//! Finding them is `lemma-verify`'s work: it reads services' `OpenAPI`
//! documents and route code, generated clients, and the code that calls
//! them, and its grammar decides what a change breaks. This module only
//! describes the composed repositories to it as a verification manifest:
//! each local service becomes up to three entries, one per part it plays.

use std::collections::BTreeMap;
use std::path::Path;

use lemma_verify::{Change as VerifyChange, Manifest, Repository, Role};

use crate::config::{LemmaFile, Service};

/// One repository as inference reads it.
pub struct Source<'a> {
    /// Its name in the system (`#billing`, or `self`).
    pub name: &'a str,
    /// Its local clone.
    pub root: &'a Path,
    /// The commit read, unless it is the changed repository; `None` for the
    /// working directory.
    pub commit: Option<&'a str>,
    /// Its `lemma.yaml` with the change.
    pub file: &'a LemmaFile,
}

/// The changed repository and its two commits, resolved.
pub struct Changed<'a> {
    /// Its name in the system.
    pub name: &'a str,
    /// The commit before the change.
    pub base: &'a str,
    /// The commit with the change; `None` for the working directory.
    pub head: Option<&'a str>,
}

/// A verification manifest, and the repository each of its entries
/// describes.
pub struct Inferred {
    /// The manifest.
    pub manifest: Manifest,
    /// Each entry's repository, by entry name.
    pub repositories: BTreeMap<String, String>,
}

/// The verification manifest for `sources`, or `None` when the changed
/// repository plays no part inference reads.
pub fn manifest(sources: &[Source<'_>], changed: &Changed<'_>) -> Option<Inferred> {
    let mut entries_found = BTreeMap::new();
    let mut owners = BTreeMap::new();
    let mut changed_entry = None;
    for source in sources {
        let is_changed = source.name == changed.name;
        let commit = if is_changed { None } else { source.commit };
        for (name, repository) in entries(source, commit) {
            if is_changed && changed_entry.is_none() {
                changed_entry = Some(name.clone());
            }
            owners.insert(name.clone(), source.name.to_owned());
            entries_found.insert(name, repository);
        }
    }
    let manifest = Manifest {
        grammar: None,
        change: VerifyChange {
            repository: changed_entry?,
            base: changed.base.to_owned(),
            head: changed.head.map(str::to_owned),
        },
        repositories: entries_found,
    };
    Some(Inferred {
        manifest,
        repositories: owners,
    })
}

/// A source's entries: per local service, its served documents, the
/// generated clients it is or uses, and its code.
fn entries(source: &Source<'_>, commit: Option<&str>) -> Vec<(String, Repository)> {
    let local: Vec<(&String, &Service)> = source
        .file
        .services
        .iter()
        .filter(|(name, _)| !name.starts_with('#'))
        .collect();
    let only_service = local.len() == 1;
    let mut found = Vec::new();
    for (service_name, service) in local {
        let name = entry_name(source.name, service_name);
        let has_code = !service.index.include.is_empty() || (only_service && is_plain(service));
        let reads_code = has_code && service.searches_calls();
        let parts = parts(&name, service, reads_code, |role| {
            entry(source.root, commit, role)
        });
        let several = parts.len() > 1;
        for (part, repository) in parts {
            // A service's documents keep its plain name: its code entry
            // refers to it by that name.
            let entry_name = match part {
                Some(part) if several => format!("{name} ({part})"),
                _ => name.clone(),
            };
            found.push((entry_name, repository));
        }
    }
    found
}

/// The parts a service plays, each with its suffix when named alongside
/// others (`None` for its documents).
fn parts(
    name: &str,
    service: &Service,
    reads_code: bool,
    entry: impl Fn(Role) -> Repository,
) -> Vec<(Option<String>, Repository)> {
    let mut parts = Vec::new();
    // A service whose operations come only from its code serves them too.
    let serves = !service.exports.openapi.is_empty() || service.exports.operations_from.is_some();
    if serves {
        parts.push((None, served(entry(Role::Service), service)));
    }
    if let Some(client) = &service.exports.client {
        let mut repository = entry(Role::Client);
        repository.generated_from.clone_from(&client.generated_from);
        repository.specs.clone_from(&client.specs);
        repository.call_names.clone_from(&client.call_names);
        // Its code says what each of its methods requests.
        repository.include.clone_from(&service.index.include);
        parts.push((Some("client".to_owned()), repository));
    }
    if reads_code {
        let mut repository = entry(Role::Consumer);
        repository.include.clone_from(&service.index.include);
        // Its own routes, if it serves any, are not calls.
        repository.service = serves.then(|| name.to_owned());
        parts.push((Some("code".to_owned()), repository));
    }
    parts
}

/// Whether a service declares nothing but code: no exports or clients.
/// Only such a service, alone in its repository, has the whole repository
/// read as its code by default.
fn is_plain(service: &Service) -> bool {
    service.exports.is_empty()
}

fn served(mut repository: Repository, service: &Service) -> Repository {
    repository.specs.clone_from(&service.exports.openapi);
    // Route code and the schemas routes import are somewhere in what the
    // service is made of.
    repository.routes.clone_from(&service.index.include);
    repository.rejects_unknown_fields = service.rejects_unknown_fields;
    repository.schemas.clone_from(&service.index.include);
    repository
}

/// `service` in the parent's own repository, `#repo:service` elsewhere, as
/// in addresses.
fn entry_name(repository: &str, service: &str) -> String {
    if repository == crate::check::SELF {
        return service.to_owned();
    }
    format!("{repository}:{service}")
}

fn entry(root: &Path, commit: Option<&str>, role: Role) -> Repository {
    Repository {
        path: root.to_path_buf(),
        commit: commit.map(str::to_owned),
        role,
        service: None,
        specs: Vec::new(),
        generated: BTreeMap::new(),
        rejects_unknown_fields: false,
        routes: Vec::new(),
        path_prefixes: Vec::new(),
        route_aliases: BTreeMap::new(),
        schemas: Vec::new(),
        generated_from: Vec::new(),
        call_names: Vec::new(),
        include: Vec::new(),
        client_receivers: Vec::new(),
        client_modules: Vec::new(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::parse;

    #[test]
    fn services_become_entries_by_the_parts_they_play() {
        let provider = parse(
            "services:\n  api:\n    exports: { openapi: [openapi.json] }\n    index: { include: [src] }\n  worker:\n    exports: { operations: {} }\n",
        )
        .unwrap();
        let consumer = parse("services:\n  web: {}\n").unwrap();
        let sources = [
            Source {
                name: "#billing",
                root: Path::new("/billing"),
                commit: Some("abc"),
                file: &provider,
            },
            Source {
                name: "#web",
                root: Path::new("/web"),
                commit: Some("def"),
                file: &consumer,
            },
        ];
        let changed = Changed {
            name: "#billing",
            base: "b",
            head: Some("h"),
        };

        let inferred = manifest(&sources, &changed).unwrap();

        let manifest = inferred.manifest;
        let names: Vec<&String> = manifest.repositories.keys().collect();
        assert_eq!(names, ["#billing:api", "#billing:api (code)", "#web:web"]);
        assert_eq!(inferred.repositories["#billing:api (code)"], "#billing");
        assert_eq!(manifest.change.repository, "#billing:api");
        assert_eq!(manifest.repositories["#billing:api"].commit, None);
        assert_eq!(
            manifest.repositories["#billing:api (code)"]
                .service
                .as_deref(),
            Some("#billing:api")
        );
        assert_eq!(
            manifest.repositories["#web:web"].commit.as_deref(),
            Some("def")
        );
    }

    #[test]
    fn a_generated_client_is_not_searched_for_calls_even_with_its_code_indexed() {
        let client = parse(
            "services:\n  sdk:\n    index: { include: [src] }\n    exports:\n      client: { specs: [openapi.json] }\n",
        )
        .unwrap();
        let sources = [Source {
            name: "#sdk",
            root: Path::new("/sdk"),
            commit: None,
            file: &client,
        }];
        let changed = Changed {
            name: "#sdk",
            base: "b",
            head: Some("h"),
        };

        let manifest = manifest(&sources, &changed).unwrap().manifest;

        let names: Vec<&String> = manifest.repositories.keys().collect();
        assert_eq!(names, ["#sdk:sdk"]);
    }

    #[test]
    fn a_client_is_read_from_its_code() {
        let file = parse(
            "services:\n  sdk:\n    index: { include: [src] }\n    exports:\n      client: { specs: [openapi.json] }\n",
        )
        .unwrap();
        let sources = [Source {
            name: "#sdk",
            root: Path::new("/sdk"),
            commit: None,
            file: &file,
        }];
        let changed = Changed {
            name: "#sdk",
            base: "b",
            head: Some("h"),
        };

        let manifest = manifest(&sources, &changed).unwrap().manifest;

        assert_eq!(manifest.repositories["#sdk:sdk"].include, ["src"]);
    }
}
