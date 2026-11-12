//! The working directory's document, from the base commit's kept document
//! and the change, without running the adapter (experimental, the graph
//! walk of the north star).
//!
//! An edit usually touches one file. When every changed file is TypeScript
//! whose routes are the same at both ends, and its zod object schemas
//! changed only in which top-level keys they have or which are optional,
//! the base document is patched where those schemas are used. Anything
//! else, such as a nested change, a moved route or a schema no route in the
//! file uses, returns `None`, and the adapter reads the code as before.

use std::collections::{BTreeMap, BTreeSet};
use std::path::Path;
use std::process::Command;

use serde_json::{Value, json};
use tree_sitter::Node;

/// One top-level key of a zod object schema, as written.
#[derive(Debug, Clone, PartialEq, Eq)]
struct Key {
    optional: Option<bool>,
    /// The value's text, so an unchanged key is known to be unchanged.
    text: String,
    /// The schema a new key would get, when syntax says.
    schema: Option<Value>,
}

/// A file's zod object schemas by name: each one's keys in order.
type Schemas = BTreeMap<String, Vec<(String, Key)>>;

const OPTIONAL: [&str; 4] = ["optional", "nullish", "default", "catch"];
const KEEPS_OPTIONALITY: [&str; 12] = [
    "describe", "openapi", "min", "max", "regex", "url", "email", "uuid", "trim", "refine",
    "length", "nullable",
];

fn text<'s>(node: Node<'_>, source: &'s str) -> &'s str {
    &source[node.byte_range()]
}

/// Whether a key's value is optional: `None` when syntax cannot say.
/// The file's top-level declarations, by name: what an identifier in a
/// schema's key may stand for.
type Declared<'t> = BTreeMap<String, Node<'t>>;

/// How many declarations an identifier may be followed through before the
/// answer is unknown (a cycle, or a very long chain).
const FOLLOWED: usize = 16;

fn optionality(
    node: Node<'_>,
    source: &str,
    declared: &Declared<'_>,
    depth: usize,
) -> Option<bool> {
    if depth > FOLLOWED {
        return None;
    }
    match node.kind() {
        "call_expression" => {
            let function = node.child_by_field_name("function")?;
            if function.kind() != "member_expression" {
                return None;
            }
            let method = text(function.child_by_field_name("property")?, source);
            let object = function.child_by_field_name("object")?;
            if OPTIONAL.contains(&method) {
                return Some(true);
            }
            if object.kind() == "identifier" && text(object, source) == "z" {
                return Some(matches!(method, "undefined" | "any" | "unknown"));
            }
            if KEEPS_OPTIONALITY.contains(&method) {
                return optionality(object, source, declared, depth + 1);
            }
            if method == "object" && object.kind() == "identifier" && text(object, source) == "z" {
                return Some(false);
            }
            None
        }
        // A schema declared in the same file: as optional as its value. One
        // imported from elsewhere is unknown here.
        "identifier" => {
            let value = declared.get(text(node, source))?;
            optionality(*value, source, declared, depth + 1)
        }
        "parenthesized_expression" => {
            optionality(node.named_child(0)?, source, declared, depth + 1)
        }
        _ => None,
    }
}

/// The schema syntax gives a new key: `z.string()` and friends, with a
/// `.describe(...)` text when there is one.
fn simple_schema(node: Node<'_>, source: &str) -> Option<Value> {
    let mut current = node;
    let mut description = None;
    loop {
        if current.kind() != "call_expression" {
            return None;
        }
        let function = current.child_by_field_name("function")?;
        if function.kind() != "member_expression" {
            return None;
        }
        let method = text(function.child_by_field_name("property")?, source);
        let object = function.child_by_field_name("object")?;
        if object.kind() == "identifier" && text(object, source) == "z" {
            let kind = match method {
                "string" => "string",
                "number" => "number",
                "boolean" => "boolean",
                _ => return None,
            };
            let mut schema = json!({ "type": kind });
            if let Some(description) = description {
                schema["description"] = Value::String(description);
            }
            return Some(schema);
        }
        if method == "describe" {
            let arguments = current.child_by_field_name("arguments")?;
            let first = arguments.named_child(0)?;
            description = Some(
                text(first, source)
                    .trim_matches(['"', '\'', '`'])
                    .to_owned(),
            );
        } else if !KEEPS_OPTIONALITY.contains(&method) && !OPTIONAL.contains(&method) {
            return None;
        }
        current = object;
    }
}

