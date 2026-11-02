//! What the services in a manifest serve, read before any consumer: the
//! markers that make a string in code a path, and the path prefixes each
//! service owns. Both come from the services' operations (or the manifest),
//! so nothing about any particular API's layout is assumed.

use std::collections::{BTreeMap, BTreeSet};

/// Paths the system's services serve, as consumers' code is read.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct System {
    /// Leading segments of every operation path, such as `/v1/`.
    pub markers: Vec<String>,
    /// Each service's path prefixes, as segments.
    prefixes: BTreeMap<String, Vec<Vec<String>>>,
}

impl System {
    /// From each service's operation paths and declared prefixes. A service
    /// that declares no prefixes owns the longest prefix all its operations
    /// share, if they share one.
    pub fn new(
        operation_paths: &BTreeMap<String, BTreeSet<String>>,
        declared: &BTreeMap<String, Vec<String>>,
    ) -> Self {
        let markers =
            lemma_http::path_markers(operation_paths.values().flatten().map(String::as_str));
        let mut prefixes = BTreeMap::new();
        for (service, paths) in operation_paths {
            let owned: Vec<Vec<String>> =
                match declared.get(service).filter(|list| !list.is_empty()) {
                    Some(list) => list.iter().map(|prefix| segments(prefix)).collect(),
                    None => common_prefix(paths).into_iter().collect(),
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

    fn paths(service: &str, list: &[&str]) -> (String, BTreeSet<String>) {
        (
            service.to_owned(),
            list.iter().map(|path| (*path).to_owned()).collect(),
        )
    }

    #[test]
    fn services_own_their_shared_prefix_unless_they_declare_one() {
        let operations = BTreeMap::from([
            paths("billing", &["/v1/invoices", "/v1/invoices/{}"]),
            paths("users", &["/v1/users/{}", "/v2/users"]),
            paths("search", &["/api/search", "/api/suggest"]),
        ]);
        let declared = BTreeMap::from([("search".to_owned(), vec!["/api/search/".to_owned()])]);

        let system = System::new(&operations, &declared);

        assert_eq!(system.markers, ["/api/", "/v1/", "/v2/"]);
        assert_eq!(system.addressed_to("/v1/invoices/{}/lines"), ["billing"]);
        assert_eq!(system.addressed_to("/v1/users/{}"), Vec::<String>::new());
        assert_eq!(system.addressed_to("/api/search/new"), ["search"]);
        assert_eq!(system.addressed_to("/api/trpc/x"), Vec::<String>::new());
    }
}
