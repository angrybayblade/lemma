//! TypeScript types as JSON Schema, read with Tree-sitter.
//!
//! Interfaces (with `extends`), type aliases, object types, enums, unions
//! and intersections are read as written: a property is required unless it
//! is marked `?` or its type includes `undefined`, and `null` in a union
//! makes it nullable. TypeScript types are not checked at run time, so the
//! same schema describes a type sent and received. Types are looked up in
//! the anchored file and the files it imports by relative path; others are
//! left open (`{}`) and reported.

use std::collections::{BTreeMap, BTreeSet, VecDeque};

use serde_json::{Map, Value, json};
use tree_sitter::{Node, Parser};

use super::shape::{any_of, nullable, object, string_enum};
use crate::GraphError;

/// How deep to follow named types inside each other.
const MAX_DEPTH: usize = 12;
/// How many files to read following imports.
const MAX_FILES: usize = 64;

/// A TypeScript type's schema, and the names it uses that could not be
/// followed.
#[derive(Debug, Clone, PartialEq)]
pub struct TypeScriptSchema {
    /// The JSON Schema.
    pub schema: Value,
    /// Named types used but not found.
    pub unresolved: BTreeSet<String>,
}

/// The schema of `type_name` in `file`, reading files through `read`
/// (relative to the repository root). `None` if no file read defines it.
pub fn type_schema(
    file: &str,
    type_name: &str,
    read: &mut dyn FnMut(&str) -> Result<Option<String>, GraphError>,
) -> Result<Option<TypeScriptSchema>, GraphError> {
    let sources = files_reached(file, read)?;
    let trees: Vec<(tree_sitter::Tree, &str)> = sources
        .iter()
        .filter_map(|(path, source)| Some((parse(path, source)?, source.as_str())))
        .collect();
    let mut declarations = BTreeMap::new();
    for (tree, source) in &trees {
        collect_declarations(tree.root_node(), source, &mut declarations);
    }
    let Some(&declaration) = declarations.get(type_name) else {
        return Ok(None);
    };
    let mut reader = Reader {
        declarations: &declarations,
        unresolved: BTreeSet::new(),
        depth: 0,
    };
    let schema = reader.declaration(declaration);
    Ok(Some(TypeScriptSchema {
        schema,
        unresolved: reader.unresolved,
    }))
}

/// `file` and every file it reaches through relative imports, `file`
/// first, each once.
fn files_reached(
    file: &str,
    read: &mut dyn FnMut(&str) -> Result<Option<String>, GraphError>,
) -> Result<Vec<(String, String)>, GraphError> {
    let mut found = Vec::new();
    let mut seen = BTreeSet::new();
    let mut pending = VecDeque::from([vec![file.to_owned()]]);
    while let Some(candidates) = pending.pop_front() {
        if found.len() >= MAX_FILES {
            break;
        }
        let Some((path, source)) = first_existing(&candidates, read)? else {
            continue;
        };
        if !seen.insert(path.clone()) {
            continue;
        }
        for specifier in relative_imports(&path, &source) {
            pending.push_back(candidate_files(&path, &specifier));
        }
        found.push((path, source));
    }
    Ok(found)
}

fn first_existing(
    candidates: &[String],
    read: &mut dyn FnMut(&str) -> Result<Option<String>, GraphError>,
) -> Result<Option<(String, String)>, GraphError> {
    for candidate in candidates {
        if let Some(source) = read(candidate)? {
            return Ok(Some((candidate.clone(), source)));
        }
    }
    Ok(None)
}

fn parse(path: &str, source: &str) -> Option<tree_sitter::Tree> {
    let is_tsx = std::path::Path::new(path)
        .extension()
        .is_some_and(|extension| extension.eq_ignore_ascii_case("tsx"));
    let (grammar, language) = if is_tsx {
        (
            tree_sitter_typescript::LANGUAGE_TSX,
            lemma_model::Language::Tsx,
        )
    } else {
        (
            tree_sitter_typescript::LANGUAGE_TYPESCRIPT,
            lemma_model::Language::TypeScript,
        )
    };
    let mut parser = Parser::new();
    parser.set_language(&grammar.into()).ok()?;
    lemma_syntax::parse_source(&mut parser, language, source)
}

