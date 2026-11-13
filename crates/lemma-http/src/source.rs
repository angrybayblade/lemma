//! Source files from a Git commit or a directory.
//!
//! Reading a commit straight from Git's object store needs no checkout, so
//! the base and head of a change can be read side by side from one clone.

use std::collections::{BTreeMap, BTreeSet};
use std::fmt::Write as _;
use std::fs;
use std::io::{BufRead, BufReader, Read, Write as _};
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::sync::{Arc, Mutex, MutexGuard, PoisonError};

/// A file's path, relative to the repository root, and its text.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SourceFile {
    /// Path relative to the repository root, with `/`.
    pub path: String,
    /// Contents, as UTF-8 (invalid bytes replaced).
    pub contents: String,
}

/// A file a listing names, with the Git blob that holds its contents when
/// the file is known to match it, so what it holds is known without
/// reading it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ListedFile {
    /// Path relative to the repository root, with `/`.
    pub path: String,
    /// The blob's object id. `None` for a file Git cannot vouch for:
    /// untracked, changed since it was staged, reached through a link, or
    /// written to disk differently from its blob.
    pub blob: Option<String>,
}

/// Listings asked for at once by [`Listings::prefetch`]: each runs several
/// Git processes.
const PARALLEL_LISTINGS: usize = 8;

/// File listings with blobs ([`list_with_blobs`]), each repository and
/// commit listed once and shared by everything one run reads.
#[derive(Default)]
pub struct Listings {
    listed: Mutex<Listed>,
}

/// A repository and a commit (`None` for the working directory).
type ListingKey = (PathBuf, Option<String>);

/// Listings by what they list. Each has a place of its own, locked while
/// it is listed, so two askers at once wait for one listing: a large
/// repository takes several Git processes a while, and many threads ask for
/// the changed repository's as a check starts.
type Listed = BTreeMap<ListingKey, Arc<Mutex<Option<Listing>>>>;

/// One listing: its files, and, of a working directory, the untracked
/// files Git named while listing it.
#[derive(Clone)]
struct Listing {
    files: Arc<Vec<ListedFile>>,
    untracked: Arc<Vec<String>>,
}

impl Listings {
    /// The files of `repository` at `commit`, or in its working directory,
    /// listed the first time they are asked for.
    pub fn get(
        &self,
        repository: &Path,
        commit: Option<&str>,
    ) -> Result<Arc<Vec<ListedFile>>, SourceError> {
        Ok(Arc::clone(&self.listing(repository, commit)?.files))
    }

    /// The untracked files, not ignored, of the working directory at
    /// `repository`, as `git ls-files --others --exclude-standard` names
    /// them: what listing it found, so asking costs no other process.
    pub fn untracked(&self, repository: &Path) -> Result<Arc<Vec<String>>, SourceError> {
        Ok(self.listing(repository, None)?.untracked)
    }

    fn listing(&self, repository: &Path, commit: Option<&str>) -> Result<Listing, SourceError> {
        let key = (repository.to_owned(), commit.map(str::to_owned));
        self.listed_once(key, || {
            let (files, untracked) = list_with_untracked(repository, commit)?;
            Ok(Listing {
                files: Arc::new(files),
                untracked: Arc::new(untracked),
            })
        })
    }

    /// The listing kept under `key`, or else what `list` gives, kept if it
    /// succeeds. Whoever asks for the same listing meanwhile waits for it.
    fn listed_once(
        &self,
        key: ListingKey,
        list: impl FnOnce() -> Result<Listing, SourceError>,
    ) -> Result<Listing, SourceError> {
        let place = Arc::clone(self.locked().entry(key).or_default());
        let mut kept = place.lock().unwrap_or_else(PoisonError::into_inner);
        if let Some(found) = kept.as_ref() {
            return Ok(found.clone());
        }
        let listed = list()?;
        *kept = Some(listed.clone());
        Ok(listed)
    }

    /// Forgets the working directory's listing of the repository at
    /// `repository`, after its files changed. Listings at commits stay.
    pub fn forget_working(&self, repository: &Path) {
        let resolved = repository.canonicalize().ok();
        self.locked().retain(|(path, commit), _| {
            let is_it =
                path == repository || resolved.is_some() && path.canonicalize().ok() == resolved;
            commit.is_some() || !is_it
        });
    }

