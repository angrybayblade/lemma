//! Loading a program: the entry module, the modules it imports, and the
//! root module of every repository it names.
//!
//! A module's name is fixed by its file's place in its repository:
//! `lemma.lm` at the root is named after the repository, and
//! `checks/replay.lm` in `functions` is `functions.checks.replay`. Imports
//! name modules, so loading turns each name into a repository and a path
//! and asks [`Sources`] for the text. Repositories come from `repo`
//! declarations: the entry module's workspace first, then the top of each
//! root module, the first declaration of a name winning (spec 13 §2).

use std::collections::{BTreeMap, BTreeSet, VecDeque};
use std::sync::Arc;

use crate::diagnostic::Diagnostic;
use crate::span::{LineIndex, Span};
use crate::stdlib;
use crate::syntax::ast::{Expression, ExpressionKind, File, Item, ModuleName};
use crate::syntax::parse;

/// The file every repository's root module is in.
pub const ROOT_FILE: &str = "lemma.lm";

/// Where a repository comes from, as a `repo` declaration says.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum RepositorySource {
    /// A working tree, `path` relative to the declaring module's file.
    Local {
        /// The path as written.
        path: String,
        /// The repository holding the declaring module.
        declared_in: String,
        /// The declaring module's file, relative to that repository.
        declaring_file: String,
    },
    /// A clone of `url`, read at `rev`.
    Git {
        /// The URL.
        url: String,
        /// The commit, branch or tag; the default branch if absent.
        rev: Option<String>,
        /// The repository holding the declaring module.
        declared_in: String,
    },
}

/// A repository the program reads from.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Repository {
    /// Its name, its root module's.
    pub name: String,
    /// Where it comes from; `None` for the entry module's repository.
    pub source: Option<RepositorySource>,
    /// Whether the entry module's workspace declared it, rather than a
    /// root module's default.
    pub is_authoritative: bool,
}

/// Why a module's text could not be read.
#[derive(Debug, thiserror::Error)]
#[error("{message}")]
pub struct SourceError {
    /// What went wrong, in a sentence.
    pub message: String,
}

/// Reads module texts. The command line reads working trees and commits;
/// the language server adds unsaved buffers; tests use memory.
pub trait Sources {
    /// The text of `path` (relative to the repository's root, separated by
    /// `/`) in `repository`; `None` when there is no such file.
    fn read(&mut self, repository: &Repository, path: &str) -> Result<Option<String>, SourceError>;
}

/// One loaded module.
#[derive(Debug, Clone)]
pub struct Module {
    /// Its name.
    pub name: String,
    /// The repository it is in; `lemma` for the standard library.
    pub repository: String,
    /// Its file, relative to the repository's root.
    pub path: String,
    /// Its text.
    pub text: Arc<str>,
    /// Its syntax tree.
    pub file: File,
    /// Line starts, for positions.
    pub lines: LineIndex,
}

/// The modules of one program, before checking.
#[derive(Debug, Clone, Default)]
pub struct Loaded {
    /// The modules, by name.
    pub modules: BTreeMap<String, Module>,
    /// The entry module's name.
    pub entry: String,
    /// The repositories, by name.
    pub repositories: BTreeMap<String, Repository>,
    /// Problems reading and parsing.
    pub diagnostics: Vec<Diagnostic>,
}

/// Loads the program whose entry module is the file `entry_path` of the
/// entry repository (the one [`Sources`] reads for a source of `None`).
pub fn load(sources: &mut dyn Sources, entry_path: &str) -> Loaded {
    let mut loader = Loader {
        sources,
        loaded: Loaded::default(),
        queue: VecDeque::new(),
        waiting: Vec::new(),
        requested: BTreeSet::new(),
    };
    loader.load_entry(entry_path);
    loader.run();
    loader.loaded
}

/// Where a module is: its name, repository and file.
struct Place {
    name: String,
    repository: String,
    path: String,
}

/// The module declaring a repository, and where that module is.
#[derive(Clone, Copy)]
struct Declaring<'s> {
    module: &'s str,
    repository: &'s str,
    file: &'s str,
}

/// A module to load, and the import that asked for it.
struct Request {
    module: String,
    from: Option<(String, Span)>,
}

struct Loader<'a> {
    sources: &'a mut dyn Sources,
    loaded: Loaded,
    queue: VecDeque<Request>,
    /// Requests for modules of repositories not declared yet.
    waiting: Vec<Request>,
    requested: BTreeSet<String>,
}

