//! What the services in a manifest serve, read before any consumer: the
//! markers that make a string in code a path, and the path prefixes each
//! service owns. Both come from the services' operations (or the manifest),
//! so nothing about any particular API's layout is assumed.
//!
//! A service owns, for each of its documents, the prefix all that
//! document's paths share, unless another service's paths start with it
//! too. Otherwise its paths are grouped by the shortest prefix no other
//! service's path starts with, and it owns the longest prefix each group
//! shares: `/api/tasks_/` when another service serves `/api/v3/`, and
//! `/internal/admin/diagnostics/` rather than all of `/internal/` when
//! that is the only internal path it documents. A call to an unserved path under a service's prefix is then
//! that service's to answer for.

use std::collections::{BTreeMap, BTreeSet};

/// Paths the system's services serve, as consumers' code is read.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct System {
    /// Leading segments of every operation path, such as `/v1/`.
    pub markers: Vec<String>,
    /// Each service's path prefixes, as segments.
    prefixes: BTreeMap<String, Vec<Vec<String>>>,
}

/// A service's operation paths, grouped by the document describing them.
pub type DocumentPaths = BTreeMap<String, BTreeSet<String>>;

impl System {
    /// From each service's operation paths, by document, and declared
    /// prefixes, which replace the derived ones.
    pub fn new(
        operation_paths: &BTreeMap<String, DocumentPaths>,
        declared: &BTreeMap<String, Vec<String>>,
    ) -> Self {
        let every_path = operation_paths
            .values()
            .flat_map(BTreeMap::values)
            .flatten();
        let markers = lemma_http::path_markers(every_path.map(String::as_str));
        let mut prefixes = BTreeMap::new();
        for (service, documents) in operation_paths {
            let owned = match declared.get(service).filter(|list| !list.is_empty()) {
                Some(list) => list.iter().map(|prefix| segments(prefix)).collect(),
                None => derived_prefixes(service, documents, operation_paths),
            };
            prefixes.insert(service.clone(), owned);
        }
        Self { markers, prefixes }
    }

    /// The services whose prefixes a path starts with.
    pub fn addressed_to(&self, path: &str) -> Vec<&str> {
        let path = segments(path);
        self.prefixes
            .iter()
            .filter(|(_, owned)| owned.iter().any(|prefix| path.starts_with(prefix)))
            .map(|(service, _)| service.as_str())
            .collect()
    }
}

/// The prefixes `service` owns, from its documents (see the module
/// documentation), sorted and unique.
fn derived_prefixes(
    service: &str,
    documents: &DocumentPaths,
    operation_paths: &BTreeMap<String, DocumentPaths>,
) -> Vec<Vec<String>> {
    let others: Vec<Vec<String>> = operation_paths
        .iter()
        .filter(|(other, _)| other.as_str() != service)
        .flat_map(|(_, documents)| documents.values().flatten())
        .map(|path| segments(path))
        .collect();
    let is_shared = |prefix: &[String]| others.iter().any(|other| other.starts_with(prefix));
    let mut owned = Vec::new();
    for paths in documents.values() {
        if let Some(prefix) = common_prefix(paths).filter(|prefix| !is_shared(prefix)) {
            owned.push(prefix);
            continue;
        }
        let mut groups: BTreeMap<Vec<String>, BTreeSet<String>> = BTreeMap::new();
        for path in paths {
            if let Some(prefix) = unshared_prefix(path, &is_shared) {
                groups.entry(prefix).or_default().insert(path.clone());
            }
        }
        owned.extend(groups.values().filter_map(common_prefix));
    }
    owned.sort();
    owned.dedup();
    owned
}

/// The shortest static prefix of `path` that `is_shared` rejects, if any.
fn unshared_prefix(path: &str, is_shared: &impl Fn(&[String]) -> bool) -> Option<Vec<String>> {
    let parts = segments(path);
    let static_length = parts.iter().take_while(|segment| *segment != "{}").count();
    (1..=static_length)
        .map(|length| parts[..length].to_vec())
        .find(|prefix| !is_shared(prefix))
}

fn segments(path: &str) -> Vec<String> {
    path.split('/')
        .filter(|segment| !segment.is_empty())
        .map(str::to_owned)
        .collect()
}

/// The segments every path starts with, stopping at a parameter; `None` if
/// they share none.
fn common_prefix(paths: &BTreeSet<String>) -> Option<Vec<String>> {
    let mut all = paths.iter().map(|path| segments(path));
    let mut prefix: Vec<String> = all.next()?;
    for path in all {
        let shared = prefix
            .iter()
            .zip(&path)
            .take_while(|(left, right)| left == right)
            .count();
        prefix.truncate(shared);
    }
    let static_part = prefix.iter().take_while(|segment| *segment != "{}").count();
    prefix.truncate(static_part);
    (!prefix.is_empty()).then_some(prefix)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn document(service: &str, documents: &[&[&str]]) -> (String, DocumentPaths) {
        let documents = documents
            .iter()
            .enumerate()
            .map(|(index, paths)| {
                let paths = paths.iter().map(|path| (*path).to_owned()).collect();
                (format!("{service}-{index}.json"), paths)
            })
            .collect();
        (service.to_owned(), documents)
    }

    #[test]
    fn a_document_owns_its_shared_prefix_unless_declared_otherwise() {
        let operations = BTreeMap::from([
            document("billing", &[&["/v1/invoices", "/v1/invoices/{}"]]),
            document("search", &[&["/api/search", "/api/suggest"]]),
        ]);
        let declared = BTreeMap::from([("search".to_owned(), vec!["/api/search/".to_owned()])]);

        let system = System::new(&operations, &declared);

        assert_eq!(system.markers, ["/api/", "/v1/"]);
        assert_eq!(system.addressed_to("/v1/invoices/{}/lines"), ["billing"]);
        assert_eq!(system.addressed_to("/api/search/new"), ["search"]);
        assert_eq!(system.addressed_to("/api/trpc/x"), Vec::<String>::new());
    }

    #[test]
    fn each_document_of_a_service_owns_its_own_prefix() {
        let operations = BTreeMap::from([document(
            "api",
            &[
                &["/api/v3/tools", "/api/v3/products"],
                &["/api/v3.1/tools", "/api/v3.1/files/{}"],
            ],
        )]);

        let system = System::new(&operations, &BTreeMap::new());

        assert_eq!(system.addressed_to("/api/v3.1/labs"), ["api"]);
        assert_eq!(system.addressed_to("/api/trpc"), Vec::<String>::new());
    }

    #[test]
    fn a_prefix_another_service_shares_narrows_to_what_tells_them_apart() {
        let operations = BTreeMap::from([
            document("gateway", &[&["/api/v3/tools", "/api/v3/products"]]),
            document(
                "worker",
                &[&[
                    "/api/tasks_/{}",
                    "/api/tasks_/x",
                    "/internal/admin/a/{}",
                    "/internal/admin/b",
                ]],
            ),
        ]);

        let system = System::new(&operations, &BTreeMap::new());

        assert_eq!(system.addressed_to("/api/tasks_/x/execute"), ["worker"]);
        assert_eq!(system.addressed_to("/api/v3/gone"), ["gateway"]);
        assert_eq!(system.addressed_to("/api/trpc"), Vec::<String>::new());
        assert_eq!(
            system.addressed_to("/internal/logger"),
            Vec::<String>::new()
        );
        assert_eq!(system.addressed_to("/internal/admin/c"), ["worker"]);
    }
}
