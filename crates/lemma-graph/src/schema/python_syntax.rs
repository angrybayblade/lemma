//! Python classes as JSON Schema, read with Tree-sitter: the fallback when
//! the repository's interpreter cannot import the type (no Pydantic, a
//! missing dependency).
//!
//! A `TypedDict`'s keys are required unless the class is `total=False` or
//! the key is `NotRequired` (`Required` overrides `total=False`). Any other
//! class with annotated fields, such as a Pydantic model or a dataclass,
//! requires the fields without a default; `Field(alias=...)` renames one.
//! An `Enum` is its string values. Base classes and nested classes in the
//! files read are followed; other names are left open (`{}`) and reported.

use std::collections::{BTreeMap, BTreeSet};

use serde_json::{Value, json};
use tree_sitter::{Node, Parser};

use super::shape::{Fields, any_of, nullable, string_enum};

/// How deep to follow named types inside each other.
const MAX_DEPTH: usize = 12;

/// A Python type's schema, and the names it uses that could not be followed.
#[derive(Debug, Clone, PartialEq)]
pub struct PythonSchema {
    /// The JSON Schema.
    pub schema: Value,
    /// Named types used but not defined in the files read.
    pub unresolved: BTreeSet<String>,
}

/// The schema of `symbol` (dotted for nested classes: `Outer.Inner`),
/// defined in one of `files`. `None` if no file defines it.
pub fn type_schema(files: &[(String, String)], symbol: &str) -> Option<PythonSchema> {
    let mut parser = Parser::new();
    parser
        .set_language(&tree_sitter_python::LANGUAGE.into())
        .ok()?;
    let trees: Vec<(tree_sitter::Tree, &str)> = files
        .iter()
        .filter_map(|(_, source)| Some((parser.parse(source, None)?, source.as_str())))
        .collect();
    let mut classes = BTreeMap::new();
    for (tree, source) in &trees {
        collect_classes(tree.root_node(), source, "", &mut classes);
    }
    let class = *classes.get(symbol)?;
    let mut reader = Reader {
        classes: &classes,
        unresolved: BTreeSet::new(),
        depth: 0,
    };
    let schema = reader.class(class);
    Some(PythonSchema {
        schema,
        unresolved: reader.unresolved,
    })
}

/// A class: its node and the source it is in.
type Class<'tree> = (Node<'tree>, &'tree str);

/// Every class under its qualified name (`Outer.Inner`) and, for nested
/// ones, its own name too where nothing else claims it.
fn collect_classes<'tree>(
    node: Node<'tree>,
    source: &'tree str,
    prefix: &str,
    out: &mut BTreeMap<String, Class<'tree>>,
) {
    let mut cursor = node.walk();
    for child in node.named_children(&mut cursor) {
        let child = if child.kind() == "decorated_definition" {
            child.child_by_field_name("definition").unwrap_or(child)
        } else {
            child
        };
        if child.kind() != "class_definition" {
            collect_classes(child, source, prefix, out);
            continue;
        }
        let Some(name) = child.child_by_field_name("name") else {
            continue;
        };
        let name = text(name, source);
        let qualified = if prefix.is_empty() {
            name.to_owned()
        } else {
            format!("{prefix}.{name}")
        };
        out.insert(qualified.clone(), (child, source));
        out.entry(name.to_owned()).or_insert((child, source));
        if let Some(body) = child.child_by_field_name("body") {
            collect_classes(body, source, &qualified, out);
        }
    }
}

struct Reader<'a, 'tree> {
    classes: &'a BTreeMap<String, Class<'tree>>,
    unresolved: BTreeSet<String>,
    depth: usize,
}

/// What a class is, from its bases.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Kind {
    TypedDict { total: bool },
    Enum,
    Fields,
}

impl Reader<'_, '_> {
    fn class(&mut self, (node, source): Class<'_>) -> Value {
        let bases = bases(node, source);
        let kind = class_kind(&bases);
        let Some(body) = node.child_by_field_name("body") else {
            return json!({});
        };
        if kind == Kind::Enum {
            return enumeration(body, source);
        }
        let mut fields = Fields::default();
        for base in bases
            .iter()
            .filter_map(|base| self.classes.get(base.name.as_str()))
        {
            let inherited = self.follow(*base);
            fields.extend_from(&inherited);
        }
        let mut cursor = body.walk();
        for statement in body.named_children(&mut cursor) {
            let Some(assignment) = statement
                .named_child(0)
                .filter(|child| child.kind() == "assignment")
            else {
                continue;
            };
            self.field(assignment, source, kind, &mut fields);
        }
        fields.into_object()
    }

