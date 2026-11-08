//! Repositories: where each one is, which commit is read, and the files and
//! worktrees at that commit.
//!
//! A `path` source is a local clone; a `git` source is cloned into Lemma's
//! cache on first use. A commit is read from Git without a checkout; a
//! worktree is created only when a resolver has to run code at a commit.

use std::collections::{BTreeMap, BTreeSet};
use std::io::Read;
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::time::{Duration, Instant};

use crate::GraphError;

/// A repository as read for one snapshot.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Checkout {
    /// The local clone.
    pub root: PathBuf,
    /// The commit read, or `None` for the working directory.
    pub commit: Option<String>,
}

impl Checkout {
    /// A file's text at this checkout, or `None` if it does not exist.
    pub fn read(&self, path: &str) -> Result<Option<String>, GraphError> {
        lemma_http::read_file(&self.root, self.commit.as_deref(), path)
            .map_err(|source| GraphError::Source { source })
    }

    /// A directory holding the repository's files at this checkout: the
    /// clone itself for the working directory, otherwise a worktree in
    /// `cache`, created once per commit. Adding one removes all but the
    /// most recently used [`KEPT_WORKTREES`].
    pub fn worktree(&self, cache: &Path) -> Result<PathBuf, GraphError> {
        let Some(commit) = &self.commit else {
            return Ok(self.root.clone());
        };
        let worktrees = cache.join("worktrees");
        let name = format!("{}-{commit}", directory_name(&self.root));
        let directory = worktrees.join(&name);
        if directory.exists() {
            mark_used(&worktrees, &name);
            return Ok(directory);
        }
        std::fs::create_dir_all(&worktrees).map_err(|source| GraphError::Io {
            path: cache.to_owned(),
            source,
        })?;
        let target = directory.to_string_lossy().into_owned();
        git(
            &self.root,
            &["worktree", "add", "--detach", "--quiet", &target, commit],
        )?;
        mark_used(&worktrees, &name);
        remove_unused_worktrees(&worktrees, KEPT_WORKTREES);
        Ok(directory)
    }
}

/// How many worktrees at past commits the cache keeps. Each is a full
/// checkout, so a monorepo's can take gigabytes, and a check at a new base
/// adds one.
pub const KEPT_WORKTREES: usize = 8;

/// Where the last use of each worktree is recorded, beside them so a
/// worktree's own files stay as Git checked them out.
const USES: &str = ".used";

/// Records that the worktree `name` was just used. Failing to only makes it
/// look older.
fn mark_used(worktrees: &Path, name: &str) {
    let uses = worktrees.join(USES);
    let _ = std::fs::create_dir_all(&uses);
    let _ = std::fs::write(uses.join(name), "");
}

/// Removes all but the `kept` most recently used worktrees in `worktrees`,
/// through Git so their clones forget them. A worktree Git cannot remove
/// (its clone is gone) is deleted outright.
fn remove_unused_worktrees(worktrees: &Path, kept: usize) {
    let Ok(entries) = std::fs::read_dir(worktrees) else {
        return;
    };
    let marks = worktrees.join(USES);
    let mut found: Vec<(std::time::SystemTime, PathBuf, String)> = entries
        .filter_map(Result::ok)
        .filter(|entry| entry.path().is_dir() && entry.file_name() != USES)
        .map(|entry| {
            let name = entry.file_name().to_string_lossy().into_owned();
            let used = std::fs::metadata(marks.join(&name))
                .or_else(|_| entry.metadata())
                .and_then(|metadata| metadata.modified())
                .unwrap_or(std::time::UNIX_EPOCH);
            (used, entry.path(), name)
        })
        .collect();
    found.sort_by(|left, right| right.cmp(left));
    for (_, path, name) in found.into_iter().skip(kept) {
        let target = path.to_string_lossy().into_owned();
        let removed = git(&path, &["worktree", "remove", "--force", &target]);
        if removed.is_err() || path.exists() {
            let _ = std::fs::remove_dir_all(&path);
        }
        let _ = std::fs::remove_file(marks.join(name));
    }
}

