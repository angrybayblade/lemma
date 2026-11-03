//! Go structs as JSON Schema, by `encoding/json`'s rules: the `json` tag
//! names a field (`-` skips it, `omitempty` makes it optional), a field
//! without a tag travels under its Go name, pointers, slices and maps can be
//! `null`, and an embedded struct's fields are its own. Named types defined
//! in the same package are followed; others are left open (`{}`) and
//! reported.
//!
//! Read with Tree-sitter rather than by reflection: reflectors' defaults
//! (`invopop/jsonschema`'s, for one) do not mark pointers as nullable, which
//! would make every `Optional` field on the other side of a link look like a
//! difference.

use std::collections::{BTreeMap, BTreeSet};

use serde_json::{Map, Value, json};
use tree_sitter::{Node, Parser};

/// How deep to follow named struct types inside each other.
const MAX_DEPTH: usize = 12;

/// A Go type's schema, and the names it uses that could not be followed.
#[derive(Debug, Clone, PartialEq)]
pub struct GoSchema {
    /// The JSON Schema.
    pub schema: Value,
    /// Named types used but not defined in the package, such as `models.X`.
    pub unresolved: BTreeSet<String>,
}

/// Which side of a message a Go type is on: `encoding/json` encodes and
/// decodes by different rules.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum View {
    /// The type is sent: fields without `omitempty` are always present, and
    /// only pointers, slices and maps can be `null`.
    Encode,
    /// The type receives: a missing field is left at its zero value and
    /// `null` decodes into any field, so nothing is required and every field
    /// accepts `null`. Only a value of the wrong type fails to decode.
    Decode,
}

/// The schema of `type_name`, defined in one of a package's files, as seen
/// from `view`. `None` if no file defines it.
pub fn struct_schema(files: &[(String, String)], type_name: &str, view: View) -> Option<GoSchema> {
    let mut parser = Parser::new();
    parser.set_language(&tree_sitter_go::LANGUAGE.into()).ok()?;
    let trees: Vec<(tree_sitter::Tree, &str)> = files
        .iter()
        .filter_map(|(_, source)| Some((parser.parse(source, None)?, source.as_str())))
        .collect();
    let mut types = BTreeMap::new();
    for (tree, source) in &trees {
        collect_types(tree.root_node(), source, &mut types);
    }
    let definition = *types.get(type_name)?;
    let mut reader = Reader {
        types: &types,
        unresolved: BTreeSet::new(),
        depth: 0,
        view,
    };
    let schema = reader.type_schema(definition.0, definition.1);
    Some(GoSchema {
        schema,
        unresolved: reader.unresolved,
    })
}

/// A type's definition: its type node and the source it is in.
type Definition<'tree> = (Node<'tree>, &'tree str);

fn collect_types<'tree>(
    node: Node<'tree>,
    source: &'tree str,
    out: &mut BTreeMap<String, Definition<'tree>>,
) {
    if node.kind() == "type_spec" {
        if let (Some(name), Some(kind)) = (
            node.child_by_field_name("name"),
            node.child_by_field_name("type"),
        ) {
            out.insert(text(name, source).to_owned(), (kind, source));
        }
        return;
    }
    let mut cursor = node.walk();
    for child in node.named_children(&mut cursor) {
        collect_types(child, source, out);
    }
}

struct Reader<'a, 'tree> {
    types: &'a BTreeMap<String, Definition<'tree>>,
    unresolved: BTreeSet<String>,
    depth: usize,
    view: View,
}

impl Reader<'_, '_> {
    fn type_schema(&mut self, node: Node<'_>, source: &str) -> Value {
        match node.kind() {
            "struct_type" => self.struct_schema(node, source),
            "pointer_type" | "slice_type" | "array_type" | "map_type" => {
                self.container(node, source)
            }
            "type_identifier" => self.named(text(node, source)),
            "qualified_type" => self.qualified(text(node, source)),
            "parenthesized_type" => node
                .named_child(0)
                .map_or_else(|| json!({}), |inner| self.type_schema(inner, source)),
            // interface{}, any, generics, functions, channels: anything.
            _ => json!({}),
        }
    }

