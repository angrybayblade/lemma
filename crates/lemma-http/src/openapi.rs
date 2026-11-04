//! Operations from an `OpenAPI` document: their parameters and the fields of
//! their request and success-response bodies.
//!
//! Body schemas are flattened to dotted field paths (`hooks[].handler.action`)
//! after resolving `$ref`s, with the union of `allOf`, `oneOf` and `anyOf`
//! branches, so a renamed or removed field shows up as one missing path.

use serde_json::Value as Json;

use crate::paths::{canonical_method, canonical_route};

/// How deep to follow nested schemas; contracts deeper than this are rare
/// and recursive schemas would otherwise never end.
const MAX_DEPTH: usize = 8;

/// One operation of a service.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Operation {
    /// Upper-case HTTP method.
    pub method: String,
    /// Canonical path; see [`crate::canonical_path`].
    pub path: String,
    /// The path as written in the spec.
    pub template: String,
    /// Its `operationId`, which generated clients name their functions after.
    pub operation_id: Option<String>,
    /// Path, query and header parameters.
    pub parameters: Vec<Parameter>,
    /// Fields of the JSON request body.
    pub request_fields: Vec<Field>,
    /// Whether the request body's schema forbids fields it does not list
    /// (`additionalProperties: false`), so a request sending one fails.
    pub rejects_unknown_fields: bool,
    /// Fields of the JSON bodies of 2xx responses.
    pub response_fields: Vec<String>,
}

impl Operation {
    /// `GET /v1/users`: how findings name the operation.
    pub fn key(&self) -> String {
        format!("{} {}", self.method, self.template)
    }
}

/// A parameter of an operation.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Parameter {
    /// Its name.
    pub name: String,
    /// `path`, `query`, `header` or `cookie`.
    pub location: String,
    /// Whether callers must send it.
    pub required: bool,
}

/// A field of a body, by dotted path.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Field {
    /// Dotted path, with `[]` for array items.
    pub path: String,
    /// Whether a body that is sent must contain it: it and every object
    /// containing it are required.
    pub required: bool,
}

/// Whether a document's name says it is YAML (`.yaml` or `.yml`), not
/// JSON.
pub fn is_yaml_document(name: &str) -> bool {
    std::path::Path::new(name)
        .extension()
        .and_then(|extension| extension.to_str())
        .is_some_and(|extension| {
            extension.eq_ignore_ascii_case("yaml") || extension.eq_ignore_ascii_case("yml")
        })
}

/// Every operation in a document, sorted by method and path.
pub fn operations(document: &Json) -> Vec<Operation> {
    let mut found = Vec::new();
    let Some(paths) = document.get("paths").and_then(Json::as_object) else {
        return found;
    };
    for (template, item) in paths {
        let Some(canonical) = canonical_route(template) else {
            continue;
        };
        let Some(methods) = item.as_object() else {
            continue;
        };
        let shared = parameters(document, item.get("parameters"));
        for (method, operation) in methods {
            let method = canonical_method(method);
            if method == "*" {
                continue; // `parameters`, `summary` and other non-operation keys
            }
            let mut operation_parameters = shared.clone();
            operation_parameters.extend(parameters(document, operation.get("parameters")));
            found.push(Operation {
                method,
                path: canonical.clone(),
                template: template.clone(),
                operation_id: operation
                    .get("operationId")
                    .and_then(Json::as_str)
                    .map(str::to_owned),
                parameters: operation_parameters,
                request_fields: request_fields(document, operation),
                rejects_unknown_fields: rejects_unknown_fields(document, operation),
                response_fields: response_fields(document, operation),
            });
        }
    }
    found.sort_by(|left, right| (&left.method, &left.path).cmp(&(&right.method, &right.path)));
    found
}

fn parameters(document: &Json, list: Option<&Json>) -> Vec<Parameter> {
    list.and_then(Json::as_array)
        .into_iter()
        .flatten()
        .filter_map(|parameter| {
            let parameter = resolve(document, parameter);
            Some(Parameter {
                name: parameter.get("name")?.as_str()?.to_owned(),
                location: parameter.get("in")?.as_str()?.to_owned(),
                required: parameter
                    .get("required")
                    .and_then(Json::as_bool)
                    .unwrap_or(false),
            })
        })
        .collect()
}

fn request_fields(document: &Json, operation: &Json) -> Vec<Field> {
    let Some(body) = operation
        .get("requestBody")
        .map(|body| resolve(document, body))
    else {
        return Vec::new();
    };
    // A field the schema requires must be in any body sent, whether or not
    // the body itself is (`requestBody.required`): generators such as
    // zod-openapi leave that flag out even where the schema requires fields.
    let mut fields = Vec::new();
    if let Some(schema) = json_schema(body) {
        flatten(document, schema, &Place::root(true), &mut fields);
    }
    dedupe(fields)
}