/// The one worktree per repository that code at a commit is run in (an
/// adapter's build, a schema resolver), reused from commit to commit and
/// held for the length of a run so two Lemmas do not use it at once.
#[derive(Debug)]
pub(crate) struct SharedWorktree {
    path: PathBuf,
    lock: PathBuf,
}

impl SharedWorktree {
    /// The build worktree of the clone at `root`, checked out at `commit`.
    /// What earlier builds installed, ignored by Git, stays.
    pub(crate) fn check_out(root: &Path, commit: &str, cache: &Path) -> Result<Self, GraphError> {
        let builds = cache.join("builds");
        std::fs::create_dir_all(&builds).map_err(|source| GraphError::Io {
            path: builds.clone(),
            source,
        })?;
        let path = builds.join(directory_name(root));
        let lock = path.with_extension("lock");
        acquire(&lock, &path)?;
        let held = Self { path, lock };
        let target = held.path.to_string_lossy().into_owned();
        if held.path.join(".git").exists() {
            git(
                &held.path,
                &["checkout", "--detach", "--force", "--quiet", commit],
            )?;
            git(&held.path, &["clean", "-fd", "--quiet"])?;
        } else {
            // A worktree Git lost track of is pruned before adding it again.
            git(root, &["worktree", "prune"])?;
            git(
                root,
                &["worktree", "add", "--detach", "--quiet", &target, commit],
            )?;
        }
        Ok(held)
    }

    pub(crate) fn path(&self) -> &Path {
        &self.path
    }
}

impl Drop for SharedWorktree {
    fn drop(&mut self) {
        // A lock left behind names a process that is gone, which the next
        // run notices and takes the lock over from.
        let _ = std::fs::remove_file(&self.lock);
    }
}

/// How long a run waits for another Lemma using the shared worktree.
const LOCK_WAIT: Duration = Duration::from_secs(1_800);

/// Takes the lock file at `lock`, which holds its owner's process id:
/// waiting while another living process holds it, and taking over one
/// whose process is gone (a run that was killed). Gives up after
/// [`LOCK_WAIT`].
fn acquire(lock: &Path, worktree: &Path) -> Result<(), GraphError> {
    let started = Instant::now();
    loop {
        match std::fs::File::create_new(lock) {
            Ok(mut file) => {
                use std::io::Write as _;
                return write!(file, "{}", std::process::id()).map_err(|source| GraphError::Io {
                    path: lock.to_owned(),
                    source,
                });
            }
            Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => {}
            Err(source) => {
                return Err(GraphError::Io {
                    path: lock.to_owned(),
                    source,
                });
            }
        }
        let holder = std::fs::read_to_string(lock)
            .ok()
            .and_then(|text| text.trim().parse::<u32>().ok());
        if holder.is_some_and(|pid| !is_running(pid)) {
            // The holder died without removing it; a failed removal is
            // retried on the next pass.
            let _ = std::fs::remove_file(lock);
            continue;
        }
        if started.elapsed() > LOCK_WAIT {
            return Err(GraphError::BuildInProgress {
                directory: worktree.to_owned(),
            });
        }
        std::thread::sleep(Duration::from_millis(200));
    }
}

/// Whether a process is running, as `kill -0` reports it; taken as running
/// where that cannot be asked.
fn is_running(pid: u32) -> bool {
    if !cfg!(unix) {
        return true;
    }
    Command::new("kill")
        .args(["-0", &pid.to_string()])
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status()
        .map_or(true, |status| status.success())
}

/// Removes a worktree of the clone at `root`, made by [`Checkout::worktree`].
pub fn remove_worktree(root: &Path, worktree: &Path) -> Result<(), GraphError> {
    let target = worktree.to_string_lossy().into_owned();
    git(root, &["worktree", "remove", "--force", &target])?;
    Ok(())
}

/// The commit a reference names in a clone.
pub fn resolve_commit(root: &Path, reference: &str) -> Result<String, GraphError> {
    let spec = format!("{reference}^{{commit}}");
    let output = git(root, &["rev-parse", "--verify", "--quiet", &spec]).map_err(|_| {
        GraphError::UnknownReference {
            directory: root.to_owned(),
            reference: reference.to_owned(),
        }
    })?;
    Ok(output.trim().to_owned())
}

