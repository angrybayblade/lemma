//! The innermost symbol containing a point, found without scanning every
//! symbol: a file's symbols nest, so the last one starting at or before the
//! point either contains it or has an ancestor that does.

use lemma_model::{Position, Symbol};

/// Symbols ordered by where they start, each with the nearest earlier one
/// containing it.
pub(super) struct Nesting<'a> {
    symbols: Vec<&'a Symbol>,
    parents: Vec<Option<usize>>,
}

impl<'a> Nesting<'a> {
    /// The symbols `keep` selects, ready for lookups.
    pub(super) fn new(symbols: &'a [Symbol], keep: impl Fn(&Symbol) -> bool) -> Self {
        let mut kept: Vec<&Symbol> = symbols.iter().filter(|symbol| keep(symbol)).collect();
        // Outer before inner where two start together.
        kept.sort_by(|left, right| {
            (left.span.start, right.span.end).cmp(&(right.span.start, left.span.end))
        });
        let mut parents = Vec::with_capacity(kept.len());
        let mut open: Vec<usize> = Vec::new();
        for (index, symbol) in kept.iter().enumerate() {
            while open
                .last()
                .is_some_and(|&last| kept[last].span.end <= symbol.span.start)
            {
                open.pop();
            }
            parents.push(open.last().copied());
            open.push(index);
        }
        Self {
            symbols: kept,
            parents,
        }
    }

    /// The innermost symbol containing `point`.
    pub(super) fn innermost(&self, point: Position) -> Option<&'a Symbol> {
        let after = self
            .symbols
            .partition_point(|symbol| symbol.span.start <= point);
        let mut candidate = after.checked_sub(1);
        while let Some(index) = candidate {
            let symbol = self.symbols[index];
            if point < symbol.span.end {
                return Some(symbol);
            }
            candidate = self.parents[index];
        }
        None
    }
}

#[cfg(test)]
mod tests {
    use lemma_model::{Span, SymbolKind};

    use super::*;

    fn symbol(name: &str, start: u32, end: u32) -> Symbol {
        let at = |line| Position { line, column: 0 };
        let span = Span {
            start: at(start),
            end: at(end),
        };
        Symbol {
            name: name.to_owned(),
            qualified_name: name.to_owned(),
            kind: SymbolKind::Function,
            span,
            name_span: span,
        }
    }

    #[test]
    fn the_innermost_symbol_holding_a_point_is_found_past_its_earlier_children() {
        let symbols = [
            symbol("Outer", 0, 20),
            symbol("Outer.first", 1, 5),
            symbol("Outer.second", 6, 10),
            symbol("other", 21, 30),
        ];
        let nesting = Nesting::new(&symbols, |_| true);
        let at = |line| {
            nesting
                .innermost(Position { line, column: 0 })
                .map(|symbol| symbol.name.as_str())
        };

        assert_eq!(at(3), Some("Outer.first"));
        assert_eq!(at(12), Some("Outer"));
        assert_eq!(at(25), Some("other"));
        assert_eq!(at(20), None);
    }
}