/// The object literal of `z.object({...})` at the root of a chain, if any.
fn object_literal<'t>(mut node: Node<'t>, source: &str) -> Option<Node<'t>> {
    loop {
        if node.kind() != "call_expression" {
            return None;
        }
        let function = node.child_by_field_name("function")?;
        if function.kind() != "member_expression" {
            return None;
        }
        let method = text(function.child_by_field_name("property")?, source);
        let object = function.child_by_field_name("object")?;
        if method == "object" && object.kind() == "identifier" && text(object, source) == "z" {
            let argument = node.child_by_field_name("arguments")?.named_child(0)?;
            return (argument.kind() == "object").then_some(argument);
        }
        node = object;
    }
}

/// A file's top-level zod object schemas; `None` if one cannot be read by
/// syntax (a spread or a computed key).
fn schemas_of(source: &str, extractor: &mut lemma_syntax::Extractor) -> Option<Schemas> {
    let parsed = extractor
        .parse(lemma_model::Language::TypeScript, source)
        .ok()?;
    let declared = top_level_declarations(parsed.tree.root_node(), source);
    let mut schemas: Schemas = BTreeMap::new();
    for (name, value) in &declared {
        if let Some(literal) = object_literal(*value, source) {
            schemas.insert(name.clone(), object_keys(literal, source, &declared)?);
        }
    }
    Some(schemas)
}

/// The values of a file's top-level `const`/`let` declarations, exported or
/// not, by name.
fn top_level_declarations<'t>(root: Node<'t>, source: &str) -> Declared<'t> {
    let mut declared = BTreeMap::new();
    let mut cursor = root.walk();
    for statement in root.named_children(&mut cursor) {
        let declaration = if statement.kind() == "export_statement" {
            statement.child_by_field_name("declaration")
        } else {
            Some(statement)
        };
        let Some(declaration) = declaration.filter(|node| node.kind() == "lexical_declaration")
        else {
            continue;
        };
        let mut inner = declaration.walk();
        for declarator in declaration.named_children(&mut inner) {
            let name = declarator.child_by_field_name("name");
            let value = declarator.child_by_field_name("value");
            if let (Some(name), Some(value)) = (name, value) {
                declared.insert(text(name, source).to_owned(), value);
            }
        }
    }
    declared
}

/// The keys of a `z.object({...})` literal, in order; `None` when one cannot
/// be read by syntax (a spread or a computed key).
fn object_keys(
    literal: Node<'_>,
    source: &str,
    declared: &Declared<'_>,
) -> Option<Vec<(String, Key)>> {
    let mut keys = Vec::new();
    let mut pairs = literal.walk();
    for pair in literal.named_children(&mut pairs) {
        if pair.kind() == "comment" {
            continue;
        }
        if pair.kind() != "pair" {
            return None;
        }
        let key = pair.child_by_field_name("key")?;
        if !matches!(key.kind(), "property_identifier" | "string") {
            return None;
        }
        let value = pair.child_by_field_name("value")?;
        keys.push((
            text(key, source).trim_matches(['"', '\'']).to_owned(),
            Key {
                optional: optionality(value, source, declared, 0),
                text: text(value, source).to_owned(),
                schema: simple_schema(value, source),
            },
        ));
    }
    Some(keys)
}

