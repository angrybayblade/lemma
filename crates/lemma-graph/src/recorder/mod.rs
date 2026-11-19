//! Recording what a command does to files while it runs (spec 13, section
//! 10): the files its processes read, ran or looked at, the directories
//! they listed, the paths they looked for and did not find, and what they
//! wrote. Evidence is kept by what a run read, so a recorder must miss
//! nothing: where a run cannot be watched there is no recorder, and
//! evidence is kept by whole repositories instead.

use std::collections::BTreeSet;
use std::path::{Path, PathBuf};

#[cfg_attr(
    not(target_os = "linux"),
    allow(
        dead_code,
        reason = "built on every platform so its parser's tests run on captured text; only Linux runs commands under it"
    )
)]
mod strace;

/// What one run did to files, by absolute path.
#[derive(Debug, Default, Clone, PartialEq, Eq)]
pub(crate) struct Recorded {
    /// Files and directories that were opened for reading, run, or looked
    /// at: a result may rest on what any of them holds, or on its being
    /// there.
    pub(crate) read: BTreeSet<PathBuf>,
    /// Directories whose entries were read.
    pub(crate) listed: BTreeSet<PathBuf>,
    /// Paths that were looked for and not found.
    pub(crate) absent: BTreeSet<PathBuf>,
    /// Paths the run opened for writing, made, renamed or removed.
    pub(crate) written: BTreeSet<PathBuf>,
}

/// Something that can watch a command run.
pub(crate) trait Recorder: Send + Sync {
    /// What identifies the recorder in a record, such as `strace 6.8`.
    fn name(&self) -> &str;

    /// `program` as a command that runs it watched, with the same exit
    /// status, leaving what was seen under the empty directory `scratch`.
    fn watching(&self, program: &[String], scratch: &Path) -> Vec<String>;

    /// What the run that [`Recorder::watching`] started did, once it has
    /// ended; `directory` is where it started.
    fn recorded(&self, scratch: &Path, directory: &Path) -> Result<Recorded, String>;
}

/// The recorder this machine can run commands under, if it has one.
pub(crate) fn find(environment: crate::repos::Environment<'_>) -> Option<Box<dyn Recorder>> {
    #[cfg(target_os = "linux")]
    {
        strace::Strace::find(environment).map(|found| Box::new(found) as Box<dyn Recorder>)
    }
    #[cfg(not(target_os = "linux"))]
    {
        let _ = environment;
        None
    }
}

/// `path` without `.` and `..` parts, resolved by name only.
pub(crate) fn normalized(path: &Path) -> PathBuf {
    let mut parts: Vec<std::path::Component<'_>> = Vec::new();
    for part in path.components() {
        match part {
            std::path::Component::CurDir => {}
            std::path::Component::ParentDir => {
                if matches!(parts.last(), Some(std::path::Component::Normal(_))) {
                    parts.pop();
                }
            }
            other => parts.push(other),
        }
    }
    parts.iter().collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_path_is_normalized_by_name() {
        assert_eq!(normalized(Path::new("/a/./b/../c")), Path::new("/a/c"));
        assert_eq!(normalized(Path::new("/../a")), Path::new("/a"));
    }
}
