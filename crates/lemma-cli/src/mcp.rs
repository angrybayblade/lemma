//! An MCP server, so agents can call Lemma's operations as tools.
//!
//! It speaks the Model Context Protocol over stdio: one JSON-RPC message
//! per line. Each tool is a `lemma` command with `--json`; its result is
//! that command's JSON, run in the daemon like any other command, so
//! language servers stay warm across tool calls and terminal use. Without
//! a daemon, commands run in a long-lived engine inside this process.

use std::io::{self, BufRead, Write};
use std::path::PathBuf;

use clap::Parser;
use serde_json::{Map, Value, json};

use crate::cli::Cli;
use crate::engine::{Engine, Invocation};

/// Protocol version used when the client does not ask for one.
const DEFAULT_PROTOCOL_VERSION: &str = "2025-06-18";

/// JSON-RPC error codes.
const METHOD_NOT_FOUND: i64 = -32601;
const INVALID_PARAMS: i64 = -32602;
const PARSE_ERROR: i64 = -32700;

/// Told to the agent once, when it connects.
const INSTRUCTIONS: &str = "Lemma resolves symbols in TypeScript, Python and Go with language \
servers, so it answers \"where is this used, defined or implemented\" exactly where text search \
guesses: it leaves out other symbols with the same name and mentions in comments and strings, and \
follows imports and aliases. Prefer it to grep or rg for those questions, especially for common \
names; use text search for strings, configuration and other files.\n\n\
Workflow: `search_symbols` (or `outline` for a file) gives each definition an id like `#415`; pass \
the id to `references`, `implementations` or `signature`. `definition` takes a file, line and \
column. Run `index` after editing files; it takes milliseconds. Only `found` and `empty` outcomes \
are answers; `empty` means the server saw nothing, not that the code is unused.";

/// A command's result, as the tool returns it.
struct Finished {
    stdout: String,
    stderr: String,
    exit_code: u8,
}

/// Tool names, descriptions and input schemas, as listed to the agent.
const TOOLS: &str = include_str!("mcp_tools.json");

/// Serves MCP on stdin and stdout until stdin closes. With `uses_daemon`,
/// commands run in the daemon when one can be reached.
pub(crate) fn serve(uses_daemon: bool) -> anyhow::Result<()> {
    let cwd = std::env::current_dir()?;
    let mut runner = Runner::new(cwd, uses_daemon);
    let stdin = io::stdin().lock();
    let mut stdout = io::stdout().lock();
    for line in stdin.lines() {
        let line = line?;
        if line.trim().is_empty() {
            continue;
        }
        let Some(reply) = handle_line(&line, &mut |args| runner.run(args)) else {
            continue;
        };
        serde_json::to_writer(&mut stdout, &reply)?;
        stdout.write_all(b"\n")?;
        stdout.flush()?;
    }
    runner.shutdown();
    Ok(())
}

/// Runs commands in the daemon, or in an engine of its own when there is
/// no daemon.
struct Runner {
    cwd: PathBuf,
    uses_daemon: bool,
    engine: Option<Engine>,
}

impl Runner {
    fn new(cwd: PathBuf, uses_daemon: bool) -> Self {
        Self {
            cwd,
            uses_daemon,
            engine: None,
        }
    }

    fn run(&mut self, args: Vec<String>) -> Finished {
        if self.uses_daemon
            && let Some(finished) = forward(args.clone(), self.cwd.clone())
        {
            return finished;
        }
        self.run_in_process(args)
    }

    fn run_in_process(&mut self, args: Vec<String>) -> Finished {
        let all_args = std::iter::once("lemma".to_owned()).chain(args);
        let cli = match Cli::try_parse_from(all_args) {
            Ok(cli) => cli,
            Err(error) => return failure(error.to_string()),
        };
        let invocation = Invocation {
            cwd: self.cwd.clone(),
            path_env: std::env::var_os("PATH"),
        };
        let engine = self.engine.get_or_insert_with(Engine::long_lived);
        let mut stdout = Vec::new();
        match engine.execute(cli, &invocation, &mut stdout) {
            Ok(exit_code) => Finished {
                stdout: String::from_utf8_lossy(&stdout).into_owned(),
                stderr: String::new(),
                exit_code,
            },
            Err(error) => failure(format!("error: {error:#}")),
        }
    }