/// The relative module specifiers (`./types`, `../models/charge`) a file
/// imports from.
fn relative_imports(path: &str, source: &str) -> Vec<String> {
    let Some(tree) = parse(path, source) else {
        return Vec::new();
    };
    let root = tree.root_node();
    let mut cursor = root.walk();
    root.named_children(&mut cursor)
        .filter(|node| matches!(node.kind(), "import_statement" | "export_statement"))
        .filter_map(|node| node.child_by_field_name("source"))
        .map(|specifier| text(specifier, source).trim_matches(['"', '\'']).to_owned())
        .filter(|specifier| specifier.starts_with('.'))
        .collect()
}

/// The files a relative specifier can mean, in the order TypeScript tries.
fn candidate_files(importer: &str, specifier: &str) -> Vec<String> {
    let directory = importer
        .rsplit_once('/')
        .map_or("", |(directory, _)| directory);
    let base = normalize(&format!("{directory}/{specifier}"));
    let base = base.strip_suffix(".js").unwrap_or(&base).to_owned();
    ["", ".ts", ".tsx", ".d.ts", "/index.ts", "/index.tsx"]
        .iter()
        .map(|suffix| format!("{base}{suffix}"))
        .filter(|candidate| {
            std::path::Path::new(candidate)
                .extension()
                .is_some_and(|extension| extension == "ts" || extension == "tsx")
        })
        .collect()
}

/// `a/./b/../c` → `a/c`, without a leading `/`.
fn normalize(path: &str) -> String {
    let mut parts: Vec<&str> = Vec::new();
    for part in path.split('/') {
        match part {
            "" | "." => {}
            ".." => {
                parts.pop();
            }
            other => parts.push(other),
        }
    }
    parts.join("/")
}

/// A declaration: its node and the source it is in.
type Declaration<'tree> = (Node<'tree>, &'tree str);

fn collect_declarations<'tree>(
    node: Node<'tree>,
    source: &'tree str,
    out: &mut BTreeMap<String, Declaration<'tree>>,
) {
    let kinds = [
        "interface_declaration",
        "type_alias_declaration",
        "enum_declaration",
    ];
    if kinds.contains(&node.kind()) {
        if let Some(name) = node.child_by_field_name("name") {
            out.entry(text(name, source).to_owned())
                .or_insert((node, source));
        }
        return;
    }
    let mut cursor = node.walk();
    for child in node.named_children(&mut cursor) {
        collect_declarations(child, source, out);
    }
}

struct Reader<'a, 'tree> {
    declarations: &'a BTreeMap<String, Declaration<'tree>>,
    unresolved: BTreeSet<String>,
    depth: usize,
}

impl Reader<'_, '_> {
    fn declaration(&mut self, (node, source): Declaration<'_>) -> Value {
        match node.kind() {
            "interface_declaration" => self.interface(node, source),
            "enum_declaration" => enumeration(node, source),
            _ => node
                .child_by_field_name("value")
                .map_or_else(|| json!({}), |value| self.type_schema(value, source)),
        }
    }

    fn interface(&mut self, node: Node<'_>, source: &str) -> Value {
        let mut schema = node.child_by_field_name("body").map_or_else(
            || object(Map::new(), Vec::new()),
            |body| self.members(body, source),
        );
        let mut cursor = node.walk();
        let bases: Vec<Node<'_>> = node
            .named_children(&mut cursor)
            .filter(|child| child.kind() == "extends_type_clause")
            .flat_map(|clause| {
                let mut cursor = clause.walk();
                clause.named_children(&mut cursor).collect::<Vec<_>>()
            })
            .collect();
        for base in bases {
            let inherited = self.type_schema(base, source);
            merge_object(&mut schema, &inherited);
        }
        schema
    }

