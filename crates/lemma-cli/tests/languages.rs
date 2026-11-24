//! What Lemma knows of each language stays in the modules that own it.
//!
//! A language is named, its file extensions are listed and its grammar is
//! used only in the places below. Anywhere else asks `lemma_model::Language`
//! or the owning module, so that a language can be added, or later moved
//! out of Lemma's core, without a search through every crate.

#![allow(
    clippy::unwrap_used,
    clippy::panic,
    reason = "language knowledge found in a new place should fail loudly"
)]

use std::collections::BTreeSet;
use std::fs;
use std::path::{Path, PathBuf};

const LANGUAGES: [&str; 5] = ["TypeScript", "Tsx", "Python", "Go", "Rust"];

const EXTENSIONS: [&str; 12] = [
    "ts", "tsx", "mts", "cts", "js", "jsx", "mjs", "cjs", "py", "pyi", "go", "rs",
];

const GRAMMARS: [&str; 4] = [
    "tree_sitter_typescript",
    "tree_sitter_python",
    "tree_sitter_go",
    "tree_sitter_rust",
];

/// Where production code may name a language, each with what it holds. A
/// path ending in `/` covers a directory.
const NAMES_A_LANGUAGE: [(&str, &str); 11] = [
    ("lemma-model/src/language.rs", "the table of languages"),
    (
        "lemma-syntax/src/languages.rs",
        "grammars, definition queries and repair",
    ),
    ("lemma-http/src/code/", "the readers of requests and routes"),
    ("lemma-graph/src/schema/", "the readers of types as schemas"),
    // TODO: to be gathered behind one interface when languages move out of
    // Lemma's core.
    ("lemma-graph/src/proof/", "the readers of source for proofs"),
    (
        "lemma-graph/src/adapters/lazy.rs",
        "the adapter that reads zod schemas",
    ),
    // TODO: scaffolds are to come from language packs (`.specs/18`).
    (
        "lemma-lm/src/new.rs",
        "what a behavior's script needs from its service",
    ),
    (
        "lemma-lm/src/scaffold.rs",
        "the scripts a behavior is scaffolded as",
    ),
    (
        "lemma-providers/src/languages.rs",
        "the names language servers use",
    ),
    (
        "lemma-query/src/languages.rs",
        "how comments and strings are written",
    ),
    (
        "lemma-query/src/unseen.rs",
        "what a language server may not have read",
    ),
];

/// Where production code may list code file extensions beyond the places
/// that may name a language.
const LISTS_EXTENSIONS: [(&str, &str); 3] = [
    (
        "lemma-graph/src/adapters/registry.rs",
        "the interpreter each adapter script runs with",
    ),
    (
        "lemma-graph/src/reach.rs",
        "module files the name walk leaves out",
    ),
    (
        "lemma-lang/src/stdlib/mod.rs",
        "the scripts an adapter may name",
    ),
];

/// The only place a grammar is named.
const NAMES_A_GRAMMAR: [(&str, &str); 1] = [(
    "lemma-syntax/src/languages.rs",
    "grammars, definition queries and repair",
)];

fn crates() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("..")
}

/// Every Rust source file of every crate, as a path from `crates/`, with
/// its production code: the text before its test module, without comments.
fn production_sources() -> Vec<(String, String)> {
    let mut sources = Vec::new();
    for entry in fs::read_dir(crates()).unwrap() {
        let source_directory = entry.unwrap().path().join("src");
        if source_directory.is_dir() {
            collect(&source_directory, &mut sources);
        }
    }
    sources.sort();
    sources
}

fn collect(directory: &Path, sources: &mut Vec<(String, String)>) {
    for entry in fs::read_dir(directory).unwrap() {
        let path = entry.unwrap().path();
        let name = path.file_name().unwrap().to_string_lossy().into_owned();
        if path.is_dir() {
            if name != "tests" {
                collect(&path, sources);
            }
            continue;
        }
        let is_rust = path.extension().is_some_and(|extension| extension == "rs");
        if !is_rust || name.ends_with("tests.rs") {
            continue;
        }
        let text = fs::read_to_string(&path).unwrap();
        let production = text.split("#[cfg(test)]").next().unwrap_or("");
        let code: Vec<&str> = production
            .lines()
            .filter(|line| !line.trim_start().starts_with("//"))
            .collect();
        let relative = path.strip_prefix(crates()).unwrap();
        sources.push((
            relative.to_string_lossy().replace('\\', "/"),
            code.join("\n"),
        ));
    }
}

