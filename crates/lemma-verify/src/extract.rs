//! Facts for each repository, by its role, in the grammar's relations.
//!
//! Relations the grammar does not declare are skipped, so a grammar can use
//! a subset. A service's facts go to the base and head snapshots; other
//! repositories are read at one commit, so their facts are the same in both.

use std::cell::RefCell;
use std::collections::{BTreeMap, BTreeSet};
use std::path::{Path, PathBuf};
use std::rc::Rc;
use std::sync::Arc;

use lemma_grammar::CompiledGrammar;
use lemma_http::{
    CodeFacts, CodeOptions, ListedFile, Operation, PathAliases, SourceFile, is_under_any,
};
use lemma_logic::Value;
use lemma_syntax::ExtractorPool;

use crate::VerifyError;
use crate::clients::{ClientMethod, Clients, NamedOperation, starts_locally};
use crate::manifest::{GeneratedDocument, Manifest, Repository, Role};
use crate::system::System;

const CODE_EXTENSIONS: [&str; 11] = [
    "ts", "tsx", "mts", "cts", "js", "jsx", "mjs", "cjs", "py", "go", "rs",
];

/// A value for a fact, before interning.
#[derive(Clone, Copy)]
enum Arg<'a> {
    Text(&'a str),
    Number(i64),
}

/// Writes facts into the grammar's relations, interning text as it goes.
pub struct FactWriter<'g> {
    grammar: &'g mut CompiledGrammar,
    /// Facts written, by relation, for the summary.
    pub counts: BTreeMap<String, usize>,
}

impl<'g> FactWriter<'g> {
    pub fn new(grammar: &'g mut CompiledGrammar) -> Self {
        Self {
            grammar,
            counts: BTreeMap::new(),
        }
    }

    /// The grammar facts are written into.
    pub(crate) fn grammar(&self) -> &CompiledGrammar {
        self.grammar
    }

    /// Whether the grammar reads `relation` in `snapshot`, so facts for it
    /// there are worth reading.
    fn wants(&self, relation: &str, snapshot: &str) -> bool {
        self.grammar.input_relation(relation, snapshot).is_some()
    }

    fn add(&mut self, relation: &str, snapshots: &[&str], args: &[Arg<'_>]) {
        for snapshot in snapshots {
            let Some(name) = self.grammar.input_relation(relation, snapshot) else {
                continue;
            };
            let row: Vec<Value> = args
                .iter()
                .map(|arg| match arg {
                    Arg::Text(text) => self.grammar.symbols.intern(text),
                    Arg::Number(number) => Value::Number(*number),
                })
                .collect();
            if self.grammar.facts.insert(&name, row) {
                *self.counts.entry(relation.to_owned()).or_default() += 1;
            }
        }
    }
}

/// Extracts every repository's facts.
pub fn extract_all(
    manifest: &Manifest,
    writer: &mut FactWriter<'_>,
    cache: Option<&crate::FactsCache>,
    listings: &Arc<lemma_http::Listings>,
    focus: &Focus<'_>,
) -> Result<Option<BTreeSet<String>>, VerifyError> {
    let mut extractor = FactReader::new(cache.cloned());
    check_files(manifest)?;
    let specs = SpecCache {
        listings: Arc::clone(listings),
        ..SpecCache::default()
    };
    listings.prefetch(&snapshot_listings(manifest));
    let system = read_system(manifest, &specs)?;
    let clients = read_clients(manifest, &system, &specs, &mut extractor)?;
    // Services and clients first: in change-driven evaluation, their facts
    // say which operations findings can be about, and so which consumer
    // files to read.
    let (consumers, others): (Vec<_>, Vec<_>) = manifest
        .repositories
        .iter()
        .partition(|(_, repository)| repository.role == Role::Consumer);
    for (name, repository) in others {
        for snapshot in snapshots(manifest, repository, &system, &clients, &specs) {
            match repository.role {
                Role::Service => service(name, repository, &snapshot, writer, &mut extractor)?,
                _ => client(name, repository, &snapshot, writer)?,
            }
        }
    }
    let operations = match focus {
        Focus::Everything => None,
        Focus::Change(plan) => Some(crate::demand::operations(writer.grammar(), plan)?),
        Focus::Operations(operations, _) => Some((*operations).clone()),
    };
    let (reads_changed_files, import_depth) = match focus {
        Focus::Change(plan) => (plan.reads_changed_files, plan.import_depth),
        Focus::Operations(_, depth) => (false, *depth),
        Focus::Everything => (false, 0),
    };
    let selection = operations.as_ref().map(|operations| {
        let grammar = writer.grammar();
        Selection::of(
            operations,
            &clients,
            grammar,
            reads_changed_files,
            import_depth,
        )
    });
    for (name, repository) in consumers {
        for mut snapshot in snapshots(manifest, repository, &system, &clients, &specs) {
            snapshot.selection = selection.as_ref();
            consumer(name, repository, &snapshot, writer, &mut extractor)?;
        }
    }
    Ok(operations)
}

/// Which consumer code to read.
pub(crate) enum Focus<'a> {
    /// All of it.
    Everything,
    /// What the change's findings can rest on, by the grammar's plan.
    Change(&'a crate::demand::Plan),
    /// The code that may call these operations (`METHOD /path`), and the
    /// files it imports to the depth given.
    Operations(&'a BTreeSet<String>, usize),
}

/// What change-driven evaluation reads of consumer code: the files that may
/// call an operation findings can be about (found by the terms their calls
/// are indexed by), the files those import, and, when a requirement holds
/// for edited code, the files the change edited.
pub(crate) struct Selection {
    terms: BTreeSet<String>,
    /// For each service, the terms of its operations no route declares,
    /// read in its own code only: that code registering them through a
    /// helper (`registerExecute(router, '/v1/execute')`) is where tracing
    /// finds their handlers.
    own_terms: BTreeMap<String, BTreeSet<String>>,
    reads_changed_files: bool,
    /// How many levels of imports the grammar's rules follow from a
    /// calling file: the files read beyond those selected.
    import_depth: usize,
}

impl Selection {
    /// The selection for `operations` (`METHOD /path`), reading the files a
    /// change edits too when `reads_changed_files`: their paths, the
    /// operation-table chains naming them, and the names client methods
    /// and generated functions give them.
    fn of(
        operations: &BTreeSet<String>,
        clients: &Clients,
        grammar: &CompiledGrammar,
        reads_changed_files: bool,
        import_depth: usize,
    ) -> Self {
        let own_terms = undeclared_operations(grammar)
            .into_iter()
            .map(|(service, operations)| (service, operation_terms(&operations, clients, grammar)))
            .collect();
        Self {
            terms: operation_terms(operations, clients, grammar),
            own_terms,
            reads_changed_files,
            import_depth,
        }
    }
}

/// The terms a call of one of `operations` (`METHOD /path`) is indexed by:
/// their paths, the operation-table chains naming them, and the names
/// client methods and generated functions give them.
fn operation_terms(
    operations: &BTreeSet<String>,
    clients: &Clients,
    grammar: &CompiledGrammar,
) -> BTreeSet<String> {
    let mut terms = BTreeSet::new();
    for operation in operations {
        let Some((method, path)) = operation.split_once(' ') else {
            continue;
        };
        terms.insert(crate::terms::path_term(path));
        let names = clients.names_calling(method, path);
        terms.extend(names.iter().map(|name| crate::terms::name_term(name)));
    }
    let chains = table_chains(grammar, operations);
    terms.extend(chains.iter().map(|chain| crate::terms::chain_term(chain)));
    terms
}

/// What the system's services serve, from their specs at every commit they
/// are read at: the markers that make a string in code a path, and the path
/// prefixes each service owns.
fn read_system(manifest: &Manifest, specs: &SpecCache) -> Result<System, VerifyError> {
    let unknown = System::default();
    let no_clients = Clients::default();
    let mut paths: BTreeMap<String, crate::system::DocumentPaths> = BTreeMap::new();
    let mut declared = BTreeMap::new();
    let services = manifest
        .repositories
        .iter()
        .filter(|(_, repository)| repository.role == Role::Service);
    for (name, repository) in services {
        let service = repository.service.clone().unwrap_or_else(|| name.clone());
        let served = paths.entry(service.clone()).or_default();
        for snapshot in snapshots(manifest, repository, &unknown, &no_clients, specs) {
            for spec in &snapshot_documents(repository, &snapshot) {
                served.entry(spec.clone()).or_default().extend(
                    spec_operations(repository, &snapshot, spec)?
                        .into_iter()
                        .map(|operation| operation.path),
                );
            }
        }
        declared.insert(service, repository.path_prefixes.clone());
    }
    Ok(System::new(&paths, &declared))
}

/// What the system's clients offer, from the code of each client that has
/// some: the methods that issue requests, and the packages the client is
/// published as. A changed client is read at the change's head.
fn read_clients(
    manifest: &Manifest,
    system: &System,
    specs: &SpecCache,
    extractor: &mut FactReader,
) -> Result<Clients, VerifyError> {
    let options = CodeOptions {
        path_markers: system.markers.clone(),
        ..CodeOptions::default()
    };
    let mut methods = Vec::new();
    let mut packages = BTreeSet::new();
    let with_code = manifest.repositories.iter().filter(|(_, repository)| {
        repository.role == Role::Client && !repository.include.is_empty()
    });
    for (name, repository) in with_code {
        let commit = if is_changed(manifest, repository) {
            manifest.change.head.as_deref()
        } else {
            repository.commit.as_deref()
        };
        let files = specs.code_files(&repository.path, commit, &repository.include)?;
        let mut found = Vec::new();
        for (_, facts) in extractor.read(&repository.path, commit, &files, &options)? {
            found.extend(client_methods(name, &facts));
        }
        packages.extend(package_names(repository, commit, &found, specs)?);
        methods.extend(found);
    }
    Ok(Clients::new(
        methods,
        packages,
        generated_names(manifest, specs)?,
    ))
}

/// Adds a document's operations under each name a generated client may
/// give their functions.
fn add_generated_names(
    by_name: &mut BTreeMap<String, BTreeSet<NamedOperation>>,
    service: &str,
    document: &str,
    operations: &[Operation],
) {
    for operation in operations {
        let Some(operation_id) = operation.operation_id.as_deref() else {
            continue;
        };
        let named = NamedOperation {
            service: service.to_owned(),
            method: operation.method.clone(),
            path: operation.path.clone(),
            document: document.to_owned(),
        };
        for casing in GENERATED_CASINGS {
            by_name
                .entry(call_name(casing, operation_id))
                .or_default()
                .insert(named.clone());
        }
    }
}

/// The casings a client generator may give an operation's function.
const GENERATED_CASINGS: [&str; 4] = ["{operationId}", "{camel}", "{pascal}", "{snake}"];

/// Every service's operations by the names a generated client may give
/// their functions: each `operationId` as written and recased.
fn generated_names(
    manifest: &Manifest,
    specs: &SpecCache,
) -> Result<BTreeMap<String, BTreeSet<NamedOperation>>, VerifyError> {
    let unknown = System::default();
    let no_clients = Clients::default();
    let mut names = BTreeMap::new();
    let services = manifest
        .repositories
        .iter()
        .filter(|(_, repository)| repository.role == Role::Service);
    for (name, repository) in services {
        let service = repository.service.as_deref().unwrap_or(name);
        for snapshot in snapshots(manifest, repository, &unknown, &no_clients, specs) {
            for document in &snapshot_documents(repository, &snapshot) {
                let operations = spec_operations(repository, &snapshot, document)?;
                add_generated_names(&mut names, service, document, &operations);
            }
        }
    }
    Ok(names)
}

/// The requests a client's file issues from inside a function or method,
/// with their method known. A table describing operations (`{ method, path
/// }` objects, `Operation("get", "/v1/x")` rows of a constant) issues none.
fn client_methods(client: &str, facts: &CodeFacts) -> Vec<ClientMethod> {
    facts
        .path_references
        .iter()
        .filter(|reference| reference.method != "*")
        .filter_map(|reference| {
            Some(ClientMethod {
                client: client.to_owned(),
                symbol: reference.location.function.clone()?,
                method: reference.method.clone(),
                path: reference.path.clone(),
                file: reference.location.file.clone(),
                line: reference.location.line,
            })
        })
        .collect()
}

/// The package names a client is published as: the `name` of each
/// `package.json` and `pyproject.toml` whose directory holds some of its
/// methods, and a Python distribution's import name (`acme-client` is
/// imported as `acme_client`).
fn package_names(
    repository: &Repository,
    commit: Option<&str>,
    methods: &[ClientMethod],
    specs: &SpecCache,
) -> Result<BTreeSet<String>, VerifyError> {
    let is_package_file = |path: &str| {
        let name = path.rsplit('/').next().unwrap_or(path);
        name == "package.json" || name == "pyproject.toml"
    };
    let mut names = BTreeSet::new();
    for file in specs.read_files_where(&repository.path, commit, &is_package_file)? {
        let directory = file
            .path
            .rsplit_once('/')
            .map_or("", |(directory, _)| directory);
        let holds_methods = methods.iter().any(|method| {
            directory.is_empty() || method.file.starts_with(&format!("{directory}/"))
        });
        if !holds_methods {
            continue;
        }
        let Some(name) = declared_package_name(&file.path, &file.contents) else {
            continue;
        };
        if file.path.ends_with("pyproject.toml") {
            names.insert(name.replace('-', "_"));
        }
        names.insert(name);
    }
    Ok(names)
}

/// The `name` a `package.json` or `pyproject.toml` declares.
fn declared_package_name(path: &str, contents: &str) -> Option<String> {
    if path.ends_with("package.json") {
        let json: serde_json::Value = serde_json::from_str(contents).ok()?;
        return json.get("name")?.as_str().map(str::to_owned);
    }
    contents.lines().find_map(|line| {
        let value = line
            .trim()
            .strip_prefix("name")?
            .trim_start()
            .strip_prefix('=')?;
        let name = value.trim().trim_matches(['"', '\'']);
        (!name.is_empty()).then(|| name.to_owned())
    })
}

/// Whether a repository's dependency manifests (`package.json`,
/// `pyproject.toml`, `setup.py`, `setup.cfg`, `requirements*.txt`) name
/// one of the system's clients.
fn depends_on_a_client(
    repository: &Repository,
    commit: Option<&str>,
    snapshot: &Snapshot<'_>,
) -> Result<bool, VerifyError> {
    let clients = snapshot.clients;
    if clients.is_empty() {
        return Ok(false);
    }
    let is_dependency_file = |path: &str| {
        let name = path.rsplit('/').next().unwrap_or(path);
        matches!(
            name,
            "package.json" | "pyproject.toml" | "setup.py" | "setup.cfg"
        ) || (name.starts_with("requirements") && has_extension(name, "txt"))
    };
    let files = snapshot
        .specs
        .read_files_where(&repository.path, commit, &is_dependency_file)?;
    Ok(files
        .iter()
        .any(|file| clients.is_depended_on_in(&file.contents)))
}

/// Fails if a spec is missing at every commit its repository is
/// read at, which means the manifest names it wrongly. Missing at one of
/// the changed repository's two commits is fine: the change added or
/// removed it.
fn check_files(manifest: &Manifest) -> Result<(), VerifyError> {
    for (name, repository) in &manifest.repositories {
        let commits: Vec<Option<&str>> = if is_changed(manifest, repository) {
            vec![Some(&manifest.change.base), manifest.change.head.as_deref()]
        } else {
            vec![repository.commit.as_deref()]
        };
        for file in repository.specs.iter().chain(&repository.generated_from) {
            if !exists_at_any(&repository.path, &commits, file)? {
                return Err(VerifyError::MissingFile {
                    repository: name.clone(),
                    file: file.clone(),
                    commits: commits
                        .iter()
                        .map(|commit| commit.unwrap_or("the working directory"))
                        .collect::<Vec<_>>()
                        .join(" or "),
                });
            }
        }
    }
    Ok(())
}

fn exists_at_any(
    repository: &Path,
    commits: &[Option<&str>],
    file: &str,
) -> Result<bool, VerifyError> {
    for commit in commits {
        if lemma_http::read_file(repository, *commit, file)?.is_some() {
            return Ok(true);
        }
    }
    Ok(false)
}

/// The commits to read a repository at. The changed repository, and any
/// other entry for the same clone without a commit of its own (one
/// repository can hold a service and its consumers), are read before and
/// with the change; every other repository once, for both snapshots.
fn snapshots<'m>(
    manifest: &'m Manifest,
    repository: &'m Repository,
    system: &'m System,
    clients: &'m Clients,
    specs: &'m SpecCache,
) -> Vec<Snapshot<'m>> {
    if !is_changed(manifest, repository) {
        return vec![Snapshot {
            commit: repository.commit.as_deref(),
            names: &BOTH,
            other: None,
            system,
            clients,
            specs,
            selection: None,
        }];
    }
    vec![
        Snapshot {
            commit: Some(&manifest.change.base),
            names: &["base"],
            other: Some(manifest.change.head.as_deref()),
            system,
            clients,
            specs,
            selection: None,
        },
        Snapshot {
            commit: manifest.change.head.as_deref(),
            names: &["head"],
            other: Some(Some(&manifest.change.base)),
            system,
            clients,
            specs,
            selection: None,
        },
    ]
}