    fn shutdown(self) {
        if let Some(engine) = self.engine {
            engine.shutdown();
        }
    }
}

#[cfg(unix)]
fn forward(args: Vec<String>, cwd: PathBuf) -> Option<Finished> {
    let finished = crate::daemon::forward(args, cwd)?;
    Some(Finished {
        stdout: finished.stdout,
        stderr: finished.stderr,
        exit_code: finished.exit_code,
    })
}

#[cfg(not(unix))]
fn forward(_args: Vec<String>, _cwd: PathBuf) -> Option<Finished> {
    None
}

fn failure(message: String) -> Finished {
    Finished {
        stdout: String::new(),
        stderr: message,
        exit_code: 1,
    }
}

/// Answers one incoming line. Notifications get no reply.
fn handle_line(line: &str, run: &mut dyn FnMut(Vec<String>) -> Finished) -> Option<Value> {
    let message: Value = match serde_json::from_str(line) {
        Ok(message) => message,
        Err(error) => return Some(error_reply(&Value::Null, PARSE_ERROR, &error.to_string())),
    };
    let id = message.get("id")?.clone();
    let method = message.get("method").and_then(Value::as_str).unwrap_or("");
    let params = message.get("params").cloned().unwrap_or(Value::Null);
    let result = match method {
        "initialize" => initialize(&params),
        "ping" => json!({}),
        "tools/list" => json!({ "tools": tool_definitions() }),
        "tools/call" => match call_tool(&params, run) {
            Ok(result) => result,
            Err(message) => return Some(error_reply(&id, INVALID_PARAMS, &message)),
        },
        _ => {
            let message = format!("unknown method `{method}`");
            return Some(error_reply(&id, METHOD_NOT_FOUND, &message));
        }
    };
    Some(json!({ "jsonrpc": "2.0", "id": id, "result": result }))
}

fn initialize(params: &Value) -> Value {
    let version = params
        .get("protocolVersion")
        .and_then(Value::as_str)
        .unwrap_or(DEFAULT_PROTOCOL_VERSION);
    json!({
        "protocolVersion": version,
        "capabilities": { "tools": {} },
        "serverInfo": { "name": "lemma", "version": env!("CARGO_PKG_VERSION") },
        "instructions": INSTRUCTIONS,
    })
}

fn error_reply(id: &Value, code: i64, message: &str) -> Value {
    json!({ "jsonrpc": "2.0", "id": id, "error": { "code": code, "message": message } })
}

/// Runs a tool. A command that ran is a result even when it found no
/// answer (exit codes 2 and 3), since its JSON says why; only a failed
/// command (exit code 1) is reported as an error.
fn call_tool(
    params: &Value,
    run: &mut dyn FnMut(Vec<String>) -> Finished,
) -> Result<Value, String> {
    let name = params
        .get("name")
        .and_then(Value::as_str)
        .ok_or("missing tool name")?;
    let empty = Map::new();
    let arguments = params
        .get("arguments")
        .and_then(Value::as_object)
        .unwrap_or(&empty);
    let args = command_for(name, arguments)?;
    let finished = run(args);
    let is_error = finished.exit_code == 1;
    let text = if is_error {
        finished.stderr.trim().to_owned()
    } else {
        finished.stdout
    };
    Ok(json!({
        "content": [{ "type": "text", "text": text }],
        "isError": is_error,
    }))
}

