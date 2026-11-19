//! The recorder for Linux: `strace` watches the system calls that name
//! files, in every process a run starts. Its output stops here; the rest
//! of Lemma sees a [`Recorded`].

use std::collections::{BTreeMap, BTreeSet};
use std::ffi::OsString;
use std::os::unix::ffi::OsStringExt;
use std::path::{Path, PathBuf};

use super::{Recorded, Recorder, normalized};
use crate::repos;

/// The system calls watched: those that name a file to read, run, look at,
/// list, change, or move to, and those that start a process. `io_uring`
/// is watched to be refused, since what it opens is not seen.
const CALLS: &str = "openat,open,openat2,creat,execve,execveat,stat,lstat,newfstatat,statx,access,\
faccessat,faccessat2,readlink,readlinkat,getdents,getdents64,chdir,fchdir,clone,clone3,fork,vfork,\
unlink,unlinkat,rmdir,rename,renameat,renameat2,mkdir,mkdirat,symlink,symlinkat,link,linkat,\
truncate,io_uring_setup";

/// `strace`, as found on this machine.
pub(super) struct Strace {
    name: String,
}

impl Strace {
    /// `strace`, if it is installed and may trace here: a container can
    /// forbid tracing, and an old `strace` lacks what the options need.
    pub(super) fn find(environment: repos::Environment<'_>) -> Option<Self> {
        let directory = std::env::temp_dir();
        let version = repos::run_program(
            &directory,
            &["strace".to_owned(), "-V".to_owned()],
            environment,
            Some(10_000),
        )
        .ok()?;
        let name = version.lines().next()?.replace(" -- version", "");
        let probe: Vec<String> = options(Path::new("/dev/null"))
            .into_iter()
            .filter(|option| option != "-ff")
            .chain(["--".to_owned(), "true".to_owned()])
            .collect();
        repos::run_program(&directory, &probe, environment, Some(10_000)).ok()?;
        Some(Self { name })
    }
}

/// The command line that watches a program, up to the program.
fn options(output: &Path) -> Vec<String> {
    let mut command: Vec<String> = [
        "strace",
        "-ff",
        "-qq",
        "-y",
        "-s",
        "4096",
        "--seccomp-bpf",
        "-e",
        "signal=none",
        "-e",
        "inject=io_uring_setup:error=ENOSYS",
        "-e",
    ]
    .iter()
    .map(|part| (*part).to_owned())
    .collect();
    command.push(format!("trace={CALLS}"));
    command.push("-o".to_owned());
    command.push(output.display().to_string());
    command
}

impl Recorder for Strace {
    fn name(&self) -> &str {
        &self.name
    }

    fn watching(&self, program: &[String], scratch: &Path) -> Vec<String> {
        let mut command = options(&scratch.join("calls"));
        command.push("--".to_owned());
        command.extend(program.iter().cloned());
        command
    }

    fn recorded(&self, scratch: &Path, directory: &Path) -> Result<Recorded, String> {
        let mut traces = BTreeMap::new();
        let entries = std::fs::read_dir(scratch).map_err(|error| error.to_string())?;
        for entry in entries {
            let path = entry.map_err(|error| error.to_string())?.path();
            let process = path
                .extension()
                .and_then(|extension| extension.to_str())
                .and_then(|extension| extension.parse::<u64>().ok());
            if let Some(process) = process {
                let text = std::fs::read(&path).map_err(|error| error.to_string())?;
                traces.insert(process, text);
            }
        }
        if traces.is_empty() {
            return Err("the recorder saw no process".to_owned());
        }
        Ok(read_traces(&traces, directory))
    }
}

/// What the processes of one run did, from each one's calls. A process
/// starts in the directory its parent was in when it started it.
fn read_traces(traces: &BTreeMap<u64, Vec<u8>>, directory: &Path) -> Recorded {
    let mut reading = Reading {
        traces,
        recorded: Recorded::default(),
        done: BTreeSet::new(),
        asked_as: BTreeMap::new(),
    };
    // The processes nothing here started come first, each followed into
    // what it started, so that a child is read from where its parent was.
    let started: BTreeSet<u64> = traces
        .values()
        .flat_map(|text| text.split(|byte| *byte == b'\n'))
        .filter_map(|line| Call::parse(line)?.started())
        .collect();
    let first = traces.keys().filter(|process| !started.contains(process));
    for process in first.chain(traces.keys()) {
        reading.process(*process, directory.to_path_buf());
    }
    reading.recorded
}

