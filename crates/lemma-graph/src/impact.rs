//! What a broken link affects further up: the operations whose handlers
//! reach the failing call, and the code in other repositories that calls
//! those operations, and so on.
//!
//! The call graph is read by name: a function calls another if its body
//! mentions the other's name. That is an approximation, so a trace is
//! reported as impact, never as a new problem. Names defined by several
//! functions in a repository are not followed, since a mention cannot say
//! which one is meant. Language servers can make this precise later.

use std::collections::{BTreeMap, BTreeSet, VecDeque};
use std::sync::Arc;

use crate::handlers::{Definition, Lookup, RouteSite, helper_called_at, resolve};

/// A function or method in a repository, with the names its body mentions.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct Function {
    /// The file it is in, relative to the repository root.
    pub file: String,
    /// Its name, such as `execute`.
    pub name: String,
    /// Its name prefixed by its containers, such as `Tools.execute`.
    pub qualified_name: String,
    /// What it is: `function`, `method`, `constant` or `variable`; empty
    /// for a file's top-level code.
    pub kind: String,
    /// Its first and last lines, counting from 1.
    pub lines: (u32, u32),
    /// Every identifier its body mentions.
    pub mentions: BTreeSet<String>,
    /// Names its code binds without being definitions of their own, such
    /// as a file's top-level destructuring (`const { handler } = make()`):
    /// code mentioning them reaches it.
    pub binds: Vec<String>,
}

/// A route a service serves, where it is declared.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Route {
    /// The repository.
    pub repository: String,
    /// The operation, as `METHOD /path`.
    pub operation: String,
    /// The service's name in findings.
    pub service: String,
    /// The file declaring it.
    pub file: String,
    /// The line declaring it, counting from 1.
    pub line: u32,
    /// The names that line gives its handler, most likely first (see
    /// [`crate::handlers::handler_names`]).
    pub handler_names: Vec<String>,
    /// The path literal as written on that line, if there is one.
    pub template: Option<String>,
    /// The declaring file's text, to find the handler the way exports do.
    pub source: Arc<str>,
    /// For a handler written inline (`r.HandleFunc("/x", func(w, r) {...})`),
    /// the identifiers its code mentions.
    pub inline_mentions: Option<BTreeSet<String>>,
    /// Whether the line passes the route's path to a helper that registers
    /// it (`registerExecute(router, '/v1/execute')`) rather than declaring
    /// it: the helper is its handler.
    pub passed_to_helper: bool,
}

/// A call from code to an operation, found by inference.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Call {
    /// The calling repository.
    pub repository: String,
    /// The file the call is in.
    pub file: String,
    /// The calling function's qualified name, if the call is in one.
    pub function: Option<String>,
    /// The operation called, as `METHOD /path`.
    pub operation: String,
}

/// Where a trace starts: names in one repository whose callers are wanted.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord)]
pub struct Seed {
    /// The repository.
    pub repository: String,
    /// The names to look for.
    pub names: Vec<String>,
}

/// One thing a broken link affects.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord)]
pub enum Affected {
    /// An operation whose handler reaches the break.
    Operation {
        /// The service serving it.
        service: String,
        /// `METHOD /path`.
        operation: String,
    },
    /// Code in another repository that calls an affected operation.
    Caller {
        /// The repository.
        repository: String,
        /// `file::Function`, or the file for a call outside functions.
        location: String,
        /// The operation it calls.
        operation: String,
    },
}

/// How many operations deep a trace follows calls across repositories.
const MAX_OPERATION_HOPS: usize = 4;
/// How many calls deep a trace follows within one repository.
const MAX_FUNCTION_HOPS: usize = 16;
/// Names defined by more functions than this in a repository are not
/// followed.
const MAX_DEFINITIONS: usize = 3;
/// Names mentioned by more functions than this in a repository are not
/// followed: reaching something through a utility everything uses is no
/// evidence.
const MAX_MENTIONS: usize = 40;

/// Where a trace starts in its first repository.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Start {
    /// Callers of these names, found by name.
    Names(Seed),
    /// These functions, already known to reach what is traced, such as
    /// from a language server. Their callers are followed on by name, to
    /// the handlers of routes.
    Reached {
        /// The repository.
        repository: String,
        /// Each function's file and first line.
        functions: Vec<(String, u32)>,
    },
}

/// A node a link joins: a function in the first repository, or something
/// affected.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord)]
pub enum Node {
    /// A function in the first repository.
    Function {
        /// The file, relative to the repository root.
        file: String,
        /// Its first line, counting from 1.
        line: u32,
        /// Its qualified name.
        name: String,
        /// What it is: `function`, `method`, `constant` or `variable`;
        /// empty for a file's top-level code.
        kind: String,
    },
    /// An affected operation or caller.
    Affected(Affected),
}

