//! Finding the function that handles a documented operation's route, from
//! the code around where its method and path are written.
//!
//! Routes are written in two broad ways. A registration names its handler
//! beside the path (`r.Handle("/x", NewHandler(db))`, `r.POST("/x",
//! h.Execute)`): the handler is the first call after the path, or the first
//! name, followed through a local variable to what built it. A route
//! definition describes the route in a value (`const X = r.createRoute({
//! method, path })`) that a registration elsewhere in the file passes with
//! its handler (`r.openapi(X, async (c) => ...)`): the handler is the
//! function passed, if it is one, else the definition holding that call, else
//! the file's top-level code.

use std::sync::LazyLock;

use regex::Regex;

// Patterns that do not depend on the route are compiled once: compiling
// one took longer than matching it, and a large service has hundreds of
// routes. A pattern that failed to compile only means its form of route
// is not recognized.

/// A name passed first in a call, up to the next argument or the call's end.
static PASSED_NAME: LazyLock<Option<Regex>> = LazyLock::new(|| Regex::new(r"^(\w+)\s*[,)\n]").ok());

/// A destructuring statement (`const { def: X, handler: Y } = ...`).
static DESTRUCTURING: LazyLock<Option<Regex>> =
    LazyLock::new(|| Regex::new(r"(?:const|let|var)\s*\{([^}]*)\}\s*=").ok());

/// An assignment from a call: `name := pkg.Build(` or `name = Build(`.
static ASSIGNED_FROM_CALL: LazyLock<Option<Regex>> =
    LazyLock::new(|| Regex::new(r"\b(\w+)\s*:?=\s*&?((?:\w+\.)*(\w+))\s*\(").ok());

/// A call whose first argument is a name: `register(X, `.
static CALL_PASSING_NAME: LazyLock<Option<Regex>> =
    LazyLock::new(|| Regex::new(r"[\w.]+\s*\(\s*(\w+)\s*,\s*").ok());

/// The name a call's argument list opens after, type arguments allowed.
static CALLEE: LazyLock<Option<Regex>> =
    LazyLock::new(|| Regex::new(r"(\w+)\s*(?:<[^>]*>)?\s*$").ok());

mod declared;

pub use declared::{Conventions, resolve_declared};

/// A definition in the index.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Definition {
    /// Its file, relative to the repository root.
    pub path: String,
    /// Its qualified name.
    pub qualified: String,
    /// What it is: `function`, `method`, `constant`, `struct` and so on.
    pub kind: String,
}

/// What finding a handler asks of the index.
pub struct Lookup<'a> {
    /// The definition a simple name means, in the language of the given
    /// file and outside tests, if exactly one definition has that name.
    pub unique: &'a dyn Fn(&str, &str) -> Option<Definition>,
    /// The innermost definition around a line (counting from 1) of a file.
    pub enclosing: &'a dyn Fn(&str, u32) -> Option<Definition>,
}

/// Where a route's method and path are written.
pub struct RouteSite<'a> {
    /// The file.
    pub file: &'a str,
    /// Its text.
    pub text: &'a str,
    /// The line the path is on, counting from 1.
    pub line: u32,
    /// The path as written.
    pub template: &'a str,
}

/// The route's handler as its file and qualified name; `(file, None)` for
/// the file's top-level code, and `(None, None)` when nothing was found.
pub fn resolve(site: &RouteSite<'_>, lookup: &Lookup<'_>) -> (Option<String>, Option<String>) {
    if let Some(found) = registered_beside(site, lookup) {
        return (Some(found.path), Some(found.qualified));
    }
    if let Some(found) = registered_elsewhere(site, lookup) {
        return found;
    }
    let enclosing = (lookup.enclosing)(site.file, site.line);
    (
        Some(site.file.to_owned()),
        enclosing.map(|found| found.qualified),
    )
}

