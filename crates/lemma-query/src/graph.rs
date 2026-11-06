//! A use graph around one symbol: what uses it (callers) and what it uses
//! (callees), followed to a depth.
//!
//! Relationships come from one of two sources. The language server is
//! precise: callers are its references, and callees are where its
//! definitions of the names in the body point. Names are instant and need
//! no server: a symbol uses another if its body mentions the other's name.
//! Names that several symbols share are skipped as callees, since a mention
//! cannot say which one is meant.

use std::collections::{BTreeMap, BTreeSet, VecDeque};
use std::path::{Path, PathBuf};

use lemma_model::{GraphNode, Language, Outcome, Position, SymbolKind, Use, UseGraph};
use lemma_store::{FactStore, StoredSymbol};

use crate::QueryError;
use crate::lookup::LookupContext;
use crate::navigation::find_definitions;
use crate::references::find_references;
use crate::sessions::SessionPool;

/// Which way to follow uses from the symbol.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Direction {
    /// What uses it.
    Callers,
    /// What it uses.
    Callees,
    /// Both.
    Both,
}

/// Where relationships come from.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum GraphSource {
    /// The language server: precise, slower on first use, cached.
    Lsp,
    /// Names in the indexed code: instant, approximate.
    Names,
}

impl GraphSource {
    fn name(self) -> &'static str {
        match self {
            Self::Lsp => "lsp",
            Self::Names => "names",
        }
    }
}

/// How far and how to follow uses.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct GraphOptions {
    /// Which way.
    pub direction: Direction,
    /// How many uses away from the symbol to go; 1 is its direct callers
    /// or callees.
    pub depth: usize,
    /// Where relationships come from.
    pub source: GraphSource,
    /// The most nodes to include.
    pub max_nodes: usize,
}

/// Symbols defined by more than this many symbols are not matched by name.
const MAX_DEFINITIONS: usize = 3;
/// The most names in one body asked about with the language server.
const MAX_ASKED: usize = 60;

/// The use graph around `root`, built from the snapshot in `context`.
pub fn symbol_graph(
    store: &mut dyn FactStore,
    context: &LookupContext<'_>,
    root: &StoredSymbol,
    options: &GraphOptions,
    pool: &mut SessionPool,
) -> Result<UseGraph, QueryError> {
    let catalog = Catalog::load(
        &*store,
        context.snapshot.id,
        &context.snapshot.snapshot.root,
    )?;
    match options.source {
        GraphSource::Lsp => {
            let mut neighbours = ServerNeighbours {
                store,
                context,
                pool,
                catalog,
            };
            walk(root, options, &mut neighbours)
        }
        GraphSource::Names => {
            let mut graph = walk(root, options, &mut NameNeighbours::new(catalog))?;
            if graph.relationships.is_empty() {
                graph
                    .notes
                    .push(empty_name_graph_note(root, options.direction));
            }
            Ok(graph)
        }
    }
}

/// Why a name graph found nothing. Names are read only from the indexed
/// files, so an empty graph does not mean nothing uses the symbol: tests
/// outside the index's `include` often do.
fn empty_name_graph_note(root: &StoredSymbol, direction: Direction) -> String {
    let name = &root.symbol.name;
    let unmentioned = format!("no indexed file mentions `{name}`");
    let names_nothing = format!("`{name}` names no indexed symbol");
    let found = match direction {
        Direction::Callers => unmentioned,
        Direction::Callees => names_nothing,
        Direction::Both => format!("{unmentioned}, and {names_nothing}"),
    };
    format!(
        "{found}; files outside the snapshot are not read, and `lemma refs` or `--source lsp` may find uses there"
    )
}

/// What is next to a symbol.
#[derive(Debug, Default)]
struct Found {
    neighbours: Vec<Neighbour>,
    /// Why the question could not be fully answered, if it could not.
    note: Option<String>,
}

/// A symbol next to another, or a file's top-level code.
#[derive(Debug, Clone, PartialEq, Eq)]
enum Neighbour {
    Symbol(StoredSymbol),
    /// The top level of an indexed file.
    File(String),
    /// A file the language server read but the snapshot does not include.
    Unindexed(String),
}

