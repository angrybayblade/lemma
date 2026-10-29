//! Sorting incoming JSON-RPC messages and answering requests the server sends
//! to the client.

use serde_json::{Value, json};

/// JSON-RPC error code for a method the receiver does not implement.
const METHOD_NOT_FOUND: i64 = -32601;

/// A message received from the server.
#[derive(Debug, Clone, PartialEq)]
pub(crate) enum Incoming {
    /// An answer to one of our requests.
    Response {
        id: i64,
        outcome: Result<Value, ResponseError>,
    },
    /// A request the server expects us to answer.
    Request {
        id: Value,
        method: String,
        params: Value,
    },
    /// A notification that needs no answer.
    Notification { method: String, params: Value },
}

/// An error carried in a JSON-RPC response.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct ResponseError {
    pub code: i64,
    pub message: String,
}

/// What the client knows when answering server requests.
#[derive(Debug, Clone)]
pub(crate) struct ServerRequestContext {
    /// Settings returned for `workspace/configuration`, keyed by section.
    pub settings: Value,
    /// The folders reported for `workspace/workspaceFolders`.
    pub workspace_folders: Value,
}

/// Works out what kind of message this is. Returns `None` for messages that
/// are not valid JSON-RPC or answer requests we never sent.
pub(crate) fn classify(message: Value) -> Option<Incoming> {
    let Value::Object(mut fields) = message else {
        return None;
    };
    let method = fields
        .remove("method")
        .and_then(|method| method.as_str().map(str::to_owned));
    let id = fields.remove("id");

    match (method, id) {
        (Some(method), Some(id)) => {
            let params = fields.remove("params").unwrap_or(Value::Null);
            Some(Incoming::Request { id, method, params })
        }
        (Some(method), None) => {
            let params = fields.remove("params").unwrap_or(Value::Null);
            Some(Incoming::Notification { method, params })
        }
        (None, Some(id)) => {
            // We only send numeric ids, so anything else is not ours.
            let id = id.as_i64()?;
            Some(Incoming::Response {
                id,
                outcome: response_outcome(fields),
            })
        }
        (None, None) => None,
    }
}

/// Answers a request from the server. Unknown methods get a standard
/// "method not found" error so the server can carry on.
pub(crate) fn answer_server_request(
    method: &str,
    params: &Value,
    context: &ServerRequestContext,
) -> Result<Value, ResponseError> {
    match method {
        "workspace/configuration" => Ok(configuration_items(params, &context.settings)),
        "workspace/workspaceFolders" => Ok(context.workspace_folders.clone()),
        "client/registerCapability"
        | "client/unregisterCapability"
        | "window/workDoneProgress/create"
        | "window/showMessageRequest"
        | "workspace/semanticTokens/refresh"
        | "workspace/inlayHint/refresh"
        | "workspace/codeLens/refresh"
        | "workspace/diagnostic/refresh" => Ok(Value::Null),
        _ => Err(ResponseError {
            code: METHOD_NOT_FOUND,
            message: format!("lemma does not handle {method}"),
        }),
    }
}

/// Builds the response envelope for a server request.
pub(crate) fn response(id: &Value, answer: Result<Value, ResponseError>) -> Value {
    match answer {
        Ok(result) => json!({ "jsonrpc": "2.0", "id": id, "result": result }),
        Err(error) => json!({
            "jsonrpc": "2.0",
            "id": id,
            "error": { "code": error.code, "message": error.message },
        }),
    }
}

fn response_outcome(mut fields: serde_json::Map<String, Value>) -> Result<Value, ResponseError> {
    let Some(error) = fields.remove("error") else {
        return Ok(fields.remove("result").unwrap_or(Value::Null));
    };
    Err(ResponseError {
        code: error
            .get("code")
            .and_then(Value::as_i64)
            .unwrap_or_default(),
        message: error
            .get("message")
            .and_then(Value::as_str)
            .unwrap_or("no message")
            .to_owned(),
    })
}

/// One settings value per requested item, in request order.
fn configuration_items(params: &Value, settings: &Value) -> Value {
    let items = params.get("items").and_then(Value::as_array);
    let answers = items
        .into_iter()
        .flatten()
        .map(|item| settings_section(settings, item.get("section").and_then(Value::as_str)))
        .collect();
    Value::Array(answers)
}

/// Looks up a dotted section such as `python.analysis`. A missing section is
/// `null`, which servers treat as "use your defaults".
fn settings_section(settings: &Value, section: Option<&str>) -> Value {
    let Some(section) = section.filter(|section| !section.is_empty()) else {
        return settings.clone();
    };
    section
        .split('.')
        .try_fold(settings, |value, key| value.get(key))
        .cloned()
        .unwrap_or(Value::Null)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn context() -> ServerRequestContext {
        ServerRequestContext {
            settings: json!({ "python": { "analysis": { "diagnosticMode": "openFilesOnly" } } }),
            workspace_folders: json!([{ "uri": "file:///repo", "name": "repo" }]),
        }
    }

    #[test]
    fn responses_carry_their_result() {
        let incoming = classify(json!({ "jsonrpc": "2.0", "id": 3, "result": [1] }));
        assert_eq!(
            incoming,
            Some(Incoming::Response {
                id: 3,
                outcome: Ok(json!([1]))
            })
        );
    }

    #[test]
    fn error_responses_carry_code_and_message() {
        let incoming = classify(json!({ "id": 3, "error": { "code": -32603, "message": "boom" } }));
        let expected = ResponseError {
            code: -32603,
            message: "boom".to_owned(),
        };
        assert_eq!(
            incoming,
            Some(Incoming::Response {
                id: 3,
                outcome: Err(expected)
            })
        );
    }

    #[test]
    fn messages_with_method_and_id_are_server_requests() {
        let incoming = classify(json!({ "id": "a", "method": "workspace/configuration" }));
        assert!(
            matches!(incoming, Some(Incoming::Request { method, .. }) if method == "workspace/configuration")
        );
    }

    #[test]
    fn responses_with_foreign_ids_are_ignored() {
        assert_eq!(classify(json!({ "id": "not-ours", "result": null })), None);
    }

    #[test]
    fn configuration_returns_each_requested_section() {
        let params = json!({ "items": [
            { "section": "python.analysis" },
            { "section": "python.missing" },
            { "scopeUri": "file:///repo" },
        ]});

        let answer = answer_server_request("workspace/configuration", &params, &context()).unwrap();

        assert_eq!(
            answer,
            json!([
                { "diagnosticMode": "openFilesOnly" },
                null,
                { "python": { "analysis": { "diagnosticMode": "openFilesOnly" } } },
            ])
        );
    }

    #[test]
    fn unknown_server_requests_get_method_not_found() {
        let answer = answer_server_request("custom/thing", &Value::Null, &context());
        assert_eq!(answer.unwrap_err().code, METHOD_NOT_FOUND);
    }
}