/// The names a route's line gives its handler, most likely first: the calls
/// after its path literal that are not chained onto another
/// (`NewHandler(db)`, not `).Methods(`), then the bare names after it. Every
/// name on the line if it writes no path literal.
pub fn handler_names(line: &str) -> Vec<String> {
    let masked = mask_strings(line);
    let mut after = 0;
    for quote in ['"', '\'', '`'] {
        let mut rest = 0;
        while let Some(open) = line[rest..].find(quote).map(|at| rest + at) {
            let Some(close) = line[open + 1..].find(quote).map(|at| open + 1 + at) else {
                break;
            };
            let literal = &line[open + 1..close];
            if literal.is_empty() || literal.starts_with('/') {
                after = after.max(close + 1);
            }
            rest = close + 1;
        }
    }
    let found = words(&masked[after..]);
    let calls = found.iter().filter(|word| word.is_call && !word.is_chained);
    let names = found.iter().filter(|word| !word.is_call);
    let mut ordered: Vec<String> = Vec::new();
    for word in calls.chain(names) {
        if !ordered.contains(&word.name) {
            ordered.push(word.name.clone());
        }
    }
    ordered
}

/// The handler a registration call passes, for a call seen registering a
/// route at `site.line` (as an adapter reports it): its last argument that
/// names a function, method or type, as `openapi(route, getUser)` or
/// `get(path, auth, handler)` do, read until the call's brackets close.
/// `None` when that argument is an inline function, whose definition is
/// the code around the call.
pub fn registered_call(site: &RouteSite<'_>, lookup: &Lookup<'_>) -> Option<Definition> {
    let masked = mask_strings(site.text);
    let start: usize = masked
        .split_inclusive('\n')
        .take(site.line.checked_sub(1)? as usize)
        .map(str::len)
        .sum();
    let rest = masked.get(start..)?;
    let line_end = rest.find('\n').unwrap_or(rest.len());
    let open = rest[..line_end].find('(')?;
    let arguments = &rest[open + 1..open + closing_bracket(&rest[open..])?];
    for argument in top_level_arguments(arguments).into_iter().rev() {
        let argument = argument.trim();
        if argument.is_empty() {
            continue;
        }
        let is_inline = argument.contains("=>")
            || argument.starts_with("async")
            || argument.starts_with("function");
        if is_inline {
            return None;
        }
        let named = words(argument)
            .iter()
            .rev()
            .find_map(|word| (lookup.unique)(&word.name, site.file));
        if named.is_some() {
            return named;
        }
    }
    None
}

/// The definition whose own line `site.line` is (`func (s *Server)
/// list(`, `def list(`, `function list(`), as an adapter reports a
/// handler's definition: the definition itself, not a call on its line.
pub fn defined_at(site: &RouteSite<'_>, lookup: &Lookup<'_>) -> Option<Definition> {
    let index = site.line.checked_sub(1)? as usize;
    let line = site.text.lines().nth(index)?.trim_start();
    let starts_definition = DEFINITION_KEYWORDS
        .iter()
        .any(|keyword| line.starts_with(keyword));
    if !starts_definition {
        return None;
    }
    (lookup.enclosing)(site.file, site.line)
}

/// How a line defining a function or method starts, in the languages
/// adapters read.
const DEFINITION_KEYWORDS: [&str; 10] = [
    "func ",
    "def ",
    "async def ",
    "function ",
    "async function ",
    "export function ",
    "export async function ",
    "export default function ",
    "fn ",
    "pub fn ",
];

/// The definition a decorator at `site.line` decorates (`@router.post("")`
/// above `def place_order`, `@Get()` above a method), passing any further
/// decorators, including ones whose arguments span lines; `None` when the
/// line is not a decorator.
pub fn decorated(site: &RouteSite<'_>, lookup: &Lookup<'_>) -> Option<Definition> {
    let lines: Vec<&str> = site.text.lines().collect();
    let mut index = site.line.checked_sub(1)? as usize;
    if !lines.get(index)?.trim_start().starts_with('@') {
        return None;
    }
    let mut depth = 0_i64;
    while let Some(line) = lines.get(index) {
        let masked = mask_strings(line);
        let is_decorator_line = depth > 0 || masked.trim_start().starts_with('@');
        if !is_decorator_line {
            let line_number = u32::try_from(index + 1).ok()?;
            return (lookup.enclosing)(site.file, line_number);
        }
        for character in masked.chars() {
            match character {
                '(' | '[' | '{' => depth += 1,
                ')' | ']' | '}' => depth -= 1,
                _ => {}
            }
        }
        index += 1;
    }
    None
}

