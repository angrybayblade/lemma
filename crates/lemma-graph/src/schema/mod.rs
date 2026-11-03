//! An operation's request or response schema, resolved to JSON Schema from
//! wherever its `lemma.yaml` says it is: a resolver's output, a JSON file,
//! or a type in code.

mod document;
mod go;
mod python;

use std::collections::BTreeMap;
use std::path::Path;

use serde_json::Value;

use crate::GraphError;
use crate::config::{Resolver, Runtime, SchemaSource};
use crate::repos::{self, Checkout};

pub use document::{at_path, from_openapi, inline_references};

/// A resolved schema, and how it was found.
#[derive(Debug, Clone, PartialEq)]
pub struct Resolved {
    /// The JSON Schema, with references inlined.
    pub schema: Value,
    /// What produced it: `resolver`, `document`, `python` or `tree-sitter`.
    pub source: &'static str,
    /// Names it uses that could not be followed; fields under them are not
    /// checked.
    pub unresolved: Vec<String>,
}

/// Which side of a message a schema describes. Most sources describe one
/// shape for both; Go structs encode and decode by different rules.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Role {
    /// What is sent.
    Sender,
    /// What receives it.
    Receiver,
}

/// What resolving needs besides the source: the repository at a commit, the
/// operation's resolver, the service's runtime, and a cache.
pub struct Context<'a> {
    /// Which side of the message the schema is for.
    pub role: Role,
    /// The repository at the commit being checked.
    pub checkout: &'a Checkout,
    /// The operation's resolver, if any.
    pub resolver: Option<&'a Resolver>,
    /// The service's interpreters and toolchains.
    pub runtime: &'a Runtime,
    /// Lemma's cache directory, for worktrees.
    pub cache: &'a Path,
    /// Resolver outputs already produced, by repository, commit and command.
    pub outputs: &'a mut BTreeMap<(String, String, String), Value>,
}

/// Resolves a schema source to JSON Schema.
pub fn resolve(source: &SchemaSource, context: &mut Context<'_>) -> Result<Resolved, GraphError> {
    match source {
        SchemaSource::Document(location) => {
            if !matches!(location.format.as_str(), "json-schema" | "openapi") {
                return Err(GraphError::Schema {
                    message: format!(
                        "unknown schema format `{}`; use `json-schema` or `openapi`",
                        location.format
                    ),
                });
            }
            let document = match (&location.file, context.resolver) {
                (Some(file), _) => read_json(context.checkout, file)?,
                (None, Some(resolver)) => run_resolver(resolver, context)?,
                (None, None) => {
                    return Err(GraphError::Schema {
                        message: format!("`{}` names neither a file nor a resolver", location.path),
                    });
                }
            };
            let found = at_path(&document, &location.path).ok_or_else(|| GraphError::Schema {
                message: format!(
                    "nothing at `{}` in the {} document",
                    location.path, location.format
                ),
            })?;
            let source = if location.file.is_some() {
                "document"
            } else {
                "resolver"
            };
            let inlined = inline_references(found, &document);
            let schema = if location.format == "openapi" {
                from_openapi(&inlined)
            } else {
                inlined
            };
            Ok(Resolved {
                schema,
                source,
                unresolved: Vec::new(),
            })
        }
        SchemaSource::Code(anchor) => code_schema(anchor, context),
    }
}

/// `file#Type`: by the language's resolver where one runs, otherwise by
/// Tree-sitter.
fn code_schema(anchor: &str, context: &mut Context<'_>) -> Result<Resolved, GraphError> {
    let (file, symbol) = anchor.split_once('#').ok_or_else(|| GraphError::Schema {
        message: format!("`{anchor}` is not a type anchor like `path/to/file.go#Type`"),
    })?;
    let extension = file.rsplit_once('.').map_or("", |(_, extension)| extension);
    match extension {
        "go" => go_schema(file, symbol, context.checkout, context.role),
        "py" => {
            let root = context.checkout.worktree(context.cache)?;
            let python = context.runtime.python.as_deref().unwrap_or("python3");
            let python = absolute_interpreter(python, &context.checkout.root);
            let schema = python::type_schema(&root, &python, file, symbol, &context.runtime.env)?;
            Ok(Resolved {
                schema: inline_references(&schema, &schema),
                source: "python",
                unresolved: Vec::new(),
            })
        }
        _ => Err(GraphError::Schema {
            message: format!("no schema reader for `.{extension}` files yet ({anchor})"),
        }),
    }
}

