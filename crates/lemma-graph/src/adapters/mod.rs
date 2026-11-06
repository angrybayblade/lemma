//! Adapters: reading a service's operations from its code rather than from
//! a committed document. An adapter imports the code in a separate process
//! and writes the `OpenAPI` document the framework itself describes for it,
//! each operation marked with `x-lemma-handler` (the file and line that
//! registers it). The protocol is in `docs/adapters.md`.
//!
//! Code is read where it already is: the working directory, uncommitted
//! changes included, which is a change's head. A commit's document is kept
//! by its tree, so it is generated once: whenever an adapter reads a clean
//! working directory, its document is that commit's too. Only a commit no
//! one has read that way is built, in one reused worktree per repository
//! where `setup` installs what importing the code needs.

mod cache;
pub mod overlay;
mod registry;

pub(crate) use cache::{commit_state, working_state};

use std::collections::{BTreeMap, BTreeSet};
use std::path::{Path, PathBuf};

use lemma_verify::GeneratedDocument;
use serde_json::Value;

use crate::GraphError;
use crate::config::{LemmaFile, OperationsFrom, Runtime};
use crate::repos::{self, Checkout};
use overlay::{Coverage, Reading};
use registry::Script;

/// How long an adapter may run before it is stopped, unless configured.
const ADAPTER_TIMEOUT_MS: u64 = 300_000;

/// How long `setup` may run before it is stopped.
const SETUP_TIMEOUT_MS: u64 = 1_800_000;

/// Whether a commit's document may be built when none is kept for it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Build {
    /// Build it in a worktree, running `setup`.
    Allowed,
    /// Use only a kept document, or the working directory when it is that
    /// commit unchanged.
    KeptOnly,
}

/// A document an adapter generated, and what it said about its reading.
#[derive(Debug, Clone, PartialEq)]
pub struct Generated {
    /// The `OpenAPI` document.
    pub document: Value,
    /// A digest of its text, which changes whenever it does.
    pub digest: String,
    /// What it could not read, for showing to a person.
    pub unread: Vec<String>,
    /// What laying it over committed documents needs to know.
    pub reading: Reading,
}

/// The names of Lemma's built-in adapters.
pub fn built_in_names() -> impl Iterator<Item = &'static str> {
    registry::BUILT_IN.iter().map(|adapter| adapter.name)
}

/// The document `from` generates from the code at `checkout`; `None` for a
/// commit with no kept document when `build` does not allow building one.
pub fn generate(
    from: &OperationsFrom,
    runtime: &Runtime,
    checkout: &Checkout,
    cache: &Path,
    build: Build,
) -> Result<Option<Generated>, GraphError> {
    let Some(commit) = &checkout.commit else {
        return in_working_directory(from, runtime, &checkout.root, cache).map(Some);
    };
    let store = cache.join("adapters");
    create_directory(&store)?;
    let adapter = Adapter::new(from, runtime, &store)?;
    adapter.at_commit(&checkout.root, commit, cache, build)
}

/// The document `from` generates from the code in the working directory at
/// `root`, uncommitted changes included.
pub fn in_working_directory(
    from: &OperationsFrom,
    runtime: &Runtime,
    root: &Path,
    cache: &Path,
) -> Result<Generated, GraphError> {
    let store = cache.join("adapters");
    create_directory(&store)?;
    Adapter::new(from, runtime, &store)?.in_working_directory(root)
}

/// An adapter ready to run: its configuration, its command, and where its
/// documents are kept.
struct Adapter<'a> {
    from: &'a OperationsFrom,
    runtime: &'a Runtime,
    store: &'a Path,
    script: Script<'a>,
    key: String,
}

impl<'a> Adapter<'a> {
    fn new(
        from: &'a OperationsFrom,
        runtime: &'a Runtime,
        store: &'a Path,
    ) -> Result<Self, GraphError> {
        let script = registry::script(from)?;
        if let Script::BuiltIn(built_in) = &script {
            write_script(store, built_in.file, built_in.text)?;
        }
        let key = cache::adapter_key(from, runtime, script.key_text());
        Ok(Self {
            from,
            runtime,
            store,
            script,
            key,
        })
    }