/// Where the bracket opening `text` closes, as an offset into it.
fn closing_bracket(text: &str) -> Option<usize> {
    let mut depth = 0_usize;
    for (at, character) in text.char_indices() {
        match character {
            '(' | '[' | '{' => depth += 1,
            ')' | ']' | '}' => {
                depth = depth.checked_sub(1)?;
                if depth == 0 {
                    return Some(at);
                }
            }
            _ => {}
        }
    }
    None
}

/// A call's arguments, split at the commas outside any bracket.
fn top_level_arguments(arguments: &str) -> Vec<&str> {
    let mut found = Vec::new();
    let mut depth = 0_usize;
    let mut start = 0;
    for (at, character) in arguments.char_indices() {
        match character {
            '(' | '[' | '{' => depth += 1,
            ')' | ']' | '}' => depth = depth.saturating_sub(1),
            ',' if depth == 0 => {
                found.push(&arguments[start..at]);
                start = at + 1;
            }
            _ => {}
        }
    }
    found.push(&arguments[start..]);
    found
}

/// A handler named after the path on its line: the first call that is not
/// chained onto another (`).Methods(`), else the first name that is a
/// definition or a local variable built by one.
fn registered_beside(site: &RouteSite<'_>, lookup: &Lookup<'_>) -> Option<Definition> {
    let index = site.line.checked_sub(1)? as usize;
    let line = site.text.lines().nth(index)?;
    let mut after = after_literal(line, site.template)?.to_owned();
    // `r.Handle(\n  "/x",\n  NewHandler(db),\n)`: the handler is on the next line.
    if after.trim().trim_end_matches(',').is_empty() {
        let next = site
            .text
            .lines()
            .skip(index + 1)
            .find(|next| !next.trim().is_empty());
        after.push_str(next.unwrap_or_default());
    }
    let words = words(&mask_strings(&after));
    let calls = words.iter().filter(|word| word.is_call && !word.is_chained);
    for word in calls {
        if let Some(found) = (lookup.unique)(&word.name, site.file) {
            return Some(found);
        }
    }
    for word in words.iter().filter(|word| !word.is_call) {
        if let Some(found) = (lookup.unique)(&word.name, site.file) {
            return Some(found);
        }
        let built_by = assigned_from(site.text, site.line, &word.name)?;
        if let Some(found) = (lookup.unique)(&built_by, site.file) {
            return Some(found);
        }
    }
    None
}

/// What follows the path's literal on a line: the whole path, or, for a
/// route registered on a router mounted under a prefix, the longest string
/// literal the path ends with (`"/{slug}/execute"`, or `""` for the
/// prefix itself).
fn after_literal<'a>(line: &'a str, template: &str) -> Option<&'a str> {
    let mut best: Option<(usize, usize)> = None;
    for quote in ['"', '\'', '`'] {
        let mut rest = 0;
        while let Some(open) = line[rest..].find(quote).map(|at| rest + at) {
            let Some(close) = line[open + 1..].find(quote).map(|at| open + 1 + at) else {
                break;
            };
            let literal = &line[open + 1..close];
            let is_path = literal.is_empty() || literal.starts_with('/');
            if is_path
                && template.ends_with(literal)
                && best.is_none_or(|(length, _)| literal.len() > length)
            {
                best = Some((literal.len(), close + 1));
            }
            rest = close + 1;
        }
    }
    best.map(|(_, end)| &line[end..])
}

