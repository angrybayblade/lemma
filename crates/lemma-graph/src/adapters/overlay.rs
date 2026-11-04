//! Laying the operations an adapter read from code over a service's
//! committed documents, so a check sees the contract as the code has it
//! while links and generated clients keep the names the documents give.
//!
//! Operations are matched by method and canonical path, so a parameter
//! named differently (`{id}`, `{nanoId}`) or a trailing slash is the same
//! operation. A document whose paths no route of the code serves, but whose
//! paths under another document's prefix are, is an alias of that prefix
//! (`/v2/x` served by the `/v1/x` route) as [`lemma_http::implied_aliases`]
//! finds it; only then is an operation borrowed across prefixes.

use std::collections::{BTreeMap, BTreeSet};

use serde_json::{Map, Value};

/// The HTTP methods an `OpenAPI` path item holds operations under; its
/// other keys (`parameters`, `summary`) are not operations.
const METHODS: [&str; 8] = [
    "get", "put", "post", "delete", "options", "head", "patch", "trace",
];

/// What laying the code's operations over the committed documents found.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Coverage {
    /// Operations the code describes.
    pub described: usize,
    /// Operations the committed documents describe.
    pub committed: usize,
    /// Committed operations the code describes too.
    pub matched: usize,
    /// Committed operations the code no longer describes, as `METHOD path`,
    /// dropped from the contract because the adapter read the code whole.
    pub removed: Vec<String>,
    /// Operations only the code describes, as `METHOD path`, added.
    pub added: Vec<String>,
    /// Committed operations whose parameters, request body or responses
    /// the code describes differently, as `METHOD path`.
    pub changed: Vec<String>,
    /// Whether enough of the committed operations matched for the code's
    /// reading to be trusted with removals and additions. When too few
    /// match, the adapter most likely read paths differently (a base path
    /// it did not see), and only matched operations are replaced.
    pub is_trusted: bool,
}

/// The committed documents, by name, with the code's operations laid over
/// them, and what that found.
#[derive(Debug, Clone, PartialEq)]
pub struct Overlay {
    /// Each committed document as the code has it, by name; without
    /// committed documents, the generated one named `from_code_name`.
    pub documents: Vec<(String, Value)>,
    /// How the code and the documents compared.
    pub coverage: Coverage,
}

/// What the adapter reported about its reading.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Reading {
    /// Routes it could not describe, by method and canonical path: their
    /// committed operations stay as they are.
    pub unread_routes: BTreeSet<(String, String)>,
    /// Whether it read every route the code registers, so an operation it
    /// does not describe is gone.
    pub is_complete: bool,
    /// Whether it describes routes only, without their schemas, as an
    /// adapter for a framework that knows its routes but not their bodies
    /// does: the committed definitions are then kept, and the code says
    /// only which routes there are and where each is handled.
    pub is_routes_only: bool,
}

/// An operation in a document: its method key and path as written.
#[derive(Debug, Clone)]
struct Located<'a> {
    method: &'a str,
    template: &'a str,
    operation: &'a Value,
}

/// The committed documents with the code's operations laid over them (see
/// the module documentation). `from_code_name` names the generated document
/// when there are no committed ones.
pub fn overlay(
    committed: &[(String, Value)],
    generated: &Value,
    reading: &Reading,
    from_code_name: &str,
) -> Overlay {
    let code = operations_by_key(generated);
    if committed.is_empty() {
        return code_alone(generated, code.len(), from_code_name);
    }
    let per_document: Vec<BTreeMap<(String, String), Located<'_>>> = committed
        .iter()
        .map(|(_, document)| operations_by_key(document))
        .collect();
    let aliases = aliases(&per_document, &code);
    let matches: Vec<BTreeMap<(String, String), (String, String)>> = per_document
        .iter()
        .map(|operations| match_operations(operations, &code, &aliases))
        .collect();
    let committed_count: usize = per_document.iter().map(BTreeMap::len).sum();
    let matched_count: usize = matches.iter().map(BTreeMap::len).sum();
    let is_trusted = !code.is_empty() && matched_count * 2 >= committed_count;
    let mut coverage = Coverage {
        described: code.len(),
        committed: committed_count,
        matched: matched_count,
        is_trusted,
        ..Coverage::default()
    };
    let used: BTreeSet<&(String, String)> = matches.iter().flat_map(BTreeMap::values).collect();
    let homes = code_only_homes(committed, &code, &used, is_trusted);
    let mut taken_ids = operation_ids(&per_document);
    let mut documents = Vec::new();
    for (index, (name, document)) in committed.iter().enumerate() {
        let mut plan = DocumentPlan {
            committed: &per_document[index],
            matches: &matches[index],
            added: homes.get(&index).map(Vec::as_slice).unwrap_or_default(),
            may_remove: is_trusted && reading.is_complete,
            is_routes_only: reading.is_routes_only,
            unread_routes: &reading.unread_routes,
            taken_ids: &mut taken_ids,
        };
        let laid = lay_over(document, generated, &code, &mut plan, &mut coverage);
        documents.push((name.clone(), laid));
    }
    if let Some(keys) = homes.get(&committed.len()) {
        let own = own_document(generated, &code, keys, &mut taken_ids, &mut coverage);
        documents.push((from_code_name.to_owned(), own));
    }
    coverage.removed.sort();
    coverage.added.sort();
    coverage.changed.sort();
    Overlay {
        documents,
        coverage,
    }
}

