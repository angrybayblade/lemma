//! Types and constant values of checked `.lm` code.
//!
//! The checker gives every expression a [`Type`], and every expression it
//! can evaluate without running anything (declarations, rows of graphs,
//! provider requests) a [`Value`]. Lowering reads the values; editors show
//! the types.

use std::fmt;

/// A declaration's address: its module and its path inside the module,
/// such as `functions::Functions.serverless.setup_webhook`.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct Address {
    /// The module's name.
    pub module: String,
    /// The path inside it, outermost first.
    pub path: Vec<String>,
}

impl Address {
    /// The address of `name` at the top of `module`.
    pub fn top(module: &str, name: &str) -> Self {
        Self {
            module: module.to_owned(),
            path: vec![name.to_owned()],
        }
    }

    /// The address of `name` inside this one.
    #[must_use]
    pub fn child(&self, name: &str) -> Self {
        let mut path = self.path.clone();
        path.push(name.to_owned());
        Self {
            module: self.module.clone(),
            path,
        }
    }

    /// The declaration's own name: the path's last segment.
    pub fn name(&self) -> &str {
        self.path.last().map_or("", String::as_str)
    }

    /// The path joined with dots, without the module.
    pub fn dotted_path(&self) -> String {
        self.path.join(".")
    }
}

impl fmt::Display for Address {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(formatter, "{}::{}", self.module, self.dotted_path())
    }
}

/// A field of a record type.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RecordField {
    /// Its name.
    pub name: String,
    /// Its type.
    pub field_type: Type,
    /// Whether a record may leave it out.
    pub is_optional: bool,
}

/// The type of a value.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Type {
    /// `true` or `false`.
    Bool,
    /// A 64-bit signed integer.
    Int,
    /// A string.
    Text,
    /// A JSON value.
    Json,
    /// `null`, which only `Json` accepts.
    Null,
    /// A list of one element type.
    List(Box<Type>),
    /// A record.
    Record(Vec<RecordField>),
    /// A service.
    Service,
    /// An operation; a client operation also has this type.
    Operation,
    /// An outbound operation.
    ClientOperation,
    /// A resolved source symbol.
    Symbol,
    /// A source location.
    Site,
    /// A resolution request: resolved, ambiguous or unresolved.
    Resolution(Box<Type>),
    /// A value of a declared entity.
    Entity(Address),
    /// A relation's name used as a value: the union of its graphs.
    Relation(Address),
    /// One looked-up tuple of a relation.
    Fact(Address),
    /// A named set of facts of a relation.
    Graph(Address),
    /// A checker's result.
    Evidence(Box<Type>),
    /// A type of the standard library, such as `Operations` or `Schema`.
    Builtin(&'static str),
    /// A module brought in with `import * as`.
    Namespace(String),
    /// A function or checker: its parameters and result.
    Function(Box<Signature>),
    /// Not known, because of an earlier error; fits anything, so one
    /// mistake is reported once.
    Unknown,
}

/// What a function takes and returns.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Signature {
    /// Its parameters, in order.
    pub parameters: Vec<Parameter>,
    /// What it returns.
    pub returns: Type,
}

/// One parameter of a function.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Parameter {
    /// Its name, which a named argument uses.
    pub name: String,
    /// Its type.
    pub parameter_type: Type,
    /// Whether a call may leave it out.
    pub is_optional: bool,
}

impl Type {
    /// Whether a value of type `self` can be used where `expected` is.
    pub fn fits(&self, expected: &Type) -> bool {
        match (self, expected) {
            (Type::Unknown, _)
            | (_, Type::Unknown)
            | (Type::ClientOperation, Type::Operation)
            | (Type::Text, Type::Builtin("Command"))
            | (Type::Null | Type::Bool | Type::Int | Type::Text, Type::Json) => true,
            (Type::List(item), Type::Builtin("Command")) => item.fits(&Type::Text),
            (Type::List(item), Type::Json) => item.fits(&Type::Json),
            (Type::Record(fields), Type::Json) => fields
                .iter()
                .all(|field| field.field_type.fits(&Type::Json)),
            (Type::List(item), Type::List(expected_item)) => item.fits(expected_item),
            (Type::Record(fields), Type::Record(expected_fields)) => {
                record_fits(fields, expected_fields)
            }
            _ => self == expected,
        }
    }

    /// Whether it is a type rule columns may have.
    pub fn is_column_type(&self) -> bool {
        matches!(
            self,
            Type::Bool
                | Type::Int
                | Type::Text
                | Type::Service
                | Type::Operation
                | Type::ClientOperation
                | Type::Symbol
                | Type::Site
                | Type::Entity(_)
                | Type::Unknown
        )
    }
}

fn record_fits(fields: &[RecordField], expected: &[RecordField]) -> bool {
    let all_known = fields
        .iter()
        .all(|field| expected.iter().any(|other| other.name == field.name));
    let types_fit = fields.iter().all(|field| {
        expected
            .iter()
            .find(|other| other.name == field.name)
            .is_none_or(|other| field.field_type.fits(&other.field_type))
    });
    let required_present = expected
        .iter()
        .filter(|other| !other.is_optional)
        .all(|other| fields.iter().any(|field| field.name == other.name));
    all_known && types_fit && required_present
}