/// The function a local variable was last assigned from before `line`:
/// `name := pkg.Build(...)` or `name = Build(...)` gives `Build`.
fn assigned_from(text: &str, line: u32, name: &str) -> Option<String> {
    let before: String = text
        .lines()
        .take(line.saturating_sub(1) as usize)
        .collect::<Vec<_>>()
        .join("\n");
    // One pattern for every name, its matches filtered by name: no match
    // of another name can overlap one of `name`, since what follows `=` holds
    // no `=`. A name `\w` does not cover gets a pattern of its own.
    if !is_word(name) {
        let pattern = Regex::new(&format!(
            r"\b{}\s*:?=\s*&?((?:\w+\.)*(\w+))\s*\(",
            regex::escape(name)
        ))
        .ok()?;
        return last_callee(pattern.captures_iter(&before), 2);
    }
    let assigned = ASSIGNED_FROM_CALL
        .as_ref()?
        .captures_iter(&before)
        .filter(|captures| captures.get(1).is_some_and(|found| found.as_str() == name));
    last_callee(assigned, 3)
}

/// The callee in group `group` of the last of `matches`.
fn last_callee<'t>(
    matches: impl Iterator<Item = regex::Captures<'t>>,
    group: usize,
) -> Option<String> {
    matches
        .last()
        .and_then(|captures| captures.get(group))
        .map(|callee| callee.as_str().to_owned())
}

/// Whether `name` is all ASCII word characters, which `\w` matches.
fn is_word(name: &str) -> bool {
    !name.is_empty()
        && name
            .chars()
            .all(|character| character.is_ascii_alphanumeric() || character == '_')
}

/// A route described in a value that a registration in the same file passes
/// first, with its handler.
fn registered_elsewhere(
    site: &RouteSite<'_>,
    lookup: &Lookup<'_>,
) -> Option<(Option<String>, Option<String>)> {
    let named = PASSED_NAME.as_ref()?;
    for definition in definitions_at(site, lookup) {
        let Some(call) = passed_first(site.text, &definition) else {
            continue;
        };
        let passed = &site.text[call.end()..];
        if let Some(name) = named.captures(passed).and_then(|captures| captures.get(1)) {
            let function = (lookup.unique)(name.as_str(), site.file)
                .filter(|found| matches!(found.kind.as_str(), "function" | "method"));
            if let Some(found) = function {
                return Some((Some(found.path), Some(found.qualified)));
            }
        }
        let call_line = line_of(site.text, call.start());
        let holder = (lookup.enclosing)(site.file, call_line).map(|found| found.qualified);
        return Some((Some(site.file.to_owned()), holder));
    }
    None
}

/// The first call in `text` that passes `value` as its first argument,
/// through to the start of the next argument.
fn passed_first<'a>(text: &'a str, value: &str) -> Option<regex::Match<'a>> {
    // As in `assigned_from`: a call passing another name first cannot
    // overlap one passing `value`, so filtering one pattern's matches finds
    // the same first call.
    if !is_word(value) {
        let pattern =
            Regex::new(&format!(r"[\w.]+\s*\(\s*{}\s*,\s*", regex::escape(value))).ok()?;
        return pattern.find(text);
    }
    CALL_PASSING_NAME
        .as_ref()?
        .captures_iter(text)
        .find(|captures| captures.get(1).is_some_and(|found| found.as_str() == value))
        .and_then(|captures| captures.get(0))
}

/// The values a route's path may be written in: the definition around it,
/// if that is a value rather than a function, and names a destructuring
/// statement around it binds (`const { def: X, handler: Y } = ...`).
fn definitions_at(site: &RouteSite<'_>, lookup: &Lookup<'_>) -> Vec<String> {
    let mut names = Vec::new();
    if let Some(found) = (lookup.enclosing)(site.file, site.line) {
        if matches!(found.kind.as_str(), "constant" | "variable") {
            let name = found
                .qualified
                .rsplit('.')
                .next()
                .unwrap_or(&found.qualified);
            names.push(name.to_owned());
        }
    }
    let Some(destructuring) = DESTRUCTURING.as_ref() else {
        return names;
    };
    let lines: Vec<&str> = site.text.lines().collect();
    let first = (site.line as usize).saturating_sub(30);
    let statement = lines
        .iter()
        .take(site.line as usize)
        .skip(first)
        .rev()
        .find_map(|line| destructuring.captures(line));
    if let Some(captures) = statement {
        let bound = captures[1]
            .split(',')
            .map(|part| part.rsplit(':').next().unwrap_or(part).trim().to_owned())
            .filter(|name| !name.is_empty());
        names.extend(bound);
    }
    names
}