/// Whether the request body's schema, or every branch of a union, sets
/// `additionalProperties: false`.
fn rejects_unknown_fields(document: &Json, operation: &Json) -> bool {
    let Some(body) = operation
        .get("requestBody")
        .map(|body| resolve(document, body))
    else {
        return false;
    };
    let Some(schema) = json_schema(body).map(|schema| resolve(document, schema)) else {
        return false;
    };
    let is_closed = |schema: &Json| schema.get("additionalProperties") == Some(&Json::Bool(false));
    let branches: Vec<&Json> = ["anyOf", "oneOf"]
        .iter()
        .filter_map(|key| schema.get(*key).and_then(Json::as_array))
        .flatten()
        .map(|branch| resolve(document, branch))
        .collect();
    if branches.is_empty() {
        return is_closed(schema);
    }
    branches.into_iter().all(is_closed)
}

fn response_fields(document: &Json, operation: &Json) -> Vec<String> {
    let mut fields = Vec::new();
    let responses = operation.get("responses").and_then(Json::as_object);
    for (status, response) in responses.into_iter().flatten() {
        if !status.starts_with('2') {
            continue;
        }
        if let Some(schema) = json_schema(resolve(document, response)) {
            flatten(document, schema, &Place::root(true), &mut fields);
        }
    }
    let mut paths: Vec<String> = dedupe(fields).into_iter().map(|field| field.path).collect();
    paths.dedup();
    paths
}

fn json_schema(holder: &Json) -> Option<&Json> {
    let content = holder.get("content")?.as_object()?;
    content
        .iter()
        .find(|(media, _)| media.contains("json"))
        .and_then(|(_, media)| media.get("schema"))
}

/// Where in a body schema flattening has reached.
struct Place {
    prefix: String,
    required: bool,
    depth: usize,
}

impl Place {
    fn root(required: bool) -> Self {
        Self {
            prefix: String::new(),
            required,
            depth: 0,
        }
    }

    fn child(&self, prefix: String, required: bool) -> Self {
        Self {
            prefix,
            required,
            depth: self.depth + 1,
        }
    }
}

/// Adds every field path under `schema` to `out`.
fn flatten(document: &Json, schema: &Json, place: &Place, out: &mut Vec<Field>) {
    if place.depth > MAX_DEPTH {
        return;
    }
    let schema = resolve(document, schema);
    flatten_branches(document, schema, place, out);
    if let Some(items) = schema.get("items") {
        flatten(
            document,
            items,
            &place.child(format!("{}[]", place.prefix), place.required),
            out,
        );
    }
    let Some(properties) = schema.get("properties").and_then(Json::as_object) else {
        return;
    };
    let required_names: Vec<&str> = schema
        .get("required")
        .and_then(Json::as_array)
        .into_iter()
        .flatten()
        .filter_map(Json::as_str)
        .collect();
    for (name, property) in properties {
        let path = if place.prefix.is_empty() {
            name.clone()
        } else {
            format!("{}.{name}", place.prefix)
        };
        let property_required = place.required && required_names.contains(&name.as_str());
        out.push(Field {
            path: path.clone(),
            required: property_required,
        });
        flatten(
            document,
            property,
            &place.child(path, property_required),
            out,
        );
    }
}

/// Follows a local `$ref` (`#/components/schemas/X`) one or more times.
fn resolve<'a>(document: &'a Json, value: &'a Json) -> &'a Json {
    let mut current = value;
    for _ in 0..MAX_DEPTH {
        let Some(reference) = current.get("$ref").and_then(Json::as_str) else {
            return current;
        };
        let Some(pointer) = reference.strip_prefix('#') else {
            return current;
        };
        match document.pointer(pointer) {
            Some(target) => current = target,
            None => return current,
        }
    }
    current
}

/// The fields of a schema's `allOf`, `oneOf` and `anyOf` branches.
fn flatten_branches(document: &Json, schema: &Json, place: &Place, out: &mut Vec<Field>) {
    // `allOf` branches all apply, so each keeps its requiredness.
    for branch in schema
        .get("allOf")
        .and_then(Json::as_array)
        .into_iter()
        .flatten()
    {
        flatten(
            document,
            branch,
            &place.child(place.prefix.clone(), place.required),
            out,
        );
    }
    // Only one `oneOf` or `anyOf` branch need apply, so a field is surely
    // required only if every branch requires it.
    for key in ["oneOf", "anyOf"] {
        let branches: Vec<Vec<Field>> = schema
            .get(key)
            .and_then(Json::as_array)
            .into_iter()
            .flatten()
            .map(|branch| {
                let mut fields = Vec::new();
                flatten(
                    document,
                    branch,
                    &place.child(place.prefix.clone(), place.required),
                    &mut fields,
                );
                dedupe(fields)
            })
            .collect();
        out.extend(required_in_every_branch(branches));
    }
}

