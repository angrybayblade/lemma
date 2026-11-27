//! Names code reads off what a call returned: `body.status` after
//! `const body = await res.json()` with `res` a `fetch(...)`, or
//! `resp.json()["items"]` in Python. A response field the code reads this
//! way is used for certain, where a name merely mentioned near the call
//! may belong to anything.
//!
//! What a call returns is often a response object before it is a body:
//! `res.status` and `res.data` are properties of `fetch`'s and axios's
//! responses, not fields of the body. So each name is read at a depth,
//! the steps between the call and the name (a property, a string key, or
//! a call such as `.json()`). A name two or more steps away is inside the
//! body whatever the call returned. A name one step away is a field only
//! if the call returned the body itself, as a generated client's method
//! or a wrapper does (`invoice.amount` after `client.invoices.retrieve`),
//! so it is kept apart, for a rule to take as a read of a top-level field
//! alone, and not at all when it is a name a response object or an
//! envelope around a body has too (`res.status`, `result.data`). Nothing
//! read through the response object's own parts is kept
//! (`res.headers.allow`, `r.history[0].status_code`).

use tree_sitter::Node;

use super::{Walk, is_identifier};

/// A name read off a call's result, and the lines the call spans, from 1,
/// so a rule can find it beside the call's facts.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, serde::Serialize, serde::Deserialize)]
pub struct ResultRead {
    /// The name read.
    pub name: String,
    /// The first line of the call.
    pub first_line: u32,
    /// The last line of the call.
    pub last_line: u32,
}

/// A variable holding a call's result, or a value read off one: the lines
/// of the call, the steps from it to the value, and whether a step went
/// into one of the response object's own parts rather than the body.
#[derive(Debug, Clone, Copy)]
pub(super) struct ResultValue {
    first_line: u32,
    last_line: u32,
    depth: u32,
    is_outside_body: bool,
}

/// The fewest steps off a call's result at which a name is taken for a
/// field of the body (see the module documentation).
const BODY_DEPTH: u32 = 2;

/// Parts of an HTTP response object other than its body, in the clients
/// code commonly uses (`fetch`, axios, requests, httpx): names read
/// through one of them, first step off a result, are not body fields.
const RESPONSE_PARTS: [&str; 6] = ["headers", "request", "history", "raw", "cookies", "config"];

/// Names a response object has in those clients, or an envelope a
/// generated client puts around a body (`{ data, error, response }`): one
/// step off a result, such a name may be the object's or the envelope's
/// own, so it is not taken for a field of the body.
const RESPONSE_NAMES: [&str; 26] = [
    "status",
    "statusText",
    "status_code",
    "ok",
    "data",
    "error",
    "errors",
    "response",
    "body",
    "bodyUsed",
    "json",
    "text",
    "content",
    "url",
    "type",
    "redirected",
    "reason",
    "encoding",
    "elapsed",
    "links",
    "next",
    "is_success",
    "is_error",
    "is_redirect",
    "http_version",
    "extensions",
];

