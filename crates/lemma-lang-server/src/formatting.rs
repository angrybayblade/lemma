//! Formatting a document: its canonical layout as one edit that replaces
//! the whole text. Pure; the server feeds it the open document's text.

use lemma_lang::syntax::{FormatError, format};
use lemma_lsp::PositionEncoding;
use lemma_lsp::lsp_types::{Position, Range, TextEdit};

/// The edits that lay `text` out in the canonical layout: none when it
/// already is, and `None` when it has syntax errors, which the document's
/// diagnostics already show.
pub fn edits(text: &str, encoding: PositionEncoding) -> Result<Option<Vec<TextEdit>>, FormatError> {
    let formatted = match format(text) {
        Ok(formatted) => formatted,
        Err(FormatError::Syntax { .. }) => return Ok(None),
        Err(error) => return Err(error),
    };
    if formatted == text {
        return Ok(Some(Vec::new()));
    }
    let whole = Range {
        start: Position::new(0, 0),
        end: end_of(text, encoding),
    };
    Ok(Some(vec![TextEdit {
        range: whole,
        new_text: formatted,
    }]))
}

/// The position just past the last character of `text`.
fn end_of(text: &str, encoding: PositionEncoding) -> Position {
    let line = text.matches('\n').count();
    let last_line = text.rsplit('\n').next().unwrap_or_default();
    let column = u32::try_from(last_line.len()).unwrap_or(u32::MAX);
    Position {
        line: u32::try_from(line).unwrap_or(u32::MAX),
        character: encoding.encode_column(last_line, column),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn an_unformatted_document_is_replaced_whole() {
        let text = "module m\nlet a=1\n";

        let found = edits(text, PositionEncoding::Utf16).unwrap().unwrap();

        assert_eq!(found.len(), 1);
        assert_eq!(found[0].new_text, "module m\n\nlet a = 1\n");
        assert_eq!(found[0].range.end, Position::new(2, 0));
    }

    #[test]
    fn the_end_of_a_last_line_without_a_newline_is_counted_in_the_encoding() {
        let text = "module m\nlet a = \"é\"";

        let found = edits(text, PositionEncoding::Utf16).unwrap().unwrap();

        assert_eq!(found[0].range.end, Position::new(1, 11));
    }

    #[test]
    fn a_formatted_document_needs_no_edits() {
        let found = edits("module m\n\nlet a = 1\n", PositionEncoding::Utf16).unwrap();

        assert_eq!(found, Some(Vec::new()));
    }

    #[test]
    fn a_document_with_syntax_errors_is_left_alone() {
        let found = edits("module m\nlet a =\n", PositionEncoding::Utf16).unwrap();

        assert_eq!(found, None);
    }
}