/// One step of a trace: `user` uses `used`, directly or through calls
/// within its repository.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord)]
pub struct Link {
    /// The node that uses.
    pub user: Node,
    /// The node it uses.
    pub used: Node,
    /// How the link was found.
    pub kind: LinkKind,
}

/// How a trace link was found.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum LinkKind {
    /// An operation reaching the code that serves it, through its route.
    Route,
    /// Code calling an operation, read from the requests it sends.
    Call,
    /// One function reaching another by naming it in its body.
    Name,
    /// A function naming a method's type rather than the method: it may
    /// call another method of that type.
    Type,
}

impl LinkKind {
    /// Its name in output: `route`, `call` or `name`.
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Route => "route",
            Self::Call => "call",
            Self::Name => "name",
            Self::Type => "type",
        }
    }
}

/// Everything reachable from `seed`: affected operations and callers, each
/// with how many operations away from the break it is, nearest first.
/// `functions` gives a repository's functions, read on demand.
#[cfg(test)]
fn trace(
    seed: &Seed,
    functions: &mut dyn FnMut(&str) -> Vec<Function>,
    routes: &[Route],
    calls: &[Call],
) -> Vec<(usize, Affected)> {
    let mut tracing = Tracing::default();
    trace_links(
        &mut tracing,
        &Start::Names(seed.clone()),
        functions,
        routes,
        calls,
    )
    .0
}

/// As [`trace`], from `start`, with the links between what it reaches:
/// operations to the handler (or caller) that reaches them, callers to the
/// operation they call, and, from reached functions, the calls from a
/// handler down to them.
pub fn trace_links(
    tracing: &mut Tracing,
    start: &Start,
    functions: &mut dyn FnMut(&str) -> Vec<Function>,
    routes: &[Route],
    calls: &[Call],
) -> (Vec<(usize, Affected)>, Vec<Link>) {
    let repositories = &mut tracing.repositories;
    let mut tracer = Tracer::default();
    tracer.pending.push_back(Step {
        start: start.clone(),
        hops: 0,
        via: None,
    });
    while let Some(step) = tracer.pending.pop_front() {
        let name = step.repository().to_owned();
        let repository = repositories
            .entry(name.clone())
            .or_insert_with(|| Repository::read(functions(&name), routes, &name));
        let walked = step.walk(&repository.functions, &repository.names);
        for (route, handler) in &repository.routes {
            if let Some(found) = handler.resolve(&repository.functions, &walked) {
                let handled = Handled {
                    route,
                    handler: found,
                    functions: &repository.functions,
                    walked: &walked,
                };
                tracer.visit(&step, &handled, calls);
            }
        }
    }
    tracer.finish()
}

/// What traces from several starts share: each repository's functions,
/// routes and name indexes, worked out once for all of them. A trace per
/// problem would otherwise index every repository again.
#[derive(Default)]
pub struct Tracing {
    repositories: BTreeMap<String, Repository>,
}

/// A repository's functions and routes, with each route's possible
/// handlers and the names its functions define and mention worked out
/// once.
struct Repository {
    functions: Vec<Function>,
    routes: Vec<(Route, Handler)>,
    names: NameIndex,
}

impl Repository {
    fn read(functions: Vec<Function>, routes: &[Route], name: &str) -> Self {
        let defined: BTreeSet<&str> = functions
            .iter()
            .map(|function| function.name.as_str())
            .collect();
        let routes: Vec<(Route, Handler)> = routes
            .iter()
            .filter(|route| route.repository == name)
            .map(|route| (route.clone(), Handler::of(route, &functions, &defined)))
            .collect();
        let mut names = NameIndex::of(&functions);
        names.registrations = registrations(&functions, &routes);
        Self {
            functions,
            routes,
            names,
        }
    }
}

/// Which function handles a route, once it is known what was reached.
#[derive(Debug, Clone, PartialEq, Eq)]
enum Handler {
    /// The definition the route's registration names, found as exports
    /// find it: a function, the constant holding a registration, or the
    /// file's top-level code.
    Definition(usize),
    /// A handler written inline: reached when its code mentions a reached
    /// function.
    Inline(BTreeSet<String>),
    /// A registration (`r.POST("/x", h.execute)`): the function its line
    /// names after the path.
    Named(Vec<String>),
    /// A route declared inside its handler (a file-based route): the
    /// function around its line.
    Enclosing(usize),
    /// A route outside functions: any function in its file, or the file's
    /// top-level code.
    File(String),
}