    /// The members of an interface body or object type.
    fn members(&mut self, body: Node<'_>, source: &str) -> Value {
        let mut properties = Map::new();
        let mut required = Vec::new();
        let mut additional = None;
        let mut cursor = body.walk();
        for member in body.named_children(&mut cursor) {
            match member.kind() {
                "property_signature" => {
                    self.property(member, source, &mut properties, &mut required);
                }
                "index_signature" => {
                    additional = member
                        .child_by_field_name("type")
                        .map(|kind| self.annotation(kind, source).0);
                }
                _ => {}
            }
        }
        let mut schema = object(properties, required);
        if let Some(additional) = additional {
            schema["additionalProperties"] = additional;
        }
        schema
    }

    fn property(
        &mut self,
        member: Node<'_>,
        source: &str,
        properties: &mut Map<String, Value>,
        required: &mut Vec<String>,
    ) {
        let Some(name) = member.child_by_field_name("name") else {
            return;
        };
        let name = text(name, source).trim_matches(['"', '\'']).to_owned();
        let marked_optional = (0..member.child_count())
            .filter_map(|index| member.child(index))
            .any(|child| child.kind() == "?");
        let (schema, may_be_undefined) = member
            .child_by_field_name("type")
            .map_or_else(|| (json!({}), false), |kind| self.annotation(kind, source));
        if !marked_optional && !may_be_undefined {
            required.push(name.clone());
        }
        properties.insert(name, schema);
    }

    /// A type annotation (`: T`) or a type: its schema, and whether it
    /// admits `undefined`.
    fn annotation(&mut self, node: Node<'_>, source: &str) -> (Value, bool) {
        let kind = if node.kind() == "type_annotation" {
            node.named_child(0)
        } else {
            Some(node)
        };
        let Some(kind) = kind else {
            return (json!({}), false);
        };
        if kind.kind() == "union_type" {
            return self.union(kind, source);
        }
        (self.type_schema(kind, source), false)
    }

    fn type_schema(&mut self, node: Node<'_>, source: &str) -> Value {
        match node.kind() {
            "predefined_type" => predefined(text(node, source)),
            "literal_type" => literal(text(node, source)),
            "union_type" => self.union(node, source).0,
            "intersection_type" => self.intersection(node, source),
            "array_type" => {
                let items = node
                    .named_child(0)
                    .map_or_else(|| json!({}), |inner| self.type_schema(inner, source));
                json!({ "type": "array", "items": items })
            }
            "object_type" => self.members(node, source),
            "generic_type" => self.generic(node, source),
            "type_identifier" | "nested_type_identifier" => self.named(text(node, source)),
            "parenthesized_type" | "readonly_type" | "type_annotation" => node
                .named_child(0)
                .map_or_else(|| json!({}), |inner| self.type_schema(inner, source)),
            "tuple_type" => json!({ "type": "array" }),
            _ => json!({}),
        }
    }

    /// A union's members, with `null` making it nullable and `undefined`
    /// making it optional; string literals become an enumeration.
    fn union(&mut self, node: Node<'_>, source: &str) -> (Value, bool) {
        let mut members = Vec::new();
        union_members(node, &mut members);
        let mut has_null = false;
        let mut has_undefined = false;
        let mut strings = Vec::new();
        let mut schemas = Vec::new();
        for member in members {
            match text(member, source) {
                "null" => has_null = true,
                "undefined" | "void" => has_undefined = true,
                literal if member.kind() == "literal_type" && is_string_literal(literal) => {
                    strings.push(literal.trim_matches(['"', '\'']).to_owned());
                }
                _ => schemas.push(self.type_schema(member, source)),
            }
        }
        if !strings.is_empty() {
            schemas.push(string_enum(strings));
        }
        let schema = any_of(schemas);
        let schema = if has_null { nullable(schema) } else { schema };
        (schema, has_undefined)
    }