/// Every repository and commit the snapshots of `manifest` read, to list
/// them all at once.
fn snapshot_listings(manifest: &Manifest) -> Vec<(PathBuf, Option<String>)> {
    let mut wanted = Vec::new();
    for repository in manifest.repositories.values() {
        let commits = if is_changed(manifest, repository) {
            vec![
                Some(manifest.change.base.clone()),
                manifest.change.head.clone(),
            ]
        } else {
            vec![repository.commit.clone()]
        };
        wanted.extend(
            commits
                .into_iter()
                .map(|commit| (repository.path.clone(), commit)),
        );
    }
    wanted
}

/// Whether a repository entry is read at the change's commits: the changed
/// one, and other entries for its clone unless they pin their own commit
/// (to ask how the change meets code from another time).
fn is_changed(manifest: &Manifest, repository: &Repository) -> bool {
    let Some(changed) = manifest.repositories.get(&manifest.change.repository) else {
        return false;
    };
    if std::ptr::eq(changed, repository) {
        return true;
    }
    changed.path == repository.path && repository.commit.is_none()
}

/// A commit to read and the snapshots its facts belong to.
/// A commit, or `None` for the working directory.
type Commit<'a> = Option<&'a str>;

struct Snapshot<'a> {
    commit: Commit<'a>,
    names: &'a [&'a str],
    /// The change's other commit, when reading the changed repository. A
    /// spec or table that exists at only one of the two commits is read
    /// from that one for both: a file added or deleted by the change, such
    /// as a spec moved elsewhere, does not mean its operations appeared or
    /// disappeared.
    other: Option<Commit<'a>>,
    /// What the system's services serve.
    system: &'a System,
    /// What the system's clients offer.
    clients: &'a Clients,
    /// Specs already read in this run.
    specs: &'a SpecCache,
    /// The consumer files to read, in change-driven evaluation; `None`
    /// reads them all.
    selection: Option<&'a Selection>,
}

/// A spec as read at one snapshot: the repository, the snapshot's commit
/// and the commit it falls back to, and the file read.
type SpecKey = (PathBuf, Option<String>, Option<Option<String>>, String);

/// The operations of each spec read in one run, so a spec several passes
/// read is parsed once.
#[derive(Default)]
struct SpecCache {
    read: RefCell<BTreeMap<SpecKey, Vec<Operation>>>,
    /// Each repository's files at a commit or in its working directory.
    listings: Arc<lemma_http::Listings>,
    /// Files read at a commit, by repository, commit and path: the entries
    /// and snapshots of one repository read many of the same files.
    blobs: RefCell<BTreeMap<(PathBuf, String, String), Rc<SourceFile>>>,
    /// Files read from a working directory, by repository and path.
    on_disk: RefCell<BTreeMap<(PathBuf, String), Rc<SourceFile>>>,
}

impl SpecCache {
    /// Every file of the repository at `repository` in `commit`, or in its
    /// working directory, with blobs, listed once per run.
    fn listing(
        &self,
        repository: &Path,
        commit: Option<&str>,
    ) -> Result<Arc<Vec<ListedFile>>, VerifyError> {
        Ok(self.listings.get(repository, commit)?)
    }

    /// The files `wanted` accepts at `commit`, each read from Git once per
    /// run. Sorted by path.
    fn commit_files(
        &self,
        repository: &Path,
        commit: &str,
        wanted: &dyn Fn(&str) -> bool,
    ) -> Result<Vec<SourceFile>, VerifyError> {
        let listing = self.listing(repository, Some(commit))?;
        let paths: Vec<&String> = listing
            .iter()
            .map(|file| &file.path)
            .filter(|path| wanted(path))
            .collect();
        let key = |path: &str| (repository.to_owned(), commit.to_owned(), path.to_owned());
        let missing: Vec<String> = {
            let blobs = self.blobs.borrow();
            paths
                .iter()
                .filter(|path| !blobs.contains_key(&key(path)))
                .map(|path| (*path).clone())
                .collect()
        };
        if !missing.is_empty() {
            let read = lemma_http::read_paths(repository, commit, &missing)?;
            let mut blobs = self.blobs.borrow_mut();
            for file in read {
                blobs.insert(key(&file.path), Rc::new(file));
            }
        }
        let blobs = self.blobs.borrow();
        Ok(paths
            .iter()
            .filter_map(|path| blobs.get(&key(path)))
            .map(|file| (**file).clone())
            .collect())
    }

