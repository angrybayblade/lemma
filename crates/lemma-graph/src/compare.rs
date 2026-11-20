//! Comparing what one side sends with what the other accepts.
//!
//! `json-schema-diff` compares an old and a new version of one schema and
//! calls a change breaking when the new version is stricter. A link has two
//! different schemas, so they are cast as versions: the **sender's** schema
//! as old, the **receiver's** as new. A breaking change is then something
//! the sender may send that the receiver rejects. For a request the caller
//! sends; for a response the provider does.
//!
//! Two of the tool's verdicts assume one schema's versions and are replaced:
//! a property only the receiver declares is harmless (whether it is
//! required is reported separately), and a property only the sender sends
//! breaks only a receiver that rejects unknown fields, which the tool judges
//! by the sender's `additionalProperties` rather than the receiver's.

use json_schema_diff::{Change, ChangeKind};
use serde_json::Value;

use crate::GraphError;
use crate::schema::UNFOLLOWED;

/// One way the sender's and receiver's schemas differ.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize)]
pub struct Difference {
    /// Where, as a JSON path (`""` for the whole document).
    pub path: String,
    /// What differs, from the sender's and receiver's points of view.
    pub description: String,
    /// Whether the receiver can reject or misread what the sender sends.
    pub breaking: bool,
}

/// How `sender`'s schema differs from `receiver`'s. With `expected_fields`,
/// a field the receiver declares but the sender never sends is a problem:
/// for a response, the receiver reads a field that will not be there.
pub fn compare(
    sender: &Value,
    receiver: &Value,
    expected_fields: bool,
) -> Result<Vec<Difference>, GraphError> {
    let (sender, receiver) = &without_unfollowed(sender, receiver);
    let changes = json_schema_diff::diff(sender.clone(), receiver.clone()).map_err(|source| {
        GraphError::Compare {
            message: source.to_string(),
        }
    })?;
    let mut differences: Vec<Difference> = changes
        .iter()
        .map(|change| difference(change, expected_fields))
        .collect();
    rejected_fields(sender, receiver, "", &mut differences);
    differences.sort_by(|left, right| {
        (&left.path, &left.description).cmp(&(&right.path, &right.description))
    });
    differences.dedup();
    Ok(differences)
}

/// The combinators whose members cannot be paired between two schemas.
const COMBINATORS: [&str; 3] = ["anyOf", "oneOf", "allOf"];

/// Both schemas with the places left out where either holds a type its
/// reader could not follow. Such a type says nothing about what is sent or
/// accepted there: comparing it as "anything" reports a sender that may
/// leave out every field and send every kind of value, none of it known.
/// Its reader reports the type by name; here its place compares as equal.
fn without_unfollowed(sender: &Value, receiver: &Value) -> (Value, Value) {
    if is_unknown(sender) || is_unknown(receiver) {
        return (
            Value::Object(serde_json::Map::new()),
            Value::Object(serde_json::Map::new()),
        );
    }
    let (Value::Object(outgoing), Value::Object(incoming)) = (sender, receiver) else {
        return (stripped(sender), stripped(receiver));
    };
    let mut outgoing = outgoing.clone();
    let mut incoming = incoming.clone();
    for key in ["items", "additionalProperties"] {
        if let (Some(left), Some(right)) = (outgoing.get(key), incoming.get(key)) {
            let (left, right) = without_unfollowed(left, right);
            outgoing.insert(key.to_owned(), left);
            incoming.insert(key.to_owned(), right);
        }
    }
    if let (Some(Value::Object(left)), Some(Value::Object(right))) = (
        outgoing.get("properties").cloned(),
        incoming.get("properties").cloned(),
    ) {
        let mut left = left;
        let mut right = right;
        let shared: Vec<String> = left
            .keys()
            .filter(|name| right.contains_key(*name))
            .cloned()
            .collect();
        for name in shared {
            let (one, other) = without_unfollowed(&left[&name], &right[&name]);
            left.insert(name.clone(), one);
            right.insert(name, other);
        }
        outgoing.insert("properties".to_owned(), Value::Object(left));
        incoming.insert("properties".to_owned(), Value::Object(right));
    }
    (
        stripped(&Value::Object(outgoing)),
        stripped(&Value::Object(incoming)),
    )
}

