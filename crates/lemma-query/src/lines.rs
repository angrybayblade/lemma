//! The text of the lines answers point at, so an agent can read a
//! reference without opening its file.

use std::collections::HashMap;
use std::fs;
use std::path::Path;

/// Longer lines are cut here; a reference needs its context, not a
/// minified bundle.
const MAX_LINE_CHARS: usize = 200;

/// Reads lines of files under a root, each file at most once.
pub(crate) struct LineReader<'a> {
    root: &'a Path,
    files: HashMap<String, Option<Vec<String>>>,
}

impl<'a> LineReader<'a> {
    pub(crate) fn new(root: &'a Path) -> Self {
        Self {
            root,
            files: HashMap::new(),
        }
    }

    /// Line `line` (zero-based) of a file relative to the root, trimmed.
    /// `None` if the file cannot be read or is shorter.
    pub(crate) fn line(&mut self, path: &str, line: u32) -> Option<String> {
        let root = self.root;
        let lines = self
            .files
            .entry(path.to_owned())
            .or_insert_with(|| {
                fs::read_to_string(root.join(path))
                    .ok()
                    .map(|text| text.lines().map(str::to_owned).collect())
            })
            .as_ref()?;
        let text = lines.get(line as usize)?.trim();
        Some(cut(text))
    }
}

fn cut(text: &str) -> String {
    match text.char_indices().nth(MAX_LINE_CHARS) {
        Some((end, _)) => format!("{}…", &text[..end]),
        None => text.to_owned(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn lines_are_trimmed_and_long_ones_cut() {
        let directory = std::env::temp_dir().join(format!("lemma-lines-{}", std::process::id()));
        fs::create_dir_all(&directory).unwrap();
        let long = "x".repeat(300);
        fs::write(directory.join("a.py"), format!("  call(a)\n{long}\n")).unwrap();
        let mut reader = LineReader::new(&directory);

        let first = reader.line("a.py", 0);
        let second = reader.line("a.py", 1).unwrap();
        let missing = reader.line("a.py", 5);

        fs::remove_dir_all(&directory).unwrap();
        assert_eq!(first.as_deref(), Some("call(a)"));
        assert_eq!(second.chars().count(), MAX_LINE_CHARS + 1);
        assert_eq!(missing, None);
    }
}
