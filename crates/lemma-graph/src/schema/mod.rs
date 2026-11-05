//! An operation's request or response schema, resolved to JSON Schema from
//! wherever its `lemma.yaml` says it is: a resolver's output, a JSON file,
//! or a type in code.

mod document;
mod go;
mod python;
mod python_syntax;
mod rust;
mod shape;
mod typescript;

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::rc::Rc;

use serde_json::Value;

use crate::GraphError;
use crate::adapters;
use crate::config::{Resolver, Runtime, SchemaSource};
use crate::repos::{self, Checkout};

pub use document::{at_path, from_openapi, inline_references};

/// A resolved schema, and how it was found.
#[derive(Debug, Clone, PartialEq)]
pub struct Resolved {
    /// The JSON Schema, with references inlined.
    pub schema: Value,
    /// What produced it: `resolver`, `document`, `python` or `tree-sitter`.
    pub source: &'static str,
    /// Names it uses that could not be followed; fields under them are not
    /// checked.
    pub unresolved: Vec<String>,
}

/// Which side of a message a schema describes. Most sources describe one
/// shape for both; Go structs encode and decode by different rules.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Role {
    /// What is sent.
    Sender,
    /// What receives it.
    Receiver,
}

/// What resolving needs besides the source: the repository at a commit, the
/// operation's resolver, the service's runtime, and a cache.
pub struct Context<'a> {
    /// Which side of the message the schema is for.
    pub role: Role,
    /// The repository at the commit being checked.
    pub checkout: &'a Checkout,
    /// The operation's resolver, if any.
    pub resolver: Option<&'a Resolver>,
    /// The service's interpreters and toolchains.
    pub runtime: &'a Runtime,
    /// Lemma's cache directory, for worktrees.
    pub cache: &'a Path,
    /// Resolver outputs already produced, by repository, commit and command.
    pub outputs: &'a mut BTreeMap<(String, String, String), Value>,
    /// What this run has read from repositories already.
    pub reads: &'a mut Reads,
}

/// Prints a Python interpreter's executable, version and installed
/// distributions, sorted.
const INTERPRETER_SCRIPT: &str = "import sys, importlib.metadata as m\n\
print(sys.executable)\nprint(sys.version)\n\
print(sorted((d.metadata['Name'] or '', d.version) for d in m.distributions()))";

/// Bumped when what keeps resolver outputs, or what they mean, changes.
const RESOLVER_PROTOCOL: &str = "1";

/// A directory read at a checkout: repository, commit, directory and
/// extension.
type DirectoryKey = (PathBuf, Option<String>, String, String);

/// What one run has read from repositories, so the operations and links
/// that share a type's package, an anchor's file, or a repository's state
/// read it once.
#[derive(Debug, Default)]
pub struct Reads {
    /// A directory's files with an extension, by repository, commit,
    /// directory and extension.
    directories: BTreeMap<DirectoryKey, Rc<Vec<(String, String)>>>,
    /// A file's text, by repository, commit and path.
    texts: BTreeMap<(PathBuf, Option<String>, String), Option<Rc<str>>>,
    /// A working directory's state (see [`adapters::working_state`]).
    states: BTreeMap<PathBuf, Option<String>>,
    /// What each Python interpreter reports of itself and its packages.
    interpreters: BTreeMap<String, String>,
    /// The shared worktree last checked out, by repository and commit,
    /// held for the run so resolvers at one commit check it out once.
    worktree: Option<(PathBuf, String, repos::SharedWorktree)>,
}

impl Reads {
    /// A file's text at the checkout, read once per run.
    pub fn text(&mut self, checkout: &Checkout, file: &str) -> Result<Option<Rc<str>>, GraphError> {
        let key = (
            checkout.root.clone(),
            checkout.commit.clone(),
            file.to_owned(),
        );
        if let Some(found) = self.texts.get(&key) {
            return Ok(found.clone());
        }
        let found: Option<Rc<str>> = checkout.read(file)?.map(Rc::from);
        self.texts.insert(key, found.clone());
        Ok(found)
    }

    /// Lets go of the shared worktree, for others (adapters) to use.
    pub fn release(&mut self) {
        self.worktree = None;
    }

    /// The shared worktree of the clone at `root`, checked out at `commit`;
    /// kept checked out for the next resolver at the same commit.
    fn worktree_at(
        &mut self,
        root: &Path,
        commit: &str,
        cache: &Path,
    ) -> Result<PathBuf, GraphError> {
        let is_current = self
            .worktree
            .as_ref()
            .is_some_and(|(held_root, held_commit, _)| held_root == root && held_commit == commit);
        if !is_current {
            // The lock is per repository: let the last one go first.
            self.worktree = None;
            let worktree = repos::SharedWorktree::check_out(root, commit, cache)?;
            self.worktree = Some((root.to_owned(), commit.to_owned(), worktree));
        }
        Ok(self
            .worktree
            .as_ref()
            .map(|(_, _, worktree)| worktree.path().to_owned())
            .unwrap_or_default())
    }

