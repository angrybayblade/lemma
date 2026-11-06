//! Values crossing the checker protocol (spec 13, section 10): a demand's
//! arguments as the JSON in `LEMMA_INPUT`, and what a checker observed,
//! from `LEMMA_OUTPUT`, as values a behavior reads.
//!
//! Operations go out as their address plus what binds them to code: the
//! service, the repository and its checkout, and the implementation's file
//! and symbol. Facts go out as their relation, their values by column and
//! whether they hold.

use std::collections::{BTreeMap, HashMap};
use std::path::PathBuf;

use lemma_lang::eval::{Datum, Evidence};
use lemma_lang::model::{Model, Operation};
use lemma_lang::types::{Address, Value};
use serde_json::{Map, Value as Json, json};

/// What encoding operations needs: where each is declared, the repository
/// each module is in, and where each repository is checked out.
pub(crate) struct Encoder<'m> {
    operations: HashMap<&'m Address, &'m Operation>,
    repositories: BTreeMap<String, String>,
    roots: BTreeMap<String, PathBuf>,
}

impl<'m> Encoder<'m> {
    /// An encoder for `model`, whose modules are in `repositories` (module
    /// name to repository name), checked out at `roots`.
    pub(crate) fn new(
        model: &'m Model,
        repositories: BTreeMap<String, String>,
        roots: BTreeMap<String, PathBuf>,
    ) -> Self {
        let operations = model
            .services
            .iter()
            .flat_map(|service| &service.operations)
            .map(|operation| (&operation.address, operation))
            .collect();
        Self {
            operations,
            repositories,
            roots,
        }
    }

    /// A value as the JSON a checker reads.
    pub(crate) fn encode(&self, datum: &Datum) -> Json {
        match datum {
            Datum::Null | Datum::Missing(_) | Datum::Unknown(_) | Datum::Evidence(_) => Json::Null,
            Datum::Bool(value) => json!(value),
            Datum::Int(value) => json!(value),
            Datum::Float(value) => json!(value),
            Datum::Text(text) => json!(text),
            Datum::List(items) => Json::Array(items.iter().map(|item| self.encode(item)).collect()),
            Datum::Record(fields) => Json::Object(
                fields
                    .iter()
                    .map(|(name, value)| (name.clone(), self.encode(value)))
                    .collect(),
            ),
            Datum::Declared(value) => self.declared(value),
            Datum::Facts(predicate, snapshot) => json!({
                "relation": predicate.address().to_string(),
                "snapshot": snapshot.as_str(),
            }),
            Datum::Fact(fact) => {
                let values: Map<String, Json> = fact
                    .columns
                    .iter()
                    .zip(&fact.key)
                    .map(|(column, value)| (column.clone(), self.encode(value)))
                    .collect();
                json!({
                    "relation": fact.predicate.address().to_string(),
                    "values": values,
                    "exists": fact.exists.as_ref().ok(),
                    "supports": fact.supports,
                })
            }
        }
    }

    fn declared(&self, value: &Value) -> Json {
        match value {
            Value::Operation(address) => self.operation(address),
            Value::Service(address) => json!({
                "service": address.to_string(),
                "name": address.dotted_path(),
                "repository": self.repository_of(address),
                "root": self.root_of(address),
            }),
            Value::Discovered { service, key } => json!({
                "operation": key,
                "discovered": true,
                "service": service.to_string(),
                "repository": self.repository_of(service),
                "root": self.root_of(service),
            }),
            Value::Entity { entity, fields } => {
                let mut object: Map<String, Json> = fields
                    .iter()
                    .map(|(name, value)| (name.clone(), self.encode(&Datum::from_value(value))))
                    .collect();
                object.insert("entity".to_owned(), json!(entity.to_string()));
                Json::Object(object)
            }
            other => self.encode(&Datum::from_value(other)),
        }
    }

    fn operation(&self, address: &Address) -> Json {
        let mut service = address.clone();
        service.path.pop();
        let declared = self.operations.get(address);
        let implementation = declared
            .and_then(|operation| operation.fields.get("implementation"))
            .and_then(crate::lm::implementation_path)
            .map(|path| {
                let (file, symbol) = path.split_once(':').unwrap_or((path.as_str(), ""));
                json!({ "path": path, "file": file, "symbol": symbol })
            });
        json!({
            "operation": address.to_string(),
            "name": address.dotted_path(),
            "client": declared.is_some_and(|operation| operation.is_client),
            "service": service.to_string(),
            "repository": self.repository_of(address),
            "root": self.root_of(address),
            "implementation": implementation,
        })
    }

    fn repository_of(&self, address: &Address) -> Option<&String> {
        self.repositories.get(&address.module)
    }

    fn root_of(&self, address: &Address) -> Option<String> {
        let repository = self.repository_of(address)?;
        self.roots
            .get(repository)
            .map(|root| root.display().to_string())
    }
}

/// A checker's output, as spec 13 section 10 defines it: what it observed,
/// or why it could not. `None` for output of neither shape.
pub(crate) fn decode_output(output: &Json) -> Option<Decoded> {
    if let Some(reason) = output.get("unavailable") {
        let reason = reason
            .as_str()
            .map_or_else(|| reason.to_string(), str::to_owned);
        return Some(Decoded::Unavailable(reason));
    }
    let observed = output.get("observed")?;
    Some(Decoded::Observed(decode(observed)))
}

/// A checker's output, read.
pub(crate) enum Decoded {
    Observed(Datum),
    Unavailable(String),
}

impl Decoded {
    /// As evidence with `key`.
    pub(crate) fn into_evidence(self, key: &str, was_reused: bool) -> Evidence {
        match self {
            Decoded::Observed(value) => Evidence::Observed {
                value,
                key: key.to_owned(),
                was_reused,
            },
            Decoded::Unavailable(reason) => Evidence::Unavailable(reason),
        }
    }
}

/// A JSON value as a value a behavior reads: objects keep their fields'
/// order, and numbers that are not integers stay numbers.
pub(crate) fn decode(value: &Json) -> Datum {
    match value {
        Json::Null => Datum::Null,
        Json::Bool(value) => Datum::Bool(*value),
        Json::Number(number) => number.as_i64().map_or_else(
            || Datum::Float(number.as_f64().unwrap_or(f64::NAN)),
            Datum::Int,
        ),
        Json::String(text) => Datum::Text(text.clone()),
        Json::Array(items) => Datum::List(items.iter().map(decode).collect()),
        Json::Object(fields) => Datum::Record(
            fields
                .iter()
                .map(|(name, value)| (name.clone(), decode(value)))
                .collect(),
        ),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn json_objects_become_records_and_integers_stay_integers() {
        let decoded =
            decode(&json!({ "status": 200, "body": { "retriable": true }, "ratio": 0.5 }));

        assert_eq!(decoded.field("status"), Some(&Datum::Int(200)));
        assert_eq!(
            decoded
                .field("body")
                .and_then(|body| body.field("retriable")),
            Some(&Datum::Bool(true))
        );
        assert_eq!(decoded.field("ratio"), Some(&Datum::Float(0.5)));
    }

    #[test]
    fn output_is_observed_or_unavailable_or_neither() {
        assert!(matches!(
            decode_output(&json!({ "observed": 1 })),
            Some(Decoded::Observed(Datum::Int(1)))
        ));
        assert!(matches!(
            decode_output(&json!({ "unavailable": "no network" })),
            Some(Decoded::Unavailable(reason)) if reason == "no network"
        ));
        assert!(decode_output(&json!({ "result": 1 })).is_none());
    }
}