    /// Lists each of `wanted` not listed yet, several at a time, so a run
    /// over many repositories waits for its slowest listing rather than
    /// for all of them in turn. A listing that fails is left for
    /// [`Listings::get`] to report.
    pub fn prefetch(&self, wanted: &[(PathBuf, Option<String>)]) {
        // Services of one repository ask for the same listing; listing it
        // twice at once would double the slowest work.
        let missing: BTreeSet<&ListingKey> = {
            let listed = self.locked();
            // One being listed now counts as missing: asking for it waits.
            let is_listed = |key: &ListingKey| {
                listed
                    .get(key)
                    .and_then(|place| place.try_lock().ok())
                    .is_some_and(|kept| kept.is_some())
            };
            wanted.iter().filter(|key| !is_listed(key)).collect()
        };
        if missing.is_empty() {
            return;
        }
        let queue = Mutex::new(missing.into_iter());
        // Workers take the next listing as each finishes, so one slow
        // listing holds up only its own worker.
        std::thread::scope(|scope| {
            for _ in 0..PARALLEL_LISTINGS {
                scope.spawn(|| self.list_from(&queue));
            }
        });
    }

    /// Lists what `queue` holds until it is empty.
    fn list_from<'a>(&self, queue: &Mutex<impl Iterator<Item = &'a (PathBuf, Option<String>)>>) {
        let next = || queue.lock().unwrap_or_else(PoisonError::into_inner).next();
        while let Some(wanted) = next() {
            self.list_ahead(wanted);
        }
    }

    /// Lists `wanted` ahead of its use; a failure is reported then.
    fn list_ahead(&self, (repository, commit): &(PathBuf, Option<String>)) {
        let _ = self.get(repository, commit.as_deref());
    }

    // A poisoned lock only means a listing thread panicked; the listings
    // already kept are whole.
    fn locked(&self) -> MutexGuard<'_, Listed> {
        self.listed.lock().unwrap_or_else(PoisonError::into_inner)
    }
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
    let listed = list_with_blobs(repository, commit)?;
    Ok(listed.into_iter().map(|file| file.path).collect())
}

/// [`list_files`], with each file's blob where Git knows it: every file of
/// a commit, and in the working directory each tracked file unchanged
/// since it was staged. Sorted by path.
pub fn list_with_blobs(
    repository: &Path,
    commit: Option<&str>,
) -> Result<Vec<ListedFile>, SourceError> {
    Ok(list_with_untracked(repository, commit)?.0)
}

/// [`list_with_blobs`], with the untracked files Git named in a working
/// directory (none at a commit).
fn list_with_untracked(
    repository: &Path,
    commit: Option<&str>,
) -> Result<(Vec<ListedFile>, Vec<String>), SourceError> {
    let (mut files, untracked) = match commit {
        Some(commit) => (commit_listing(repository, commit)?, Vec::new()),
        None => working_listing(repository)?,
    };
    files.retain(|file| !file.path.contains("node_modules/"));
    files.sort_by(|left, right| left.path.cmp(&right.path));
    files.dedup_by(|later, earlier| later.path == earlier.path);
    Ok((files, untracked))
}

/// Every entry of `commit`'s tree, with its blob. A submodule's commit is
/// listed too, as a path with no blob.
fn commit_listing(repository: &Path, commit: &str) -> Result<Vec<ListedFile>, SourceError> {
    // `-z`: paths as they are, not quoted when they hold non-ASCII bytes.
    let output = git(repository, &["ls-tree", "-r", "-z", commit])?;
    let mut files = Vec::new();
    for entry in String::from_utf8_lossy(&output).split('\0') {
        // `<mode> <type> <object>\t<path>`.
        let Some((described, path)) = entry.split_once('\t') else {
            continue;
        };
        let mut fields = described.split(' ').skip(1);
        let (kind, object) = (fields.next(), fields.next());
        let blob = object.filter(|_| kind == Some("blob")).map(str::to_owned);
        files.push(ListedFile {
            path: path.to_owned(),
            blob,
        });
    }
    Ok(files)
}

