//! Recording which Git commit a snapshot was taken from, and which source
//! files differ from it.

use std::path::Path;
use std::process::{Child, Command, Stdio};

use lemma_model::Language;

/// The commit checked out in the indexed directory.
#[derive(Debug, Clone)]
pub(crate) struct Head {
    /// The commit id.
    pub revision: String,
    /// The indexed directory's path inside the repository, ending in `/`, or
    /// empty at the repository root. Git reports paths from the repository
    /// root, so this is stripped from them.
    pub prefix: String,
}

/// `git rev-parse`, started early so it runs alongside the directory scan.
pub(crate) struct HeadProbe {
    child: Option<Child>,
}

impl HeadProbe {
    pub fn start(root: &Path) -> Self {
        Self {
            child: spawn_git(root, ["rev-parse", "--show-prefix", "HEAD"]),
        }
    }

    /// The checked-out commit, or `None` outside a repository or before the
    /// first commit.
    pub fn finish(mut self) -> Option<Head> {
        let output = self.child.take().and_then(output_of)?;
        let mut lines = output.lines();
        let prefix = lines.next()?.to_owned();
        let revision = lines.next()?.to_owned();
        Some(Head { revision, prefix })
    }
}

impl Drop for HeadProbe {
    fn drop(&mut self) {
        if let Some(child) = &mut self.child {
            // The process may already have exited; either way it is done.
            let _ = child.kill();
            let _ = child.wait();
        }
    }
}

/// Source files that differ from `HEAD`, including untracked and deleted
/// ones, relative to `root` and sorted. With `only`, Git looks at just those
/// paths, which is fast on any repository size; without it, Git checks every
/// supported source file. `None` if Git failed.
pub(crate) fn uncommitted_paths(
    root: &Path,
    head: &Head,
    only: Option<&[&str]>,
) -> Option<Vec<String>> {
    let mut args = vec![
        "status".to_owned(),
        "--porcelain".to_owned(),
        "-z".to_owned(),
        "--untracked-files=all".to_owned(),
        "--".to_owned(),
    ];
    match only {
        Some(paths) => args.extend(paths.iter().map(|path| format!(":(literal){path}"))),
        None => args.extend(source_pathspecs()),
    }
    let output = spawn_git(root, args).and_then(output_of)?;
    let mut paths = parse_status(&output, &head.prefix);
    paths.sort();
    paths.dedup();
    Some(paths)
}

/// Paths from `git status --porcelain -z`, with `prefix` removed. A rename
/// or copy lists both paths, since both differ from `HEAD`.
fn parse_status(output: &str, prefix: &str) -> Vec<String> {
    let mut paths = Vec::new();
    let mut entries = output.split('\0').filter(|entry| !entry.is_empty());
    while let Some(entry) = entries.next() {
        let Some((status, path)) = entry.split_at_checked(3) else {
            continue;
        };
        paths.push(path);
        if status.contains(['R', 'C']) {
            paths.extend(entries.next());
        }
    }
    paths
        .into_iter()
        .filter_map(|path| path.strip_prefix(prefix))
        .map(str::to_owned)
        .collect()
}

/// `*.py`, `*.ts`, and so on. Git matches these at any depth.
fn source_pathspecs() -> impl Iterator<Item = String> {
    Language::ALL
        .into_iter()
        .flat_map(Language::extensions)
        .map(|extension| format!("*.{extension}"))
}

fn spawn_git<I, S>(root: &Path, args: I) -> Option<Child>
where
    I: IntoIterator<Item = S>,
    S: AsRef<std::ffi::OsStr>,
{
    // `--no-optional-locks` keeps `status` from refreshing and locking the
    // repository's index, so Lemma never blocks or races the user's own Git
    // commands, and stopping a command early cannot leave a stale lock.
    Command::new("git")
        .arg("--no-optional-locks")
        .arg("-C")
        .arg(root)
        .args(args)
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .spawn()
        .ok()
}

/// The command's standard output without the trailing newline, or `None` if
/// it failed.
fn output_of(child: Child) -> Option<String> {
    let output = child.wait_with_output().ok()?;
    if !output.status.success() {
        return None;
    }
    let text = String::from_utf8_lossy(&output.stdout);
    Some(text.trim_end_matches('\n').to_owned())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn status_paths_drop_the_prefix_and_keep_both_sides_of_a_rename() {
        let output = " M apps/a.py\0R  apps/new.py\0apps/old.py\0?? apps/b c.ts\0";

        let paths = parse_status(output, "apps/");

        assert_eq!(paths, ["a.py", "new.py", "old.py", "b c.ts"]);
    }

    #[test]
    fn status_paths_outside_the_prefix_are_ignored() {
        let paths = parse_status(" M other/a.py\0 M apps/b.py\0", "apps/");

        assert_eq!(paths, ["b.py"]);
    }
}