/// A source of relationships. There are two: the language server and names.
trait Neighbours {
    fn callers(&mut self, of: &StoredSymbol) -> Result<Found, QueryError>;
    fn callees(&mut self, of: &StoredSymbol) -> Result<Found, QueryError>;
}

/// Follows uses breadth first from `root`, each way the options ask.
fn walk(
    root: &StoredSymbol,
    options: &GraphOptions,
    neighbours: &mut dyn Neighbours,
) -> Result<UseGraph, QueryError> {
    let mut graph = Builder::new(root, options);
    let ways: &[Way] = match options.direction {
        Direction::Both => &[Way::Up, Way::Down],
        Direction::Callers => &[Way::Up],
        Direction::Callees => &[Way::Down],
    };
    for way in ways {
        follow(*way, root, options.depth, neighbours, &mut graph)?;
    }
    Ok(graph.finish())
}

/// One way to follow uses: up to callers, or down to callees.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Way {
    Up,
    Down,
}

impl Way {
    fn name(self) -> &'static str {
        match self {
            Self::Up => "callers",
            Self::Down => "callees",
        }
    }
}

fn follow(
    way: Way,
    root: &StoredSymbol,
    max_depth: usize,
    neighbours: &mut dyn Neighbours,
    graph: &mut Builder,
) -> Result<(), QueryError> {
    let mut seen = BTreeSet::from([root.id]);
    let mut pending = VecDeque::from([(root.clone(), 0usize)]);
    while let Some((symbol, depth)) = pending.pop_front() {
        if depth >= max_depth {
            continue;
        }
        let found = match way {
            Way::Up => neighbours.callers(&symbol)?,
            Way::Down => neighbours.callees(&symbol)?,
        };
        for next in graph.record(way, &symbol, found) {
            if seen.insert(next.id) {
                pending.push_back((next, depth + 1));
            }
        }
    }
    Ok(())
}

/// A graph as it is built.
struct Builder {
    nodes: Vec<GraphNode>,
    ids: BTreeSet<String>,
    relationships: BTreeSet<Use>,
    notes: Vec<String>,
    truncated: bool,
    max_nodes: usize,
    root: String,
    source: GraphSource,
}

impl Builder {
    fn new(root: &StoredSymbol, options: &GraphOptions) -> Self {
        let mut builder = Self {
            nodes: Vec::new(),
            ids: BTreeSet::new(),
            relationships: BTreeSet::new(),
            notes: Vec::new(),
            truncated: false,
            max_nodes: options.max_nodes.max(1),
            root: node_id(root),
            source: options.source,
        };
        builder.add(&Neighbour::Symbol(root.clone()));
        builder
    }

    /// Adds a node unless it is already there; its id, or `None` if the
    /// graph is full.
    fn add(&mut self, neighbour: &Neighbour) -> Option<String> {
        let node = graph_node(neighbour);
        if self.ids.contains(&node.id) {
            return Some(node.id);
        }
        if self.nodes.len() >= self.max_nodes {
            self.truncated = true;
            return None;
        }
        let id = node.id.clone();
        self.ids.insert(id.clone());
        self.nodes.push(node);
        Some(id)
    }

    /// Adds what was found next to `symbol`; the symbols to follow on.
    fn record(&mut self, way: Way, symbol: &StoredSymbol, found: Found) -> Vec<StoredSymbol> {
        if let Some(note) = found.note {
            let asked = way.name();
            let name = &symbol.symbol.qualified_name;
            self.notes.push(format!("{asked} of {name}: {note}"));
        }
        let mut next = Vec::new();
        for neighbour in found.neighbours {
            let Some(id) = self.add(&neighbour) else {
                continue;
            };
            let (from, to) = match way {
                Way::Down => (node_id(symbol), id),
                Way::Up => (id, node_id(symbol)),
            };
            self.relationships.insert(Use {
                from,
                to,
                source: self.source.name().to_owned(),
            });
            if let Neighbour::Symbol(symbol) = neighbour {
                next.push(symbol);
            }
        }
        next
    }

    fn finish(self) -> UseGraph {
        UseGraph {
            nodes: self.nodes,
            relationships: self.relationships.into_iter().collect(),
            root: Some(self.root),
            source: self.source.name().to_owned(),
            notes: self.notes,
            truncated: self.truncated,
        }
    }
}

