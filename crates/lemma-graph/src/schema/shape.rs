//! Building blocks the type readers share: JSON Schema for objects, and
//! allowing `null`.

use serde_json::{Map, Value, json};

/// Which side of a message a type is on. Serializers encode and decode by
/// different rules: what is always sent is not what must be received.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum View {
    /// The type is sent: the schema says what can appear on the wire.
    Encode,
    /// The type receives: the schema says what is accepted.
    Decode,
}

/// `schema`, also allowing `null`.
pub fn nullable(schema: Value) -> Value {
    match schema.get("type") {
        Some(Value::String(kind)) => {
            let mut nullable = schema.clone();
            nullable["type"] = json!([kind, "null"]);
            nullable
        }
        Some(Value::Array(kinds)) if kinds.contains(&json!("null")) => schema,
        Some(Value::Array(kinds)) => {
            let mut kinds = kinds.clone();
            kinds.push(json!("null"));
            let mut nullable = schema.clone();
            nullable["type"] = Value::Array(kinds);
            nullable
        }
        Some(_) => schema,
        None if schema == json!({}) => schema,
        None => json!({ "anyOf": [schema, { "type": "null" }] }),
    }
}

/// An object with `properties`, listing `required` ones when there are any.
pub fn object(properties: Map<String, Value>, required: Vec<String>) -> Value {
    let mut schema = Map::new();
    schema.insert("type".to_owned(), Value::from("object"));
    schema.insert("properties".to_owned(), Value::Object(properties));
    if !required.is_empty() {
        schema.insert("required".to_owned(), Value::from(required));
    }
    Value::Object(schema)
}

/// An object's fields as a reader collects them.
#[derive(Debug, Default)]
pub struct Fields {
    /// Each field's schema, by wire name.
    pub properties: Map<String, Value>,
    /// The wire names that must be present, in order.
    pub required: Vec<String>,
}

impl Fields {
    /// Adds a field; a later one with the same name replaces it.
    pub fn insert(&mut self, name: String, schema: Value, is_required: bool) {
        self.required.retain(|existing| existing != &name);
        if is_required {
            self.required.push(name.clone());
        }
        self.properties.insert(name, schema);
    }

    /// Adds another object schema's properties and required names, as an
    /// inherited, embedded or flattened type contributes them.
    pub fn extend_from(&mut self, schema: &Value) {
        if let Some(fields) = schema.get("properties").and_then(Value::as_object) {
            self.properties.extend(fields.clone());
        }
        if let Some(names) = schema.get("required").and_then(Value::as_array) {
            self.required
                .extend(names.iter().filter_map(Value::as_str).map(str::to_owned));
        }
    }

    /// The object schema.
    pub fn into_object(self) -> Value {
        object(self.properties, self.required)
    }
}

/// Any of `schemas`: the one schema if there is one, `{}` (anything) if
/// any of them is.
pub fn any_of(schemas: Vec<Value>) -> Value {
    if schemas.iter().any(|schema| schema == &json!({})) {
        return json!({});
    }
    match <[Value; 1]>::try_from(schemas) {
        Ok([only]) => only,
        Err(schemas) => json!({ "anyOf": schemas }),
    }
}

/// String constants as an enumeration.
pub fn string_enum(values: Vec<String>) -> Value {
    let mut schema = Map::new();
    schema.insert("type".to_owned(), Value::from("string"));
    schema.insert("enum".to_owned(), Value::from(values));
    Value::Object(schema)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn null_is_added_once_and_wraps_untyped_schemas() {
        assert_eq!(
            nullable(json!({ "type": "string" })),
            json!({ "type": ["string", "null"] })
        );
        assert_eq!(
            nullable(json!({ "type": ["string", "null"] })),
            json!({ "type": ["string", "null"] })
        );
        assert_eq!(
            nullable(json!({ "enum": ["a"] })),
            json!({ "anyOf": [{ "enum": ["a"] }, { "type": "null" }] })
        );
        assert_eq!(nullable(json!({})), json!({}));
    }
}
