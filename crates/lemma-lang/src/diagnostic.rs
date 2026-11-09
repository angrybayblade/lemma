//! Problems found in `.lm` source, each pinned to a span of one module.
//!
//! Parsing, name resolution and type checking all report through these, so
//! the command line and the language server show the same messages.

use crate::span::Span;

/// How serious a diagnostic is.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum Severity {
    /// The module cannot be used as written.
    Error,
    /// The module works, but probably not as intended.
    Warning,
    /// Worth knowing.
    Note,
}

/// One problem, with where it is.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Diagnostic {
    /// How serious it is.
    pub severity: Severity,
    /// The module it is in, by name; empty before the name is known.
    pub module: String,
    /// The source it covers.
    pub span: Span,
    /// What is wrong, in a sentence.
    pub message: String,
    /// Whether this is the end of a link naming an operation its service
    /// does not have. Still an error in a module being edited, but
    /// `lemma check` reports it as a finding about the change.
    pub is_removed_link_end: bool,
}

impl Diagnostic {
    /// An error at `span`.
    pub fn error(span: Span, message: impl Into<String>) -> Self {
        Self {
            severity: Severity::Error,
            module: String::new(),
            span,
            message: message.into(),
            is_removed_link_end: false,
        }
    }

    /// A warning at `span`.
    pub fn warning(span: Span, message: impl Into<String>) -> Self {
        Self {
            severity: Severity::Warning,
            ..Self::error(span, message)
        }
    }

    /// A note at `span`.
    pub fn note(span: Span, message: impl Into<String>) -> Self {
        Self {
            severity: Severity::Note,
            ..Self::error(span, message)
        }
    }

    /// The same diagnostic, marked as a link naming a removed operation.
    #[must_use]
    pub fn as_removed_link_end(mut self) -> Self {
        self.is_removed_link_end = true;
        self
    }

    /// The same diagnostic, attributed to `module`.
    #[must_use]
    pub fn in_module(mut self, module: &str) -> Self {
        module.clone_into(&mut self.module);
        self
    }
}

/// Whether any diagnostic is an error.
pub fn has_errors(diagnostics: &[Diagnostic]) -> bool {
    diagnostics
        .iter()
        .any(|diagnostic| diagnostic.severity == Severity::Error)
}