/// Without committed documents, the generated one is the contract.
fn code_alone(generated: &Value, described: usize, from_code_name: &str) -> Overlay {
    let coverage = Coverage {
        described,
        is_trusted: true,
        ..Coverage::default()
    };
    Overlay {
        documents: vec![(from_code_name.to_owned(), generated.clone())],
        coverage,
    }
}

/// The `operationId`s the committed documents give.
fn operation_ids(per_document: &[BTreeMap<(String, String), Located<'_>>]) -> BTreeSet<String> {
    per_document
        .iter()
        .flat_map(BTreeMap::values)
        .filter_map(|located| located.operation.get("operationId")?.as_str())
        .map(str::to_owned)
        .collect()
}

/// A document's operations by upper-case method and canonical path.
fn operations_by_key(document: &Value) -> BTreeMap<(String, String), Located<'_>> {
    let mut found = BTreeMap::new();
    let Some(paths) = document.get("paths").and_then(Value::as_object) else {
        return found;
    };
    for (template, item) in paths {
        let Some(canonical) = lemma_http::canonical_route(template) else {
            continue;
        };
        let Some(item) = item.as_object() else {
            continue;
        };
        for (method, operation) in item {
            if !METHODS.contains(&method.as_str()) {
                continue;
            }
            let key = (method.to_uppercase(), canonical.clone());
            found.entry(key).or_insert(Located {
                method,
                template,
                operation,
            });
        }
    }
    found
}

/// The prefixes the code's routes are also served under, from the
/// committed documents and the operations the code describes.
fn aliases(
    per_document: &[BTreeMap<(String, String), Located<'_>>],
    code: &BTreeMap<(String, String), Located<'_>>,
) -> BTreeMap<String, String> {
    let documents: Vec<Vec<(String, String)>> = per_document
        .iter()
        .map(|operations| operations.keys().cloned().collect())
        .collect();
    let routes: BTreeSet<(String, String)> = code.keys().cloned().collect();
    lemma_http::implied_aliases(&documents, &routes)
}

/// Each committed operation the code describes, mapped to the code's key
/// for it: the same key, or the key under the prefix an alias serves it
/// from.
fn match_operations(
    committed: &BTreeMap<(String, String), Located<'_>>,
    code: &BTreeMap<(String, String), Located<'_>>,
    aliases: &BTreeMap<String, String>,
) -> BTreeMap<(String, String), (String, String)> {
    let mut matched = BTreeMap::new();
    for key in committed.keys() {
        if code.contains_key(key) {
            matched.insert(key.clone(), key.clone());
            continue;
        }
        let (method, path) = key;
        let through_alias = aliases.iter().find_map(|(written, served)| {
            let rest = path.strip_prefix(served.as_str())?;
            let candidate = (method.clone(), format!("{written}{rest}"));
            code.contains_key(&candidate).then_some(candidate)
        });
        if let Some(candidate) = through_alias {
            matched.insert(key.clone(), candidate);
        }
    }
    matched
}

/// Which committed document each operation only the code describes goes
/// into, by document index: the one whose shared prefix is the longest the
/// path starts with. One that fits no document's prefix gets the index
/// past the last, a document of its own, so it cannot change what the
/// committed documents' paths share. None when the reading is not trusted.
fn code_only_homes<'c>(
    committed: &[(String, Value)],
    code: &'c BTreeMap<(String, String), Located<'c>>,
    used: &BTreeSet<&(String, String)>,
    is_trusted: bool,
) -> BTreeMap<usize, Vec<&'c (String, String)>> {
    let mut homes: BTreeMap<usize, Vec<&(String, String)>> = BTreeMap::new();
    if !is_trusted {
        return homes;
    }
    let prefixes: Vec<String> = committed
        .iter()
        .map(|(_, document)| shared_prefix(document))
        .collect();
    for key in code.keys().filter(|key| !used.contains(key)) {
        let home = prefixes
            .iter()
            .enumerate()
            .filter(|(_, prefix)| is_under(&key.1, prefix))
            .max_by_key(|(_, prefix)| prefix.len())
            .map_or(committed.len(), |(index, _)| index);
        homes.entry(home).or_default().push(key);
    }
    homes
}