    fn intersection(&mut self, node: Node<'_>, source: &str) -> Value {
        let mut merged = object(Map::new(), Vec::new());
        let mut cursor = node.walk();
        for member in node.named_children(&mut cursor) {
            let part = self.type_schema(member, source);
            merge_object(&mut merged, &part);
        }
        merged
    }

    fn generic(&mut self, node: Node<'_>, source: &str) -> Value {
        let name = node
            .child_by_field_name("name")
            .map_or("", |name| text(name, source));
        let arguments: Vec<Node<'_>> = node
            .child_by_field_name("type_arguments")
            .map(|list| {
                let mut cursor = list.walk();
                list.named_children(&mut cursor).collect()
            })
            .unwrap_or_default();
        let argument = |index: usize, reader: &mut Self| {
            arguments
                .get(index)
                .map_or_else(|| json!({}), |inner| reader.type_schema(*inner, source))
        };
        match name {
            "Array" | "ReadonlyArray" | "Set" => {
                json!({ "type": "array", "items": argument(0, self) })
            }
            "Record" | "Map" => {
                json!({ "type": "object", "additionalProperties": argument(1, self) })
            }
            "Partial" => {
                let mut schema = argument(0, self);
                if let Some(object) = schema.as_object_mut() {
                    object.remove("required");
                }
                schema
            }
            "Promise" | "Readonly" | "Required" | "NonNullable" => argument(0, self),
            other => self.named(other),
        }
    }

    fn named(&mut self, name: &str) -> Value {
        if name == "Date" {
            return json!({ "type": "string", "format": "date-time" });
        }
        let short = name.rsplit('.').next().unwrap_or(name);
        let Some(&declaration) = self.declarations.get(short) else {
            self.unresolved.insert(name.to_owned());
            return json!({});
        };
        if self.depth >= MAX_DEPTH {
            return json!({});
        }
        self.depth += 1;
        let schema = self.declaration(declaration);
        self.depth -= 1;
        schema
    }
}

/// Members of nested unions (`A | B | C` parses as `(A | B) | C`).
fn union_members<'tree>(node: Node<'tree>, out: &mut Vec<Node<'tree>>) {
    let mut cursor = node.walk();
    for child in node.named_children(&mut cursor) {
        if child.kind() == "union_type" {
            union_members(child, out);
        } else {
            out.push(child);
        }
    }
}

/// Adds an object's properties and required names to `into`'s.
fn merge_object(into: &mut Value, from: &Value) {
    if let (Some(target), Some(source)) = (
        into.get_mut("properties").and_then(Value::as_object_mut),
        from.get("properties").and_then(Value::as_object),
    ) {
        for (name, schema) in source {
            target.entry(name.clone()).or_insert_with(|| schema.clone());
        }
    }
    let Some(names) = from.get("required").and_then(Value::as_array) else {
        return;
    };
    let Some(object) = into.as_object_mut() else {
        return;
    };
    let required = object.entry("required").or_insert_with(|| json!([]));
    if let Value::Array(existing) = required {
        for name in names {
            if !existing.contains(name) {
                existing.push(name.clone());
            }
        }
    }
}

fn enumeration(node: Node<'_>, source: &str) -> Value {
    let Some(body) = node.child_by_field_name("body") else {
        return json!({});
    };
    let mut cursor = body.walk();
    let mut strings = Vec::new();
    let mut numeric = false;
    for member in body.named_children(&mut cursor) {
        let value = member
            .child_by_field_name("value")
            .map(|value| text(value, source));
        match value {
            Some(literal) if is_string_literal(literal) => {
                strings.push(literal.trim_matches(['"', '\'']).to_owned());
            }
            _ => numeric = true,
        }
    }
    if numeric {
        return json!({ "type": "integer" });
    }
    string_enum(strings)
}