impl Handler {
    fn of(route: &Route, functions: &[Function], defined: &BTreeSet<&str>) -> Self {
        if let Some(mentions) = &route.inline_mentions {
            return Self::Inline(mentions.clone());
        }
        if let Some(index) = resolved_handler(route, functions) {
            return Self::Definition(index);
        }
        let enclosing = functions.iter().position(|function| {
            function.file == route.file
                && function.lines.0 <= route.line
                && route.line <= function.lines.1
        });
        let enclosing_name = enclosing.map(|index| functions[index].name.as_str());
        let named = route
            .handler_names
            .iter()
            .find(|name| defined.contains(name.as_str()) && Some(name.as_str()) != enclosing_name);
        if let Some(name) = named {
            return Self::Named(vec![name.clone()]);
        }
        match enclosing {
            Some(index) => Self::Enclosing(index),
            None => Self::File(route.file.clone()),
        }
    }

    /// The reached function that handles the route, if one does.
    fn resolve(&self, functions: &[Function], walked: &Walked) -> Option<usize> {
        match self {
            Self::Definition(index) | Self::Enclosing(index) => {
                walked.is_reached[*index].then_some(*index)
            }
            Self::Inline(mentions) => walked.order.iter().copied().find(|index| {
                callable_names(&functions[*index])
                    .iter()
                    .any(|name| mentions.contains(*name))
            }),
            Self::Named(names) => walked
                .order
                .iter()
                .copied()
                .find(|index| names.contains(&functions[*index].name)),
            Self::File(file) => walked
                .order
                .iter()
                .copied()
                .find(|index| &functions[*index].file == file),
        }
    }
}

/// The definition a route's registration names, found with the resolver
/// exports use, over the functions the trace reads.
fn resolved_handler(route: &Route, functions: &[Function]) -> Option<usize> {
    let template = route.template.as_deref()?;
    let definition = |function: &Function| Definition {
        path: function.file.clone(),
        qualified: function.qualified_name.clone(),
        kind: function.kind.clone(),
    };
    let unique = |name: &str, from: &str| {
        let mut named = functions
            .iter()
            .filter(|function| function.name == name && language(&function.file) == language(from));
        let first = named.next()?;
        named.next().is_none().then(|| definition(first))
    };
    let enclosing = |file: &str, line: u32| {
        functions
            .iter()
            .filter(|function| {
                function.file == file
                    && !function.kind.is_empty()
                    && function.lines.0 <= line
                    && line <= function.lines.1
            })
            .min_by_key(|function| function.lines.1 - function.lines.0)
            .map(definition)
    };
    let lookup = Lookup {
        unique: &unique,
        enclosing: &enclosing,
    };
    let site = RouteSite {
        file: &route.file,
        text: &route.source,
        line: route.line,
        template,
    };
    let (path, symbol) = if route.passed_to_helper {
        let helper = helper_called_at(&site, &lookup)?;
        (Some(helper.path), Some(helper.qualified))
    } else {
        resolve(&site, &lookup)
    };
    let path = path?;
    functions.iter().position(|function| {
        function.file == path
            && match &symbol {
                Some(symbol) => &function.qualified_name == symbol,
                None => function.kind.is_empty(),
            }
    })
}

/// A route whose handler a walk reached.
struct Handled<'a> {
    route: &'a Route,
    handler: usize,
    functions: &'a [Function],
    walked: &'a Walked,
}

/// What a trace has found so far, and where it has yet to look.
#[derive(Default)]
struct Tracer {
    /// Traced breadth first, so the first distance recorded is the shortest.
    affected: BTreeMap<Affected, usize>,
    links: BTreeSet<Link>,
    seen_operations: BTreeSet<String>,
    pending: VecDeque<Step>,
}

impl Tracer {
    /// Records a route whose handler was reached, and queues the callers of
    /// its operation.
    fn visit(&mut self, step: &Step, handled: &Handled<'_>, calls: &[Call]) {
        let route = handled.route;
        let operation = Affected::Operation {
            service: route.service.clone(),
            operation: route.operation.clone(),
        };
        self.affected.entry(operation.clone()).or_insert(step.hops);
        let used = if let Some(caller) = &step.via {
            Node::Affected(caller.clone())
        } else {
            let functions = handled.functions;
            self.links
                .extend(handled.walked.chain(handled.handler, functions));
            function_node(&functions[handled.handler])
        };
        self.links.insert(Link {
            user: Node::Affected(operation.clone()),
            used,
            kind: LinkKind::Route,
        });
        if step.hops >= MAX_OPERATION_HOPS || !self.seen_operations.insert(route.operation.clone())
        {
            return;
        }
        for call in calls
            .iter()
            .filter(|call| call.operation == route.operation)
        {
            let caller = caller(call);
            self.affected.entry(caller.clone()).or_insert(step.hops + 1);
            self.links.insert(Link {
                user: Node::Affected(caller.clone()),
                used: Node::Affected(operation.clone()),
                kind: LinkKind::Call,
            });
            self.pending.push_back(Step {
                start: Start::Names(call_seed(call)),
                hops: step.hops + 1,
                via: Some(caller),
            });
        }
    }