    fn field(&mut self, assignment: Node<'_>, source: &str, kind: Kind, fields: &mut Fields) {
        let (Some(name), Some(annotation)) = (
            assignment.child_by_field_name("left"),
            assignment.child_by_field_name("type"),
        ) else {
            return;
        };
        let name = text(name, source);
        if name.starts_with("__") || text(annotation, source).starts_with("ClassVar") {
            return;
        }
        let annotated = self.annotation(annotation, source);
        let default = assignment.child_by_field_name("right");
        let (wire, has_default) = match default {
            Some(value) => field_call(value, source),
            None => (None, false),
        };
        let is_required = match kind {
            Kind::TypedDict { total } => annotated.required.unwrap_or(total),
            _ => !has_default,
        };
        let wire = wire.unwrap_or_else(|| name.to_owned());
        fields.insert(wire, annotated.schema, is_required);
    }

    fn follow(&mut self, class: Class<'_>) -> Value {
        if self.depth >= MAX_DEPTH {
            return json!({});
        }
        self.depth += 1;
        let schema = self.class(class);
        self.depth -= 1;
        schema
    }

    /// An annotation's schema, and whether `Required`/`NotRequired` says
    /// it must be present.
    fn annotation(&mut self, node: Node<'_>, source: &str) -> Annotated {
        let node = if node.kind() == "type" {
            node.named_child(0).unwrap_or(node)
        } else {
            node
        };
        let (name, arguments) = generic_parts(node, source);
        match last_segment(&name) {
            "Required" | "NotRequired" => {
                let inner = arguments
                    .first()
                    .map_or_else(|| json!({}), |inner| self.type_schema(*inner, source));
                Annotated {
                    schema: inner,
                    required: Some(last_segment(&name) == "Required"),
                }
            }
            _ => Annotated {
                schema: self.type_schema(node, source),
                required: None,
            },
        }
    }

    fn type_schema(&mut self, node: Node<'_>, source: &str) -> Value {
        match node.kind() {
            "type" => node
                .named_child(0)
                .map_or_else(|| json!({}), |inner| self.type_schema(inner, source)),
            "none" => json!({ "type": "null" }),
            "string" => {
                // A quoted forward reference.
                let name = text(node, source).trim_matches(['"', '\'']).to_owned();
                self.named(&name)
            }
            "binary_operator" | "union_type" => {
                let mut members = Vec::new();
                union_members(node, &mut members);
                self.union(&members, source)
            }
            "subscript" | "generic_type" => self.generic(node, source),
            "identifier" | "attribute" => self.named(text(node, source)),
            _ => json!({}),
        }
    }

    fn generic(&mut self, node: Node<'_>, source: &str) -> Value {
        let (name, arguments) = generic_parts(node, source);
        let argument = |index: usize, reader: &mut Self| {
            arguments
                .get(index)
                .map_or_else(|| json!({}), |inner| reader.type_schema(*inner, source))
        };
        match last_segment(&name) {
            "Optional" => nullable(argument(0, self)),
            "Union" => self.union(&arguments, source),
            "list" | "List" | "Sequence" | "set" | "Set" | "frozenset" | "FrozenSet" | "tuple"
            | "Tuple" | "Iterable" => json!({ "type": "array", "items": argument(0, self) }),
            "dict" | "Dict" | "Mapping" | "MutableMapping" => {
                json!({ "type": "object", "additionalProperties": argument(1, self) })
            }
            "Literal" => literal_values(&arguments, source),
            "Annotated" | "Final" | "Required" | "NotRequired" | "ReadOnly" => argument(0, self),
            other => self.named(other),
        }
    }

