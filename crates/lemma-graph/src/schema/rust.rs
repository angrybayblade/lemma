//! Rust types as JSON Schema, by serde's rules, read with Tree-sitter.
//!
//! Fields travel under their names, changed by `rename` and the
//! container's `rename_all`; `skip` drops them and `flatten` inlines them.
//! Sent, every field is present unless `skip_serializing_if` may leave it
//! out, and an `Option` is `null` when empty. Received, a field is required
//! unless it is an `Option` or has a `default`, and only an `Option` accepts
//! `null`; `deny_unknown_fields` rejects others. Enums follow serde's
//! tagging: external (the default), internal (`tag`), adjacent (`tag` and
//! `content`), or `untagged`. Types defined in the files given are
//! followed; others are left open (`{}`) and reported.

use std::collections::{BTreeMap, BTreeSet};

use serde_json::{Map, Value, json};
use tree_sitter::{Node, Parser};

use super::shape::{Fields, View, any_of, nullable, object, string_enum};

/// How deep to follow named types inside each other.
const MAX_DEPTH: usize = 12;

/// A Rust type's schema, and the names it uses that could not be followed.
#[derive(Debug, Clone, PartialEq)]
pub struct RustSchema {
    /// The JSON Schema.
    pub schema: Value,
    /// Named types used but not defined in the files read.
    pub unresolved: BTreeSet<String>,
}

/// The schema of `type_name`, defined in one of `files`, as seen from
/// `view`. `None` if no file defines it.
pub fn type_schema(files: &[(String, String)], type_name: &str, view: View) -> Option<RustSchema> {
    let mut parser = Parser::new();
    parser
        .set_language(&tree_sitter_rust::LANGUAGE.into())
        .ok()?;
    let trees: Vec<(tree_sitter::Tree, &str)> = files
        .iter()
        .filter_map(|(_, source)| Some((parser.parse(source, None)?, source.as_str())))
        .collect();
    let mut items = BTreeMap::new();
    for (tree, source) in &trees {
        collect_items(tree.root_node(), source, &mut items);
    }
    let item = *items.get(type_name)?;
    let mut reader = Reader {
        items: &items,
        unresolved: BTreeSet::new(),
        depth: 0,
        view,
    };
    let schema = reader.item(item);
    Some(RustSchema {
        schema,
        unresolved: reader.unresolved,
    })
}

/// A struct, enum or type alias: its node and the source it is in.
type Item<'tree> = (Node<'tree>, &'tree str);

fn collect_items<'tree>(
    node: Node<'tree>,
    source: &'tree str,
    out: &mut BTreeMap<String, Item<'tree>>,
) {
    if matches!(node.kind(), "struct_item" | "enum_item" | "type_item") {
        if let Some(name) = node.child_by_field_name("name") {
            out.entry(text(name, source).to_owned())
                .or_insert((node, source));
        }
        return;
    }
    let mut cursor = node.walk();
    for child in node.named_children(&mut cursor) {
        collect_items(child, source, out);
    }
}

struct Reader<'a, 'tree> {
    items: &'a BTreeMap<String, Item<'tree>>,
    unresolved: BTreeSet<String>,
    depth: usize,
    view: View,
}

/// A type's schema, and whether it is an `Option` (which serde lets a
/// receiver leave out).
struct Typed {
    schema: Value,
    is_option: bool,
}

impl Reader<'_, '_> {
    fn item(&mut self, (node, source): Item<'_>) -> Value {
        let attributes = serde_attributes(node, source);
        match node.kind() {
            "struct_item" => self.structure(node, source, &attributes),
            "enum_item" => self.enumeration(node, source, &attributes),
            _ => node
                .child_by_field_name("type")
                .map_or_else(|| json!({}), |kind| self.type_schema(kind, source).schema),
        }
    }

    fn structure(&mut self, node: Node<'_>, source: &str, attributes: &Attributes) -> Value {
        let Some(body) = node.child_by_field_name("body") else {
            return json!({ "type": "null" });
        };
        if body.kind() == "ordered_field_declaration_list" {
            return self.tuple(body, source);
        }
        self.fields(body, source, attributes)
    }

    /// A tuple struct: a newtype is its one field; others are arrays.
    fn tuple(&mut self, body: Node<'_>, source: &str) -> Value {
        let mut cursor = body.walk();
        let types: Vec<Node<'_>> = body
            .named_children(&mut cursor)
            .filter(|child| {
                child.kind() != "attribute_item" && child.kind() != "visibility_modifier"
            })
            .collect();
        match types.as_slice() {
            [only] => self.type_schema(*only, source).schema,
            _ => json!({ "type": "array" }),
        }
    }

