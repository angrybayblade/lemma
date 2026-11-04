//! Finding a route's handler by the calls a service's `lemma.yaml` says it
//! writes routes with (`exports.routes`): a `register` call takes a route
//! and its handler (`r.Handle("/x", NewHandler(db))`, `router.openapi(DEF,
//! handler)`), a `define` call only describes a route (`createRoute({
//! method, path })`). When a directive decides, the result is not a guess
//! about the code's conventions.

use regex::Regex;

use super::{Definition, Lookup, RouteSite, after_literal, assigned_from, line_of, mask_strings};

/// The calls a service writes its routes with, by name.
#[derive(Debug, Clone, Copy, Default)]
pub struct Conventions<'a> {
    /// Calls that register a route with its handler, the handler being the
    /// argument after the route.
    pub register: &'a [String],
    /// Calls that describe a route without its handler; a `register` call
    /// later passes the value they build.
    pub define: &'a [String],
}

impl Conventions<'_> {
    /// Whether no calls are declared.
    pub fn is_empty(&self) -> bool {
        self.register.is_empty() && self.define.is_empty()
    }
}

/// A call the route's path is written inside: its name and where its
/// argument list opens.
#[derive(Debug, Clone, PartialEq, Eq)]
struct Call {
    name: String,
    opens_at: usize,
}

/// The route's handler as the declared calls show it, `(file, None)` for the
/// file's top-level code; `None` when no declared call holds the route.
pub fn resolve_declared(
    site: &RouteSite<'_>,
    conventions: Conventions<'_>,
    lookup: &Lookup<'_>,
) -> Option<(Option<String>, Option<String>)> {
    if conventions.is_empty() {
        return None;
    }
    let masked = mask_strings(site.text);
    let at = path_offset(site)?;
    let calls = calls_around(&masked, at);
    if let Some(register) = calls
        .iter()
        .find(|call| conventions.register.contains(&call.name))
    {
        return Some(handler_passed(site, &masked, register, at, lookup));
    }
    let define = calls
        .iter()
        .find(|call| conventions.define.contains(&call.name))?;
    let value = defined_value(&masked, define.opens_at)?;
    let (register, after_value) = registration_of(&masked, &value, conventions.register)?;
    Some(handler_passed(
        site,
        &masked,
        &register,
        after_value,
        lookup,
    ))
}

/// Where the route's path literal ends in the file's text.
fn path_offset(site: &RouteSite<'_>) -> Option<usize> {
    let index = site.line.checked_sub(1)? as usize;
    let line_start: usize = site
        .text
        .split_inclusive('\n')
        .take(index)
        .map(str::len)
        .sum();
    let line = site.text[line_start..].lines().next()?;
    let rest = after_literal(line, site.template)?;
    Some(line_start + line.len() - rest.len())
}

/// The calls whose argument lists hold offset `at`, innermost first,
/// through object literals but not out of a function body.
fn calls_around(masked: &str, at: usize) -> Vec<Call> {
    let mut calls = Vec::new();
    let mut depth = 0usize;
    for (offset, character) in masked[..at].char_indices().rev() {
        match character {
            ')' | ']' | '}' => depth += 1,
            '(' if depth == 0 => calls.extend(call_opening(masked, offset)),
            '{' if depth == 0 && opens_body(&masked[..offset]) => break,
            '(' | '[' | '{' => depth = depth.saturating_sub(1),
            _ => {}
        }
    }
    calls
}

/// The call whose argument list opens at `offset`, if a name precedes it.
fn call_opening(masked: &str, offset: usize) -> Option<Call> {
    let before = masked[..offset].trim_end();
    let start = before
        .rfind(|character: char| !(character.is_alphanumeric() || character == '_'))
        .map_or(0, |at| at + 1);
    let name = &before[start..];
    if name.is_empty() || name.starts_with(char::is_numeric) {
        return None;
    }
    Some(Call {
        name: name.to_owned(),
        opens_at: offset,
    })
}

