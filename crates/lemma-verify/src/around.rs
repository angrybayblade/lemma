//! Whether a name a change newly requires is written anywhere in a
//! consumer's repository.
//!
//! A function passed its request (`axios.post(url, data)`) says nothing
//! about what is in it, so a newly required field it never names may be
//! set by a caller. But a name no file of the repository writes cannot be
//! set by any caller there, and the request lacks it for certain. So for
//! the few names a change newly requires, each consumer's repository is
//! searched for the word, outside the files of the service that requires
//! it: its documents and its own code name it, and send nothing.

use std::cell::RefCell;
use std::collections::{BTreeMap, BTreeSet};
use std::path::{Path, PathBuf};

use crate::manifest::{Manifest, Repository, Role};

/// The names each operation requires in a request, by snapshot, as the
/// services' documents are read.
#[derive(Default)]
pub(crate) struct Required {
    /// By service and operation: the names required at the base, and at
    /// the head.
    by_operation: BTreeMap<(String, String), [BTreeSet<String>; 2]>,
}

impl Required {
    /// Notes that `service`'s `operation` requires `name` in `snapshots`.
    /// A repository read once, for both snapshots, requires nothing newly.
    pub(crate) fn note(&mut self, service: &str, operation: &str, snapshots: &[&str], name: &str) {
        let at = match snapshots {
            ["base"] => 0,
            ["head"] => 1,
            _ => return,
        };
        let key = (service.to_owned(), operation.to_owned());
        self.by_operation.entry(key).or_default()[at].insert(name.to_owned());
    }

    /// For each service, the names an operation of it requires at the
    /// head and did not at the base.
    fn newly(&self) -> BTreeMap<String, BTreeSet<String>> {
        let mut newly: BTreeMap<String, BTreeSet<String>> = BTreeMap::new();
        for ((service, _), [base, head]) in &self.by_operation {
            let names: Vec<&String> = head.difference(base).collect();
            if !names.is_empty() {
                let of_service = newly.entry(service.clone()).or_default();
                of_service.extend(names.into_iter().cloned());
            }
        }
        newly
    }
}

/// One search: a clone, the commit read, the service and the name.
type Search = (PathBuf, Option<String>, String, String);

/// The newly required names, and which consumers' repositories write them.
pub(crate) struct Around<'m> {
    manifest: &'m Manifest,
    newly: BTreeMap<String, BTreeSet<String>>,
    /// Searches made, since several consumers may share a clone.
    searched: RefCell<BTreeMap<Search, bool>>,
}

impl<'m> Around<'m> {
    pub(crate) fn new(manifest: &'m Manifest, required: &Required) -> Self {
        Self {
            manifest,
            newly: required.newly(),
            searched: RefCell::default(),
        }
    }

    /// The newly required names some file of `repository` at `commit`
    /// writes, each with the service requiring it, that service's own
    /// files left out. A repository that cannot be searched is taken to
    /// write them all: what cannot be ruled out may be sent.
    pub(crate) fn named_in(
        &self,
        repository: &Repository,
        commit: Option<&str>,
    ) -> Vec<(String, String)> {
        let started = std::time::Instant::now();
        let asked = self.newly.iter().flat_map(|(service, names)| {
            let own = own_paths(self.manifest, service, &repository.path);
            names
                .iter()
                .filter_map(move |name| Some((service, name, own.clone()?)))
        });
        let named: Vec<(String, String)> = asked
            .filter(|(service, name, own)| self.is_named(repository, commit, (service, name), own))
            .map(|(service, name, _)| (service.clone(), name.clone()))
            .collect();
        if !self.newly.is_empty() {
            log::debug!(
                "searched {} for newly required names in {:.0?}: {} written there",
                repository.path.display(),
                started.elapsed(),
                named.len()
            );
        }
        named
    }

    fn is_named(
        &self,
        repository: &Repository,
        commit: Option<&str>,
        (service, name): (&String, &String),
        own: &[String],
    ) -> bool {
        let search = (
            repository.path.clone(),
            commit.map(str::to_owned),
            service.clone(),
            name.clone(),
        );
        if let Some(found) = self.searched.borrow().get(&search) {
            return *found;
        }
        let found = lemma_http::holds_word(&repository.path, commit, name, own).unwrap_or(true);
        self.searched.borrow_mut().insert(search, found);
        found
    }
}

/// The paths of `service`'s own files in the clone at `path`: its
/// documents, its route and schema code, and the code described as its
/// own. Empty when the service is in another clone. `None` when the whole
/// clone is the service's, so nothing in it is another's code.
fn own_paths(manifest: &Manifest, service: &str, path: &Path) -> Option<Vec<String>> {
    let mut own = Vec::new();
    for (name, repository) in &manifest.repositories {
        if repository.path != path {
            continue;
        }
        match repository.role {
            Role::Service if repository.service.as_deref().unwrap_or(name) == service => {
                own.extend(repository.specs.iter().cloned());
                own.extend(repository.routes.iter().cloned());
                own.extend(repository.schemas.iter().cloned());
            }
            Role::Consumer if repository.service.as_deref() == Some(service) => {
                if repository.include.is_empty() {
                    return None;
                }
                own.extend(repository.include.iter().cloned());
            }
            _ => {}
        }
    }
    own.sort();
    own.dedup();
    Some(own)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_name_is_newly_required_where_an_operation_requires_it_only_at_the_head() {
        let mut required = Required::default();
        required.note("api", "POST /v1/users", &["base"], "name");
        required.note("api", "POST /v1/users", &["head"], "name");
        required.note("api", "POST /v1/users", &["head"], "email");
        required.note("api", "GET /v1/users", &["base"], "email");
        required.note("api", "GET /v1/users", &["head"], "email");
        required.note("billing", "POST /v1/charges", &["base", "head"], "currency");

        let newly = required.newly();

        let expected = BTreeMap::from([("api".to_owned(), BTreeSet::from(["email".to_owned()]))]);
        assert_eq!(newly, expected);
    }
}