impl Loader<'_> {
    fn load_entry(&mut self, entry_path: &str) {
        let entry = Repository {
            name: String::new(),
            source: None,
            is_authoritative: true,
        };
        let text = match self.sources.read(&entry, entry_path) {
            Ok(Some(text)) => text,
            Ok(None) => {
                self.report(Diagnostic::error(
                    Span::default(),
                    format!("there is no file `{entry_path}`"),
                ));
                return;
            }
            Err(error) => {
                self.report(Diagnostic::error(Span::default(), error.to_string()));
                return;
            }
        };
        let parsed = parse(&text);
        let Some(declared) = parsed.file.module.as_ref().map(ModuleName::dotted) else {
            self.loaded.diagnostics.extend(parsed.diagnostics);
            return;
        };
        let repository = repository_of(&declared, entry_path);
        if let Err(message) = check_name(&declared, &repository, entry_path) {
            let span = parsed
                .file
                .module
                .as_ref()
                .map_or(Span::default(), |name| name.span);
            self.loaded
                .diagnostics
                .push(Diagnostic::error(span, message).in_module(&declared));
        }
        self.loaded.repositories.insert(
            repository.clone(),
            Repository {
                name: repository.clone(),
                source: None,
                is_authoritative: true,
            },
        );
        self.loaded.entry.clone_from(&declared);
        self.requested.insert(declared.clone());
        let declaring = Declaring {
            module: &declared,
            repository: &repository,
            file: entry_path,
        };
        self.add_workspace_repositories(&parsed.file, declaring);
        let place = Place {
            name: declared,
            repository,
            path: entry_path.to_owned(),
        };
        self.add_module(place, text, parsed);
    }

    fn run(&mut self) {
        loop {
            while let Some(request) = self.queue.pop_front() {
                self.load_request(request);
            }
            let repositories = &self.loaded.repositories;
            let (ready, still_waiting): (Vec<Request>, Vec<Request>) =
                std::mem::take(&mut self.waiting)
                    .into_iter()
                    .partition(|request| {
                        repositories.contains_key(&first_segment(&request.module))
                    });
            self.waiting = still_waiting;
            if ready.is_empty() {
                break;
            }
            self.queue.extend(ready);
        }
        for request in std::mem::take(&mut self.waiting) {
            let repository = first_segment(&request.module);
            self.report_at(
                request.from.as_ref(),
                format!(
                    "no repository `{repository}` is declared; add `repo {repository} = local(\"...\")` to the workspace"
                ),
            );
        }
    }

    fn load_request(&mut self, request: Request) {
        if let Some(text) = stdlib::source(&request.module) {
            let parsed = parse(text);
            let path = format!("{}.lm", request.module.replace('.', "/"));
            let place = Place {
                name: request.module,
                repository: stdlib::REPOSITORY.to_owned(),
                path,
            };
            self.add_module(place, text.to_owned(), parsed);
            return;
        }
        if request.module.starts_with("lemma.") || request.module == stdlib::REPOSITORY {
            let message = format!("the standard library has no module `{}`", request.module);
            self.report_at(request.from.as_ref(), message);
            return;
        }
        let repository_name = first_segment(&request.module);
        let Some(repository) = self.loaded.repositories.get(&repository_name).cloned() else {
            self.waiting.push(request);
            return;
        };
        let path = path_of(&request.module);
        let text = match self.sources.read(&repository, &path) {
            Ok(Some(text)) => text,
            Ok(None) => {
                let message = format!(
                    "module `{}` is not there: repository `{repository_name}` has no file `{path}`",
                    request.module
                );
                self.report_at(request.from.as_ref(), message);
                return;
            }
            Err(error) => {
                self.report_at(request.from.as_ref(), error.to_string());
                return;
            }
        };
        let parsed = parse(&text);
        self.check_declared_name(&parsed.file, &request.module, &repository_name, &path);
        if path == ROOT_FILE {
            let declaring = Declaring {
                module: &request.module,
                repository: &repository_name,
                file: &path,
            };
            self.add_default_repositories(&parsed.file, declaring);
        }
        let place = Place {
            name: request.module,
            repository: repository_name,
            path,
        };
        self.add_module(place, text, parsed);
    }

    /// Reports a `module` line that does not match the file's place.
    fn check_declared_name(&mut self, file: &File, expected: &str, repository: &str, path: &str) {
        let Some(name) = file
            .module
            .as_ref()
            .filter(|name| name.dotted() != expected)
        else {
            return;
        };
        self.loaded.diagnostics.push(
            Diagnostic::error(
                name.span,
                format!(
                    "this file is `{path}` in `{repository}`, so its module is named `{expected}`"
                ),
            )
            .in_module(expected),
        );
    }

    fn add_module(&mut self, place: Place, text: String, parsed: crate::syntax::Parsed) {
        let Place {
            name,
            repository,
            path,
        } = place;
        for diagnostic in parsed.diagnostics {
            self.loaded.diagnostics.push(diagnostic.in_module(&name));
        }
        for (module, span) in imported_modules(&parsed.file) {
            self.request(module, Some((name.clone(), span)));
        }
        self.loaded.modules.insert(
            name.clone(),
            Module {
                name,
                repository,
                path,
                lines: LineIndex::new(&text),
                text: Arc::from(text),
                file: parsed.file,
            },
        );
    }

    fn request(&mut self, module: String, from: Option<(String, Span)>) {
        if self.requested.insert(module.clone()) {
            self.queue.push_back(Request { module, from });
        }
    }

    fn add_workspace_repositories(&mut self, file: &File, declaring: Declaring) {
        let declared = file
            .items
            .iter()
            .filter_map(|item| match item {
                Item::Workspace(workspace) => Some(&workspace.repos),
                _ => None,
            })
            .flatten();
        for repo in declared {
            self.declare(repo, declaring, true);
        }
        self.add_default_repositories(file, declaring);
    }

    /// A root module's top-level `repo` declarations: defaults.
    fn add_default_repositories(&mut self, file: &File, declaring: Declaring) {
        let declared = file.items.iter().filter_map(|item| match item {
            Item::Repo(repo) => Some(repo),
            _ => None,
        });
        for repo in declared {
            self.declare(repo, declaring, false);
        }
    }

    fn declare(
        &mut self,
        repo: &crate::syntax::ast::Repo,
        declaring: Declaring,
        is_authoritative: bool,
    ) {
        let name = repo.name.text.clone();
        if self.loaded.repositories.contains_key(&name) {
            return;
        }
        let Some(source) = repository_source(&repo.source, declaring.repository, declaring.file)
        else {
            self.loaded.diagnostics.push(
                Diagnostic::error(
                    repo.source.span,
                    "a repository comes from `local(\"path\")` or `git(url: \"...\", rev: \"...\")`",
                )
                .in_module(declaring.module),
            );
            return;
        };
        self.loaded.repositories.insert(
            name.clone(),
            Repository {
                name: name.clone(),
                source: Some(source),
                is_authoritative,
            },
        );
        self.request(name, Some((declaring.module.to_owned(), repo.name.span)));
    }

    fn report(&mut self, diagnostic: Diagnostic) {
        self.loaded.diagnostics.push(diagnostic);
    }

    fn report_at(&mut self, from: Option<&(String, Span)>, message: String) {
        let diagnostic = match from {
            Some((module, span)) => Diagnostic::error(*span, message).in_module(module),
            None => Diagnostic::error(Span::default(), message),
        };
        self.loaded.diagnostics.push(diagnostic);
    }
}