/// Whether nothing is known of what `schema` describes: it stands for an
/// unfollowed type, or one of the alternatives it combines does, and the
/// alternatives of two schemas cannot be paired.
fn is_unknown(schema: &Value) -> bool {
    let Value::Object(fields) = schema else {
        return false;
    };
    fields.contains_key(UNFOLLOWED)
        || COMBINATORS
            .iter()
            .any(|combinator| fields.get(*combinator).is_some_and(holds_unfollowed))
}

/// Whether an unfollowed type's schema is anywhere inside `value`.
fn holds_unfollowed(value: &Value) -> bool {
    match value {
        Value::Object(fields) => {
            fields.contains_key(UNFOLLOWED) || fields.values().any(holds_unfollowed)
        }
        Value::Array(items) => items.iter().any(holds_unfollowed),
        _ => false,
    }
}

/// `schema` with each unfollowed type left on one side alone read as
/// accepting anything: there is nothing across from it to compare with.
fn stripped(schema: &Value) -> Value {
    match schema {
        Value::Object(fields) if fields.contains_key(UNFOLLOWED) => {
            Value::Object(serde_json::Map::new())
        }
        Value::Object(fields) => Value::Object(
            fields
                .iter()
                .map(|(key, value)| (key.clone(), stripped(value)))
                .collect(),
        ),
        Value::Array(items) => Value::Array(items.iter().map(stripped).collect()),
        other => other.clone(),
    }
}

fn difference(change: &Change, expected_fields: bool) -> Difference {
    let at = if change.path.is_empty() {
        "the root"
    } else {
        change.path.as_str()
    };
    let description = match &change.change {
        ChangeKind::TypeAdd { added } => {
            format!("the receiver also accepts {added:?} at {at}; the sender never sends it")
        }
        ChangeKind::TypeRemove { removed } => {
            format!("the sender may send {removed:?} at {at}; the receiver does not accept it")
        }
        ChangeKind::PropertyAdd { added, .. } if expected_fields => {
            format!("the receiver expects `{added}` at {at}; the sender does not send it")
        }
        ChangeKind::PropertyAdd { added, .. } => {
            format!("the receiver knows `{added}` at {at}; the sender never sends it")
        }
        ChangeKind::PropertyRemove { removed, .. } => {
            format!("the sender sends `{removed}` at {at}; the receiver does not declare it")
        }
        ChangeKind::RequiredAdd { property } => {
            format!("the receiver requires `{property}` at {at}; the sender may leave it out")
        }
        ChangeKind::RequiredRemove { property } => format!(
            "the sender always sends `{property}` at {at}; the receiver treats it as optional"
        ),
        ChangeKind::EnumAdd { added, .. } => {
            format!("the receiver accepts {added} at {at}; the sender never sends it")
        }
        ChangeKind::EnumRemove { removed, .. } => {
            format!("the sender may send {removed} at {at}; the receiver does not accept it")
        }
        ChangeKind::ConstAdd { added } => {
            format!("the receiver requires the value {added} at {at}")
        }
        ChangeKind::ConstRemove { removed } => {
            format!("the sender may send {removed} at {at}; the receiver no longer requires it")
        }
        other => format!(
            "{} at {at}",
            serde_json::to_string(other).unwrap_or_default()
        ),
    };
    let breaking = match &change.change {
        ChangeKind::PropertyAdd { .. } => expected_fields,
        ChangeKind::PropertyRemove { .. } => false,
        other => other.is_breaking(),
    };
    Difference {
        path: change.path.clone(),
        description,
        breaking,
    }
}

