//! Source files from a Git commit or a directory.
//!
//! Reading a commit straight from Git's object store needs no checkout, so
//! the base and head of a change can be read side by side from one clone.

use std::fmt::Write as _;
use std::fs;
use std::io::{BufRead, BufReader, Read, Write as _};
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};

/// A file's path, relative to the repository root, and its text.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SourceFile {
    /// Path relative to the repository root, with `/`.
    pub path: String,
    /// Contents, as UTF-8 (invalid bytes replaced).
    pub contents: String,
}

/// Why files could not be read.
#[derive(Debug, thiserror::Error)]
pub enum SourceError {
    /// Git failed, for example because the commit does not exist.
    #[error("git failed in {}: {message}", repository.display())]
    Git {
        /// The repository.
        repository: PathBuf,
        /// What git printed.
        message: String,
    },
    /// A file could not be read.
    #[error("could not read {}", path.display())]
    Io {
        /// The file or directory.
        path: PathBuf,
        /// Why.
        #[source]
        source: std::io::Error,
    },
}

/// Files under `prefixes` (all files if empty) whose extension is in
/// `extensions`, read from `commit` if given, otherwise from the working
/// directory. Sorted by path.
pub fn read_files(
    repository: &Path,
    commit: Option<&str>,
    prefixes: &[String],
    extensions: &[&str],
) -> Result<Vec<SourceFile>, SourceError> {
    let wanted = |path: &str| {
        let extension = path.rsplit_once('.').map_or("", |(_, extension)| extension);
        extensions.contains(&extension) && !path.contains("node_modules/")
    };
    let mut files = match commit {
        Some(commit) => {
            let paths: Vec<String> = git_paths(repository, commit, prefixes)?
                .into_iter()
                .filter(|path| wanted(path))
                .collect();
            git_contents(repository, commit, &paths)?
        }
        None => directory_files(repository, prefixes, &wanted)?,
    };
    files.sort_by(|left, right| left.path.cmp(&right.path));
    Ok(files)
}

/// Files whose path `wanted` accepts, anywhere in the repository, read from
/// `commit` or the working directory. Sorted by path. For configuration
/// found by name, such as every `tsconfig.json`.
pub fn read_files_where(
    repository: &Path,
    commit: Option<&str>,
    wanted: &dyn Fn(&str) -> bool,
) -> Result<Vec<SourceFile>, SourceError> {
    let wanted = |path: &str| wanted(path) && !path.contains("node_modules/");
    let mut files = match commit {
        Some(commit) => {
            let paths: Vec<String> = git_paths(repository, commit, &[])?
                .into_iter()
                .filter(|path| wanted(path))
                .collect();
            git_contents(repository, commit, &paths)?
        }
        None => directory_files(repository, &[], &wanted)?,
    };
    files.sort_by(|left, right| left.path.cmp(&right.path));
    Ok(files)
}

/// Every file's path in the repository at `commit`, or in the working
/// directory, without reading them. Sorted. The working directory is listed
/// as Git sees it, tracked and untracked files but not ignored ones, so a
/// build's output (a client generated into an ignored directory) does not
/// count as part of the repository; outside Git, every file is.
pub fn list_files(repository: &Path, commit: Option<&str>) -> Result<Vec<String>, SourceError> {
    let mut paths = match commit {
        Some(commit) => git_paths(repository, commit, &[])?,
        None => working_paths(repository)?,
    };
    paths.retain(|path| !path.contains("node_modules/"));
    paths.sort();
    Ok(paths)
}

/// The working directory's files as Git lists them, tracked and untracked
/// but not ignored, less those deleted from the disk; every file when Git
/// lists none, as for a directory a parent repository ignores.
fn working_paths(repository: &Path) -> Result<Vec<String>, SourceError> {
    let listed = git(
        repository,
        &[
            "ls-files",
            "-z",
            "--cached",
            "--others",
            "--exclude-standard",
        ],
    )
    .map(|output| {
        String::from_utf8_lossy(&output)
            .split('\0')
            .filter(|path| !path.is_empty())
            .map(str::to_owned)
            .collect::<Vec<_>>()
    })
    .unwrap_or_default();
    if listed.is_empty() {
        return Ok(directory_paths(repository, &[])?
            .into_iter()
            .map(|(_, relative)| relative)
            .collect());
    }
    // Files deleted but not staged are still in the index; Git names them
    // in one command, cheaper than checking every listed file on disk.
    let deleted: std::collections::BTreeSet<String> =
        git(repository, &["ls-files", "-z", "--deleted"])
            .map(|output| {
                String::from_utf8_lossy(&output)
                    .split('\0')
                    .filter(|path| !path.is_empty())
                    .map(str::to_owned)
                    .collect()
            })
            .unwrap_or_default();
    Ok(listed
        .into_iter()
        .filter(|path| !deleted.contains(path))
        .collect())
}

