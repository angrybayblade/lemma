//! The `.lm` language: Lemma's modules of services, relations, rules,
//! requirements and checks.
//!
//! This crate is the front end. It parses modules ([`syntax`]), resolves
//! their names across modules and repositories, and type-checks them,
//! reporting [`Diagnostic`]s as it goes. It reads no files itself: callers
//! hand it module texts, so the command line, checks at past commits and
//! the language server share it. Lowering a checked program into what
//! Lemma evaluates lives with each consumer. See
//! `.specs/13-lm-grammar.md`.

pub mod check;
pub mod diagnostic;
pub mod eval;
pub mod files;
pub mod load;
pub mod model;
pub mod span;
pub mod stdlib;
pub mod syntax;
pub mod types;

pub use check::{Checked, Definition, Reference, SymbolIndex, Target};
pub use diagnostic::{Diagnostic, Severity};
pub use load::{Repository, RepositorySource, SourceError, Sources};
pub use span::{LineIndex, Position, Span};

/// Loads the program whose entry module is `entry_path` in the entry
/// repository, and checks it.
pub fn check_program(sources: &mut dyn Sources, entry_path: &str) -> Checked {
    check::check(load::load(sources, entry_path))
}