    /// The working directory's files `wanted` accepts, the directory listed
    /// and each file read once per run. Sorted by path.
    fn working_files(
        &self,
        repository: &Path,
        wanted: &dyn Fn(&str) -> bool,
    ) -> Result<Vec<SourceFile>, VerifyError> {
        let listing = self.listing(repository, None)?;
        let paths: Vec<&String> = listing
            .iter()
            .map(|file| &file.path)
            .filter(|path| wanted(path))
            .collect();
        let key = |path: &str| (repository.to_owned(), path.to_owned());
        let missing: Vec<String> = {
            let on_disk = self.on_disk.borrow();
            paths
                .iter()
                .filter(|path| !on_disk.contains_key(&key(path)))
                .map(|path| (*path).clone())
                .collect()
        };
        if !missing.is_empty() {
            let read = lemma_http::read_from_disk(repository, &missing)?;
            let mut on_disk = self.on_disk.borrow_mut();
            for file in read {
                on_disk.insert(key(&file.path), Rc::new(file));
            }
        }
        let on_disk = self.on_disk.borrow();
        Ok(paths
            .iter()
            .filter_map(|path| on_disk.get(&key(path)))
            .map(|file| (**file).clone())
            .collect())
    }

    /// The code files under `prefixes`, listed with their blobs but not
    /// read: their facts may already be known. Sorted by path.
    fn code_files(
        &self,
        repository: &Path,
        commit: Option<&str>,
        prefixes: &[String],
    ) -> Result<Vec<ListedFile>, VerifyError> {
        if commit.is_none() {
            lemma_http::require_prefixes(repository, prefixes)?;
        }
        let is_code = |path: &str| {
            let extension = path.rsplit_once('.').map_or("", |(_, extension)| extension);
            CODE_EXTENSIONS.contains(&extension) && is_under_any(path, prefixes)
        };
        let listing = self.listing(repository, commit)?;
        Ok(listing
            .iter()
            .filter(|file| is_code(&file.path))
            .cloned()
            .collect())
    }

    /// [`lemma_http::read_files_where`], reading a commit's files once per
    /// run.
    fn read_files_where(
        &self,
        repository: &Path,
        commit: Option<&str>,
        wanted: &dyn Fn(&str) -> bool,
    ) -> Result<Vec<SourceFile>, VerifyError> {
        let wanted = |path: &str| wanted(path) && !path.contains("node_modules/");
        match commit {
            Some(commit) => self.commit_files(repository, commit, &wanted),
            None => self.working_files(repository, &wanted),
        }
    }

    /// [`lemma_http::list_files`], listing a commit once per run.
    fn list_files(
        &self,
        repository: &Path,
        commit: Option<&str>,
    ) -> Result<Vec<String>, VerifyError> {
        let listing = self.listing(repository, commit)?;
        Ok(listing.iter().map(|file| file.path.clone()).collect())
    }
}

/// The code a service's routes and the schemas they use are in.
fn route_and_schema_files(
    repository: &Repository,
    snapshot: &Snapshot<'_>,
) -> Result<Vec<ListedFile>, VerifyError> {
    let directories: Vec<String> = repository
        .routes
        .iter()
        .chain(&repository.schemas)
        .cloned()
        .collect();
    snapshot
        .specs
        .code_files(&repository.path, snapshot.commit, &directories)
}

fn service(
    name: &str,
    repository: &Repository,
    snapshot: &Snapshot<'_>,
    writer: &mut FactWriter<'_>,
    extractor: &mut FactReader,
) -> Result<(), VerifyError> {
    let (commit, snapshots) = (snapshot.commit, snapshot.names);
    let service_name = repository.service.as_deref().unwrap_or(name);
    if repository.rejects_unknown_fields {
        writer.add(
            "rejects_unknown_fields",
            snapshots,
            &[Arg::Text(service_name)],
        );
    }
    let documents = write_operations(service_name, repository, snapshot, writer)?;
    // Without route directories there is no route code to read; an empty
    // list would otherwise mean the whole repository. Routes are read at
    // the head, and at the base too when the grammar reads them there (a
    // requirement over routes telling what was there before the change).
    let code_snapshots: Vec<&str> = snapshots
        .iter()
        .copied()
        .filter(|snapshot| {
            *snapshot == "head"
                || SERVICE_CODE_RELATIONS
                    .iter()
                    .any(|relation| writer.wants(relation, snapshot))
        })
        .collect();
    if repository.routes.is_empty() || code_snapshots.is_empty() {
        return Ok(());
    }
    let files = route_and_schema_files(repository, snapshot)?;
    let read = extractor.read(&repository.path, commit, &files, &CodeOptions::default())?;
    let is_route_file = |path: &str| {
        repository
            .routes
            .iter()
            .any(|directory| path.starts_with(directory.as_str()))
    };
    let written = read
        .iter()
        .filter(|(path, _)| is_route_file(path))
        .flat_map(|(_, facts)| written_routes(facts))
        .collect();
    let mut route_aliases = lemma_http::implied_aliases(&documents, &written);
    route_aliases.extend(repository.route_aliases.clone());
    let scope = ServiceScope {
        name,
        service: service_name,
        snapshots: &code_snapshots,
        known: files.iter().map(|file| file.path.clone()).collect(),
        route_aliases: &route_aliases,
        imports: read_import_aliases(repository, commit, snapshot.specs)?,
    };
    for (path, facts) in &read {
        schema_facts(&scope, path, facts, writer);
        if !is_route_file(path) {
            continue;
        }
        for route in facts.method_paths.iter().filter(|item| item.method != "*") {
            route_facts(&scope, path, route, writer);
        }
    }
    Ok(())
}

/// Writes the operations of each of a service's specs, and returns them by
/// spec as method and canonical path.
fn write_operations(
    service_name: &str,
    repository: &Repository,
    snapshot: &Snapshot<'_>,
    writer: &mut FactWriter<'_>,
) -> Result<Vec<Vec<(String, String)>>, VerifyError> {
    let mut documents = Vec::new();
    for spec in &snapshot_documents(repository, snapshot) {
        let operations = spec_operations(repository, snapshot, spec)?;
        for operation in &operations {
            operation_facts(service_name, operation, snapshot.names, writer);
        }
        documents.push(
            operations
                .into_iter()
                .map(|operation| (operation.method, operation.path))
                .collect(),
        );
    }
    Ok(documents)
}

/// The routes a file's code writes, as method and canonical path.
fn written_routes(facts: &CodeFacts) -> impl Iterator<Item = (String, String)> + '_ {
    facts
        .method_paths
        .iter()
        .filter(|item| item.method != "*")
        .map(|item| (item.method.clone(), item.path.clone()))
}

/// What every route file of one service shares.
/// The relations a service's route and schema code is written to.
const SERVICE_CODE_RELATIONS: [&str; 6] = [
    "route",
    "route_schema",
    "inline_strict_route",
    "strict_schema",
    "union_member",
    "imports",
];

struct ServiceScope<'a> {
    name: &'a str,
    service: &'a str,
    /// The snapshots the service's code facts are written to.
    snapshots: &'a [&'a str],
    known: BTreeSet<String>,
    route_aliases: &'a BTreeMap<String, String>,
    imports: PathAliases,
}

impl ServiceScope<'_> {
    /// A route's path and the paths its prefixes are aliased to.
    fn route_paths(&self, path: &str) -> Vec<String> {
        let mut paths = vec![path.to_owned()];
        for (from, to) in self.route_aliases {
            if let Some(rest) = path.strip_prefix(from.as_str()) {
                paths.push(format!("{to}{rest}"));
            }
        }
        paths
    }
}

/// Which schemas a route file's routes use, which schemas it defines strict
/// or as unions, and the files it imports, where its schemas may be defined.
fn schema_facts(
    scope: &ServiceScope<'_>,
    path: &str,
    facts: &CodeFacts,
    writer: &mut FactWriter<'_>,
) {
    let file_id = format!("{}:{path}", scope.name);
    for used in &facts.schema_uses {
        for path in scope.route_paths(&used.path) {
            schema_use_facts(scope, &file_id, used, &path, writer);
        }
    }
    for schema in &facts.strict_schemas {
        writer.add(
            "strict_schema",
            scope.snapshots,
            &[Arg::Text(&file_id), Arg::Text(schema)],
        );
    }
    for union in &facts.schema_unions {
        for member in &union.members {
            let args = [
                Arg::Text(&file_id),
                Arg::Text(&union.name),
                Arg::Text(member),
            ];
            writer.add("union_member", scope.snapshots, &args);
        }
    }
    for specifier in &facts.imports {
        if let Some(target) = resolve_import(path, specifier, &scope.known, &scope.imports) {
            let target_id = format!("{}:{target}", scope.name);
            writer.add(
                "imports",
                scope.snapshots,
                &[Arg::Text(&file_id), Arg::Text(&target_id)],
            );
        }
    }
}

/// The schema one route part uses, at one of the route's paths.
fn schema_use_facts(
    scope: &ServiceScope<'_>,
    file_id: &str,
    used: &lemma_http::SchemaUse,
    path: &str,
    writer: &mut FactWriter<'_>,
) {
    let route = [
        Arg::Text(scope.service),
        Arg::Text(&used.method),
        Arg::Text(path),
        Arg::Text(&used.part),
    ];
    if let Some(schema) = &used.schema {
        let mut args = route.to_vec();
        args.extend([Arg::Text(file_id), Arg::Text(schema)]);
        writer.add("route_schema", scope.snapshots, &args);
    } else if used.is_inline_strict {
        writer.add("inline_strict_route", scope.snapshots, &route);
    }
}

/// A route a service serves, at its path and any aliased path.
fn route_facts(
    scope: &ServiceScope<'_>,
    file: &str,
    route: &lemma_http::MethodPath,
    writer: &mut FactWriter<'_>,
) {
    let line = i64::from(route.location.line);
    for path in scope.route_paths(&route.path) {
        let args = [
            Arg::Text(scope.service),
            Arg::Text(&route.method),
            Arg::Text(&path),
            Arg::Text(file),
            Arg::Number(line),
        ];
        writer.add("route", scope.snapshots, &args);
    }
}

fn operation_facts(
    service: &str,
    operation: &Operation,
    snapshots: &[&str],
    writer: &mut FactWriter<'_>,
) {
    let key = operation_key(&operation.method, &operation.path);
    let op = key.as_str();
    writer.add(
        "operation",
        snapshots,
        &[
            Arg::Text(service),
            Arg::Text(op),
            Arg::Text(&operation.method),
            Arg::Text(&operation.path),
        ],
    );
    if operation.rejects_unknown_fields {
        writer.add("strict_request", snapshots, &[Arg::Text(op)]);
    }
    for parameter in &operation.parameters {
        let args = [
            Arg::Text(op),
            Arg::Text(&parameter.name),
            Arg::Text(&parameter.location),
        ];
        writer.add("parameter", snapshots, &args);
        if parameter.required {
            writer.add("required_parameter", snapshots, &args);
        }
    }
    for field in &operation.request_fields {
        let args = [
            Arg::Text(op),
            Arg::Text(&field.path),
            Arg::Text(leaf(&field.path)),
        ];
        writer.add("request_field", snapshots, &args);
        if field.required {
            writer.add("required_request_field", snapshots, &args);
        }
    }
    for field in &operation.response_fields {
        writer.add(
            "response_field",
            snapshots,
            &[Arg::Text(op), Arg::Text(field), Arg::Text(leaf(field))],
        );
    }
}

