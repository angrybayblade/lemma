//! What expressions evaluate to when a behavior runs: values, facts and
//! evidence, and the two ways of not having a value, missing and unknown
//! (spec 13, section 5).

use std::fmt;

use crate::model::Predicate;
use crate::syntax::ast::Snapshot;
use crate::types::{Type, Value};

/// A value while a behavior runs.
#[derive(Debug, Clone, PartialEq)]
pub enum Datum {
    /// A JSON `null` that is present.
    Null,
    /// A boolean.
    Bool(bool),
    /// An integer.
    Int(i64),
    /// A JSON number that is not an integer.
    Float(f64),
    /// A string.
    Text(String),
    /// A list, or a JSON array.
    List(Vec<Datum>),
    /// A record, or a JSON object, fields in order.
    Record(Vec<(String, Datum)>),
    /// A declared thing: a service, an operation or an entity value.
    Declared(Value),
    /// A relation, graph or rule's facts at a snapshot, to look up or
    /// iterate.
    Facts(Predicate, Snapshot),
    /// One looked-up tuple of a relation.
    Fact(Box<FactDatum>),
    /// A checker's answer.
    Evidence(Box<Evidence>),
    /// What reading an absent JSON field or element gives.
    Missing(Absence),
    /// A truth Lemma cannot establish, with why.
    Unknown(String),
}

/// What was absent: the field or element, and the expression it was read
/// from.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Absence {
    /// The field's name, or `[index]`.
    pub name: String,
}

/// A fact lookup's result.
#[derive(Debug, Clone, PartialEq)]
pub struct FactDatum {
    /// What was looked up.
    pub predicate: Predicate,
    /// The key, one value per column.
    pub key: Vec<Datum>,
    /// Its columns' names.
    pub columns: Vec<String>,
    /// Whether it holds: `None` when it was not found and its relation is
    /// not completely covered, with why.
    pub exists: Result<bool, String>,
    /// Where each stated fact came from, for `supports`.
    pub supports: Vec<String>,
}

/// A checker's answer to one demand.
#[derive(Debug, Clone, PartialEq)]
pub enum Evidence {
    /// What it observed.
    Observed {
        /// The value.
        value: Datum,
        /// The evidence's key: what it was computed from.
        key: String,
        /// Whether stored evidence with an equal key was reused instead of
        /// running the checker.
        was_reused: bool,
    },
    /// It could not observe anything, with why.
    Unavailable(String),
    /// Stored evidence no longer matches its inputs, and running was not
    /// allowed.
    Stale(String),
}

/// A condition's value (spec 13, section 5).
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Truth {
    /// It holds.
    True,
    /// It does not hold.
    False,
    /// It read an absent JSON field.
    Missing(String),
    /// Lemma cannot tell, with why.
    Unknown(String),
}

impl Datum {
    /// The value a constant has while running.
    pub fn from_value(value: &Value) -> Datum {
        match value {
            Value::Bool(value) => Datum::Bool(*value),
            Value::Int(value) => Datum::Int(*value),
            Value::Text(text) => Datum::Text(text.clone()),
            Value::Null => Datum::Null,
            Value::List(items) => Datum::List(items.iter().map(Datum::from_value).collect()),
            Value::Record(fields) => Datum::Record(
                fields
                    .iter()
                    .map(|(name, value)| (name.clone(), Datum::from_value(value)))
                    .collect(),
            ),
            other => Datum::Declared(other.clone()),
        }
    }

    /// The constant a value is, when it is one: what allowances and
    /// lowered rows compare with.
    pub fn to_value(&self) -> Option<Value> {
        let value = match self {
            Datum::Null => Value::Null,
            Datum::Bool(value) => Value::Bool(*value),
            Datum::Int(value) => Value::Int(*value),
            Datum::Text(text) => Value::Text(text.clone()),
            Datum::List(items) => Value::List(
                items
                    .iter()
                    .map(Datum::to_value)
                    .collect::<Option<Vec<Value>>>()?,
            ),
            Datum::Record(fields) => Value::Record(
                fields
                    .iter()
                    .map(|(name, value)| Some((name.clone(), value.to_value()?)))
                    .collect::<Option<Vec<(String, Value)>>>()?,
            ),
            Datum::Declared(value) => value.clone(),
            Datum::Float(_)
            | Datum::Facts(..)
            | Datum::Fact(_)
            | Datum::Evidence(_)
            | Datum::Missing(_)
            | Datum::Unknown(_) => return None,
        };
        Some(value)
    }

    /// Whether it is missing or unknown, so an operation on it passes it on.
    pub fn is_absent(&self) -> bool {
        matches!(self, Datum::Missing(_) | Datum::Unknown(_))
    }

    /// The field `name` of a record or JSON object.
    pub fn field(&self, name: &str) -> Option<&Datum> {
        let Datum::Record(fields) = self else {
            return None;
        };
        fields
            .iter()
            .find(|(field, _)| field == name)
            .map(|(_, value)| value)
    }