    fn output(&self, state: &str) -> PathBuf {
        self.store
            .join(format!("{}.json", cache::key(&self.key, state)))
    }

    /// The document of the code in the working directory at `root`.
    fn in_working_directory(&self, root: &Path) -> Result<Generated, GraphError> {
        let Some(state) = cache::working_state(root)? else {
            log::info!(
                "adapter: reading {} in its working directory (not kept: not a Git repository)",
                root.display()
            );
            let output = self.store.join(format!("{}-uncached.json", self.key));
            self.run(root, root, &output)?;
            return read_generated(&output);
        };
        let output = self.output(&state);
        if output.exists() {
            log::info!(
                "adapter: {} working directory unchanged since last read; kept document",
                root.display()
            );
            return read_generated(&output);
        }
        log::info!(
            "adapter: reading {} in its working directory",
            root.display()
        );
        self.run(root, root, &output)?;
        read_generated(&output)
    }

    /// The document of the code at `commit` of the clone at `root`.
    fn at_commit(
        &self,
        root: &Path,
        commit: &str,
        cache: &Path,
        build: Build,
    ) -> Result<Option<Generated>, GraphError> {
        let state = cache::commit_state(root, commit)?;
        let output = self.output(&state);
        let short = short_commit(commit);
        if output.exists() {
            log::info!("adapter: {} at {short}: kept document", root.display());
            return read_generated(&output).map(Some);
        }
        if cache::working_state(root)?.as_deref() == Some(state.as_str()) {
            log::info!(
                "adapter: {} at {short}: working directory is that commit; reading it there",
                root.display()
            );
            self.run(root, root, &output)?;
            return read_generated(&output).map(Some);
        }
        if build == Build::KeptOnly {
            log::info!(
                "adapter: {} at {short}: no kept document and not built for a pinned \
                 repository; committed documents stand",
                root.display()
            );
            return Ok(None);
        }
        log::info!(
            "adapter: {} at {short}: no kept document; building it in Lemma's worktree",
            root.display()
        );
        let worktree = repos::SharedWorktree::check_out(root, commit, cache)?;
        self.set_up(worktree.path())?;
        self.run(root, worktree.path(), &output)?;
        read_generated(&output).map(Some)
    }

    /// Runs `setup` in a worktree of a commit. A reused worktree keeps what
    /// earlier runs installed, so installing again is mostly a check.
    fn set_up(&self, worktree: &Path) -> Result<(), GraphError> {
        let Some(setup) = &self.from.setup else {
            return Ok(());
        };
        let directory = directory(self.from, worktree)?;
        let env = self.env(worktree, worktree, Path::new(""));
        log::debug!("adapter setup in {}: {setup}", directory.display());
        let started = std::time::Instant::now();
        repos::run_within(&directory, setup, &env, Some(SETUP_TIMEOUT_MS))?;
        log::info!(
            "adapter: setup finished in {:.1}s",
            started.elapsed().as_secs_f64()
        );
        Ok(())
    }

    /// Runs the adapter on the code at `code` (the clone at `repository`,
    /// or a worktree of it), keeping its document at `output` only if it
    /// succeeds: a failed or stopped run leaves nothing to be mistaken for
    /// a document later.
    fn run(&self, repository: &Path, code: &Path, output: &Path) -> Result<(), GraphError> {
        let directory = directory(self.from, code)?;
        let written = output.with_extension(format!("{}.partial", std::process::id()));
        let env = self.env(repository, code, &written);
        let command = self.command(repository, code);
        let timeout_ms = self.from.timeout_ms.unwrap_or(ADAPTER_TIMEOUT_MS);
        log::debug!("adapter command in {}: {command}", directory.display());
        let started = std::time::Instant::now();
        let result = repos::run_within(&directory, &command, &env, Some(timeout_ms))
            .and_then(|_| read_generated(&written).map(|_| ()))
            .map_err(|error| match error {
                GraphError::Io { path, .. } if path == written => GraphError::Resolver {
                    command: command.clone(),
                    directory: directory.clone(),
                    message: "the adapter wrote no document to $LEMMA_OUTPUT".to_owned(),
                },
                other => other,
            });
        let seconds = started.elapsed().as_secs_f64();
        if let Err(error) = result {
            log::debug!("adapter failed after {seconds:.1}s");
            // The partial file may not exist; either way it is not a
            // document.
            let _ = std::fs::remove_file(&written);
            return Err(error);
        }
        log::info!("adapter: read the code in {seconds:.1}s");
        std::fs::rename(&written, output).map_err(|source| GraphError::Io {
            path: output.to_owned(),
            source,
        })
    }

