use std::io::Cursor;

use serde_json::{Value, json};

use super::*;

/// Frames messages as a client would send them.
fn input(messages: &[Value]) -> Cursor<Vec<u8>> {
    let mut bytes = Vec::new();
    for message in messages {
        write_message(&mut bytes, message).unwrap();
    }
    Cursor::new(bytes)
}

/// Every message the server wrote.
fn output(bytes: Vec<u8>) -> Vec<Value> {
    let mut reader = Cursor::new(bytes);
    let mut messages = Vec::new();
    while let Some(message) = read_message(&mut reader).unwrap() {
        messages.push(message);
    }
    messages
}

fn request(id: i64, method: &str, params: &Value) -> Value {
    json!({ "jsonrpc": "2.0", "id": id, "method": method, "params": params })
}

fn notification(method: &str, params: &Value) -> Value {
    json!({ "jsonrpc": "2.0", "method": method, "params": params })
}

fn response(messages: &[Value], id: i64) -> Value {
    messages
        .iter()
        .find(|message| message.get("id") == Some(&json!(id)))
        .and_then(|message| message.get("result").cloned())
        .unwrap_or(Value::Null)
}

const MODULE: &str = "module example\nrelation Owns(team: Text, service: Text)\nrule owned(team: Text) {\n    (team) :- Owns(team, _)\n}\nrequire broken {\n    when: missing(x)\n}\n";

/// Opens `lemma.lm` holding `MODULE` and sends `requests` after it.
fn session(requests: &[Value]) -> (Vec<Value>, Url) {
    let directory = tempfile::tempdir().unwrap();
    let path = directory.path().join("lemma.lm");
    std::fs::write(&path, MODULE).unwrap();
    let uri = Url::from_file_path(&path).unwrap();
    let mut messages = vec![
        request(
            1,
            "initialize",
            &json!({ "capabilities": { "general": { "positionEncodings": ["utf-8"] } } }),
        ),
        notification("initialized", &json!({})),
        notification(
            "textDocument/didOpen",
            &json!({
                "textDocument": { "uri": uri, "languageId": "lemma", "version": 1, "text": MODULE }
            }),
        ),
    ];
    messages.extend(requests.iter().map(|message| {
        let mut message = message.clone();
        if let Some(document) = message.pointer_mut("/params/textDocument/uri") {
            *document = json!(uri);
        }
        message
    }));
    messages.push(request(99, "shutdown", &Value::Null));
    messages.push(notification("exit", &Value::Null));
    let mut written = Vec::new();
    serve(input(&messages), &mut written, directory.path()).unwrap();
    (output(written), uri)
}

fn at(line: u32, character: u32) -> Value {
    json!({ "textDocument": { "uri": "" }, "position": { "line": line, "character": character } })
}

#[test]
fn opening_a_document_publishes_its_problems() {
    let (messages, uri) = session(&[]);

    let published = messages
        .iter()
        .find(|message| {
            message["method"] == "textDocument/publishDiagnostics"
                && message["params"]["uri"] == json!(uri)
        })
        .unwrap();
    let diagnostics = published["params"]["diagnostics"].as_array().unwrap();
    assert_eq!(diagnostics.len(), 1, "{diagnostics:#?}");
    assert!(
        diagnostics[0]["message"]
            .as_str()
            .unwrap()
            .contains("missing")
    );
    assert_eq!(diagnostics[0]["range"]["start"]["line"], 6);
}

#[test]
fn the_server_negotiates_byte_positions() {
    let (messages, _) = session(&[]);

    assert_eq!(
        response(&messages, 1)["capabilities"]["positionEncoding"],
        "utf-8"
    );
}

#[test]
fn hovering_a_relation_shows_its_columns() {
    let (messages, _) = session(&[request(2, "textDocument/hover", &at(3, 15))]);

    let hover = response(&messages, 2);
    let text = hover["contents"]["value"].as_str().unwrap();
    assert!(
        text.contains("relation Owns: (team: Text, service: Text)"),
        "{text}"
    );
}