fn client(
    name: &str,
    repository: &Repository,
    snapshot: &Snapshot<'_>,
    writer: &mut FactWriter<'_>,
) -> Result<(), VerifyError> {
    let both = snapshot.names;
    for method in snapshot.clients.methods_of(name) {
        let key = method.key();
        let args = [
            Arg::Text(name),
            Arg::Text(&key),
            Arg::Text(&method.method),
            Arg::Text(&method.path),
            Arg::Text(&method.file),
            Arg::Number(i64::from(method.line)),
        ];
        writer.add("client_operation", both, &args);
    }
    let default_names = [DEFAULT_CALL_NAME.to_owned()];
    let templates = if repository.call_names.is_empty() {
        &default_names[..]
    } else {
        &repository.call_names[..]
    };
    for spec in &repository.generated_from {
        for operation in spec_operations(repository, snapshot, spec)? {
            let Some(operation_id) = operation.operation_id.as_deref() else {
                continue;
            };
            for template in templates {
                let function = call_name(template, operation_id);
                let args = [
                    Arg::Text(name),
                    Arg::Text(&function),
                    Arg::Text(&operation.method),
                    Arg::Text(&operation.path),
                    Arg::Text(spec),
                    Arg::Number(0),
                ];
                writer.add("client_operation", both, &args);
            }
        }
    }
    for spec in &repository.specs {
        for operation in spec_operations(repository, snapshot, spec)? {
            for parameter in &operation.parameters {
                let args = [
                    Arg::Text(name),
                    Arg::Text(&operation.method),
                    Arg::Text(&operation.path),
                    Arg::Text(&parameter.name),
                    Arg::Text(&parameter.location),
                ];
                writer.add("client_parameter", both, &args);
            }
        }
    }
    Ok(())
}

/// What every file of one consumer shares.
struct ConsumerScope<'a> {
    name: &'a str,
    /// The service this consumer's code belongs to, whose own operations
    /// it does not consume.
    service: Option<&'a str>,
    /// The snapshots this reading of the repository belongs to.
    snapshots: &'a [&'a str],
    known: BTreeSet<String>,
    options: CodeOptions,
    /// What the system's services serve.
    system: &'a System,
    /// What the system's clients offer.
    clients: &'a Clients,
    /// Whether the repository depends on one of the system's clients.
    depends_on_a_client: bool,
    /// Every file in the repository, to tell a module generated at build
    /// time (imported, but in no file) from one that exists.
    every_file: BTreeSet<String>,
    /// Files re-exporting a generated module whole, with the names they
    /// export it as and the module.
    reexports: BTreeMap<String, Vec<(String, String)>>,
    /// Path aliases from the repository's `tsconfig` files.
    imports: PathAliases,
}

const BOTH: [&str; 2] = ["base", "head"];

fn consumer(
    name: &str,
    repository: &Repository,
    snapshot: &Snapshot<'_>,
    writer: &mut FactWriter<'_>,
    extractor: &mut FactReader,
) -> Result<(), VerifyError> {
    let files =
        snapshot
            .specs
            .code_files(&repository.path, snapshot.commit, &repository.include)?;
    let scope = ConsumerScope {
        name,
        service: repository.service.as_deref(),
        snapshots: snapshot.names,
        known: files.iter().map(|file| file.path.clone()).collect(),
        options: CodeOptions {
            client_receivers: repository.client_receivers.clone(),
            client_modules: repository.client_modules.clone(),
            path_markers: snapshot.system.markers.clone(),
        },
        system: snapshot.system,
        clients: snapshot.clients,
        depends_on_a_client: depends_on_a_client(repository, snapshot.commit, snapshot)?,
        every_file: snapshot
            .specs
            .list_files(&repository.path, snapshot.commit)?
            .into_iter()
            .collect(),
        reexports: BTreeMap::new(),
        imports: read_import_aliases(repository, snapshot.commit, snapshot.specs)?,
    };
    let to_read = if snapshot.selection.is_some() {
        selected_files(&files, repository, snapshot, &scope, extractor)?
    } else {
        files
    };
    let read = extractor.read(&repository.path, snapshot.commit, &to_read, &scope.options)?;
    let scope = ConsumerScope {
        reexports: generated_reexports(&scope, &read),
        ..scope
    };
    for (path, facts) in &read {
        consumer_file(&scope, path, facts, writer);
    }
    Ok(())
}

/// Each service's operations (`METHOD /path`) that no route declares,
/// from the specs' and routes' facts written so far.
fn undeclared_operations(grammar: &CompiledGrammar) -> BTreeMap<String, BTreeSet<String>> {
    let display = |value: &Value| grammar.symbols.display(*value);
    let declared: BTreeSet<(String, String)> = grammar
        .facts
        .rows("route")
        .filter_map(|row| {
            // `service, method, path, file, line`.
            let [service, method, path, ..] = &row[..] else {
                return None;
            };
            Some((
                display(service),
                operation_key(&display(method), &display(path)),
            ))
        })
        .collect();
    let mut undeclared: BTreeMap<String, BTreeSet<String>> = BTreeMap::new();
    for row in grammar.facts.rows("operation") {
        // `service, operation, method, path`.
        let [service, operation, ..] = &row[..] else {
            continue;
        };
        let key = (display(service), display(operation));
        if !declared.contains(&key) {
            let (service, operation) = key;
            undeclared.entry(service).or_default().insert(operation);
        }
    }
    undeclared
}

/// The operation-table chains that request one of `operations`, from the
/// clients' facts written so far.
fn table_chains(grammar: &CompiledGrammar, operations: &BTreeSet<String>) -> Vec<String> {
    let display = |value: &Value| grammar.symbols.display(*value);
    ["client_operation", "client_operation@base"]
        .iter()
        .flat_map(|relation| grammar.facts.rows(relation))
        .filter_map(|row| {
            // `client, chain, method, path, file, line`.
            let [_, chain, method, path, ..] = &row[..] else {
                return None;
            };
            let operation = operation_key(&display(method), &display(path));
            operations.contains(&operation).then(|| display(chain))
        })
        .collect()
}

/// The consumer files change-driven evaluation reads at `snapshot`: those
/// that may call an operation findings can be about, those the change
/// edited when a requirement holds for edited code, and the files these
/// import, whose declarations their code may use.
fn selected_files(
    files: &[ListedFile],
    repository: &Repository,
    snapshot: &Snapshot<'_>,
    scope: &ConsumerScope<'_>,
    extractor: &mut FactReader,
) -> Result<Vec<ListedFile>, VerifyError> {
    let Some(selection) = snapshot.selection else {
        return Ok(files.to_vec());
    };
    let mut terms = selection.terms.clone();
    if let Some(own) = scope
        .service
        .and_then(|service| selection.own_terms.get(service))
    {
        terms.extend(own.iter().cloned());
    }
    let edited = if selection.reads_changed_files {
        edited_paths(repository, snapshot)?
    } else {
        BTreeSet::new()
    };
    let here = Candidates {
        repository,
        commit: snapshot.commit,
        files,
        known: &scope.known,
        imports: &scope.imports,
        options: &scope.options,
    };
    let mut chosen = chosen_paths(&here, &terms, &edited, selection.import_depth, extractor)?;
    // In the changed repository both commits read the same files: a file
    // read at one only (imported by an edited file, say) would have facts
    // there and none at the other, which reads as a change.
    if let Some(other) = snapshot.other {
        let there_files =
            snapshot
                .specs
                .code_files(&repository.path, other, &repository.include)?;
        let there_known: BTreeSet<String> =
            there_files.iter().map(|file| file.path.clone()).collect();
        let there_imports = read_import_aliases(repository, other, snapshot.specs)?;
        let there = Candidates {
            repository,
            commit: other,
            files: &there_files,
            known: &there_known,
            imports: &there_imports,
            options: &scope.options,
        };
        chosen.extend(chosen_paths(
            &there,
            &terms,
            &edited,
            selection.import_depth,
            extractor,
        )?);
    }
    Ok(files
        .iter()
        .filter(|file| chosen.contains(&file.path))
        .cloned()
        .collect())
}

/// One commit's consumer files to choose among, and how their imports
/// resolve there.
struct Candidates<'a> {
    repository: &'a Repository,
    commit: Commit<'a>,
    files: &'a [ListedFile],
    known: &'a BTreeSet<String>,
    imports: &'a PathAliases,
    options: &'a CodeOptions,
}

/// The paths among `candidates` that may make a call `terms` name, or that
/// those import, `depth` levels down, and those `edited`. Edited files'
/// imports are not followed: requirements holding for edited code read
/// only facts of the edited file itself.
fn chosen_paths(
    candidates: &Candidates<'_>,
    terms: &BTreeSet<String>,
    edited: &BTreeSet<String>,
    depth: usize,
    extractor: &mut FactReader,
) -> Result<BTreeSet<String>, VerifyError> {
    let place = (candidates.repository.path.as_path(), candidates.commit);
    let mut chosen = extractor.select(place, candidates.files, candidates.options, terms)?;
    // The files they import, as far down as the grammar's rules follow
    // imports, each level read for the next.
    let mut frontier = chosen.clone();
    for _ in 0..depth {
        let level: Vec<ListedFile> = candidates
            .files
            .iter()
            .filter(|file| frontier.contains(&file.path))
            .cloned()
            .collect();
        let read = extractor.read(place.0, place.1, &level, candidates.options)?;
        frontier = read
            .iter()
            .flat_map(|(path, facts)| {
                imported_files(path, facts, candidates.known, candidates.imports)
            })
            .filter(|path| !chosen.contains(path))
            .collect();
        chosen.extend(frontier.iter().cloned());
    }
    chosen.extend(
        candidates
            .files
            .iter()
            .filter(|file| edited.contains(&file.path))
            .map(|file| file.path.clone()),
    );
    Ok(chosen)
}

/// The files the change edited in the changed repository: those whose
/// contents differ between its two commits, added and deleted ones among
/// them, or that Git cannot vouch for. The same at either commit, so both
/// read the same files. None in a repository the change leaves alone.
fn edited_paths(
    repository: &Repository,
    snapshot: &Snapshot<'_>,
) -> Result<BTreeSet<String>, VerifyError> {
    let Some(other) = snapshot.other else {
        return Ok(BTreeSet::new());
    };
    let here = snapshot.specs.listing(&repository.path, snapshot.commit)?;
    let there = snapshot.specs.listing(&repository.path, other)?;
    let mut edited = differing_paths(&here, &there);
    edited.extend(differing_paths(&there, &here));
    Ok(edited)
}

/// The paths in `listing` whose contents `other` lacks or holds otherwise,
/// or that Git cannot vouch for.
fn differing_paths(listing: &[ListedFile], other: &[ListedFile]) -> BTreeSet<String> {
    let blobs: BTreeMap<&str, Option<&String>> = other
        .iter()
        .map(|file| (file.path.as_str(), file.blob.as_ref()))
        .collect();
    listing
        .iter()
        .filter(|file| {
            file.blob.is_none()
                || blobs.get(file.path.as_str()).copied().flatten() != file.blob.as_ref()
        })
        .map(|file| file.path.clone())
        .collect()
}