    /// The adapter's environment: the service's runtime environment, the
    /// adapter's own, and the protocol's variables. In a worktree, a Python
    /// adapter imports the worktree's code before any installed copy of it.
    fn env(&self, repository: &Path, code: &Path, output: &Path) -> BTreeMap<String, String> {
        let mut env = self.runtime.env.clone();
        env.extend(self.from.env.clone());
        env.insert("LEMMA_OUTPUT".to_owned(), output.display().to_string());
        env.insert("LEMMA_ENTRY".to_owned(), self.from.entry.join("\n"));
        env.insert("LEMMA_EXCLUDE".to_owned(), self.from.exclude.join("\n"));
        env.insert("LEMMA_ROOT".to_owned(), code.display().to_string());
        if code != repository {
            let mut first = vec![directory(self.from, code).unwrap_or_else(|_| code.to_owned())];
            first.push(code.join("src"));
            let mut paths: Vec<String> = first
                .iter()
                .map(|path| path.display().to_string())
                .collect();
            paths.extend(env.get("PYTHONPATH").cloned());
            env.insert("PYTHONPATH".to_owned(), paths.join(":"));
        }
        env
    }

    /// The command to run on the code at `code`: a built-in adapter's
    /// script, a script in that code, or the configured command.
    /// Interpreters are the clone's own, at `repository`.
    fn command(&self, repository: &Path, code: &Path) -> String {
        match &self.script {
            Script::BuiltIn(built_in) => registry::invocation(
                built_in.interpreter,
                &self.store.join(built_in.file),
                self.runtime,
                repository,
            ),
            Script::InRepository { path, interpreter } => {
                registry::invocation(*interpreter, &code.join(path), self.runtime, repository)
            }
            Script::Command(command) => (*command).to_owned(),
        }
    }
}

fn write_script(store: &Path, file: &str, text: &str) -> Result<PathBuf, GraphError> {
    let path = store.join(file);
    if std::fs::read_to_string(&path).ok().as_deref() == Some(text) {
        return Ok(path);
    }
    std::fs::write(&path, text).map_err(|source| GraphError::Io {
        path: path.clone(),
        source,
    })?;
    Ok(path)
}

/// A commit as logs show it: its first ten characters.
fn short_commit(commit: &str) -> String {
    commit.chars().take(10).collect()
}

/// Where the adapter runs in the code at `code`: its `directory`, which
/// must stay inside the repository.
fn directory(from: &OperationsFrom, code: &Path) -> Result<PathBuf, GraphError> {
    let Some(directory) = &from.directory else {
        return Ok(code.to_owned());
    };
    let relative = Path::new(directory);
    let escapes = relative.is_absolute()
        || relative
            .components()
            .any(|part| matches!(part, std::path::Component::ParentDir));
    if escapes {
        return Err(GraphError::Adapter {
            message: format!("`directory: {directory}` must be inside the repository"),
        });
    }
    Ok(code.join(relative))
}

fn create_directory(path: &Path) -> Result<(), GraphError> {
    std::fs::create_dir_all(path).map_err(|source| GraphError::Io {
        path: path.to_owned(),
        source,
    })
}

/// A generated document read back, with its reading.
fn read_generated(path: &Path) -> Result<Generated, GraphError> {
    let text = std::fs::read_to_string(path).map_err(|source| GraphError::Io {
        path: path.to_owned(),
        source,
    })?;
    let document: Value =
        serde_json::from_str(&text).map_err(|source| GraphError::ResolverOutput {
            command: format!("reading {}", path.display()),
            source,
        })?;
    let (unread, reading) = reading_of(&document);
    Ok(Generated {
        document,
        digest: lemma_model::content_hash(text.as_bytes()),
        unread,
        reading,
    })
}