    fn union(&mut self, members: &[Node<'_>], source: &str) -> Value {
        let mut has_none = false;
        let mut schemas = Vec::new();
        for member in members {
            if matches!(text(*member, source), "None" | "NoneType") {
                has_none = true;
                continue;
            }
            schemas.push(self.type_schema(*member, source));
        }
        let schema = any_of(schemas);
        if has_none { nullable(schema) } else { schema }
    }

    fn named(&mut self, name: &str) -> Value {
        if let Some(known) = well_known(last_segment(name)) {
            return known;
        }
        let class = self
            .classes
            .get(name)
            .or_else(|| self.classes.get(last_segment(name)));
        let Some(&class) = class else {
            self.unresolved.insert(name.to_owned());
            return json!({});
        };
        self.follow(class)
    }
}

/// An annotation's schema, with what `Required` or `NotRequired` said.
struct Annotated {
    schema: Value,
    required: Option<bool>,
}

/// A base class: its name, and its `total=` keyword for a `TypedDict`.
struct Base {
    name: String,
    total: Option<bool>,
}

fn bases(node: Node<'_>, source: &str) -> Vec<Base> {
    let Some(list) = node.child_by_field_name("superclasses") else {
        return Vec::new();
    };
    let total = keyword_argument(list, source, "total").map(|value| value == "True");
    let mut cursor = list.walk();
    list.named_children(&mut cursor)
        .filter(|child| child.kind() != "keyword_argument")
        .map(|child| Base {
            name: last_segment(text(child, source)).to_owned(),
            total,
        })
        .collect()
}

fn class_kind(bases: &[Base]) -> Kind {
    if let Some(base) = bases.iter().find(|base| base.name == "TypedDict") {
        return Kind::TypedDict {
            total: base.total.unwrap_or(true),
        };
    }
    if bases
        .iter()
        .any(|base| matches!(base.name.as_str(), "Enum" | "StrEnum" | "IntEnum"))
    {
        return Kind::Enum;
    }
    Kind::Fields
}

/// A default value: the wire name a `Field(alias=...)` gives, and whether
/// it supplies a default (`Field(...)` and `Field()` without one do not).
fn field_call(value: Node<'_>, source: &str) -> (Option<String>, bool) {
    let is_field = value.kind() == "call"
        && value
            .child_by_field_name("function")
            .is_some_and(|function| last_segment(text(function, source)) == "Field");
    let Some(arguments) = value.child_by_field_name("arguments").filter(|_| is_field) else {
        return (None, true);
    };
    let alias = keyword_argument(arguments, source, "alias")
        .map(|alias| alias.trim_matches(['"', '\'']).to_owned());
    let mut cursor = arguments.walk();
    let positional_default = arguments
        .named_children(&mut cursor)
        .find(|child| child.kind() != "keyword_argument")
        .is_some_and(|first| text(first, source) != "...");
    let keyword_default = keyword_argument(arguments, source, "default")
        .is_some_and(|value| value != "...")
        || keyword_argument(arguments, source, "default_factory").is_some();
    (alias, positional_default || keyword_default)
}

fn keyword_argument<'s>(arguments: Node<'_>, source: &'s str, key: &str) -> Option<&'s str> {
    let mut cursor = arguments.walk();
    arguments
        .named_children(&mut cursor)
        .filter(|child| child.kind() == "keyword_argument")
        .find(|argument| {
            argument
                .child_by_field_name("name")
                .is_some_and(|name| text(name, source) == key)
        })
        .and_then(|argument| argument.child_by_field_name("value"))
        .map(|value| text(value, source))
}

/// A subscripted type's name and arguments: `Dict[str, int]` (a
/// `subscript`) or `dict[str, int]` (a `generic_type`).
fn generic_parts<'tree>(node: Node<'tree>, source: &str) -> (String, Vec<Node<'tree>>) {
    let mut cursor = node.walk();
    match node.kind() {
        "subscript" => {
            let name = node
                .child_by_field_name("value")
                .map_or_else(String::new, |value| text(value, source).to_owned());
            let arguments = node
                .children_by_field_name("subscript", &mut cursor)
                .collect();
            (name, arguments)
        }
        "generic_type" => {
            let children: Vec<Node<'tree>> = node.named_children(&mut cursor).collect();
            let name = children
                .first()
                .map_or_else(String::new, |first| text(*first, source).to_owned());
            let arguments = children
                .iter()
                .filter(|child| child.kind() == "type_parameter")
                .flat_map(|parameter| {
                    let mut cursor = parameter.walk();
                    parameter.named_children(&mut cursor).collect::<Vec<_>>()
                })
                .collect();
            (name, arguments)
        }
        _ => (String::new(), Vec::new()),
    }
}

