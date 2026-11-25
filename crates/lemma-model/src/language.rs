//! The languages Lemma reads, and what it knows about each before parsing a
//! file: its name, the files that are its own, and the language it shares
//! projects with.
//!
//! This is the one place that lists them. Other crates ask here instead of
//! keeping a list of their own, so that a language is added, or later moved
//! out of Lemma's core, by changing one table.

use std::fmt;
use std::str::FromStr;

use serde::{Deserialize, Serialize};

use crate::UnknownName;

/// A source language Lemma can extract symbols from.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Language {
    /// TypeScript (`.ts`, `.mts`, `.cts`).
    TypeScript,
    /// TypeScript with JSX (`.tsx`).
    Tsx,
    /// Python (`.py`, `.pyi`).
    Python,
    /// Go (`.go`).
    Go,
    /// Rust (`.rs`).
    Rust,
}

/// What Lemma knows about one language without parsing anything.
struct Entry {
    language: Language,
    /// The lowercase name used in storage, configuration and output.
    name: &'static str,
    /// Extensions of the files indexed under this language, without the dot.
    extensions: &'static [&'static str],
    /// The language whose projects, names and tools this one shares.
    family: Language,
}

/// One entry per language, in the order of [`Language::ALL`].
const ENTRIES: [Entry; 5] = [
    Entry {
        language: Language::TypeScript,
        name: "typescript",
        extensions: &["ts", "mts", "cts"],
        family: Language::TypeScript,
    },
    Entry {
        language: Language::Tsx,
        name: "tsx",
        extensions: &["tsx"],
        family: Language::TypeScript,
    },
    Entry {
        language: Language::Python,
        name: "python",
        extensions: &["py", "pyi"],
        family: Language::Python,
    },
    Entry {
        language: Language::Go,
        name: "go",
        extensions: &["go"],
        family: Language::Go,
    },
    Entry {
        language: Language::Rust,
        name: "rust",
        extensions: &["rs"],
        family: Language::Rust,
    },
];

/// JavaScript files. JavaScript has no language of its own in Lemma's
/// model: its files are not indexed for symbols, and where code is read for
/// requests, routes and names, the TSX grammar parses them, JSX included,
/// because TypeScript's syntax is a superset of JavaScript's.
// TODO: when languages move out of Lemma's core, decide whether JavaScript
// becomes a language of its own, indexed like the others.
const JAVASCRIPT_EXTENSIONS: [&str; 4] = ["js", "jsx", "mjs", "cjs"];

/// Languages Lemma knows of and has no reader for, each with the extensions
/// its files carry. Nothing in such a file is read, so `lemma verify` says
/// how many of them a description includes.
// TODO: when languages move out of Lemma's core, this becomes what
// detection proposes a language pack from.
const UNREAD_LANGUAGES: [(&str, &[&str]); 13] = [
    ("Astro", &["astro"]),
    ("C#", &["cs"]),
    ("Dart", &["dart"]),
    ("Elixir", &["ex", "exs"]),
    ("Handlebars", &["hbs", "handlebars"]),
    ("Java", &["java"]),
    ("Kotlin", &["kt", "kts"]),
    ("PHP", &["php"]),
    ("Ruby", &["rb", "erb"]),
    ("Scala", &["scala"]),
    ("Svelte", &["svelte"]),
    ("Swift", &["swift"]),
    ("Vue", &["vue"]),
];

/// The name of the language a file with `extension` is written in, when
/// Lemma knows the language and reads nothing from its files.
pub fn unread_language(extension: &str) -> Option<&'static str> {
    UNREAD_LANGUAGES
        .iter()
        .find(|(_, extensions)| extensions.contains(&extension))
        .map(|(name, _)| *name)
}

impl Language {
    /// Every supported language, in a stable order.
    pub const ALL: [Language; 5] = [
        Self::TypeScript,
        Self::Tsx,
        Self::Python,
        Self::Go,
        Self::Rust,
    ];

    /// Extensions of the files Lemma reads requests, routes and names from,
    /// without the leading dot. JavaScript files are among them though no
    /// language indexes them, and Python stub files (`.pyi`) are not though
    /// Python indexes them: a stub declares names and calls nothing.
    pub const CODE_EXTENSIONS: [&'static str; 11] = [
        "ts", "tsx", "mts", "cts", "js", "jsx", "mjs", "cjs", "py", "go", "rs",
    ];

    /// Picks the language a file with this extension is indexed under, the
    /// extension given without the leading dot.
    pub fn from_extension(extension: &str) -> Option<Self> {
        ENTRIES
            .iter()
            .find(|entry| entry.extensions.contains(&extension))
            .map(|entry| entry.language)
    }