struct Reading<'t> {
    traces: &'t BTreeMap<u64, Vec<u8>>,
    recorded: Recorded,
    done: BTreeSet<u64>,
    /// For a path the system says was opened, the paths it was asked for
    /// by, where they differ: the way there went through a link.
    asked_as: BTreeMap<PathBuf, BTreeSet<PathBuf>>,
}

impl Reading<'_> {
    fn process(&mut self, process: u64, mut directory: PathBuf) {
        let Some(text) = self.traces.get(&process) else {
            return;
        };
        if !self.done.insert(process) {
            return;
        }
        for line in text.split(|byte| *byte == b'\n') {
            let Some(call) = Call::parse(line) else {
                continue;
            };
            if let Some(here) = call.working_directory() {
                directory = here;
            }
            if let Some(child) = call.started() {
                self.process(child, directory.clone());
                continue;
            }
            self.take(&call, &mut directory);
        }
    }

    /// Records one call's effect, or follows a change of directory.
    fn take(&mut self, call: &Call, directory: &mut PathBuf) {
        let at = |index: usize| call.path_at(index, directory);
        let from = |index: usize| call.path(index, directory);
        match call.name.as_str() {
            "chdir" if call.succeeded() => {
                if let Some(path) = from(0) {
                    self.recorded.read.insert(path.clone());
                    *directory = path;
                }
            }
            "fchdir" if call.succeeded() => {
                if let Some(path) = call.descriptor(0) {
                    *directory = path;
                }
            }
            "open" | "creat" => self.opened(call, from(0), 1),
            "openat" | "openat2" => self.opened(call, at(0), 2),
            "execve" | "stat" | "lstat" | "access" | "readlink" => self.looked(call, from(0)),
            "execveat" | "newfstatat" | "statx" | "faccessat" | "faccessat2" | "readlinkat" => {
                self.looked(call, at(0));
            }
            "getdents" | "getdents64" if call.succeeded() => {
                if let Some(path) = call.descriptor(0) {
                    let asked = self.asked_as.get(&path).into_iter().flatten();
                    self.recorded.listed.extend(asked.cloned());
                    self.recorded.listed.insert(path);
                }
            }
            "unlink" | "rmdir" | "mkdir" | "truncate" => self.wrote(call, [from(0)]),
            "unlinkat" | "mkdirat" => self.wrote(call, [at(0)]),
            "rename" | "link" => self.wrote(call, [from(0), from(1)]),
            "symlink" => self.wrote(call, [from(1)]),
            "symlinkat" => self.wrote(call, [at(1)]),
            "renameat" | "renameat2" | "linkat" => self.wrote(call, [at(0), at(2)]),
            _ => {}
        }
    }

    /// An `open`: the path asked for and the file the system says was
    /// opened, read or written by the flags, or a path not found. Both
    /// paths are kept: where a link is on the way, the path asked for
    /// says the result rests on where the link points.
    fn opened(&mut self, call: &Call, asked: Option<PathBuf>, flags: usize) {
        let flags = call.arguments.get(flags).map_or("", String::as_str);
        let writes = call.name == "creat"
            || ["O_WRONLY", "O_RDWR", "O_CREAT", "O_TRUNC", "O_APPEND"]
                .iter()
                .any(|flag| flags.contains(flag));
        if !call.succeeded() {
            if call.is_not_found() && !writes {
                self.recorded.absent.extend(asked);
            }
            return;
        }
        // A file made without a name is asked for by its directory.
        let asked = asked.filter(|_| !flags.contains("O_TMPFILE"));
        let led_to = call.result_path();
        if let (Some(asked), Some(led_to)) = (&asked, &led_to) {
            if asked != led_to {
                let known = self.asked_as.entry(led_to.clone()).or_default();
                known.insert(asked.clone());
            }
        }
        for path in asked.into_iter().chain(led_to) {
            if writes {
                self.recorded.written.insert(path.clone());
            }
            if !flags.contains("O_WRONLY") {
                self.recorded.read.insert(path);
            }
        }
    }

    /// A call that runs a file or looks at one.
    fn looked(&mut self, call: &Call, path: Option<PathBuf>) {
        if call.succeeded() {
            self.recorded.read.extend(path);
        } else if call.is_not_found() {
            self.recorded.absent.extend(path);
        }
    }

    fn wrote<const N: usize>(&mut self, call: &Call, paths: [Option<PathBuf>; N]) {
        if call.succeeded() {
            self.recorded.written.extend(paths.into_iter().flatten());
        }
    }
}