/// Members of `A | B | C`, however the grammar nests them.
fn union_members<'tree>(node: Node<'tree>, out: &mut Vec<Node<'tree>>) {
    if !matches!(node.kind(), "binary_operator" | "union_type" | "type") {
        out.push(node);
        return;
    }
    let mut cursor = node.walk();
    for child in node.named_children(&mut cursor) {
        union_members(child, out);
    }
}

fn literal_values(arguments: &[Node<'_>], source: &str) -> Value {
    let values: Vec<String> = arguments
        .iter()
        .map(|argument| text(*argument, source))
        .filter(|value| value.starts_with(['"', '\'']))
        .map(|value| value.trim_matches(['"', '\'']).to_owned())
        .collect();
    if values.len() == arguments.len() {
        return string_enum(values);
    }
    json!({})
}

/// An `Enum`'s values, if they are all strings.
fn enumeration(body: Node<'_>, source: &str) -> Value {
    let mut values = Vec::new();
    let mut cursor = body.walk();
    for statement in body.named_children(&mut cursor) {
        let Some(value) = statement
            .named_child(0)
            .filter(|child| child.kind() == "assignment")
            .and_then(|assignment| assignment.child_by_field_name("right"))
        else {
            continue;
        };
        if value.kind() != "string" {
            return json!({});
        }
        values.push(text(value, source).trim_matches(['"', '\'']).to_owned());
    }
    string_enum(values)
}

fn well_known(name: &str) -> Option<Value> {
    let schema = match name {
        "str" | "bytes" => json!({ "type": "string" }),
        "int" => json!({ "type": "integer" }),
        "float" | "Decimal" => json!({ "type": "number" }),
        "bool" => json!({ "type": "boolean" }),
        "dict" | "Dict" => json!({ "type": "object" }),
        "list" | "List" => json!({ "type": "array" }),
        "datetime" => json!({ "type": "string", "format": "date-time" }),
        "date" => json!({ "type": "string", "format": "date" }),
        "UUID" => json!({ "type": "string", "format": "uuid" }),
        "Any" | "object" => json!({}),
        "None" => json!({ "type": "null" }),
        _ => return None,
    };
    Some(schema)
}

fn last_segment(name: &str) -> &str {
    name.rsplit('.').next().unwrap_or(name)
}

fn text<'s>(node: Node<'_>, source: &'s str) -> &'s str {
    node.utf8_text(source.as_bytes()).unwrap_or_default()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn schema(source: &str, symbol: &str) -> PythonSchema {
        type_schema(&[("m.py".to_owned(), source.to_owned())], symbol).unwrap()
    }

    #[test]
    fn typed_dicts_follow_totality_and_required_markers() {
        let source = r#"
import typing as t
import typing_extensions as te

class Function:
    class Request(te.TypedDict):
        action: str
        params: dict
        metadata: t.Dict[str, t.Any]

    class Response(te.TypedDict, total=False):
        data: dict
        error: t.Optional[str]
        status: te.Required[int]
        kind: t.Literal["a", "b"]
        meta: Metadata
"#;

        let request = schema(source, "Function.Request");
        let response = schema(source, "Function.Response");

        assert_eq!(
            request.schema["required"],
            json!(["action", "params", "metadata"])
        );
        assert_eq!(response.schema["required"], json!(["status"]));
        assert_eq!(
            response.schema["properties"]["error"]["type"],
            json!(["string", "null"])
        );
        assert_eq!(
            response.schema["properties"]["kind"],
            json!({ "type": "string", "enum": ["a", "b"] })
        );
        assert!(response.unresolved.contains("Metadata"));
    }

    #[test]
    fn models_require_fields_without_defaults_and_use_aliases() {
        let source = r#"
from enum import Enum
from pydantic import BaseModel, Field

class Status(str, Enum):
    PENDING = "pending"
    PAID = "paid"

class Base(BaseModel):
    id: str

class Charge(Base):
    amount: int
    note: str | None = None
    customer_id: str = Field(..., alias="customerId")
    tags: list[str] = Field(default_factory=list)
    status: Status
"#;

        let found = schema(source, "Charge");

        assert_eq!(
            found.schema["required"],
            json!(["id", "amount", "customerId", "status"])
        );
        let properties = &found.schema["properties"];
        assert_eq!(properties["note"]["type"], json!(["string", "null"]));
        assert_eq!(properties["tags"]["items"]["type"], "string");
        assert_eq!(properties["status"]["enum"], json!(["pending", "paid"]));
    }
}