    fn container(&mut self, node: Node<'_>, source: &str) -> Value {
        let element = |reader: &mut Self, field: &str| {
            node.child_by_field_name(field)
                .or_else(|| node.named_child(node.named_child_count().saturating_sub(1)))
                .map_or_else(|| json!({}), |inner| reader.type_schema(inner, source))
        };
        let schema = match node.kind() {
            "pointer_type" => node
                .named_child(0)
                .map_or_else(|| json!({}), |inner| self.type_schema(inner, source)),
            "slice_type" | "array_type" => {
                json!({ "type": "array", "items": element(self, "element") })
            }
            _ => json!({ "type": "object", "additionalProperties": element(self, "value") }),
        };
        // A nil pointer, slice or map is encoded as null; null decodes into one.
        if node.kind() == "array_type" {
            return schema;
        }
        nullable(schema)
    }

    fn named(&mut self, name: &str) -> Value {
        if let Some(primitive) = primitive(name) {
            return primitive;
        }
        let Some(&(definition, source)) = self.types.get(name) else {
            self.unresolved.insert(name.to_owned());
            return json!({});
        };
        if self.depth >= MAX_DEPTH {
            return json!({});
        }
        self.depth += 1;
        let schema = self.type_schema(definition, source);
        self.depth -= 1;
        schema
    }

    fn qualified(&mut self, name: &str) -> Value {
        match name {
            "time.Time" => json!({ "type": "string", "format": "date-time" }),
            "json.RawMessage" => json!({}),
            _ => {
                self.unresolved.insert(name.to_owned());
                json!({})
            }
        }
    }

    fn struct_schema(&mut self, node: Node<'_>, source: &str) -> Value {
        let mut properties = Map::new();
        let mut required = Vec::new();
        let Some(list) = (0..node.named_child_count())
            .filter_map(|index| node.named_child(index))
            .find(|child| child.kind() == "field_declaration_list")
        else {
            return json!({ "type": "object" });
        };
        let mut cursor = list.walk();
        for field in list
            .named_children(&mut cursor)
            .filter(|child| child.kind() == "field_declaration")
        {
            self.field(field, source, &mut properties, &mut required);
        }
        let mut schema = json!({ "type": "object", "properties": properties });
        if !required.is_empty() {
            schema["required"] = json!(required);
        }
        schema
    }

    fn field(
        &mut self,
        field: Node<'_>,
        source: &str,
        properties: &mut Map<String, Value>,
        required: &mut Vec<String>,
    ) {
        let Some(kind) = field.child_by_field_name("type") else {
            return;
        };
        let tag = field
            .child_by_field_name("tag")
            .map(|tag| text(tag, source));
        let names: Vec<&str> = {
            let mut cursor = field.walk();
            field
                .children_by_field_name("name", &mut cursor)
                .map(|name| text(name, source))
                .collect()
        };
        let (tag_name, omit_empty) = json_tag(tag);
        if tag_name.as_deref() == Some("-") {
            return;
        }
        if names.is_empty() && tag_name.is_none() {
            self.embed(kind, source, properties, required);
            return;
        }
        let mut schema = self.type_schema(kind, source);
        if self.view == View::Decode {
            schema = nullable(schema);
        }
        // A tagged embedded field is a field named by its tag.
        let wire_names: Vec<String> = match &tag_name {
            Some(name) => vec![name.clone()],
            None => names.iter().map(|name| (*name).to_owned()).collect(),
        };
        for wire in wire_names {
            if !omit_empty && self.view == View::Encode {
                required.push(wire.clone());
            }
            properties.insert(wire, schema.clone());
        }
    }

    /// An embedded struct: its fields become the outer struct's.
    fn embed(
        &mut self,
        kind: Node<'_>,
        source: &str,
        properties: &mut Map<String, Value>,
        required: &mut Vec<String>,
    ) {
        let inner = if kind.kind() == "pointer_type" {
            kind.named_child(0)
        } else {
            Some(kind)
        };
        let Some(inner) = inner else { return };
        let embedded = self.type_schema(inner, source);
        if let Some(fields) = embedded.get("properties").and_then(Value::as_object) {
            properties.extend(fields.clone());
        }
        if let Some(names) = embedded.get("required").and_then(Value::as_array) {
            required.extend(names.iter().filter_map(Value::as_str).map(str::to_owned));
        }
    }
}