/// One system call as `strace` prints it: `name(arguments) = result`.
#[derive(Debug, PartialEq, Eq)]
struct Call {
    name: String,
    arguments: Vec<String>,
    /// The number returned, and what follows it: an error's name, or the
    /// path of a returned descriptor.
    result: i64,
    after: String,
}

impl Call {
    fn parse(line: &[u8]) -> Option<Self> {
        let line = String::from_utf8_lossy(line);
        let open = line.find('(')?;
        let name = line.get(..open)?;
        if name.is_empty() || !name.chars().all(|c| c.is_ascii_alphanumeric() || c == '_') {
            return None;
        }
        let close = line.rfind(") = ")?;
        let arguments = split_arguments(line.get(open + 1..close)?);
        let result = line.get(close + 4..)?.trim();
        let digits = result
            .find(|c: char| !(c.is_ascii_digit() || c == '-'))
            .unwrap_or(result.len());
        Some(Self {
            name: name.to_owned(),
            arguments,
            result: result.get(..digits)?.parse().ok()?,
            after: result.get(digits..)?.trim().to_owned(),
        })
    }

    fn succeeded(&self) -> bool {
        self.result >= 0
    }

    /// Whether the call failed because the path, or a directory on the
    /// way to it, is not there.
    fn is_not_found(&self) -> bool {
        self.after.starts_with("ENOENT") || self.after.starts_with("ENOTDIR")
    }

    /// The process a call started, if it started one.
    fn started(&self) -> Option<u64> {
        let starts = matches!(self.name.as_str(), "clone" | "clone3" | "fork" | "vfork");
        if !starts || self.result <= 0 {
            return None;
        }
        u64::try_from(self.result).ok()
    }

    /// The path of the descriptor a call returned.
    fn result_path(&self) -> Option<PathBuf> {
        annotation(&self.after)
    }

    /// The path of the descriptor given as argument `index`.
    fn descriptor(&self, index: usize) -> Option<PathBuf> {
        annotation(self.arguments.get(index)?)
    }

    /// Where the process is, when a call says so: `AT_FDCWD</here>`.
    fn working_directory(&self) -> Option<PathBuf> {
        self.arguments
            .iter()
            .find(|argument| argument.starts_with("AT_FDCWD<"))
            .and_then(|argument| annotation(argument))
    }

    /// The path given as argument `index`, from `directory` if relative.
    fn path(&self, index: usize, directory: &Path) -> Option<PathBuf> {
        let path = quoted(self.arguments.get(index)?)?;
        Some(normalized(&directory.join(path)))
    }

    /// The path given as argument `index + 1`, from the directory that
    /// argument `index` names; that directory itself when the path is
    /// empty.
    fn path_at(&self, index: usize, directory: &Path) -> Option<PathBuf> {
        let base = self.arguments.get(index)?;
        let base = if base.starts_with("AT_FDCWD") {
            annotation(base).unwrap_or_else(|| directory.to_path_buf())
        } else {
            annotation(base)?
        };
        let path = quoted(self.arguments.get(index + 1)?)?;
        Some(normalized(&base.join(path)))
    }
}

/// A call's arguments, split at the commas outside strings, brackets and
/// descriptors' paths.
fn split_arguments(text: &str) -> Vec<String> {
    let mut arguments = Vec::new();
    let mut current = String::new();
    let mut depth = 0u32;
    let mut in_string = false;
    let mut in_path = false;
    let mut characters = text.chars().peekable();
    while let Some(character) = characters.next() {
        if character == '\\' && (in_string || in_path) {
            current.push(character);
            current.extend(characters.next());
            continue;
        }
        match character {
            '"' if !in_path => in_string = !in_string,
            '<' if !in_string && !in_path && follows_descriptor(&current) => in_path = true,
            '>' if in_path => in_path = false,
            '(' | '[' | '{' if !in_string && !in_path => depth += 1,
            ')' | ']' | '}' if !in_string && !in_path => depth = depth.saturating_sub(1),
            ',' if !in_string && !in_path && depth == 0 => {
                arguments.push(current.trim().to_owned());
                current.clear();
                continue;
            }
            _ => {}
        }
        current.push(character);
    }
    if !current.trim().is_empty() {
        arguments.push(current.trim().to_owned());
    }
    arguments
}

