//! The commands and tools the agent skills tell an agent to use exist in this
//! build, so a renamed command or a removed option cannot leave a skill
//! giving instructions that fail.

#![allow(
    clippy::unwrap_used,
    clippy::panic,
    reason = "a skill that names something missing should fail loudly"
)]

use std::fs;
use std::io::Write;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};

/// The tools the skills name, by the name the MCP server offers them under.
const TOOLS_THE_SKILLS_NAME: [&str; 10] = [
    "index",
    "search_symbols",
    "outline",
    "references",
    "definition",
    "implementations",
    "signature",
    "graph",
    "check",
    "verify",
];

fn repository() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("../..")
}

/// Every Markdown file under `directory`, sorted.
fn markdown_files(directory: &Path) -> Vec<PathBuf> {
    let mut files = Vec::new();
    for entry in fs::read_dir(directory).unwrap() {
        let path = entry.unwrap().path();
        if path.is_dir() {
            files.extend(markdown_files(&path));
        } else if path.extension().is_some_and(|extension| extension == "md") {
            files.push(path);
        }
    }
    files.sort();
    files
}

/// The pages an agent or a reader of the skills follows.
fn skill_pages() -> Vec<PathBuf> {
    let mut pages = markdown_files(&repository().join("skills"));
    pages.push(repository().join("docs/lemma-mode.md"));
    pages
}

/// The `lemma` commands a page gives: inline code, and lines of fenced blocks.
fn commands_on(text: &str) -> Vec<String> {
    let mut commands = Vec::new();
    let mut is_fenced = false;
    for line in text.lines() {
        if line.trim_start().starts_with("```") {
            is_fenced = !is_fenced;
        } else if is_fenced {
            commands.push(line.trim().to_owned());
        } else {
            commands.extend(line.split('`').skip(1).step_by(2).map(str::to_owned));
        }
    }
    commands.retain(|command| command.starts_with("lemma "));
    commands
}

fn help_for(path: &[&str]) -> Option<String> {
    let output = Command::new(env!("CARGO_BIN_EXE_lemma"))
        .args(path)
        .arg("--help")
        .output()
        .unwrap();
    output
        .status
        .success()
        .then(|| String::from_utf8_lossy(&output.stdout).into_owned())
}

fn help_has_option(help: &str, option: &str) -> bool {
    help.split(|character: char| !(character.is_ascii_alphanumeric() || character == '-'))
        .any(|word| word == option)
}

/// What is wrong with `command`, if its subcommand or an option does not exist.
fn problem_with(command: &str) -> Option<String> {
    let words: Vec<&str> = command.split_whitespace().skip(1).collect();
    let first = *words.first()?;
    if !first
        .chars()
        .all(|character| character.is_ascii_lowercase())
    {
        return None;
    }
    let Some(mut help) = help_for(&[first]) else {
        return Some(format!("no such command: {command}"));
    };
    let path_length = words
        .iter()
        .take_while(|word| !word.starts_with(['-', '<', '\'', '#']))
        .count();
    for length in 2..=path_length {
        let Some(deeper) = help_for(&words[..length]) else {
            break;
        };
        help = deeper;
    }
    words
        .iter()
        .filter(|word| word.starts_with("--"))
        .map(|word| word.split('=').next().unwrap())
        .find(|option| !help_has_option(&help, option))
        .map(|option| format!("no option {option}: {command}"))
}

#[test]
fn every_command_the_skills_give_exists() {
    let mut problems = Vec::new();
    for page in skill_pages() {
        let text = fs::read_to_string(&page).unwrap();
        for command in commands_on(&text) {
            if let Some(problem) = problem_with(&command) {
                problems.push(format!("{}: {problem}", page.display()));
            }
        }
    }

    assert!(problems.is_empty(), "{}", problems.join("\n"));
}

#[test]
fn the_skills_do_not_mention_the_retired_configuration_file() {
    let mut pages = skill_pages();
    pages.push(repository().join("skills/lemma-verify/hooks/stop-check.py"));

    let stale: Vec<_> = pages
        .iter()
        .filter(|page| fs::read_to_string(page).unwrap().contains("lemma.yaml"))
        .collect();

    assert!(stale.is_empty(), "{stale:?}");
}

#[test]
fn every_tool_the_skills_name_is_offered() {
    let mut child = Command::new(env!("CARGO_BIN_EXE_lemma"))
        .arg("mcp")
        .env("LEMMA_NO_DAEMON", "1")
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .spawn()
        .unwrap();
    {
        let mut stdin = child.stdin.take().unwrap();
        writeln!(stdin, r#"{{"jsonrpc":"2.0","id":1,"method":"tools/list"}}"#).unwrap();
    }

    let output = child.wait_with_output().unwrap();

    let reply: serde_json::Value = serde_json::from_slice(&output.stdout).unwrap();
    let offered: Vec<_> = reply["result"]["tools"]
        .as_array()
        .unwrap()
        .iter()
        .map(|tool| tool["name"].as_str().unwrap())
        .collect();
    for tool in TOOLS_THE_SKILLS_NAME {
        assert!(offered.contains(&tool), "{tool} is not offered");
    }
}