/// Fields the sender sends that a receiver with `additionalProperties:
/// false` rejects, through nested objects and array items.
fn rejected_fields(sender: &Value, receiver: &Value, path: &str, out: &mut Vec<Difference>) {
    let empty = serde_json::Map::new();
    let sender_properties = sender
        .get("properties")
        .and_then(Value::as_object)
        .unwrap_or(&empty);
    let receiver_properties = receiver
        .get("properties")
        .and_then(Value::as_object)
        .unwrap_or(&empty);
    let strict = receiver.get("additionalProperties") == Some(&Value::Bool(false));
    for (name, sent) in sender_properties {
        let at = format!("{path}.{name}");
        match receiver_properties.get(name) {
            Some(accepted) => rejected_fields(sent, accepted, &at, out),
            None if strict => out.push(Difference {
                path: path.to_owned(),
                description: format!(
                    "the sender sends `{name}` at {}; the receiver rejects unknown fields",
                    if path.is_empty() { "the root" } else { path }
                ),
                breaking: true,
            }),
            None => {}
        }
    }
    if let (Some(sent), Some(accepted)) = (sender.get("items"), receiver.get("items")) {
        rejected_fields(sent, accepted, &format!("{path}.[]"), out);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn a_field_the_receiver_requires_and_the_sender_omits_is_breaking() {
        let sender = json!({ "type": "object", "properties": { "auth": { "type": "object" } }, "required": ["auth"] });
        let receiver = json!({
            "type": "object",
            "properties": { "auth": { "type": "object" }, "secrets": { "type": "object" } },
            "required": ["auth", "secrets"]
        });

        let differences = compare(&sender, &receiver, false).unwrap();

        let breaking: Vec<&str> = differences
            .iter()
            .filter(|d| d.breaking)
            .map(|d| d.description.as_str())
            .collect();
        assert_eq!(
            breaking,
            ["the receiver requires `secrets` at the root; the sender may leave it out"]
        );
    }

    #[test]
    fn extra_fields_are_fine_unless_the_receiver_rejects_them() {
        let sender = json!({ "type": "object", "properties": { "a": {}, "b": {} } });
        let lenient = json!({ "type": "object", "properties": { "a": {} } });
        let strict =
            json!({ "type": "object", "properties": { "a": {} }, "additionalProperties": false });

        let with_lenient = compare(&sender, &lenient, false).unwrap();
        let with_strict = compare(&sender, &strict, false).unwrap();

        assert!(with_lenient.iter().all(|d| !d.breaking));
        assert!(with_strict.iter().any(|d| d.breaking));
    }

    #[test]
    fn null_the_receiver_cannot_take_is_breaking() {
        let sender =
            json!({ "type": "object", "properties": { "n": { "type": ["integer", "null"] } } });
        let receiver = json!({ "type": "object", "properties": { "n": { "type": "integer" } } });

        let differences = compare(&sender, &receiver, false).unwrap();

        assert!(differences.iter().any(|d| d.breaking && d.path == ".n"));
    }

    #[test]
    fn a_response_field_the_receiver_expects_and_the_sender_drops_is_breaking() {
        let sender = json!({ "type": "object", "properties": { "a": {} } });
        let receiver = json!({ "type": "object", "properties": { "a": {}, "gone": {} } });

        let response = compare(&sender, &receiver, true).unwrap();
        let request = compare(&sender, &receiver, false).unwrap();

        assert!(
            response
                .iter()
                .any(|d| d.breaking && d.description.contains("`gone`"))
        );
        assert!(request.iter().all(|d| !d.breaking));
    }

    #[test]
    fn a_type_the_reader_could_not_follow_is_not_compared_as_anything() {
        let sender = crate::schema::unfollowed("z.infer");
        let receiver = json!({
            "type": "object",
            "properties": { "customer": { "type": "string" } },
            "required": ["customer"]
        });

        // Read as "anything", the sender would leave `customer` out and
        // send arrays, numbers and nulls: none of that is known.
        assert_eq!(compare(&sender, &receiver, false).unwrap(), []);
        assert_eq!(compare(&receiver, &sender, true).unwrap(), []);
    }

    #[test]
    fn fields_beside_an_unfollowed_type_are_still_compared() {
        let sender = json!({
            "type": "object",
            "properties": {
                "meta": crate::schema::unfollowed("Meta"),
                "amount": { "type": "string" }
            },
            "required": ["meta", "amount"]
        });
        let receiver = json!({
            "type": "object",
            "properties": {
                "meta": { "type": "object", "properties": { "id": { "type": "string" } }, "required": ["id"] },
                "amount": { "type": "integer" }
            },
            "required": ["meta", "amount"]
        });

        let differences = compare(&sender, &receiver, false).unwrap();

        assert!(
            differences
                .iter()
                .all(|difference| difference.path.contains("amount")),
            "{differences:?}"
        );
        assert!(
            differences.iter().any(|difference| difference.breaking),
            "{differences:?}"
        );
    }

    #[test]
    fn an_alternative_that_could_not_be_followed_leaves_the_whole_choice_out() {
        let sender = json!({
            "type": "object",
            "properties": {
                "result": { "anyOf": [crate::schema::unfollowed("Outcome"), { "type": "null" }] }
            }
        });
        let receiver = json!({
            "type": "object",
            "properties": { "result": { "type": "string" } }
        });

        assert_eq!(compare(&sender, &receiver, false).unwrap(), []);
    }
}