/// The working directory's files as Git lists them, tracked and untracked
/// but not ignored, less those deleted from the disk; every file, with no
/// blobs, when Git lists none, as for a directory a parent repository
/// ignores. With them, the untracked files Git named, nested repositories
/// left out.
fn working_listing(repository: &Path) -> Result<(Vec<ListedFile>, Vec<String>), SourceError> {
    let answers = working_state(repository);
    let tracked = &answers.tracked;
    if tracked.is_empty() {
        let files = directory_paths(repository, &[])?
            .into_iter()
            .map(|(_, relative)| unvouched(relative))
            .collect();
        return Ok((files, Vec::new()));
    }
    let changes = &answers.changes;
    let mut files = Vec::new();
    for entry in tracked {
        // `<tag> <mode> <object> <stage>\t<path>`; `S` marks a file outside
        // a sparse checkout, which is not on disk.
        let Some((described, path)) = entry.split_once('\t') else {
            continue;
        };
        let mut fields = described.split(' ');
        let (tag, mode, object, stage) =
            (fields.next(), fields.next(), fields.next(), fields.next());
        let is_deleted = changes
            .as_ref()
            .is_some_and(|changes| changes.deleted.contains(path));
        if tag == Some("S") || is_deleted {
            continue;
        }
        match mode {
            // A link is a file only if it reaches one; a link to a
            // directory brings that directory's files.
            Some("120000") => linked_paths(repository, path, &mut files),
            // Another repository's commit: not this repository's files.
            Some("160000") => {}
            _ => {
                // A conflicted file is staged more than once and matches
                // none of its blobs.
                let is_clean = stage == Some("0")
                    && changes
                        .as_ref()
                        .is_some_and(|changes| !changes.modified.contains(path));
                files.push(ListedFile {
                    path: path.to_owned(),
                    blob: object.filter(|_| is_clean).map(str::to_owned),
                });
            }
        }
    }
    for path in &answers.untracked {
        // A trailing `/` is a nested repository Git does not descend into.
        if !path.ends_with('/') {
            linked_paths(repository, path, &mut files);
        }
    }
    drop_converted_blobs(repository, &answers, &mut files);
    let untracked = answers
        .untracked
        .into_iter()
        .filter(|path| !path.ends_with('/'))
        .collect();
    Ok((files, untracked))
}

/// What Git says about a working directory, asked all at once: each
/// question is a process, and one after another they add up across many
/// repositories.
struct WorkingState {
    /// `git ls-files -s -t` entries; none outside a repository.
    tracked: Vec<String>,
    changes: Option<WorkingChanges>,
    /// Untracked files not ignored.
    untracked: Vec<String>,
    /// `git config --list`; `None` when Git could not say.
    configuration: Option<String>,
    /// The repository's own attributes file.
    info_attributes: Option<PathBuf>,
}

fn working_state(repository: &Path) -> WorkingState {
    std::thread::scope(|scope| {
        let tracked =
            scope.spawn(|| git_listing(repository, &["ls-files", "-z", "-s", "-t", "--cached"]));
        let changes = scope.spawn(|| working_changes(repository));
        let untracked = scope.spawn(|| {
            git_listing(
                repository,
                &["ls-files", "-z", "--others", "--exclude-standard"],
            )
        });
        let configuration = scope.spawn(|| {
            let output = git(repository, &["config", "--list"]).ok()?;
            Some(String::from_utf8_lossy(&output).into_owned())
        });
        let info_attributes = scope.spawn(|| {
            let output = git(repository, &["rev-parse", "--git-path", "info/attributes"]).ok()?;
            Some(repository.join(String::from_utf8_lossy(&output).trim()))
        });
        // A question whose thread failed counts as one Git could not
        // answer: no listing, and no blob trusted.
        WorkingState {
            tracked: tracked.join().unwrap_or_default(),
            changes: changes.join().ok().flatten(),
            untracked: untracked.join().unwrap_or_default(),
            configuration: configuration.join().ok().flatten(),
            info_attributes: info_attributes.join().ok().flatten(),
        }
    })
}

/// A file listed with no blob.
fn unvouched(path: String) -> ListedFile {
    ListedFile { path, blob: None }
}

/// Tracked files the working directory no longer matches.
struct WorkingChanges {
    deleted: BTreeSet<String>,
    modified: BTreeSet<String>,
}

