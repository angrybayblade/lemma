//! What a change can reach (spec 17): the symbols whose code mentions,
//! directly or through other symbols, a name a change touched.
//!
//! Mentions are names, not resolved references, so this over-approximates
//! on purpose: a symbol that mentions `save` is taken to reach every `save`.
//! That is the safe direction, since a symbol wrongly left out is a finding
//! missed, while one wrongly kept only costs work.

use std::collections::{BTreeMap, BTreeSet, VecDeque};

use crate::read::{Index, Root};
use crate::{FileRecord, StoreError};

/// A symbol by its file and qualified name.
pub type SymbolAddress = (String, String);

/// The symbols of `files` that can reach a symbol named in `changed`,
/// the changed ones among them. A symbol's mentions include the names
/// defined inside it, so a class is reached through its changed method.
/// A changed name no symbol mentions reaches only itself.
pub fn reaching(
    files: &[(String, FileRecord)],
    changed: &[SymbolAddress],
) -> BTreeSet<SymbolAddress> {
    let mentioned_by = mentioners(files);
    let found = reaching_with(changed, |name| {
        let addresses = mentioned_by.get(name).map_or_else(Vec::new, |found| {
            found
                .iter()
                .map(|(path, symbol)| ((*path).to_owned(), (*symbol).to_owned()))
                .collect()
        });
        Ok::<_, std::convert::Infallible>(addresses)
    });
    match found {
        Ok(reached) => reached,
        Err(never) => match never {},
    }
}

impl Index {
    /// The symbols of `root` that can reach a symbol named in `changed`, as
    /// [`reaching`] finds them, read from the root's reverse mention lists
    /// instead of from every file.
    pub fn reaching(
        &self,
        root: &Root,
        changed: &[SymbolAddress],
    ) -> Result<BTreeSet<SymbolAddress>, StoreError> {
        let view = self.view(root)?;
        reaching_with(changed, |name| {
            let mut addresses = Vec::new();
            for at in view.mentioning(name)? {
                let segment = view.segment(at.segment)?;
                let row = segment.symbol(at.symbol)?;
                let path = segment.path(row.file)?.to_owned();
                addresses.push((path, row.qualified_name.to_owned()));
            }
            Ok(addresses)
        })
    }
}

/// Follows names outwards from `changed`; `mentioners` gives the symbols
/// whose code mentions a name.
fn reaching_with<E>(
    changed: &[SymbolAddress],
    mut mentioners: impl FnMut(&str) -> Result<Vec<SymbolAddress>, E>,
) -> Result<BTreeSet<SymbolAddress>, E> {
    let mut reached: BTreeSet<SymbolAddress> = changed.iter().cloned().collect();
    let mut names_seen: BTreeSet<String> = BTreeSet::new();
    let mut queue: VecDeque<String> = changed
        .iter()
        .map(|(_, name)| last_name(name).to_owned())
        .collect();
    while let Some(name) = queue.pop_front() {
        if !names_seen.insert(name.clone()) {
            continue;
        }
        for address in mentioners(&name)? {
            let next = last_name(&address.1).to_owned();
            if reached.insert(address) {
                queue.push_back(next);
            }
        }
    }
    Ok(reached)
}

/// For each name, the symbols whose code mentions it.
fn mentioners(files: &[(String, FileRecord)]) -> BTreeMap<&str, Vec<(&str, &str)>> {
    let mut by_name: BTreeMap<&str, Vec<(&str, &str)>> = BTreeMap::new();
    for (path, record) in files {
        for symbol in &record.symbols {
            for mention in &symbol.mentions {
                by_name
                    .entry(mention.as_str())
                    .or_default()
                    .push((path.as_str(), symbol.qualified_name.as_str()));
            }
        }
    }
    by_name
}

/// The name code calls a symbol by: the last part of its qualified name.
fn last_name(qualified_name: &str) -> &str {
    qualified_name.rsplit('.').next().unwrap_or(qualified_name)
}