/// The `json` key of a struct tag: its name (if given) and `omitempty`.
fn json_tag(tag: Option<&str>) -> (Option<String>, bool) {
    let Some(tag) = tag else {
        return (None, false);
    };
    let tag = tag.trim_matches(['`', '"']);
    let Some(start) = tag.find("json:\"") else {
        return (None, false);
    };
    let value = &tag[start + "json:\"".len()..];
    let value = &value[..value.find('"').unwrap_or(value.len())];
    let mut parts = value.split(',');
    let name = parts
        .next()
        .filter(|name| !name.is_empty())
        .map(str::to_owned);
    let omit_empty = parts.any(|option| option == "omitempty");
    (name, omit_empty)
}

fn primitive(name: &str) -> Option<Value> {
    let schema = match name {
        "string" => json!({ "type": "string" }),
        "bool" => json!({ "type": "boolean" }),
        "int" | "int8" | "int16" | "int32" | "int64" | "uint" | "uint8" | "uint16" | "uint32"
        | "uint64" | "uintptr" | "byte" | "rune" => json!({ "type": "integer" }),
        "float32" | "float64" => json!({ "type": "number" }),
        "any" | "error" => json!({}),
        _ => return None,
    };
    Some(schema)
}

/// `schema`, also allowing `null`.
fn nullable(schema: Value) -> Value {
    match schema.get("type") {
        Some(Value::String(kind)) => {
            let mut nullable = schema.clone();
            nullable["type"] = json!([kind, "null"]);
            nullable
        }
        Some(_) => schema,
        None if schema == json!({}) => schema,
        None => json!({ "anyOf": [schema, { "type": "null" }] }),
    }
}

fn text<'s>(node: Node<'_>, source: &'s str) -> &'s str {
    node.utf8_text(source.as_bytes()).unwrap_or_default()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn schema(source: &str, name: &str) -> GoSchema {
        struct_schema(
            &[("a.go".to_owned(), source.to_owned())],
            name,
            View::Encode,
        )
        .unwrap()
    }

    #[test]
    fn tags_omitempty_pointers_and_nesting_follow_encoding_json() {
        let source = r#"package p
type Meta struct {
	Amount string `json:"amount_cents"`
}
type Response struct {
	Base
	Data      map[string]interface{} `json:"data"`
	Error     *string                `json:"error"`
	Meta      *Meta                  `json:"metadata,omitempty"`
	Items     []Meta                 `json:"items"`
	Skipped   string                 `json:"-"`
	Untagged  bool
	Inline    struct {
		Ok bool `json:"ok"`
	} `json:"inline"`
}
type Base struct {
	ID string `json:"id"`
}
"#;

        let found = schema(source, "Response");

        let properties = &found.schema["properties"];
        assert_eq!(properties["error"]["type"], json!(["string", "null"]));
        assert_eq!(properties["data"]["type"], json!(["object", "null"]));
        assert_eq!(
            properties["metadata"]["properties"]["amount_cents"]["type"],
            "string"
        );
        assert_eq!(properties["metadata"]["type"], json!(["object", "null"]));
        assert_eq!(
            properties["items"]["items"]["properties"]["amount_cents"]["type"],
            "string"
        );
        assert_eq!(properties["inline"]["properties"]["ok"]["type"], "boolean");
        assert_eq!(properties["Untagged"]["type"], "boolean");
        assert_eq!(properties["id"]["type"], "string");
        assert!(properties.get("Skipped").is_none());
        let required: Vec<&str> = found.schema["required"]
            .as_array()
            .unwrap()
            .iter()
            .filter_map(Value::as_str)
            .collect();
        assert!(required.contains(&"error"));
        assert!(!required.contains(&"metadata"));
        assert!(found.unresolved.is_empty());
    }

    #[test]
    fn types_from_other_packages_are_left_open_and_reported() {
        let found = schema(
            "package p\ntype R struct {\n\tM models.Module `json:\"m\"`\n}\n",
            "R",
        );

        assert_eq!(found.schema["properties"]["m"], json!({}));
        assert!(found.unresolved.contains("models.Module"));
    }

    #[test]
    fn a_receiving_struct_requires_nothing_and_accepts_null() {
        let source = "package p\ntype R struct {\n\tName string `json:\"name\"`\n}\n";

        let decoded =
            struct_schema(&[("a.go".to_owned(), source.to_owned())], "R", View::Decode).unwrap();

        assert_eq!(decoded.schema.get("required"), None);
        assert_eq!(
            decoded.schema["properties"]["name"]["type"],
            json!(["string", "null"])
        );
    }
}