/// Files the working directory changed against `base`, or `None` when Git
/// cannot say. New files are included.
fn changed_files(root: &Path, base: &str) -> Option<Vec<String>> {
    let diff = Command::new("git")
        .arg("-C")
        .arg(root)
        .args(["diff", "--name-only", "--no-ext-diff", base, "--"])
        .output()
        .ok()?;
    let untracked = Command::new("git")
        .arg("-C")
        .arg(root)
        .args(["ls-files", "--others", "--exclude-standard"])
        .output()
        .ok()?;
    if !diff.status.success() || !untracked.status.success() {
        return None;
    }
    let mut files: Vec<String> = String::from_utf8_lossy(&diff.stdout)
        .lines()
        .map(str::to_owned)
        .collect();
    // New files that are not code are read by no adapter.
    files.extend(
        String::from_utf8_lossy(&untracked.stdout)
            .lines()
            .filter(|path| is_typescript(path) || is_other_code(path))
            .map(str::to_owned),
    );
    Some(files)
}

fn is_typescript(path: &str) -> bool {
    [".ts", ".tsx", ".mts", ".cts"]
        .iter()
        .any(|extension| path.ends_with(extension))
}

fn is_json(path: &str) -> bool {
    Path::new(path)
        .extension()
        .is_some_and(|extension| extension.eq_ignore_ascii_case("json"))
}

fn is_script(path: &str) -> bool {
    [".js", ".jsx", ".mjs", ".cjs"]
        .iter()
        .any(|extension| path.ends_with(extension))
}

fn is_other_code(path: &str) -> bool {
    [".js", ".jsx", ".py", ".go", ".rs", ".json"]
        .iter()
        .any(|extension| path.ends_with(extension))
}

fn base_text(root: &Path, base: &str, path: &str) -> Option<String> {
    let output = Command::new("git")
        .arg("-C")
        .arg(root)
        .args(["show", &format!("{base}:{path}")])
        .output()
        .ok()?;
    output
        .status
        .success()
        .then(|| String::from_utf8_lossy(&output.stdout).into_owned())
}

/// What changed in one schema: keys added (with the schema for each) and
/// removed, and keys whose optionality changed.
#[derive(Debug, Clone, PartialEq, serde::Serialize, serde::Deserialize)]
pub(crate) struct SchemaChange {
    base_keys: BTreeSet<String>,
    added: Vec<(String, Value, bool)>,
    removed: Vec<String>,
    optionality: Vec<(String, bool)>,
}

/// The change between a schema's base and head keys, or `None` when it is
/// more than syntax can patch.
fn schema_change(base: &[(String, Key)], head: &[(String, Key)]) -> Option<SchemaChange> {
    let base_map: BTreeMap<&str, &Key> = base
        .iter()
        .map(|(name, key)| (name.as_str(), key))
        .collect();
    let head_map: BTreeMap<&str, &Key> = head
        .iter()
        .map(|(name, key)| (name.as_str(), key))
        .collect();
    let mut change = SchemaChange {
        base_keys: base_map.keys().map(|name| (*name).to_owned()).collect(),
        added: Vec::new(),
        removed: Vec::new(),
        optionality: Vec::new(),
    };
    for (name, key) in &head_map {
        match base_map.get(name) {
            None => {
                let optional = key.optional?;
                let schema = key.schema.clone()?;
                change.added.push(((*name).to_owned(), schema, optional));
            }
            Some(before) if before.text == key.text => {}
            Some(before) => {
                // Only optionality may differ; a value changed any other
                // way needs the adapter.
                let (was, is) = (before.optional?, key.optional?);
                let strip = |text: &str| text.replace(".optional()", "").replace(".nullish()", "");
                if strip(&before.text) != strip(&key.text) || was == is {
                    return None;
                }
                change.optionality.push(((*name).to_owned(), is));
            }
        }
    }
    for name in base_map.keys() {
        if !head_map.contains_key(name) {
            change.removed.push((*name).to_owned());
        }
    }
    Some(change)
}

/// Request body schemas in `document` whose properties are exactly `keys`.
fn matching_bodies<'d>(
    document: &'d mut Value,
    keys: &BTreeSet<String>,
) -> Vec<(String, &'d mut Value)> {
    let mut found = Vec::new();
    let Some(paths) = document.get_mut("paths").and_then(Value::as_object_mut) else {
        return found;
    };
    for (path, item) in paths.iter_mut() {
        let Some(methods) = item.as_object_mut() else {
            continue;
        };
        for (method, operation) in methods.iter_mut() {
            let key = format!("{} {path}", method.to_uppercase());
            let schema = operation.pointer_mut("/requestBody/content/application~1json/schema");
            let Some(schema) = schema else { continue };
            let properties: BTreeSet<String> = schema
                .get("properties")
                .and_then(Value::as_object)
                .map(|properties| properties.keys().cloned().collect())
                .unwrap_or_default();
            if &properties == keys {
                found.push((key, schema));
            }
        }
    }
    found
}