/// A function a documented path is passed to as a literal argument
/// (`registerExecute(router, '/v1/execute', version)`), which defines and
/// registers the route itself: one function by that name in the language,
/// whose file describes a route with the operation's method. A file that
/// only calls a method-named function (`.get(`) counts when nothing
/// describes one. `files` are the texts of the service's code, by path;
/// `templates` the ways the path may be written.
pub fn passed_to_helper(
    files: &std::collections::BTreeMap<String, String>,
    method: &str,
    templates: &[String],
    lookup: &Lookup<'_>,
) -> Option<Definition> {
    let helpers = helpers_passed(files, templates, lookup);
    let describes = |helper: &&Definition| {
        files
            .get(&helper.path)
            .is_some_and(|text| describes_method(text, method))
    };
    let calls = |helper: &&Definition| {
        files
            .get(&helper.path)
            .is_some_and(|text| calls_method(text, method))
    };
    helpers
        .iter()
        .find(describes)
        .or_else(|| helpers.iter().find(calls))
        .cloned()
}

/// Every function one of `templates` is passed to as a literal argument,
/// in file order, if one function in the language has that name.
fn helpers_passed(
    files: &std::collections::BTreeMap<String, String>,
    templates: &[String],
    lookup: &Lookup<'_>,
) -> Vec<Definition> {
    let mut found = Vec::new();
    for template in templates {
        for (path, text) in files {
            for at in literal_positions(text, template) {
                let helper = callee_before(&text[..at])
                    .and_then(|callee| (lookup.unique)(&callee, path))
                    .filter(|helper| helper.kind == "function");
                found.extend(helper);
            }
        }
    }
    found
}

/// The function a route's path literal is passed to on its line
/// (`registerExecute(router, '/v1/execute')`), if one function in the
/// language has that name: the handler of a route a helper registers.
pub fn helper_called_at(site: &RouteSite<'_>, lookup: &Lookup<'_>) -> Option<Definition> {
    let index = site.line.checked_sub(1)? as usize;
    let line_start: usize = site
        .text
        .split_inclusive('\n')
        .take(index)
        .map(str::len)
        .sum();
    let line = site.text[line_start..].lines().next()?;
    let at = literal_positions(line, site.template).into_iter().next()?;
    let callee = callee_before(&site.text[..line_start + at])?;
    (lookup.unique)(&callee, site.file)
        .filter(|found| matches!(found.kind.as_str(), "function" | "method"))
}

/// Where `template` is written as a quoted literal in `text`.
fn literal_positions(text: &str, template: &str) -> Vec<usize> {
    let mut positions = Vec::new();
    for quote in ['"', '\'', '`'] {
        let literal = format!("{quote}{template}{quote}");
        positions.extend(text.match_indices(&literal).map(|(at, _)| at));
    }
    positions.sort_unstable();
    positions
}

/// The function whose argument list the end of `before` is inside:
/// `register(router, ` gives `register`.
fn callee_before(before: &str) -> Option<String> {
    let opener = unclosed_paren(before)?;
    CALLEE
        .as_ref()?
        .captures(&before[..opener])
        .and_then(|captures| captures.get(1))
        .map(|name| name.as_str().to_owned())
}

/// The offset of the last `(` in `before` that is still open, if the end
/// of `before` is inside an argument list rather than a block or another
/// statement (`return path === '/x'` inside a function body is not an
/// argument).
fn unclosed_paren(before: &str) -> Option<usize> {
    let mut depth = 0usize;
    for (at, character) in before.char_indices().rev() {
        match character {
            ')' | ']' | '}' => depth += 1,
            '(' if depth == 0 => return Some(at),
            '{' | ';' if depth == 0 => return None,
            '(' | '[' | '{' => depth = depth.saturating_sub(1),
            _ => {}
        }
    }
    None
}