    /// What a Python interpreter reports of its version and installed
    /// packages, asked once per run: a cached schema depends on both.
    fn interpreter(&mut self, python: &str) -> String {
        if let Some(found) = self.interpreters.get(python) {
            return found.clone();
        }
        let found = std::process::Command::new(python)
            .args(["-c", INTERPRETER_SCRIPT])
            .output()
            .ok()
            .filter(|output| output.status.success())
            .map_or_else(
                || format!("{python} (did not report)"),
                |output| lemma_model::content_hash(&output.stdout),
            );
        self.interpreters.insert(python.to_owned(), found.clone());
        found
    }

    fn working_state(&mut self, root: &Path) -> Result<Option<String>, GraphError> {
        if let Some(found) = self.states.get(root) {
            return Ok(found.clone());
        }
        let found = adapters::working_state(root)?;
        self.states.insert(root.to_owned(), found.clone());
        Ok(found)
    }

    /// The files with `extension` directly in `directory` at the checkout,
    /// read once per run.
    fn directory(
        &mut self,
        checkout: &Checkout,
        directory: &str,
        extension: &str,
    ) -> Result<Rc<Vec<(String, String)>>, GraphError> {
        let key = (
            checkout.root.clone(),
            checkout.commit.clone(),
            directory.to_owned(),
            extension.to_owned(),
        );
        if let Some(found) = self.directories.get(&key) {
            return Ok(Rc::clone(found));
        }
        let prefixes = if directory.is_empty() {
            Vec::new()
        } else {
            vec![directory.to_owned()]
        };
        let files = lemma_http::read_files(
            &checkout.root,
            checkout.commit.as_deref(),
            &prefixes,
            &[extension],
        )
        .map_err(|source| GraphError::Source { source })?;
        let found: Rc<Vec<(String, String)>> = Rc::new(
            files
                .into_iter()
                .filter(|candidate| {
                    let parent = candidate
                        .path
                        .rsplit_once('/')
                        .map_or("", |(parent, _)| parent);
                    parent == directory
                })
                .map(|candidate| (candidate.path, candidate.contents))
                .collect(),
        );
        self.directories.insert(key, Rc::clone(&found));
        Ok(found)
    }
}

/// Resolves a schema source to JSON Schema.
pub fn resolve(source: &SchemaSource, context: &mut Context<'_>) -> Result<Resolved, GraphError> {
    match source {
        SchemaSource::Document(location) => {
            if !matches!(location.format.as_str(), "json-schema" | "openapi") {
                return Err(GraphError::Schema {
                    message: format!(
                        "unknown schema format `{}`; use `json-schema` or `openapi`",
                        location.format
                    ),
                });
            }
            let document = match (&location.file, context.resolver) {
                (Some(file), _) => read_json(context.checkout, file)?,
                (None, Some(resolver)) => run_resolver(resolver, context)?,
                (None, None) => {
                    return Err(GraphError::Schema {
                        message: format!("`{}` names neither a file nor a resolver", location.path),
                    });
                }
            };
            let found = at_path(&document, &location.path).ok_or_else(|| GraphError::Schema {
                message: format!(
                    "nothing at `{}` in the {} document",
                    location.path, location.format
                ),
            })?;
            let source = if location.file.is_some() {
                "document"
            } else {
                "resolver"
            };
            let inlined = inline_references(found, &document);
            let schema = if location.format == "openapi" {
                from_openapi(&inlined)
            } else {
                inlined
            };
            Ok(Resolved {
                schema,
                source,
                unresolved: Vec::new(),
            })
        }
        SchemaSource::Code(anchor) => code_schema(anchor, context),
    }
}

/// `file#Type`: by the language's resolver where one runs, otherwise by
/// Tree-sitter.
fn code_schema(anchor: &str, context: &mut Context<'_>) -> Result<Resolved, GraphError> {
    let (file, symbol) = anchor.split_once('#').ok_or_else(|| GraphError::Schema {
        message: format!("`{anchor}` is not a type anchor like `path/to/file.go#Type`"),
    })?;
    let extension = file.rsplit_once('.').map_or("", |(_, extension)| extension);
    let missing = || GraphError::Schema {
        message: format!("no type `{symbol}` in {file} or the files beside it"),
    };
    let checkout = context.checkout;
    match extension {
        "go" => {
            let package = directory_files(context.reads, checkout, file, "go", "_test.go")?;
            let found =
                go::struct_schema(&package, symbol, view(context.role)).ok_or_else(missing)?;
            Ok(syntax(found.schema, found.unresolved))
        }
        "rs" => {
            let module = directory_files(context.reads, checkout, file, "rs", "")?;
            let found =
                rust::type_schema(&module, symbol, view(context.role)).ok_or_else(missing)?;
            Ok(syntax(found.schema, found.unresolved))
        }
        "ts" | "tsx" | "mts" | "cts" => {
            let mut read = |path: &str| checkout.read(path);
            let found = typescript::type_schema(file, symbol, &mut read)?.ok_or_else(missing)?;
            Ok(syntax(found.schema, found.unresolved))
        }
        "py" => python_schema(file, symbol, context),
        _ => Err(GraphError::Schema {
            message: format!("no schema reader for `.{extension}` files yet ({anchor})"),
        }),
    }
}