fn consumer_file(
    scope: &ConsumerScope<'_>,
    path: &str,
    facts: &CodeFacts,
    writer: &mut FactWriter<'_>,
) {
    let file_id = format!("{}:{path}", scope.name);
    let mut scopes = BTreeSet::from([file_id.clone()]);
    write_calls(scope, &file_id, facts, writer, &mut scopes);
    for mention in &facts.mentions {
        let mention_scope = consumer_id(&file_id, mention.function.as_ref());
        writer.add(
            "mentions",
            scope.snapshots,
            &[Arg::Text(&mention_scope), Arg::Text(&mention.name)],
        );
        scopes.insert(mention_scope);
    }
    for function_scope in &scopes {
        writer.add(
            "in_file",
            scope.snapshots,
            &[Arg::Text(function_scope), Arg::Text(&file_id)],
        );
        let simple_name = function_scope
            .strip_prefix(&file_id)
            .and_then(|symbol| symbol.strip_prefix("::"))
            .and_then(|symbol| symbol.rsplit('.').next());
        if let Some(simple_name) = simple_name {
            writer.add(
                "named",
                scope.snapshots,
                &[Arg::Text(function_scope), Arg::Text(simple_name)],
            );
        }
    }
    write_imports(scope, path, &file_id, facts, writer);
    if let Some(service) = scope.service {
        writer.add(
            "part_of_service",
            scope.snapshots,
            &[Arg::Text(&file_id), Arg::Text(service)],
        );
    }
}

/// A consumer's id: the function a fact is in, or the file outside functions.
fn consumer_id(file_id: &str, symbol: Option<&String>) -> String {
    symbol.map_or_else(
        || file_id.to_owned(),
        |symbol| format!("{file_id}::{symbol}"),
    )
}

/// Calls by path and through clients, adding each caller to `scopes`.
fn write_calls(
    scope: &ConsumerScope<'_>,
    file_id: &str,
    facts: &CodeFacts,
    writer: &mut FactWriter<'_>,
    scopes: &mut BTreeSet<String>,
) {
    let calls = facts
        .path_references
        .iter()
        .map(|reference| (&reference.method, &reference.path, &reference.location));
    let descriptors = facts
        .method_paths
        .iter()
        .map(|item| (&item.method, &item.path, &item.location));
    for (method, call_path, location) in calls.chain(descriptors) {
        let consumer = consumer_id(file_id, location.symbol.as_ref());
        let args = [
            Arg::Text(&consumer),
            Arg::Text(method),
            Arg::Text(call_path),
            Arg::Text(file_id),
            Arg::Number(i64::from(location.line)),
        ];
        writer.add("calls_path", scope.snapshots, &args);
        for service in scope.system.addressed_to(call_path) {
            writer.add(
                "addressed_to",
                scope.snapshots,
                &[Arg::Text(call_path), Arg::Text(service)],
            );
        }
        scopes.insert(consumer);
    }
    for call in &facts.client_calls {
        let consumer = consumer_id(file_id, call.location.symbol.as_ref());
        let args = [
            Arg::Text(&consumer),
            Arg::Text(&call.chain),
            Arg::Text(file_id),
            Arg::Number(i64::from(call.location.line)),
        ];
        writer.add("calls_client", scope.snapshots, &args);
        scopes.insert(consumer);
    }
    write_client_method_calls(scope, file_id, facts, writer, scopes);
    write_generated_calls(scope, file_id, facts, writer, scopes);
}

/// Files re-exporting a generated module whole (`export * as api from
/// '../generated/api'`), with each name and module.
fn generated_reexports(
    scope: &ConsumerScope<'_>,
    read: &[(String, Rc<CodeFacts>)],
) -> BTreeMap<String, Vec<(String, String)>> {
    let mut found = BTreeMap::new();
    for (path, facts) in read {
        let generated: Vec<(String, String)> = facts
            .reexports
            .iter()
            .filter(|reexport| is_generated_module(scope, path, &reexport.module))
            .map(|reexport| (reexport.name.clone(), reexport.module.clone()))
            .collect();
        if !generated.is_empty() {
            found.insert(path.clone(), generated);
        }
    }
    found
}

/// Whether an import names a module inside the repository that no file
/// holds: a client generated at build time. Packages and existing modules
/// are not.
fn is_generated_module(scope: &ConsumerScope<'_>, file: &str, specifier: &str) -> bool {
    if has_extension(file, "rs") {
        return false;
    }
    let bases = if specifier.starts_with('.') {
        let directory = Path::new(file).parent().unwrap_or(Path::new(""));
        let base = if has_extension(file, "py") {
            python_module(directory, specifier)
        } else {
            join(directory, specifier.trim_end_matches(".js"))
        };
        base.into_iter().collect()
    } else {
        scope.imports.candidates(file, specifier)
    };
    !bases.is_empty()
        && bases
            .iter()
            .all(|base| module_file(base, &scope.every_file).is_none())
}

/// Calls of functions a generated client exports, through a name imported
/// from it or from a module re-exporting it, as calls of the operations
/// they are named after.
fn write_generated_calls(
    scope: &ConsumerScope<'_>,
    file_id: &str,
    facts: &CodeFacts,
    writer: &mut FactWriter<'_>,
    scopes: &mut BTreeSet<String>,
) {
    // The consumer's name may hold a colon itself (`#platform:api (code)`).
    let path = file_id
        .strip_prefix(scope.name)
        .and_then(|rest| rest.strip_prefix(':'))
        .unwrap_or(file_id);
    for call in &facts.member_calls {
        let Some((function, module)) = generated_function(scope, path, facts, &call.parts) else {
            continue;
        };
        let Some(operation) = scope.clients.named_operation(function, module) else {
            continue;
        };
        let client = format!("{} (generated)", operation.service);
        let key = format!("{}:{function}", operation.service);
        let declared = [
            Arg::Text(&client),
            Arg::Text(&key),
            Arg::Text(&operation.method),
            Arg::Text(&operation.path),
            Arg::Text(&operation.document),
            Arg::Number(0),
        ];
        writer.add("client_operation", scope.snapshots, &declared);
        let consumer = consumer_id(file_id, call.location.symbol.as_ref());
        let called = [
            Arg::Text(&consumer),
            Arg::Text(&key),
            Arg::Text(file_id),
            Arg::Number(i64::from(call.location.line)),
        ];
        writer.add("calls_client", scope.snapshots, &called);
        scopes.insert(consumer);
    }
}

/// The generated function a call names, and the generated module it is
/// from: `listTools` in `listTools()` imported from it, or in
/// `api.listTools()` where `api` is the module or a re-export of it.
/// Change-driven evaluation finds such calls by the call's last name and
/// the names a file imports (`terms.rs`), for calls of at most two names
/// rooted at an import; a call resolved here must stay within that.
fn generated_function<'f>(
    scope: &'f ConsumerScope<'_>,
    path: &str,
    facts: &'f CodeFacts,
    parts: &'f [String],
) -> Option<(&'f str, &'f str)> {
    let root = parts.first()?;
    let binding = facts
        .import_bindings
        .iter()
        .find(|binding| &binding.local == root)?;
    let is_generated = is_generated_module(scope, path, &binding.module);
    match (&binding.imported, parts) {
        (None, [_, function]) if is_generated => Some((function, &binding.module)),
        (Some(function), [_]) if is_generated => Some((function, &binding.module)),
        (Some(name), [_, function]) => {
            let target = resolve_import(path, &binding.module, &scope.known, &scope.imports)?;
            let (_, module) = scope
                .reexports
                .get(&target)?
                .iter()
                .find(|(exported, _)| exported == name)?;
            Some((function, module))
        }
        _ => None,
    }
}

/// Calls through chains that reach a client's method (see `clients`), as
/// calls of that method.
fn write_client_method_calls(
    scope: &ConsumerScope<'_>,
    file_id: &str,
    facts: &CodeFacts,
    writer: &mut FactWriter<'_>,
    scopes: &mut BTreeSet<String>,
) {
    if scope.clients.is_empty() {
        return;
    }
    let imports_a_client = facts
        .imported_modules
        .iter()
        .any(|module| scope.clients.is_client_module(module));
    for call in &facts.member_calls {
        let uses_a_client = imports_a_client
            || (scope.depends_on_a_client && starts_locally(call, &facts.imported_names));
        if !uses_a_client {
            continue;
        }
        let Some(method) = scope.clients.resolve(call) else {
            continue;
        };
        let consumer = consumer_id(file_id, call.location.symbol.as_ref());
        let key = method.key();
        let args = [
            Arg::Text(&consumer),
            Arg::Text(&key),
            Arg::Text(file_id),
            Arg::Number(i64::from(call.location.line)),
        ];
        writer.add("calls_client", scope.snapshots, &args);
        scopes.insert(consumer);
    }
}

/// Path aliases from every `tsconfig*.json` in a repository.
fn read_import_aliases(
    repository: &Repository,
    commit: Option<&str>,
    specs: &SpecCache,
) -> Result<PathAliases, VerifyError> {
    let is_tsconfig = |path: &str| {
        let name = path.rsplit('/').next().unwrap_or(path);
        name.starts_with("tsconfig") && has_extension(name, "json")
    };
    let files = specs.read_files_where(&repository.path, commit, &is_tsconfig)?;
    Ok(PathAliases::from_files(&files))
}

/// The files a file imports, and for Go the other files of its package.
fn write_imports(
    scope: &ConsumerScope<'_>,
    path: &str,
    file_id: &str,
    facts: &CodeFacts,
    writer: &mut FactWriter<'_>,
) {
    for target in imported_files(path, facts, &scope.known, &scope.imports) {
        let target_id = format!("{}:{target}", scope.name);
        writer.add(
            "imports",
            scope.snapshots,
            &[Arg::Text(file_id), Arg::Text(&target_id)],
        );
    }
}

/// The documents a service is read from at a snapshot, by name: those
/// built from its code for that snapshot, if Lemma ran an adapter,
/// otherwise its committed specs.
fn snapshot_documents(repository: &Repository, snapshot: &Snapshot<'_>) -> Vec<String> {
    generated_at(repository, snapshot).map_or_else(
        || repository.specs.clone(),
        |documents| {
            documents
                .iter()
                .map(|document| document.name.clone())
                .collect()
        },
    )
}

/// The documents built from a service's code for a snapshot of this
/// repository.
fn generated_at<'r>(
    repository: &'r Repository,
    snapshot: &Snapshot<'_>,
) -> Option<&'r Vec<GeneratedDocument>> {
    let role = match snapshot.names {
        ["base"] => "base",
        ["head"] => "head",
        _ => "both",
    };
    repository
        .generated
        .get(role)
        .or_else(|| repository.generated.get("both"))
}