#[test]
fn a_use_goes_to_its_declaration() {
    let (messages, uri) = session(&[request(3, "textDocument/definition", &at(3, 15))]);

    let location = response(&messages, 3);
    assert_eq!(location["uri"], json!(uri));
    assert_eq!(location["range"]["start"]["line"], 1);
}

#[test]
fn symbols_list_the_declarations() {
    let (messages, _) = session(&[request(
        4,
        "textDocument/documentSymbol",
        &json!({ "textDocument": { "uri": "" } }),
    )]);

    let symbols = response(&messages, 4);
    let names: Vec<&str> = symbols
        .as_array()
        .unwrap()
        .iter()
        .map(|symbol| symbol["name"].as_str().unwrap())
        .collect();
    assert_eq!(names, ["Owns", "owned", "broken"]);
}

#[test]
fn completion_offers_the_modules_names_and_keywords() {
    let (messages, _) = session(&[request(5, "textDocument/completion", &at(3, 8))]);

    let labels: Vec<String> = response(&messages, 5)
        .as_array()
        .unwrap()
        .iter()
        .map(|item| item["label"].as_str().unwrap().to_owned())
        .collect();
    assert!(labels.contains(&"Owns".to_owned()));
    assert!(labels.contains(&"require".to_owned()));
}

#[test]
fn an_unknown_request_is_answered_with_an_error() {
    let (messages, _) = session(&[request(6, "textDocument/rename", &json!({}))]);

    let answer = messages.iter().find(|message| message["id"] == 6).unwrap();
    assert_eq!(answer["error"]["code"], METHOD_NOT_FOUND);
}

#[test]
fn a_document_below_the_root_is_its_repositorys() {
    let directory = tempfile::tempdir().unwrap();
    std::fs::write(directory.path().join("lemma.lm"), "module example\n").unwrap();
    std::fs::create_dir(directory.path().join("checks")).unwrap();
    let nested = directory.path().join("checks").join("replay.lm");

    let (root, entry) = entry_of(&nested);

    assert_eq!(root, directory.path());
    assert_eq!(entry, "checks/replay.lm");
}

fn change(text: &str) -> Value {
    notification(
        "textDocument/didChange",
        &json!({ "textDocument": { "uri": "", "version": 2 }, "contentChanges": [{ "text": text }] }),
    )
}

fn formatting(id: i64) -> Value {
    request(
        id,
        "textDocument/formatting",
        &json!({ "textDocument": { "uri": "" }, "options": { "tabSize": 4, "insertSpaces": true } }),
    )
}

#[test]
fn a_request_after_a_change_is_answered_from_the_changed_text() {
    let changed = format!("{MODULE}relation Added(name: Text)\n");

    let (messages, _) = session(&[
        change(&changed),
        request(
            4,
            "textDocument/documentSymbol",
            &json!({ "textDocument": { "uri": "" } }),
        ),
    ]);

    let symbols = response(&messages, 4);
    let names: Vec<&str> = symbols
        .as_array()
        .unwrap()
        .iter()
        .map(|symbol| symbol["name"].as_str().unwrap())
        .collect();
    assert_eq!(names, ["Owns", "owned", "broken", "Added"]);
}

#[test]
fn the_server_advertises_formatting() {
    let (messages, _) = session(&[]);

    assert_eq!(
        response(&messages, 1)["capabilities"]["documentFormattingProvider"],
        true
    );
}

#[test]
fn formatting_replaces_a_document_with_its_canonical_layout() {
    let (messages, _) = session(&[formatting(7)]);

    let edits = response(&messages, 7);
    assert_eq!(
        edits[0]["newText"],
        "module example\n\nrelation Owns(team: Text, service: Text)\n\nrule owned(team: Text) {\n    (team) :- Owns(team, _)\n}\n\nrequire broken {\n    when: missing(x)\n}\n"
    );
    assert_eq!(
        edits[0]["range"]["end"],
        json!({ "line": 8, "character": 0 })
    );
}

#[test]
fn formatting_a_document_with_syntax_errors_changes_nothing() {
    let (messages, _) = session(&[change("module example\nlet a =\n"), formatting(8)]);

    let answer = messages.iter().find(|message| message["id"] == 8).unwrap();
    assert_eq!(answer["result"], Value::Null);
}

mod burst;