fn node_id(symbol: &StoredSymbol) -> String {
    format!("symbol:{}", symbol.id)
}

fn graph_node(neighbour: &Neighbour) -> GraphNode {
    match neighbour {
        Neighbour::Symbol(symbol) => GraphNode {
            id: node_id(symbol),
            label: symbol.symbol.qualified_name.clone(),
            kind: symbol.symbol.kind.as_str().to_owned(),
            path: Some(symbol.path.clone()),
            line: Some(symbol.symbol.span.start.line + 1),
        },
        Neighbour::File(path) => GraphNode {
            id: format!("file:{path}"),
            label: format!("{path} (top level)"),
            kind: "file".to_owned(),
            path: Some(path.clone()),
            line: None,
        },
        Neighbour::Unindexed(path) => GraphNode {
            id: format!("file:{path}"),
            label: format!("{path} (outside the snapshot)"),
            kind: "file".to_owned(),
            path: Some(path.clone()),
            line: None,
        },
    }
}

/// Every symbol in a snapshot, by file and by name, and the source text
/// of files as they are read.
struct Catalog {
    root: PathBuf,
    by_file: BTreeMap<String, Vec<StoredSymbol>>,
    by_name: BTreeMap<String, Vec<StoredSymbol>>,
    sources: BTreeMap<String, Option<String>>,
}

impl Catalog {
    fn load(store: &dyn FactStore, snapshot_id: i64, root: &str) -> Result<Self, QueryError> {
        let mut by_file = BTreeMap::new();
        let mut by_name: BTreeMap<String, Vec<StoredSymbol>> = BTreeMap::new();
        for file in store.files(snapshot_id)? {
            let symbols = store.symbols_in_file(snapshot_id, &file.path)?;
            for symbol in symbols.iter().filter(|symbol| is_usable(symbol)) {
                by_name
                    .entry(symbol.symbol.name.clone())
                    .or_default()
                    .push(symbol.clone());
            }
            by_file.insert(file.path, symbols);
        }
        Ok(Self {
            root: PathBuf::from(root),
            by_file,
            by_name,
            sources: BTreeMap::new(),
        })
    }

    /// A symbol's body: the lines it spans, with the first line's number
    /// (counting from 0).
    fn body(&mut self, symbol: &StoredSymbol) -> Option<(String, u32)> {
        let root = self.root.clone();
        let source = self
            .sources
            .entry(symbol.path.clone())
            .or_insert_with(|| read_source(&root, &symbol.path))
            .as_deref()?;
        let span = symbol.symbol.span;
        let lines: Vec<&str> = source
            .lines()
            .skip(span.start.line as usize)
            .take((span.end.line - span.start.line) as usize + 1)
            .collect();
        Some((lines.join("\n"), span.start.line))
    }

    /// Whether a line belongs to an import: `use`, `import`, `from ...
    /// import`, or a re-export, including one written over several lines.
    fn is_import(&mut self, path: &str, line: u32) -> bool {
        let root = self.root.clone();
        let Some(source) = self
            .sources
            .entry(path.to_owned())
            .or_insert_with(|| read_source(&root, path))
            .as_deref()
        else {
            return false;
        };
        let lines: Vec<&str> = source.lines().collect();
        let mut start = (line as usize).min(lines.len().saturating_sub(1));
        // Back to the line the statement starts on: the first after one that
        // ends a statement or block. Only top-level code is asked about, so
        // a line ending in `{` opens the statement (`use a::{`), not a body.
        while start > 0 {
            let previous = lines[start - 1].trim_end();
            if previous.is_empty() || previous.ends_with([';', '}']) {
                break;
            }
            start -= 1;
        }
        let text = lines.get(start).copied().unwrap_or_default().trim_start();
        let text = text.strip_prefix("pub ").unwrap_or(text);
        ["use ", "import ", "from ", "export {", "export *"]
            .iter()
            .any(|keyword| text.starts_with(keyword))
    }