/// One file's text, from `commit` or the working directory, or `None` if
/// the file does not exist there. A file that appears or disappears between
/// two commits is normal, such as a spec added by the change.
pub fn read_file(
    repository: &Path,
    commit: Option<&str>,
    path: &str,
) -> Result<Option<String>, SourceError> {
    let Some(commit) = commit else {
        return match fs::read_to_string(repository.join(path)) {
            Ok(text) => Ok(Some(text)),
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(None),
            Err(source) => Err(SourceError::Io {
                path: repository.join(path),
                source,
            }),
        };
    };
    let listed = git(repository, &["ls-tree", "--name-only", commit, "--", path])?;
    if listed.is_empty() {
        return Ok(None);
    }
    let output = git(repository, &["show", &format!("{commit}:{path}")])?;
    Ok(Some(String::from_utf8_lossy(&output).into_owned()))
}

fn git_paths(
    repository: &Path,
    commit: &str,
    prefixes: &[String],
) -> Result<Vec<String>, SourceError> {
    let mut args = vec!["ls-tree", "-r", "--name-only", commit, "--"];
    args.extend(prefixes.iter().map(String::as_str));
    let output = git(repository, &args)?;
    Ok(String::from_utf8_lossy(&output)
        .lines()
        .map(str::to_owned)
        .collect())
}

/// Every file under the working directory at `repository`, relative to
/// it with `/`, except dependencies and Git's own files: what reading the
/// working directory reads from. Unsorted.
pub fn walk_files(repository: &Path) -> Result<Vec<String>, SourceError> {
    Ok(directory_paths(repository, &[])?
        .into_iter()
        .map(|(_, relative)| relative)
        .collect())
}

/// The files at `paths` under the working directory at `repository`.
pub fn read_from_disk(repository: &Path, paths: &[String]) -> Result<Vec<SourceFile>, SourceError> {
    paths
        .iter()
        .map(|path| {
            let full = repository.join(path);
            let bytes = fs::read(&full).map_err(|source| SourceError::Io { path: full, source })?;
            Ok(SourceFile {
                path: path.clone(),
                contents: String::from_utf8_lossy(&bytes).into_owned(),
            })
        })
        .collect()
}

/// The files at `paths` in `commit`, read with one `git cat-file --batch`;
/// paths the commit lacks are left out. In `paths`' order.
pub fn read_paths(
    repository: &Path,
    commit: &str,
    paths: &[String],
) -> Result<Vec<SourceFile>, SourceError> {
    git_contents(repository, commit, paths)
}

/// Reads many blobs with one `git cat-file --batch`.
fn git_contents(
    repository: &Path,
    commit: &str,
    paths: &[String],
) -> Result<Vec<SourceFile>, SourceError> {
    let io_error = |source| SourceError::Io {
        path: repository.to_owned(),
        source,
    };
    let mut child = Command::new("git")
        .arg("-C")
        .arg(repository)
        .args(["cat-file", "--batch"])
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .spawn()
        .map_err(io_error)?;
    let requests = paths.iter().fold(String::new(), |mut requests, path| {
        let _ = writeln!(requests, "{commit}:{path}");
        requests
    });
    let mut stdin = child
        .stdin
        .take()
        .ok_or_else(|| io_error(std::io::Error::other("no stdin")))?;
    let writer = std::thread::spawn(move || stdin.write_all(requests.as_bytes()));
    let stdout = child
        .stdout
        .take()
        .ok_or_else(|| io_error(std::io::Error::other("no stdout")))?;
    let mut reader = BufReader::new(stdout);
    let mut files = Vec::with_capacity(paths.len());
    for path in paths {
        let mut header = String::new();
        reader.read_line(&mut header).map_err(io_error)?;
        let size: Option<usize> = header
            .split_whitespace()
            .nth(2)
            .and_then(|size| size.parse().ok());
        let Some(size) = size else { continue }; // "missing"
        let mut contents = vec![0; size + 1]; // plus the trailing newline
        reader.read_exact(&mut contents).map_err(io_error)?;
        contents.pop();
        files.push(SourceFile {
            path: path.clone(),
            contents: String::from_utf8_lossy(&contents).into_owned(),
        });
    }
    let _ = writer.join();
    let _ = child.wait();
    Ok(files)
}

