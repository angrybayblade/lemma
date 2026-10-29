//! Core domain types shared by every Lemma crate.
//!
//! These types describe source snapshots, the symbols found in them, and what
//! semantic providers report about those symbols. They know nothing about
//! parsers, language servers, or storage.

use std::fmt;
use std::str::FromStr;

use serde::{Deserialize, Serialize};

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
}

impl Language {
    /// Every supported language, in a stable order.
    pub const ALL: [Language; 4] = [Self::TypeScript, Self::Tsx, Self::Python, Self::Go];

    /// Picks the language for a file extension, without the leading dot.
    pub fn from_extension(extension: &str) -> Option<Self> {
        match extension {
            "ts" | "mts" | "cts" => Some(Self::TypeScript),
            "tsx" => Some(Self::Tsx),
            "py" | "pyi" => Some(Self::Python),
            "go" => Some(Self::Go),
            _ => None,
        }
    }

    /// The lowercase name used in storage, configuration, and output.
    pub fn as_str(self) -> &'static str {
        match self {
            Self::TypeScript => "typescript",
            Self::Tsx => "tsx",
            Self::Python => "python",
            Self::Go => "go",
        }
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
        Self::ALL
            .into_iter()
            .find(|language| language.as_str() == name)
            .ok_or_else(|| UnknownName(name.to_owned()))
    }
}

/// What kind of definition a symbol is.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SymbolKind {
    /// A free-standing function, including functions assigned to constants.
    Function,
    /// A function defined on a class or type.
    Method,
    /// A class.
    Class,
    /// An interface.
    Interface,
    /// A Go struct type.
    Struct,
    /// An enum.
    Enum,
    /// A named type that is not a class, interface, struct, or enum.
    TypeAlias,
    /// A module-level constant.
    Constant,
    /// A module-level variable.
    Variable,
}

impl SymbolKind {
    /// Every symbol kind, in a stable order.
    pub const ALL: [SymbolKind; 9] = [
        Self::Function,
        Self::Method,
        Self::Class,
        Self::Interface,
        Self::Struct,
        Self::Enum,
        Self::TypeAlias,
        Self::Constant,
        Self::Variable,
    ];

    /// The `snake_case` name used in storage, configuration, and output.
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Function => "function",
            Self::Method => "method",
            Self::Class => "class",
            Self::Interface => "interface",
            Self::Struct => "struct",
            Self::Enum => "enum",
            Self::TypeAlias => "type_alias",
            Self::Constant => "constant",
            Self::Variable => "variable",
        }
    }
}

impl fmt::Display for SymbolKind {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.pad(self.as_str())
    }
}

impl FromStr for SymbolKind {
    type Err = UnknownName;

    fn from_str(name: &str) -> Result<Self, Self::Err> {
        Self::ALL
            .into_iter()
            .find(|kind| kind.as_str() == name)
            .ok_or_else(|| UnknownName(name.to_owned()))
    }
}

/// A name that does not match any known language or symbol kind.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct UnknownName(pub String);

impl fmt::Display for UnknownName {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(formatter, "unknown name `{}`", self.0)
    }
}

impl std::error::Error for UnknownName {}

/// A zero-based location in a file. Columns count UTF-8 bytes, matching
/// Tree-sitter; language server positions are converted on the way in.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize)]
pub struct Position {
    /// Zero-based line number.
    pub line: u32,
    /// Zero-based column in UTF-8 bytes.
    pub column: u32,
}

/// A half-open range between two positions in the same file.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize)]
pub struct Span {
    /// First position inside the range.
    pub start: Position,
    /// First position after the range.
    pub end: Position,
}

/// A definition found in a source file.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Symbol {
    /// The identifier as written, such as `delete`.
    pub name: String,
    /// The name prefixed by its containers, such as `TaskRouter.delete`.
    pub qualified_name: String,
    /// What kind of definition this is.
    pub kind: SymbolKind,
    /// The whole definition, including its body.
    pub span: Span,
    /// Just the identifier. Language servers are queried at this location.
    pub name_span: Span,
}

/// A source file and the symbols extracted from it.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct IndexedFile {
    /// Path relative to the snapshot root, using `/` separators.
    pub path: String,
    /// Language the file was parsed as.
    pub language: Language,
    /// Hash of the file contents when it was indexed.
    pub content_hash: String,
    /// Whether the parser had to recover from syntax errors. Symbols near an
    /// error may be missing.
    pub has_syntax_errors: bool,
    /// Definitions in source order.
    pub symbols: Vec<Symbol>,
}

/// The inputs a set of facts was derived from.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Snapshot {
    /// Absolute path of the indexed directory.
    pub root: String,
    /// Git commit at the time of indexing, if the root is in a repository.
    pub revision: Option<String>,
    /// Whether the working tree had uncommitted changes. The revision alone
    /// does not describe a dirty tree; the fingerprint does.
    pub is_dirty: bool,
    /// Hash of every indexed file's path and contents plus the extractor
    /// version. Equal fingerprints mean equal inputs.
    pub fingerprint: String,
}

/// A place in the code that refers to a symbol.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
pub struct Reference {
    /// Path relative to the snapshot root, or absolute when outside it.
    pub path: String,
    /// The referring range.
    pub span: Span,
    /// Whether the reference lies outside the snapshot root, such as in an
    /// installed dependency.
    pub is_external: bool,
    /// The innermost indexed symbol containing the reference, if any.
    pub enclosing_symbol: Option<String>,
}

/// Who produced a semantic result, so it can be traced and invalidated.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Provenance {
    /// The configured provider identifier, such as `pyright`.
    pub provider_id: String,
    /// The version the server reported about itself, if any.
    pub provider_version: Option<String>,
    /// Hash of the provider's command, arguments, and settings.
    pub config_fingerprint: String,
}

/// The result of asking for a symbol's references.
///
/// These describe what Lemma could learn, not whether code is correct. An
/// empty result only means the provider found nothing within its view.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "state", rename_all = "snake_case")]
pub enum ReferenceOutcome {
    /// The provider returned references.
    Found {
        /// References sorted by path and position.
        references: Vec<Reference>,
    },
    /// The provider answered with no references.
    Empty,
    /// The provider does not offer reference lookup.
    Unsupported,
    /// No configured provider for the language could be started.
    ProviderUnavailable {
        /// Provider identifiers that were considered.
        candidates: Vec<String>,
    },
    /// The file changed since it was indexed, so stored positions may be wrong.
    Stale,
    /// The provider failed while answering.
    ProviderFailed {
        /// What went wrong, in the provider's words where possible.
        message: String,
    },
}

/// Hash of a file's contents. Used to tell whether a file changed since it
/// was indexed.
pub fn content_hash(contents: &[u8]) -> String {
    blake3::hash(contents).to_hex().to_string()
}
