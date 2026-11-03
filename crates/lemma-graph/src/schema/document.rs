//! Schemas inside JSON documents: finding one by JSON path, and inlining
//! the `$ref`s it uses so it can be compared on its own.

use serde_json::{Map, Value};

/// How deep to inline nested references; recursive schemas stop here.
const MAX_DEPTH: usize = 16;

/// The value at a JSON path: `$.a.b`, `$.items[0].name`, or `$` itself.
/// Keys with dots or brackets can be quoted: `$["a.b"]`.
pub fn at_path<'a>(document: &'a Value, path: &str) -> Option<&'a Value> {
    let mut current = document;
    for step in steps(path.strip_prefix('$').unwrap_or(path))? {
        current = match step {
            Step::Key(key) => current.get(key.as_str())?,
            Step::Index(index) => current.get(index)?,
        };
    }
    Some(current)
}

enum Step {
    Key(String),
    Index(usize),
}

fn steps(path: &str) -> Option<Vec<Step>> {
    let mut steps = Vec::new();
    let mut rest = path;
    while !rest.is_empty() {
        if let Some(after) = rest.strip_prefix('.') {
            let end = after.find(['.', '[']).unwrap_or(after.len());
            steps.push(Step::Key(after[..end].to_owned()));
            rest = &after[end..];
            continue;
        }
        let after = rest.strip_prefix('[')?;
        let end = after.find(']')?;
        let inside = after[..end].trim();
        let step = match inside.parse::<usize>() {
            Ok(index) => Step::Index(index),
            Err(_) => Step::Key(inside.trim_matches(['"', '\'']).to_owned()),
        };
        steps.push(step);
        rest = &after[end + 1..];
    }
    Some(steps)
}

/// `schema` with every local `$ref` (`#/...`, resolved in `document`)
/// replaced by what it refers to, and definitions sections dropped. A
/// reference that cannot be found, or that recurses, becomes `{}` (any).
pub fn inline_references(schema: &Value, document: &Value) -> Value {
    let mut inlined = inline(schema, document, &mut Vec::new());
    if let Value::Object(object) = &mut inlined {
        for key in ["$defs", "definitions", "$schema", "$id"] {
            object.remove(key);
        }
    }
    inlined
}

fn inline(value: &Value, document: &Value, active: &mut Vec<String>) -> Value {
    match value {
        Value::Object(object) => inline_object(object, document, active),
        Value::Array(items) => Value::Array(
            items
                .iter()
                .map(|item| inline(item, document, active))
                .collect(),
        ),
        other => other.clone(),
    }
}

fn inline_object(object: &Map<String, Value>, document: &Value, active: &mut Vec<String>) -> Value {
    let Some(reference) = object.get("$ref").and_then(Value::as_str) else {
        let inlined = object
            .iter()
            .filter(|(key, _)| !matches!(key.as_str(), "$defs" | "definitions"))
            .map(|(key, value)| (key.clone(), inline(value, document, active)))
            .collect();
        return Value::Object(inlined);
    };
    let target = reference
        .strip_prefix('#')
        .and_then(|pointer| document.pointer(pointer));
    let recursing = active.iter().any(|seen| seen == reference) || active.len() >= MAX_DEPTH;
    let Some(target) = target.filter(|_| !recursing) else {
        return Value::Object(Map::new());
    };
    active.push(reference.to_owned());
    let mut resolved = inline(target, document, active);
    active.pop();
    // Keywords beside `$ref` (a description, `nullable`) still apply.
    if let Value::Object(resolved_object) = &mut resolved {
        for (key, value) in object.iter().filter(|(key, _)| key.as_str() != "$ref") {
            resolved_object.insert(key.clone(), inline(value, document, active));
        }
    }
    resolved
}

/// An `OpenAPI` 3.0 schema as JSON Schema: `nullable: true`, which JSON
/// Schema does not know, becomes `null` among the allowed types. `OpenAPI`
/// 3.1 schemas are JSON Schema already and pass through unchanged.
pub fn from_openapi(schema: &Value) -> Value {
    match schema {
        Value::Object(object) => {
            let mut converted: Map<String, Value> = object
                .iter()
                .filter(|(key, _)| key.as_str() != "nullable")
                .map(|(key, value)| (key.clone(), from_openapi(value)))
                .collect();
            if object.get("nullable") == Some(&Value::Bool(true)) {
                allow_null(&mut converted);
            }
            Value::Object(converted)
        }
        Value::Array(items) => Value::Array(items.iter().map(from_openapi).collect()),
        other => other.clone(),
    }
}

fn allow_null(schema: &mut Map<String, Value>) {
    match schema.get("type").cloned() {
        Some(Value::String(kind)) => {
            schema.insert(
                "type".to_owned(),
                Value::from(vec![kind, "null".to_owned()]),
            );
        }
        Some(Value::Array(mut kinds)) => {
            if !kinds.contains(&Value::from("null")) {
                kinds.push(Value::from("null"));
            }
            schema.insert("type".to_owned(), Value::Array(kinds));
        }
        _ => {
            let inner = Value::Object(std::mem::take(schema));
            let null = serde_json::json!({ "type": "null" });
            schema.insert("anyOf".to_owned(), Value::Array(vec![inner, null]));
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn json_paths_reach_keys_and_indices() {
        let document = json!({ "create_charge": { "request": { "type": "object" } }, "list": [1, { "a.b": 2 }] });

        assert_eq!(
            at_path(&document, "$.create_charge.request"),
            Some(&json!({ "type": "object" }))
        );
        assert_eq!(at_path(&document, "$.list[1][\"a.b\"]"), Some(&json!(2)));
        assert_eq!(at_path(&document, "$.missing"), None);
        assert_eq!(at_path(&document, "$"), Some(&document));
    }

    #[test]
    fn openapi_nullable_becomes_a_null_type() {
        let schema = json!({ "type": "object", "properties": {
            "a": { "type": "string", "nullable": true },
            "b": { "properties": { "c": {} }, "nullable": true },
            "d": { "type": "integer", "nullable": false }
        } });

        let converted = from_openapi(&schema);

        assert_eq!(
            converted["properties"]["a"],
            json!({ "type": ["string", "null"] })
        );
        assert_eq!(
            converted["properties"]["b"],
            json!({ "anyOf": [{ "properties": { "c": {} } }, { "type": "null" }] })
        );
        assert_eq!(converted["properties"]["d"], json!({ "type": "integer" }));
    }

    #[test]
    fn references_are_inlined_and_recursion_stops() {
        let document = json!({
            "$defs": {
                "Item": { "type": "object", "properties": { "id": { "type": "string" } } },
                "Node": { "type": "object", "properties": { "next": { "$ref": "#/$defs/Node" } } }
            },
            "request": {
                "type": "object",
                "properties": {
                    "item": { "$ref": "#/$defs/Item", "description": "one item" },
                    "node": { "$ref": "#/$defs/Node" }
                }
            }
        });

        let inlined = inline_references(&document["request"], &document);

        assert_eq!(
            inlined["properties"]["item"]["properties"]["id"]["type"],
            "string"
        );
        assert_eq!(inlined["properties"]["item"]["description"], "one item");
        assert_eq!(
            inlined["properties"]["node"]["properties"]["next"],
            json!({})
        );
    }
}