#[cfg(test)]
mod tests {
    use lemma_model::Language;
    use lemma_syntax::Extractor;

    use super::*;

    fn files(sources: &[(&str, &str)]) -> Vec<(String, FileRecord)> {
        let mut extractor = Extractor::new().unwrap();
        sources
            .iter()
            .map(|(path, code)| {
                let record = crate::read_file(&mut extractor, Language::Python, code).unwrap();
                ((*path).to_owned(), record)
            })
            .collect()
    }

    fn changed(path: &str, name: &str) -> Vec<SymbolAddress> {
        vec![(path.to_owned(), name.to_owned())]
    }

    fn names(reached: &BTreeSet<SymbolAddress>) -> Vec<&str> {
        let mut names: Vec<&str> = reached.iter().map(|(_, name)| name.as_str()).collect();
        names.sort_unstable();
        names
    }

    #[test]
    fn a_change_reaches_what_calls_it_through_other_functions() {
        let files = files(&[
            ("models.py", "def total(order):\n    return order.sum\n"),
            (
                "service.py",
                "def charge(order):\n    return total(order)\n",
            ),
            (
                "routes.py",
                "def create():\n    return charge(load())\n\ndef health():\n    return 'ok'\n",
            ),
        ]);

        let reached = reaching(&files, &changed("models.py", "total"));

        assert_eq!(names(&reached), ["charge", "create", "total"]);
    }

    #[test]
    fn a_method_is_reached_by_its_name_and_its_class_contains_it() {
        let files = files(&[
            (
                "store.py",
                "class Store:\n    def save(self, row):\n        return row\n",
            ),
            ("api.py", "def post(store):\n    return store.save(1)\n"),
        ]);

        let reached = reaching(&files, &changed("store.py", "Store.save"));

        assert_eq!(names(&reached), ["Store", "Store.save", "post"]);
    }

    #[test]
    fn cycles_end() {
        let files = files(&[(
            "a.py",
            "def ping():\n    return pong()\n\ndef pong():\n    return ping()\n",
        )]);

        let reached = reaching(&files, &changed("a.py", "ping"));

        assert_eq!(names(&reached), ["ping", "pong"]);
    }

    #[test]
    fn the_store_reaches_what_the_files_reach() {
        let files = files(&[
            ("models.py", "def total(order):\n    return order.sum\n"),
            (
                "service.py",
                "def charge(order):\n    return total(order)\n",
            ),
            (
                "routes.py",
                "def create():\n    return charge(1)\n\ndef other():\n    return 2\n",
            ),
        ]);
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("index.ls");
        let changes: Vec<crate::Change> = files
            .iter()
            .map(|(path, record)| crate::Change::Put {
                path: path.clone(),
                record: record.clone(),
                stamp: crate::FileStamp::default(),
            })
            .collect();
        let source = crate::SourceRecord {
            repository: "r".to_owned(),
            revision: "v".to_owned(),
            profile: [0; 32],
        };
        let commit = crate::Commit {
            root: "head",
            parent: None,
            source: &source,
        };
        crate::Writer::open(&path)
            .unwrap()
            .commit(&commit, &changes)
            .unwrap();
        let index = Index::open(&path, crate::ReadMode::Mapped).unwrap();

        let stored = index
            .reaching(&index.root("head").unwrap(), &changed("models.py", "total"))
            .unwrap();

        assert_eq!(stored, reaching(&files, &changed("models.py", "total")));
        assert_eq!(names(&stored), ["charge", "create", "total"]);
    }

    #[test]
    fn nothing_reaches_an_unmentioned_change() {
        let files = files(&[(
            "a.py",
            "def lonely():\n    return 1\n\ndef other():\n    return 2\n",
        )]);

        let reached = reaching(&files, &changed("a.py", "lonely"));

        assert_eq!(names(&reached), ["lonely"]);
    }
}