    /// The innermost symbol in `path` whose definition contains `position`.
    fn enclosing(&self, path: &str, position: Position) -> Option<&StoredSymbol> {
        self.by_file
            .get(path)?
            .iter()
            .filter(|symbol| {
                symbol.symbol.span.start <= position && position < symbol.symbol.span.end
            })
            .max_by_key(|symbol| symbol.symbol.span.start)
    }

    /// The symbols a name may mean, if few enough share it.
    fn defined(&self, name: &str) -> Option<&[StoredSymbol]> {
        self.by_name
            .get(name)
            .filter(|symbols| symbols.len() <= MAX_DEFINITIONS)
            .map(Vec::as_slice)
    }
}

/// Symbols a use can point at. Variables are left out: their names are too
/// often reused for unrelated local values.
fn is_usable(symbol: &StoredSymbol) -> bool {
    symbol.symbol.kind != SymbolKind::Variable
}

fn read_source(root: &Path, path: &str) -> Option<String> {
    std::fs::read_to_string(root.join(path)).ok()
}

/// Relationships from the language server.
struct ServerNeighbours<'a, 'b> {
    store: &'a mut dyn FactStore,
    context: &'a LookupContext<'b>,
    pool: &'a mut SessionPool,
    catalog: Catalog,
}

impl Neighbours for ServerNeighbours<'_, '_> {
    fn callers(&mut self, of: &StoredSymbol) -> Result<Found, QueryError> {
        let report = find_references(&mut *self.store, self.context, of.clone(), false, self.pool)?;
        let references = match report.answer.outcome {
            Outcome::Found(references) => references,
            Outcome::Empty => return Ok(Found::default()),
            other => {
                return Ok(Found {
                    neighbours: Vec::new(),
                    note: Some(other.state().replace('_', " ")),
                });
            }
        };
        let mut neighbours = Vec::new();
        for reference in references.iter().filter(|reference| !reference.is_external) {
            let enclosing = self
                .catalog
                .enclosing(&reference.path, reference.span.start)
                .cloned();
            let neighbour = match enclosing {
                Some(symbol) if symbol.id == of.id => continue,
                Some(symbol) => Neighbour::Symbol(symbol),
                // An import names the symbol without using it.
                None if self
                    .catalog
                    .is_import(&reference.path, reference.span.start.line) =>
                {
                    continue;
                }
                None if !self.catalog.by_file.contains_key(&reference.path) => {
                    Neighbour::Unindexed(reference.path.clone())
                }
                None => Neighbour::File(reference.path.clone()),
            };
            if !neighbours.contains(&neighbour) {
                neighbours.push(neighbour);
            }
        }
        Ok(Found {
            neighbours,
            note: None,
        })
    }

    fn callees(&mut self, of: &StoredSymbol) -> Result<Found, QueryError> {
        let Some((body, first_line)) = self.catalog.body(of) else {
            return Ok(unreadable(of));
        };
        let candidates = self.candidates(of, &body, first_line);
        let mut neighbours = Vec::new();
        let mut unanswered = BTreeSet::new();
        for position in candidates {
            match self.defined_at(of, position)? {
                Ok(symbols) => push_new(&mut neighbours, symbols, of),
                Err(state) => {
                    unanswered.insert(state);
                }
            }
        }
        let note =
            (!unanswered.is_empty()).then(|| unanswered.into_iter().collect::<Vec<_>>().join(", "));
        Ok(Found { neighbours, note })
    }
}

impl ServerNeighbours<'_, '_> {
    /// Where to ask: the first use of each name in the body that names an
    /// indexed symbol, after the symbol's own name.
    fn candidates(&self, of: &StoredSymbol, body: &str, first_line: u32) -> Vec<Position> {
        let mut asked = BTreeSet::new();
        code_words(body, first_line, of.language)
            .into_iter()
            .filter(|word| {
                word.name != of.symbol.name
                    && self.catalog.by_name.contains_key(&word.name)
                    && word.position > of.symbol.name_span.start
            })
            .filter(|word| asked.insert(word.name.clone()))
            .take(MAX_ASKED)
            .map(|word| word.position)
            .collect()
    }