/// A spec's operations at a snapshot, parsed once per run.
fn spec_operations(
    repository: &Repository,
    snapshot: &Snapshot<'_>,
    spec: &str,
) -> Result<Vec<Operation>, VerifyError> {
    let generated = generated_at(repository, snapshot)
        .and_then(|documents| documents.iter().find(|document| document.name == spec));
    let file = generated.map_or_else(|| spec.to_owned(), |found| found.path.display().to_string());
    let key = (
        repository.path.clone(),
        snapshot.commit.map(str::to_owned),
        snapshot.other.map(|other| other.map(str::to_owned)),
        file,
    );
    if let Some(found) = snapshot.specs.read.borrow().get(&key) {
        return Ok(found.clone());
    }
    let operations = read_spec_operations(repository, snapshot, spec)?;
    snapshot
        .specs
        .read
        .borrow_mut()
        .insert(key, operations.clone());
    Ok(operations)
}

fn read_spec_operations(
    repository: &Repository,
    snapshot: &Snapshot<'_>,
    spec: &str,
) -> Result<Vec<Operation>, VerifyError> {
    let generated = generated_at(repository, snapshot)
        .and_then(|documents| documents.iter().find(|document| document.name == spec));
    let text = if let Some(generated) = generated {
        Some(
            std::fs::read_to_string(&generated.path).map_err(|source| VerifyError::Generated {
                file: generated.path.display().to_string(),
                source,
            })?,
        )
    } else {
        read_at_snapshot(repository, snapshot, spec)?
    };
    let Some(text) = text else {
        return Ok(Vec::new());
    };
    let is_yaml = generated.is_none() && lemma_http::is_yaml_document(spec);
    let document: serde_json::Value = if is_yaml {
        yaml_serde::from_str(&text).map_err(|source| VerifyError::YamlSpec {
            file: spec.to_owned(),
            source,
        })?
    } else {
        serde_json::from_str(&text).map_err(|source| VerifyError::Spec {
            file: spec.to_owned(),
            source,
        })?
    };
    Ok(lemma_http::operations(&document))
}

/// A file at the snapshot's commit, or at the change's other commit if the
/// change added or deleted it.
fn read_at_snapshot(
    repository: &Repository,
    snapshot: &Snapshot<'_>,
    file: &str,
) -> Result<Option<String>, VerifyError> {
    if let Some(text) = lemma_http::read_file(&repository.path, snapshot.commit, file)? {
        return Ok(Some(text));
    }
    let Some(other) = snapshot.other else {
        return Ok(None);
    };
    Ok(lemma_http::read_file(&repository.path, other, file)?)
}

/// A file's facts, read with this thread's extractor from `extractors`.
fn extracted(
    file: &SourceFile,
    options: &CodeOptions,
    extractors: &ExtractorPool,
) -> Result<CodeFacts, VerifyError> {
    let facts = extractors
        .with(|extractor| lemma_http::extract(&file.path, &file.contents, options, extractor))
        .map_err(|source| VerifyError::Syntax { source })??;
    Ok(facts)
}

/// The key a file's facts are kept under, in memory and between runs: the
/// build of Lemma, the path, the options (as their key text gives them)
/// and what names the contents ([`lemma_http::contents_key`]).
fn unit_key(build: &str, path: &str, options: &str, contents: &[u8]) -> String {
    let mut described = format!("{build}\0facts\0{path}\0{options}\0").into_bytes();
    described.extend_from_slice(contents);
    lemma_model::content_hash(&described)
}

/// Files with their unit keys, and the contents read to key those Git
/// cannot vouch for.
type Keyed = (Vec<(ListedFile, String)>, BTreeMap<String, SourceFile>);

/// A parsed file's facts, and their encoding when they should be kept.
type ReadFacts = (CodeFacts, Option<Vec<u8>>);

/// Reads files' code facts, each once per run for the same contents and
/// options (a file a change leaves alone has one blob at both its
/// commits), and keeps them between runs when given a cache.
struct FactReader {
    extractors: ExtractorPool,
    /// The build of Lemma and the units it keeps, when facts are kept.
    kept: Option<(String, lemma_store::UnitCache)>,
    /// Facts read in this run, by unit key.
    read: BTreeMap<String, Rc<CodeFacts>>,
    /// Units whose terms this run has sent to the cache, which its writer
    /// may not have stored yet.
    termed: BTreeSet<String>,
}

impl FactReader {
    /// A reader keeping facts in `cache`, if given. A cache that cannot be
    /// opened only means every file is parsed.
    fn new(cache: Option<crate::FactsCache>) -> Self {
        let kept = cache.and_then(|cache| {
            std::fs::create_dir_all(&cache.directory).ok()?;
            let units = lemma_store::UnitCache::open(&cache.directory.join("units.db")).ok()?;
            Some((cache.build, units))
        });
        Self {
            extractors: ExtractorPool::new(),
            kept,
            read: BTreeMap::new(),
            termed: BTreeSet::new(),
        }
    }

    /// The facts of `files` in `repository` at `commit` (the working
    /// directory when `None`), less test files and files no longer there,
    /// by path in `files`' order. A file is read from disk or Git only when
    /// its facts are not known by its blob: Git names files it can vouch
    /// for without their being read.
    fn read(
        &mut self,
        repository: &Path,
        commit: Option<&str>,
        files: &[ListedFile],
        options: &CodeOptions,
    ) -> Result<Vec<(String, Rc<CodeFacts>)>, VerifyError> {
        let (keyed, mut contents) = self.keyed(repository, commit, files, options)?;
        let unknown: Vec<(&ListedFile, &String)> = keyed
            .iter()
            .filter(|(_, key)| !self.read.contains_key(key))
            .map(|(file, key)| (file, key))
            .collect();
        let kept = self.kept_facts(&unknown);
        let to_read: Vec<String> = unknown
            .iter()
            .filter(|(file, key)| !kept.contains_key(*key) && !contents.contains_key(&file.path))
            .map(|(file, _)| file.path.clone())
            .collect();
        for file in lemma_http::read_listed(repository, commit, &to_read)? {
            contents.insert(file.path.clone(), file);
        }
        let to_parse: Vec<(&SourceFile, &String)> = unknown
            .iter()
            .filter(|(_, key)| !kept.contains_key(*key))
            .filter_map(|(file, key)| Some((contents.get(&file.path)?, *key)))
            .collect();
        let parsed = self.parse(&to_parse, options)?;
        self.read.extend(kept);
        self.read.extend(parsed);
        Ok(keyed
            .iter()
            .filter_map(|(file, key)| Some((file.path.clone(), Rc::clone(self.read.get(key)?))))
            .collect())
    }

    /// `files` less test files and files no longer there, each with its unit
    /// key, and the contents read to key the files Git cannot vouch for.
    fn keyed(
        &self,
        repository: &Path,
        commit: Option<&str>,
        files: &[ListedFile],
        options: &CodeOptions,
    ) -> Result<Keyed, VerifyError> {
        let code: Vec<&ListedFile> = files
            .iter()
            .filter(|file| !lemma_http::is_test_file(&file.path))
            .collect();
        // A file Git has no blob for is named by its contents, so those are
        // read first.
        let unvouched: Vec<String> = code
            .iter()
            .filter(|file| file.blob.is_none())
            .map(|file| file.path.clone())
            .collect();
        let contents: BTreeMap<String, SourceFile> =
            lemma_http::read_listed(repository, commit, &unvouched)?
                .into_iter()
                .map(|file| (file.path.clone(), file))
                .collect();
        let build = self.kept.as_ref().map_or("", |(build, _)| build.as_str());
        let options_text = format!("{options:?}");
        let mut keyed = Vec::new();
        for file in code {
            // A file gone between listing and reading has no key.
            let Some(named) = lemma_http::contents_key(file, contents.get(&file.path)) else {
                continue;
            };
            let key = unit_key(build, &file.path, &options_text, &named);
            keyed.push((file.clone(), key));
        }
        Ok((keyed, contents))
    }

    /// The paths of those of `files` whose facts may write a call that one
    /// of `terms` names, or that re-export a module whole, found by their
    /// indexed terms without reading the others' facts. A file whose terms
    /// are not indexed yet is read and indexed now; without a cache every
    /// file is read.
    fn select(
        &mut self,
        (repository, commit): (&Path, Option<&str>),
        files: &[ListedFile],
        options: &CodeOptions,
        terms: &BTreeSet<String>,
    ) -> Result<BTreeSet<String>, VerifyError> {
        let (keyed, _) = self.keyed(repository, commit, files, options)?;
        let mut wanted = terms.clone();
        wanted.insert(crate::terms::REEXPORTS.to_owned());
        let matches = |facts: &CodeFacts| {
            crate::terms::code_terms(facts)
                .iter()
                .any(|term| wanted.contains(term))
        };
        let keys: Vec<String> = keyed.iter().map(|(_, key)| key.clone()).collect();
        let among: BTreeSet<String> = keys.iter().cloned().collect();
        // A cache that cannot be read, in either lookup, only means reading
        // every file: none counts as indexed.
        let (indexed, hits) = match &self.kept {
            Some((_, units)) => {
                let wanted: Vec<String> = wanted.iter().cloned().collect();
                match (units.indexed(&keys), units.units_with(&wanted, &among)) {
                    (Ok(indexed), Ok(hits)) => (indexed, hits),
                    _ => (BTreeSet::new(), BTreeSet::new()),
                }
            }
            None => (BTreeSet::new(), BTreeSet::new()),
        };
        let mut chosen: BTreeSet<String> = keyed
            .iter()
            .filter(|(_, key)| indexed.contains(key) && hits.contains(key))
            .map(|(file, _)| file.path.clone())
            .collect();
        let unindexed: Vec<ListedFile> = keyed
            .iter()
            .filter(|(_, key)| !indexed.contains(key))
            .map(|(file, _)| file.clone())
            .collect();
        if unindexed.is_empty() {
            return Ok(chosen);
        }
        let key_of: BTreeMap<&str, &String> = keyed
            .iter()
            .map(|(file, key)| (file.path.as_str(), key))
            .collect();
        let mut backfill = Vec::new();
        for (path, facts) in self.read(repository, commit, &unindexed, options)? {
            if matches(&facts) {
                chosen.insert(path.clone());
            }
            let Some(key) = key_of.get(path.as_str()) else {
                continue;
            };
            if self.termed.insert((*key).clone()) {
                backfill.push(((*key).clone(), crate::terms::code_terms(&facts)));
            }
        }
        if let Some((_, units)) = &mut self.kept {
            units.put_terms_later(backfill);
        }
        Ok(chosen)
    }

    /// The facts the cache keeps for `files`, decoded in parallel. A cache
    /// that cannot be read, or an entry that does not decode, only means
    /// parsing.
    fn kept_facts(&self, files: &[(&ListedFile, &String)]) -> BTreeMap<String, Rc<CodeFacts>> {
        let Some((_, units)) = &self.kept else {
            return BTreeMap::new();
        };
        let keys: Vec<String> = files.iter().map(|(_, key)| (*key).clone()).collect();
        let decoded = units.get_decoded(&keys, |bytes| {
            serde_json::from_slice::<CodeFacts>(bytes).ok()
        });
        decoded
            .unwrap_or_default()
            .into_iter()
            .map(|(key, facts)| (key, Rc::new(facts)))
            .collect()
    }