impl fmt::Display for Type {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Type::Bool => write!(formatter, "Bool"),
            Type::Int => write!(formatter, "Int"),
            Type::Text => write!(formatter, "Text"),
            Type::Json => write!(formatter, "Json"),
            Type::Null => write!(formatter, "null"),
            Type::List(item) => write!(formatter, "List<{item}>"),
            Type::Record(fields) => {
                let written: Vec<String> = fields
                    .iter()
                    .map(|field| format!("{}: {}", field.name, field.field_type))
                    .collect();
                write!(formatter, "{{ {} }}", written.join(", "))
            }
            Type::Service => write!(formatter, "Service"),
            Type::Operation => write!(formatter, "Operation"),
            Type::ClientOperation => write!(formatter, "ClientOperation"),
            Type::Symbol => write!(formatter, "Symbol"),
            Type::Site => write!(formatter, "Site"),
            Type::Resolution(inner) => write!(formatter, "Resolution<{inner}>"),
            Type::Entity(address) | Type::Relation(address) => {
                write!(formatter, "{}", address.name())
            }
            Type::Fact(address) => write!(formatter, "Fact<{}>", address.name()),
            Type::Graph(address) => write!(formatter, "Graph<{}>", address.name()),
            Type::Evidence(inner) => write!(formatter, "Evidence<{inner}>"),
            Type::Builtin(name) => write!(formatter, "{name}"),
            Type::Namespace(module) => write!(formatter, "module {module}"),
            Type::Function(signature) => {
                let parameters: Vec<String> = signature
                    .parameters
                    .iter()
                    .map(|parameter| format!("{}: {}", parameter.name, parameter.parameter_type))
                    .collect();
                write!(
                    formatter,
                    "({}) -> {}",
                    parameters.join(", "),
                    signature.returns
                )
            }
            Type::Unknown => write!(formatter, "?"),
        }
    }
}

/// A value the checker computed without running anything.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Value {
    /// A boolean.
    Bool(bool),
    /// An integer.
    Int(i64),
    /// A string.
    Text(String),
    /// `null`.
    Null,
    /// A list.
    List(Vec<Value>),
    /// A record, fields in written order.
    Record(Vec<(String, Value)>),
    /// A declared service.
    Service(Address),
    /// A declared operation.
    Operation(Address),
    /// An operation a service's providers discover, named by its key or
    /// its `operationId`.
    Discovered {
        /// The service.
        service: Address,
        /// The key or name.
        key: String,
    },
    /// An entity value.
    Entity {
        /// The entity type.
        entity: Address,
        /// Its fields.
        fields: Vec<(String, Value)>,
    },
    /// A call of a standard-library function: a provider, resolution or
    /// repository request, built and not run.
    Request {
        /// The function, such as `lemma.python.file` or `symbol`.
        function: String,
        /// Its arguments by parameter name; a method's receiver is
        /// `receiver`.
        arguments: Vec<(String, Value)>,
    },
}

impl Value {
    /// The text, if it is one.
    pub fn as_text(&self) -> Option<&str> {
        match self {
            Value::Text(text) => Some(text),
            _ => None,
        }
    }

    /// A request's argument by name.
    pub fn argument(&self, name: &str) -> Option<&Value> {
        let Value::Request { arguments, .. } = self else {
            return None;
        };
        arguments
            .iter()
            .find(|(parameter, _)| parameter == name)
            .map(|(_, value)| value)
    }

    /// A record's field by name.
    pub fn field(&self, name: &str) -> Option<&Value> {
        let Value::Record(fields) = self else {
            return None;
        };
        fields
            .iter()
            .find(|(field, _)| field == name)
            .map(|(_, value)| value)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_client_operation_fits_where_an_operation_is_expected() {
        assert!(Type::ClientOperation.fits(&Type::Operation));
        assert!(!Type::Operation.fits(&Type::ClientOperation));
    }

    #[test]
    fn a_record_fits_when_its_fields_are_known_and_required_ones_present() {
        let expected = Type::Record(vec![
            RecordField {
                name: "include".to_owned(),
                field_type: Type::List(Box::new(Type::Text)),
                is_optional: false,
            },
            RecordField {
                name: "exclude".to_owned(),
                field_type: Type::List(Box::new(Type::Text)),
                is_optional: true,
            },
        ]);
        let written = Type::Record(vec![RecordField {
            name: "include".to_owned(),
            field_type: Type::List(Box::new(Type::Text)),
            is_optional: false,
        }]);
        let unknown_field = Type::Record(vec![RecordField {
            name: "includes".to_owned(),
            field_type: Type::List(Box::new(Type::Text)),
            is_optional: false,
        }]);

        assert!(written.fits(&expected));
        assert!(!unknown_field.fits(&expected));
    }

    #[test]
    fn addresses_print_module_and_path() {
        let address = Address::top("functions", "Functions").child("setup");

        assert_eq!(address.to_string(), "functions::Functions.setup");
    }
}