/// What a generated document says about its reading: `x-lemma-unread`
/// lists what could not be read, each a module (`{"module", "reason"}`, or
/// a plain string) or a route (`{"method", "path", "reason"}`, with
/// `"prefix": true` for every route under the path), and
/// `x-lemma-complete: true` claims every route was read. A module that
/// could not be read makes the reading incomplete, whatever the claim.
/// `x-lemma-schemas: false` says the document describes routes only.
fn reading_of(document: &Value) -> (Vec<String>, Reading) {
    let entries = document
        .get("x-lemma-unread")
        .and_then(Value::as_array)
        .cloned()
        .unwrap_or_default();
    let mut shown = Vec::new();
    let mut unread_routes = BTreeSet::new();
    let mut unread_prefixes = BTreeSet::new();
    let mut has_unread_module = false;
    for entry in &entries {
        let text = |key: &str| entry.get(key).and_then(Value::as_str);
        let reason = text("reason").unwrap_or("no reason given");
        let route = text("method").zip(text("path").and_then(lemma_http::canonical_route));
        if let Some((method, path)) = route {
            shown.push(format!(
                "could not describe {} {}: {reason}",
                method.to_uppercase(),
                text("path").unwrap_or_default()
            ));
            let is_prefix = entry.get("prefix") == Some(&Value::Bool(true));
            if is_prefix {
                unread_prefixes.insert((method.to_uppercase(), path));
            } else {
                unread_routes.insert((method.to_uppercase(), path));
            }
            continue;
        }
        has_unread_module = true;
        match (entry.as_str(), text("module")) {
            (Some(plain), _) => shown.push(plain.to_owned()),
            (None, Some(module)) => shown.push(format!("could not import {module}: {reason}")),
            (None, None) => shown.push(entry.to_string()),
        }
    }
    let claims_complete = document.get("x-lemma-complete") == Some(&Value::Bool(true));
    let reading = Reading {
        unread_routes,
        unread_prefixes,
        is_complete: claims_complete && !has_unread_module,
        is_routes_only: document.get("x-lemma-schemas") == Some(&Value::Bool(false)),
    };
    (shown, reading)
}

/// The document each local service with `operations_from` in `file`
/// generates from the code at `root` as it is on disk, by service name.
/// A service whose adapter fails keeps its error, so the caller can fall
/// back to the committed documents and say why.
pub fn from_working_directory(
    file: &LemmaFile,
    root: &Path,
    cache: &Path,
) -> BTreeMap<String, Result<Generated, GraphError>> {
    let mut documents = BTreeMap::new();
    for (name, service) in &file.services {
        let Some(from) = &service.exports.operations_from else {
            continue;
        };
        if name.starts_with('#') {
            continue;
        }
        let generated = in_working_directory(from, &service.runtime, root, cache);
        documents.insert(name.clone(), generated);
    }
    documents
}

/// A service's committed documents at `checkout`, by name, parsed from
/// JSON or YAML; documents missing there are left out.
pub fn committed_documents(
    checkout: &Checkout,
    names: &[String],
) -> Result<Vec<(String, Value)>, GraphError> {
    let mut documents = Vec::new();
    for name in names {
        let Some(text) = checkout.read(name)? else {
            continue;
        };
        documents.push((name.clone(), parse_document(name, &text)?));
    }
    Ok(documents)
}

/// An `OpenAPI` document's text, as JSON, or YAML for a `.yaml` or `.yml`
/// file.
pub fn parse_document(name: &str, text: &str) -> Result<Value, GraphError> {
    if lemma_http::is_yaml_document(name) {
        return yaml_serde::from_str(text).map_err(|source| GraphError::Schema {
            message: format!("could not read {name} as YAML: {source}"),
        });
    }
    serde_json::from_str(text).map_err(|source| GraphError::ResolverOutput {
        command: format!("reading {name}"),
        source,
    })
}