    /// The indexed symbols the name at `position` is defined as, or why the
    /// server could not say.
    fn defined_at(
        &mut self,
        of: &StoredSymbol,
        position: Position,
    ) -> Result<Result<Vec<StoredSymbol>, String>, QueryError> {
        let report = find_definitions(
            &mut *self.store,
            self.context,
            &of.path,
            position,
            self.pool,
        )?;
        Ok(match report.answer.outcome {
            Outcome::Found(targets) => Ok(targets
                .into_iter()
                .filter_map(|target| target.symbol)
                .collect()),
            Outcome::Empty => Ok(Vec::new()),
            other => Err(other.state().replace('_', " ")),
        })
    }
}

/// Adds the symbols not already there, other than `of` itself and its own
/// members (a type does not use its methods).
fn push_new(neighbours: &mut Vec<Neighbour>, symbols: Vec<StoredSymbol>, of: &StoredSymbol) {
    let others = symbols
        .into_iter()
        .filter(|symbol| symbol.id != of.id && !contains(of, symbol));
    for symbol in others {
        let neighbour = Neighbour::Symbol(symbol);
        if !neighbours.contains(&neighbour) {
            neighbours.push(neighbour);
        }
    }
}

fn unreadable(symbol: &StoredSymbol) -> Found {
    Found {
        neighbours: Vec::new(),
        note: Some(format!("{} could not be read", symbol.path)),
    }
}

/// Relationships from names: a symbol uses another if its body mentions the
/// other's name.
struct NameNeighbours {
    catalog: Catalog,
    /// Every symbol that can call, with the names its body mentions; read
    /// when first needed.
    mentions: Option<Vec<(StoredSymbol, Mentions)>>,
}

impl NameNeighbours {
    fn new(catalog: Catalog) -> Self {
        Self {
            catalog,
            mentions: None,
        }
    }

    fn mentions(&mut self) -> &[(StoredSymbol, Mentions)] {
        if self.mentions.is_none() {
            let callers: Vec<StoredSymbol> = self
                .catalog
                .by_file
                .values()
                .flatten()
                .filter(|symbol| {
                    matches!(
                        symbol.symbol.kind,
                        SymbolKind::Function | SymbolKind::Method
                    )
                })
                .cloned()
                .collect();
            let mut read = Vec::with_capacity(callers.len());
            for symbol in callers {
                let names = names_in(&mut self.catalog, &symbol);
                read.push((symbol, names));
            }
            self.mentions = Some(read);
        }
        self.mentions.as_deref().unwrap_or_default()
    }
}

impl Neighbours for NameNeighbours {
    fn callers(&mut self, of: &StoredSymbol) -> Result<Found, QueryError> {
        let name = of.symbol.name.clone();
        let needs_call = is_callable(of);
        let is_shared = self.catalog.defined(&name).is_none();
        // A name many symbols share must come with its type's name too.
        let container = of
            .symbol
            .qualified_name
            .rsplit_once('.')
            .map(|(container, _)| container.rsplit('.').next().unwrap_or(container).to_owned())
            .filter(|_| is_shared);
        let matching: Vec<StoredSymbol> = self
            .mentions()
            .iter()
            .filter(|(symbol, mentions)| {
                symbol.id != of.id
                    && same_language(symbol, of)
                    && mentions.uses(&name, needs_call)
                    && container
                        .as_ref()
                        .is_none_or(|container| mentions.uses(container, false))
            })
            .map(|(symbol, _)| symbol.clone())
            .collect();
        let neighbours = innermost(matching)
            .into_iter()
            .filter(|symbol| !contains(symbol, of))
            .map(Neighbour::Symbol)
            .collect();
        let note = (is_shared && container.is_none()).then(|| {
            format!(
                "`{name}` names more than {MAX_DEFINITIONS} symbols, so callers may be unrelated"
            )
        });
        Ok(Found { neighbours, note })
    }

    fn callees(&mut self, of: &StoredSymbol) -> Result<Found, QueryError> {
        let Some((body, first_line)) = self.catalog.body(of) else {
            return Ok(unreadable(of));
        };
        let mut neighbours = Vec::new();
        let mentions = Mentions::of(&code_words(&body, first_line, of.language));
        for (name, called) in &mentions.0 {
            if name == &of.symbol.name {
                continue;
            }
            let Some(defined) = self.catalog.defined(name) else {
                continue;
            };
            let used = defined.iter().filter(|symbol| {
                symbol.id != of.id
                    && same_language(symbol, of)
                    && !contains(of, symbol)
                    && (*called || !is_callable(symbol))
            });
            neighbours.extend(used.cloned().map(Neighbour::Symbol));
        }
        Ok(Found {
            neighbours,
            note: None,
        })
    }
}