    /// Type-strict equality: values of different types are never equal,
    /// and object fields compare whatever their order.
    pub fn same_as(&self, other: &Datum) -> bool {
        match (self, other) {
            (Datum::List(left), Datum::List(right)) => {
                left.len() == right.len()
                    && left
                        .iter()
                        .zip(right)
                        .all(|(left, right)| left.same_as(right))
            }
            (Datum::Record(left), Datum::Record(right)) => {
                left.len() == right.len()
                    && left.iter().all(|(name, value)| {
                        other
                            .field(name)
                            .is_some_and(|candidate| value.same_as(candidate))
                    })
                    && right.iter().all(|(name, _)| self.field(name).is_some())
            }
            (Datum::Declared(left), Datum::Declared(right)) => left == right,
            (Datum::Fact(left), Datum::Fact(right)) => {
                left.predicate == right.predicate
                    && left.key.len() == right.key.len()
                    && left
                        .key
                        .iter()
                        .zip(&right.key)
                        .all(|(left, right)| left.same_as(right))
            }
            _ => self == other,
        }
    }

    /// Whether it fits `expected`, the type a checker says it observes.
    pub fn fits(&self, expected: &Type) -> bool {
        match (self, expected) {
            (_, Type::Json | Type::Unknown)
            | (Datum::Bool(_), Type::Bool)
            | (Datum::Int(_), Type::Int)
            | (Datum::Text(_), Type::Text) => true,
            (Datum::List(items), Type::List(item)) => items.iter().all(|value| value.fits(item)),
            (Datum::Record(fields), Type::Record(expected)) => {
                expected.iter().all(|field| match self.field(&field.name) {
                    Some(value) => value.fits(&field.field_type),
                    None => field.is_optional,
                }) && fields
                    .iter()
                    .all(|(name, _)| expected.iter().any(|field| field.name == *name))
            }
            _ => false,
        }
    }

    /// Its truth, as a condition.
    pub fn truth(&self) -> Truth {
        match self {
            Datum::Bool(true) => Truth::True,
            Datum::Bool(false) => Truth::False,
            Datum::Missing(absence) => Truth::Missing(absence.name.clone()),
            Datum::Unknown(reason) => Truth::Unknown(reason.clone()),
            other => Truth::Unknown(format!("{other} is not a `Bool`")),
        }
    }
}

impl From<Truth> for Datum {
    fn from(truth: Truth) -> Self {
        match truth {
            Truth::True => Datum::Bool(true),
            Truth::False => Datum::Bool(false),
            Truth::Missing(name) => Datum::Missing(Absence { name }),
            Truth::Unknown(reason) => Datum::Unknown(reason),
        }
    }
}

impl fmt::Display for Datum {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Datum::Null => write!(formatter, "null"),
            Datum::Bool(value) => write!(formatter, "{value}"),
            Datum::Int(value) => write!(formatter, "{value}"),
            Datum::Float(value) => write!(formatter, "{value}"),
            Datum::Text(text) => write!(formatter, "{text:?}"),
            Datum::List(items) => {
                let written: Vec<String> = items.iter().map(ToString::to_string).collect();
                write!(formatter, "[{}]", written.join(", "))
            }
            Datum::Record(fields) => {
                let written: Vec<String> = fields
                    .iter()
                    .map(|(name, value)| format!("{name}: {value}"))
                    .collect();
                write!(formatter, "{{{}}}", written.join(", "))
            }
            Datum::Declared(value) => write!(formatter, "{}", declared_text(value)),
            Datum::Facts(predicate, snapshot) => write!(
                formatter,
                "{}@{}",
                predicate.address().name(),
                snapshot.as_str()
            ),
            Datum::Fact(fact) => {
                let key: Vec<String> = fact.key.iter().map(ToString::to_string).collect();
                write!(
                    formatter,
                    "{}[{}]",
                    fact.predicate.address().name(),
                    key.join(", ")
                )
            }
            Datum::Evidence(evidence) => match &**evidence {
                Evidence::Observed { value, .. } => write!(formatter, "evidence {value}"),
                Evidence::Unavailable(reason) => write!(formatter, "no evidence ({reason})"),
                Evidence::Stale(reason) => write!(formatter, "stale evidence ({reason})"),
            },
            Datum::Missing(absence) => write!(formatter, "missing `{}`", absence.name),
            Datum::Unknown(reason) => write!(formatter, "unknown ({reason})"),
        }
    }
}

/// A declared thing as a reader names it: an operation by its path in its
/// module, such as `Worker.call_setup_webhook`.
fn declared_text(value: &Value) -> String {
    match value {
        Value::Service(address) | Value::Operation(address) => address.dotted_path(),
        Value::Discovered { service, key } => format!("{}[{key:?}]", service.dotted_path()),
        Value::Entity { entity, fields } => {
            let written: Vec<String> = fields
                .iter()
                .map(|(name, value)| format!("{name}: {}", Datum::from_value(value)))
                .collect();
            format!("{}({})", entity.name(), written.join(", "))
        }
        other => format!("{other:?}"),
    }
}