    fn finish(self) -> (Vec<(usize, Affected)>, Vec<Link>) {
        let mut nearest: Vec<(usize, Affected)> = self
            .affected
            .into_iter()
            .map(|(item, distance)| (distance, item))
            .collect();
        nearest.sort();
        (nearest, self.links.into_iter().collect())
    }
}

fn function_node(function: &Function) -> Node {
    Node::Function {
        file: function.file.clone(),
        line: function.lines.0,
        name: function.qualified_name.clone(),
        kind: function.kind.clone(),
    }
}

/// A repository to look for routes in, and how it was reached.
struct Step {
    start: Start,
    hops: usize,
    /// The caller in this repository that led here, after the first.
    via: Option<Affected>,
}

impl Step {
    fn repository(&self) -> &str {
        match &self.start {
            Start::Names(seed) => &seed.repository,
            Start::Reached { repository, .. } => repository,
        }
    }

    fn walk(&self, functions: &[Function], lookup: &NameIndex) -> Walked {
        match &self.start {
            Start::Names(seed) => {
                let seeds = seed.names.iter().map(|name| (name.clone(), None)).collect();
                let mut walked = Walked::from_names(functions, lookup, seeds, Vec::new());
                // Functions named by the seed are reached themselves.
                let named = functions
                    .iter()
                    .enumerate()
                    .filter(|(_, function)| seed.names.contains(&function.name));
                for (index, _) in named {
                    walked.add(index, None, false);
                }
                walked
            }
            Start::Reached {
                functions: given, ..
            } => {
                let given: Vec<usize> = functions
                    .iter()
                    .enumerate()
                    .filter(|(_, function)| {
                        given
                            .iter()
                            .any(|(file, line)| &function.file == file && function.lines.0 == *line)
                    })
                    .map(|(index, _)| index)
                    .collect();
                // The walk that found them may stop short of a route's
                // handler (a constructor registered for a method's type), so
                // it goes on by name from them.
                let reached: Vec<(usize, Option<usize>)> =
                    given.iter().map(|index| (*index, None)).collect();
                let seeds = next_names(functions, &reached, &lookup.followable, &BTreeSet::new())
                    .into_iter()
                    .collect();
                Walked::from_names(functions, lookup, seeds, given)
            }
        }
    }
}

/// The functions a walk reached, in order, each with the reached function
/// it calls that led to it.
struct Walked {
    order: Vec<usize>,
    is_reached: Vec<bool>,
    /// The function each reached one was reached through: the one it calls.
    callee: BTreeMap<usize, usize>,
    /// Functions reached before walking, which a chain stops at.
    given: BTreeSet<usize>,
    /// Functions reached by naming the type of the function they were
    /// reached through, not the function itself.
    through_type: BTreeSet<usize>,
}

/// A function a walk reaches: its index, the function it was reached
/// through, and whether through that function's type.
type Reached = (usize, Option<usize>, bool);

impl Walked {
    /// Walks upwards from `seeds` (names, each with the function it came
    /// from), after the `given` functions, which are reached already.
    fn from_names(
        functions: &[Function],
        names: &NameIndex,
        seeds: Vec<(String, Option<usize>)>,
        given: Vec<usize>,
    ) -> Self {
        let mut walked = Self {
            order: Vec::new(),
            is_reached: vec![false; functions.len()],
            callee: BTreeMap::new(),
            given: given.iter().copied().collect(),
            through_type: BTreeSet::new(),
        };
        for index in given {
            walked.add(index, None, false);
        }
        let followable = &names.followable;
        let mut frontier: BTreeMap<String, Option<usize>> = BTreeMap::new();
        for (name, from) in seeds {
            frontier.entry(name).or_insert(from);
        }
        let mut followed: BTreeSet<String> = BTreeSet::new();
        for _ in 0..MAX_FUNCTION_HOPS {
            let next = walked.callers(functions, names, &frontier);
            if next.is_empty() {
                break;
            }
            followed.extend(frontier.keys().cloned());
            let onward: Vec<(usize, Option<usize>)> = next
                .iter()
                .filter(|(index, _, _)| !names.registrations.contains(index))
                .map(|(index, from, _)| (*index, *from))
                .collect();
            let next_frontier = next_names(functions, &onward, followable, &followed);
            for (index, from, through_type) in next {
                walked.add(index, from, through_type);
            }
            frontier = next_frontier;
        }
        walked
    }