/// Whether `path` is `prefix` or below it, segment by segment: `/api/v3.1`
/// is not under `/api/v3`.
fn is_under(path: &str, prefix: &str) -> bool {
    !prefix.is_empty()
        && path
            .strip_prefix(prefix)
            .is_some_and(|rest| rest.is_empty() || rest.starts_with('/'))
}

/// What to do to one committed document.
struct DocumentPlan<'p> {
    committed: &'p BTreeMap<(String, String), Located<'p>>,
    matches: &'p BTreeMap<(String, String), (String, String)>,
    added: &'p [&'p (String, String)],
    may_remove: bool,
    is_routes_only: bool,
    unread_routes: &'p BTreeSet<(String, String)>,
    /// The names operations already have: an added operation whose code
    /// reuses one (a definition registered at a second path) goes unnamed
    /// rather than be confused with it.
    taken_ids: &'p mut BTreeSet<String>,
}

/// One committed document with its matched operations replaced, removed
/// ones dropped, the code's own added, and the components they refer to.
fn lay_over(
    document: &Value,
    generated: &Value,
    code: &BTreeMap<(String, String), Located<'_>>,
    plan: &mut DocumentPlan<'_>,
    coverage: &mut Coverage,
) -> Value {
    let renames = component_renames(document, generated);
    let mut paths = document
        .get("paths")
        .and_then(Value::as_object)
        .cloned()
        .unwrap_or_default();
    for (key, located) in plan.committed {
        let Some(code_key) = plan.matches.get(key) else {
            let is_unread = plan.unread_routes.contains(key);
            if plan.may_remove && !is_unread {
                remove_operation(&mut paths, located);
                coverage
                    .removed
                    .push(format!("{} {}", key.0, located.template));
            }
            continue;
        };
        let from_code = &code[code_key];
        if plan.is_routes_only {
            let mut operation = located.operation.clone();
            copy_handler(from_code.operation, &mut operation);
            set_operation(&mut paths, located.template, located.method, operation);
            continue;
        }
        let is_same_path = key == code_key;
        let mut operation = named_like(from_code, located, is_same_path);
        rename_path_parameters(&mut operation, from_code.template, located.template);
        if contract_parts(generated, &operation) != contract_parts(document, located.operation) {
            coverage
                .changed
                .push(format!("{} {}", key.0, located.template));
        }
        rewrite_references(&mut operation, &renames);
        set_operation(&mut paths, located.template, located.method, operation);
    }
    for key in plan.added {
        let from_code = &code[*key];
        let mut operation = from_code.operation.clone();
        let id = operation
            .get("operationId")
            .and_then(Value::as_str)
            .map(str::to_owned);
        if let (Some(id), Some(object)) = (id, operation.as_object_mut()) {
            if !plan.taken_ids.insert(id) {
                object.remove("operationId");
            }
        }
        rewrite_references(&mut operation, &renames);
        set_operation(&mut paths, from_code.template, from_code.method, operation);
        coverage
            .added
            .push(format!("{} {}", key.0, from_code.template));
    }
    paths.retain(|_, item| item.as_object().is_none_or(has_operation));
    let mut result = document.clone();
    if let Some(object) = result.as_object_mut() {
        object.insert("paths".to_owned(), Value::Object(paths));
        merge_components(object, generated, &renames);
    }
    result
}