/// Whether a `{` after `before` opens a function or block body rather than
/// an object literal: it follows `)`, `=>` or a keyword, not a value
/// position.
fn opens_body(before: &str) -> bool {
    let before = before.trim_end();
    before.ends_with(')') || before.ends_with("=>") || before.ends_with("else")
}

/// The handler a registration passes after the argument holding `at`: a
/// named function, the function a call builds it with, or else the
/// definition holding the registration, or the file's top-level code.
fn handler_passed(
    site: &RouteSite<'_>,
    masked: &str,
    register: &Call,
    at: usize,
    lookup: &Lookup<'_>,
) -> (Option<String>, Option<String>) {
    let line = line_of(site.text, register.opens_at);
    let passed = next_argument(masked, at).map(|range| site.text[range].trim());
    if let Some(found) = passed.and_then(|argument| named_handler(site, argument, line, lookup)) {
        return (Some(found.path), Some(found.qualified));
    }
    let holder = (lookup.enclosing)(site.file, line).map(|found| found.qualified);
    (Some(site.file.to_owned()), holder)
}

/// The byte range of the argument after the one holding offset `at`.
fn next_argument(masked: &str, at: usize) -> Option<std::ops::Range<usize>> {
    let start = argument_end(masked, at)?;
    if !masked[start..].starts_with(',') {
        return None;
    }
    let start = start + 1;
    let end = argument_end(masked, start)?;
    Some(start..end)
}

/// Where the argument holding offset `at` ends: the `,` or `)` that closes
/// it at its own depth.
fn argument_end(masked: &str, at: usize) -> Option<usize> {
    let mut depth = 0usize;
    for (offset, character) in masked[at..].char_indices() {
        match character {
            '(' | '[' | '{' => depth += 1,
            ',' | ')' if depth == 0 => return Some(at + offset),
            ')' | ']' | '}' => depth = depth.checked_sub(1)?,
            _ => {}
        }
    }
    None
}

/// The function an argument names (`listItems`, `h.Execute`) or calls to
/// build the handler (`NewHandler(db)`), or a local variable assigned from
/// such a call before `line`.
fn named_handler(
    site: &RouteSite<'_>,
    argument: &str,
    line: u32,
    lookup: &Lookup<'_>,
) -> Option<Definition> {
    let pattern = Regex::new(r"^&?(?:\w+\.)*(\w+)\s*(\()?").ok()?;
    let captures = pattern.captures(argument)?;
    let name = captures.get(1)?.as_str();
    let is_call = captures.get(2).is_some();
    let is_whole = captures.get(0)?.as_str().trim_end() == argument;
    if !is_call && !is_whole {
        return None;
    }
    let is_function = |found: &Definition| matches!(found.kind.as_str(), "function" | "method");
    if let Some(found) = (lookup.unique)(name, site.file).filter(is_function) {
        return Some(found);
    }
    if is_call {
        return None;
    }
    let built_by = assigned_from(site.text, line, name)?;
    (lookup.unique)(&built_by, site.file).filter(is_function)
}

/// The name a `define` call's value is bound to: `const X = define(...)`,
/// or the first name a destructuring binds (`const { def: X } = ...`). The
/// binding may sit on the line before the call.
fn defined_value(masked: &str, opens_at: usize) -> Option<String> {
    let line_start = masked[..opens_at].rfind('\n').map_or(0, |at| at + 1);
    let previous_start = masked[..line_start.saturating_sub(1)]
        .rfind('\n')
        .map_or(0, |at| at + 1);
    let assigned =
        Regex::new(r"(?:const|let|var)\s+(\w+)\s*(?::[^=]+)?=\s*[\w.]*\s*(?:<[^>]*>)?\s*$").ok()?;
    let destructured = Regex::new(r"(?:const|let|var)\s*\{\s*(?:\w+\s*:\s*)?(\w+)").ok()?;
    for start in [line_start, previous_start] {
        let statement = &masked[start..opens_at];
        let captures = assigned
            .captures(statement)
            .or_else(|| destructured.captures(statement));
        if let Some(name) = captures.and_then(|captures| captures.get(1)) {
            return Some(name.as_str().to_owned());
        }
    }
    None
}