/// The modules a file imports or re-exports from, with where.
fn imported_modules(file: &File) -> Vec<(String, Span)> {
    let mut modules = Vec::new();
    for item in &file.items {
        match item {
            Item::Import(import) => modules.push((import.module.dotted(), import.module.span)),
            Item::Reexport(reexport) => {
                modules.push((reexport.module.dotted(), reexport.module.span));
            }
            _ => {}
        }
    }
    modules
}

fn repository_source(
    expression: &Expression,
    declared_in: &str,
    declaring_file: &str,
) -> Option<RepositorySource> {
    let ExpressionKind::Call { callee, arguments } = &expression.kind else {
        return None;
    };
    let ExpressionKind::Name(function) = &callee.kind else {
        return None;
    };
    let text = |name: &str, position: usize| {
        arguments
            .iter()
            .enumerate()
            .find(|(index, argument)| match &argument.name {
                Some(written) => written.text == name,
                None => *index == position,
            })
            .and_then(|(_, argument)| match &argument.value.kind {
                ExpressionKind::Text(text) => Some(text.clone()),
                _ => None,
            })
    };
    match function.as_str() {
        "local" => Some(RepositorySource::Local {
            path: text("path", 0)?,
            declared_in: declared_in.to_owned(),
            declaring_file: declaring_file.to_owned(),
        }),
        "git" => Some(RepositorySource::Git {
            url: text("url", 0)?,
            rev: text("rev", 1),
            declared_in: declared_in.to_owned(),
        }),
        _ => None,
    }
}

fn first_segment(module: &str) -> String {
    module.split('.').next().unwrap_or(module).to_owned()
}

/// The file a module is in, relative to its repository's root.
pub fn path_of(module: &str) -> String {
    match module.split_once('.') {
        None => ROOT_FILE.to_owned(),
        Some((_, rest)) => format!("{}.lm", rest.replace('.', "/")),
    }
}