/// What a caller depends on in an operation, its parameters, request body
/// and responses, with the document's references resolved: two documents
/// may name and arrange their components differently, and frameworks add
/// keys of their own to an operation that are not part of its contract.
fn contract_parts(document: &Value, operation: &Value) -> Value {
    let parts = ["parameters", "requestBody", "responses"]
        .into_iter()
        .map(|key| {
            let part = operation.get(key).cloned().unwrap_or(Value::Null);
            (key.to_owned(), resolved(document, &part, &mut Vec::new()))
        })
        .collect();
    Value::Object(parts)
}

/// `value` with every local `$ref` replaced by what it points at; a
/// reference back into one being resolved is left as it is.
fn resolved(document: &Value, value: &Value, resolving: &mut Vec<String>) -> Value {
    match value {
        Value::Object(object) => {
            let target = object.get("$ref").and_then(Value::as_str);
            if let Some(target) =
                target.filter(|target| !resolving.iter().any(|seen| seen == target))
            {
                let pointed = target
                    .strip_prefix('#')
                    .and_then(|pointer| document.pointer(pointer))
                    .cloned()
                    .unwrap_or(Value::Null);
                resolving.push(target.to_owned());
                let found = resolved(document, &pointed, resolving);
                resolving.pop();
                return found;
            }
            let entries = object
                .iter()
                .map(|(key, child)| (key.clone(), resolved(document, child, resolving)))
                .collect();
            Value::Object(entries)
        }
        Value::Array(items) => Value::Array(
            items
                .iter()
                .map(|item| resolved(document, item, resolving))
                .collect(),
        ),
        other => other.clone(),
    }
}

/// A document of operations only the code describes and no committed
/// document's prefix covers, with the generated document's components.
fn own_document(
    generated: &Value,
    code: &BTreeMap<(String, String), Located<'_>>,
    keys: &[&(String, String)],
    taken_ids: &mut BTreeSet<String>,
    coverage: &mut Coverage,
) -> Value {
    let mut paths = Map::new();
    for key in keys {
        let from_code = &code[*key];
        let mut operation = from_code.operation.clone();
        let id = operation
            .get("operationId")
            .and_then(Value::as_str)
            .map(str::to_owned);
        if let (Some(id), Some(object)) = (id, operation.as_object_mut()) {
            if !taken_ids.insert(id) {
                object.remove("operationId");
            }
        }
        set_operation(&mut paths, from_code.template, from_code.method, operation);
        coverage
            .added
            .push(format!("{} {}", key.0, from_code.template));
    }
    let mut document = Map::new();
    for key in ["openapi", "info"] {
        if let Some(value) = generated.get(key) {
            document.insert(key.to_owned(), value.clone());
        }
    }
    document.insert("paths".to_owned(), Value::Object(paths));
    if let Some(components) = generated.get("components") {
        document.insert("components".to_owned(), components.clone());
    }
    Value::Object(document)
}

/// Gives `operation` the code's `x-lemma-handler`, if it has one.
fn copy_handler(from_code: &Value, operation: &mut Value) {
    let handler = from_code.get("x-lemma-handler").cloned();
    if let (Some(handler), Some(object)) = (handler, operation.as_object_mut()) {
        object.insert("x-lemma-handler".to_owned(), handler);
    }
}

fn has_operation(item: &Map<String, Value>) -> bool {
    item.keys().any(|key| METHODS.contains(&key.as_str()))
}

fn remove_operation(paths: &mut Map<String, Value>, located: &Located<'_>) {
    if let Some(item) = paths
        .get_mut(located.template)
        .and_then(Value::as_object_mut)
    {
        item.remove(located.method);
    }
}

fn set_operation(paths: &mut Map<String, Value>, template: &str, method: &str, operation: Value) {
    let item = paths
        .entry(template.to_owned())
        .or_insert_with(|| Value::Object(Map::new()));
    if let Some(item) = item.as_object_mut() {
        item.insert(method.to_owned(), operation);
    }
}

/// The code's operation under the committed one's name. The `operationId`
/// is how links and generated clients name an operation, and the committed
/// documents are what they were generated from, so the committed name
/// wins; a framework's own default (`FastAPI`'s `read_item_items__id__get`)
/// would otherwise rename every operation. One borrowed through an alias
/// without a committed name gets none, rather than its sibling's.
fn named_like(from_code: &Located<'_>, committed: &Located<'_>, is_same_path: bool) -> Value {
    let mut operation = from_code.operation.clone();
    let Some(object) = operation.as_object_mut() else {
        return operation;
    };
    match committed.operation.get("operationId") {
        Some(id) => {
            object.insert("operationId".to_owned(), id.clone());
        }
        None if !is_same_path => {
            object.remove("operationId");
        }
        None => {}
    }
    operation
}

