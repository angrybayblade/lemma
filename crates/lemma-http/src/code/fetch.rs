//! The method of a `fetch` that names none. The Fetch standard gives it:
//! a request made from a URL, with no `method` among its options, is a
//! `GET`. So `fetch('/v1/users')` calls the `GET` of that path, and is not
//! a call of its `POST` or `DELETE`.
//!
//! The method is only known where the whole call is written out. Options
//! passed as a variable, spread from one, or holding a computed `method`
//! may name any method, and so may a `Request` passed for the URL: those
//! stay requests of no known method.

use tree_sitter::Node;

use super::{Dialect, Walk};

/// The objects `fetch` is a property of in a browser, a worker or Node.
const GLOBALS: [&str; 3] = ["window", "globalThis", "self"];

impl Walk<'_> {
    /// Whether the call is a `fetch` that is a `GET` because nothing it is
    /// given can name another method.
    pub(super) fn is_get_by_default(&self, parts: &[String], arguments: Node<'_>) -> bool {
        if self.dialect != Dialect::Script {
            return false;
        }
        let is_fetch = match parts {
            [called] => called == "fetch",
            [global, called] => called == "fetch" && GLOBALS.contains(&global.as_str()),
            _ => false,
        };
        if !is_fetch {
            return false;
        }
        let mut cursor = arguments.walk();
        let mut given = arguments.named_children(&mut cursor);
        let Some(input) = given.next() else {
            return false;
        };
        if self.is_request_object(input) {
            return false;
        }
        given.next().is_none_or(|init| self.names_no_method(init))
    }

    /// `new Request(...)`, which carries a method of its own.
    fn is_request_object(&self, input: Node<'_>) -> bool {
        input.kind() == "new_expression"
            && input
                .child_by_field_name("constructor")
                .is_some_and(|constructor| self.text(constructor) == "Request")
    }

    /// Whether `init` is an object written out in full with no `method`.
    fn names_no_method(&self, init: Node<'_>) -> bool {
        if init.kind() != "object" {
            return false;
        }
        let mut cursor = init.walk();
        init.named_children(&mut cursor)
            .all(|member| match member.kind() {
                "pair" => member
                    .child_by_field_name("key")
                    .is_some_and(|key| !self.is_method_key(key)),
                "shorthand_property_identifier" => !self.is_method_key(member),
                "comment" => true,
                _ => false, // a spread or a computed member may hold one
            })
    }

    /// Whether `key` is `method`, bare or quoted.
    fn is_method_key(&self, key: Node<'_>) -> bool {
        self.text(key).trim_matches(['"', '\'']) == "method"
    }
}

#[cfg(test)]
mod tests {
    use lemma_syntax::Extractor;

    use crate::code::{CodeOptions, extract};

    /// The method and path of each request `source` makes.
    fn requests(file: &str, source: &str) -> Vec<(String, String)> {
        let mut extractor = Extractor::new().unwrap();
        let options = CodeOptions {
            path_markers: vec!["/v1/".to_owned()],
            ..CodeOptions::default()
        };
        let facts = extract(file, source, &options, &mut extractor).unwrap();
        facts
            .path_references
            .into_iter()
            .map(|reference| (reference.method, reference.path))
            .collect()
    }

    fn request(method: &str, path: &str) -> (String, String) {
        (method.to_owned(), path.to_owned())
    }

    #[test]
    fn a_fetch_given_no_method_is_a_get() {
        let source = "const users = () => fetch('/v1/users')\nconst user = (id) => window.fetch(`/v1/users/${id}`, { headers, credentials: 'include' })\n";

        let found = requests("src/users.ts", source);

        assert_eq!(
            found,
            [request("GET", "/v1/users"), request("GET", "/v1/users/{}")]
        );
    }

    #[test]
    fn a_fetch_whose_options_may_name_a_method_has_none_known() {
        let source = "const a = (options) => fetch('/v1/a', options)\nconst b = (rest) => fetch('/v1/b', { ...rest })\nconst c = (method) => fetch('/v1/c', { method })\nconst d = (verb) => fetch('/v1/d', { 'method': verb })\nconst e = () => fetch(new Request('/v1/e', init))\n";

        let found = requests("src/any.ts", source);

        let methods: Vec<&str> = found.iter().map(|(method, _)| method.as_str()).collect();
        assert_eq!(methods, ["*", "*", "*", "*", "*"], "{found:?}");
    }

    #[test]
    fn a_method_written_in_the_options_is_kept() {
        let source = "const save = (body) => fetch('/v1/users', { method: 'POST', body })\n";

        let found = requests("src/users.ts", source);

        assert_eq!(found, [request("POST", "/v1/users")]);
    }

    #[test]
    fn a_fetch_method_of_something_else_is_not_the_platforms() {
        let source = "const items = () => store.fetch('/v1/items')\n";

        let found = requests("src/items.ts", source);

        assert_eq!(found, [request("*", "/v1/items")]);
    }

    #[test]
    fn a_function_named_fetch_in_python_has_no_known_method() {
        let source = "def users():\n    return fetch(\"/v1/users\")\n";

        let found = requests("app/users.py", source);

        assert_eq!(found, [request("*", "/v1/users")]);
    }
}