/// Whether code describes a route with an HTTP method: `method: 'post'`
/// or `Methods("POST")`.
fn describes_method(text: &str, method: &str) -> bool {
    let lower = text.to_lowercase();
    let method = method.to_lowercase();
    [
        format!("method: '{method}'"),
        format!("method: \"{method}\""),
        format!("methods(\"{method}\""),
    ]
    .iter()
    .any(|evidence| lower.contains(evidence))
}

/// Whether code calls a function named after an HTTP method, such as
/// `router.post(`.
fn calls_method(text: &str, method: &str) -> bool {
    text.to_lowercase()
        .contains(&format!(".{}(", method.to_lowercase()))
}

/// The line (counting from 1) of a byte offset.
fn line_of(text: &str, offset: usize) -> u32 {
    let newlines = text[..offset].matches('\n').count();
    u32::try_from(newlines + 1).unwrap_or(u32::MAX)
}

/// An identifier in code, whether it is called, and whether the call is
/// chained onto another call's result.
#[derive(Debug, Clone, PartialEq, Eq)]
struct Word {
    name: String,
    is_call: bool,
    is_chained: bool,
}

/// Keywords and literals that can sit beside a handler but never are one.
const NOT_NAMES: [&str; 12] = [
    "async", "await", "func", "function", "return", "new", "true", "false", "nil", "null", "this",
    "self",
];

fn words(text: &str) -> Vec<Word> {
    let mut found = Vec::new();
    let mut start = None;
    for (index, character) in text.char_indices().chain([(text.len(), ' ')]) {
        let is_part = character.is_alphanumeric() || character == '_';
        match (start, is_part) {
            (None, true) => start = Some(index),
            (Some(begin), false) => {
                start = None;
                let name = &text[begin..index];
                if name.chars().next().is_none_or(char::is_numeric) || NOT_NAMES.contains(&name) {
                    continue;
                }
                found.push(Word {
                    name: name.to_owned(),
                    is_call: text[index..].trim_start().starts_with('('),
                    is_chained: text[..begin].trim_end().ends_with(")."),
                });
            }
            _ => {}
        }
    }
    found
}

/// `text` with string literals and `//` comments blanked, so their words
/// are not names and their brackets do not count. Byte offsets stay the
/// same. A literal ends at the end of its line, so one stray quote (an
/// apostrophe in a comment) cannot blank the rest of the file.
fn mask_strings(text: &str) -> String {
    let mut masked = String::with_capacity(text.len());
    for line in text.split_inclusive('\n') {
        mask_line(line, &mut masked);
    }
    masked
}

/// Appends `line` to `masked` with its literals and comment blanked.
fn mask_line(line: &str, masked: &mut String) {
    let blank = |character: char, masked: &mut String| {
        masked.extend(std::iter::repeat_n(' ', character.len_utf8()));
    };
    let (body, newline) = line
        .strip_suffix('\n')
        .map_or((line, ""), |body| (body, "\n"));
    let mut quote = None;
    let mut is_escaped = false;
    let mut characters = body.char_indices();
    while let Some((at, character)) = characters.next() {
        match quote {
            Some(_) if is_escaped => {
                is_escaped = false;
                blank(character, masked);
            }
            Some(_) if character == '\\' => {
                is_escaped = true;
                masked.push(' ');
            }
            Some(open) if character == open => {
                quote = None;
                masked.push(' ');
            }
            Some(_) => blank(character, masked),
            None if body[at..].starts_with("//") => {
                blank(character, masked);
                for (_, rest) in characters.by_ref() {
                    blank(rest, masked);
                }
            }
            None if matches!(character, '"' | '\'' | '`') => {
                quote = Some(character);
                masked.push(' ');
            }
            None => masked.push(character),
        }
    }
    masked.push_str(newline);
}

#[cfg(test)]
mod tests {
    use super::*;

    fn definition(path: &str, qualified: &str, kind: &str) -> Definition {
        Definition {
            path: path.to_owned(),
            qualified: qualified.to_owned(),
            kind: kind.to_owned(),
        }
    }