    fn fields(&mut self, body: Node<'_>, source: &str, container: &Attributes) -> Value {
        let mut fields = Fields::default();
        let mut cursor = body.walk();
        for field in body
            .named_children(&mut cursor)
            .filter(|child| child.kind() == "field_declaration")
        {
            self.field(field, source, container, &mut fields);
        }
        let mut schema = fields.into_object();
        if self.view == View::Decode && container.has("deny_unknown_fields") {
            schema["additionalProperties"] = json!(false);
        }
        schema
    }

    fn field(
        &mut self,
        field: Node<'_>,
        source: &str,
        container: &Attributes,
        fields: &mut Fields,
    ) {
        let attributes = serde_attributes(field, source);
        let skipped = match self.view {
            View::Encode => attributes.has("skip") || attributes.has("skip_serializing"),
            View::Decode => attributes.has("skip") || attributes.has("skip_deserializing"),
        };
        let (Some(name), Some(kind)) = (
            field.child_by_field_name("name"),
            field.child_by_field_name("type"),
        ) else {
            return;
        };
        if skipped {
            return;
        }
        let typed = self.type_schema(kind, source);
        if attributes.has("flatten") {
            fields.extend_from(&typed.schema);
            return;
        }
        let wire = attributes.value("rename").map_or_else(
            || rename(text(name, source), container.value("rename_all")),
            str::to_owned,
        );
        let is_required = match self.view {
            View::Encode => !attributes.has("skip_serializing_if"),
            View::Decode => {
                !typed.is_option && !attributes.has("default") && !container.has("default")
            }
        };
        if self.view == View::Decode {
            for alias in attributes.values("alias") {
                fields.insert(alias.to_owned(), typed.schema.clone(), false);
            }
        }
        fields.insert(wire, typed.schema, is_required);
    }

    fn enumeration(&mut self, node: Node<'_>, source: &str, attributes: &Attributes) -> Value {
        let Some(body) = node.child_by_field_name("body") else {
            return json!({});
        };
        let mut cursor = body.walk();
        let variants: Vec<Node<'_>> = body
            .named_children(&mut cursor)
            .filter(|child| child.kind() == "enum_variant")
            .collect();
        let all_unit = variants
            .iter()
            .all(|variant| variant.child_by_field_name("body").is_none());
        if all_unit && attributes.value("tag").is_none() && !attributes.has("untagged") {
            let names = variants
                .iter()
                .map(|variant| variant_name(*variant, source, attributes))
                .collect();
            return string_enum(names);
        }
        let schemas = variants
            .iter()
            .map(|variant| self.variant(*variant, source, attributes))
            .collect();
        any_of(schemas)
    }

    /// One variant, tagged as the enum says.
    fn variant(&mut self, variant: Node<'_>, source: &str, attributes: &Attributes) -> Value {
        let name = variant_name(variant, source, attributes);
        let content = variant
            .child_by_field_name("body")
            .map(|body| match body.kind() {
                "ordered_field_declaration_list" => self.tuple(body, source),
                _ => self.fields(body, source, &serde_attributes(variant, source)),
            });
        if attributes.has("untagged") {
            return content.unwrap_or_else(|| json!({ "type": "null" }));
        }
        let tag = json!({ "const": name });
        match (
            attributes.value("tag"),
            attributes.value("content"),
            content,
        ) {
            (Some(tag_name), Some(content_name), Some(content)) => {
                let properties = Map::from_iter([
                    (tag_name.to_owned(), tag),
                    (content_name.to_owned(), content),
                ]);
                object(
                    properties,
                    vec![tag_name.to_owned(), content_name.to_owned()],
                )
            }
            (Some(tag_name), _, content) => {
                let mut schema = content.unwrap_or_else(|| object(Map::new(), Vec::new()));
                tagged(&mut schema, tag_name, tag);
                schema
            }
            (None, _, None) => json!({ "const": name }),
            (None, _, Some(content)) => {
                object(Map::from_iter([(name.clone(), content)]), vec![name])
            }
        }
    }

