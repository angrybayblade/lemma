//! A service's `OpenAPI` documents as operations: each operation with an
//! `operationId` becomes one, named by it, whose request and response
//! schemas point into the document. A link can then name a documented
//! operation (`#billing:services.api.createCharge`) without declaring it.

use std::collections::BTreeMap;

use serde_json::Value;

use crate::config::{DocumentLocation, Operation, SchemaSource, Schemas};

const METHODS: [&str; 8] = [
    "get", "put", "post", "delete", "options", "head", "patch", "trace",
];

/// The operations `document` (the file `file`) declares, by `operationId`.
/// Operations without one are left out; they are still found in code.
pub fn operations(file: &str, document: &Value) -> BTreeMap<String, Operation> {
    let mut found = BTreeMap::new();
    let Some(paths) = document.get("paths").and_then(Value::as_object) else {
        return found;
    };
    for (path, item) in paths {
        for method in METHODS {
            let Some(operation) = item.get(method) else {
                continue;
            };
            let Some(id) = operation.get("operationId").and_then(Value::as_str) else {
                continue;
            };
            let at = format!("$.paths[\"{path}\"].{method}");
            found.insert(id.to_owned(), declared(file, operation, &at));
        }
    }
    found
}

/// The operation `document` declares for `method` and `path`, path
/// parameters compared whatever their names (`{id}` and `{}` match),
/// whether or not it has an `operationId`.
pub fn operation_at(file: &str, document: &Value, method: &str, path: &str) -> Option<Operation> {
    let method = method.to_lowercase();
    let wanted = without_parameter_names(path);
    let paths = document.get("paths")?.as_object()?;
    let (written, item) = paths
        .iter()
        .find(|(written, _)| without_parameter_names(written) == wanted)?;
    let operation = item.get(&method)?;
    let at = format!("$.paths[\"{written}\"].{method}");
    Some(declared(file, operation, &at))
}

/// `/users/{id}` as `/users/{}`.
fn without_parameter_names(path: &str) -> String {
    let mut written = String::with_capacity(path.len());
    let mut in_parameter = false;
    for character in path.chars() {
        match character {
            '{' => {
                in_parameter = true;
                written.push('{');
            }
            '}' => {
                in_parameter = false;
                written.push('}');
            }
            _ if in_parameter => {}
            _ => written.push(character),
        }
    }
    written
}

fn declared(file: &str, operation: &Value, at: &str) -> Operation {
    let location = |path: String| {
        SchemaSource::Document(DocumentLocation {
            format: "openapi".to_owned(),
            path,
            file: Some(file.to_owned()),
        })
    };
    let request = operation
        .pointer("/requestBody/content/application~1json/schema")
        .map(|_| format!("{at}.requestBody.content[\"application/json\"].schema"));
    let response = success_status(operation)
        .map(|status| format!("{at}.responses[\"{status}\"].content[\"application/json\"].schema"));
    Operation {
        previously: Vec::new(),
        path: file.to_owned(),
        schema: Some(Schemas {
            resolver: None,
            request: request.map(location),
            response: response.map(location),
        }),
    }
}

/// The lowest `2xx` response with a JSON body, or `default`.
fn success_status(operation: &Value) -> Option<String> {
    let responses = operation.get("responses")?.as_object()?;
    let has_json = |response: &Value| {
        response
            .pointer("/content/application~1json/schema")
            .is_some()
    };
    responses
        .iter()
        .filter(|(status, response)| status.starts_with('2') && has_json(response))
        .map(|(status, _)| status.clone())
        .min()
        .or_else(|| {
            responses
                .get("default")
                .filter(|response| has_json(response))
                .map(|_| "default".to_owned())
        })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::schema::at_path;
    use serde_json::json;

    #[test]
    fn documented_operations_point_at_their_schemas() {
        let document = json!({ "paths": { "/v1/charges/{id}": {
            "post": {
                "operationId": "updateCharge",
                "requestBody": { "content": { "application/json": { "schema": { "type": "object" } } } },
                "responses": {
                    "204": {},
                    "201": { "content": { "application/json": { "schema": { "type": "string" } } } }
                }
            },
            "get": { "responses": {} }
        } } });

        let found = operations("openapi.json", &document);

        assert_eq!(found.keys().collect::<Vec<_>>(), ["updateCharge"]);
        let schemas = found["updateCharge"].schema.clone().unwrap();
        let (Some(SchemaSource::Document(request)), Some(SchemaSource::Document(response))) =
            (schemas.request, schemas.response)
        else {
            panic!("expected document locations");
        };
        assert_eq!(
            at_path(&document, &request.path),
            Some(&json!({ "type": "object" }))
        );
        assert_eq!(
            at_path(&document, &response.path),
            Some(&json!({ "type": "string" }))
        );
    }
}