/// The `lemma` arguments a tool call stands for.
fn command_for(name: &str, arguments: &Map<String, Value>) -> Result<Vec<String>, String> {
    let mut args = vec!["--json".to_owned()];
    match name {
        "index" => {
            args.push("index".to_owned());
            args.push(optional_string(arguments, "path")?.unwrap_or_else(|| ".".to_owned()));
        }
        "search_symbols" => {
            args.extend(["symbols".to_owned(), required_string(arguments, "query")?]);
            if optional_bool(arguments, "exact")? {
                args.push("--exact".to_owned());
            }
            push_filters(&mut args, arguments)?;
            if let Some(limit) = arguments.get("limit") {
                let limit = limit.as_u64().ok_or("`limit` must be a positive integer")?;
                args.extend(["--limit".to_owned(), limit.to_string()]);
            }
        }
        "outline" => args.extend(["outline".to_owned(), required_string(arguments, "file")?]),
        "references" => {
            push_symbol_command(&mut args, "refs", arguments)?;
            if optional_bool(arguments, "include_declaration")? {
                args.push("--include-declaration".to_owned());
            }
        }
        "definition" => {
            let file = required_string(arguments, "file")?;
            let line = required_count(arguments, "line")?;
            let column = required_count(arguments, "column")?;
            args.extend(["definition".to_owned(), format!("{file}:{line}:{column}")]);
            push_refresh(&mut args, arguments)?;
        }
        "implementations" => push_symbol_command(&mut args, "implementations", arguments)?,
        "signature" => push_symbol_command(&mut args, "signature", arguments)?,
        _ => return Err(format!("unknown tool `{name}`")),
    }
    Ok(args)
}

fn push_symbol_command(
    args: &mut Vec<String>,
    command: &str,
    arguments: &Map<String, Value>,
) -> Result<(), String> {
    args.extend([command.to_owned(), required_string(arguments, "symbol")?]);
    push_filters(args, arguments)?;
    push_refresh(args, arguments)
}

fn push_filters(args: &mut Vec<String>, arguments: &Map<String, Value>) -> Result<(), String> {
    for (field, flag) in [
        ("kind", "--kind"),
        ("language", "--language"),
        ("path", "--path"),
    ] {
        if let Some(value) = optional_string(arguments, field)? {
            args.extend([flag.to_owned(), value]);
        }
    }
    Ok(())
}

fn push_refresh(args: &mut Vec<String>, arguments: &Map<String, Value>) -> Result<(), String> {
    if optional_bool(arguments, "refresh")? {
        args.push("--refresh".to_owned());
    }
    Ok(())
}

fn required_string(arguments: &Map<String, Value>, field: &str) -> Result<String, String> {
    optional_string(arguments, field)?.ok_or_else(|| format!("missing `{field}`"))
}

fn optional_string(arguments: &Map<String, Value>, field: &str) -> Result<Option<String>, String> {
    match arguments.get(field) {
        None | Some(Value::Null) => Ok(None),
        Some(Value::String(text)) => Ok(Some(text.clone())),
        Some(_) => Err(format!("`{field}` must be a string")),
    }
}

fn optional_bool(arguments: &Map<String, Value>, field: &str) -> Result<bool, String> {
    match arguments.get(field) {
        None | Some(Value::Null) => Ok(false),
        Some(Value::Bool(flag)) => Ok(*flag),
        Some(_) => Err(format!("`{field}` must be true or false")),
    }
}

/// A line or column, counted from 1.
fn required_count(arguments: &Map<String, Value>, field: &str) -> Result<u64, String> {
    arguments
        .get(field)
        .and_then(Value::as_u64)
        .filter(|count| *count >= 1)
        .ok_or_else(|| format!("`{field}` must be an integer from 1"))
}