    /// Functions not yet reached that mention a name in `frontier`, other
    /// than their own or their type's: each with the function the name came
    /// from, and whether the name was that function's type.
    fn callers(
        &self,
        functions: &[Function],
        names: &NameIndex,
        frontier: &BTreeMap<String, Option<usize>>,
    ) -> Vec<Reached> {
        // Only functions that mention a frontier name can be callers; they
        // are taken in index order, as a scan of every function would.
        let candidates: BTreeSet<usize> = frontier
            .keys()
            .filter_map(|name| names.mentioners.get(name))
            .flatten()
            .copied()
            .collect();
        let mut found = Vec::new();
        for index in candidates {
            let function = &functions[index];
            if self.is_reached[index] {
                continue;
            }
            let own = callable_names(function);
            // A call by name stays within one language.
            let same_language = |from: &Option<usize>| {
                from.is_none_or(|from| language(&functions[from].file) == language(&function.file))
            };
            let from = function
                .mentions
                .iter()
                .filter(|name| !own.contains(&name.as_str()))
                .filter_map(|name| Some((name, frontier.get(name)?)))
                .find(|(_, from)| same_language(from));
            if let Some((name, from)) = from {
                let through_type = from.is_some_and(|from| functions[from].name != *name);
                found.push((index, *from, through_type));
            }
        }
        found
    }

    fn add(&mut self, index: usize, callee: Option<usize>, through_type: bool) {
        if self.is_reached[index] {
            return;
        }
        self.is_reached[index] = true;
        self.order.push(index);
        if through_type {
            self.through_type.insert(index);
        }
        if let Some(callee) = callee {
            self.callee.insert(index, callee);
        }
    }

    /// The calls from `handler` down to a given function, as links.
    fn chain(&self, handler: usize, functions: &[Function]) -> Vec<Link> {
        let mut links = Vec::new();
        let mut current = handler;
        while !self.given.contains(&current) {
            let Some(&callee) = self.callee.get(&current) else {
                break;
            };
            let kind = if self.through_type.contains(&current) {
                LinkKind::Type
            } else {
                LinkKind::Name
            };
            links.push(Link {
                user: function_node(&functions[current]),
                used: function_node(&functions[callee]),
                kind,
            });
            current = callee;
        }
        links
    }
}

/// The names to look for next: those of the functions just reached, unless
/// followed already or shared by several functions, each with the function
/// it belongs to.
fn next_names(
    functions: &[Function],
    reached: &[(usize, Option<usize>)],
    followable: &Followable,
    followed: &BTreeSet<String>,
) -> BTreeMap<String, Option<usize>> {
    let mut names = BTreeMap::new();
    for (index, _) in reached {
        let candidates = callable_names(&functions[*index])
            .into_iter()
            .filter(|name| followable.contains(name) && !followed.contains(*name));
        for name in candidates {
            names.entry(name.to_owned()).or_insert(Some(*index));
        }
    }
    names
}

/// The functions reached from `names` by following mentions upwards.
#[cfg(test)]
fn reach<'a>(index: &'a [Function], names: &[String]) -> Vec<&'a Function> {
    let seed = Seed {
        repository: String::new(),
        names: names.to_vec(),
    };
    let step = Step {
        start: Start::Names(seed),
        hops: 0,
        via: None,
    };
    step.walk(index, &NameIndex::of(index))
        .order
        .into_iter()
        .map(|position| &index[position])
        .collect()
}

/// The names a function is reached by: its own, and for a method its
/// type's, since callers often hold the type (a constructor, a handler
/// object) rather than name the method.
fn callable_names(function: &Function) -> Vec<&str> {
    if function.name.is_empty() {
        return function.binds.iter().map(String::as_str).collect();
    }
    let mut names = vec![function.name.as_str()];
    if let Some((container, _)) = function.qualified_name.rsplit_once('.') {
        names.push(container.rsplit('.').next().unwrap_or(container));
    }
    names
}

/// A file's language, as far as calls by name go: its extension, with
/// JavaScript and TypeScript as one.
fn language(file: &str) -> &str {
    match file.rsplit_once('.').map_or("", |(_, extension)| extension) {
        "ts" | "tsx" | "mts" | "cts" | "js" | "jsx" | "mjs" | "cjs" => "typescript",
        other => other,
    }
}

/// What a walk looks names up in: which it may follow, and which functions
/// mention each, by their index.
struct NameIndex {
    followable: Followable,
    mentioners: BTreeMap<String, Vec<usize>>,
    /// Functions holding a route's registration, whose names a walk does
    /// not follow on: what mentions a registration (a router collecting
    /// routes, a generator documenting them) reads the route, it does not
    /// call its handler, and the route's operation is reached through the
    /// route itself.
    registrations: BTreeSet<usize>,
}

