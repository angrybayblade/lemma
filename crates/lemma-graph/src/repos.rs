//! Repositories: where each one is, which commit is read, and the files and
//! worktrees at that commit.
//!
//! A `path` source is a local clone; a `git` source is cloned into Lemma's
//! cache on first use. A commit is read from Git without a checkout; a
//! worktree is created only when a resolver has to run code at a commit.

use std::collections::BTreeMap;
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
    /// `cache`, created once per commit.
    pub fn worktree(&self, cache: &Path) -> Result<PathBuf, GraphError> {
        let Some(commit) = &self.commit else {
            return Ok(self.root.clone());
        };
        let name = format!("{}-{commit}", directory_name(&self.root));
        let directory = cache.join("worktrees").join(name);
        if directory.exists() {
            return Ok(directory);
        }
        std::fs::create_dir_all(cache.join("worktrees")).map_err(|source| GraphError::Io {
            path: cache.to_owned(),
            source,
        })?;
        let target = directory.to_string_lossy().into_owned();
        git(
            &self.root,
            &["worktree", "add", "--detach", "--quiet", &target, commit],
        )?;
        Ok(directory)
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
    let output = Command::new("git")
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
    fn urls_become_directory_names() {
        assert_eq!(
            sanitize("git@github.com:org/repo.git"),
            "git_github.com_org_repo.git"
        );
    }
}
