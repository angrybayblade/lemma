//! The standard library: modules under `lemma.`, compiled into Lemma.
//!
//! Each module has `.lm` source (relations, rules, requirements) and may
//! add built-in functions written in Rust, such as `lemma.python.file`.
//! Built-in functions build requests; nothing runs when they are called.

use crate::types::{Parameter, RecordField, Signature, Type};

/// The standard library's module sources, by name.
const MODULES: [(&str, &str); 9] = [
    ("lemma.http", include_str!("http.lm")),
    ("lemma.links", include_str!("links.lm")),
    ("lemma.python", include_str!("python.lm")),
    ("lemma.typescript", include_str!("typescript.lm")),
    ("lemma.go", include_str!("go.lm")),
    ("lemma.rust", include_str!("rust.lm")),
    ("lemma.json", include_str!("json.lm")),
    ("lemma.hono", include_str!("hono.lm")),
    ("lemma.fastapi", include_str!("fastapi.lm")),
];

/// The repository name standard-library modules belong to.
pub const REPOSITORY: &str = "lemma";

/// A standard-library module's source.
pub fn source(module: &str) -> Option<&'static str> {
    MODULES
        .iter()
        .find(|(name, _)| *name == module)
        .map(|(_, text)| *text)
}

/// Every standard-library module's name.
pub fn module_names() -> impl Iterator<Item = &'static str> {
    MODULES.iter().map(|(name, _)| *name)
}

/// A built-in function: its module (empty for the prelude every module
/// sees), its name, what it takes and returns, and what it is for.
#[derive(Debug, Clone)]
pub struct Builtin {
    /// The module exporting it; empty for the prelude.
    pub module: &'static str,
    /// Its name.
    pub name: &'static str,
    /// Its signature.
    pub signature: Signature,
    /// One sentence for hovers.
    pub documentation: &'static str,
}

/// The built-in adapters `http.adapter` names; `lemma-graph` keeps its
/// registry equal to this.
pub const ADAPTERS: [&str; 5] = [
    "hono-zod-openapi",
    "fastapi",
    "express-routes",
    "flask-routes",
    "go-http-routes",
];

/// A problem with a built-in function's known arguments, beyond their
/// types: an adapter name Lemma does not have.
pub fn argument_problem(
    function: &str,
    arguments: &[(String, crate::types::Value)],
) -> Option<String> {
    if function != "lemma.http.adapter" {
        return None;
    }
    let name = arguments
        .iter()
        .find(|(parameter, _)| parameter == "name")
        .and_then(|(_, value)| value.as_text())?;
    if ADAPTERS.contains(&name) {
        return None;
    }
    Some(format!(
        "there is no built-in adapter `{name}`; there are {}",
        ADAPTERS.join(", ")
    ))
}

/// The type of a command: a shell string or an argument list.
pub const COMMAND: Type = Type::Builtin("Command");
/// Operations a provider discovers.
pub const OPERATIONS: Type = Type::Builtin("Operations");
/// A schema's source.
pub const SCHEMA: Type = Type::Builtin("Schema");
/// A source file to resolve symbols and types in.
pub const SOURCE_FILE: Type = Type::Builtin("SourceFile");
/// A JSON document a schema is read from.
pub const DOCUMENT: Type = Type::Builtin("Document");
/// A generated client's description.
pub const CLIENT: Type = Type::Builtin("Client");
/// Where a repository comes from.
pub const REPOSITORY_SOURCE: Type = Type::Builtin("Repository");
/// Facts a provider discovers for a graph.
pub const FACTS: Type = Type::Builtin("Facts");
/// A severity name: `error`, `warning` or `info`.
pub const SEVERITY: Type = Type::Builtin("Severity");
/// A level name: `audit`, `check` or `strict`.
pub const LEVEL: Type = Type::Builtin("Level");

fn required(name: &str, parameter_type: Type) -> Parameter {
    Parameter {
        name: name.to_owned(),
        parameter_type,
        is_optional: false,
    }
}

fn optional(name: &str, parameter_type: Type) -> Parameter {
    Parameter {
        is_optional: true,
        ..required(name, parameter_type)
    }
}

fn texts() -> Type {
    Type::List(Box::new(Type::Text))
}

fn builtin(
    module: &'static str,
    name: &'static str,
    parameters: Vec<Parameter>,
    returns: Type,
    documentation: &'static str,
) -> Builtin {
    Builtin {
        module,
        name,
        signature: Signature {
            parameters,
            returns,
        },
        documentation,
    }
}