fn names_a_language(line: &str) -> bool {
    line.match_indices("Language::").any(|(at, matched)| {
        let after = &line[at + matched.len()..];
        LANGUAGES.iter().any(|language| {
            after.strip_prefix(language).is_some_and(|rest| {
                !rest.starts_with(|next: char| next.is_alphanumeric() || next == '_')
            })
        })
    })
}

/// Whether a line writes two or more code file extensions as strings, as a
/// list or a match arm does. One alone is too often something else.
fn lists_extensions(line: &str) -> bool {
    let written = EXTENSIONS
        .iter()
        .filter(|extension| {
            line.contains(&format!("\"{extension}\""))
                || line.contains(&format!("\".{extension}\""))
        })
        .count();
    written >= 2
}

fn names_a_grammar(line: &str) -> bool {
    GRAMMARS.iter().any(|grammar| line.contains(grammar))
}

fn is_under(path: &str, allowed: &[(&str, &str)]) -> bool {
    allowed.iter().any(|(place, _)| covers(place, path))
}

fn covers(place: &str, path: &str) -> bool {
    if place.ends_with('/') {
        path.starts_with(place)
    } else {
        path == place
    }
}

/// The files with a line `found` holds for.
fn files_where(found: fn(&str) -> bool) -> BTreeSet<String> {
    production_sources()
        .into_iter()
        .filter(|(_, code)| code.lines().any(found))
        .map(|(path, _)| path)
        .collect()
}

/// The places of `allowed` that cover none of `files`: entries to delete.
fn unused<'a>(allowed: &[(&'a str, &'a str)], files: &BTreeSet<String>) -> Vec<&'a str> {
    allowed
        .iter()
        .filter(|(place, _)| !files.iter().any(|file| covers(place, file)))
        .map(|(place, _)| *place)
        .collect()
}

#[test]
fn a_language_is_named_only_where_its_knowledge_lives() {
    let naming = files_where(names_a_language);

    let elsewhere: Vec<&String> = naming
        .iter()
        .filter(|file| !is_under(file, &NAMES_A_LANGUAGE))
        .collect();

    assert!(
        elsewhere.is_empty(),
        "these files name a language; ask `lemma_model::Language` or a `languages` module instead: {elsewhere:?}"
    );
    assert_eq!(unused(&NAMES_A_LANGUAGE, &naming), Vec::<&str>::new());
}

#[test]
fn code_file_extensions_are_listed_only_where_a_language_is_known() {
    let listing = files_where(lists_extensions);
    let allowed: Vec<(&str, &str)> = NAMES_A_LANGUAGE
        .into_iter()
        .chain(LISTS_EXTENSIONS)
        .collect();

    let elsewhere: Vec<&String> = listing
        .iter()
        .filter(|file| !is_under(file, &allowed))
        .collect();

    assert!(
        elsewhere.is_empty(),
        "these files list code file extensions; ask `lemma_model::Language` instead: {elsewhere:?}"
    );
    assert_eq!(unused(&LISTS_EXTENSIONS, &listing), Vec::<&str>::new());
}

#[test]
fn a_grammar_is_named_only_by_the_syntax_crate() {
    let naming = files_where(names_a_grammar);

    let elsewhere: Vec<&String> = naming
        .iter()
        .filter(|file| !is_under(file, &NAMES_A_GRAMMAR))
        .collect();

    assert!(
        elsewhere.is_empty(),
        "these files name a grammar; ask `lemma_syntax::parser` instead: {elsewhere:?}"
    );
    assert_eq!(unused(&NAMES_A_GRAMMAR, &naming), Vec::<&str>::new());
}