fn go_schema(
    file: &str,
    symbol: &str,
    checkout: &Checkout,
    role: Role,
) -> Result<Resolved, GraphError> {
    let directory = file.rsplit_once('/').map_or("", |(directory, _)| directory);
    let prefixes = if directory.is_empty() {
        Vec::new()
    } else {
        vec![directory.to_owned()]
    };
    let files = lemma_http::read_files(
        &checkout.root,
        checkout.commit.as_deref(),
        &prefixes,
        &["go"],
    )
    .map_err(|source| GraphError::Source { source })?;
    let package: Vec<(String, String)> = files
        .into_iter()
        .filter(|candidate| {
            candidate
                .path
                .rsplit_once('/')
                .map_or("", |(parent, _)| parent)
                == directory
                && !candidate.path.ends_with("_test.go")
        })
        .map(|candidate| (candidate.path, candidate.contents))
        .collect();
    let view = match role {
        Role::Sender => go::View::Encode,
        Role::Receiver => go::View::Decode,
    };
    let found = go::struct_schema(&package, symbol, view).ok_or_else(|| GraphError::Schema {
        message: format!("no type `{symbol}` in the Go package {directory}"),
    })?;
    Ok(Resolved {
        schema: found.schema,
        source: "tree-sitter",
        unresolved: found.unresolved.into_iter().collect(),
    })
}

fn read_json(checkout: &Checkout, file: &str) -> Result<Value, GraphError> {
    let text = checkout.read(file)?.ok_or_else(|| GraphError::Schema {
        message: format!("no file {file}"),
    })?;
    serde_json::from_str(&text).map_err(|source| GraphError::ResolverOutput {
        command: format!("reading {file}"),
        source,
    })
}

/// Runs the operation's resolver at the checkout once, and parses its output.
fn run_resolver(resolver: &Resolver, context: &mut Context<'_>) -> Result<Value, GraphError> {
    let key = (
        context.checkout.root.to_string_lossy().into_owned(),
        context.checkout.commit.clone().unwrap_or_default(),
        resolver.command.clone(),
    );
    if let Some(output) = context.outputs.get(&key) {
        return Ok(output.clone());
    }
    let root = context.checkout.worktree(context.cache)?;
    let command = expand_runtime(&resolver.command, context.runtime, &context.checkout.root);
    let mut env = context.runtime.env.clone();
    env.extend(resolver.env.clone());
    let stdout = repos::run(&root, &command, &env)?;
    let text = match &resolver.output {
        Some(output) => {
            std::fs::read_to_string(root.join(output)).map_err(|source| GraphError::Io {
                path: root.join(output),
                source,
            })?
        }
        None => stdout,
    };
    let document: Value =
        serde_json::from_str(&text).map_err(|source| GraphError::ResolverOutput {
            command: resolver.command.clone(),
            source,
        })?;
    context.outputs.insert(key, document.clone());
    Ok(document)
}

/// `{python}` and the like in a command, replaced by the service's runtime.
fn expand_runtime(command: &str, runtime: &Runtime, repository: &Path) -> String {
    let python = absolute_interpreter(runtime.python.as_deref().unwrap_or("python3"), repository);
    let node = absolute_interpreter(runtime.node.as_deref().unwrap_or("node"), repository);
    command
        .replace("{python}", &python)
        .replace("{node}", &node)
        .replace("{go}", runtime.go.as_deref().unwrap_or("go"))
        .replace("{cargo}", runtime.cargo.as_deref().unwrap_or("cargo"))
}

/// An interpreter given relative to the repository (`.venv/bin/python`)
/// made absolute, so a worktree elsewhere still finds the environment.
fn absolute_interpreter(interpreter: &str, repository: &Path) -> String {
    if interpreter.contains('/') && !interpreter.starts_with('/') {
        return repository.join(interpreter).to_string_lossy().into_owned();
    }
    interpreter.to_owned()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn runtime_placeholders_use_the_repository_environment() {
        let runtime = Runtime {
            python: Some(".venv/bin/python".to_owned()),
            ..Runtime::default()
        };

        let command = expand_runtime(
            "{python} spec.py && {go} version",
            &runtime,
            Path::new("/repo"),
        );

        assert_eq!(command, "/repo/.venv/bin/python spec.py && go version");
    }
}