/// Renames the code's path parameters to the committed document's names,
/// position by position, so the operation's parameters agree with the
/// path it is filed under.
fn rename_path_parameters(operation: &mut Value, code_template: &str, committed_template: &str) {
    let code_names = parameter_names(code_template);
    let committed_names = parameter_names(committed_template);
    if code_names == committed_names || code_names.len() != committed_names.len() {
        return;
    }
    let renames: BTreeMap<&str, &str> = code_names
        .iter()
        .copied()
        .zip(committed_names.iter().copied())
        .collect();
    let Some(parameters) = operation
        .get_mut("parameters")
        .and_then(Value::as_array_mut)
    else {
        return;
    };
    for parameter in parameters {
        let is_path = parameter.get("in").and_then(Value::as_str) == Some("path");
        let committed_name = parameter
            .get("name")
            .and_then(Value::as_str)
            .and_then(|name| renames.get(name));
        if let (true, Some(committed_name)) = (is_path, committed_name) {
            parameter["name"] = Value::String((*committed_name).to_owned());
        }
    }
}

/// The names of a path template's parameters, in order.
fn parameter_names(template: &str) -> Vec<&str> {
    template
        .split('{')
        .skip(1)
        .filter_map(|part| part.split_once('}').map(|(name, _)| name))
        .collect()
}

/// The generated components to rename, by kind and name, because a
/// committed component of the same kind and name differs: the committed
/// one stays for the operations the code did not replace.
fn component_renames(document: &Value, generated: &Value) -> BTreeMap<String, String> {
    let mut renames = BTreeMap::new();
    let Some(kinds) = generated.get("components").and_then(Value::as_object) else {
        return renames;
    };
    for (kind, components) in kinds {
        let Some(components) = components.as_object() else {
            continue;
        };
        for (name, component) in components {
            let committed = document.pointer(&format!("/components/{kind}/{}", escape(name)));
            if committed.is_some_and(|committed| committed != component) {
                renames.insert(
                    reference(kind, name),
                    reference(kind, &format!("{name}__code")),
                );
            }
        }
    }
    renames
}

fn reference(kind: &str, name: &str) -> String {
    format!("#/components/{kind}/{}", escape(name))
}

/// A name as a JSON pointer token.
fn escape(name: &str) -> String {
    name.replace('~', "~0").replace('/', "~1")
}

/// Points every `$ref` in `value` that `renames` names at its new name.
fn rewrite_references(value: &mut Value, renames: &BTreeMap<String, String>) {
    if renames.is_empty() {
        return;
    }
    match value {
        Value::Object(object) => {
            for (key, child) in object.iter_mut() {
                let new_target = (key == "$ref")
                    .then(|| child.as_str().and_then(|target| renames.get(target)))
                    .flatten();
                match new_target {
                    Some(new_target) => *child = Value::String(new_target.clone()),
                    None => rewrite_references(child, renames),
                }
            }
        }
        Value::Array(items) => {
            for item in items {
                rewrite_references(item, renames);
            }
        }
        _ => {}
    }
}

/// Adds every kind of the generated document's components to `document`,
/// under their new names where they collide with a different committed one.
fn merge_components(
    document: &mut Map<String, Value>,
    generated: &Value,
    renames: &BTreeMap<String, String>,
) {
    let Some(kinds) = generated.get("components").and_then(Value::as_object) else {
        return;
    };
    let components = document
        .entry("components")
        .or_insert_with(|| Value::Object(Map::new()));
    let Some(components) = components.as_object_mut() else {
        return;
    };
    for (kind, generated_components) in kinds {
        let Some(generated_components) = generated_components.as_object() else {
            continue;
        };
        let target = components
            .entry(kind.clone())
            .or_insert_with(|| Value::Object(Map::new()));
        let Some(target) = target.as_object_mut() else {
            continue;
        };
        for (name, component) in generated_components {
            let mut component = component.clone();
            rewrite_references(&mut component, renames);
            let new_name = renames
                .get(&reference(kind, name))
                .map_or(name.clone(), |_| format!("{name}__code"));
            target.insert(new_name, component);
        }
    }
}