/// Whether an argument so far is a descriptor, whose path may follow.
fn follows_descriptor(argument: &str) -> bool {
    let argument = argument.trim();
    argument == "AT_FDCWD" || (!argument.is_empty() && argument.chars().all(|c| c.is_ascii_digit()))
}

/// The path in `3</some/path>` or `AT_FDCWD</some/path>`.
fn annotation(text: &str) -> Option<PathBuf> {
    let start = text.find('<')?;
    let end = text.rfind('>')?;
    let path = unescaped(text.get(start + 1..end)?);
    let path = PathBuf::from(OsString::from_vec(path));
    path.is_absolute().then_some(path)
}

/// The text of a quoted argument, as the bytes it stands for.
fn quoted(argument: &str) -> Option<PathBuf> {
    let inner = argument.strip_prefix('"')?;
    let end = closing_quote(inner)?;
    let bytes = unescaped(inner.get(..end)?);
    Some(PathBuf::from(OsString::from_vec(bytes)))
}

/// Where a string's closing quote is, past any escaped ones.
fn closing_quote(text: &str) -> Option<usize> {
    let mut is_escaped = false;
    for (index, character) in text.char_indices() {
        match character {
            _ if is_escaped => is_escaped = false,
            '\\' => is_escaped = true,
            '"' => return Some(index),
            _ => {}
        }
    }
    None
}

/// `text` with C escapes replaced by the bytes they name.
fn unescaped(text: &str) -> Vec<u8> {
    let bytes = text.as_bytes();
    let mut found = Vec::with_capacity(bytes.len());
    let mut index = 0;
    while index < bytes.len() {
        if bytes[index] != b'\\' || index + 1 == bytes.len() {
            found.push(bytes[index]);
            index += 1;
            continue;
        }
        let (byte, used) = escape(&bytes[index + 1..]);
        found.push(byte);
        index += 1 + used;
    }
    found
}

