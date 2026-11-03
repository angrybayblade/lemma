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

/// A function or method in a repository, with the names its body mentions.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Function {
    /// The file it is in, relative to the repository root.
    pub file: String,
    /// Its name, such as `execute`.
    pub name: String,
    /// Its name prefixed by its containers, such as `Tools.execute`.
    pub qualified_name: String,
    /// Its first and last lines, counting from 1.
    pub lines: (u32, u32),
    /// Every identifier its body mentions.
    pub mentions: BTreeSet<String>,
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
    /// The identifiers on that line.
    pub line_mentions: BTreeSet<String>,
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
const MAX_FUNCTION_HOPS: usize = 8;
/// Names defined by more functions than this in a repository are not
/// followed.
const MAX_DEFINITIONS: usize = 3;

/// Everything reachable from `seed`: affected operations and callers, each
/// with how many operations away from the break it is, nearest first.
/// `functions` gives a repository's functions, read on demand.
pub fn trace(
    seed: &Seed,
    functions: &mut dyn FnMut(&str) -> Vec<Function>,
    routes: &[Route],
    calls: &[Call],
) -> Vec<(usize, Affected)> {
    let mut indexes: BTreeMap<String, Vec<Function>> = BTreeMap::new();
    // Traced breadth first, so the first distance recorded is the shortest.
    let mut affected: BTreeMap<Affected, usize> = BTreeMap::new();
    let mut seen_operations = BTreeSet::new();
    let mut pending = VecDeque::from([(seed.clone(), 0usize)]);
    while let Some((seed, hops)) = pending.pop_front() {
        let index = indexes
            .entry(seed.repository.clone())
            .or_insert_with(|| functions(&seed.repository));
        let reached = reach(index, &seed.names);
        let repository_routes = routes
            .iter()
            .filter(|route| route.repository == seed.repository);
        for route in repository_routes.filter(|route| is_reached(route, index, &reached)) {
            let operation = Affected::Operation {
                service: route.service.clone(),
                operation: route.operation.clone(),
            };
            affected.entry(operation).or_insert(hops);
            if hops >= MAX_OPERATION_HOPS || !seen_operations.insert(route.operation.clone()) {
                continue;
            }
            for call in calls
                .iter()
                .filter(|call| call.operation == route.operation)
            {
                affected.entry(caller(call)).or_insert(hops + 1);
                pending.push_back((call_seed(call), hops + 1));
            }
        }
    }
    let mut nearest: Vec<(usize, Affected)> = affected
        .into_iter()
        .map(|(item, distance)| (distance, item))
        .collect();
    nearest.sort();
    nearest
}

/// The functions reached from `names` by following mentions upwards.
fn reach<'a>(index: &'a [Function], names: &[String]) -> Vec<&'a Function> {
    let definitions = definition_counts(index);
    let mut reached: Vec<&Function> = Vec::new();
    let mut frontier: BTreeSet<&str> = names.iter().map(String::as_str).collect();
    let mut followed: BTreeSet<&str> = BTreeSet::new();
    for _ in 0..MAX_FUNCTION_HOPS {
        let next: Vec<&Function> = index
            .iter()
            .filter(|function| !reached.contains(function))
            .filter(|function| {
                // A type's own methods are not its users: a sibling is
                // reached only by its own name being called.
                let own = callable_names(function);
                function
                    .mentions
                    .iter()
                    .any(|name| frontier.contains(name.as_str()) && !own.contains(&name.as_str()))
            })
            .collect();
        if next.is_empty() {
            break;
        }
        followed.extend(frontier.iter().copied());
        frontier = next
            .iter()
            .flat_map(|function| callable_names(function))
            .filter(|name| definitions.get(name).copied().unwrap_or(0) <= MAX_DEFINITIONS)
            .filter(|name| !followed.contains(name))
            .collect();
        reached.extend(next);
    }
    // Functions named by the seed are reached themselves.
    for function in index {
        if names.contains(&function.name) && !reached.contains(&function) {
            reached.push(function);
        }
    }
    reached
}

/// The names a function is reached by: its own, and for a method its
/// type's, since callers often hold the type (a constructor, a handler
/// object) rather than name the method.
fn callable_names(function: &Function) -> Vec<&str> {
    if function.name.is_empty() {
        return Vec::new();
    }
    let mut names = vec![function.name.as_str()];
    if let Some((container, _)) = function.qualified_name.rsplit_once('.') {
        names.push(container.rsplit('.').next().unwrap_or(container));
    }
    names
}

fn definition_counts(index: &[Function]) -> BTreeMap<&str, usize> {
    let mut counts = BTreeMap::new();
    for function in index {
        *counts.entry(function.name.as_str()).or_insert(0) += 1;
    }
    counts
}

/// Whether a route's handler is reached. A registration (`r.POST("/x",
/// h.execute)`) is reached when its line names a reached function; a route
/// declared inside its handler (a file-based route) when the function
/// around its line is.
fn is_reached(route: &Route, index: &[Function], reached: &[&Function]) -> bool {
    let enclosing = index.iter().find(|function| {
        function.file == route.file
            && function.lines.0 <= route.line
            && route.line <= function.lines.1
    });
    let defined: BTreeSet<&str> = index
        .iter()
        .map(|function| function.name.as_str())
        .collect();
    let named_handlers: Vec<&String> = route
        .line_mentions
        .iter()
        .filter(|name| defined.contains(name.as_str()))
        .filter(|name| enclosing.is_none_or(|function| &function.name != *name))
        .collect();
    if !named_handlers.is_empty() {
        return reached
            .iter()
            .any(|function| named_handlers.contains(&&function.name));
    }
    match enclosing {
        Some(function) => reached.contains(&function),
        None => reached.iter().any(|function| function.file == route.file),
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
pub fn identifiers(text: &str) -> BTreeSet<String> {
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
            lines,
            mentions: identifiers(body),
        }
    }

    fn route(repository: &str, operation: &str, file: &str, line: u32, text: &str) -> Route {
        Route {
            repository: repository.to_owned(),
            operation: operation.to_owned(),
            service: repository.to_owned(),
            file: file.to_owned(),
            line,
            line_mentions: identifiers(text),
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
}