    #[test]
    fn a_variable_is_followed_to_its_last_assignment_before_the_route() {
        let text = "h := handlers.Old(db)\nother = Build(x)\nh = pkg.New(db)\nr.Handle(\"/x\", h)\nh = Later(db)\n";

        let built_by = assigned_from(text, 4, "h");

        assert_eq!(built_by.as_deref(), Some("New"));
    }

    #[test]
    fn a_name_is_not_matched_inside_a_longer_name() {
        let text = "handler := Build(db)\nr.Handle(\"/x\", h)\n";

        let built_by = assigned_from(text, 2, "h");

        assert_eq!(built_by, None);
    }

    #[test]
    fn a_name_beyond_ascii_is_followed_too() {
        let text = "größe := sizes.Build(db)\nr.Handle(\"/x\", größe)\n";

        let built_by = assigned_from(text, 2, "größe");

        assert_eq!(built_by.as_deref(), Some("Build"));
    }

    #[test]
    fn the_first_call_passing_a_value_first_is_found_past_others() {
        let text =
            "app.use(cors, {});\nconst x = wrap(Other, 1);\nrouter.openapi(Route, handler);\n";

        let call = passed_first(text, "Route").map(|found| found.as_str());

        assert_eq!(call, Some("router.openapi(Route, "));
    }

    #[test]
    fn a_registration_names_its_handler_after_the_path() {
        let text = "func Setup(r *mux.Router) {\n\th := handlers.CreateInsertHandler(db)\n\tr.Handle(\"/products\", products.NewUpsertHandler(db, svc)).Methods(\"POST\")\n\tr.Handle(\"/insert\", h).Methods(\"POST\")\n}\n";
        let unique = |name: &str, _: &str| match name {
            "NewUpsertHandler" => Some(definition(
                "products/upsert.go",
                "NewUpsertHandler",
                "function",
            )),
            "CreateInsertHandler" => Some(definition(
                "handlers/insert.go",
                "CreateInsertHandler",
                "function",
            )),
            "svc" | "Methods" | "Handle" => Some(definition("x.go", name, "struct")),
            _ => None,
        };
        let enclosing = |_: &str, _: u32| Some(definition("router.go", "Setup", "function"));
        let lookup = Lookup {
            unique: &unique,
            enclosing: &enclosing,
        };
        let site = |line, template| RouteSite {
            file: "router.go",
            text,
            line,
            template,
        };

        let upsert = resolve(&site(3, "/products"), &lookup);
        let insert = resolve(&site(4, "/insert"), &lookup);

        assert_eq!(upsert.1.as_deref(), Some("NewUpsertHandler"));
        assert_eq!(insert.1.as_deref(), Some("CreateInsertHandler"));
    }

    #[test]
    fn a_route_on_a_mounted_router_matches_the_end_of_its_path() {
        let line = r#"	sub.Handle("/{slug}/execute", NewExecuteHandler(db)).Methods("POST")"#;
        let root = r#"	sub.Handle("", NewCreateHandler(db)).Methods("POST")"#;

        assert_eq!(
            after_literal(line, "/api/tasks_/{slug}/execute"),
            Some(", NewExecuteHandler(db)).Methods(\"POST\")")
        );
        assert_eq!(
            after_literal(root, "/api/polling_monitors"),
            Some(", NewCreateHandler(db)).Methods(\"POST\")")
        );
    }

    #[test]
    fn a_route_definition_leads_to_the_registration_that_passes_it() {
        let text = "const GET_DEF = router.createRoute({\n  method: 'get',\n  path: '/v1/items',\n})\n\nexport const getItems = router.openapi(\n  GET_DEF,\n  async (c) => {\n    return c.json([])\n  },\n)\n\nexport const routes = [GET_DEF]\n";
        let unique = |_: &str, _: &str| None;
        let enclosing = |_: &str, line: u32| match line {
            1..=4 => Some(definition("items.ts", "GET_DEF", "constant")),
            6..=11 => Some(definition("items.ts", "getItems", "constant")),
            _ => None,
        };
        let lookup = Lookup {
            unique: &unique,
            enclosing: &enclosing,
        };
        let site = RouteSite {
            file: "items.ts",
            text,
            line: 3,
            template: "/v1/items",
        };

        let found = resolve(&site, &lookup);

        assert_eq!(
            found,
            (Some("items.ts".to_owned()), Some("getItems".to_owned()))
        );
    }

