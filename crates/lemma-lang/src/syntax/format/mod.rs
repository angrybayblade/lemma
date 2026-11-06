//! The canonical layout of `.lm` modules (`.specs/13-lm-grammar.md` §1,
//! Layout).
//!
//! The formatter works on tokens and keeps the author's line breaks: it
//! re-indents each line, normalises the spaces between tokens, collapses
//! runs of blank lines and keeps every comment. Newlines separate items,
//! so it never adds or removes one between two tokens; it only changes how
//! many blank lines there are, which the parser does not see. A module with
//! syntax errors is refused, and the result is checked to read as the same
//! tokens and comments before it is returned.

mod facts;
mod layout;
mod spacing;

use crate::diagnostic::Diagnostic;
use crate::syntax::lexer::{Comment, Token, TokenKind, lex};
use crate::syntax::parse;

/// Why a module was not formatted.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum FormatError {
    /// The module does not parse, so where its items begin and end is a
    /// guess the formatter will not act on.
    #[error("the module has syntax errors")]
    Syntax {
        /// The parser's diagnostics, in source order.
        diagnostics: Vec<Diagnostic>,
    },
    /// The laid-out text would read differently from the original. This is
    /// a bug in the formatter, caught before any text was replaced.
    #[error("formatting would change how the module reads near byte {offset}")]
    Changed {
        /// Where the original and the laid-out text first differ, in the
        /// original.
        offset: u32,
    },
}

/// Lays `source` out in the canonical layout.
pub fn format(source: &str) -> Result<String, FormatError> {
    let parsed = parse(source);
    if !parsed.diagnostics.is_empty() {
        return Err(FormatError::Syntax {
            diagnostics: parsed.diagnostics,
        });
    }
    let lexed = lex(source);
    let facts = facts::collect(&parsed.file);
    let classified = spacing::classify(source, &lexed.tokens, &facts);
    let formatted = layout::render(source, &lexed, &classified, &parsed.file);
    verify(source, &lexed.tokens, &lexed.comments, &formatted)?;
    Ok(formatted)
}

/// Whether `source` is already in the canonical layout.
pub fn is_formatted(source: &str) -> Result<bool, FormatError> {
    Ok(format(source)? == source)
}

/// Checks that `formatted` has the same tokens as the original, with each
/// run of newlines as one, and the same comments.
fn verify(
    source: &str,
    tokens: &[Token],
    comments: &[Comment],
    formatted: &str,
) -> Result<(), FormatError> {
    let relexed = lex(formatted);
    let before = significant(source, tokens);
    let after = significant(formatted, &relexed.tokens);
    if let Some(difference) = first_difference(&before, &after) {
        let offset = before
            .get(difference)
            .map_or(source.len(), |token| token.0.span.start as usize);
        return Err(FormatError::Changed {
            offset: u32::try_from(offset).unwrap_or(u32::MAX),
        });
    }
    let comments_before = comment_texts(source, comments);
    let comments_after = comment_texts(formatted, &relexed.comments);
    if let Some(difference) = first_difference(&comments_before, &comments_after) {
        let offset = comments
            .get(difference)
            .map_or(source.len(), |comment| comment.span.start as usize);
        return Err(FormatError::Changed {
            offset: u32::try_from(offset).unwrap_or(u32::MAX),
        });
    }
    Ok(())
}

/// A token and its text, compared by kind and text only.
#[derive(Debug, Clone, Copy)]
struct Written<'a>(Token, &'a str);

impl PartialEq for Written<'_> {
    fn eq(&self, other: &Self) -> bool {
        self.0.kind == other.0.kind && self.1 == other.1
    }
}

/// The tokens as the parser sees them: each run of newlines as one, and
/// none before the first token or after the last.
fn significant<'a>(source: &'a str, tokens: &[Token]) -> Vec<Written<'a>> {
    let mut kept: Vec<Written<'a>> = Vec::with_capacity(tokens.len());
    for token in tokens {
        let is_newline = token.kind == TokenKind::Newline;
        let follows_newline = kept
            .last()
            .is_none_or(|written| written.0.kind == TokenKind::Newline);
        if token.kind == TokenKind::End || (is_newline && follows_newline) {
            continue;
        }
        kept.push(Written(*token, token.span.text(source)));
    }
    if kept
        .last()
        .is_some_and(|written| written.0.kind == TokenKind::Newline)
    {
        kept.pop();
    }
    kept
}

/// Comments' texts without trailing whitespace, which the layout drops.
fn comment_texts<'a>(source: &'a str, comments: &[Comment]) -> Vec<&'a str> {
    comments
        .iter()
        .map(|comment| comment.span.text(source).trim_end())
        .collect()
}

/// The first index where two sequences differ, or their shorter length when
/// one extends the other.
fn first_difference<T: PartialEq>(before: &[T], after: &[T]) -> Option<usize> {
    let shared = before
        .iter()
        .zip(after)
        .position(|(left, right)| left != right);
    match shared {
        Some(index) => Some(index),
        None if before.len() != after.len() => Some(before.len().min(after.len())),
        None => None,
    }
}

#[cfg(test)]
mod tests;