impl Walk<'_> {
    /// `const x = await call(...)`, `x = call(...)`, `const { data } =
    /// await call(...)`, or a value read off such a variable: binds the
    /// variables to the result, and records the names a destructuring
    /// pattern reads.
    pub(super) fn bind_result(&mut self, node: Node<'_>) {
        let (pattern, value) = match node.kind() {
            "assignment" => (
                node.child_by_field_name("left"),
                node.child_by_field_name("right"),
            ),
            _ => (
                node.child_by_field_name("name"),
                node.child_by_field_name("value"),
            ),
        };
        let (Some(pattern), Some(value)) = (pattern, value) else {
            return;
        };
        let Some(result) = self.result_value(value) else {
            return;
        };
        let function = self.enclosing_function(node);
        match pattern.kind() {
            "identifier" => {
                let key = (function, self.text(pattern).to_owned());
                self.results.insert(key, result);
            }
            "object_pattern" => self.bind_pattern(pattern, result, function.as_ref()),
            _ => {}
        }
    }

    /// `{ data, items: list }`: each key is read one step past `result`,
    /// and each variable holds what its key read.
    fn bind_pattern(&mut self, pattern: Node<'_>, result: ResultValue, function: Option<&String>) {
        for child in Self::children(pattern) {
            let (key, variable) = match child.kind() {
                "shorthand_property_identifier_pattern" => (Some(child), Some(child)),
                "pair_pattern" => (
                    child.child_by_field_name("key"),
                    child
                        .child_by_field_name("value")
                        .filter(|value| value.kind() == "identifier"),
                ),
                _ => continue,
            };
            let Some(key) = key else {
                continue;
            };
            let name = self.text(key).to_owned();
            let read = step(result, Some(&name));
            self.record_result_read(name, read);
            if let Some(variable) = variable {
                let key = (function.cloned(), self.text(variable).to_owned());
                self.results.insert(key, read);
            }
        }
    }

    /// Records the name `node` reads, if it reads one off a call's result:
    /// a property, a string key, or the key of `.get("key")`.
    pub(super) fn read_result(&mut self, node: Node<'_>) {
        if is_called(node) {
            return; // `items.map(...)`: a method, not a field
        }
        let (name, base) = match node.kind() {
            "member_expression" => (
                node.child_by_field_name("property")
                    .map(|property| self.text(property).to_owned()),
                node.child_by_field_name("object"),
            ),
            "attribute" => (
                node.child_by_field_name("attribute")
                    .map(|attribute| self.text(attribute).to_owned()),
                node.child_by_field_name("object"),
            ),
            "subscript_expression" | "subscript" => (
                self.subscript_key(node),
                node.child_by_field_name("object")
                    .or_else(|| node.child_by_field_name("value")),
            ),
            "call_expression" | "call" => (self.get_key(node), Self::get_receiver(node)),
            _ => return,
        };
        let (Some(name), Some(base)) = (name, base) else {
            return;
        };
        let Some(result) = self.result_value(base) else {
            return;
        };
        let read = step(result, Some(&name));
        self.record_result_read(name, read);
    }

    fn record_result_read(&mut self, name: String, read: ResultValue) {
        if read.is_outside_body || !is_identifier(&name) {
            return;
        }
        let is_one_step = read.depth < BODY_DEPTH;
        if is_one_step && RESPONSE_NAMES.contains(&name.as_str()) {
            return;
        }
        let read = ResultRead {
            name,
            first_line: read.first_line,
            last_line: read.last_line,
        };
        if is_one_step {
            self.facts.direct_result_reads.push(read);
        } else {
            self.facts.result_reads.push(read);
        }
    }

    /// What `node` holds of a call's result: a fresh call's result, a
    /// variable bound to one, or a value read off either.
    fn result_value(&self, node: Node<'_>) -> Option<ResultValue> {
        match node.kind() {
            "await_expression"
            | "await"
            | "parenthesized_expression"
            | "non_null_expression"
            | "as_expression"
            | "satisfies_expression" => {
                let inner = Self::children(node).into_iter().next()?;
                self.result_value(inner)
            }
            "identifier" => {
                let key = (self.enclosing_function(node), self.text(node).to_owned());
                self.results.get(&key).copied()
            }
            "member_expression" | "attribute" => {
                let object = node.child_by_field_name("object")?;
                let property = node
                    .child_by_field_name("property")
                    .or_else(|| node.child_by_field_name("attribute"))
                    .map(|property| self.text(property));
                Some(step(self.result_value(object)?, property))
            }
            "subscript_expression" | "subscript" => {
                let object = node
                    .child_by_field_name("object")
                    .or_else(|| node.child_by_field_name("value"))?;
                let base = self.result_value(object)?;
                // `items[0]` stays where `items` was; `body["items"]` steps.
                Some(match self.subscript_key(node) {
                    Some(key) => step(base, Some(&key)),
                    None => base,
                })
            }
            "call_expression" | "call" => Some(self.call_result(node)),
            _ => None,
        }
    }

    /// The result of the call `node`: a step past what it was called on,
    /// as `res.json()` is past `res`, or a fresh result.
    fn call_result(&self, node: Node<'_>) -> ResultValue {
        let called_on = node
            .child_by_field_name("function")
            .and_then(|function| self.result_value(function));
        if let Some(base) = called_on {
            return step(base, None);
        }
        ResultValue {
            first_line: line_of(node.start_position().row),
            last_line: line_of(node.end_position().row),
            depth: 0,
            is_outside_body: false,
        }
    }

    /// The string key of `body["items"]`.
    fn subscript_key(&self, node: Node<'_>) -> Option<String> {
        let index = node
            .child_by_field_name("index")
            .or_else(|| node.child_by_field_name("subscript"))?;
        self.string_value(index)
    }

    /// The key of `body.get("items")`.
    fn get_key(&self, node: Node<'_>) -> Option<String> {
        let function = node.child_by_field_name("function")?;
        let method = function
            .child_by_field_name("property")
            .or_else(|| function.child_by_field_name("attribute"))?;
        if self.text(method) != "get" {
            return None;
        }
        let arguments = node.child_by_field_name("arguments")?;
        let first = Self::children(arguments).into_iter().next()?;
        self.string_value(first)
    }

    /// `body` in `body.get("items")`.
    fn get_receiver(node: Node<'_>) -> Option<Node<'_>> {
        node.child_by_field_name("function")?
            .child_by_field_name("object")
    }
}

/// Whether `node` is the function of the call around it, as `items.map`
/// is in `items.map(f)`.
fn is_called(node: Node<'_>) -> bool {
    node.parent().is_some_and(|parent| {
        matches!(parent.kind(), "call_expression" | "call")
            && parent
                .child_by_field_name("function")
                .is_some_and(|function| function.id() == node.id())
    })
}

