//! Turns flat query matches into symbols named by what lexically contains them.

use std::ops::Range;

use lemma_model::{Span, Symbol, SymbolKind};

/// A definition as matched by a query, before nesting is known.
#[derive(Debug, Clone)]
pub(crate) struct Definition {
    pub name: String,
    /// Go method receiver type. Go methods are declared outside their type,
    /// so lexical nesting cannot name them.
    pub receiver: Option<String>,
    pub kind: SymbolKind,
    pub span: Span,
    pub name_span: Span,
    pub byte_range: Range<usize>,
}

/// A definition that may still contain later ones.
struct Container {
    byte_range: Range<usize>,
    qualified_name: String,
    kind: SymbolKind,
}

/// Orders definitions by position and names each after its containers, so a
/// method `delete` in class `ToolRouter` becomes `ToolRouter.delete`.
pub(crate) fn into_symbols(mut definitions: Vec<Definition>) -> Vec<Symbol> {
    // Outer definitions sort before the inner ones that start at the same byte.
    definitions.sort_by(|left, right| {
        left.byte_range
            .start
            .cmp(&right.byte_range.start)
            .then(right.byte_range.end.cmp(&left.byte_range.end))
    });
    definitions.dedup_by(|later, earlier| {
        later.byte_range == earlier.byte_range && later.name_span == earlier.name_span
    });

    let mut open_containers: Vec<Container> = Vec::new();
    let mut symbols = Vec::with_capacity(definitions.len());
    for definition in definitions {
        while open_containers
            .last()
            .is_some_and(|container| !contains(&container.byte_range, &definition.byte_range))
        {
            open_containers.pop();
        }

        let container = open_containers.last();
        let kind = refine_kind(definition.kind, container.map(|container| container.kind));
        let prefix = definition
            .receiver
            .as_deref()
            .or(container.map(|container| container.qualified_name.as_str()));
        let qualified_name = qualify(&definition.name, prefix);

        open_containers.push(Container {
            byte_range: definition.byte_range.clone(),
            qualified_name: qualified_name.clone(),
            kind,
        });
        symbols.push(Symbol {
            name: definition.name,
            qualified_name,
            kind,
            span: definition.span,
            name_span: definition.name_span,
        });
    }
    symbols
}

fn contains(outer: &Range<usize>, inner: &Range<usize>) -> bool {
    outer.start <= inner.start && inner.end <= outer.end
}

/// Python methods are plain functions syntactically; their class makes them
/// methods.
fn refine_kind(kind: SymbolKind, container_kind: Option<SymbolKind>) -> SymbolKind {
    if kind == SymbolKind::Function && container_kind == Some(SymbolKind::Class) {
        return SymbolKind::Method;
    }
    kind
}

fn qualify(name: &str, prefix: Option<&str>) -> String {
    match prefix {
        Some(prefix) => format!("{prefix}.{name}"),
        None => name.to_owned(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use lemma_model::Position;

    fn definition(name: &str, kind: SymbolKind, byte_range: Range<usize>) -> Definition {
        let span = Span {
            start: Position { line: 0, column: 0 },
            end: Position { line: 0, column: 0 },
        };
        Definition {
            name: name.to_owned(),
            receiver: None,
            kind,
            span,
            name_span: span,
            byte_range,
        }
    }

    fn qualified_names(symbols: &[Symbol]) -> Vec<&str> {
        symbols
            .iter()
            .map(|symbol| symbol.qualified_name.as_str())
            .collect()
    }

    #[test]
    fn nested_definitions_are_prefixed_by_their_containers() {
        let symbols = into_symbols(vec![
            definition("inner", SymbolKind::Function, 20..30),
            definition("Outer", SymbolKind::Class, 0..50),
            definition("method", SymbolKind::Function, 10..40),
            definition("after", SymbolKind::Function, 60..70),
        ]);

        assert_eq!(
            qualified_names(&symbols),
            ["Outer", "Outer.method", "Outer.method.inner", "after"]
        );
    }

    #[test]
    fn functions_directly_inside_classes_become_methods() {
        let symbols = into_symbols(vec![
            definition("Outer", SymbolKind::Class, 0..50),
            definition("method", SymbolKind::Function, 10..40),
            definition("inner", SymbolKind::Function, 20..30),
        ]);

        let kinds: Vec<_> = symbols.iter().map(|symbol| symbol.kind).collect();
        assert_eq!(
            kinds,
            [SymbolKind::Class, SymbolKind::Method, SymbolKind::Function]
        );
    }

    #[test]
    fn receiver_takes_precedence_over_lexical_containers() {
        let mut method = definition("Delete", SymbolKind::Method, 0..10);
        method.receiver = Some("Router".to_owned());

        assert_eq!(
            qualified_names(&into_symbols(vec![method])),
            ["Router.Delete"]
        );
    }

    #[test]
    fn duplicate_matches_are_collapsed() {
        let symbols = into_symbols(vec![
            definition("value", SymbolKind::Constant, 0..10),
            definition("value", SymbolKind::Constant, 0..10),
        ]);

        assert_eq!(symbols.len(), 1);
    }
}