    fn type_schema(&mut self, node: Node<'_>, source: &str) -> Typed {
        let plain = |schema: Value| Typed {
            schema,
            is_option: false,
        };
        match node.kind() {
            "primitive_type" => plain(primitive(text(node, source)).unwrap_or_else(|| json!({}))),
            "reference_type" => node
                .child_by_field_name("type")
                .map_or_else(|| plain(json!({})), |inner| self.type_schema(inner, source)),
            "array_type" => {
                let items = self.element(node, source, "element");
                plain(json!({ "type": "array", "items": items }))
            }
            "tuple_type" => plain(json!({ "type": "array" })),
            "generic_type" => self.generic(node, source),
            "type_identifier" | "scoped_type_identifier" => plain(self.named(text(node, source))),
            _ => plain(json!({})),
        }
    }

    fn element(&mut self, node: Node<'_>, source: &str, field: &str) -> Value {
        node.child_by_field_name(field)
            .map_or_else(|| json!({}), |inner| self.type_schema(inner, source).schema)
    }

    fn generic(&mut self, node: Node<'_>, source: &str) -> Typed {
        let base = node
            .child_by_field_name("type")
            .map_or("", |base| last_segment(text(base, source)));
        let arguments: Vec<Node<'_>> = node
            .child_by_field_name("type_arguments")
            .map(|list| {
                let mut cursor = list.walk();
                list.named_children(&mut cursor).collect()
            })
            .unwrap_or_default();
        let argument = |index: usize, reader: &mut Self| {
            arguments.get(index).map_or_else(
                || json!({}),
                |inner| reader.type_schema(*inner, source).schema,
            )
        };
        let schema = match base {
            "Option" => {
                return Typed {
                    schema: nullable(argument(0, self)),
                    is_option: true,
                };
            }
            "Vec" | "VecDeque" | "HashSet" | "BTreeSet" | "IndexSet" => {
                json!({ "type": "array", "items": argument(0, self) })
            }
            "HashMap" | "BTreeMap" | "IndexMap" => {
                json!({ "type": "object", "additionalProperties": argument(1, self) })
            }
            "Box" | "Rc" | "Arc" | "Cow" => argument(0, self),
            "DateTime" => json!({ "type": "string", "format": "date-time" }),
            other => self.named(other),
        };
        Typed {
            schema,
            is_option: false,
        }
    }

    fn named(&mut self, name: &str) -> Value {
        let short = last_segment(name);
        if let Some(known) = well_known(short) {
            return known;
        }
        let Some(&item) = self.items.get(short) else {
            self.unresolved.insert(name.to_owned());
            return json!({});
        };
        if self.depth >= MAX_DEPTH {
            return json!({});
        }
        self.depth += 1;
        let schema = self.item(item);
        self.depth -= 1;
        schema
    }
}

/// An internally tagged variant: its fields plus the tag.
fn tagged(schema: &mut Value, tag_name: &str, tag: Value) {
    if let Some(properties) = schema.get_mut("properties").and_then(Value::as_object_mut) {
        properties.insert(tag_name.to_owned(), tag);
    }
    let required = schema
        .as_object_mut()
        .map(|object| object.entry("required").or_insert_with(|| json!([])));
    if let Some(Value::Array(names)) = required {
        names.push(json!(tag_name));
    }
}

fn variant_name(variant: Node<'_>, source: &str, container: &Attributes) -> String {
    let attributes = serde_attributes(variant, source);
    if let Some(renamed) = attributes.value("rename") {
        return renamed.to_owned();
    }
    let name = variant
        .child_by_field_name("name")
        .map_or("", |name| text(name, source));
    rename(name, container.value("rename_all"))
}

fn primitive(name: &str) -> Option<Value> {
    let schema = match name {
        "bool" => json!({ "type": "boolean" }),
        "i8" | "i16" | "i32" | "i64" | "i128" | "isize" | "u8" | "u16" | "u32" | "u64" | "u128"
        | "usize" => json!({ "type": "integer" }),
        "f32" | "f64" => json!({ "type": "number" }),
        "char" | "str" => json!({ "type": "string" }),
        _ => return None,
    };
    Some(schema)
}

/// Types from the standard library and common crates that serde encodes
/// in a known way.
fn well_known(name: &str) -> Option<Value> {
    let schema = match name {
        "String" | "PathBuf" => json!({ "type": "string" }),
        "Uuid" => json!({ "type": "string", "format": "uuid" }),
        "NaiveDate" => json!({ "type": "string", "format": "date" }),
        "NaiveDateTime" | "OffsetDateTime" => json!({ "type": "string", "format": "date-time" }),
        "Value" => json!({}),
        other => return primitive(other),
    };
    Some(schema)
}