fn apply(schema: &mut Value, change: &SchemaChange) {
    let mut required: BTreeSet<String> = schema
        .get("required")
        .and_then(Value::as_array)
        .map(|names| {
            names
                .iter()
                .filter_map(Value::as_str)
                .map(str::to_owned)
                .collect()
        })
        .unwrap_or_default();
    if let Some(properties) = schema.get_mut("properties").and_then(Value::as_object_mut) {
        for name in &change.removed {
            properties.remove(name);
            required.remove(name);
        }
        for (name, property, optional) in &change.added {
            properties.insert(name.clone(), property.clone());
            if !optional {
                required.insert(name.clone());
            }
        }
    }
    for (name, optional) in &change.optionality {
        if *optional {
            required.remove(name);
        } else {
            required.insert(name.clone());
        }
    }
    schema["required"] = Value::Array(required.into_iter().map(Value::String).collect());
}

/// The head document: `base_document` patched for the working directory's
/// change against `base`, or `None` when the adapter must read the code.
pub(super) fn patched(
    root: &Path,
    base: &str,
    base_document: Value,
) -> Option<(Value, Vec<SchemaChange>)> {
    let mut changes = Vec::new();
    // Files in other languages (a Go service beside a TypeScript one) are
    // never imported by the code an adapter of TypeScript reads.
    let files: Vec<String> = changed_files(root, base)?
        .into_iter()
        .filter(|path| is_typescript(path) || is_script(path) || is_json(path))
        .collect();
    if files.iter().any(|path| !is_typescript(path)) {
        return None;
    }
    let mut extractor = lemma_syntax::Extractor::new().ok()?;
    let mut document = base_document;
    for path in &files {
        let before = base_text(root, base, path)?;
        let after = std::fs::read_to_string(root.join(path)).ok()?;
        let options = lemma_http::CodeOptions::default();
        let routes = |source: &str, extractor: &mut lemma_syntax::Extractor| {
            lemma_http::extract(path, source, &options, extractor)
                .ok()
                .map(|facts| {
                    let mut found: Vec<(String, String)> = facts
                        .method_paths
                        .iter()
                        .filter(|route| route.method != "*")
                        .map(|route| (route.method.clone(), route.path.clone()))
                        .collect();
                    found.sort();
                    found
                })
        };
        if routes(&before, &mut extractor)? != routes(&after, &mut extractor)? {
            return None;
        }
        let base_schemas = schemas_of(&before, &mut extractor)?;
        let head_schemas = schemas_of(&after, &mut extractor)?;
        if base_schemas.keys().collect::<Vec<_>>() != head_schemas.keys().collect::<Vec<_>>() {
            return None;
        }
        for (name, head) in &head_schemas {
            let base_keys = &base_schemas[name];
            if base_keys == head {
                continue;
            }
            let change = schema_change(base_keys, head)?;
            let bodies = matching_bodies(&mut document, &change.base_keys);
            if bodies.is_empty() {
                return None;
            }
            for (_, body) in bodies {
                apply(body, &change);
            }
            changes.push(change);
        }
    }
    Some((document, changes))
}

/// The changes a patched document was made with, from the document of
/// `base_digest`: what a contract laid over the base's document needs to be
/// patched the same way.
#[derive(Debug, Clone, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct Patch {
    pub(crate) base_digest: String,
    pub(crate) changes: Vec<SchemaChange>,
}

