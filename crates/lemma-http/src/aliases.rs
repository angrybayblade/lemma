//! Route aliases a service's documents and routes imply. A service that
//! documents a new version beside an old one (`/v2/` beside `/v1/`) often
//! serves the new paths with the old routes, rewriting the prefix before
//! routing. Its documents then describe `/v2/x` while its code only writes
//! `/v1/x`. Seen across a whole document, that is an alias: `/v1/` routes
//! are also served under `/v2/`.
//!
//! The same holds for the path a document puts every operation under with
//! its `servers` URL (or Swagger's `basePath`): routes written as
//! `/api/articles` serve the documented `/articles` of a document served at
//! `https://example.test/api`.

use std::collections::{BTreeMap, BTreeSet};

/// The aliases, from the prefix routes are written under to the prefix
/// they are also served under (`/v1/` to `/v2/`), that `documents` and
/// `routes` imply. `documents` are each document's operations and `routes`
/// the routes the code writes, both as method and canonical path.
///
/// A document is aliased to another's prefix when most of its operations
/// that have no route of their own have one at the same path under that
/// prefix, and at least two do. Measuring among the unrouted operations
/// lets a document whose new paths have routes of their own keep the alias
/// its old paths imply. Each document's prefix is the static prefix its
/// paths share.
///
/// `served_under` holds the paths the documents' `servers` put their
/// operations under (see [`crate::Operation::served_under`]): routes
/// written under one are served at the documented paths without it.
pub fn implied_aliases(
    documents: &[Vec<(String, String)>],
    routes: &BTreeSet<(String, String)>,
    served_under: &BTreeSet<String>,
) -> BTreeMap<String, String> {
    let prefixes: Vec<String> = documents
        .iter()
        .map(|operations| shared_prefix(operations))
        .collect();
    let mut aliases = BTreeMap::new();
    for (operations, served) in documents.iter().zip(&prefixes) {
        if served.is_empty() {
            continue;
        }
        let unrouted: Vec<&(String, String)> = operations
            .iter()
            .filter(|operation| !routes.contains(*operation))
            .collect();
        // A version's sibling sits at the same depth: `/v1` beside `/v2`,
        // even when every `/v1` path happens to share `/v1/users`.
        let depth = served.matches('/').count();
        let siblings: BTreeSet<String> = prefixes
            .iter()
            .map(|other| truncated(other, depth))
            .filter(|other| !other.is_empty() && other != served)
            .collect();
        for written in &siblings {
            let routed_there = unrouted
                .iter()
                .filter(|(method, path)| {
                    let rest = &path[served.len()..];
                    routes.contains(&(method.clone(), format!("{written}{rest}")))
                })
                .count();
            if routed_there >= 2 && routed_there * 2 > unrouted.len() {
                aliases.insert(format!("{written}/"), format!("{served}/"));
            }
        }
    }
    for prefix in served_under {
        aliases
            .entry(format!("{prefix}/"))
            .or_insert_with(|| "/".to_owned());
    }
    aliases
}

/// `prefix` cut to its first `depth` segments; empty if it has fewer.
fn truncated(prefix: &str, depth: usize) -> String {
    let segments: Vec<&str> = prefix.split('/').collect();
    if segments.len() <= depth {
        return String::new();
    }
    segments[..=depth].join("/")
}

/// The static prefix every path shares, segment by segment (`/v2`); empty
/// if they share none or there are no paths.
fn shared_prefix(operations: &[(String, String)]) -> String {
    let mut paths = operations
        .iter()
        .map(|(_, path)| path.split('/').collect::<Vec<_>>());
    let Some(mut prefix) = paths.next() else {
        return String::new();
    };
    for path in paths {
        let shared = prefix
            .iter()
            .zip(&path)
            .take_while(|(left, right)| left == right)
            .count();
        prefix.truncate(shared);
    }
    let static_length = prefix
        .iter()
        .take_while(|segment| **segment != "{}")
        .count();
    prefix.truncate(static_length);
    prefix.join("/")
}

#[cfg(test)]
mod tests {
    use super::*;

    fn operations(list: &[(&str, &str)]) -> Vec<(String, String)> {
        list.iter()
            .map(|(method, path)| ((*method).to_owned(), (*path).to_owned()))
            .collect()
    }

    #[test]
    fn a_new_version_served_by_the_old_routes_is_an_alias() {
        let documents = [
            operations(&[("GET", "/v1/users"), ("POST", "/v1/users/{}")]),
            operations(&[
                ("GET", "/v2/users"),
                ("POST", "/v2/users/{}"),
                ("GET", "/v2/teams"),
            ]),
        ];
        let routes = operations(&[
            ("GET", "/v1/users"),
            ("POST", "/v1/users/{}"),
            ("GET", "/v2/teams"),
        ])
        .into_iter()
        .collect();

        let aliases = implied_aliases(&documents, &routes, &BTreeSet::new());

        assert_eq!(
            aliases,
            BTreeMap::from([("/v1/".to_owned(), "/v2/".to_owned())])
        );
    }

    #[test]
    fn routes_of_its_own_for_new_paths_keep_a_documents_alias() {
        let documents = [
            operations(&[("GET", "/v1/a"), ("GET", "/v1/b"), ("GET", "/v1/c")]),
            operations(&[
                ("GET", "/v2/a"),
                ("GET", "/v2/b"),
                ("GET", "/v2/new1"),
                ("GET", "/v2/new2"),
                ("GET", "/v2/new3"),
            ]),
        ];
        let routes = operations(&[
            ("GET", "/v1/a"),
            ("GET", "/v1/b"),
            ("GET", "/v1/c"),
            ("GET", "/v2/new1"),
            ("GET", "/v2/new2"),
            ("GET", "/v2/new3"),
        ])
        .into_iter()
        .collect();

        let aliases = implied_aliases(&documents, &routes, &BTreeSet::new());

        assert_eq!(
            aliases,
            BTreeMap::from([("/v1/".to_owned(), "/v2/".to_owned())])
        );
    }

    #[test]
    fn the_path_a_documents_servers_put_it_under_is_an_alias_to_the_root() {
        let documents = [operations(&[("GET", "/articles"), ("GET", "/tags")])];
        let routes = operations(&[("GET", "/api/articles")])
            .into_iter()
            .collect();
        let served_under = BTreeSet::from(["/api".to_owned()]);

        let aliases = implied_aliases(&documents, &routes, &served_under);

        assert_eq!(
            aliases,
            BTreeMap::from([("/api/".to_owned(), "/".to_owned())])
        );
    }

    #[test]
    fn routes_under_a_prefix_no_document_is_served_at_imply_nothing() {
        let documents = [operations(&[("GET", "/articles"), ("GET", "/tags")])];
        let routes = operations(&[("GET", "/api/articles"), ("GET", "/api/tags")])
            .into_iter()
            .collect();

        let aliases = implied_aliases(&documents, &routes, &BTreeSet::new());

        assert_eq!(aliases, BTreeMap::new());
    }

    #[test]
    fn documents_with_their_own_routes_imply_nothing() {
        let documents = [
            operations(&[("GET", "/v1/users"), ("GET", "/v1/teams")]),
            operations(&[("GET", "/admin/users"), ("GET", "/admin/teams")]),
        ];
        let routes = operations(&[("GET", "/v1/users"), ("GET", "/admin/teams")])
            .into_iter()
            .collect();

        assert_eq!(
            implied_aliases(&documents, &routes, &BTreeSet::new()),
            BTreeMap::new()
        );
    }
}