/// The names a symbol's code mentions.
fn names_in(catalog: &mut Catalog, symbol: &StoredSymbol) -> Mentions {
    let Some((body, line)) = catalog.body(symbol) else {
        return Mentions::default();
    };
    Mentions::of(&code_words(&body, line, symbol.language))
}

/// The names some code mentions, each with whether it is ever called.
#[derive(Debug, Default)]
struct Mentions(BTreeMap<String, bool>);

impl Mentions {
    fn of(words: &[Word]) -> Self {
        let mut names = BTreeMap::new();
        for word in words {
            *names.entry(word.name.clone()).or_insert(false) |= word.is_call;
        }
        Self(names)
    }

    /// Whether the code uses `name`: calls it, when `as_call`.
    fn uses(&self, name: &str, as_call: bool) -> bool {
        self.0.get(name).is_some_and(|called| *called || !as_call)
    }
}

/// Whether two symbols are in one language, as far as a use by name goes:
/// TypeScript with and without JSX count as one.
fn same_language(left: &StoredSymbol, right: &StoredSymbol) -> bool {
    let family = |language: Language| match language {
        Language::Tsx => Language::TypeScript,
        other => other,
    };
    family(left.language) == family(right.language)
}

/// Whether a symbol is used by calling it, so only a call counts as a use.
fn is_callable(symbol: &StoredSymbol) -> bool {
    matches!(
        symbol.symbol.kind,
        SymbolKind::Function | SymbolKind::Method
    )
}

/// Whether `outer`'s definition contains `inner`'s.
fn contains(outer: &StoredSymbol, inner: &StoredSymbol) -> bool {
    outer.path == inner.path
        && outer.symbol.span.start <= inner.symbol.span.start
        && inner.symbol.span.end <= outer.symbol.span.end
}

/// Symbols that contain none of the others: a method mentioning a name is
/// the caller, not also the class around it.
fn innermost(symbols: Vec<StoredSymbol>) -> Vec<StoredSymbol> {
    let kept: Vec<bool> = symbols
        .iter()
        .map(|outer| {
            !symbols
                .iter()
                .any(|inner| inner.id != outer.id && contains(outer, inner))
        })
        .collect();
    symbols
        .into_iter()
        .zip(kept)
        .filter_map(|(symbol, keep)| keep.then_some(symbol))
        .collect()
}

/// An identifier in code, where it starts, and whether it is called.
#[derive(Debug, Clone, PartialEq, Eq)]
struct Word {
    name: String,
    position: Position,
    is_call: bool,
}

/// The identifiers in `text`'s code, outside comments and string literals.
/// `first_line` is the line `text` starts on, counting from 0; columns are
/// UTF-8 bytes, as positions are.
fn code_words(text: &str, first_line: u32, language: Language) -> Vec<Word> {
    let mut found = Vec::new();
    let mut open_block = None;
    for (offset, line) in text.lines().enumerate() {
        let line_number = first_line + u32::try_from(offset).unwrap_or(u32::MAX);
        let code = mask_comments_and_strings(line, language, &mut open_block);
        for (column, word) in words(&code) {
            let after = code[column + word.len()..].trim_start();
            found.push(Word {
                name: word.to_owned(),
                position: Position {
                    line: line_number,
                    column: u32::try_from(column).unwrap_or(u32::MAX),
                },
                is_call: after.starts_with('(') || after.starts_with("::<"),
            });
        }
    }
    found
}