    #[test]
    fn a_top_level_registration_is_the_files_own_code() {
        let text = "const { def: RUN_DEF, handler: wrapper } = makeRoute({\n  method: 'post',\n  path: '/v1/run',\n})\n\nrouter.openapi(RUN_DEF, wrapper)\n";
        let unique = |_: &str, _: &str| None;
        let enclosing = |_: &str, _: u32| None;
        let lookup = Lookup {
            unique: &unique,
            enclosing: &enclosing,
        };
        let site = RouteSite {
            file: "run.ts",
            text,
            line: 3,
            template: "/v1/run",
        };

        assert_eq!(resolve(&site, &lookup), (Some("run.ts".to_owned()), None));
    }

    #[test]
    fn a_path_passed_to_a_helper_finds_the_helper_not_a_comparison() {
        let files = std::collections::BTreeMap::from([
            (
                "docs.ts".to_owned(),
                "function isToolRoute(route) {\n  return route.path === '/v1/tools'\n}\nconst doc = { method: 'get' }\n".to_owned(),
            ),
            (
                "tools.ts".to_owned(),
                "export function registerTools(router, path) {\n  router.openapi(createRoute({ method: 'get', path }), list)\n}\nregisterTools(router, '/v1/tools')\n".to_owned(),
            ),
        ]);
        let unique = |name: &str, _: &str| match name {
            "isToolRoute" => Some(definition("docs.ts", "isToolRoute", "function")),
            "registerTools" => Some(definition("tools.ts", "registerTools", "function")),
            _ => None,
        };
        let enclosing = |_: &str, _: u32| None;
        let lookup = Lookup {
            unique: &unique,
            enclosing: &enclosing,
        };

        let found = passed_to_helper(&files, "get", &["/v1/tools".to_owned()], &lookup);

        assert_eq!(
            found.map(|helper| helper.qualified).as_deref(),
            Some("registerTools")
        );
    }

    #[test]
    fn masking_keeps_offsets_and_a_stray_quote_ends_at_its_line() {
        let text = "// don't\nr.Handle(\"/x\", h) // é\n";

        let masked = mask_strings(text);

        assert_eq!(masked.len(), text.len());
        assert_eq!(masked.lines().nth(1), Some("r.Handle(    , h)      "));
    }

    #[test]
    fn a_route_lines_handler_is_the_call_after_its_path() {
        let line =
            r#"	api.Handle("/{slug}/execute", NewExecuteHandler(db, functions)).Methods("POST")"#;

        let names = handler_names(line);

        assert_eq!(names.first().map(String::as_str), Some("NewExecuteHandler"));
        assert!(!names.contains(&"Handle".to_owned()));
        assert!(!names.contains(&"Methods".to_owned()));
    }

    #[test]
    fn a_handler_on_the_line_after_its_path_is_found() {
        let text = "func Routes(r *mux.Router) {\n\tr.Handle(\n\t\t\"/health/deep\",\n\t\thandlers.NewDeepHealth(db),\n\t).Methods(\"GET\")\n}\n";
        let unique = |name: &str, _: &str| {
            (name == "NewDeepHealth").then(|| definition("health.go", "NewDeepHealth", "function"))
        };
        let enclosing = |_: &str, _: u32| Some(definition("router.go", "Routes", "function"));
        let lookup = Lookup {
            unique: &unique,
            enclosing: &enclosing,
        };
        let site = RouteSite {
            file: "router.go",
            text,
            line: 3,
            template: "/health/deep",
        };

        let found = resolve(&site, &lookup);

        assert_eq!(found.1.as_deref(), Some("NewDeepHealth"));
    }
}