/// Applies `changes` to `document` where each one's schema is used, as
/// [`patched`] did to the adapter's document; the operations patched, as
/// `METHOD path`. Nothing is patched where a change's schema is found
/// nowhere in this document.
pub(crate) fn apply_changes(document: &mut Value, changes: &[SchemaChange]) -> Vec<String> {
    let mut patched = Vec::new();
    for change in changes {
        for (key, body) in matching_bodies(document, &change.base_keys) {
            apply(body, change);
            patched.push(key);
        }
    }
    patched
}

/// The commit the working directory at `root` is on.
pub(super) fn head_commit(root: &Path) -> Option<String> {
    let output = Command::new("git")
        .arg("-C")
        .arg(root)
        .args(["rev-parse", "HEAD"])
        .output()
        .ok()?;
    output
        .status
        .success()
        .then(|| String::from_utf8_lossy(&output.stdout).trim().to_owned())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn schemas(source: &str) -> Schemas {
        schemas_of(source, &mut lemma_syntax::Extractor::new().unwrap()).unwrap()
    }

    fn keys(schemas: &Schemas, name: &str) -> Vec<(String, Option<bool>)> {
        schemas[name]
            .iter()
            .map(|(key, found)| (key.clone(), found.optional))
            .collect()
    }

    #[test]
    fn a_zod_objects_keys_and_their_optionality_are_read_by_syntax() {
        let found = schemas(
            "import { z } from 'zod'\n\
             const Id = z.string().min(1)\n\
             export const Request = z.object({\n\
               id: Id.describe('the id'),\n\
               name: z.string().describe('a name'),\n\
               alias: z.string().optional(),\n\
               limit: z.number().default(10),\n\
             })\n",
        );

        assert_eq!(
            keys(&found, "Request"),
            [
                ("id".to_owned(), Some(false)),
                ("name".to_owned(), Some(false)),
                ("alias".to_owned(), Some(true)),
                ("limit".to_owned(), Some(true)),
            ]
        );
    }

    #[test]
    fn a_spread_into_a_zod_object_is_left_to_the_adapter() {
        let source = "const Base = z.object({ id: z.string() })\n\
                      export const Request = z.object({ ...Base.shape, name: z.string() })\n";

        let found = schemas_of(source, &mut lemma_syntax::Extractor::new().unwrap());

        assert!(found.is_none());
    }

    fn change_adding(field: &str, base_keys: &[&str]) -> SchemaChange {
        SchemaChange {
            base_keys: base_keys.iter().map(|key| (*key).to_owned()).collect(),
            added: vec![(field.to_owned(), json!({ "type": "string" }), false)],
            removed: Vec::new(),
            optionality: Vec::new(),
        }
    }

    fn body(properties: &[&str]) -> Value {
        let properties: serde_json::Map<String, Value> = properties
            .iter()
            .map(|name| ((*name).to_owned(), json!({ "type": "string" })))
            .collect();
        json!({ "requestBody": { "content": { "application/json": { "schema": {
            "type": "object", "properties": properties, "required": ["id"]
        } } } } })
    }

    #[test]
    fn a_change_patches_every_operation_using_its_schema_and_no_other() {
        let mut document = json!({ "paths": {
            "/v1/link": { "post": body(&["id", "name"]) },
            "/v1.1/link": { "post": body(&["id", "name"]) },
            "/v1/other": { "post": body(&["id"]) },
        } });

        let patched = apply_changes(&mut document, &[change_adding("uuid", &["id", "name"])]);

        assert_eq!(patched, ["POST /v1.1/link", "POST /v1/link"]);
        let required = |path: &str| {
            document["paths"][path]["post"]["requestBody"]["content"]["application/json"]["schema"]
                ["required"]
                .clone()
        };
        assert_eq!(required("/v1/link"), json!(["id", "uuid"]));
        assert_eq!(required("/v1/other"), json!(["id"]));
    }

    #[test]
    fn a_changed_value_other_than_its_optionality_is_left_to_the_adapter() {
        let key = |text: &str, optional: bool| Key {
            optional: Some(optional),
            text: text.to_owned(),
            schema: None,
        };
        let base = vec![("id".to_owned(), key("z.string()", false))];
        let head = vec![("id".to_owned(), key("z.number()", false))];

        assert!(schema_change(&base, &head).is_none());
    }
}