/// A schema read with Tree-sitter.
fn syntax(schema: Value, unresolved: std::collections::BTreeSet<String>) -> Resolved {
    Resolved {
        schema,
        source: "tree-sitter",
        unresolved: unresolved.into_iter().collect(),
    }
}

/// A Python type by Pydantic in the repository's environment, or by
/// Tree-sitter when that cannot import it.
fn python_schema(
    file: &str,
    symbol: &str,
    context: &mut Context<'_>,
) -> Result<Resolved, GraphError> {
    let python = context.runtime.python.as_deref().unwrap_or("python3");
    let python = absolute_interpreter(python, &context.checkout.root);
    let interpreter = context.reads.interpreter(&python);
    let described = format!(
        "pydantic\n{}\n{python}\n{interpreter}\n{file}\n{symbol}\n{:?}",
        python::SCRIPT,
        context.runtime.env
    );
    let env = context.runtime.env.clone();
    let imported = kept_or_run(context, &described, true, |root| {
        python::type_schema(root, &python, file, symbol, &env)
    });
    let error = match imported {
        Ok(schema) => {
            return Ok(Resolved {
                schema: inline_references(&schema, &schema),
                source: "python",
                unresolved: Vec::new(),
            });
        }
        // Python could not import it: Tree-sitter reads it instead. Not
        // being able to get at the code at all is not that, and is reported.
        Err(error @ (GraphError::Resolver { .. } | GraphError::ResolverOutput { .. })) => error,
        Err(error) => return Err(error),
    };
    let package = directory_files(context.reads, context.checkout, file, "py", "")?;
    match python_syntax::type_schema(&package, symbol) {
        Some(found) => Ok(syntax(found.schema, found.unresolved)),
        None => Err(error),
    }
}

/// The files with `extension` in `file`'s directory at the checkout, but
/// not those ending in `excluded` (when it is not empty), `file` first.
fn directory_files(
    reads: &mut Reads,
    checkout: &Checkout,
    file: &str,
    extension: &str,
    excluded: &str,
) -> Result<Vec<(String, String)>, GraphError> {
    let directory = file.rsplit_once('/').map_or("", |(directory, _)| directory);
    let mut found: Vec<(String, String)> = reads
        .directory(checkout, directory, extension)?
        .iter()
        .filter(|(path, _)| excluded.is_empty() || !path.ends_with(excluded))
        .cloned()
        .collect();
    // The anchored file's definitions win over its neighbours'.
    found.sort_by_key(|(path, _)| path != file);
    Ok(found)
}

fn view(role: Role) -> shape::View {
    match role {
        Role::Sender => shape::View::Encode,
        Role::Receiver => shape::View::Decode,
    }
}

fn read_json(checkout: &Checkout, file: &str) -> Result<Value, GraphError> {
    let text = checkout.read(file)?.ok_or_else(|| GraphError::Schema {
        message: format!("no file {file}"),
    })?;
    serde_json::from_str(&text).map_err(|source| GraphError::ResolverOutput {
        command: format!("reading {file}"),
        source,
    })
}

/// Runs the operation's resolver at the checkout once, and parses its output.
fn run_resolver(resolver: &Resolver, context: &mut Context<'_>) -> Result<Value, GraphError> {
    let key = (
        context.checkout.root.to_string_lossy().into_owned(),
        context.checkout.commit.clone().unwrap_or_default(),
        resolver.command.clone(),
    );
    if let Some(output) = context.outputs.get(&key) {
        return Ok(output.clone());
    }
    let command = expand_runtime(&resolver.command, context.runtime, &context.checkout.root);
    let mut env = context.runtime.env.clone();
    env.extend(resolver.env.clone());
    let python = absolute_interpreter(
        context.runtime.python.as_deref().unwrap_or("python3"),
        &context.checkout.root,
    );
    let interpreter = if resolver.command.contains("{python}") {
        context.reads.interpreter(&python)
    } else {
        String::new()
    };
    let described = format!(
        "command\n{command}\n{env:?}\n{:?}\n{interpreter}",
        resolver.output
    );
    let document = kept_or_run(context, &described, resolver.cache, |root| {
        let stdout = repos::run(root, &command, &env)?;
        let text = match &resolver.output {
            Some(output) => {
                std::fs::read_to_string(root.join(output)).map_err(|source| GraphError::Io {
                    path: root.join(output),
                    source,
                })?
            }
            None => stdout,
        };
        serde_json::from_str(&text).map_err(|source| GraphError::ResolverOutput {
            command: resolver.command.clone(),
            source,
        })
    })?;
    context.outputs.insert(key, document.clone());
    Ok(document)
}