fn directory_files(
    repository: &Path,
    prefixes: &[String],
    wanted: &dyn Fn(&str) -> bool,
) -> Result<Vec<SourceFile>, SourceError> {
    let mut files = Vec::new();
    for (path, relative) in directory_paths(repository, prefixes)? {
        if !wanted(&relative) {
            continue;
        }
        let bytes = fs::read(&path).map_err(|source| SourceError::Io {
            path: path.clone(),
            source,
        })?;
        files.push(SourceFile {
            path: relative,
            contents: String::from_utf8_lossy(&bytes).into_owned(),
        });
    }
    Ok(files)
}

/// Every file under `prefixes` (the whole repository if empty) in the
/// working directory, with its path relative to the repository.
/// What a directory walk does with an entry.
enum Walked {
    /// Lists it.
    Directory(PathBuf),
    /// Keeps it.
    File(PathBuf),
}

/// What to do with a directory entry; `None` to skip it. Dependencies and
/// Git's own files are never source; a link to a directory is not followed.
fn walked(entry: &fs::DirEntry) -> Option<Walked> {
    let kind = entry.file_type().ok()?;
    let path = entry.path();
    if kind.is_dir() {
        let name = entry.file_name();
        let is_source = name != "node_modules" && name != ".git";
        return is_source.then_some(Walked::Directory(path));
    }
    let is_file = kind.is_file() || (kind.is_symlink() && path.is_file());
    is_file.then_some(Walked::File(path))
}

/// A file's path, and its path relative to `repository` with `/`.
fn relative_entry(repository: &Path, path: PathBuf) -> (PathBuf, String) {
    let relative = path
        .strip_prefix(repository)
        .unwrap_or(&path)
        .to_string_lossy()
        .replace('\\', "/");
    (path, relative)
}

fn directory_paths(
    repository: &Path,
    prefixes: &[String],
) -> Result<Vec<(PathBuf, String)>, SourceError> {
    let roots: Vec<PathBuf> = if prefixes.is_empty() {
        vec![repository.to_owned()]
    } else {
        prefixes
            .iter()
            .map(|prefix| repository.join(prefix))
            .collect()
    };
    let mut found = Vec::new();
    let mut pending = Vec::new();
    for root in roots {
        let metadata = fs::metadata(&root).map_err(|source| SourceError::Io {
            path: root.clone(),
            source,
        })?;
        if metadata.is_dir() {
            pending.push(root);
        } else {
            found.push(relative_entry(repository, root));
        }
    }
    // Each entry's type comes with its directory's listing, so the walk
    // needs no `stat` per file.
    while let Some(directory) = pending.pop() {
        let entries = fs::read_dir(&directory).map_err(|source| SourceError::Io {
            path: directory.clone(),
            source,
        })?;
        for entry in entries.filter_map(Result::ok) {
            match walked(&entry) {
                Some(Walked::Directory(path)) => pending.push(path),
                Some(Walked::File(path)) => found.push(relative_entry(repository, path)),
                None => {}
            }
        }
    }
    Ok(found)
}

fn git(repository: &Path, args: &[&str]) -> Result<Vec<u8>, SourceError> {
    let output = Command::new("git")
        .arg("-C")
        .arg(repository)
        .args(args)
        .stdin(Stdio::null())
        .output()
        .map_err(|source| SourceError::Io {
            path: repository.to_owned(),
            source,
        })?;
    if !output.status.success() {
        return Err(SourceError::Git {
            repository: repository.to_owned(),
            message: String::from_utf8_lossy(&output.stderr).trim().to_owned(),
        });
    }
    Ok(output.stdout)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_missing_file_is_none_not_an_error() {
        let crate_directory = Path::new(env!("CARGO_MANIFEST_DIR"));

        let missing = read_file(crate_directory, None, "no-such-spec.json").unwrap();
        let present = read_file(crate_directory, None, "Cargo.toml").unwrap();

        assert_eq!(missing, None);
        assert!(present.is_some_and(|text| text.contains("lemma-http")));
    }
}
