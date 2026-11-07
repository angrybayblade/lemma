//! The quickstart and the tutorials, run as a reader would run them, so
//! they cannot drift from what Lemma does.
//!
//! A page is read top to bottom. An HTML comment before a fenced block says
//! what the block is, and readers never see it:
//!
//! - `<!-- file: billing/mod.lm -->` writes the block to that file;
//! - `<!-- append: mod.lm -->` adds the block to the end of that file;
//! - `<!-- after: quickstart.md -->`, alone at the top, runs that page first.
//!
//! A ```` ```console ```` block runs each `$ command` with `lemma` and `lm`
//! from this build, and compares what it prints, stdout and stderr together,
//! with the lines below it. In those lines, `[..]` matches any text within a
//! line and a line of `...` matches any number of lines. `cd` changes the
//! directory later commands run in. Other blocks, such as install steps, are
//! not run.

#![allow(
    clippy::unwrap_used,
    clippy::panic,
    reason = "a page that does not run as written should fail loudly"
)]

use std::path::{Path, PathBuf};
use std::process::Command;

/// The documentation directory.
fn docs() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("../../docs")
}

#[test]
fn the_quickstart_runs_as_written() {
    run_page("quickstart.md");
}

#[test]
fn the_links_tutorial_runs_as_written() {
    run_page("tutorials/1-links.md");
}

#[test]
fn the_requirements_tutorial_runs_as_written() {
    run_page("tutorials/2-requirements.md");
}

#[test]
fn the_allowances_tutorial_runs_as_written() {
    run_page("tutorials/3-allowances.md");
}

#[test]
fn the_behaviors_tutorial_runs_as_written() {
    run_page("tutorials/4-behaviors.md");
}

#[test]
fn the_features_tutorial_runs_as_written() {
    run_page("tutorials/5-features.md");
}

/// Runs a page, and the pages it comes after, in a fresh directory.
fn run_page(page: &str) {
    let scratch = tempfile::tempdir().unwrap();
    let mut reader = Reader::new(scratch.path());
    reader.run(page);
}

/// A reader working through pages in one directory.
struct Reader {
    root: PathBuf,
    directory: PathBuf,
    bin: PathBuf,
}

/// What a comment before a block asks for.
enum Directive {
    File(String),
    Append(String),
}

impl Reader {
    fn new(root: &Path) -> Self {
        let bin = root.join(".bin");
        std::fs::create_dir_all(&bin).unwrap();
        for (name, path) in [
            ("lemma", env!("CARGO_BIN_EXE_lemma")),
            ("lm", env!("CARGO_BIN_EXE_lm")),
        ] {
            std::os::unix::fs::symlink(path, bin.join(name)).unwrap();
        }
        let directory = root.join("work");
        std::fs::create_dir_all(&directory).unwrap();
        Self {
            root: root.to_path_buf(),
            directory,
            bin,
        }
    }

    fn run(&mut self, page: &str) {
        let text = std::fs::read_to_string(docs().join(page))
            .unwrap_or_else(|error| panic!("cannot read docs/{page}: {error}"));
        if let Some(earlier) = text.lines().next().and_then(|line| comment(line, "after:")) {
            let base = Path::new(page).parent().unwrap_or(Path::new(""));
            let earlier = normalize(&base.join(earlier));
            self.run(&earlier.to_string_lossy());
        }
        let mut directive = None;
        let mut lines = text.lines();
        while let Some(line) = lines.next() {
            if let Some(path) = comment(line, "file:") {
                directive = Some(Directive::File(path.to_owned()));
                continue;
            }
            if let Some(path) = comment(line, "append:") {
                directive = Some(Directive::Append(path.to_owned()));
                continue;
            }
            let Some(language) = line.strip_prefix("```") else {
                continue;
            };
            let block: Vec<&str> = lines.by_ref().take_while(|line| *line != "```").collect();
            self.block(page, language, &block, directive.take());
        }
    }

    fn block(&mut self, page: &str, language: &str, block: &[&str], directive: Option<Directive>) {
        let mut content = block.join("\n");
        content.push('\n');
        match directive {
            Some(Directive::File(path)) => self.write(&path, &content, false),
            Some(Directive::Append(path)) => self.write(&path, &content, true),
            None if language == "console" => self.console(page, block),
            None => {}
        }
    }