/// What `run` produces from the code at the checkout, `described` naming
/// how: kept in Lemma's cache by the code's Git tree, so a commit's output
/// is produced once. The working directory, when it is the commit
/// unchanged, is run in place; any other commit in the repository's one
/// shared worktree, never a worktree of its own. Only successes are kept.
fn kept_or_run(
    context: &mut Context<'_>,
    described: &str,
    may_keep: bool,
    run: impl FnOnce(&Path) -> Result<Value, GraphError>,
) -> Result<Value, GraphError> {
    let checkout = context.checkout;
    let state = match &checkout.commit {
        Some(commit) => Some(adapters::commit_state(&checkout.root, commit)?),
        None => context.reads.working_state(&checkout.root)?,
    };
    let kept = state.as_ref().filter(|_| may_keep).map(|state| {
        let described = format!(
            "{RESOLVER_PROTOCOL}\n{}\n{described}\n{}\n{state}",
            env!("CARGO_PKG_VERSION"),
            inherited_environment()
        );
        let key = lemma_model::content_hash(described.as_bytes());
        context
            .cache
            .join("resolvers")
            .join(&key[..2])
            .join(format!("{}.json", &key[2..]))
    });
    if let Some(found) = kept.as_deref().and_then(read_kept) {
        return Ok(found);
    }
    // Only now, on a miss, is the working directory's state worth asking.
    let is_checked_out = match &checkout.commit {
        None => true,
        Some(_) => state.is_some() && context.reads.working_state(&checkout.root)? == state,
    };
    let output = if is_checked_out {
        run(&checkout.root)?
    } else {
        let commit = checkout.commit.as_deref().unwrap_or("HEAD");
        let worktree = context
            .reads
            .worktree_at(&checkout.root, commit, context.cache)?;
        run(&worktree)?
    };
    if let Some(kept) = &kept {
        keep(kept, &output);
    }
    Ok(output)
}

/// The variables commands inherit that decide which interpreter and
/// libraries they use.
fn inherited_environment() -> String {
    ["PATH", "VIRTUAL_ENV", "PYTHONPATH"]
        .iter()
        .map(|name| format!("{name}={}", std::env::var(name).unwrap_or_default()))
        .collect::<Vec<_>>()
        .join("\n")
}

fn read_kept(path: &Path) -> Option<Value> {
    serde_json::from_str(&std::fs::read_to_string(path).ok()?).ok()
}

/// Keeps a produced output. A cache that cannot be written only costs the
/// next run another run, so failures are not errors.
fn keep(path: &Path, output: &Value) {
    let (Some(directory), Ok(text)) = (path.parent(), serde_json::to_string(output)) else {
        return;
    };
    let partial = path.with_extension(format!("{}.partial", std::process::id()));
    let _ = std::fs::create_dir_all(directory)
        .and_then(|()| std::fs::write(&partial, text))
        .and_then(|()| std::fs::rename(&partial, path));
}

/// `{python}` and the like in a command, replaced by the service's runtime.
fn expand_runtime(command: &str, runtime: &Runtime, repository: &Path) -> String {
    let python = absolute_interpreter(runtime.python.as_deref().unwrap_or("python3"), repository);
    let node = absolute_interpreter(runtime.node.as_deref().unwrap_or("node"), repository);
    command
        .replace("{python}", &python)
        .replace("{node}", &node)
        .replace("{go}", runtime.go.as_deref().unwrap_or("go"))
        .replace("{cargo}", runtime.cargo.as_deref().unwrap_or("cargo"))
}

/// An interpreter given relative to the repository (`.venv/bin/python`)
/// made absolute, so a worktree elsewhere still finds the environment.
pub(crate) fn absolute_interpreter(interpreter: &str, repository: &Path) -> String {
    if interpreter.contains('/') && !interpreter.starts_with('/') {
        return repository.join(interpreter).to_string_lossy().into_owned();
    }
    interpreter.to_owned()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn runtime_placeholders_use_the_repository_environment() {
        let runtime = Runtime {
            python: Some(".venv/bin/python".to_owned()),
            ..Runtime::default()
        };

        let command = expand_runtime(
            "{python} spec.py && {go} version",
            &runtime,
            Path::new("/repo"),
        );

        assert_eq!(command, "/repo/.venv/bin/python spec.py && go version");
    }
}
