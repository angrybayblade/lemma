//! `lemma lm fmt`: rewriting `.lm` files in the canonical layout
//! (`.specs/13-lm-grammar.md` §1, Layout).

use std::path::{Path, PathBuf};
use std::process::ExitCode;

use anyhow::Context;
use lemma_lang::LineIndex;
use lemma_lang::syntax::{FormatError, format};

/// Directories a search for modules does not enter, besides hidden ones
/// such as `.git`: build output and installed packages.
const SKIPPED_DIRECTORIES: [&str; 2] = ["target", "node_modules"];

/// What became of one file.
#[derive(Debug, PartialEq, Eq)]
enum Outcome {
    /// It was already in the canonical layout.
    Unchanged,
    /// It was rewritten, or with `--check` would have been.
    Changed,
    /// It has syntax errors, so it was left alone. Each is
    /// `line:column: message`.
    Refused(Vec<String>),
}

/// Formats `paths` (the current directory when empty), or with `check`
/// lists the files that are not formatted.
pub(crate) fn run(paths: &[PathBuf], check: bool) -> anyhow::Result<ExitCode> {
    let roots = if paths.is_empty() {
        vec![PathBuf::from(".")]
    } else {
        paths.to_vec()
    };
    let files = modules(&roots)?;
    let mut has_failed = false;
    for file in &files {
        has_failed |= report(&shown(file), format_file(file, check), check);
    }
    Ok(if has_failed {
        ExitCode::FAILURE
    } else {
        ExitCode::SUCCESS
    })
}

/// Prints what needs the user's attention about one file, and returns
/// whether it fails the command.
fn report(shown: &str, outcome: anyhow::Result<Outcome>, check: bool) -> bool {
    match outcome {
        Ok(Outcome::Unchanged) => false,
        Ok(Outcome::Changed) => {
            if check {
                println!("{shown}");
            }
            check
        }
        Ok(Outcome::Refused(problems)) => {
            for problem in problems {
                eprintln!("{shown}:{problem}");
            }
            true
        }
        Err(error) => {
            eprintln!("error: {error:#}");
            true
        }
    }
}

/// A path as the user would write it: without a leading `./`.
fn shown(path: &Path) -> String {
    path.strip_prefix(".").unwrap_or(path).display().to_string()
}

/// Formats one file, writing it back unless `check` is set.
fn format_file(path: &Path, check: bool) -> anyhow::Result<Outcome> {
    let text =
        std::fs::read_to_string(path).with_context(|| format!("cannot read {}", path.display()))?;
    let formatted = match format(&text) {
        Ok(formatted) => formatted,
        Err(FormatError::Syntax { diagnostics }) => {
            let lines = LineIndex::new(&text);
            let problems = diagnostics
                .iter()
                .map(|diagnostic| {
                    let position = lines.position(diagnostic.span.start);
                    let (line, column) = (position.line + 1, position.column + 1);
                    format!("{line}:{column}: error: {}", diagnostic.message)
                })
                .collect();
            return Ok(Outcome::Refused(problems));
        }
        Err(error) => {
            return Err(error).with_context(|| format!("cannot format {}", path.display()));
        }
    };
    if formatted == text {
        return Ok(Outcome::Unchanged);
    }
    if !check {
        std::fs::write(path, formatted)
            .with_context(|| format!("cannot write {}", path.display()))?;
    }
    Ok(Outcome::Changed)
}

/// The files `roots` name: each file itself, and the `.lm` files under
/// each directory, sorted.
fn modules(roots: &[PathBuf]) -> anyhow::Result<Vec<PathBuf>> {
    let mut files = Vec::new();
    for root in roots {
        let metadata =
            std::fs::metadata(root).with_context(|| format!("cannot find {}", root.display()))?;
        if metadata.is_dir() {
            search(root, &mut files)?;
        } else {
            files.push(root.clone());
        }
    }
    files.sort();
    files.dedup();
    Ok(files)
}