impl NameIndex {
    fn of(functions: &[Function]) -> Self {
        let mut mentioners: BTreeMap<String, Vec<usize>> = BTreeMap::new();
        for (index, function) in functions.iter().enumerate() {
            let mentioned: BTreeSet<&String> = function.mentions.iter().collect();
            for name in mentioned {
                mentioners.entry(name.clone()).or_default().push(index);
            }
        }
        Self {
            followable: Followable::of(functions),
            mentioners,
            registrations: BTreeSet::new(),
        }
    }
}

/// The definitions that register `routes`: those whose code holds a route's
/// declaring line, and a handler that is a value rather than a function
/// (`const send = router.openapi(def, async (c) => ...)`), which code
/// reads as part of a route table but never calls.
fn registrations(functions: &[Function], routes: &[(Route, Handler)]) -> BTreeSet<usize> {
    let mut found = BTreeSet::new();
    for (route, handler) in routes {
        let holding = functions.iter().enumerate().filter(|(_, function)| {
            function.file == route.file
                && function.lines.0 <= route.line
                && route.line <= function.lines.1
        });
        found.extend(holding.map(|(index, _)| index));
        if let Handler::Definition(index) | Handler::Enclosing(index) = handler {
            let is_value = matches!(functions[*index].kind.as_str(), "constant" | "variable");
            if is_value {
                found.insert(*index);
            }
        }
    }
    found
}

/// The names a walk may follow: defined by few functions, so a mention
/// says which is meant, and mentioned by few, so reaching a function
/// through one is evidence (`errorToHTTPException`, which every handler
/// mentions, is not). A type's name, which no function defines, is held to
/// the second only.
struct Followable {
    definitions: BTreeMap<String, usize>,
    mentions: BTreeMap<String, usize>,
}

impl Followable {
    fn of(index: &[Function]) -> Self {
        let mut definitions: BTreeMap<String, usize> = BTreeMap::new();
        let mut mentions: BTreeMap<String, usize> = BTreeMap::new();
        for function in index {
            *definitions.entry(function.name.clone()).or_insert(0) += 1;
            for bound in &function.binds {
                *definitions.entry(bound.clone()).or_insert(0) += 1;
            }
            for name in &function.mentions {
                *mentions.entry(name.clone()).or_insert(0) += 1;
            }
        }
        Self {
            definitions,
            mentions,
        }
    }

    fn contains(&self, name: &str) -> bool {
        self.definitions.get(name).copied().unwrap_or(0) <= MAX_DEFINITIONS
            && self.mentions.get(name).copied().unwrap_or(0) <= MAX_MENTIONS
    }
}

fn caller(call: &Call) -> Affected {
    let location = match &call.function {
        Some(function) => format!("{}::{function}", call.file),
        None => call.file.clone(),
    };
    Affected::Caller {
        repository: call.repository.clone(),
        location,
        operation: call.operation.clone(),
    }
}

/// A call's function as the next place to look for callers: its own name
/// (a method's last segment).
fn call_seed(call: &Call) -> Seed {
    let names = call
        .function
        .as_deref()
        .map(|function| function.rsplit('.').next().unwrap_or(function).to_owned())
        .into_iter()
        .collect();
    Seed {
        repository: call.repository.clone(),
        names,
    }
}

/// The identifiers in some source text.
pub(crate) fn identifiers(text: &str) -> BTreeSet<String> {
    let mut found = BTreeSet::new();
    let mut current = String::new();
    for character in text.chars() {
        if character.is_alphanumeric() || character == '_' {
            current.push(character);
            continue;
        }
        take_identifier(&mut current, &mut found);
    }
    take_identifier(&mut current, &mut found);
    found
}

