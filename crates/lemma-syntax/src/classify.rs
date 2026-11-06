//! Decides what kind of symbol a matched definition node is.

use lemma_model::SymbolKind;
use tree_sitter::Node;

/// Maps a definition node to a symbol kind, or `None` for node types the
/// queries should not have matched.
pub(crate) fn kind_of(definition: Node<'_>, source: &str) -> Option<SymbolKind> {
    let kind = match definition.kind() {
        "function_item" if rust_impl_or_trait(definition).is_some() => SymbolKind::Method,
        "function_declaration"
        | "generator_function_declaration"
        | "function_definition"
        | "function_item"
        | "macro_definition" => SymbolKind::Function,
        "method_definition"
        | "method_declaration"
        | "method_elem"
        | "method_signature"
        | "abstract_method_signature"
        | "function_signature_item" => SymbolKind::Method,
        "class_declaration" | "abstract_class_declaration" | "class_definition" => {
            SymbolKind::Class
        }
        "struct_item" | "union_item" => SymbolKind::Struct,
        "interface_declaration" | "trait_item" => SymbolKind::Interface,
        "type_alias_declaration" | "type_alias" | "type_item" => SymbolKind::TypeAlias,
        "enum_declaration" | "enum_item" => SymbolKind::Enum,
        "type_spec" => go_type_kind(definition),
        "const_spec" | "const_item" | "static_item" => SymbolKind::Constant,
        "var_spec" => SymbolKind::Variable,
        "public_field_definition" | "property_signature" | "field_declaration" => SymbolKind::Field,
        "variable_declarator" => typescript_variable_kind(definition, source),
        "assignment" => python_assignment_kind(definition, source),
        _ => return None,
    };
    Some(kind)
}

/// The receiver type of a Go method, such as `Router` for
/// `func (r *Router) Delete()`, or the type of the Rust `impl` block a
/// function is in, such as `Router` for `impl Service for Router`. Other
/// definitions have none.
pub(crate) fn receiver_type(definition: Node<'_>, source: &str) -> Option<String> {
    if definition.kind() == "function_item" {
        let block = rust_impl_or_trait(definition)?;
        if block.kind() != "impl_item" {
            return None; // a trait is a definition itself, so nesting names it
        }
        let type_node = block.child_by_field_name("type")?;
        let type_name = first_descendant_of_kind(type_node, "type_identifier")?;
        return Some(source[type_name.byte_range()].to_owned());
    }
    if definition.kind() != "method_declaration" {
        return None;
    }
    let receiver = definition.child_by_field_name("receiver")?;
    let type_name = first_descendant_of_kind(receiver, "type_identifier")?;
    Some(source[type_name.byte_range()].to_owned())
}

/// The `impl` or `trait` block a Rust function is directly inside.
fn rust_impl_or_trait(function: Node<'_>) -> Option<Node<'_>> {
    let block = function
        .parent()
        .filter(|parent| parent.kind() == "declaration_list")?
        .parent()?;
    matches!(block.kind(), "impl_item" | "trait_item").then_some(block)
}

fn go_type_kind(type_spec: Node<'_>) -> SymbolKind {
    match type_spec
        .child_by_field_name("type")
        .map(|node| node.kind())
    {
        Some("struct_type") => SymbolKind::Struct,
        Some("interface_type") => SymbolKind::Interface,
        _ => SymbolKind::TypeAlias,
    }
}

/// `const run = () => {}` is a function; other declarations are constants or
/// variables depending on the keyword.
fn typescript_variable_kind(declarator: Node<'_>, source: &str) -> SymbolKind {
    let value_kind = declarator
        .child_by_field_name("value")
        .map(|node| node.kind());
    if matches!(
        value_kind,
        Some("arrow_function" | "function_expression" | "function" | "generator_function")
    ) {
        return SymbolKind::Function;
    }

    let keyword = declarator
        .parent()
        .and_then(|declaration| declaration.child_by_field_name("kind"))
        .map(|node| &source[node.byte_range()]);
    if keyword == Some("const") {
        return SymbolKind::Constant;
    }
    SymbolKind::Variable
}

/// Python has no `const`, so `UPPER_CASE` names are treated as constants by
/// convention.
fn python_assignment_kind(assignment: Node<'_>, source: &str) -> SymbolKind {
    if is_in_class_body(assignment) {
        return SymbolKind::Field;
    }
    let name = assignment
        .child_by_field_name("left")
        .map_or("", |node| &source[node.byte_range()]);
    if is_constant_name(name) {
        return SymbolKind::Constant;
    }
    SymbolKind::Variable
}

/// Whether a Python assignment is a statement of a class body: an
/// attribute of the class, not a module-level name.
fn is_in_class_body(assignment: Node<'_>) -> bool {
    assignment
        .parent()
        .and_then(|statement| statement.parent())
        .and_then(|block| block.parent())
        .is_some_and(|owner| owner.kind() == "class_definition")
}

fn is_constant_name(name: &str) -> bool {
    let has_letter = name
        .chars()
        .any(|character| character.is_ascii_alphabetic());
    let is_upper_snake = name.chars().all(|character| {
        character.is_ascii_uppercase() || character.is_ascii_digit() || character == '_'
    });
    has_letter && is_upper_snake
}

fn first_descendant_of_kind<'tree>(node: Node<'tree>, kind: &str) -> Option<Node<'tree>> {
    if node.kind() == kind {
        return Some(node);
    }
    let mut cursor = node.walk();
    node.named_children(&mut cursor)
        .find_map(|child| first_descendant_of_kind(child, kind))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn upper_snake_case_names_are_constants() {
        assert!(is_constant_name("MAX_RETRIES"));
        assert!(is_constant_name("V2"));
    }

    #[test]
    fn other_names_are_not_constants() {
        assert!(!is_constant_name("client"));
        assert!(!is_constant_name("MaxRetries"));
        assert!(!is_constant_name("_"));
        assert!(!is_constant_name("__all__"));
    }
}