/// Where the checked-out commit left the repository's default branch: its
/// merge base with the first default branch found (see
/// [`default_branches`]). On the default branch itself that is the
/// checked-out commit, so only uncommitted work is the change.
///
/// Without one that shares its history, the checked-out commit's first
/// parent when it is a merge, as CI checks a pull request out; else the
/// checked-out commit when there is uncommitted work to check, and an
/// error otherwise, since the change would be empty and pass unchecked.
pub fn fork_point(root: &Path) -> Result<String, GraphError> {
    for candidate in default_branches(root) {
        if resolve_commit(root, &candidate).is_err() {
            continue;
        }
        if let Ok(base) = git(root, &["merge-base", "HEAD", &candidate]) {
            return Ok(base.trim().to_owned());
        }
    }
    if resolve_commit(root, "HEAD^2").is_ok() {
        return resolve_commit(root, "HEAD^1");
    }
    let head = resolve_commit(root, "HEAD")?;
    let status = git(root, &["status", "--porcelain"])?;
    if !status.trim().is_empty() {
        return Ok(head);
    }
    Err(GraphError::NoForkPoint {
        directory: root.to_owned(),
    })
}

/// The branches that may be the repository's default, most telling first:
/// what each remote's `HEAD` names, then `main`, `master`, `develop`,
/// `trunk` and `init.defaultBranch` on each remote (`origin` first), then
/// the same locally.
fn default_branches(root: &Path) -> Vec<String> {
    let lines = |args: &[&str]| -> Vec<String> {
        git(root, args)
            .unwrap_or_default()
            .lines()
            .map(str::trim)
            .filter(|line| !line.is_empty())
            .map(str::to_owned)
            .collect()
    };
    let mut names: Vec<String> = ["main", "master", "develop", "trunk"]
        .map(str::to_owned)
        .to_vec();
    names.extend(lines(&["config", "--get", "init.defaultBranch"]));
    let mut remotes = lines(&["remote"]);
    remotes.sort_by_key(|remote| remote != "origin");
    let mut branches = lines(&["for-each-ref", "--format=%(symref)", "refs/remotes/*/HEAD"]);
    for remote in &remotes {
        branches.extend(names.iter().map(|name| format!("{remote}/{name}")));
    }
    branches.extend(names);
    branches
}

/// Every file Git tracks at `checkout`, and in a working directory only
/// those still there; `None` where it is no repository.
pub(crate) fn tracked_files(checkout: &Checkout) -> Option<BTreeSet<String>> {
    if let Some(commit) = &checkout.commit {
        let listed = git(&checkout.root, &["ls-tree", "-r", "--name-only", commit]).ok()?;
        return Some(listed.lines().map(str::to_owned).collect());
    }
    let listed = git(&checkout.root, &["ls-files"]).ok()?;
    let is_present = |file: &&str| checkout.root.join(file).exists();
    Some(
        listed
            .lines()
            .filter(is_present)
            .map(str::to_owned)
            .collect(),
    )
}

/// Whether `url` at `reference` is in the clone cache.
pub fn is_cloned(url: &str, reference: Option<&str>, cache: &Path) -> bool {
    let directory = cache.join("repos").join(sanitize(url));
    directory.exists() && resolve_commit(&directory, reference.unwrap_or("HEAD")).is_ok()
}

/// Whether a command may clone or fetch a repository it does not have.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum Cloning {
    /// It may.
    #[default]
    Allowed,
    /// It works with what is in the clone cache, and says what is missing.
    Forbidden,
}

/// A repository named by `git(...)` that is not in the clone cache and may
/// not be fetched.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, serde::Serialize)]
pub struct UnavailableRepository {
    /// Its name, as loading names it: `#billing`.
    pub name: String,
    /// Where it can be cloned from.
    pub url: String,
    /// The commit, branch or tag it is pinned to.
    pub reference: Option<String>,
}

