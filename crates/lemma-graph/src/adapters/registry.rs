//! Which adapters there are and how each runs. An adapter is a script and
//! the interpreter it runs with: one built into Lemma, named by
//! `adapter: <name>`, or one in the repository, named by its path
//! (`adapter: tools/lemma-adapter.ts`). A `command` runs as written.
//! Adding a built-in adapter is a script under `adapters/` and a row here.

use std::path::Path;

use crate::GraphError;
use crate::config::{OperationsFrom, Runtime};
use crate::schema::absolute_interpreter;

/// What runs an adapter's script.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum Interpreter {
    /// `bun run`, for TypeScript run directly.
    Bun,
    /// `node`, for JavaScript.
    Node,
    /// The service's Python.
    Python,
    /// `go run`, for a Go file inside the module whose code it reads.
    Go,
}

/// An adapter built into Lemma.
#[derive(Debug)]
pub(super) struct BuiltIn {
    /// The name `adapter:` gives.
    pub name: &'static str,
    /// Its script's file name, as written to Lemma's cache.
    pub file: &'static str,
    /// The script.
    pub text: &'static str,
    /// What runs it.
    pub interpreter: Interpreter,
}

/// The built-in adapters.
pub(super) const BUILT_IN: &[BuiltIn] = &[
    BuiltIn {
        name: "hono-zod-openapi",
        file: "hono-zod-openapi.ts",
        text: include_str!("../../adapters/hono-zod-openapi.ts"),
        interpreter: Interpreter::Bun,
    },
    BuiltIn {
        name: "fastapi",
        file: "fastapi_openapi.py",
        text: include_str!("../../adapters/fastapi_openapi.py"),
        interpreter: Interpreter::Python,
    },
    BuiltIn {
        name: "express-routes",
        file: "express-routes.ts",
        text: include_str!("../../adapters/express-routes.ts"),
        interpreter: Interpreter::Bun,
    },
    BuiltIn {
        name: "flask-routes",
        file: "flask_routes.py",
        text: include_str!("../../adapters/flask_routes.py"),
        interpreter: Interpreter::Python,
    },
    BuiltIn {
        name: "go-http-routes",
        file: "go-http-routes.go",
        text: include_str!("../../adapters/go-http-routes.go"),
        interpreter: Interpreter::Go,
    },
];

/// What an `operations_from` runs.
#[derive(Debug)]
pub(super) enum Script<'a> {
    /// A built-in adapter.
    BuiltIn(&'static BuiltIn),
    /// A script in the repository, by its path from the repository root.
    InRepository {
        path: &'a str,
        interpreter: Interpreter,
    },
    /// A command, run with `sh -c`.
    Command(&'a str),
}

impl Script<'_> {
    /// Whether it reads TypeScript, so that a change syntax can follow in
    /// TypeScript is all that can change its document: a built-in adapter
    /// run with Bun. A command may read anything.
    pub(super) fn reads_typescript(&self) -> bool {
        match self {
            Script::BuiltIn(built_in) => built_in.interpreter == Interpreter::Bun,
            Script::InRepository { interpreter, .. } => *interpreter == Interpreter::Bun,
            Script::Command(_) => false,
        }
    }

    /// What of the script decides its documents beyond the configuration
    /// and the code: a built-in script's text. A script in the repository
    /// is part of the code, so its tree already decides.
    pub(super) fn key_text(&self) -> &str {
        match self {
            Script::BuiltIn(built_in) => built_in.text,
            Script::InRepository { .. } | Script::Command(_) => "",
        }
    }
}

/// The script `from` names.
pub(super) fn script(from: &OperationsFrom) -> Result<Script<'_>, GraphError> {
    let name = match (&from.adapter, &from.command) {
        (None, Some(command)) => return Ok(Script::Command(command)),
        (Some(name), None) => name,
        _ => {
            return Err(GraphError::Adapter {
                message: "needs one of `adapter` and `command`".to_owned(),
            });
        }
    };
    if let Some(built_in) = BUILT_IN.iter().find(|built_in| built_in.name == name) {
        return Ok(Script::BuiltIn(built_in));
    }
    if let Some(interpreter) = interpreter_for(name) {
        return Ok(Script::InRepository {
            path: name,
            interpreter,
        });
    }
    let known: Vec<&str> = BUILT_IN.iter().map(|built_in| built_in.name).collect();
    Err(GraphError::Adapter {
        message: format!(
            "no adapter `{name}`: name a built-in one ({}), a script in the repository (`.ts`, `.js`, `.py` or `.go`), or give a `command`",
            known.join(", ")
        ),
    })
}

/// The interpreter a script's file extension calls for.
fn interpreter_for(path: &str) -> Option<Interpreter> {
    let extension = Path::new(path).extension()?.to_str()?.to_ascii_lowercase();
    match extension.as_str() {
        "ts" | "tsx" | "mts" | "cts" => Some(Interpreter::Bun),
        "js" | "mjs" | "cjs" => Some(Interpreter::Node),
        "py" => Some(Interpreter::Python),
        "go" => Some(Interpreter::Go),
        _ => None,
    }
}

/// The command running `script` with `interpreter`, from the service's
/// runtime; a relative interpreter is the clone's own, at `repository`.
pub(super) fn invocation(
    interpreter: Interpreter,
    script: &Path,
    runtime: &Runtime,
    repository: &Path,
) -> String {
    let script = script.display();
    match interpreter {
        Interpreter::Bun => format!("{} run {script}", runtime.bun.as_deref().unwrap_or("bun")),
        Interpreter::Node => format!("{} {script}", runtime.node.as_deref().unwrap_or("node")),
        Interpreter::Python => {
            let python = runtime.python.as_deref().unwrap_or("python3");
            format!("{} {script}", absolute_interpreter(python, repository))
        }
        Interpreter::Go => format!("{} run {script}", runtime.go.as_deref().unwrap_or("go")),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn only_adapters_reading_typescript_are_patched() {
        let hono = named("hono-zod-openapi");
        let fastapi = named("fastapi");
        let command = OperationsFrom {
            command: Some("make spec".to_owned()),
            ..OperationsFrom::default()
        };

        assert!(script(&hono).unwrap().reads_typescript());
        assert!(!script(&fastapi).unwrap().reads_typescript());
        assert!(!script(&command).unwrap().reads_typescript());
    }

    fn named(adapter: &str) -> OperationsFrom {
        OperationsFrom {
            adapter: Some(adapter.to_owned()),
            ..OperationsFrom::default()
        }
    }

    #[test]
    fn every_script_the_language_accepts_has_an_interpreter() {
        for extension in lemma_lang::stdlib::SCRIPT_EXTENSIONS {
            let path = format!("tools/adapter.{extension}");

            assert!(interpreter_for(&path).is_some(), "{path}");
        }
    }

    #[test]
    fn an_adapter_is_built_in_or_a_script_in_the_repository() {
        let built_in = named("fastapi");
        let in_repository = named("tools/lemma-express.js");
        let unknown = named("express");

        assert!(matches!(script(&built_in), Ok(Script::BuiltIn(found)) if found.name == "fastapi"));
        assert!(matches!(
            script(&in_repository),
            Ok(Script::InRepository {
                interpreter: Interpreter::Node,
                ..
            })
        ));
        assert!(script(&unknown).is_err());
    }
}