/// What `git diff-files` finds changed since files were staged; `None`
/// when Git cannot say, so no blob is trusted. Git compares each file's
/// recorded size and times first, so this reads only files whose times
/// changed, and a file it is unsure of counts as modified.
fn working_changes(repository: &Path) -> Option<WorkingChanges> {
    let output = git(repository, &["diff-files", "-z", "--name-status"]).ok()?;
    let mut changes = WorkingChanges {
        deleted: BTreeSet::new(),
        modified: BTreeSet::new(),
    };
    let text = String::from_utf8_lossy(&output);
    let mut fields = text.split('\0');
    // `<status>\0<path>\0` pairs.
    while let (Some(status), Some(path)) = (fields.next(), fields.next()) {
        let kind = if status == "D" {
            &mut changes.deleted
        } else {
            &mut changes.modified
        };
        kind.insert(path.to_owned());
    }
    Some(changes)
}

/// Removes the blob of each file Git writes to disk differently from it:
/// through a filter (such as Git LFS), in another encoding, with `ident`
/// expanded, or with CRLF line ends. For these the blob does not say what
/// the file holds. Every blob goes when Git cannot tell.
fn drop_converted_blobs(repository: &Path, state: &WorkingState, files: &mut [ListedFile]) {
    let with_blobs: Vec<&str> = files
        .iter()
        .filter(|file| file.blob.is_some())
        .map(|file| file.path.as_str())
        .collect();
    let converted = converted_paths(repository, state, &with_blobs);
    for file in files.iter_mut() {
        let is_converted = converted
            .as_ref()
            .is_none_or(|converted| converted.contains(&file.path));
        if is_converted {
            file.blob = None;
        }
    }
}

/// The attribute names that make Git write a file differently from its
/// blob.
const CONVERTING_ATTRIBUTES: [&str; 4] = ["filter", "ident", "working-tree-encoding", "eol"];

/// The files attributes can come from: each `.gitattributes` among
/// `paths`, the repository's own `info/attributes`, and the user's and the
/// system's attributes files.
fn attribute_files(
    repository: &Path,
    configuration: &str,
    info_attributes: Option<&Path>,
    paths: &[&str],
) -> Vec<PathBuf> {
    let mut files: Vec<PathBuf> = paths
        .iter()
        .filter(|path| path.rsplit('/').next() == Some(".gitattributes"))
        .map(|path| repository.join(path))
        .collect();
    files.extend(info_attributes.map(Path::to_owned));
    let configured = configuration
        .lines()
        .find_map(|line| line.strip_prefix("core.attributesfile="));
    let user_file = match configured {
        Some(path) => Some(PathBuf::from(path)),
        None => std::env::var_os("XDG_CONFIG_HOME")
            .map(|directory| PathBuf::from(directory).join("git/attributes"))
            .or_else(|| {
                std::env::var_os("HOME")
                    .map(|home| PathBuf::from(home).join(".config/git/attributes"))
            }),
    };
    files.extend(user_file);
    files.push(PathBuf::from("/etc/gitattributes"));
    files
}

/// Whether any of `files` sets an attribute that converts; `None` when one
/// exists but cannot be read.
fn any_converting_attributes(files: &[PathBuf]) -> Option<bool> {
    for file in files {
        let text = match fs::read_to_string(file) {
            Ok(text) => text,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => continue,
            Err(_) => return None,
        };
        if sets_converting_attribute(&text) {
            return Some(true);
        }
    }
    Some(false)
}

/// Whether an attributes file sets a converting attribute on any pattern,
/// or in a macro (`[attr]lfs filter=lfs`). A line is a pattern and then
/// attributes: `name`, `name=value`, or `-name` and `!name`, which unset.
fn sets_converting_attribute(text: &str) -> bool {
    text.lines()
        .map(str::trim)
        .filter(|line| !line.is_empty() && !line.starts_with('#'))
        .flat_map(|line| line.split_whitespace().skip(1))
        .filter(|attribute| !attribute.starts_with(['-', '!']))
        .any(|attribute| {
            let name = attribute.split('=').next().unwrap_or(attribute);
            CONVERTING_ATTRIBUTES.contains(&name)
        })
}