/// Adds the `.lm` files under `directory` to `files`. Symbolic links are
/// not followed, so a link cannot make the search loop.
fn search(directory: &Path, files: &mut Vec<PathBuf>) -> anyhow::Result<()> {
    let entries = std::fs::read_dir(directory)
        .with_context(|| format!("cannot read {}", directory.display()))?;
    for entry in entries {
        let entry = entry.with_context(|| format!("cannot read {}", directory.display()))?;
        let path = entry.path();
        let kind = entry
            .file_type()
            .with_context(|| format!("cannot read {}", path.display()))?;
        if kind.is_dir() && !is_skipped(&entry.file_name().to_string_lossy()) {
            search(&path, files)?;
        } else if kind.is_file() && path.extension().is_some_and(|extension| extension == "lm") {
            files.push(path);
        }
    }
    Ok(())
}

fn is_skipped(directory: &str) -> bool {
    directory.starts_with('.') || SKIPPED_DIRECTORIES.contains(&directory)
}

#[cfg(test)]
mod tests {
    use super::*;

    const FORMATTED: &str = "module m\n\nlet a = 1\n";

    #[test]
    fn a_search_finds_modules_outside_hidden_and_build_directories() {
        let temporary = tempfile::tempdir().unwrap();
        let root = temporary.path();
        for directory in ["checks", ".git", "target", "node_modules/pkg"] {
            std::fs::create_dir_all(root.join(directory)).unwrap();
        }
        for file in [
            "lemma.lm",
            "checks/replay.lm",
            "checks/notes.md",
            ".git/old.lm",
            "target/copy.lm",
            "node_modules/pkg/dep.lm",
        ] {
            std::fs::write(root.join(file), FORMATTED).unwrap();
        }

        let found = modules(&[root.to_path_buf()]).unwrap();

        assert_eq!(
            found,
            [root.join("checks/replay.lm"), root.join("lemma.lm")]
        );
    }

    #[test]
    fn a_named_file_is_formatted_whatever_its_directory() {
        let temporary = tempfile::tempdir().unwrap();
        let hidden = temporary.path().join(".hidden.lm");
        std::fs::write(&hidden, FORMATTED).unwrap();

        let found = modules(std::slice::from_ref(&hidden)).unwrap();

        assert_eq!(found, [hidden]);
    }

    #[test]
    fn formatting_rewrites_a_file_in_place() {
        let temporary = tempfile::tempdir().unwrap();
        let path = temporary.path().join("lemma.lm");
        std::fs::write(&path, "module m\nlet a=1\n").unwrap();

        let outcome = format_file(&path, false).unwrap();

        assert_eq!(outcome, Outcome::Changed);
        assert_eq!(std::fs::read_to_string(&path).unwrap(), FORMATTED);
    }

    #[test]
    fn checking_leaves_an_unformatted_file_alone() {
        let temporary = tempfile::tempdir().unwrap();
        let path = temporary.path().join("lemma.lm");
        std::fs::write(&path, "module m\nlet a=1\n").unwrap();

        let outcome = format_file(&path, true).unwrap();

        assert_eq!(outcome, Outcome::Changed);
        assert_eq!(
            std::fs::read_to_string(&path).unwrap(),
            "module m\nlet a=1\n"
        );
    }

    #[test]
    fn a_formatted_file_is_unchanged() {
        let temporary = tempfile::tempdir().unwrap();
        let path = temporary.path().join("lemma.lm");
        std::fs::write(&path, FORMATTED).unwrap();

        assert_eq!(format_file(&path, false).unwrap(), Outcome::Unchanged);
    }

    #[test]
    fn a_file_with_syntax_errors_is_refused_and_left_alone() {
        let temporary = tempfile::tempdir().unwrap();
        let path = temporary.path().join("lemma.lm");
        let broken = "module m\n\nlet a =\n";
        std::fs::write(&path, broken).unwrap();

        let outcome = format_file(&path, false).unwrap();

        let Outcome::Refused(problems) = outcome else {
            panic!("expected a refusal, got {outcome:?}");
        };
        assert!(
            problems[0].starts_with("4:1: error: expected a value"),
            "{problems:?}"
        );
        assert_eq!(std::fs::read_to_string(&path).unwrap(), broken);
    }
}