/// The local clone of a `git` source, cloned into `cache` on first use and
/// fetched when `reference` is not there yet.
pub fn clone_remote(
    url: &str,
    reference: Option<&str>,
    cache: &Path,
) -> Result<PathBuf, GraphError> {
    let directory = cache.join("repos").join(sanitize(url));
    if !directory.exists() {
        std::fs::create_dir_all(cache.join("repos")).map_err(|source| GraphError::Io {
            path: cache.to_owned(),
            source,
        })?;
        let target = directory.to_string_lossy().into_owned();
        git(cache, &["clone", "--quiet", "--no-checkout", url, &target])?;
    }
    let wanted = reference.unwrap_or("HEAD");
    if resolve_commit(&directory, wanted).is_err() {
        git(&directory, &["fetch", "--quiet", "origin", wanted])?;
    }
    Ok(directory)
}

/// Runs a command in `directory` with `sh -c`, returning its standard output.
pub fn run(
    directory: &Path,
    command: &str,
    env: &BTreeMap<String, String>,
) -> Result<String, GraphError> {
    run_within(directory, command, env, None)
}

/// [`run`], killing the command and everything it started if it takes
/// longer than `timeout_ms`: code that waits on a database at import would
/// otherwise hang a check for good.
pub fn run_within(
    directory: &Path,
    command: &str,
    env: &BTreeMap<String, String>,
    timeout_ms: Option<u64>,
) -> Result<String, GraphError> {
    let mut process = Command::new("sh");
    process.arg("-c").arg(command);
    run_process(process, command, directory, env, timeout_ms)
}

/// Runs a program with its arguments, without a shell, in `directory` with
/// `env` added; its standard output if it succeeds within `timeout_ms`.
pub fn run_program(
    directory: &Path,
    program: &[String],
    env: &BTreeMap<String, String>,
    timeout_ms: Option<u64>,
) -> Result<String, GraphError> {
    let Some((name, arguments)) = program.split_first() else {
        return Err(GraphError::Resolver {
            command: String::new(),
            directory: directory.to_owned(),
            message: "the command is empty".to_owned(),
        });
    };
    let mut process = Command::new(name);
    process.args(arguments);
    run_process(process, &program.join(" "), directory, env, timeout_ms)
}

/// Runs `process` (described as `command`) in its own process group, so a
/// timeout stops what it started too.
fn run_process(
    mut process: Command,
    command: &str,
    directory: &Path,
    env: &BTreeMap<String, String>,
    timeout_ms: Option<u64>,
) -> Result<String, GraphError> {
    let io_error = |source| GraphError::Io {
        path: directory.to_owned(),
        source,
    };
    process
        .current_dir(directory)
        .envs(env)
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    #[cfg(unix)]
    std::os::unix::process::CommandExt::process_group(&mut process, 0);
    let mut child = process.spawn().map_err(io_error)?;
    let stdout = read_in_background(child.stdout.take());
    let stderr = read_in_background(child.stderr.take());
    let started = Instant::now();
    let status = loop {
        if let Some(status) = child.try_wait().map_err(io_error)? {
            break Some(status);
        }
        let elapsed_ms = u64::try_from(started.elapsed().as_millis()).unwrap_or(u64::MAX);
        if timeout_ms.is_some_and(|limit| elapsed_ms > limit) {
            kill_group(&mut child);
            break None;
        }
        std::thread::sleep(Duration::from_millis(20));
    };
    let stdout = stdout.join().unwrap_or_default();
    let stderr = stderr.join().unwrap_or_default();
    let failure = |message: String| GraphError::Resolver {
        command: command.to_owned(),
        directory: directory.to_owned(),
        message,
    };
    let Some(status) = status else {
        return Err(failure(format!(
            "stopped after {} ms without finishing",
            timeout_ms.unwrap_or_default()
        )));
    };
    if !status.success() {
        return Err(failure(failure_message(&stdout, &stderr)));
    }
    Ok(String::from_utf8_lossy(&stdout).into_owned())
}