fn adapter_parameters() -> Vec<Parameter> {
    vec![
        required("entry", texts()),
        optional("directory", Type::Text),
        optional("setup", COMMAND),
        optional("env", Type::Json),
        optional("exclude", texts()),
        optional("timeout_ms", Type::Int),
    ]
}

/// Every built-in function.
pub fn builtins() -> Vec<Builtin> {
    let mut all = prelude();
    all.extend(http());
    all.extend(adapters());
    all.extend(json());
    all.extend(languages());
    all
}

fn prelude() -> Vec<Builtin> {
    vec![
        builtin(
            "",
            "local",
            vec![required("path", Type::Text)],
            REPOSITORY_SOURCE,
            "A repository's working tree, relative to the declaring file.",
        ),
        builtin(
            "",
            "git",
            vec![required("url", Type::Text), optional("rev", Type::Text)],
            REPOSITORY_SOURCE,
            "A repository cloned from `url`, read at `rev` (a commit, preferably).",
        ),
    ]
}

fn http() -> Vec<Builtin> {
    vec![
        builtin(
            "lemma.http",
            "openapi",
            vec![required("document", Type::Text)],
            OPERATIONS,
            "The operations an OpenAPI document in the repository describes.",
        ),
        builtin(
            "lemma.http",
            "generated_client",
            vec![
                required("generated_from", texts()),
                optional("specs", texts()),
                optional("call_names", texts()),
            ],
            CLIENT,
            "A client generated from OpenAPI documents, one function per operation.",
        ),
        builtin(
            "lemma.http",
            "adapter",
            [vec![required("name", Type::Text)], adapter_parameters()].concat(),
            OPERATIONS,
            "Operations one of Lemma's built-in adapters reads from the service's code, by its name.",
        ),
        builtin(
            "lemma.http",
            "adapter_command",
            [vec![required("command", COMMAND)], adapter_parameters()].concat(),
            OPERATIONS,
            "Operations a command reads from the service's code, following docs/adapters.md.",
        ),
        builtin(
            "lemma.http",
            "inferred_links",
            vec![optional("services", Type::List(Box::new(Type::Service)))],
            FACTS,
            "The links Lemma infers from calls in code to documented operations.",
        ),
    ]
}

fn adapters() -> Vec<Builtin> {
    vec![
        builtin(
            "lemma.hono",
            "app",
            adapter_parameters(),
            OPERATIONS,
            "Operations a Hono app built with @hono/zod-openapi registers.",
        ),
        builtin(
            "lemma.fastapi",
            "app",
            adapter_parameters(),
            OPERATIONS,
            "Operations a FastAPI app registers.",
        ),
    ]
}

fn json() -> Vec<Builtin> {
    vec![
        builtin(
            "lemma.json",
            "document",
            vec![required("file", Type::Text), optional("format", Type::Text)],
            DOCUMENT,
            "A JSON document in the repository; `format: \"openapi\"` converts OpenAPI 3.0's `nullable`.",
        ),
        builtin(
            "lemma.json",
            "command",
            vec![
                required("command", COMMAND),
                optional("output", Type::Text),
                optional("env", Type::Json),
                optional("cache", Type::Bool),
                optional("format", Type::Text),
            ],
            DOCUMENT,
            "A command's JSON output, produced at each snapshot and kept by the code's Git tree.",
        ),
    ]
}

fn languages() -> Vec<Builtin> {
    let mut all: Vec<Builtin> = ["lemma.python", "lemma.typescript", "lemma.go", "lemma.rust"]
        .into_iter()
        .map(|language| {
            builtin(
                language,
                "file",
                vec![required("path", Type::Text)],
                SOURCE_FILE,
                "A source file, relative to the repository root.",
            )
        })
        .collect();
    all.push(builtin(
        "lemma.python",
        "module",
        vec![required("name", Type::Text)],
        SOURCE_FILE,
        "A Python module by its dotted name, read from the file its dots name.",
    ));
    all
}