/// `line` with comments and string literals blanked out, byte for byte, so
/// columns still match. `open_block` carries a block comment or Python
/// triple-quoted string from one line to the next, as its closing text.
fn mask_comments_and_strings(
    line: &str,
    language: Language,
    open_block: &mut Option<&'static str>,
) -> String {
    let is_python = language == Language::Python;
    let mut code = line.as_bytes().to_vec();
    let mut index = 0;
    while index < line.len() {
        let rest = &line[index..];
        let masked = if let Some(end) = *open_block {
            rest.find(end).map_or(rest.len(), |at| {
                *open_block = None;
                at + end.len()
            })
        } else if (is_python && rest.starts_with('#')) || (!is_python && rest.starts_with("//")) {
            rest.len()
        } else if let Some(opener) = block_opener(rest, is_python) {
            *open_block = Some(opener);
            opener.len()
        } else if let Some(length) = string_length(rest, language) {
            length
        } else {
            index += rest.chars().next().map_or(1, char::len_utf8);
            continue;
        };
        code[index..index + masked].fill(b' ');
        index += masked;
    }
    String::from_utf8(code).unwrap_or_default()
}

/// A block comment or triple-quoted string starting here: its closing text.
fn block_opener(rest: &str, is_python: bool) -> Option<&'static str> {
    if is_python {
        return ["\"\"\"", "'''"]
            .into_iter()
            .find(|quote| rest.starts_with(quote));
    }
    rest.starts_with("/*").then_some("*/")
}

/// The length of a string literal starting here, through its closing quote
/// or the end of the line. Rust's `'` also starts lifetimes, so it is not
/// taken as a quote there.
fn string_length(rest: &str, language: Language) -> Option<usize> {
    let quote = rest.chars().next()?;
    let is_quote = quote == '"' || quote == '`' || (quote == '\'' && language != Language::Rust);
    if !is_quote {
        return None;
    }
    let mut escaped = false;
    for (offset, character) in rest.char_indices().skip(1) {
        match character {
            _ if escaped => escaped = false,
            '\\' => escaped = true,
            _ if character == quote => return Some(offset + 1),
            _ => {}
        }
    }
    Some(rest.len())
}

/// The identifiers in one line, with the byte column each starts at.
fn words(line: &str) -> Vec<(usize, &str)> {
    let mut found = Vec::new();
    let mut start = None;
    for (column, character) in line.char_indices().chain([(line.len(), ' ')]) {
        let is_part = character.is_alphanumeric() || character == '_';
        match (start, is_part) {
            (None, true) => start = Some(column),
            (Some(begin), false) => {
                let word = &line[begin..column];
                if word.chars().next().is_some_and(|first| !first.is_numeric()) {
                    found.push((begin, word));
                }
                start = None;
            }
            _ => {}
        }
    }
    found
}

#[cfg(test)]
mod tests {
    use super::*;
    use lemma_model::{Language, Span, Symbol};

    fn symbol(id: i64, qualified: &str, path: &str, lines: (u32, u32)) -> StoredSymbol {
        let name = qualified.rsplit('.').next().unwrap_or(qualified);
        let start = Position {
            line: lines.0,
            column: 0,
        };
        let end = Position {
            line: lines.1,
            column: 0,
        };
        StoredSymbol {
            id,
            path: path.to_owned(),
            language: Language::Python,
            symbol: Symbol {
                name: name.to_owned(),
                qualified_name: qualified.to_owned(),
                kind: SymbolKind::Function,
                span: Span { start, end },
                name_span: Span { start, end: start },
            },
        }
    }

    /// `charge` is called by `checkout`, which `main` calls; `charge` calls
    /// `send`.
    struct Fixed {
        symbols: Vec<StoredSymbol>,
    }

    impl Neighbours for Fixed {
        fn callers(&mut self, of: &StoredSymbol) -> Result<Found, QueryError> {
            let callers: &[i64] = match of.id {
                1 => &[2],
                2 => &[3],
                _ => &[],
            };
            Ok(self.found(callers))
        }

        fn callees(&mut self, of: &StoredSymbol) -> Result<Found, QueryError> {
            Ok(self.found(if of.id == 1 { &[4] } else { &[] }))
        }
    }

    impl Fixed {
        fn found(&self, ids: &[i64]) -> Found {
            let neighbours = self
                .symbols
                .iter()
                .filter(|symbol| ids.contains(&symbol.id))
                .cloned()
                .map(Neighbour::Symbol)
                .collect();
            Found {
                neighbours,
                note: None,
            }
        }
    }