/// The tools, from `mcp_tools.json`. The file is part of the build and
/// checked by a test, so it always parses.
fn tool_definitions() -> Value {
    serde_json::from_str(TOOLS).unwrap_or_else(|_| json!([]))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn arguments(value: &Value) -> Map<String, Value> {
        value.as_object().cloned().unwrap()
    }

    fn answered(stdout: &str, exit_code: u8) -> impl FnMut(Vec<String>) -> Finished {
        let stdout = stdout.to_owned();
        move |_| Finished {
            stdout: stdout.clone(),
            stderr: "error: no index found".to_owned(),
            exit_code,
        }
    }

    #[test]
    fn references_become_a_refs_command_with_filters() {
        let args = command_for(
            "references",
            &arguments(&json!({ "symbol": "Router.delete", "language": "python", "include_declaration": true })),
        )
        .unwrap();

        assert_eq!(
            args,
            [
                "--json",
                "refs",
                "Router.delete",
                "--language",
                "python",
                "--include-declaration"
            ]
        );
    }

    #[test]
    fn definitions_join_file_line_and_column() {
        let args = command_for(
            "definition",
            &arguments(&json!({ "file": "src/app.py", "line": 12, "column": 5 })),
        )
        .unwrap();

        assert_eq!(args, ["--json", "definition", "src/app.py:12:5"]);
    }

    #[test]
    fn missing_or_mistyped_arguments_are_rejected() {
        assert!(command_for("references", &Map::new()).is_err());
        assert!(
            command_for(
                "definition",
                &arguments(&json!({ "file": "a.py", "line": 0, "column": 1 }))
            )
            .is_err()
        );
        assert!(command_for("outline", &arguments(&json!({ "file": 3 }))).is_err());
        assert!(command_for("rename", &Map::new()).is_err());
    }

    #[test]
    fn notifications_get_no_reply() {
        let line = r#"{"jsonrpc":"2.0","method":"notifications/initialized"}"#;

        assert_eq!(handle_line(line, &mut answered("", 0)), None);
    }

    #[test]
    fn the_tool_list_parses_and_every_tool_has_a_schema() {
        let tools: Vec<Value> = serde_json::from_str(TOOLS).unwrap();

        assert!(
            tools
                .iter()
                .all(|tool| tool["inputSchema"]["type"] == "object")
        );
    }

    #[test]
    fn tools_are_listed() {
        let line = r#"{"jsonrpc":"2.0","id":1,"method":"tools/list"}"#;

        let reply = handle_line(line, &mut answered("", 0)).unwrap();

        let names: Vec<_> = reply["result"]["tools"]
            .as_array()
            .unwrap()
            .iter()
            .map(|tool| tool["name"].as_str().unwrap())
            .collect();
        assert_eq!(
            names,
            [
                "index",
                "search_symbols",
                "outline",
                "references",
                "definition",
                "implementations",
                "signature"
            ]
        );
    }

    #[test]
    fn a_command_without_an_answer_is_still_a_result() {
        let line = r#"{"jsonrpc":"2.0","id":2,"method":"tools/call","params":{"name":"signature","arguments":{"symbol":"x"}}}"#;

        let reply = handle_line(line, &mut answered("{\"state\":\"stale\"}", 3)).unwrap();

        assert_eq!(reply["result"]["isError"], false);
        assert_eq!(
            reply["result"]["content"][0]["text"],
            "{\"state\":\"stale\"}"
        );
    }

    #[test]
    fn a_failed_command_is_an_error_with_its_message() {
        let line = r#"{"jsonrpc":"2.0","id":3,"method":"tools/call","params":{"name":"outline","arguments":{"file":"a.py"}}}"#;

        let reply = handle_line(line, &mut answered("", 1)).unwrap();

        assert_eq!(reply["result"]["isError"], true);
        assert_eq!(
            reply["result"]["content"][0]["text"],
            "error: no index found"
        );
    }

    #[test]
    fn unknown_methods_are_reported() {
        let line = r#"{"jsonrpc":"2.0","id":4,"method":"resources/list"}"#;

        let reply = handle_line(line, &mut answered("", 0)).unwrap();

        assert_eq!(reply["error"]["code"], METHOD_NOT_FOUND);
    }
}