/// Which of `paths` Git converts when writing them out; `None` when every
/// file may be converted, or Git cannot tell.
fn converted_paths(
    repository: &Path,
    state: &WorkingState,
    paths: &[&str],
) -> Option<BTreeSet<String>> {
    if paths.is_empty() {
        return Some(BTreeSet::new());
    }
    // Windows checks text files out with CRLF line ends by default.
    if cfg!(windows) {
        return None;
    }
    let configuration = state.configuration.as_deref()?;
    let converts_line_ends = configuration
        .lines()
        .any(|line| line == "core.autocrlf=true" || line == "core.eol=crlf");
    if converts_line_ends {
        return None;
    }
    // Asking Git about every file is slow in a large repository (seconds
    // at 75k files), and needless when no attributes file sets anything
    // that converts.
    let sources = attribute_files(
        repository,
        configuration,
        state.info_attributes.as_deref(),
        paths,
    );
    if !any_converting_attributes(&sources)? {
        return Some(BTreeSet::new());
    }
    let input = paths.iter().fold(String::new(), |mut input, path| {
        input.push_str(path);
        input.push('\0');
        input
    });
    let output = git_with_input(
        repository,
        &[
            &["check-attr", "-z", "--stdin"][..],
            &CONVERTING_ATTRIBUTES[..],
        ]
        .concat(),
        input.into_bytes(),
    )
    .ok()?;
    let text = String::from_utf8_lossy(&output);
    let mut fields = text.split('\0');
    let mut converted = BTreeSet::new();
    // `<path>\0<attribute>\0<value>\0` triples.
    while let (Some(path), Some(attribute), Some(value)) =
        (fields.next(), fields.next(), fields.next())
    {
        let is_given = value != "unspecified" && value != "unset";
        let converts = match attribute {
            "ident" => value == "set",
            "eol" => value == "crlf",
            _ => is_given,
        };
        if converts {
            converted.insert(path.to_owned());
        }
    }
    Some(converted)
}

/// The NUL-separated paths a `git` listing prints; none when Git fails, as
/// outside a repository.
fn git_listing(repository: &Path, args: &[&str]) -> Vec<String> {
    git(repository, args)
        .map(|output| {
            String::from_utf8_lossy(&output)
                .split('\0')
                .filter(|path| !path.is_empty())
                .map(str::to_owned)
                .collect()
        })
        .unwrap_or_default()
}

/// Adds `path` if it is a file, or the files under it, by their paths
/// through it, if it leads to a directory (a link to shared code). Neither
/// has a blob: Git stores a link's target, not what it reaches.
fn linked_paths(repository: &Path, path: &str, files: &mut Vec<ListedFile>) {
    let full = repository.join(path);
    if full.is_file() {
        files.push(unvouched(path.to_owned()));
        return;
    }
    if !full.is_dir() {
        return;
    }
    let Ok(found) = directory_paths(repository, &[path.to_owned()]) else {
        return;
    };
    files.extend(found.into_iter().map(|(_, relative)| unvouched(relative)));
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
    // One `cat-file`, which says "missing" for a path the commit lacks.
    let mut read = git_contents(repository, commit, &[path.to_owned()])?;
    Ok(read.pop().map(|file| file.contents))
}

/// Whether `path` is a file at `commit`, or in the working directory. At a
/// commit Git is asked for the object's type, not its contents: a service
/// document runs to megabytes.
pub fn file_exists(
    repository: &Path,
    commit: Option<&str>,
    path: &str,
) -> Result<bool, SourceError> {
    let Some(commit) = commit else {
        return Ok(read_file(repository, None, path)?.is_some());
    };
    let output = Command::new("git")
        .arg("-C")
        .arg(repository)
        .args(["cat-file", "-t", &format!("{commit}:{path}")])
        .stderr(Stdio::null())
        .output()
        .map_err(|source| SourceError::Io {
            path: repository.to_owned(),
            source,
        })?;
    // Git fails for a path the commit lacks; a directory's path names a
    // tree, which is not a file.
    Ok(output.status.success() && output.stdout.trim_ascii() == b"blob")
}

fn git_paths(
    repository: &Path,
    commit: &str,
    prefixes: &[String],
) -> Result<Vec<String>, SourceError> {
    // `-z`: paths as they are, not quoted when they hold non-ASCII bytes.
    let mut args = vec!["ls-tree", "-r", "-z", "--name-only", commit, "--"];
    args.extend(prefixes.iter().map(String::as_str));
    let output = git(repository, &args)?;
    Ok(String::from_utf8_lossy(&output)
        .split('\0')
        .filter(|path| !path.is_empty())
        .map(str::to_owned)
        .collect())
}