    /// Parses `files` in parallel and keeps the new facts, by key. The
    /// results keep the files' order, so the first failure in that order
    /// is the one reported, whatever thread hit it.
    fn parse(
        &mut self,
        files: &[(&SourceFile, &String)],
        options: &CodeOptions,
    ) -> Result<Vec<(String, Rc<CodeFacts>)>, VerifyError> {
        use rayon::prelude::*;
        let is_keeping = self.kept.is_some();
        let extractors = &self.extractors;
        let read: Vec<Result<ReadFacts, VerifyError>> = files
            .par_iter()
            .with_min_len(8)
            .map(|(file, _)| {
                let facts = extracted(file, options, extractors)?;
                let encoded = is_keeping
                    .then(|| serde_json::to_vec(&facts).ok())
                    .flatten();
                Ok((facts, encoded))
            })
            .collect();
        let mut parsed = Vec::new();
        let mut new_units = Vec::new();
        let mut new_terms = Vec::new();
        for ((_, key), result) in files.iter().zip(read) {
            let (facts, encoded) = result?;
            if let Some(encoded) = encoded {
                new_units.push(((*key).clone(), encoded));
                new_terms.push(((*key).clone(), crate::terms::code_terms(&facts)));
                self.termed.insert((*key).clone());
            }
            parsed.push(((*key).clone(), Rc::new(facts)));
        }
        if let Some((_, units)) = &mut self.kept {
            units.put_later(new_units);
            units.put_terms_later(new_terms);
        }
        Ok(parsed)
    }
}

/// `GET /v1/users/{}`: how rules name an operation.
pub fn operation_key(method: &str, path: &str) -> String {
    format!("{method} {path}")
}

/// The last name in a field path: `hooks[].handler.action` → `action`.
fn leaf(field: &str) -> &str {
    field
        .rsplit('.')
        .next()
        .unwrap_or(field)
        .trim_end_matches("[]")
}

/// The call name a client is assumed to use when the manifest gives none:
/// the `operationId` as written.
const DEFAULT_CALL_NAME: &str = "{operationId}";

/// A generated client's name for an operation, from a template naming its
/// `operationId` as written (`{operationId}`) or recased: `{camel}`
/// (`listUsers`), `{pascal}` (`ListUsers`) or `{snake}` (`list_users`).
/// Generators differ, so the manifest says which (`call_names`).
fn call_name(template: &str, operation_id: &str) -> String {
    let words = words(operation_id);
    let capitalized: Vec<String> = words.iter().map(|word| capitalize(word)).collect();
    let camel = match capitalized.split_first() {
        Some((first, rest)) => first.to_lowercase() + &rest.concat(),
        None => String::new(),
    };
    template
        .replace("{operationId}", operation_id)
        .replace("{camel}", &camel)
        .replace("{pascal}", &capitalized.concat())
        .replace("{snake}", &words.join("_"))
}

/// An identifier's words, split at `_`, `-`, `.`, spaces and lower-to-upper
/// case changes, in lower case: `get_pollRevokeJob` is `get poll revoke job`.
fn words(identifier: &str) -> Vec<String> {
    let mut words = Vec::new();
    let mut current = String::new();
    let mut previous_lower = false;
    for character in identifier.chars() {
        if matches!(character, '_' | '-' | '.' | ' ') {
            words.extend((!current.is_empty()).then(|| std::mem::take(&mut current)));
            previous_lower = false;
            continue;
        }
        if character.is_uppercase() && previous_lower {
            words.push(std::mem::take(&mut current));
        }
        previous_lower = character.is_lowercase() || character.is_ascii_digit();
        current.extend(character.to_lowercase());
    }
    words.extend((!current.is_empty()).then_some(current));
    words
}

fn capitalize(word: &str) -> String {
    let mut characters = word.chars();
    characters
        .next()
        .map(|first| first.to_uppercase().chain(characters).collect())
        .unwrap_or_default()
}

fn has_extension(path: &str, extension: &str) -> bool {
    Path::new(path)
        .extension()
        .is_some_and(|found| found.eq_ignore_ascii_case(extension))
}

/// The files `path` imports, resolved, and for Go the other files of its
/// package, which share its names without importing them.
fn imported_files(
    path: &str,
    facts: &CodeFacts,
    known: &BTreeSet<String>,
    imports: &PathAliases,
) -> Vec<String> {
    let mut imported: Vec<String> = facts
        .imports
        .iter()
        .filter_map(|specifier| resolve_import(path, specifier, known, imports))
        .collect();
    imported.extend(same_package(path, known));
    imported
}

/// For a Go file, the other files of its package: Go code uses names from
/// its whole package without importing them.
fn same_package(path: &str, known: &BTreeSet<String>) -> Vec<String> {
    if !has_extension(path, "go") {
        return Vec::new();
    }
    let directory = Path::new(path).parent().unwrap_or(Path::new(""));
    known
        .iter()
        .filter(|other| {
            other.as_str() != path
                && has_extension(other, "go")
                && !lemma_http::is_test_file(other)
                && Path::new(other.as_str()).parent() == Some(directory)
        })
        .cloned()
        .collect()
}

/// The repository file a relative import names, if it is one of `known`.
fn resolve_import(
    file: &str,
    specifier: &str,
    known: &BTreeSet<String>,
    aliases: &PathAliases,
) -> Option<String> {
    if has_extension(file, "rs") {
        return rust_module(file, specifier, known);
    }
    if !specifier.starts_with('.') {
        return aliases
            .candidates(file, specifier)
            .iter()
            .find_map(|base| module_file(base, known));
    }
    let directory = Path::new(file).parent().unwrap_or(Path::new(""));
    let is_python = Path::new(file)
        .extension()
        .is_some_and(|extension| extension.eq_ignore_ascii_case("py"));
    let base = if is_python {
        python_module(directory, specifier)?
    } else {
        join(directory, specifier.trim_end_matches(".js"))?
    };
    module_file(&base, known)
}

/// The file a module path names, trying source extensions and index files.
fn module_file(base: &str, known: &BTreeSet<String>) -> Option<String> {
    let candidates = [
        base.to_owned(),
        format!("{base}.ts"),
        format!("{base}.tsx"),
        format!("{base}.js"),
        format!("{base}.jsx"),
        format!("{base}/index.ts"),
        format!("{base}/index.tsx"),
        format!("{base}/index.js"),
        format!("{base}.py"),
        format!("{base}/__init__.py"),
    ];
    candidates
        .into_iter()
        .find(|candidate| known.contains(candidate))
}

/// The file of the module a Rust `use` path names: `crate::models::Tool`
/// is `src/models.rs` or `src/models/mod.rs` (the last segments may be
/// items, so shorter paths are tried too).
fn rust_module(file: &str, specifier: &str, known: &BTreeSet<String>) -> Option<String> {
    let path = Path::new(file);
    let directory = path.parent().unwrap_or(Path::new(""));
    let is_module_root = matches!(
        path.file_name().and_then(|name| name.to_str()),
        Some("mod.rs" | "lib.rs" | "main.rs")
    );
    let mut segments = specifier.split("::");
    let base = match segments.next()? {
        "crate" => {
            let source_root = file.rfind("src/").map_or("", |index| &file[..index + 4]);
            Path::new(source_root).to_path_buf()
        }
        "self" if is_module_root => directory.to_path_buf(),
        "self" => directory.join(path.file_stem()?),
        "super" if is_module_root => directory.parent()?.to_path_buf(),
        "super" => directory.to_path_buf(),
        _ => return None,
    };
    let segments: Vec<&str> = segments.collect();
    for length in (1..=segments.len()).rev() {
        let module = segments[..length]
            .iter()
            .fold(base.clone(), |module, segment| module.join(segment));
        let module = module.to_string_lossy();
        let candidates = [format!("{module}.rs"), format!("{module}/mod.rs")];
        if let Some(found) = candidates
            .into_iter()
            .find(|candidate| known.contains(candidate))
        {
            return Some(found);
        }
    }
    None
}

/// `.models` or `..core.models` relative to a Python package directory.
fn python_module(directory: &Path, specifier: &str) -> Option<String> {
    let dots = specifier.chars().take_while(|c| *c == '.').count();
    let mut base = directory.to_path_buf();
    for _ in 1..dots {
        base = base.parent()?.to_path_buf();
    }
    let module = specifier[dots..].replace('.', "/");
    let joined = if module.is_empty() {
        base
    } else {
        base.join(module)
    };
    Some(joined.to_string_lossy().replace('\\', "/"))
}