    fn write(&self, path: &str, content: &str, appends: bool) {
        let target = self.directory.join(path);
        std::fs::create_dir_all(target.parent().unwrap()).unwrap();
        let mut text = if appends {
            std::fs::read_to_string(&target).unwrap()
        } else {
            String::new()
        };
        if appends {
            text.push('\n');
        }
        text.push_str(content);
        std::fs::write(target, text).unwrap();
    }

    /// Runs each command of a console block and compares its output.
    fn console(&mut self, page: &str, block: &[&str]) {
        let mut index = 0;
        while index < block.len() {
            let Some(command) = block[index].strip_prefix("$ ") else {
                panic!(
                    "docs/{page}: expected `$ command`, found `{}`",
                    block[index]
                );
            };
            let expected: Vec<&str> = block[index + 1..]
                .iter()
                .take_while(|line| !line.starts_with("$ "))
                .copied()
                .collect();
            index += 1 + expected.len();
            let printed = self.execute(command);
            let printed: Vec<&str> = printed.lines().map(str::trim_end).collect();
            assert!(
                matches(&expected, &printed),
                "docs/{page}: `{command}` printed\n{}\nbut the page shows\n{}",
                printed.join("\n"),
                expected.join("\n")
            );
        }
    }

    fn execute(&mut self, command: &str) -> String {
        if let Some(target) = command.strip_prefix("cd ") {
            self.directory = normalize(&self.directory.join(target.trim()));
            return String::new();
        }
        let path = format!(
            "{}:{}",
            self.bin.display(),
            std::env::var("PATH").unwrap_or_default()
        );
        let output = Command::new("bash")
            .arg("-c")
            .arg(format!("{command} 2>&1"))
            .current_dir(&self.directory)
            .env("PATH", path)
            .env("HOME", &self.root)
            .env("XDG_CACHE_HOME", self.root.join(".cache"))
            .env("LEMMA_NO_DAEMON", "1")
            .env("GIT_AUTHOR_NAME", "Reader")
            .env("GIT_AUTHOR_EMAIL", "reader@example.com")
            .env("GIT_COMMITTER_NAME", "Reader")
            .env("GIT_COMMITTER_EMAIL", "reader@example.com")
            .output()
            .unwrap();
        String::from_utf8_lossy(&output.stdout).into_owned()
    }
}

/// The text of an HTML comment starting with `key`, as in
/// `<!-- file: mod.lm -->`.
fn comment<'l>(line: &'l str, key: &str) -> Option<&'l str> {
    let inner = line.trim().strip_prefix("<!--")?.strip_suffix("-->")?;
    Some(inner.trim().strip_prefix(key)?.trim())
}

/// `a/b/../c` as `a/c`.
fn normalize(path: &Path) -> PathBuf {
    let mut normalized = PathBuf::new();
    for component in path.components() {
        match component {
            std::path::Component::ParentDir => {
                normalized.pop();
            }
            std::path::Component::CurDir => {}
            other => normalized.push(other),
        }
    }
    normalized
}

/// Whether `printed` matches the lines a page shows.
fn matches(expected: &[&str], printed: &[&str]) -> bool {
    let Some((first, rest)) = expected.split_first() else {
        return printed.is_empty();
    };
    if *first == "..." {
        return (0..=printed.len()).any(|skipped| matches(rest, &printed[skipped..]));
    }
    let Some((line, printed_rest)) = printed.split_first() else {
        return false;
    };
    line_matches(first, line) && matches(rest, printed_rest)
}

/// Whether one printed line matches a shown line, `[..]` matching any text.
fn line_matches(expected: &str, printed: &str) -> bool {
    let mut pieces = expected.split("[..]");
    let Some(head) = pieces.next() else {
        return expected == printed;
    };
    let Some(mut remaining) = printed.strip_prefix(head) else {
        return false;
    };
    let pieces: Vec<&str> = pieces.collect();
    for (position, piece) in pieces.iter().enumerate() {
        let is_last = position + 1 == pieces.len();
        if is_last {
            return remaining.ends_with(piece);
        }
        let Some(found) = remaining.find(piece) else {
            return false;
        };
        remaining = &remaining[found + piece.len()..];
    }
    remaining.is_empty()
}