fn predefined(name: &str) -> Value {
    match name {
        "string" => json!({ "type": "string" }),
        "number" => json!({ "type": "number" }),
        "boolean" => json!({ "type": "boolean" }),
        "null" => json!({ "type": "null" }),
        "object" => json!({ "type": "object" }),
        _ => json!({}),
    }
}

fn literal(text: &str) -> Value {
    if is_string_literal(text) {
        return json!({ "const": text.trim_matches(['"', '\'']) });
    }
    match text {
        "null" => json!({ "type": "null" }),
        "true" => json!({ "const": true }),
        "false" => json!({ "const": false }),
        number => number
            .parse::<f64>()
            .map_or_else(|_| json!({}), |value| json!({ "const": value })),
    }
}

fn is_string_literal(text: &str) -> bool {
    (text.starts_with('"') && text.ends_with('"'))
        || (text.starts_with('\'') && text.ends_with('\''))
}

fn text<'s>(node: Node<'_>, source: &'s str) -> &'s str {
    node.utf8_text(source.as_bytes()).unwrap_or_default()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn schema(files: &[(&str, &str)], file: &str, name: &str) -> TypeScriptSchema {
        let files: BTreeMap<String, String> = files
            .iter()
            .map(|(path, source)| ((*path).to_owned(), (*source).to_owned()))
            .collect();
        let mut read = |path: &str| Ok(files.get(path).cloned());
        type_schema(file, name, &mut read).unwrap().unwrap()
    }

    #[test]
    fn interfaces_follow_optional_markers_unions_and_imports() {
        let models = r#"
export interface Base { id: string }
export enum Status { Pending = "pending", Paid = "paid" }
"#;
        let charge = r#"
import type { Base, Status } from "../models";
export interface Charge extends Base {
  amount: number;
  note?: string;
  receipt: string | null;
  refunded: boolean | undefined;
  kind: "card" | "bank";
  status: Status;
  tags: Array<string>;
  metadata: Record<string, unknown>;
  lines: { sku: string; quantity?: number }[];
  customer: Customer;
}
"#;

        let found = schema(
            &[
                ("src/models/index.ts", models),
                ("src/api/charge.ts", charge),
            ],
            "src/api/charge.ts",
            "Charge",
        );

        let properties = &found.schema["properties"];
        assert_eq!(properties["id"]["type"], "string");
        assert_eq!(properties["receipt"]["type"], json!(["string", "null"]));
        assert_eq!(
            properties["kind"],
            json!({ "type": "string", "enum": ["card", "bank"] })
        );
        assert_eq!(properties["status"]["enum"], json!(["pending", "paid"]));
        assert_eq!(properties["tags"]["items"]["type"], "string");
        assert_eq!(properties["metadata"]["type"], "object");
        assert_eq!(properties["lines"]["items"]["required"], json!(["sku"]));
        assert_eq!(
            found.schema["required"],
            json!([
                "amount", "receipt", "kind", "status", "tags", "metadata", "lines", "customer",
                "id"
            ])
        );
        assert!(found.unresolved.contains("Customer"));
    }

    #[test]
    fn aliases_intersections_and_partials_are_read() {
        let source = r"
type Id = { id: string };
type Named = Id & { name: string };
type Patch = Partial<Named>;
";

        let named = schema(&[("a.ts", source)], "a.ts", "Named");
        let patch = schema(&[("a.ts", source)], "a.ts", "Patch");

        assert_eq!(named.schema["required"], json!(["id", "name"]));
        assert_eq!(patch.schema.get("required"), None);
        assert_eq!(patch.schema["properties"]["name"]["type"], "string");
    }

    #[test]
    fn relative_specifiers_resolve_like_typescript() {
        assert_eq!(
            candidate_files("src/api/charge.ts", "../models"),
            [
                "src/models.ts",
                "src/models.tsx",
                "src/models.d.ts",
                "src/models/index.ts",
                "src/models/index.tsx"
            ]
        );
        assert_eq!(candidate_files("a.ts", "./b.js")[0], "b.ts");
    }
}