/// Joins a relative specifier to a directory, resolving `.` and `..`.
fn join(directory: &Path, specifier: &str) -> Option<String> {
    let mut parts: Vec<String> = directory
        .iter()
        .map(|part| part.to_string_lossy().into_owned())
        .collect();
    for part in specifier.split('/') {
        match part {
            "." | "" => {}
            ".." => {
                parts.pop()?;
            }
            name => parts.push(name.to_owned()),
        }
    }
    Some(parts.join("/"))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn http_contracts() -> CompiledGrammar {
        let grammar = lemma_grammar::parse_grammar(crate::HTTP_CONTRACTS).unwrap();
        lemma_grammar::compile(&grammar).unwrap()
    }

    fn derive(grammar: &CompiledGrammar, relation: &str) -> Vec<Vec<String>> {
        let request = lemma_logic::Request::default();
        let derived = lemma_logic::evaluate(
            &lemma_logic::ReferenceEngine,
            &grammar.program,
            &grammar.facts,
            &request,
        )
        .unwrap();
        derived
            .rows(relation)
            .map(|row| {
                row.iter()
                    .map(|value| grammar.symbols.display(*value))
                    .collect()
            })
            .collect()
    }

    #[test]
    fn a_removed_operation_does_not_also_report_its_fields() {
        let mut grammar = http_contracts();
        let mut writer = FactWriter::new(&mut grammar);
        let kept = [
            Arg::Text("billing"),
            Arg::Text("GET /api/a"),
            Arg::Text("GET"),
            Arg::Text("/api/a"),
        ];
        let gone = [
            Arg::Text("billing"),
            Arg::Text("POST /api/b"),
            Arg::Text("POST"),
            Arg::Text("/api/b"),
        ];
        writer.add("operation", &["base", "head"], &kept);
        writer.add("operation", &["base"], &gone);
        for op in ["GET /api/a", "POST /api/b"] {
            let field = [Arg::Text(op), Arg::Text("name"), Arg::Text("name")];
            writer.add("request_field", &["base"], &field);
        }

        let removed = derive(&grammar, "removed_request_field");

        assert_eq!(removed, [["GET /api/a", "name", "name"]]);
        assert_eq!(derive(&grammar, "removed_operation"), [["POST /api/b"]]);
    }

    #[test]
    fn sending_a_removed_field_is_an_error_only_where_unknown_fields_fail() {
        let mut grammar = http_contracts();
        let mut writer = FactWriter::new(&mut grammar);
        for op in ["PATCH /api/strict", "PATCH /api/lenient"] {
            let path = op.split(' ').nth(1).unwrap();
            let operation = [
                Arg::Text("billing"),
                Arg::Text(op),
                Arg::Text("PATCH"),
                Arg::Text(path),
            ];
            writer.add("operation", &BOTH, &operation);
            let field = [Arg::Text(op), Arg::Text("old"), Arg::Text("old")];
            writer.add("request_field", &["base"], &field);
            let call = [
                Arg::Text("d:f::c"),
                Arg::Text("PATCH"),
                Arg::Text(path),
                Arg::Text("d:f"),
                Arg::Number(1),
            ];
            writer.add("calls_path", &BOTH, &call);
        }
        writer.add(
            "strict_request",
            &["head"],
            &[Arg::Text("PATCH /api/strict")],
        );
        writer.add("mentions", &BOTH, &[Arg::Text("d:f"), Arg::Text("old")]);
        writer.add("in_file", &BOTH, &[Arg::Text("d:f::c"), Arg::Text("d:f")]);

        let errors = derive(&grammar, "requirement:consumer_sends_rejected_field");
        let warnings = derive(&grammar, "requirement:consumer_uses_removed_request_field");

        assert_eq!(errors.len(), 1);
        assert_eq!(errors[0][1], "PATCH /api/strict");
        assert_eq!(warnings.len(), 1);
        assert_eq!(warnings[0][1], "PATCH /api/lenient");
    }

    #[test]
    fn strict_schemas_reach_routes_through_imports_and_unions() {
        let mut grammar = http_contracts();
        let mut writer = FactWriter::new(&mut grammar);
        let route = |path, part, schema| {
            [
                Arg::Text("billing"),
                Arg::Text("POST"),
                Arg::Text(path),
                Arg::Text(part),
                Arg::Text("p:routes.ts"),
                Arg::Text(schema),
            ]
        };
        writer.add(
            "route_schema",
            &["head"],
            &route("/api/a", "body", "Imported"),
        );
        writer.add(
            "route_schema",
            &["head"],
            &route("/api/b", "body", "Either"),
        );
        writer.add("route_schema", &["head"], &route("/api/c", "body", "Mixed"));
        writer.add(
            "imports",
            &["head"],
            &[Arg::Text("p:routes.ts"), Arg::Text("p:_schemas.ts")],
        );
        writer.add(
            "strict_schema",
            &["head"],
            &[Arg::Text("p:_schemas.ts"), Arg::Text("Imported")],
        );
        for (union, member) in [
            ("Either", "Imported"),
            ("Mixed", "Imported"),
            ("Mixed", "*"),
        ] {
            writer.add(
                "union_member",
                &["head"],
                &[
                    Arg::Text("p:routes.ts"),
                    Arg::Text(union),
                    Arg::Text(member),
                ],
            );
        }

        let strict = derive(&grammar, "strict_route");

        let paths: Vec<&str> = strict.iter().map(|row| row[2].as_str()).collect();
        assert_eq!(paths, ["/api/a", "/api/b"]);
    }

    #[test]
    fn a_name_in_another_function_of_the_file_is_not_a_use() {
        let mut grammar = http_contracts();
        let mut writer = FactWriter::new(&mut grammar);
        for (scope, name) in [
            ("d:f::caller", "own"),
            ("d:f::other", "elsewhere"),
            ("d:f", "shape"),
        ] {
            writer.add("mentions", &BOTH, &[Arg::Text(scope), Arg::Text(name)]);
            writer.add("in_file", &BOTH, &[Arg::Text(scope), Arg::Text("d:f")]);
        }

        let uses = derive(&grammar, "uses");

        let caller_uses: Vec<&str> = uses
            .iter()
            .filter(|row| row[0] == "d:f::caller")
            .map(|row| row[1].as_str())
            .collect();
        assert_eq!(caller_uses, ["own", "shape"]);
    }

    #[test]
    fn a_caller_sends_what_the_functions_it_calls_in_its_file_name() {
        let mut grammar = http_contracts();
        let mut writer = FactWriter::new(&mut grammar);
        for (scope, name) in [
            ("d:f::Api.update", "build_body"),
            ("d:f::Api.build_body", "connection_id"),
            ("d:f::unrelated", "region"),
        ] {
            writer.add("mentions", &BOTH, &[Arg::Text(scope), Arg::Text(name)]);
            writer.add("in_file", &BOTH, &[Arg::Text(scope), Arg::Text("d:f")]);
        }
        writer.add(
            "named",
            &BOTH,
            &[Arg::Text("d:f::Api.build_body"), Arg::Text("build_body")],
        );
        writer.add(
            "named",
            &BOTH,
            &[Arg::Text("d:f::unrelated"), Arg::Text("unrelated")],
        );

        let sends = derive(&grammar, "sends");

        let sent: Vec<&str> = sends
            .iter()
            .filter(|row| row[0] == "d:f::Api.update")
            .map(|row| row[1].as_str())
            .collect();
        assert_eq!(sent, ["build_body", "connection_id"]);
    }

    #[test]
    fn a_service_does_not_call_its_own_operations() {
        let mut grammar = http_contracts();
        let mut writer = FactWriter::new(&mut grammar);
        for (service, path) in [("billing", "/v1/invoices"), ("users", "/v1/users")] {
            let op = format!("GET {path}");
            writer.add(
                "operation",
                &BOTH,
                &[
                    Arg::Text(service),
                    Arg::Text(&op),
                    Arg::Text("GET"),
                    Arg::Text(path),
                ],
            );
            let call = [
                Arg::Text("a:route.ts"),
                Arg::Text("GET"),
                Arg::Text(path),
                Arg::Text("a:route.ts"),
                Arg::Number(1),
            ];
            writer.add("calls_path", &BOTH, &call);
        }
        writer.add(
            "part_of_service",
            &BOTH,
            &[Arg::Text("a:route.ts"), Arg::Text("billing")],
        );

        let calls = derive(&grammar, "calls");

        let operations: Vec<&str> = calls.iter().map(|row| row[1].as_str()).collect();
        assert_eq!(operations, ["GET /v1/users"]);
    }

    #[test]
    fn only_new_calls_under_a_service_prefix_are_unserved() {
        let mut grammar = http_contracts();
        let mut writer = FactWriter::new(&mut grammar);
        let operation = [
            Arg::Text("billing"),
            Arg::Text("GET /v1/billing/a"),
            Arg::Text("GET"),
            Arg::Text("/v1/billing/a"),
        ];
        writer.add("operation", &BOTH, &operation);
        let call = |path| {
            [
                Arg::Text("d:f::c"),
                Arg::Text("GET"),
                Arg::Text(path),
                Arg::Text("d:f"),
                Arg::Number(3),
            ]
        };
        writer.add("calls_path", &BOTH, &call("/v1/billing/old"));
        for path in ["/v1/billing/old", "/v1/billing/new", "/v1/other/x"] {
            writer.add("calls_path", &["head"], &call(path));
        }
        for path in ["/v1/billing/old", "/v1/billing/new"] {
            writer.add(
                "addressed_to",
                &BOTH,
                &[Arg::Text(path), Arg::Text("billing")],
            );
        }

        let unserved = derive(&grammar, "unserved_call");

        assert_eq!(unserved, [["d:f::c", "GET", "/v1/billing/new", "d:f", "3"]]);
    }

    fn known(paths: &[&str]) -> BTreeSet<String> {
        paths.iter().map(|path| (*path).to_owned()).collect()
    }

    #[test]
    fn typescript_imports_resolve_with_extensions_and_index_files() {
        let files = known(&["src/types/configs.ts", "src/models/index.ts"]);

        assert_eq!(
            resolve_import(
                "src/models/a.ts",
                "../types/configs.js",
                &files,
                &PathAliases::default()
            )
            .as_deref(),
            Some("src/types/configs.ts")
        );
        assert_eq!(
            resolve_import("src/b.ts", "./models", &files, &PathAliases::default()).as_deref(),
            Some("src/models/index.ts")
        );
    }

    #[test]
    fn rust_use_paths_resolve_to_module_files() {
        let files = known(&[
            "svc/src/models.rs",
            "svc/src/api/mod.rs",
            "svc/src/api/types.rs",
        ]);

        assert_eq!(
            resolve_import(
                "svc/src/api/client.rs",
                "crate::models::Tool",
                &files,
                &PathAliases::default()
            )
            .as_deref(),
            Some("svc/src/models.rs")
        );
        assert_eq!(
            resolve_import(
                "svc/src/api/client.rs",
                "super::types",
                &files,
                &PathAliases::default()
            )
            .as_deref(),
            Some("svc/src/api/types.rs")
        );
        assert_eq!(
            resolve_import(
                "svc/src/api/mod.rs",
                "self::types",
                &files,
                &PathAliases::default()
            )
            .as_deref(),
            Some("svc/src/api/types.rs")
        );
    }

    #[test]
    fn path_aliases_resolve_through_tsconfig_paths() {
        let files = known(&[
            "apps/web/src/server/trpc.ts",
            "apps/web/src/server/index.ts",
        ]);
        let aliases = PathAliases::from_files(&[SourceFile {
            path: "apps/web/tsconfig.json".to_owned(),
            contents: r#"{ "compilerOptions": { "paths": { "~/*": ["./src/*"] } } }"#.to_owned(),
        }]);

        assert_eq!(
            resolve_import(
                "apps/web/src/app/page.tsx",
                "~/server/trpc",
                &files,
                &aliases
            )
            .as_deref(),
            Some("apps/web/src/server/trpc.ts")
        );
        assert_eq!(
            resolve_import("apps/web/src/app/page.tsx", "~/server", &files, &aliases).as_deref(),
            Some("apps/web/src/server/index.ts")
        );
        assert_eq!(
            resolve_import("apps/web/src/a.ts", "react", &files, &aliases),
            None
        );
    }

    #[test]
    fn call_names_follow_the_template_a_generator_uses() {
        assert_eq!(
            call_name("{camel}", "get_pollRevokeJob"),
            "getPollRevokeJob"
        );
        assert_eq!(
            call_name("{pascal}.Invoke", "list-users"),
            "ListUsers.Invoke"
        );
        assert_eq!(call_name("{snake}", "ListUsersByOrg"), "list_users_by_org");
        assert_eq!(call_name("{operationId}", "users.list"), "users.list");
    }

    #[test]
    fn python_relative_imports_resolve_to_modules_and_packages() {
        let files = known(&["pkg/core/models.py", "pkg/types/__init__.py"]);

        assert_eq!(
            resolve_import("pkg/core/a.py", ".models", &files, &PathAliases::default()).as_deref(),
            Some("pkg/core/models.py")
        );
        assert_eq!(
            resolve_import("pkg/core/a.py", "..types", &files, &PathAliases::default()).as_deref(),
            Some("pkg/types/__init__.py")
        );
    }

    #[test]
    fn field_leaves_drop_containers_and_array_marks() {
        assert_eq!(leaf("hooks[].handler.action"), "action");
        assert_eq!(leaf("items[]"), "items");
    }
}