/// A method of a built-in type: its receiver type's name and the method.
pub fn method(receiver: &Type, name: &str) -> Option<Builtin> {
    if matches!(receiver, Type::Json) && name == "has" {
        return Some(builtin(
            "",
            "has",
            vec![required("name", Type::Text)],
            Type::Bool,
            "Whether a JSON object has the field, `null` included. Always a `Bool`.",
        ));
    }
    let receiver_name = match receiver {
        Type::Builtin(receiver_name) => *receiver_name,
        _ => return None,
    };
    let found = match (receiver_name, name) {
        ("SourceFile", "symbol") => builtin(
            "",
            "symbol",
            vec![required("name", Type::Text)],
            Type::Resolution(Box::new(Type::Symbol)),
            "A symbol defined in the file, such as `create_charge` or `Charges.Create`.",
        ),
        ("SourceFile", "method") => builtin(
            "",
            "method",
            vec![required("type", Type::Text), required("name", Type::Text)],
            Type::Resolution(Box::new(Type::Symbol)),
            "A method of a type defined in the file.",
        ),
        ("SourceFile", "type") => builtin(
            "",
            "type",
            vec![required("name", Type::Text)],
            SCHEMA,
            "A type defined in the file, read as a schema by its language's serializer rules.",
        ),
        ("Document", "at") => builtin(
            "",
            "at",
            vec![required("path", Type::Text)],
            SCHEMA,
            "The schema at a JSON path in the document, such as `$.create_charge.request`.",
        ),
        _ => return None,
    };
    Some(found)
}

/// The fields a service may have, and their types.
pub fn service_fields() -> Vec<RecordField> {
    vec![
        field("index", index_type()),
        field("operations", Type::Unknown),
        field("client", CLIENT),
        field("symbols", texts()),
        field(
            "routes",
            record(vec![field("register", texts()), field("define", texts())]),
        ),
        field("runtime", runtime_type()),
        field("rejects_unknown_fields", Type::Bool),
    ]
}

fn index_type() -> Type {
    record(vec![
        field("include", texts()),
        field("exclude", texts()),
        field("languages", texts()),
    ])
}

fn runtime_type() -> Type {
    record(vec![
        field("python", Type::Text),
        field("node", Type::Text),
        field("bun", Type::Text),
        field("go", Type::Text),
        field("cargo", Type::Text),
        field("env", Type::Json),
    ])
}

/// The properties an operation may have, by role.
pub fn operation_fields(is_client: bool) -> Vec<RecordField> {
    let resolution = Type::Resolution(Box::new(Type::Symbol));
    let mut fields = vec![
        field("implementation", resolution),
        field("dispatch", Type::Text),
        field("previously", texts()),
    ];
    let schemas = if is_client {
        ["sends", "accepts"]
    } else {
        ["request", "response"]
    };
    for name in schemas {
        fields.push(field(name, SCHEMA));
    }
    fields
}

/// The fields a workspace may have.
pub fn workspace_fields() -> Vec<RecordField> {
    vec![
        field("language", Type::Text),
        field(
            "config",
            record(vec![
                field("level", LEVEL),
                field("verification", Type::Json),
            ]),
        ),
    ]
}

/// An optional field of a record type.
fn field(name: &str, field_type: Type) -> RecordField {
    RecordField {
        name: name.to_owned(),
        field_type,
        is_optional: true,
    }
}

fn record(fields: Vec<RecordField>) -> Type {
    Type::Record(fields)
}

/// The names an enumerated field accepts, by its type.
pub fn enumeration(field_type: &Type) -> Option<&'static [&'static str]> {
    match field_type {
        Type::Builtin("Severity") => Some(&["error", "warning", "info"]),
        Type::Builtin("Level") => Some(&["audit", "check", "strict"]),
        _ => None,
    }
}

/// A built-in type's name, as written in type expressions.
pub fn type_named(name: &str) -> Option<Type> {
    let found = match name {
        "Bool" => Type::Bool,
        "Int" => Type::Int,
        "Text" => Type::Text,
        "Json" => Type::Json,
        "Service" => Type::Service,
        "Operation" => Type::Operation,
        "ClientOperation" => Type::ClientOperation,
        "Symbol" => Type::Symbol,
        "Site" => Type::Site,
        "Operations" => OPERATIONS,
        "Schema" => SCHEMA,
        "SourceFile" => SOURCE_FILE,
        "Document" => DOCUMENT,
        "Client" => CLIENT,
        _ => return None,
    };
    Some(found)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn every_module_source_names_itself() {
        for name in module_names() {
            let text = source(name).unwrap_or_default();

            assert!(
                text.starts_with(&format!("module {name}\n")),
                "{name} does not start with its module line"
            );
        }
    }
}
