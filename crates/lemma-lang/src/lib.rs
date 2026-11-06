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

pub mod diagnostic;
pub mod span;
pub mod syntax;

pub use diagnostic::{Diagnostic, Severity};
pub use span::{LineIndex, Position, Span};