fn take_identifier(current: &mut String, found: &mut BTreeSet<String>) {
    let word = std::mem::take(current);
    if word.chars().next().is_some_and(|first| !first.is_numeric()) {
        found.insert(word);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn function(file: &str, name: &str, lines: (u32, u32), body: &str) -> Function {
        Function {
            file: file.to_owned(),
            name: name.to_owned(),
            qualified_name: name.to_owned(),
            kind: "function".to_owned(),
            lines,
            mentions: identifiers(body),
            binds: Vec::new(),
        }
    }

    fn route(repository: &str, operation: &str, file: &str, line: u32, text: &str) -> Route {
        Route {
            repository: repository.to_owned(),
            operation: operation.to_owned(),
            service: repository.to_owned(),
            file: file.to_owned(),
            line,
            handler_names: crate::handlers::handler_names(text),
            template: None,
            source: Arc::from(""),
            inline_mentions: None,
            passed_to_helper: false,
        }
    }

    /// Two repositories: `tools` runs the failing call from one handler,
    /// `gateway` calls that handler's operation from its own.
    fn fixture_functions(repository: &str) -> Vec<Function> {
        match repository {
            "tools" => vec![
                function(
                    "lib/run.go",
                    "Run",
                    (1, 9),
                    "functions.Charge.InvokeWithEnv(ctx)",
                ),
                function(
                    "handlers/execute.go",
                    "Execute",
                    (1, 9),
                    "lib.Run(ctx, req)",
                ),
                function("handlers/list.go", "List", (1, 9), "lib.Search(ctx)"),
                function(
                    "handlers/routes.go",
                    "Register",
                    (1, 9),
                    "r.POST(h.Execute) r.GET(h.List)",
                ),
            ],
            "gateway" => vec![function(
                "api/execute.ts",
                "handler",
                (1, 20),
                "tools.execute(body)",
            )],
            _ => Vec::new(),
        }
    }

    fn fixture_routes() -> Vec<Route> {
        vec![
            route(
                "tools",
                "POST /execute",
                "handlers/routes.go",
                2,
                "r.POST(\"/execute\", h.Execute)",
            ),
            route(
                "tools",
                "GET /list",
                "handlers/routes.go",
                3,
                "r.GET(\"/list\", h.List)",
            ),
            route(
                "gateway",
                "POST /v1/execute",
                "api/execute.ts",
                5,
                "case \"POST\":",
            ),
        ]
    }

    fn fixture_calls() -> Vec<Call> {
        vec![
            Call {
                repository: "gateway".to_owned(),
                file: "api/execute.ts".to_owned(),
                function: Some("handler".to_owned()),
                operation: "POST /execute".to_owned(),
            },
            Call {
                repository: "web".to_owned(),
                file: "src/run.ts".to_owned(),
                function: Some("Runner.run".to_owned()),
                operation: "POST /v1/execute".to_owned(),
            },
        ]
    }

    #[test]
    fn a_break_reaches_operations_and_callers_across_repositories() {
        let mut functions = fixture_functions;
        let routes = fixture_routes();
        let calls = fixture_calls();
        let seed = Seed {
            repository: "tools".to_owned(),
            names: vec!["Charge".to_owned()],
        };

        let affected = trace(&seed, &mut functions, &routes, &calls);

        let operations: Vec<&str> = affected
            .iter()
            .filter_map(|(_, item)| match item {
                Affected::Operation { operation, .. } => Some(operation.as_str()),
                Affected::Caller { .. } => None,
            })
            .collect();
        // Nearest first; `GET /list` is not reached.
        assert_eq!(operations, ["POST /execute", "POST /v1/execute"]);
        assert!(affected.contains(&(
            2,
            Affected::Caller {
                repository: "web".to_owned(),
                location: "src/run.ts::Runner.run".to_owned(),
                operation: "POST /v1/execute".to_owned(),
            }
        )));
    }

    #[test]
    fn a_trace_from_reached_functions_links_handlers_operations_and_callers() {
        let mut functions = fixture_functions;
        let start = Start::Reached {
            repository: "tools".to_owned(),
            functions: vec![("handlers/execute.go".to_owned(), 1)],
        };

        let mut tracing = Tracing::default();
        let (_, links) = trace_links(
            &mut tracing,
            &start,
            &mut functions,
            &fixture_routes(),
            &fixture_calls(),
        );

        let execute = Node::Affected(Affected::Operation {
            service: "tools".to_owned(),
            operation: "POST /execute".to_owned(),
        });
        assert!(links.contains(&Link {
            user: execute.clone(),
            used: Node::Function {
                file: "handlers/execute.go".to_owned(),
                line: 1,
                name: "Execute".to_owned(),
                kind: "function".to_owned(),
            },
            kind: LinkKind::Route,
        }));
        let gateway_handler = Node::Affected(Affected::Caller {
            repository: "gateway".to_owned(),
            location: "api/execute.ts::handler".to_owned(),
            operation: "POST /execute".to_owned(),
        });
        assert!(links.contains(&Link {
            user: gateway_handler.clone(),
            used: execute,
            kind: LinkKind::Call,
        }));
        assert!(links.contains(&Link {
            user: Node::Affected(Affected::Operation {
                service: "gateway".to_owned(),
                operation: "POST /v1/execute".to_owned(),
            }),
            used: gateway_handler,
            kind: LinkKind::Route,
        }));
    }

    #[test]
    fn naming_only_a_methods_type_is_marked_as_reaching_it_through_the_type() {
        let mut invoke = function("action.go", "Invoke", (1, 3), "");
        invoke.qualified_name = "ExecuteAction.Invoke".to_owned();
        let index = vec![
            invoke,
            function(
                "tools.go",
                "ExecuteTool",
                (1, 5),
                "ExecuteAction.InvokeWithRuntimeEnv()",
            ),
            function("jobs.go", "RunJob", (1, 5), "action.Invoke()"),
        ];
        let names = NameIndex::of(&index);
        let seeds = vec![
            ("Invoke".to_owned(), Some(0)),
            ("ExecuteAction".to_owned(), Some(0)),
        ];

        let walked = Walked::from_names(&index, &names, seeds, vec![0]);

        assert_eq!(walked.through_type, BTreeSet::from([1]));
        assert!(walked.is_reached[2]);
    }

    #[test]
    fn a_walk_stops_at_a_route_registration() {
        let index = vec![
            function("tools.ts", "executeTool", (1, 3), "run()"),
            function("tools.ts", "routes", (5, 9), "[{ handler: executeTool }]"),
            function("openapi.ts", "generateDoc", (1, 4), "describe(routes)"),
        ];
        let route = route("api", "POST /tools", "tools.ts", 6, "handler: executeTool");
        let mut names = NameIndex::of(&index);
        names.registrations = registrations(&index, &[(route, Handler::Definition(0))]);
        let step = Step {
            start: Start::Names(Seed {
                repository: String::new(),
                names: vec!["executeTool".to_owned()],
            }),
            hops: 0,
            via: None,
        };

        let mut reached: Vec<&str> = step
            .walk(&index, &names)
            .order
            .into_iter()
            .map(|position| index[position].name.as_str())
            .collect();

        reached.sort_unstable();
        assert_eq!(reached, ["executeTool", "routes"]);
    }

    #[test]
    fn names_many_functions_share_are_not_followed() {
        let index = vec![
            function("a.go", "Invoke", (1, 2), "Charge"),
            function("b.go", "Invoke", (1, 2), ""),
            function("c.go", "Invoke", (1, 2), ""),
            function("d.go", "Invoke", (1, 2), ""),
            function("e.go", "Handler", (1, 2), "x.Invoke()"),
        ];

        let reached = reach(&index, &["Charge".to_owned()]);

        let names: Vec<&str> = reached
            .iter()
            .map(|function| function.file.as_str())
            .collect();
        assert_eq!(names, ["a.go"]);
    }

    #[test]
    fn a_method_is_reached_through_its_type() {
        let mut handle = function("h.go", "handle", (1, 5), "lib.Run()");
        handle.qualified_name = "ExecuteHandler.handle".to_owned();
        let index = vec![
            handle,
            function("h.go", "NewExecuteHandler", (6, 9), "&ExecuteHandler{}"),
        ];

        let reached = reach(&index, &["Run".to_owned()]);

        let names: Vec<&str> = reached
            .iter()
            .map(|function| function.name.as_str())
            .collect();
        assert_eq!(names, ["handle", "NewExecuteHandler"]);
    }

    #[test]
    fn sibling_methods_are_not_reached_through_their_type() {
        let mut run = function("a.ts", "run", (1, 3), "client.execute()");
        run.qualified_name = "Action.run".to_owned();
        let mut rename = function("a.ts", "rename", (4, 6), "this.store as Action");
        rename.qualified_name = "Action.rename".to_owned();
        let index = vec![run, rename];

        let reached = reach(&index, &["execute".to_owned()]);

        let names: Vec<&str> = reached
            .iter()
            .map(|function| function.name.as_str())
            .collect();
        assert_eq!(names, ["run"]);
    }

    #[test]
    fn identifiers_skip_numbers_and_punctuation() {
        assert_eq!(
            identifiers("x.Run(2, \"y\") + _z9"),
            BTreeSet::from([
                "Run".to_owned(),
                "_z9".to_owned(),
                "x".to_owned(),
                "y".to_owned()
            ])
        );
    }

    #[test]
    fn a_name_too_many_functions_mention_is_not_followed() {
        let mut index = vec![
            function("lib/refresh.ts", "refresh", (1, 2), "charge()"),
            function("lib/errors.ts", "toHttpError", (1, 2), "refresh()"),
            function("routes/one.ts", "one", (1, 2), "toHttpError()"),
        ];
        for number in 0..=MAX_MENTIONS {
            let name = format!("other{number}");
            index.push(function(
                &format!("routes/{name}.ts"),
                &name,
                (1, 2),
                "toHttpError()",
            ));
        }

        let reached = reach(&index, &["charge".to_owned()]);

        let names: Vec<&str> = reached
            .iter()
            .map(|function| function.name.as_str())
            .collect();
        assert_eq!(names, ["refresh", "toHttpError"]);
    }
}
