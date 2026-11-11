//! Terms a file's code facts are indexed by: what fact writing can turn
//! into a call of an operation, taken before any resolution that reads
//! other files, so the files that may call an operation can be found
//! without reading every file's facts.
//!
//! Every term is a superset of what it stands for. A file whose facts can
//! write a call of an operation carries one of the terms that operation is
//! looked up by (see `.specs/11-change-driven-evaluation.md`):
//!
//! - `path:` each path the file names with a method or alone: `calls_path`
//!   rows carry these paths, matched to an operation's path exactly;
//! - `chain:` each operation-table chain it calls: `calls_client` rows;
//! - `name:` the last name of each member call that could resolve, and each
//!   name it imports, normalized: client methods (a chain of three names or
//!   more) and generated functions (a call rooted at an import) are matched
//!   by name, and an imported function may be called by another local name;
//! - `reexports` when it re-exports a module whole, which every consumer
//!   file's generated calls may go through.

use lemma_http::CodeFacts;

use crate::clients::normalized;

/// The term of a file that re-exports a module whole.
pub(crate) const REEXPORTS: &str = "reexports";

/// The term for calls of a path.
pub(crate) fn path_term(path: &str) -> String {
    format!("path:{path}")
}

/// The term for calls through an operation table's chain.
pub(crate) fn chain_term(chain: &str) -> String {
    format!("chain:{chain}")
}

/// The term for calls of something named `name`, normalized as client
/// methods are matched.
pub(crate) fn name_term(name: &str) -> String {
    format!("name:{}", normalized(name))
}

/// The term for calls through a chain whose method is named `name` under a
/// resource named `resource` (`billing.invoices.create` has the pair
/// `invoices`, `create`): a client method resolves only for a chain naming one
/// of its resources immediately before its name (see `Clients::resolve`).
pub(crate) fn pair_term(resource: &str, name: &str) -> String {
    format!("pair:{}.{}", normalized(resource), normalized(name))
}

/// The resource a chain names immediately before its last name, as
/// `Clients::resolve` reads it: the last name between the root and the
/// method that is more than one letter long.
pub(crate) fn immediate_resource(parts: &[String]) -> Option<&String> {
    let (_, before) = parts.split_last()?;
    before
        .iter()
        .skip(1)
        .rfind(|part| normalized(part).len() > 1)
}

/// The terms `facts` are indexed by, sorted and without repeats.
pub(crate) fn code_terms(facts: &CodeFacts) -> Vec<String> {
    let mut terms = Vec::new();
    let paths = facts
        .path_references
        .iter()
        .map(|reference| &reference.path)
        .chain(facts.method_paths.iter().map(|item| &item.path));
    terms.extend(paths.map(|path| path_term(path)));
    terms.extend(
        facts
            .client_calls
            .iter()
            .map(|call| chain_term(&call.chain)),
    );
    // Only calls that can resolve: a client method needs a resource before
    // its name (`client.tools.list`), a generated function an imported root
    // (`api.listTools`, `listTools`).
    let imported_roots: Vec<&str> = facts
        .import_bindings
        .iter()
        .map(|binding| binding.local.as_str())
        .collect();
    for call in &facts.member_calls {
        let Some(last) = call.parts.last() else {
            continue;
        };
        if call.parts.len() >= 3 {
            // A client method: found by its resource and its name.
            if let Some(resource) = immediate_resource(&call.parts) {
                terms.push(pair_term(resource, last));
            }
            continue;
        }
        let is_imported = call
            .parts
            .first()
            .is_some_and(|root| imported_roots.contains(&root.as_str()));
        if is_imported {
            // A generated function: found by its name.
            terms.push(name_term(last));
        }
    }
    let imported = facts
        .import_bindings
        .iter()
        .filter_map(|binding| binding.imported.as_ref());
    terms.extend(imported.map(|name| name_term(name)));
    if !facts.reexports.is_empty() {
        terms.push(REEXPORTS.to_owned());
    }
    terms.sort();
    terms.dedup();
    terms
}

#[cfg(test)]
mod tests {
    use super::*;
    use lemma_http::extract;
    use lemma_syntax::Extractor;

    #[test]
    fn a_file_is_indexed_by_what_it_calls_and_imports() {
        let source = "import { listTools as lt } from './generated/api'\n\
                      export * as api from './generated/api'\n\
                      async function load(client) {\n\
                        await fetch('/v1/users', { method: 'POST' })\n\
                        await client.api_keys.update_status()\n\
                        return lt()\n\
                      }\n";
        let options = lemma_http::CodeOptions {
            path_markers: vec!["/v1/".to_owned()],
            ..lemma_http::CodeOptions::default()
        };
        let facts = extract("load.ts", source, &options, &mut Extractor::new().unwrap()).unwrap();

        let terms = code_terms(&facts);

        for expected in [
            "path:/v1/users",
            "pair:apikeys.updatestatus",
            "name:listtools",
            "name:lt",
            REEXPORTS,
        ] {
            assert!(
                terms.iter().any(|term| term == expected),
                "{expected} in {terms:?}"
            );
        }
        assert!(
            !terms.iter().any(|term| term == "name:updatestatus"),
            "{terms:?}"
        );
    }

    fn parts(text: &str) -> Vec<String> {
        text.split('.').map(str::to_owned).collect()
    }

    #[test]
    fn a_chains_resource_is_the_name_before_its_method() {
        let chain = parts("client.billing.invoices.create");

        assert_eq!(
            immediate_resource(&chain).map(String::as_str),
            Some("invoices")
        );
    }

    #[test]
    fn a_chains_resource_skips_one_letter_names_as_resolving_does() {
        let chain = parts("client.tools.x.list");

        assert_eq!(
            immediate_resource(&chain).map(String::as_str),
            Some("tools")
        );
    }

    #[test]
    fn a_chain_of_its_root_and_method_has_no_resource() {
        let chain = parts("client.x.list");

        assert_eq!(immediate_resource(&chain), None);
    }
}