/// What a failed command said: its standard error, or the end of its
/// standard output, where some tools (pnpm) report failures.
fn failure_message(stdout: &[u8], stderr: &[u8]) -> String {
    let stderr = String::from_utf8_lossy(stderr).trim().to_owned();
    if !stderr.is_empty() {
        return stderr;
    }
    let stdout = String::from_utf8_lossy(stdout);
    let lines: Vec<&str> = stdout.trim().lines().collect();
    lines[lines.len().saturating_sub(20)..].join("\n")
}

fn read_in_background(
    pipe: Option<impl Read + Send + 'static>,
) -> std::thread::JoinHandle<Vec<u8>> {
    std::thread::spawn(move || {
        let mut read = Vec::new();
        if let Some(mut pipe) = pipe {
            // A pipe that fails part way leaves what was read, which is
            // only shown in a failure message.
            let _ = pipe.read_to_end(&mut read);
        }
        read
    })
}

/// Kills a command started in its own process group, and what it started.
fn kill_group(child: &mut Child) {
    #[cfg(unix)]
    {
        let group = format!("-{}", child.id());
        let _ = Command::new("kill").args(["-KILL", "--", &group]).status();
    }
    let _ = child.kill();
    let _ = child.wait();
}

/// Lemma's cache directory: `$XDG_CACHE_HOME/lemma` or `~/.cache/lemma`.
pub fn cache_directory() -> PathBuf {
    if let Some(cache) = std::env::var_os("XDG_CACHE_HOME") {
        return PathBuf::from(cache).join("lemma");
    }
    if let Some(home) = std::env::var_os("HOME") {
        return PathBuf::from(home).join(".cache").join("lemma");
    }
    // Windows names no `HOME`; never the current directory, which is
    // usually a repository Lemma would then read its own cache from.
    std::env::var_os("LOCALAPPDATA")
        .map_or_else(std::env::temp_dir, PathBuf::from)
        .join("lemma")
}

pub(crate) fn git(directory: &Path, args: &[&str]) -> Result<String, GraphError> {
    // `--no-optional-locks` keeps `status` from rewriting the repository's
    // index: the rewrite would block the user's own Git commands, and the
    // index watching `.git` would take it for a new commit and re-read the
    // head on the next query.
    let output = Command::new("git")
        .arg("--no-optional-locks")
        .arg("-C")
        .arg(directory)
        .args(args)
        .output()
        .map_err(|source| GraphError::Io {
            path: directory.to_owned(),
            source,
        })?;
    if !output.status.success() {
        return Err(GraphError::Git {
            directory: directory.to_owned(),
            message: String::from_utf8_lossy(&output.stderr).trim().to_owned(),
        });
    }
    Ok(String::from_utf8_lossy(&output.stdout).into_owned())
}

/// The Git directory a clone and all its worktrees share.
pub fn git_common_directory(root: &Path) -> Result<PathBuf, GraphError> {
    let common = git(root, &["rev-parse", "--git-common-dir"])?;
    Ok(root.join(common.trim()))
}

pub(crate) fn directory_name(path: &Path) -> String {
    sanitize(&path.to_string_lossy())
}

