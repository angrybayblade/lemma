//! Clients the description names whose functions no read call reaches. A
//! client exists to be called, so where none of its calls was read, its
//! callers are probably somewhere Lemma did not read or written in a way
//! it does not follow: a clean result then says nothing of them. See
//! `.specs/16-verifying-descriptions.md`, "Missing, suspected: candidates".

use std::collections::{BTreeMap, BTreeSet};

use super::outside::Candidate;

/// The candidate for a client none of whose functions has a call read.
const CLIENT_WITHOUT_CALLERS: &str = "client_without_callers";

/// How fact writing names a client built at install time, which is known
/// only from the calls of it.
const BUILT_AT_INSTALL: &str = " (generated)";

/// What the rows say of one client's functions.
#[derive(Default)]
struct Functions<'r> {
    /// How `calls_client` names each one.
    keys: BTreeSet<&'r str>,
    /// The files they are in, or the documents they are named from.
    files: BTreeSet<&'r str>,
    /// The operations they request, as method and path.
    operations: BTreeSet<(&'r str, &'r str)>,
}

/// One candidate for each client among the `client_operation` rows of
/// `relations` that none of the `calls_client` rows calls.
pub(super) fn clients_without_callers(
    relations: &BTreeMap<String, Vec<Vec<String>>>,
) -> Vec<Candidate> {
    let rows = |name: &str| relations.get(name).map_or(&[][..], Vec::as_slice);
    let called: BTreeSet<&str> = rows("calls_client")
        .iter()
        .filter_map(|row| row.get(1))
        .map(String::as_str)
        .collect();
    let mut clients: BTreeMap<&str, Functions<'_>> = BTreeMap::new();
    for row in rows("client_operation") {
        let [client, key, method, path, file, ..] = &row[..] else {
            continue;
        };
        let functions = clients.entry(client).or_default();
        functions.keys.insert(key);
        functions.files.insert(file);
        functions.operations.insert((method, path));
    }
    clients
        .into_iter()
        .filter(|(client, _)| !client.ends_with(BUILT_AT_INSTALL))
        .filter(|(_, functions)| functions.keys.is_disjoint(&called))
        .map(|(client, functions)| Candidate {
            name: CLIENT_WITHOUT_CALLERS.to_owned(),
            location: client.to_owned(),
            files: functions.files.len(),
            operations: functions.operations.len(),
            places: Vec::new(),
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn rows(rows: &[&[&str]]) -> Vec<Vec<String>> {
        rows.iter()
            .map(|row| row.iter().map(|cell| (*cell).to_owned()).collect())
            .collect()
    }

    fn relations(calls: &[&[&str]]) -> BTreeMap<String, Vec<Vec<String>>> {
        let functions: &[&[&str]] = &[
            &[
                "sdk",
                "src/sdk.ts:addPet",
                "POST",
                "/v1/pets",
                "src/sdk.ts",
                "3",
            ],
            &[
                "sdk",
                "src/sdk.ts:listPets",
                "GET",
                "/v1/pets",
                "src/sdk.ts",
                "9",
            ],
            &[
                "billing (generated)",
                "billing:pay",
                "POST",
                "/v1/pay",
                "openapi.json",
                "0",
            ],
        ];
        BTreeMap::from([
            ("client_operation".to_owned(), rows(functions)),
            ("calls_client".to_owned(), rows(calls)),
        ])
    }

    #[test]
    fn a_client_no_call_reaches_is_a_candidate_with_what_it_offers() {
        let found = clients_without_callers(&relations(&[]));

        assert_eq!(found.len(), 1, "{found:?}");
        assert_eq!(found[0].name, "client_without_callers");
        assert_eq!(found[0].location, "sdk");
        assert_eq!((found[0].files, found[0].operations), (1, 2));
    }

    #[test]
    fn a_client_with_one_call_read_is_no_candidate() {
        let calls: &[&[&str]] = &[&[
            "web:src/a.ts::save",
            "src/sdk.ts:addPet",
            "web:src/a.ts",
            "4",
        ]];

        let found = clients_without_callers(&relations(calls));

        assert_eq!(found, Vec::new());
    }
}
