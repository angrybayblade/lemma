//! Soufflé as an engine for Lemma's Datalog.
//!
//! A program is translated to Soufflé source, facts are written as
//! tab-separated files, and `souffle` runs as a process. Results are read
//! back and sorted, so they do not depend on Soufflé's thread scheduling.
//! Explanations come from Soufflé's provenance mode in the same run.

mod provenance;
mod translate;

use std::collections::BTreeSet;
use std::fmt::Write as _;
use std::fs;
use std::io::Write as _;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};

use lemma_logic::{
    ColumnType, Derived, Engine, Fact, FactSet, Features, Program, RelationRole, Tuple, Value,
};

pub use translate::{Translation, translate};

/// Why Soufflé could not evaluate a program.
#[derive(Debug, thiserror::Error)]
pub enum SouffleError {
    /// `souffle` is not on `PATH`.
    #[error("souffle is not installed or not on PATH")]
    NotFound,
    /// A working file could not be written or read.
    #[error("could not use Soufflé's working files")]
    Io(#[from] std::io::Error),
    /// Soufflé exited with an error.
    #[error("souffle failed: {stderr}")]
    Failed {
        /// What Soufflé printed.
        stderr: String,
    },
    /// An output file did not hold the expected rows.
    #[error("could not read Soufflé's output for `{relation}`: {line}")]
    Output {
        /// The relation.
        relation: String,
        /// The offending line.
        line: String,
    },
    /// A number does not fit Soufflé's 32-bit numbers.
    #[error("a value in `{relation}` does not fit Soufflé's 32-bit numbers")]
    OutOfRange {
        /// The relation.
        relation: String,
    },
}

/// Runs programs with the `souffle` executable.
#[derive(Debug, Clone)]
pub struct SouffleEngine {
    executable: PathBuf,
    jobs: Option<usize>,
}

/// A program translated for Soufflé.
#[derive(Debug, Clone)]
pub struct Prepared {
    program: Program,
    translation: Translation,
}

impl SouffleEngine {
    /// Uses `souffle` from `PATH`.
    pub fn find() -> Result<Self, SouffleError> {
        let executable = which::which("souffle").map_err(|_| SouffleError::NotFound)?;
        Ok(Self::with_executable(executable))
    }

    /// Uses a specific `souffle` executable.
    pub fn with_executable(executable: PathBuf) -> Self {
        Self {
            executable,
            jobs: None,
        }
    }

    /// Lets Soufflé use this many threads. Results do not depend on it.
    #[must_use]
    pub fn with_jobs(mut self, jobs: usize) -> Self {
        self.jobs = Some(jobs);
        self
    }

    fn command(&self, directory: &Path, explain: bool) -> Command {
        let mut command = Command::new(&self.executable);
        command
            .current_dir(directory)
            .args(["-F", "facts", "-D", "out"])
            .stdout(Stdio::piped())
            .stderr(Stdio::piped());
        if let Some(jobs) = self.jobs {
            command.arg(format!("--jobs={jobs}"));
        }
        if explain {
            command.args(["-t", "explain"]).stdin(Stdio::piped());
        } else {
            command.stdin(Stdio::null());
        }
        command.arg("program.dl");
        command
    }
}

impl Engine for SouffleEngine {
    type Prepared = Prepared;
    type Error = SouffleError;

    fn name(&self) -> &'static str {
        "souffle"
    }

    fn capabilities(&self) -> Features {
        Features {
            ordering: true,
            derivations: true,
            negation: true,
            recursion: true,
        }
    }

    fn prepare(&self, program: &Program) -> Result<Prepared, SouffleError> {
        Ok(Prepared {
            program: program.clone(),
            translation: translate(program),
        })
    }

    fn evaluate(
        &self,
        prepared: &Prepared,
        facts: &FactSet,
        request: &lemma_logic::Request,
    ) -> Result<Derived, SouffleError> {
        let directory = tempfile::tempdir()?;
        fs::write(
            directory.path().join("program.dl"),
            &prepared.translation.source,
        )?;
        write_facts(directory.path(), prepared, facts)?;
        fs::create_dir(directory.path().join("out"))?;
        let explain = !request.explain.is_empty();
        let mut child = self.command(directory.path(), explain).spawn()?;
        if let Some(mut stdin) = child.stdin.take() {
            stdin.write_all(explain_script(prepared, &request.explain)?.as_bytes())?;
        }
        let output = child.wait_with_output()?;
        if !output.status.success() {
            return Err(SouffleError::Failed {
                stderr: String::from_utf8_lossy(&output.stderr).trim().to_owned(),
            });
        }
        let relations = read_outputs(directory.path(), prepared, request)?;
        let derivations = if explain {
            let stdout = String::from_utf8_lossy(&output.stdout);
            provenance::parse(&stdout, &prepared.program, &prepared.translation)
        } else {
            Vec::new()
        };
        Ok(Derived {
            relations,
            derivations,
        })
    }
}