fn last_segment(path: &str) -> &str {
    path.rsplit("::").next().unwrap_or(path)
}

/// A name in the case `rename_all` asks for: `camelCase`, `PascalCase`,
/// `snake_case`, `SCREAMING_SNAKE_CASE`, `kebab-case`,
/// `SCREAMING-KEBAB-CASE`, `lowercase` or `UPPERCASE`.
fn rename(name: &str, rule: Option<&str>) -> String {
    let Some(rule) = rule else {
        return name.to_owned();
    };
    let words = words(name);
    let capitalized = || -> Vec<String> { words.iter().map(|word| capitalize(word)).collect() };
    match rule {
        "lowercase" => words.concat(),
        "UPPERCASE" => words.concat().to_uppercase(),
        "PascalCase" => capitalized().concat(),
        "camelCase" => {
            let mut parts = capitalized();
            if let Some(first) = parts.first_mut() {
                *first = first.to_lowercase();
            }
            parts.concat()
        }
        "snake_case" => words.join("_"),
        "SCREAMING_SNAKE_CASE" => words.join("_").to_uppercase(),
        "kebab-case" => words.join("-"),
        "SCREAMING-KEBAB-CASE" => words.join("-").to_uppercase(),
        _ => name.to_owned(),
    }
}

/// A `snake_case` or `PascalCase` name's words, in lower case.
fn words(name: &str) -> Vec<String> {
    let mut words = Vec::new();
    let mut current = String::new();
    for character in name.chars() {
        if character == '_' {
            words.push(std::mem::take(&mut current));
            continue;
        }
        if character.is_uppercase() && !current.is_empty() {
            words.push(std::mem::take(&mut current));
        }
        current.extend(character.to_lowercase());
    }
    words.push(current);
    words.retain(|word| !word.is_empty());
    words
}

fn capitalize(word: &str) -> String {
    let mut characters = word.chars();
    characters.next().map_or_else(String::new, |first| {
        first.to_uppercase().chain(characters).collect()
    })
}

/// The arguments of the `#[serde(...)]` attributes on an item, field or
/// variant: `rename = "x"` as a key with a value, `skip` as a bare key.
#[derive(Debug, Default)]
struct Attributes(Vec<(String, Option<String>)>);

impl Attributes {
    fn has(&self, key: &str) -> bool {
        self.0.iter().any(|(name, _)| name == key)
    }

    fn value<'a>(&'a self, key: &'a str) -> Option<&'a str> {
        self.values(key).next()
    }

    fn values<'a>(&'a self, key: &'a str) -> impl Iterator<Item = &'a str> {
        self.0
            .iter()
            .filter(move |(name, _)| name == key)
            .filter_map(|(_, value)| value.as_deref())
    }
}

/// The serde attributes written just before `node`.
fn serde_attributes(node: Node<'_>, source: &str) -> Attributes {
    let mut found = Vec::new();
    let mut previous = node.prev_sibling();
    while let Some(sibling) = previous {
        match sibling.kind() {
            "attribute_item" => found.extend(serde_arguments(text(sibling, source))),
            "line_comment" | "block_comment" => {}
            _ => break,
        }
        previous = sibling.prev_sibling();
    }
    Attributes(found)
}

/// `#[serde(rename = "a", skip)]` → `[("rename", Some("a")), ("skip", None)]`.
fn serde_arguments(attribute: &str) -> Vec<(String, Option<String>)> {
    let inner = attribute
        .trim()
        .strip_prefix("#[")
        .and_then(|rest| rest.strip_suffix(']'))
        .map(str::trim);
    let Some(arguments) = inner
        .and_then(|inner| inner.strip_prefix("serde"))
        .map(str::trim)
        .and_then(|rest| rest.strip_prefix('('))
        .and_then(|rest| rest.strip_suffix(')'))
    else {
        return Vec::new();
    };
    split_top_level(arguments)
        .into_iter()
        .map(|argument| match argument.split_once('=') {
            Some((key, value)) => (
                key.trim().to_owned(),
                Some(value.trim().trim_matches('"').to_owned()),
            ),
            None => (argument.trim().to_owned(), None),
        })
        .filter(|(key, _)| !key.is_empty())
        .collect()
}