/// Where the code registers each operation of `document`, as an adapter
/// marked it with `x-lemma-handler` (`file:line`), by method and canonical
/// path.
pub fn handler_sites(document: &Value) -> BTreeMap<(String, String), (String, u32)> {
    let mut sites = BTreeMap::new();
    let Some(paths) = document.get("paths").and_then(Value::as_object) else {
        return sites;
    };
    for (template, item) in paths {
        let (Some(path), Some(methods)) = (lemma_http::canonical_route(template), item.as_object())
        else {
            continue;
        };
        for (method, operation) in methods {
            let Some(tag) = operation.get("x-lemma-handler").and_then(Value::as_str) else {
                continue;
            };
            let Some((file, line)) = tag.rsplit_once(':') else {
                continue;
            };
            let Ok(line) = line.parse() else {
                continue;
            };
            sites.insert(
                (method.to_uppercase(), path.clone()),
                (file.to_owned(), line),
            );
        }
    }
    sites
}

/// A service's contract as read from its code at one checkout.
#[derive(Debug, Clone, PartialEq)]
pub struct Contract {
    /// The documents to check against, each standing for a committed one.
    pub documents: Vec<GeneratedDocument>,
    /// What the adapter could not read.
    pub unread: Vec<String>,
    /// How the code compared with the committed documents.
    pub coverage: Coverage,
}

/// The contract of `service` that `generated` gives, laid over its
/// `committed` documents, each written to `cache` under a name its content
/// decides, so services and runs never write over each other's.
pub fn contract(
    service: &str,
    generated: &Generated,
    committed: &[(String, Value)],
    cache: &Path,
) -> Result<Contract, GraphError> {
    let store = cache.join("contracts");
    create_directory(&store)?;
    let from_code_name = format!("{service} (from code)");
    let laid = overlay::overlay(
        committed,
        &generated.document,
        &generated.reading,
        &from_code_name,
    );
    let mut documents = Vec::new();
    for (name, document) in &laid.documents {
        let text =
            serde_json::to_string(document).map_err(|source| GraphError::ResolverOutput {
                command: format!("writing the contract for {name}"),
                source,
            })?;
        let path = store.join(format!(
            "{}.json",
            lemma_model::content_hash(text.as_bytes())
        ));
        if !path.exists() {
            let partial = path.with_extension(format!("{}.partial", std::process::id()));
            std::fs::write(&partial, &text)
                .and_then(|()| std::fs::rename(&partial, &path))
                .map_err(|source| GraphError::Io {
                    path: path.clone(),
                    source,
                })?;
        }
        documents.push(GeneratedDocument {
            name: name.clone(),
            path,
        });
    }
    Ok(Contract {
        documents,
        unread: generated.unread.clone(),
        coverage: laid.coverage,
    })
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::*;

    #[test]
    fn an_unread_module_makes_a_reading_incomplete_whatever_it_claims() {
        let document = json!({
            "x-lemma-complete": true,
            "x-lemma-unread": [
                {"method": "post", "path": "/a/{id}", "reason": "lazy schema"},
                {"module": "src/b.ts", "reason": "syntax error"}
            ]
        });

        let (shown, reading) = reading_of(&document);

        assert!(!reading.is_complete);
        assert!(
            reading
                .unread_routes
                .contains(&("POST".to_owned(), "/a/{}".to_owned()))
        );
        assert_eq!(shown.len(), 2);
    }

    #[test]
    fn unread_routes_alone_keep_a_reading_complete() {
        let document = json!({
            "x-lemma-complete": true,
            "x-lemma-unread": [{"method": "get", "path": "/a", "reason": "lazy"}]
        });

        let (_, reading) = reading_of(&document);

        assert!(reading.is_complete);
    }

    #[test]
    fn a_directory_outside_the_repository_is_refused() {
        let from = OperationsFrom {
            directory: Some("../elsewhere".to_owned()),
            ..OperationsFrom::default()
        };

        assert!(directory(&from, Path::new("/repo")).is_err());
    }
}