    fn fixed() -> Fixed {
        Fixed {
            symbols: vec![
                symbol(1, "charge", "pay.py", (0, 3)),
                symbol(2, "checkout", "cart.py", (0, 3)),
                symbol(3, "main", "app.py", (0, 3)),
                symbol(4, "send", "mail.py", (0, 3)),
            ],
        }
    }

    fn options(direction: Direction, depth: usize) -> GraphOptions {
        GraphOptions {
            direction,
            depth,
            source: GraphSource::Names,
            max_nodes: 100,
        }
    }

    #[test]
    fn an_empty_caller_graph_says_the_indexed_files_do_not_mention_the_symbol() {
        let root = symbol(1, "Billing.charge", "pay.py", (0, 3));

        let note = empty_name_graph_note(&root, Direction::Callers);

        assert!(note.starts_with("no indexed file mentions `charge`; files outside the snapshot"));
    }

    #[test]
    fn a_reference_outside_the_snapshot_is_labelled_so() {
        let node = graph_node(&Neighbour::Unindexed("tests/test_pay.py".to_owned()));

        assert_eq!(node.label, "tests/test_pay.py (outside the snapshot)");
    }

    #[test]
    fn uses_are_followed_both_ways_to_the_depth_asked() {
        let mut neighbours = fixed();
        let root = neighbours.symbols[0].clone();

        let shallow = walk(&root, &options(Direction::Both, 1), &mut neighbours).unwrap();
        let deep = walk(&root, &options(Direction::Callers, 2), &mut neighbours).unwrap();

        let pairs = |graph: &UseGraph| -> Vec<(String, String)> {
            graph
                .relationships
                .iter()
                .map(|relationship| (relationship.from.clone(), relationship.to.clone()))
                .collect()
        };
        assert_eq!(
            pairs(&shallow),
            [
                ("symbol:1".to_owned(), "symbol:4".to_owned()),
                ("symbol:2".to_owned(), "symbol:1".to_owned()),
            ]
        );
        assert_eq!(
            pairs(&deep),
            [
                ("symbol:2".to_owned(), "symbol:1".to_owned()),
                ("symbol:3".to_owned(), "symbol:2".to_owned()),
            ]
        );
        assert_eq!(shallow.root.as_deref(), Some("symbol:1"));
    }

    #[test]
    fn a_full_graph_is_marked_truncated() {
        let mut neighbours = fixed();
        let root = neighbours.symbols[0].clone();
        let mut small = options(Direction::Callers, 3);
        small.max_nodes = 2;

        let graph = walk(&root, &small, &mut neighbours).unwrap();

        assert_eq!(graph.nodes.len(), 2);
        assert!(graph.truncated);
    }

    #[test]
    fn words_skip_comments_and_strings_and_mark_calls() {
        let rust = "fn run() {\n    send(\"definition\", to); // calls definition\n    /* old(\n    gone() */ keep\n}";
        let python = "def run():\n    \"\"\"Docs about send.\"\"\"\n    send(to)  # not this(";

        let words = |text: &str, language| -> Vec<(String, u32, u32, bool)> {
            code_words(text, 10, language)
                .into_iter()
                .map(|word| {
                    (
                        word.name,
                        word.position.line,
                        word.position.column,
                        word.is_call,
                    )
                })
                .collect()
        };

        assert_eq!(
            words(rust, Language::Rust),
            [
                ("fn".to_owned(), 10, 0, false),
                ("run".to_owned(), 10, 3, true),
                ("send".to_owned(), 11, 4, true),
                ("to".to_owned(), 11, 23, false),
                ("keep".to_owned(), 13, 14, false),
            ]
        );
        let python_names: Vec<String> = words(python, Language::Python)
            .into_iter()
            .map(|(name, ..)| name)
            .collect();
        assert_eq!(python_names, ["def", "run", "send", "to"]);
    }

    #[test]
    fn a_class_around_a_calling_method_is_not_a_caller_too() {
        let class = symbol(1, "Cart", "cart.py", (0, 9));
        let method = symbol(2, "Cart.pay", "cart.py", (2, 5));

        let kept = innermost(vec![class, method.clone()]);

        assert_eq!(kept, [method]);
    }
}