/// `value` one step further, through `name` where the step reads one.
fn step(value: ResultValue, name: Option<&str>) -> ResultValue {
    let enters_part = value.depth == 0 && name.is_some_and(|name| RESPONSE_PARTS.contains(&name));
    ResultValue {
        depth: value.depth + 1,
        is_outside_body: value.is_outside_body || enters_part,
        ..value
    }
}

fn line_of(row: usize) -> u32 {
    u32::try_from(row + 1).unwrap_or(u32::MAX)
}

#[cfg(test)]
mod tests {
    use lemma_syntax::Extractor;

    use crate::code::{CodeOptions, extract};

    /// The names `source` reads two or more steps off call results, with
    /// the call's lines.
    fn reads(file: &str, source: &str) -> Vec<(String, u32, u32)> {
        let mut extractor = Extractor::new().unwrap();
        let facts = extract(file, source, &CodeOptions::default(), &mut extractor).unwrap();
        facts
            .result_reads
            .into_iter()
            .map(|read| (read.name, read.first_line, read.last_line))
            .collect()
    }

    /// The names `source` reads one step off call results.
    fn direct_reads(file: &str, source: &str) -> Vec<(String, u32, u32)> {
        let mut extractor = Extractor::new().unwrap();
        let facts = extract(file, source, &CodeOptions::default(), &mut extractor).unwrap();
        facts
            .direct_result_reads
            .into_iter()
            .map(|read| (read.name, read.first_line, read.last_line))
            .collect()
    }

    fn read(name: &str, first_line: u32, last_line: u32) -> (String, u32, u32) {
        (name.to_owned(), first_line, last_line)
    }

    #[test]
    fn fields_of_a_fetched_body_are_read_off_the_fetch() {
        let source = "export async function show(id: string) {\n  const res = await fetch(`/v1/invoices/${id}`)\n  const body = await res.json()\n  return body.status\n}\n";

        let found = reads("src/invoices.ts", source);

        assert_eq!(found, vec![read("status", 2, 2)]);
    }

    #[test]
    fn the_response_objects_own_properties_are_not_fields() {
        let source = "export async function list() {\n  const res = await axios.get('/v1/items')\n  if (res.status !== 200) return []\n  return res.data.items\n}\n";

        let found = reads("src/items.ts", source);

        assert_eq!(found, vec![read("items", 2, 2)]);
    }

    #[test]
    fn a_destructured_result_is_followed_into_its_variables() {
        let source = "export async function list() {\n  const { data } = await axios.get(\n    '/v1/items',\n  )\n  return data.items.map((item) => item.id)\n}\n";

        let found = reads("src/items.ts", source);

        assert_eq!(found, vec![read("items", 2, 4)]);
    }

    #[test]
    fn python_keys_read_off_a_json_body_are_read_off_the_request() {
        let source = "def total(client):\n    resp = requests.get(\"/v1/orders\")\n    body = resp.json()\n    return resp.json()[\"items\"], body.get(\"total\")\n";

        let found = reads("app/orders.py", source);

        assert_eq!(found, vec![read("items", 2, 2), read("total", 2, 2)]);
    }

    #[test]
    fn names_read_through_the_response_objects_own_parts_are_not_fields() {
        let source = "def check(url):\n    r = requests.get(url)\n    return r.headers[\"allow\"], r.history[0].status_code, r.json()[\"items\"]\n";

        let found = reads("app/check.py", source);

        assert_eq!(found, vec![read("items", 2, 2)]);
    }

    #[test]
    fn a_name_one_step_off_a_result_is_kept_apart_from_those_inside_the_body() {
        let source = "export async function amount(client: Client, id: string) {\n  const invoice = await client.invoices.retrieve(id)\n  return invoice.amount\n}\n";

        let inside = reads("src/invoices.ts", source);
        let one_step = direct_reads("src/invoices.ts", source);

        assert_eq!(inside, Vec::<(String, u32, u32)>::new());
        assert_eq!(one_step, vec![read("amount", 2, 2)]);
    }

    #[test]
    fn a_name_a_response_object_has_is_not_read_one_step_off_a_result() {
        let source = "export async function state(client: Client, id: string) {\n  const result = await client.invoices.retrieve(id)\n  return [result.status, result.data, result.error]\n}\n";

        let one_step = direct_reads("src/invoices.ts", source);

        assert_eq!(one_step, Vec::<(String, u32, u32)>::new());
    }

    #[test]
    fn a_key_taken_out_of_a_result_is_read_one_step_off_it() {
        let source = "export async function page(client: Client) {\n  const { items, next_cursor: cursor } = await client.invoices.list()\n  return [items, cursor]\n}\n";

        let one_step = direct_reads("src/invoices.ts", source);

        assert_eq!(
            one_step,
            vec![read("items", 2, 2), read("next_cursor", 2, 2)]
        );
    }
}