/// The static prefix every path of a document shares, as written
/// (`/api/v3.1`).
fn shared_prefix(document: &Value) -> String {
    let mut paths = document
        .get("paths")
        .and_then(Value::as_object)
        .into_iter()
        .flat_map(|paths| paths.keys())
        .map(|path| path.split('/').collect::<Vec<_>>());
    let Some(mut prefix) = paths.next() else {
        return String::new();
    };
    for path in paths {
        let shared = prefix
            .iter()
            .zip(&path)
            .take_while(|(left, right)| left == right)
            .count();
        prefix.truncate(shared);
    }
    let static_length = prefix
        .iter()
        .take_while(|segment| !segment.starts_with('{'))
        .count();
    prefix.truncate(static_length);
    prefix.join("/")
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::*;

    fn named(documents: Vec<Value>) -> Vec<(String, Value)> {
        documents
            .into_iter()
            .enumerate()
            .map(|(index, document)| (format!("doc{index}.json"), document))
            .collect()
    }

    fn complete() -> Reading {
        Reading {
            unread_routes: BTreeSet::new(),
            is_complete: true,
            ..Reading::default()
        }
    }

    #[test]
    fn code_replaces_operations_at_their_path_or_through_an_alias() {
        let v1 = json!({"paths": {
            "/api/v1/a": {"get": {"summary": "old a", "operationId": "getA"}},
            "/api/v1/b": {"get": {"summary": "old b"}}
        }});
        let v2 = json!({"paths": {
            "/api/v2/a": {"get": {"summary": "old a", "operationId": "getA2"}},
            "/api/v2/b": {"get": {"summary": "old b"}}
        }});
        let generated = json!({
            "paths": {
                "/api/v1/a": {"get": {"summary": "new a", "operationId": "framework_name"}},
                "/api/v1/b": {"get": {"summary": "new b"}}
            },
            "components": {"schemas": {"A": {"type": "object"}}}
        });

        let laid = overlay(&named(vec![v1, v2]), &generated, &complete(), "code");

        let v1 = &laid.documents[0].1;
        let v2 = &laid.documents[1].1;
        assert_eq!(v1["paths"]["/api/v1/a"]["get"]["summary"], "new a");
        assert_eq!(v1["paths"]["/api/v1/a"]["get"]["operationId"], "getA");
        assert_eq!(v2["paths"]["/api/v2/a"]["get"]["summary"], "new a");
        assert_eq!(v2["paths"]["/api/v2/a"]["get"]["operationId"], "getA2");
        assert_eq!(v2["components"]["schemas"]["A"]["type"], "object");
        assert_eq!(laid.coverage.matched, 4);
    }

    #[test]
    fn only_differences_in_what_callers_depend_on_are_changes() {
        let committed = json!({
            "paths": {
                "/a": {"post": {"requestBody": {"$ref": "#/components/requestBodies/A"}}},
                "/b": {"post": {"requestBody": {"content": {"application/json": {"schema": {"required": ["x"]}}}}}}
            },
            "components": {"requestBodies": {"A": {"content": {}}}}
        });
        let generated = json!({"paths": {
            "/a": {"post": {"requestBody": {"content": {}}, "visibility": "public"}},
            "/b": {"post": {"requestBody": {"content": {"application/json": {"schema": {"required": ["x", "y"]}}}}}}
        }});

        let laid = overlay(&named(vec![committed]), &generated, &complete(), "code");

        assert_eq!(laid.coverage.changed, ["POST /b"]);
    }

    #[test]
    fn a_routes_only_reading_keeps_committed_schemas() {
        let committed = json!({"paths": {
            "/a": {"post": {"operationId": "postA", "requestBody": {"required": true}}},
            "/gone": {"get": {}}
        }});
        let generated = json!({"paths": {
            "/a": {"post": {"x-lemma-handler": "app.js:3"}},
            "/new": {"get": {}}
        }});
        let reading = Reading {
            is_complete: true,
            is_routes_only: true,
            ..Reading::default()
        };

        let laid = overlay(&named(vec![committed]), &generated, &reading, "code");

        let operation = &laid.documents[0].1["paths"]["/a"]["post"];
        assert_eq!(operation["requestBody"]["required"], true);
        assert_eq!(operation["x-lemma-handler"], "app.js:3");
        assert_eq!(laid.coverage.changed, Vec::<String>::new());
        assert_eq!(laid.coverage.removed, ["GET /gone"]);
    }

    #[test]
    fn unrelated_documents_do_not_borrow_each_others_operations() {
        let admin = json!({"paths": {"/admin/users": {"get": {"summary": "admin"}}}});
        let public = json!({"paths": {"/public/users": {"get": {"summary": "old"}}}});
        let generated = json!({"paths": {"/public/users": {"get": {"summary": "new"}}}});
        let reading = Reading::default();

        let laid = overlay(&named(vec![admin, public]), &generated, &reading, "code");

        assert_eq!(
            laid.documents[0].1["paths"]["/admin/users"]["get"]["summary"],
            "admin"
        );
    }

    #[test]
    fn differently_named_parameters_are_the_same_operation() {
        let committed = json!({"paths": {"/items/{id}/": {"get": {"operationId": "getItem"}}}});
        let generated = json!({"paths": {"/items/{itemId}": {"get": {
            "parameters": [{"name": "itemId", "in": "path", "required": true}]
        }}}});

        let laid = overlay(&named(vec![committed]), &generated, &complete(), "code");

        let operation = &laid.documents[0].1["paths"]["/items/{id}/"]["get"];
        assert_eq!(operation["parameters"][0]["name"], "id");
        assert_eq!(operation["operationId"], "getItem");
        assert_eq!(laid.coverage.removed, Vec::<String>::new());
    }

    #[test]
    fn a_complete_reading_removes_what_the_code_no_longer_serves() {
        let committed = json!({"paths": {
            "/a": {"get": {}}, "/b": {"get": {}}, "/gone": {"delete": {}}, "/lazy": {"post": {}}
        }});
        let generated =
            json!({"paths": {"/a": {"get": {}}, "/b": {"get": {}}, "/new": {"put": {}}}});
        let reading = Reading {
            unread_routes: BTreeSet::from([("POST".to_owned(), "/lazy".to_owned())]),
            is_complete: true,
            ..Reading::default()
        };

        let laid = overlay(&named(vec![committed]), &generated, &reading, "code");

        let paths = &laid.documents[0].1["paths"];
        assert!(paths.get("/gone").is_none());
        assert!(paths.get("/lazy").is_some());
        assert!(paths.get("/new").is_none());
        assert_eq!(laid.documents[1].0, "code");
        assert!(laid.documents[1].1["paths"].get("/new").is_some());
        assert_eq!(laid.coverage.removed, ["DELETE /gone"]);
        assert_eq!(laid.coverage.added, ["PUT /new"]);
    }

    #[test]
    fn a_reading_matching_too_little_changes_only_what_matched() {
        let committed =
            json!({"paths": {"/api/a": {"get": {}}, "/api/b": {"get": {}}, "/api/c": {"get": {}}}});
        let generated =
            json!({"paths": {"/a": {"get": {}}, "/api/c": {"get": {"summary": "new"}}}});

        let laid = overlay(&named(vec![committed]), &generated, &complete(), "code");

        let paths = &laid.documents[0].1["paths"];
        assert!(!laid.coverage.is_trusted);
        assert!(paths.get("/api/a").is_some());
        assert!(paths.get("/a").is_none());
        assert_eq!(paths["/api/c"]["get"]["summary"], "new");
    }

    #[test]
    fn a_differing_component_of_the_same_name_is_renamed() {
        let committed = json!({
            "paths": {"/a": {"get": {"responses": {"200": {"$ref": "#/components/responses/R"}}}},
                      "/b": {"get": {"responses": {"200": {"$ref": "#/components/responses/R"}}}}},
            "components": {"responses": {"R": {"description": "committed"}}}
        });
        let generated = json!({
            "paths": {"/a": {"get": {"responses": {"200": {"$ref": "#/components/responses/R"}}}}},
            "components": {"responses": {"R": {"description": "code"}}}
        });

        let laid = overlay(
            &named(vec![committed]),
            &generated,
            &Reading::default(),
            "code",
        );

        let document = &laid.documents[0].1;
        assert_eq!(
            document["paths"]["/a"]["get"]["responses"]["200"]["$ref"],
            "#/components/responses/R__code"
        );
        assert_eq!(
            document["paths"]["/b"]["get"]["responses"]["200"]["$ref"],
            "#/components/responses/R"
        );
        assert_eq!(
            document["components"]["responses"]["R"]["description"],
            "committed"
        );
    }
}