/// Whether `path` is one of `prefixes` or under one; every path is when
/// there are none.
pub fn is_under_any(path: &str, prefixes: &[String]) -> bool {
    prefixes.is_empty()
        || prefixes.iter().any(|prefix| {
            let prefix = normal_prefix(prefix);
            prefix.is_empty()
                || path == prefix
                || path
                    .strip_prefix(prefix)
                    .is_some_and(|rest| rest.starts_with('/'))
        })
}

/// A prefix as paths are listed: without a leading `./` or a trailing `/`;
/// empty for the whole repository (`.`).
fn normal_prefix(prefix: &str) -> &str {
    let mut prefix = prefix.trim_end_matches('/');
    while let Some(rest) = prefix.strip_prefix("./") {
        prefix = rest;
    }
    if prefix == "." { "" } else { prefix }
}

/// What names a listed file's contents in a cache key: its blob when Git
/// vouches for it, otherwise the contents as `read`. `None` for a file with
/// neither, one gone before it could be read.
pub fn contents_key(file: &ListedFile, read: Option<&SourceFile>) -> Option<Vec<u8>> {
    if let Some(blob) = &file.blob {
        return Some(format!("blob:{blob}").into_bytes());
    }
    let read = read?;
    let mut key = b"text:".to_vec();
    key.extend_from_slice(read.contents.as_bytes());
    Some(key)
}

/// The files at `paths` in `commit`, or in the working directory; paths
/// with no file there are left out. In `paths`' order.
pub fn read_listed(
    repository: &Path,
    commit: Option<&str>,
    paths: &[String],
) -> Result<Vec<SourceFile>, SourceError> {
    match commit {
        Some(commit) => git_contents(repository, commit, paths),
        None => read_from_disk(repository, paths),
    }
}