/// Splits on commas outside quotes and parentheses.
fn split_top_level(text: &str) -> Vec<String> {
    let mut parts = Vec::new();
    let mut current = String::new();
    let mut depth = 0usize;
    let mut quoted = false;
    for character in text.chars() {
        match character {
            '"' => quoted = !quoted,
            '(' if !quoted => depth += 1,
            ')' if !quoted => depth = depth.saturating_sub(1),
            ',' if !quoted && depth == 0 => {
                parts.push(std::mem::take(&mut current));
                continue;
            }
            _ => {}
        }
        current.push(character);
    }
    parts.push(current);
    parts
}

fn text<'s>(node: Node<'_>, source: &'s str) -> &'s str {
    node.utf8_text(source.as_bytes()).unwrap_or_default()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn schema(source: &str, name: &str, view: View) -> RustSchema {
        type_schema(&[("lib.rs".to_owned(), source.to_owned())], name, view).unwrap()
    }

    const CHARGE: &str = r#"
#[derive(Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct Charge {
    pub amount_cents: u64,
    #[serde(rename = "cur")]
    pub currency: String,
    pub note: Option<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub tags: Vec<String>,
    #[serde(skip)]
    pub cache: Cache,
    #[serde(flatten)]
    pub meta: Meta,
    pub status: Status,
    pub customer: models::Customer,
}

#[derive(Serialize, Deserialize)]
pub struct Meta {
    pub source: String,
}

#[derive(Serialize, Deserialize)]
#[serde(rename_all = "SCREAMING_SNAKE_CASE")]
pub enum Status {
    Pending,
    PaidInFull,
}
"#;

    #[test]
    fn sent_fields_follow_serde_renames_skips_and_flattening() {
        let found = schema(CHARGE, "Charge", View::Encode);

        let properties = &found.schema["properties"];
        assert_eq!(properties["amountCents"]["type"], "integer");
        assert_eq!(properties["cur"]["type"], "string");
        assert_eq!(properties["note"]["type"], json!(["string", "null"]));
        assert_eq!(properties["source"]["type"], "string");
        assert_eq!(
            properties["status"]["enum"],
            json!(["PENDING", "PAID_IN_FULL"])
        );
        assert!(properties.get("cache").is_none());
        assert_eq!(
            found.schema["required"],
            json!(["amountCents", "cur", "note", "source", "status", "customer"])
        );
        assert!(found.unresolved.contains("models::Customer"));
        assert!(found.schema.get("additionalProperties").is_none());
    }

    #[test]
    fn received_fields_are_required_unless_optional_or_defaulted() {
        let found = schema(CHARGE, "Charge", View::Decode);

        assert_eq!(
            found.schema["required"],
            json!(["amountCents", "cur", "source", "status", "customer"])
        );
        assert_eq!(found.schema["additionalProperties"], json!(false));
    }

    #[test]
    fn enums_follow_serde_tagging() {
        let source = r#"
#[serde(tag = "type")]
enum Event { Created { id: String }, Deleted }
enum External { Plain, Wrapped(u32) }
#[serde(untagged)]
enum Loose { Number(u32), Text(String) }
struct Id(String);
"#;

        let internal = schema(source, "Event", View::Encode).schema;
        let external = schema(source, "External", View::Encode).schema;
        let untagged = schema(source, "Loose", View::Encode).schema;
        let newtype = schema(source, "Id", View::Encode).schema;

        assert_eq!(
            internal["anyOf"][0],
            json!({ "type": "object", "properties": { "id": { "type": "string" }, "type": { "const": "Created" } }, "required": ["id", "type"] })
        );
        assert_eq!(
            internal["anyOf"][1]["properties"]["type"],
            json!({ "const": "Deleted" })
        );
        assert_eq!(external["anyOf"][0], json!({ "const": "Plain" }));
        assert_eq!(
            external["anyOf"][1],
            json!({ "type": "object", "properties": { "Wrapped": { "type": "integer" } }, "required": ["Wrapped"] })
        );
        assert_eq!(
            untagged,
            json!({ "anyOf": [{ "type": "integer" }, { "type": "string" }] })
        );
        assert_eq!(newtype, json!({ "type": "string" }));
    }

    #[test]
    fn rename_all_rules_rewrite_words() {
        assert_eq!(rename("amount_cents", Some("camelCase")), "amountCents");
        assert_eq!(rename("PaidInFull", Some("snake_case")), "paid_in_full");
        assert_eq!(rename("paid_in_full", Some("kebab-case")), "paid-in-full");
        assert_eq!(rename("Paid", None), "Paid");
    }
}