/// The byte an escape names and how many characters of it follow the
/// backslash.
fn escape(rest: &[u8]) -> (u8, usize) {
    let simple = match rest[0] {
        b'n' => Some(b'\n'),
        b't' => Some(b'\t'),
        b'r' => Some(b'\r'),
        b'v' => Some(0x0b),
        b'f' => Some(0x0c),
        b'0'..=b'7' | b'x' => None,
        other => Some(other),
    };
    if let Some(byte) = simple {
        return (byte, 1);
    }
    if rest[0] == b'x' {
        let digits = rest.get(1..3).and_then(|hex| std::str::from_utf8(hex).ok());
        return match digits.and_then(|hex| u8::from_str_radix(hex, 16).ok()) {
            Some(byte) => (byte, 3),
            None => (b'x', 1),
        };
    }
    let digits = rest
        .iter()
        .take(3)
        .take_while(|byte| (b'0'..=b'7').contains(byte))
        .count();
    let value = rest[..digits]
        .iter()
        .fold(0u32, |value, digit| value * 8 + u32::from(digit - b'0'));
    (u8::try_from(value & 0xff).unwrap_or(0), digits)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn recorded(traces: &[(u64, &str)]) -> Recorded {
        let traces: BTreeMap<u64, Vec<u8>> = traces
            .iter()
            .map(|(process, text)| (*process, text.as_bytes().to_vec()))
            .collect();
        read_traces(&traces, Path::new("/work"))
    }

    fn paths(set: &BTreeSet<PathBuf>) -> Vec<&str> {
        set.iter().filter_map(|path| path.to_str()).collect()
    }

    #[test]
    fn an_open_is_a_read_of_the_path_asked_for_and_the_file_it_led_to() {
        let found = recorded(&[(
            1,
            "openat(AT_FDCWD</work>, \"link.txt\", O_RDONLY|O_CLOEXEC) = 3</work/real.txt>\n",
        )]);

        assert_eq!(paths(&found.read), ["/work/link.txt", "/work/real.txt"]);
        assert!(found.written.is_empty());
    }

    #[test]
    fn a_listing_through_a_link_is_also_of_the_path_asked_for() {
        let found = recorded(&[(
            1,
            "openat(AT_FDCWD</work>, \"current\", O_RDONLY|O_DIRECTORY) = 3</work/v2>\n\
             getdents64(3</work/v2>, 0x55d /* 4 entries */, 32768) = 104\n\
             chdir(\"current\") = 0\n",
        )]);

        assert_eq!(paths(&found.listed), ["/work/current", "/work/v2"]);
        assert_eq!(paths(&found.read), ["/work/current", "/work/v2"]);
    }

    #[test]
    fn a_file_looked_at_counts_as_read_and_one_not_found_as_absent() {
        let found = recorded(&[(
            1,
            "newfstatat(AT_FDCWD</work>, \"a.py\", {st_mode=S_IFREG|0644, st_size=5, ...}, 0) = 0\n\
             access(\"b.py\", F_OK) = -1 ENOENT (No such file or directory)\n\
             statx(AT_FDCWD</work>, \"c/d.py\", AT_STATX_SYNC_AS_STAT, STATX_ALL, 0x7ffc) = -1 ENOTDIR (Not a directory)\n",
        )]);

        assert_eq!(paths(&found.read), ["/work/a.py"]);
        assert_eq!(paths(&found.absent), ["/work/b.py", "/work/c/d.py"]);
    }

    #[test]
    fn a_listing_is_the_directory_whose_entries_were_read() {
        let found = recorded(&[(
            1,
            "openat(AT_FDCWD</work>, \"src\", O_RDONLY|O_NONBLOCK|O_CLOEXEC|O_DIRECTORY) = 3</work/src>\n\
             getdents64(3</work/src>, 0x55d /* 4 entries */, 32768) = 104\n",
        )]);

        assert_eq!(paths(&found.listed), ["/work/src"]);
    }

    #[test]
    fn what_a_run_writes_is_kept_apart_from_what_it_reads() {
        let found = recorded(&[(
            1,
            "openat(AT_FDCWD</work>, \"out.txt\", O_WRONLY|O_CREAT|O_TRUNC, 0666) = 3</work/out.txt>\n\
             openat(AT_FDCWD</work>, \"state.db\", O_RDWR) = 4</work/state.db>\n\
             renameat2(AT_FDCWD</work>, \"tmp\", AT_FDCWD</work>, \"final\", 0) = 0\n\
             unlink(\"gone\") = 0\n",
        )]);

        assert_eq!(paths(&found.read), ["/work/state.db"]);
        assert_eq!(
            paths(&found.written),
            [
                "/work/final",
                "/work/gone",
                "/work/out.txt",
                "/work/state.db",
                "/work/tmp"
            ]
        );
    }

    #[test]
    fn a_process_starts_where_its_parent_was() {
        let found = recorded(&[
            (
                1,
                "chdir(\"/work/app\") = 0\nvfork() = 2\naccess(\"here\", F_OK) = 0\n",
            ),
            (
                2,
                "execve(\"./run.sh\", [\"run.sh\"], 0x7ff /* 3 vars */) = 0\nstat(\"data\", {st_mode=S_IFREG}) = 0\n",
            ),
        ]);

        assert_eq!(
            paths(&found.read),
            [
                "/work/app",
                "/work/app/data",
                "/work/app/here",
                "/work/app/run.sh"
            ]
        );
    }

    #[test]
    fn escapes_in_a_path_are_the_bytes_they_name() {
        let found = recorded(&[(
            1,
            "openat(AT_FDCWD</work>, \"caf\\303\\251, \\\"x\\\".txt\", O_RDONLY) = 3</work/caf\\303\\251, \"x\".txt>\n\
             access(\"a\\tb\", F_OK) = 0\n",
        )]);

        assert_eq!(paths(&found.read), ["/work/a\tb", "/work/café, \"x\".txt"]);
    }

    #[test]
    fn lines_that_are_not_finished_calls_are_passed_over() {
        let found = recorded(&[(
            1,
            "--- SIGCHLD {si_signo=SIGCHLD} ---\nopenat(AT_FDCWD</work>, \"a\", O_RDONLY <unfinished ...>\n+++ exited with 0 +++\n",
        )]);

        assert_eq!(found, Recorded::default());
    }
}