/// The files at `paths` under the working directory at `repository`;
/// paths with no file there are left out, as [`read_paths`] leaves out
/// what a commit lacks.
pub fn read_from_disk(repository: &Path, paths: &[String]) -> Result<Vec<SourceFile>, SourceError> {
    let mut files = Vec::new();
    for path in paths {
        let full = repository.join(path);
        let bytes = match fs::read(&full) {
            Ok(bytes) => bytes,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => continue,
            Err(source) => return Err(SourceError::Io { path: full, source }),
        };
        files.push(SourceFile {
            path: path.clone(),
            contents: String::from_utf8_lossy(&bytes).into_owned(),
        });
    }
    Ok(files)
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
    // Callers ask for what their caches lack, often nothing; starting Git
    // for no file costs a process.
    if paths.is_empty() {
        return Ok(Vec::new());
    }
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
        // `<object> <type> <size>`, or `<name> missing`.
        let mut fields = header.split_whitespace().skip(1);
        let kind = fields.next().unwrap_or_default();
        let size: Option<usize> = fields.next().and_then(|size| size.parse().ok());
        let Some(size) = size else { continue };
        let mut contents = vec![0; size + 1]; // plus the trailing newline
        reader.read_exact(&mut contents).map_err(io_error)?;
        contents.pop();
        // A directory's path names a tree, which is not a file.
        if kind != "blob" {
            continue;
        }
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
/// Git's own files are never source; a link counts as what it leads to.
fn walked(entry: &fs::DirEntry) -> Option<Walked> {
    let kind = entry.file_type().ok()?;
    let path = entry.path();
    let is_directory = kind.is_dir() || (kind.is_symlink() && path.is_dir());
    if is_directory {
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
        let metadata = match fs::metadata(&root) {
            Ok(metadata) => metadata,
            // A directory the description names and the change deleted holds
            // no files; saying so is `lemma check`'s job, not a failure.
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => continue,
            Err(source) => {
                return Err(SourceError::Io {
                    path: root.clone(),
                    source,
                });
            }
        };
        if metadata.is_dir() {
            pending.push(root);
        } else {
            found.push(relative_entry(repository, root));
        }
    }
    // Each entry's type comes with its directory's listing, so the walk
    // needs no `stat` per file. Links to directories are followed, each
    // real directory once, so a cycle of links ends.
    let mut visited = BTreeSet::new();
    while let Some(directory) = pending.pop() {
        let real = fs::canonicalize(&directory).unwrap_or_else(|_| directory.clone());
        if !visited.insert(real) {
            continue;
        }
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

/// Runs `git` with `input` on its standard input, written from another
/// thread so a large input and output cannot block each other.
fn git_with_input(
    repository: &Path,
    args: &[&str],
    input: Vec<u8>,
) -> Result<Vec<u8>, SourceError> {
    let io_error = |source| SourceError::Io {
        path: repository.to_owned(),
        source,
    };
    let mut child = Command::new("git")
        .arg("-C")
        .arg(repository)
        .args(args)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .map_err(io_error)?;
    let mut stdin = child
        .stdin
        .take()
        .ok_or_else(|| io_error(std::io::Error::other("no stdin")))?;
    let writer = std::thread::spawn(move || stdin.write_all(&input));
    let output = child.wait_with_output().map_err(io_error)?;
    let _ = writer.join();
    if !output.status.success() {
        return Err(SourceError::Git {
            repository: repository.to_owned(),
            message: String::from_utf8_lossy(&output.stderr).trim().to_owned(),
        });
    }
    Ok(output.stdout)
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
    fn prefixes_match_as_git_pathspecs_do() {
        let prefixes = |list: &[&str]| {
            list.iter()
                .map(|prefix| (*prefix).to_owned())
                .collect::<Vec<_>>()
        };

        assert!(is_under_any("src/app.ts", &prefixes(&["./src"])));
        assert!(is_under_any("src/app.ts", &prefixes(&["src/"])));
        assert!(is_under_any("src/app.ts", &prefixes(&["."])));
        assert!(is_under_any("src/app.ts", &prefixes(&["src/app.ts"])));
        assert!(!is_under_any("srcx/app.ts", &prefixes(&["src"])));
        assert!(!is_under_any("lib/app.ts", &prefixes(&["./src"])));
    }

    #[test]
    fn askers_at_once_wait_for_one_listing() {
        let listings = Listings::default();
        let times_listed = std::sync::atomic::AtomicUsize::new(0);
        let both_asking = std::sync::Barrier::new(2);
        let ask = || {
            both_asking.wait();
            let key = (PathBuf::from("repository"), None);
            listings.listed_once(key, || {
                times_listed.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
                // Long enough for the other asker to arrive meanwhile.
                std::thread::sleep(std::time::Duration::from_millis(50));
                Ok(Listing {
                    files: Arc::default(),
                    untracked: Arc::default(),
                })
            })
        };

        std::thread::scope(|scope| {
            let other = scope.spawn(ask);
            ask().unwrap();
            other.join().unwrap().unwrap();
        });

        assert_eq!(times_listed.into_inner(), 1);
    }

    #[test]
    fn a_listing_that_failed_is_asked_for_again() {
        let listings = Listings::default();
        let key = || (PathBuf::from("repository"), None);
        let failed = listings.listed_once(key(), || {
            Err(SourceError::Git {
                repository: PathBuf::from("repository"),
                message: "not now".to_owned(),
            })
        });

        let listed = listings.listed_once(key(), || {
            Ok(Listing {
                files: Arc::default(),
                untracked: Arc::default(),
            })
        });

        assert!(failed.is_err());
        assert!(listed.is_ok());
    }

    fn git_in(directory: &Path, args: &[&str]) {
        let status = Command::new("git")
            .arg("-C")
            .arg(directory)
            .args(["-c", "user.name=t", "-c", "user.email=t@t"])
            .args(["-c", "commit.gpgsign=false"])
            .args(args)
            .status()
            .unwrap();
        assert!(status.success(), "git {args:?}");
    }

    fn blobs(listed: &[ListedFile]) -> Vec<(&str, bool)> {
        listed
            .iter()
            .map(|file| (file.path.as_str(), file.blob.is_some()))
            .collect()
    }

    #[test]
    fn the_working_directory_vouches_only_for_files_unchanged_since_staged() {
        let directory = tempfile::tempdir().unwrap();
        let root = directory.path();
        for name in ["kept.ts", "edited.ts", "deleted.ts"] {
            fs::write(root.join(name), "export {}\n").unwrap();
        }
        git_in(root, &["init", "-q"]);
        git_in(root, &["add", "."]);
        git_in(root, &["commit", "-q", "-m", "base"]);
        fs::write(root.join("edited.ts"), "export const changed = 1\n").unwrap();
        fs::remove_file(root.join("deleted.ts")).unwrap();
        fs::write(root.join("new.ts"), "export {}\n").unwrap();

        let working = list_with_blobs(root, None).unwrap();
        let committed = list_with_blobs(root, Some("HEAD")).unwrap();

        assert_eq!(
            blobs(&working),
            [("edited.ts", false), ("kept.ts", true), ("new.ts", false)]
        );
        let kept_blob = |listed: &[ListedFile]| {
            listed
                .iter()
                .find(|file| file.path == "kept.ts")
                .and_then(|file| file.blob.clone())
        };
        assert_eq!(kept_blob(&working), kept_blob(&committed));
    }

    #[test]
    fn files_git_converts_on_checkout_have_no_blob() {
        let directory = tempfile::tempdir().unwrap();
        let root = directory.path();
        fs::write(root.join(".gitattributes"), "*.bin filter=lfs\n").unwrap();
        fs::write(root.join("model.bin"), "pointer\n").unwrap();
        fs::write(root.join("app.ts"), "export {}\n").unwrap();
        git_in(root, &["init", "-q"]);
        git_in(root, &["add", "."]);
        git_in(root, &["commit", "-q", "-m", "base"]);

        let working = list_with_blobs(root, None).unwrap();

        assert_eq!(
            blobs(&working),
            [
                (".gitattributes", true),
                ("app.ts", true),
                ("model.bin", false)
            ]
        );
    }

    #[test]
    fn only_attributes_that_change_what_is_written_count_as_converting() {
        let linguist = "gen/** linguist-generated=true\napps/by_filter.py -text\n";
        let lfs = "*.bin filter=lfs diff=lfs\n";
        let macro_definition = "[attr]big filter=lfs\n";
        let unset = "*.ts -eol !filter\n";

        assert!(!sets_converting_attribute(linguist));
        assert!(sets_converting_attribute(lfs));
        assert!(sets_converting_attribute(macro_definition));
        assert!(!sets_converting_attribute(unset));
    }

    #[test]
    fn a_file_exists_at_a_commit_only_as_a_file_it_holds() {
        let directory = tempfile::tempdir().unwrap();
        let root = directory.path();
        fs::create_dir_all(root.join("docs")).unwrap();
        fs::write(root.join("docs/openapi.json"), "{}\n").unwrap();
        git_in(root, &["init", "-q"]);
        git_in(root, &["add", "."]);
        git_in(root, &["commit", "-q", "-m", "base"]);

        let exists = |path: &str| file_exists(root, Some("HEAD"), path).unwrap();

        assert!(exists("docs/openapi.json"));
        assert!(!exists("docs"));
        assert!(!exists("docs/missing.json"));
    }

    #[test]
    fn a_missing_file_is_none_not_an_error() {
        let crate_directory = Path::new(env!("CARGO_MANIFEST_DIR"));

        let missing = read_file(crate_directory, None, "no-such-spec.json").unwrap();
        let present = read_file(crate_directory, None, "Cargo.toml").unwrap();

        assert_eq!(missing, None);
        assert!(present.is_some_and(|text| text.contains("lemma-http")));
    }

    #[test]
    fn reading_from_disk_leaves_out_missing_files() {
        let directory = tempfile::tempdir().unwrap();
        fs::write(directory.path().join("here.ts"), "export {}").unwrap();
        let paths = ["here.ts".to_owned(), "gone.ts".to_owned()];

        let read = read_from_disk(directory.path(), &paths).unwrap();

        let names: Vec<&str> = read.iter().map(|file| file.path.as_str()).collect();
        assert_eq!(names, ["here.ts"]);
    }

    #[cfg(unix)]
    #[test]
    fn linked_directories_are_walked_once_even_in_a_cycle() {
        let directory = tempfile::tempdir().unwrap();
        let shared = directory.path().join("shared");
        fs::create_dir(&shared).unwrap();
        fs::write(shared.join("client.ts"), "export {}").unwrap();
        let app = directory.path().join("app");
        fs::create_dir(&app).unwrap();
        std::os::unix::fs::symlink(&shared, app.join("shared")).unwrap();
        std::os::unix::fs::symlink(&app, shared.join("back")).unwrap();

        let mut walked: Vec<String> = directory_paths(&app, &[])
            .unwrap()
            .into_iter()
            .map(|(_, relative)| relative)
            .collect();
        walked.sort();

        assert_eq!(walked, ["shared/client.ts"]);
    }
}
