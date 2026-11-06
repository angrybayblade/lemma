//! Source positions: byte ranges in a module's text, and the conversion to
//! lines and columns that diagnostics and editors need.

/// A byte range in one module's source text, end exclusive.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Default)]
pub struct Span {
    /// The first byte.
    pub start: u32,
    /// One past the last byte.
    pub end: u32,
}

impl Span {
    /// A span from `start` to `end`.
    pub fn new(start: usize, end: usize) -> Self {
        Self {
            start: offset(start),
            end: offset(end),
        }
    }

    /// The smallest span covering both.
    #[must_use]
    pub fn to(self, other: Span) -> Span {
        Span {
            start: self.start.min(other.start),
            end: self.end.max(other.end),
        }
    }

    /// Whether `position` falls inside the span, or at its end.
    pub fn touches(self, position: u32) -> bool {
        self.start <= position && position <= self.end
    }

    /// The text the span covers.
    pub fn text(self, source: &str) -> &str {
        source
            .get(self.start as usize..self.end as usize)
            .unwrap_or("")
    }
}

/// Byte offsets beyond 4 GiB are clamped: no module is that large, and a
/// clamped span still points at the end of the file.
fn offset(value: usize) -> u32 {
    u32::try_from(value).unwrap_or(u32::MAX)
}

/// A zero-based line and column, the column counted in bytes.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub struct Position {
    /// The line.
    pub line: u32,
    /// The byte offset within the line.
    pub column: u32,
}

/// Where each line of a text starts, to turn byte offsets into positions.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LineIndex {
    starts: Vec<u32>,
}

impl LineIndex {
    /// Indexes `text`.
    pub fn new(text: &str) -> Self {
        let mut starts = vec![0];
        for (index, byte) in text.bytes().enumerate() {
            if byte == b'\n' {
                starts.push(offset(index + 1));
            }
        }
        Self { starts }
    }

    /// The position of a byte offset.
    pub fn position(&self, byte: u32) -> Position {
        let line = self.starts.partition_point(|start| *start <= byte) - 1;
        Position {
            line: offset(line),
            column: byte - self.starts[line],
        }
    }

    /// The byte offset of a position, clamped to the text.
    pub fn offset(&self, position: Position) -> u32 {
        let Some(start) = self.starts.get(position.line as usize) else {
            return self.starts.last().copied().unwrap_or(0);
        };
        start + position.column
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn positions_count_lines_and_byte_columns() {
        let index = LineIndex::new("ab\ncd\n");

        assert_eq!(index.position(4), Position { line: 1, column: 1 });
        assert_eq!(index.offset(Position { line: 1, column: 1 }), 4);
    }

    #[test]
    fn joining_spans_covers_both() {
        let joined = Span::new(4, 6).to(Span::new(1, 3));

        assert_eq!(joined, Span::new(1, 6));
    }
}
