//! Converting columns between Lemma's UTF-8 byte offsets and whatever unit a
//! language server counts in. Servers default to UTF-16 code units.

use lsp_types::PositionEncodingKind;

/// The unit a server uses for column offsets.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PositionEncoding {
    /// UTF-8 bytes, the same as Lemma.
    Utf8,
    /// UTF-16 code units, the LSP default.
    Utf16,
    /// Unicode code points.
    Utf32,
}

impl PositionEncoding {
    /// The encoding a server negotiated. Servers that do not say use UTF-16.
    pub fn negotiated(kind: Option<&PositionEncodingKind>) -> Self {
        match kind.map(PositionEncodingKind::as_str) {
            Some("utf-8") => Self::Utf8,
            Some("utf-32") => Self::Utf32,
            _ => Self::Utf16,
        }
    }

    /// Converts a UTF-8 byte column on this line into the server's unit.
    pub fn encode_column(self, line_text: &str, utf8_column: u32) -> u32 {
        if self == Self::Utf8 {
            return utf8_column;
        }
        line_text
            .char_indices()
            .take_while(|(byte_index, _)| to_u32(*byte_index) < utf8_column)
            .map(|(_, character)| self.units(character))
            .sum()
    }

    /// Converts a column in the server's unit on this line into UTF-8 bytes.
    /// Columns past the end of the line clamp to the line's length.
    pub fn decode_column(self, line_text: &str, encoded_column: u32) -> u32 {
        if self == Self::Utf8 {
            return encoded_column;
        }
        let mut encoded = 0;
        for (byte_index, character) in line_text.char_indices() {
            if encoded >= encoded_column {
                return to_u32(byte_index);
            }
            encoded += self.units(character);
        }
        to_u32(line_text.len())
    }

    fn units(self, character: char) -> u32 {
        match self {
            Self::Utf8 => to_u32(character.len_utf8()),
            Self::Utf16 => to_u32(character.len_utf16()),
            Self::Utf32 => 1,
        }
    }
}

/// The text of a zero-based line, without its line ending. Missing lines are
/// empty.
pub fn line_text(source: &str, line: u32) -> &str {
    usize::try_from(line)
        .ok()
        .and_then(|line| source.lines().nth(line))
        .unwrap_or("")
}

fn to_u32(value: usize) -> u32 {
    u32::try_from(value).unwrap_or(u32::MAX)
}

#[cfg(test)]
mod tests {
    use super::*;

    // "é" is 2 UTF-8 bytes and 1 UTF-16 unit; "😀" is 4 bytes and 2 units.
    const LINE: &str = "é😀x";

    #[test]
    fn utf16_columns_round_trip() {
        let utf8_column_of_x = 6;
        let utf16_column_of_x = 3;

        assert_eq!(
            PositionEncoding::Utf16.encode_column(LINE, utf8_column_of_x),
            utf16_column_of_x
        );
        assert_eq!(
            PositionEncoding::Utf16.decode_column(LINE, utf16_column_of_x),
            utf8_column_of_x
        );
    }

    #[test]
    fn utf32_counts_code_points() {
        assert_eq!(PositionEncoding::Utf32.encode_column(LINE, 6), 2);
        assert_eq!(PositionEncoding::Utf32.decode_column(LINE, 2), 6);
    }

    #[test]
    fn utf8_columns_pass_through() {
        assert_eq!(PositionEncoding::Utf8.encode_column(LINE, 6), 6);
        assert_eq!(PositionEncoding::Utf8.decode_column(LINE, 6), 6);
    }

    #[test]
    fn columns_past_the_end_clamp_to_the_line() {
        assert_eq!(PositionEncoding::Utf16.decode_column(LINE, 99), 7);
    }

    #[test]
    fn unknown_or_missing_encodings_default_to_utf16() {
        assert_eq!(PositionEncoding::negotiated(None), PositionEncoding::Utf16);
        assert_eq!(
            PositionEncoding::negotiated(Some(&PositionEncodingKind::UTF8)),
            PositionEncoding::Utf8
        );
    }

    #[test]
    fn line_text_strips_line_endings() {
        assert_eq!(line_text("a\r\nb\n", 0), "a");
        assert_eq!(line_text("a\r\nb\n", 1), "b");
        assert_eq!(line_text("a\r\nb\n", 5), "");
    }
}