/// A string as a safe directory name.
fn sanitize(text: &str) -> String {
    text.chars()
        .map(|c| {
            if c.is_ascii_alphanumeric() || c == '-' || c == '.' {
                c
            } else {
                '_'
            }
        })
        .collect::<String>()
        .trim_matches('_')
        .to_owned()
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A repository on `main` with one commit, made with a fixed author.
    fn repository() -> tempfile::TempDir {
        let directory = tempfile::tempdir().unwrap();
        let root = directory.path();
        git(root, &["init", "-q", "-b", "main"]).unwrap();
        commit(root, "first");
        directory
    }

    fn commit(root: &Path, message: &str) {
        std::fs::write(root.join("file.txt"), message).unwrap();
        git(root, &["add", "."]).unwrap();
        git(
            root,
            &[
                "-c",
                "user.name=t",
                "-c",
                "user.email=t@t",
                "commit",
                "-q",
                "-m",
                message,
            ],
        )
        .unwrap();
    }

    #[test]
    fn a_branch_forks_where_it_left_main() {
        let directory = repository();
        let root = directory.path();
        let main = resolve_commit(root, "main").unwrap();
        git(root, &["checkout", "-q", "-b", "feature"]).unwrap();
        commit(root, "second");
        commit(root, "third");

        let fork = fork_point(root).unwrap();

        assert_eq!(fork, main);
    }

    #[test]
    fn a_clean_checkout_with_no_default_branch_has_no_fork_point() {
        let directory = repository();
        let root = directory.path();
        git(root, &["checkout", "-q", "--detach"]).unwrap();
        git(root, &["branch", "-q", "-D", "main"]).unwrap();

        let fork = fork_point(root);

        assert!(
            matches!(fork, Err(GraphError::NoForkPoint { .. })),
            "{fork:?}"
        );
    }

    #[test]
    fn a_merge_commit_forks_from_its_first_parent() {
        let directory = repository();
        let root = directory.path();
        git(root, &["checkout", "-q", "-b", "feature"]).unwrap();
        commit(root, "feature work");
        git(root, &["checkout", "-q", "main"]).unwrap();
        std::fs::write(root.join("other.txt"), "main work").unwrap();
        git(root, &["add", "."]).unwrap();
        git(
            root,
            &[
                "-c",
                "user.name=t",
                "-c",
                "user.email=t@t",
                "commit",
                "-q",
                "-m",
                "main work",
            ],
        )
        .unwrap();
        let main = resolve_commit(root, "main").unwrap();
        git(root, &["checkout", "-q", "--detach"]).unwrap();
        git(
            root,
            &[
                "-c",
                "user.name=t",
                "-c",
                "user.email=t@t",
                "merge",
                "-q",
                "--no-edit",
                "feature",
            ],
        )
        .unwrap();
        git(root, &["branch", "-q", "-D", "main", "feature"]).unwrap();

        let fork = fork_point(root).unwrap();

        assert_eq!(fork, main);
    }

    #[test]
    fn on_main_the_fork_point_is_the_checked_out_commit() {
        let directory = repository();
        let root = directory.path();

        let fork = fork_point(root).unwrap();

        assert_eq!(fork, resolve_commit(root, "HEAD").unwrap());
    }

    #[test]
    fn a_lock_left_by_a_process_that_is_gone_is_taken_over() {
        let directory = tempfile::tempdir().unwrap();
        let lock = directory.path().join("repo.lock");
        // A process id no live process has.
        std::fs::write(&lock, "999999999").unwrap();

        let taken = acquire(&lock, directory.path());

        assert!(taken.is_ok());
        assert_eq!(
            std::fs::read_to_string(&lock).unwrap(),
            std::process::id().to_string()
        );
    }

    #[test]
    fn adding_a_worktree_keeps_only_the_most_recently_used() {
        let directory = repository();
        let root = directory.path();
        commit(root, "second");
        let cache = tempfile::tempdir().unwrap();
        let commits: Vec<String> = git(root, &["rev-list", "HEAD"])
            .unwrap()
            .lines()
            .map(str::to_owned)
            .collect();
        let at = |commit: &String| Checkout {
            root: root.to_owned(),
            commit: Some(commit.clone()),
        };
        let first = at(&commits[0]).worktree(cache.path()).unwrap();
        let second = at(&commits[1]).worktree(cache.path()).unwrap();
        let used = |name: &Path, seconds: u64| {
            let mark = cache
                .path()
                .join("worktrees")
                .join(USES)
                .join(name.file_name().unwrap());
            let file = std::fs::File::options().write(true).open(mark).unwrap();
            file.set_modified(std::time::UNIX_EPOCH + Duration::from_secs(seconds))
                .unwrap();
        };
        used(&first, 2_000);
        used(&second, 1_000);

        remove_unused_worktrees(&cache.path().join("worktrees"), 1);

        assert!(first.exists());
        assert!(!second.exists());
        assert!(
            !git(root, &["worktree", "list"])
                .unwrap()
                .contains(&*second.to_string_lossy())
        );
    }

    #[test]
    fn urls_become_directory_names() {
        assert_eq!(
            sanitize("git@github.com:org/repo.git"),
            "git_github.com_org_repo.git"
        );
    }
}