/// The first `register` call passing `value` as its first argument, and
/// where that argument ends.
fn registration_of(masked: &str, value: &str, register: &[String]) -> Option<(Call, usize)> {
    if register.is_empty() {
        return None;
    }
    let names = register
        .iter()
        .map(|name| regex::escape(name))
        .collect::<Vec<_>>()
        .join("|");
    let pattern = Regex::new(&format!(r"\b({names})\s*\(\s*{}\b", regex::escape(value))).ok()?;
    let found = pattern.captures(masked)?;
    let name = found.get(1)?;
    let whole = found.get(0)?;
    let opens_at = masked[name.end()..whole.end()].find('(')? + name.end();
    let call = Call {
        name: name.as_str().to_owned(),
        opens_at,
    };
    Some((call, whole.end()))
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
    fn a_registration_passes_its_handler_after_the_path() {
        let text = "func Setup(r *mux.Router) {\n\th := handlers.CreateInsertHandler(db)\n\tr.Handle(\"/products\", products.NewUpsertHandler(db, svc)).Methods(\"POST\")\n\tr.Handle(\"/insert\", h).Methods(\"POST\")\n}\n";
        let unique = |name: &str, _: &str| match name {
            "NewUpsertHandler" | "CreateInsertHandler" => {
                Some(definition("handlers.go", name, "function"))
            }
            _ => None,
        };
        let enclosing = |_: &str, _: u32| Some(definition("router.go", "Setup", "function"));
        let lookup = Lookup {
            unique: &unique,
            enclosing: &enclosing,
        };
        let register = ["Handle".to_owned()];
        let conventions = Conventions {
            register: &register,
            define: &[],
        };
        let site = |line, template| RouteSite {
            file: "router.go",
            text,
            line,
            template,
        };

        let upsert = resolve_declared(&site(3, "/products"), conventions, &lookup);
        let insert = resolve_declared(&site(4, "/insert"), conventions, &lookup);

        assert_eq!(
            upsert.and_then(|found| found.1).as_deref(),
            Some("NewUpsertHandler")
        );
        assert_eq!(
            insert.and_then(|found| found.1).as_deref(),
            Some("CreateInsertHandler")
        );
    }

    #[test]
    fn a_defined_route_leads_to_the_registration_passing_it() {
        let text = "const GET_DEF = router.createRoute({\n  method: 'get',\n  path: '/v1/items',\n})\n\nexport const getItems = router.openapi(\n  GET_DEF,\n  async (c) => {\n    return c.json([])\n  },\n)\n";
        let unique = |_: &str, _: &str| None;
        let enclosing = |_: &str, line: u32| match line {
            6..=11 => Some(definition("items.ts", "getItems", "constant")),
            _ => None,
        };
        let lookup = Lookup {
            unique: &unique,
            enclosing: &enclosing,
        };
        let register = ["openapi".to_owned()];
        let define = ["createRoute".to_owned()];
        let conventions = Conventions {
            register: &register,
            define: &define,
        };
        let site = RouteSite {
            file: "items.ts",
            text,
            line: 3,
            template: "/v1/items",
        };

        let found = resolve_declared(&site, conventions, &lookup);

        assert_eq!(
            found,
            Some((Some("items.ts".to_owned()), Some("getItems".to_owned())))
        );
    }

    #[test]
    fn a_route_outside_declared_calls_is_left_to_reading_the_code() {
        let text = "app.get('/v1/health', health)\n";
        let unique = |_: &str, _: &str| None;
        let enclosing = |_: &str, _: u32| None;
        let lookup = Lookup {
            unique: &unique,
            enclosing: &enclosing,
        };
        let register = ["openapi".to_owned()];
        let conventions = Conventions {
            register: &register,
            define: &[],
        };
        let site = RouteSite {
            file: "app.ts",
            text,
            line: 1,
            template: "/v1/health",
        };

        assert_eq!(resolve_declared(&site, conventions, &lookup), None);
    }
}