/// The repository an entry module declaring `declared` at `path` is in.
fn repository_of(declared: &str, path: &str) -> String {
    let segments: Vec<&str> = declared.split('.').collect();
    let is_standalone = segments.len() == 1 && path != ROOT_FILE;
    if is_standalone {
        return declared.to_owned();
    }
    segments.first().copied().unwrap_or_default().to_owned()
}

/// Whether the entry module's declared name matches its file. A single
/// file outside any repository layout, such as `contracts.lm` declaring
/// `module contracts`, stands alone as its own root.
fn check_name(declared: &str, repository: &str, path: &str) -> Result<(), String> {
    if declared == repository && !path.contains('/') {
        let stem = path.strip_suffix(".lm").unwrap_or(path);
        if path == ROOT_FILE || stem == declared {
            return Ok(());
        }
    }
    let expected = path_of(declared);
    if expected == path {
        return Ok(());
    }
    Err(format!(
        "this file is `{path}`, so its module is named `{repository}.{}`",
        path.strip_suffix(".lm").unwrap_or(path).replace('/', ".")
    ))
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Repositories in memory: (repository, path) to text; the entry
    /// repository is "".
    struct Memory(BTreeMap<(String, String), String>);

    impl Sources for Memory {
        fn read(
            &mut self,
            repository: &Repository,
            path: &str,
        ) -> Result<Option<String>, SourceError> {
            let key = match &repository.source {
                None => String::new(),
                Some(_) => repository.name.clone(),
            };
            Ok(self.0.get(&(key, path.to_owned())).cloned())
        }
    }

    fn memory(files: &[(&str, &str, &str)]) -> Memory {
        Memory(
            files
                .iter()
                .map(|(repository, path, text)| {
                    (
                        ((*repository).to_owned(), (*path).to_owned()),
                        (*text).to_owned(),
                    )
                })
                .collect(),
        )
    }

    #[test]
    fn imports_load_modules_from_declared_repositories_and_the_standard_library() {
        let mut sources = memory(&[
            (
                "",
                "lemma.lm",
                "module example\nworkspace {\n    repo functions = local(\"../functions\")\n}\nimport { Functions } from functions\nimport lemma.links\n",
            ),
            (
                "functions",
                "lemma.lm",
                "module functions\nexport service Functions {}\n",
            ),
        ]);

        let loaded = load(&mut sources, "lemma.lm");

        assert_eq!(loaded.diagnostics, []);
        let names: Vec<&str> = loaded.modules.keys().map(String::as_str).collect();
        assert_eq!(names, ["example", "functions", "lemma.links"]);
        assert_eq!(loaded.entry, "example");
    }

    #[test]
    fn a_root_module_pins_repositories_the_workspace_does_not_name() {
        let mut sources = memory(&[
            (
                "",
                "lemma.lm",
                "module example\nworkspace {\n    repo platform = local(\"../platform\")\n}\n",
            ),
            (
                "platform",
                "lemma.lm",
                "module platform\nrepo functions = git(url: \"https://git.example.com/f.git\", rev: \"abc\")\nrepo example = local(\"elsewhere\")\n",
            ),
            ("functions", "lemma.lm", "module functions\n"),
        ]);

        let loaded = load(&mut sources, "lemma.lm");

        assert_eq!(loaded.diagnostics, []);
        assert!(loaded.modules.contains_key("functions"));
        assert!(!loaded.repositories["functions"].is_authoritative);
        assert_eq!(loaded.repositories["example"].source, None);
    }

    #[test]
    fn a_module_named_against_its_file_is_reported() {
        let mut sources = memory(&[
            (
                "",
                "lemma.lm",
                "module example\nimport example.checks.replay\n",
            ),
            ("example", "checks/replay.lm", "module example.replay\n"),
        ]);
        sources.0.insert(
            (String::new(), "checks/replay.lm".to_owned()),
            "module example.replay\n".to_owned(),
        );

        let loaded = load(&mut sources, "lemma.lm");

        assert_eq!(loaded.diagnostics.len(), 1);
        assert!(
            loaded.diagnostics[0]
                .message
                .contains("example.checks.replay")
        );
    }

    #[test]
    fn an_undeclared_repository_is_reported_at_the_import() {
        let mut sources = memory(&[("", "lemma.lm", "module example\nimport functions\n")]);

        let loaded = load(&mut sources, "lemma.lm");

        assert!(
            loaded.diagnostics[0]
                .message
                .contains("no repository `functions`")
        );
        assert_eq!(loaded.diagnostics[0].module, "example");
    }

    #[test]
    fn a_lone_file_stands_as_its_own_root() {
        assert_eq!(check_name("contracts", "contracts", "contracts.lm"), Ok(()));
        assert!(check_name("contracts", "contracts", "grammars/contracts.lm").is_err());
    }
}