/// Every field of any branch, required only where every branch requires it.
fn required_in_every_branch(branches: Vec<Vec<Field>>) -> Vec<Field> {
    let count = branches.len();
    let mut seen: std::collections::BTreeMap<String, usize> = std::collections::BTreeMap::new();
    for field in branches.iter().flatten().filter(|field| field.required) {
        *seen.entry(field.path.clone()).or_default() += 1;
    }
    branches
        .into_iter()
        .flatten()
        .map(|field| Field {
            required: seen.get(&field.path) == Some(&count),
            path: field.path,
        })
        .collect()
}

/// One field per path; required if any occurrence is.
fn dedupe(mut fields: Vec<Field>) -> Vec<Field> {
    fields.sort_by(|left, right| {
        left.path
            .cmp(&right.path)
            .then(right.required.cmp(&left.required))
    });
    fields.dedup_by(|later, earlier| later.path == earlier.path);
    fields
}

#[cfg(test)]
mod tests {
    use super::*;

    fn document() -> Json {
        serde_json::json!({
            "paths": {
                "/v1/users": {
                    "get": {
                        "parameters": [
                            { "name": "search", "in": "query", "required": false },
                            { "$ref": "#/components/parameters/Limit" }
                        ],
                        "responses": { "200": { "content": { "application/json": {
                            "schema": { "$ref": "#/components/schemas/Page" } } } } }
                    },
                    "post": {
                        "requestBody": { "required": true, "content": { "application/json": { "schema": {
                            "type": "object", "required": ["name"], "additionalProperties": false,
                            "properties": {
                                "name": { "type": "string" },
                                "hooks": { "type": "array", "items": { "type": "object", "required": ["action"],
                                    "properties": { "action": { "type": "string" } } } }
                            } } } } },
                        "responses": { "201": { "description": "created" } }
                    }
                }
            },
            "components": {
                "parameters": { "Limit": { "name": "limit", "in": "query" } },
                "schemas": { "Page": { "allOf": [
                    { "type": "object", "properties": { "items": { "type": "array",
                        "items": { "type": "object", "properties": { "id": { "type": "string" } } } } } },
                    { "type": "object", "properties": { "next_cursor": { "type": "string" } } }
                ] } }
            }
        })
    }

    #[test]
    fn parameters_are_read_through_references() {
        let found = operations(&document());

        let get = &found[0];
        assert_eq!(get.key(), "GET /v1/users");
        let names: Vec<&str> = get.parameters.iter().map(|p| p.name.as_str()).collect();
        assert_eq!(names, ["search", "limit"]);
    }

    #[test]
    fn response_bodies_flatten_to_field_paths() {
        let found = operations(&document());

        assert_eq!(
            found[0].response_fields,
            ["items", "items[].id", "next_cursor"]
        );
    }

    #[test]
    fn closed_request_schemas_reject_unknown_fields() {
        let found = operations(&document());

        let rejects: Vec<bool> = found
            .iter()
            .map(|operation| operation.rejects_unknown_fields)
            .collect();

        assert_eq!(rejects, [false, true]);
    }

    #[test]
    fn request_fields_know_what_is_required() {
        let found = operations(&document());

        let post = &found[1];
        let required: Vec<(&str, bool)> = post
            .request_fields
            .iter()
            .map(|f| (f.path.as_str(), f.required))
            .collect();
        assert_eq!(
            required,
            [("hooks", false), ("hooks[].action", false), ("name", true)]
        );
    }

    #[test]
    fn a_union_requires_only_what_every_branch_requires() {
        let document = serde_json::json!({ "paths": { "/api/x": { "patch": {
            "requestBody": { "content": { "application/json": { "schema": { "anyOf": [
                { "type": "object", "required": ["id", "a"],
                  "properties": { "id": {}, "a": {} } },
                { "type": "object", "required": ["id", "b"],
                  "properties": { "id": {}, "b": {} } }
            ] } } } }
        } } } });

        let found = operations(&document);

        let required: Vec<(&str, bool)> = found[0]
            .request_fields
            .iter()
            .map(|field| (field.path.as_str(), field.required))
            .collect();
        assert_eq!(required, [("a", false), ("b", false), ("id", true)]);
    }
}