fn write_facts(directory: &Path, prepared: &Prepared, facts: &FactSet) -> Result<(), SouffleError> {
    let facts_directory = directory.join("facts");
    fs::create_dir(&facts_directory)?;
    let inputs = prepared
        .program
        .relations()
        .filter(|relation| relation.role == RelationRole::Input);
    for relation in inputs {
        let mut text = String::new();
        for row in facts.rows(&relation.name) {
            let values = row
                .iter()
                .map(|value| {
                    number(*value).ok_or_else(|| SouffleError::OutOfRange {
                        relation: relation.name.clone(),
                    })
                })
                .collect::<Result<Vec<_>, _>>()?;
            let fields: Vec<String> = values.iter().map(ToString::to_string).collect();
            let _ = writeln!(text, "{}", fields.join("\t"));
        }
        let file = prepared
            .translation
            .file_of(&relation.name)
            .unwrap_or_default();
        fs::write(facts_directory.join(file), text)?;
    }
    Ok(())
}

/// A value as a Soufflé number, if it fits in 32 bits.
fn number(value: Value) -> Option<i32> {
    match value {
        Value::Symbol(id) => i32::try_from(id).ok(),
        Value::Number(number) => i32::try_from(number).ok(),
    }
}

fn explain_script(prepared: &Prepared, wanted: &[Fact]) -> Result<String, SouffleError> {
    let mut script = String::from("format json\n");
    for fact in wanted {
        let identifier = prepared
            .translation
            .identifiers
            .get(&fact.relation)
            .cloned()
            .unwrap_or_default();
        let values = fact
            .row
            .iter()
            .map(|value| {
                number(*value).ok_or_else(|| SouffleError::OutOfRange {
                    relation: fact.relation.clone(),
                })
            })
            .collect::<Result<Vec<_>, _>>()?;
        let fields: Vec<String> = values.iter().map(ToString::to_string).collect();
        let _ = writeln!(script, "explain {identifier}({})", fields.join(", "));
    }
    script.push_str("exit\n");
    Ok(script)
}

fn read_outputs(
    directory: &Path,
    prepared: &Prepared,
    request: &lemma_logic::Request,
) -> Result<std::collections::BTreeMap<String, BTreeSet<Tuple>>, SouffleError> {
    let wanted = |name: &str| request.outputs.is_empty() || request.outputs.contains(name);
    let mut relations = std::collections::BTreeMap::new();
    let derived = prepared
        .program
        .relations()
        .filter(|relation| relation.role == RelationRole::Derived && wanted(&relation.name));
    for relation in derived {
        let file = prepared
            .translation
            .file_of(&relation.name)
            .unwrap_or_default();
        let text = fs::read_to_string(directory.join("out").join(file)).unwrap_or_default();
        let kinds: Vec<ColumnType> = relation.columns.iter().map(|column| column.kind).collect();
        let mut rows = BTreeSet::new();
        for line in text.lines().filter(|line| !line.is_empty()) {
            let row = parse_row(line.split('\t'), &kinds).ok_or_else(|| SouffleError::Output {
                relation: relation.name.clone(),
                line: line.to_owned(),
            })?;
            rows.insert(row);
        }
        relations.insert(relation.name.clone(), rows);
    }
    Ok(relations)
}

/// Parses fields as values of the given column types.
pub(crate) fn parse_row<'a>(
    fields: impl Iterator<Item = &'a str>,
    kinds: &[ColumnType],
) -> Option<Tuple> {
    let values: Vec<Value> = fields
        .zip(kinds)
        .map(|(field, kind)| {
            let number: i64 = field.trim().parse().ok()?;
            Some(match kind {
                ColumnType::Symbol => Value::Symbol(u32::try_from(number).ok()?),
                ColumnType::Number => Value::Number(number),
            })
        })
        .collect::<Option<_>>()?;
    (values.len() == kinds.len()).then(|| values.into_boxed_slice())
}