    /// Picks the language whose grammar parses a file with this extension:
    /// the one it is indexed under, or TSX for a JavaScript file.
    pub fn parsing(extension: &str) -> Option<Self> {
        if JAVASCRIPT_EXTENSIONS.contains(&extension) {
            return Some(Self::Tsx);
        }
        Self::from_extension(extension)
    }

    /// Whether files with this extension are read for requests, routes and
    /// names (see [`Language::CODE_EXTENSIONS`]).
    pub fn is_code_extension(extension: &str) -> bool {
        Self::CODE_EXTENSIONS.contains(&extension)
    }

    /// File extensions of this language, without the leading dot.
    pub fn extensions(self) -> &'static [&'static str] {
        self.entry().extensions
    }

    /// The lowercase name used in storage, configuration, and output.
    pub fn as_str(self) -> &'static str {
        self.entry().name
    }

    /// The language whose projects, names and tools this one shares:
    /// TypeScript for TSX, and itself for every other. A name in one
    /// language can refer to a definition in another only within a family.
    #[must_use]
    pub fn family(self) -> Self {
        self.entry().family
    }

    /// This language's place in [`Language::ALL`], for keeping one of
    /// something per language in an array.
    pub fn position(self) -> usize {
        self as usize
    }

    fn entry(self) -> &'static Entry {
        &ENTRIES[self.position()]
    }
}

impl fmt::Display for Language {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.pad(self.as_str())
    }
}

impl FromStr for Language {
    type Err = UnknownName;

    fn from_str(name: &str) -> Result<Self, Self::Err> {
        ENTRIES
            .iter()
            .find(|entry| entry.name == name)
            .map(|entry| entry.language)
            .ok_or_else(|| UnknownName(name.to_owned()))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn entries_are_in_the_order_of_all() {
        let listed: Vec<Language> = ENTRIES.iter().map(|entry| entry.language).collect();

        assert_eq!(listed, Language::ALL);
    }

    #[test]
    fn no_extension_belongs_to_two_languages() {
        let mut extensions: Vec<&str> = ENTRIES
            .iter()
            .flat_map(|entry| entry.extensions.iter().copied())
            .chain(JAVASCRIPT_EXTENSIONS)
            .collect();
        let listed = extensions.len();

        extensions.sort_unstable();
        extensions.dedup();

        assert_eq!(extensions.len(), listed);
    }

    #[test]
    fn a_language_is_read_from_configuration_by_its_name() {
        use serde::de::IntoDeserializer;
        use serde::de::value::{Error, StrDeserializer};

        for language in Language::ALL {
            let name: StrDeserializer<'_, Error> = language.as_str().into_deserializer();

            assert_eq!(Language::deserialize(name).unwrap(), language);
            assert_eq!(language.as_str().parse::<Language>().unwrap(), language);
        }
    }

    #[test]
    fn tsx_is_in_the_typescript_family_and_the_rest_are_their_own() {
        assert_eq!(Language::Tsx.family(), Language::TypeScript);
        for language in [
            Language::TypeScript,
            Language::Python,
            Language::Go,
            Language::Rust,
        ] {
            assert_eq!(language.family(), language);
        }
    }

    #[test]
    fn javascript_is_parsed_as_tsx_and_indexed_under_no_language() {
        for extension in JAVASCRIPT_EXTENSIONS {
            assert_eq!(Language::parsing(extension), Some(Language::Tsx));
            assert_eq!(Language::from_extension(extension), None);
        }
    }

    #[test]
    fn every_code_extension_has_a_grammar() {
        for extension in Language::CODE_EXTENSIONS {
            assert!(Language::parsing(extension).is_some(), "{extension}");
        }
    }

    #[test]
    fn python_stub_files_are_indexed_and_not_read_as_code() {
        assert_eq!(Language::from_extension("pyi"), Some(Language::Python));
        assert!(!Language::is_code_extension("pyi"));
    }

    #[test]
    fn a_language_lemma_does_not_read_is_named_by_its_file_extensions() {
        assert_eq!(unread_language("vue"), Some("Vue"));
        assert_eq!(unread_language("erb"), Some("Ruby"));
        assert_eq!(unread_language("md"), None);
    }

    #[test]
    fn no_file_type_is_both_read_and_listed_as_not_read() {
        let unread = UNREAD_LANGUAGES
            .iter()
            .flat_map(|(_, extensions)| extensions.iter());

        for extension in unread {
            assert!(!Language::is_code_extension(extension), "{extension}");
            assert_eq!(Language::from_extension(extension), None, "{extension}");
        }
    }
}
