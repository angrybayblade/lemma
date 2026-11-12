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
    declared_operations(file, &declarations(document))
}

/// What a document declares of one operation: enough to build it for the
/// file the document is read from, and small enough to keep between runs
/// by the document's contents instead of parsing megabytes again.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct Declaration {
    /// Its `operationId`.
    pub id: String,
    /// Where it is in the document: `$.paths["/x"].post`.
    pub at: String,
    /// Whether it has a JSON request body.
    pub has_request: bool,
    /// The status of its JSON response, if it has one (see
    /// [`success_status`]).
    pub response_status: Option<String>,
}

/// What `document` declares of each operation with an `operationId`, in
/// the document's order.
pub fn declarations(document: &Value) -> Vec<Declaration> {
    let mut found = Vec::new();
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
            found.push(declaration(id, at, operation));
        }
    }
    found
}

/// What `operation`, at `at` in its document, declares.
fn declaration(id: &str, at: String, operation: &Value) -> Declaration {
    Declaration {
        id: id.to_owned(),
        at,
        has_request: operation
            .pointer("/requestBody/content/application~1json/schema")
            .is_some(),
        response_status: success_status(operation),
    }
}

/// The operations of `declarations`, read from the file `file`, by
/// `operationId`; a later declaration of an id wins, as in the document.
pub fn declared_operations(
    file: &str,
    declarations: &[Declaration],
) -> BTreeMap<String, Operation> {
    declarations
        .iter()
        .map(|declaration| (declaration.id.clone(), declared(file, declaration)))
        .collect()
}

fn declared(file: &str, declaration: &Declaration) -> Operation {
    let location = |path: String| {
        SchemaSource::Document(DocumentLocation {
            format: "openapi".to_owned(),
            path,
            file: Some(file.to_owned()),
        })
    };
    let at = &declaration.at;
    let request = declaration
        .has_request
        .then(|| format!("{at}.requestBody.content[\"application/json\"].schema"));
    let response = declaration
        .response_status
        .as_ref()
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
    Some(declared(file, &declaration("", at, operation)))
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
    fn kept_declarations_give_the_operations_the_document_does() {
        let document = json!({ "paths": {
            "/v1/charges": {
                "post": {
                    "operationId": "createCharge",
                    "requestBody": { "content": { "application/json": { "schema": {} } } },
                    "responses": { "default": { "content": { "application/json": { "schema": {} } } } }
                },
                "get": { "operationId": "listCharges", "responses": {} }
            }
        } });
        let kept = serde_json::to_string(&declarations(&document)).unwrap();

        let read: Vec<Declaration> = serde_json::from_str(&kept).unwrap();

        assert_eq!(
            declared_operations("billing/openapi.json", &read),
            operations("billing/openapi.json", &document)
        );
    }

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
