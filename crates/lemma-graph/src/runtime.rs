//! Running a repository's Python in a runtime kept warm: a process that has
//! already imported what the repository's code imports from the
//! interpreter's own directories, so a run pays only for the repository's
//! own modules. Who keeps such runtimes, and how, is the caller's; a check
//! only asks, and runs the command itself when nothing warm answers. See
//! `.specs/20-warm-runtimes.md`.

use std::collections::BTreeMap;
use std::path::PathBuf;
use std::sync::Arc;

use serde::{Deserialize, Serialize};

use crate::repos;

/// What a run executes, as the interpreter's command line names it.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum PythonCode {
    /// `python -m NAME`.
    Module(String),
    /// `python PATH`, relative to the run's directory.
    Script(String),
    /// `python -c TEXT`.
    Code(String),
}

/// One run of a repository's Python.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PythonRun {
    /// The interpreter, as an absolute path or a name on `PATH`.
    pub python: String,
    /// What tells runtimes apart: the interpreter, its installed packages,
    /// and the variables that decide which libraries it loads. Runs with
    /// equal keys can share a warm runtime.
    pub key: String,
    /// What to execute.
    pub code: PythonCode,
    /// Its arguments.
    pub arguments: Vec<String>,
    /// Where it runs.
    pub directory: PathBuf,
    /// Its whole environment.
    pub environment: BTreeMap<String, String>,
}

/// Something that keeps runtimes warm and runs Python in them.
pub trait WarmRuntime: Send + Sync {
    /// The standard output of `run`, if a warm runtime ran it and it exited
    /// cleanly. `None` for anything else, a failed run included: the caller
    /// then runs it itself, and reports what that run says.
    fn run(&self, run: &PythonRun) -> Option<String>;
}

/// The warm runtimes a check may ask.
#[derive(Clone)]
pub struct Runtime(pub Arc<dyn WarmRuntime>);

/// Two are equal only if they are the same: what they hold is processes.
impl PartialEq for Runtime {
    fn eq(&self, other: &Self) -> bool {
        Arc::ptr_eq(&self.0, &other.0)
    }
}

impl Eq for Runtime {}

impl std::fmt::Debug for Runtime {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.write_str("Runtime")
    }
}

/// What a command runs, when it is the service's interpreter running one
/// module or one script with plain arguments: `{python} -m NAME ARGS` or
/// `{python} PATH.py ARGS`. `None` for anything a shell would have to read:
/// a pipe, a quote, a variable, another program.
pub(crate) fn invocation(command: &str) -> Option<(PythonCode, Vec<String>)> {
    let mut words = command.split_whitespace();
    if words.next() != Some("{python}") {
        return None;
    }
    let rest: Vec<&str> = words.collect();
    let is_plain = |word: &&str| {
        word.chars()
            .all(|letter| letter.is_alphanumeric() || "-_./=:,@+%".contains(letter))
    };
    if !rest.iter().all(is_plain) {
        return None;
    }
    let arguments = |from: usize| rest[from..].iter().map(|word| (*word).to_owned()).collect();
    match rest.as_slice() {
        ["-m", module, ..] if !module.starts_with('-') => {
            Some((PythonCode::Module((*module).to_owned()), arguments(2)))
        }
        [script, ..] if !script.starts_with('-') && is_python_file(script) => {
            Some((PythonCode::Script((*script).to_owned()), arguments(1)))
        }
        _ => None,
    }
}

/// Whether `path` names a Python source file, as the interpreter takes one.
fn is_python_file(path: &str) -> bool {
    std::path::Path::new(path).extension() == Some(std::ffi::OsStr::new("py"))
}

/// The whole environment a command with `environment` starts in.
pub(crate) fn whole_environment(environment: repos::Environment<'_>) -> BTreeMap<String, String> {
    let mut whole: BTreeMap<String, String> = match environment.base {
        Some(base) => base.clone(),
        None => std::env::vars_os()
            .filter_map(|(name, value)| Some((name.into_string().ok()?, value.into_string().ok()?)))
            .collect(),
    };
    whole.extend(environment.added.clone());
    whole
}

/// The variables a run inherits that decide how its interpreter starts and
/// what it loads, besides every one whose name starts with `PYTHON`: where
/// programs and libraries are found, and how text is encoded.
const STARTUP_VARIABLES: [&str; 5] = ["PATH", "VIRTUAL_ENV", "LANG", "LC_ALL", "LC_CTYPE"];

/// The key of the runtime that can run `python` for a run: `interpreter`
/// names its version and installed packages, `declared` are the variables
/// a description sets for it, and `whole` is the run's whole environment,
/// of which the variables an interpreter reads as it starts count. Two runs
/// with one key start interpreters that differ in nothing a warm one keeps.
pub(crate) fn key(
    python: &str,
    interpreter: &str,
    declared: &BTreeMap<String, String>,
    whole: &BTreeMap<String, String>,
) -> String {
    let starts_with: Vec<(&String, &String)> = whole
        .iter()
        .filter(|(name, _)| {
            name.starts_with("PYTHON") || STARTUP_VARIABLES.contains(&name.as_str())
        })
        .collect();
    let described = format!("runtime\n{python}\n{interpreter}\n{declared:?}\n{starts_with:?}");
    lemma_model::content_hash(described.as_bytes())[..24].to_owned()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn variables(pairs: &[(&str, &str)]) -> BTreeMap<String, String> {
        pairs
            .iter()
            .map(|(name, value)| ((*name).to_owned(), (*value).to_owned()))
            .collect()
    }

    #[test]
    fn a_runtimes_key_counts_what_an_interpreter_reads_as_it_starts_and_nothing_else() {
        let none = BTreeMap::new();
        let shell = variables(&[("PATH", "/bin"), ("TERM", "xterm"), ("PWD", "/a")]);
        let other_shell = variables(&[("PATH", "/bin"), ("TERM", "dumb"), ("PWD", "/b")]);
        let other_path = variables(&[("PATH", "/bin"), ("PYTHONPATH", "src")]);

        let one = key("python3", "3.12", &none, &shell);

        assert_eq!(one, key("python3", "3.12", &none, &other_shell));
        assert_ne!(one, key("python3", "3.12", &none, &other_path));
        assert_ne!(one, key("python3", "3.13", &none, &shell));
        assert_ne!(
            one,
            key("python3", "3.12", &variables(&[("MODE", "x")]), &shell)
        );
    }

    #[test]
    fn a_module_run_by_the_services_interpreter_is_an_invocation() {
        let found = invocation("{python} -m billing.schemas --all");

        let expected = (
            PythonCode::Module("billing.schemas".to_owned()),
            vec!["--all".to_owned()],
        );
        assert_eq!(found, Some(expected));
    }

    #[test]
    fn a_script_run_by_the_services_interpreter_is_an_invocation() {
        let found = invocation("{python} tools/dump.py out.json");

        let expected = (
            PythonCode::Script("tools/dump.py".to_owned()),
            vec!["out.json".to_owned()],
        );
        assert_eq!(found, Some(expected));
    }

    #[test]
    fn what_a_shell_would_have_to_read_is_no_invocation() {
        assert_eq!(invocation("{python} -m billing.schemas | jq ."), None);
        assert_eq!(invocation("{python} -m billing.schemas > out.json"), None);
        assert_eq!(invocation("{python} -c 'print(1)'"), None);
        assert_eq!(invocation("uv run python -m billing.schemas"), None);
        assert_eq!(invocation("{python} -X dev -m billing.schemas"), None);
        assert_eq!(invocation("{python} -m $MODULE"), None);
    }
}
