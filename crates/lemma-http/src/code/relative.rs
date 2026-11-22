//! Paths written without their leading slash and given first to a call
//! named for an HTTP verb: `api.get('articles/' + slug)` over a wrapper
//! that joins the path to a base (`fetch(`${base}/${path}`)`), or
//! `axios.post('users/login')` with a `baseURL` set once. The code never
//! writes `/articles/...`, so no string in it holds a marker.
//!
//! A wrapper may also join two arguments (`api.get('articles', slug)` for
//! `/articles/{slug}`), so the path must be the whole of what is passed:
//! another string after it, or after a path of a method without a body a
//! bare name, may be the rest of the path, and then nothing is read.
//!
//! A path of two segments or more under a marker is taken as written: no
//! other string looks like `articles/{}/comments`. A single word (`'user'`,
//! `'tags'`) is also what `cookies.get('user')` passes, so it counts only
//! when what it is called on is known to make requests: an HTTP library or
//! an `api` module by its usual name, or a receiver the same file calls
//! with a longer path.
//!
//! Whatever is looked at here and not read is kept as an
//! [`UnreadRequest`], with why: the reader declines nothing silently.

use std::collections::BTreeSet;

use tree_sitter::Node;

use super::{Location, PathReference, Walk, called_method};
use crate::paths::{canonical_path, canonical_route};

/// What a single word given to its verb functions is a path for: the HTTP
/// libraries that take a path under a configured base, and `api`, the
/// name a module wrapping one goes by (`import * as api from '$lib/api'`).
/// The other names clients go by (`client`, `session`, `http`) also name
/// caches and sessions, whose `get('user')` is no request.
const REQUESTERS: [&str; 7] = [
    "axios",
    "requests",
    "httpx",
    "ky",
    "got",
    "superagent",
    "api",
];

/// A call that looks like a request and was not read as a call of a path,
/// and why.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, serde::Serialize, serde::Deserialize)]
pub struct UnreadRequest {
    /// Why: `joined`, `computed`, `unproven` or `wrapped`.
    pub reason: String,
    /// The first argument as the code writes it, shortened.
    pub written: String,
    /// Where.
    pub location: Location,
}

/// The path may go on past what is written: another argument, or a piece
/// glued to it, may be the rest (`api.get('articles', slug)`).
pub(super) const JOINED: &str = "joined";
/// A request function is given a path computed elsewhere
/// (`axios.get(url)`, ``fetch(`${base}/${path}`)``).
pub(super) const COMPUTED: &str = "computed";
/// A single word that is a path if what it is given to makes requests,
/// which nothing in the file shows (`store.get('user')`).
pub(super) const UNPROVEN: &str = "unproven";
/// A path's first word given to a method that is not named for a verb, on
/// a receiver the file makes requests through (`Api.query('articles')`).
pub(super) const WRAPPED: &str = "wrapped";

/// Paths read without a leading slash: the receivers that proved to make
/// requests, the single words waiting on one, and the words given to
/// other methods of one.
#[derive(Default)]
pub(super) struct Relative {
    receivers: BTreeSet<String>,
    waiting: Vec<(String, PathReference, String)>,
    wrapped: Vec<(String, UnreadRequest)>,
}

impl Walk<'_> {
    /// Reads `receiver.verb('path/without/slash', ...)`; whether the call
    /// was one.
    pub(super) fn relative_request(&mut self, parts: &[String], arguments: Node<'_>) -> bool {
        let Some((called, receiver)) = parts.split_last() else {
            return false;
        };
        let Some(first) = arguments.named_child(0) else {
            return false;
        };
        let Some((written, is_whole)) = self.first_written(first) else {
            return false;
        };
        let Some(path) = self.relative_path(&written) else {
            return false;
        };
        let receiver = receiver.join(".");
        let Some(method) = called_method(called) else {
            let unread = self.unread(WRAPPED, first);
            self.relative.wrapped.push((receiver, unread));
            return false;
        };
        if !is_whole || self.may_be_joined(called, arguments) {
            let unread = self.unread(JOINED, first);
            self.facts.unread_requests.push(unread);
            return false;
        }
        let reference = PathReference {
            method,
            path,
            location: self.location(first),
            is_relative: true,
        };
        if reference.path.matches('/').count() >= 2 {
            self.relative.receivers.insert(receiver);
        } else if !is_requester(&receiver) {
            let text = self.shortened(first);
            self.relative.waiting.push((receiver, reference, text));
            return true;
        }
        self.consumed.push(first.id());
        self.facts.path_references.push(reference);
        true
    }

    /// Notes a request function given a path that is computed: a verb of an
    /// HTTP library or `fetch`, whose first argument is not a string.
    pub(super) fn computed_request(&mut self, parts: &[String], arguments: Node<'_>) {
        let Some((called, receiver)) = parts.split_last() else {
            return;
        };
        let is_request = (receiver.is_empty() && called == "fetch")
            || (called_method(called).is_some() && is_requester(&receiver.join(".")));
        let Some(first) = arguments.named_child(0).filter(|_| is_request) else {
            return;
        };
        // A string, or a join starting with one, is written: read as a
        // path elsewhere, or not one.
        if matches!(self.first_written(first), Some((text, _)) if !text.starts_with("${")) {
            return;
        }
        let unread = self.unread(COMPUTED, first);
        self.facts.unread_requests.push(unread);
    }

    /// What `first` writes of a path: a string whole, or the string a join
    /// with `+` starts with, which is then not the whole of it.
    fn first_written(&self, first: Node<'_>) -> Option<(String, bool)> {
        if let Some(written) = self.string_value(first) {
            return Some((written, true));
        }
        let mut leftmost = first;
        while matches!(leftmost.kind(), "binary_expression" | "binary_operator") {
            leftmost = leftmost.child_by_field_name("left")?;
        }
        let written = self.string_value(leftmost)?;
        (leftmost.id() != first.id()).then_some((written, false))
    }

    /// The path `written` is with a slash before it, if it then starts
    /// with a segment a service serves.
    fn relative_path(&self, written: &str) -> Option<String> {
        let starts_a_path = written
            .chars()
            .next()
            .is_some_and(|first| first.is_ascii_alphanumeric());
        if !starts_a_path || written.contains("://") || written.contains(char::is_whitespace) {
            return None;
        }
        let rooted = format!("/{written}");
        let path = canonical_path(&rooted, &self.options.path_markers)?;
        // The marker has to start the path: `docs/v1/users` is not `/v1/users`.
        (canonical_route(&rooted).as_deref() == Some(path.as_str())).then_some(path)
    }

    /// Whether the argument after the first may be more of the path: a
    /// string, or for a method that sends no body, a bare name.
    fn may_be_joined(&self, called: &str, arguments: Node<'_>) -> bool {
        let Some(second) = arguments.named_child(1) else {
            return false;
        };
        let sends_a_body = matches!(
            called.to_ascii_lowercase().as_str(),
            "post" | "put" | "patch"
        );
        self.string_value(second).is_some() || (second.kind() == "identifier" && !sends_a_body)
    }

    /// An unread request at `first`, the call's first argument.
    pub(super) fn unread(&self, reason: &str, first: Node<'_>) -> UnreadRequest {
        UnreadRequest {
            reason: reason.to_owned(),
            written: self.shortened(first),
            location: self.location(first),
        }
    }

    /// `node` as the code writes it, on one line and at most 80 characters.
    fn shortened(&self, node: Node<'_>) -> String {
        let text: Vec<&str> = self.text(node).split_whitespace().collect();
        text.join(" ").chars().take(80).collect()
    }

    /// Takes the single words given to receivers that proved, elsewhere in
    /// the file, to make requests, and notes the rest as unread, with the
    /// words such receivers' other methods were given.
    pub(super) fn finish_relative(&mut self) {
        let relative = std::mem::take(&mut self.relative);
        for (receiver, reference, written) in relative.waiting {
            if relative.receivers.contains(&receiver) {
                self.facts.path_references.push(reference);
            } else {
                self.facts.unread_requests.push(UnreadRequest {
                    reason: UNPROVEN.to_owned(),
                    written,
                    location: reference.location,
                });
            }
        }
        for (receiver, unread) in relative.wrapped {
            if relative.receivers.contains(&receiver) {
                self.facts.unread_requests.push(unread);
            }
        }
    }
}

/// Whether `receiver` starts with a name whose verb functions take a path.
fn is_requester(receiver: &str) -> bool {
    receiver
        .split('.')
        .next()
        .is_some_and(|root| REQUESTERS.contains(&root.to_ascii_lowercase().as_str()))
}

#[cfg(test)]
mod tests {
    use lemma_syntax::Extractor;

    use crate::code::{CodeOptions, extract};

    /// The paths `source` calls, as method and path, where each was read
    /// without its leading slash.
    fn relative_calls(source: &str) -> Vec<(String, String)> {
        let options = CodeOptions {
            path_markers: ["/articles/", "/tags/", "/user/"]
                .map(str::to_owned)
                .to_vec(),
            ..CodeOptions::default()
        };
        let mut extractor = Extractor::new().unwrap();
        let facts = extract("src/api.ts", source, &options, &mut extractor).unwrap();
        facts
            .path_references
            .into_iter()
            .filter(|reference| reference.is_relative)
            .map(|reference| (reference.method, reference.path))
            .collect()
    }

    fn call(method: &str, path: &str) -> (String, String) {
        (method.to_owned(), path.to_owned())
    }

    /// The requests `source` makes that were not read, as why and what
    /// was written.
    fn unread(source: &str) -> Vec<(String, String)> {
        let options = CodeOptions {
            path_markers: ["/articles/", "/tags/", "/user/"]
                .map(str::to_owned)
                .to_vec(),
            ..CodeOptions::default()
        };
        let mut extractor = Extractor::new().unwrap();
        let facts = extract("src/api.ts", source, &options, &mut extractor).unwrap();
        facts
            .unread_requests
            .into_iter()
            .map(|unread| (unread.reason, unread.written))
            .collect()
    }

    fn declined(reason: &str, written: &str) -> (String, String) {
        (reason.to_owned(), written.to_owned())
    }

    #[test]
    fn a_path_that_may_go_on_is_declined_as_joined() {
        let source = "export const one = (slug: string) => api.get('articles', slug)
export const some = (kind: string) => api.get('articles' + kind)
";

        let found = unread(source);

        assert_eq!(
            found,
            vec![
                declined("joined", "'articles'"),
                declined("joined", "'articles' + kind"),
            ]
        );
    }

    #[test]
    fn a_request_function_given_a_computed_path_is_declined_as_computed() {
        let source = "export const send = (path: string) => fetch(`${base}/${path}`)
export const load = (url: string) => axios.get(url)
export const seen = (key: string) => cache.get(key)
";

        let found = unread(source);

        assert_eq!(
            found,
            vec![
                declined("computed", "`${base}/${path}`"),
                declined("computed", "url"),
            ]
        );
    }

    #[test]
    fn a_single_word_on_an_unknown_receiver_is_declined_as_unproven() {
        let source = "export const seen = () => store.get('user')
";

        let found = unread(source);

        assert_eq!(found, vec![declined("unproven", "'user'")]);
    }

    #[test]
    fn a_word_given_to_another_method_of_a_requesting_receiver_is_declined_as_wrapped() {
        let source = "export const list = (params: Params) => Api.query('articles', params)
export const remove = (slug: string) => Api.delete(`articles/${slug}`)
export const other = () => Text.query('articles')
";

        let found = unread(source);

        assert_eq!(found, vec![declined("wrapped", "'articles'")]);
    }

    #[test]
    fn a_path_without_its_slash_given_to_a_verb_is_a_call() {
        let source = "export const comments = (slug: string) => api.get(`articles/${slug}/comments`, token())\n";

        let found = relative_calls(source);

        assert_eq!(found, vec![call("GET", "/articles/{}/comments")]);
    }

    #[test]
    fn a_single_word_is_a_path_on_an_http_library() {
        let source = "export const tags = () => axios.get('tags')\nexport const who = () => cookies.get('user')\n";

        let found = relative_calls(source);

        assert_eq!(found, vec![call("GET", "/tags")]);
    }

    #[test]
    fn a_single_word_is_a_path_on_a_receiver_the_file_calls_with_a_longer_one() {
        let source = "export const save = (user: User) => api.put('user', user)\nexport const remove = (slug: string) => api.delete(`articles/${slug}`)\nexport const seen = () => store.get('user')\n";

        let found = relative_calls(source);

        assert_eq!(
            found,
            vec![call("DELETE", "/articles/{}"), call("PUT", "/user")]
        );
    }

    #[test]
    fn a_path_that_may_be_joined_with_the_next_argument_is_not_read() {
        let source = "export const one = (slug: string) => api.get('articles', slug)\nexport const of = (slug: string) => api.get('articles', `${slug}/comments`)\nexport const all = () => api.delete(`articles/${id}`)\n";

        let found = relative_calls(source);

        assert_eq!(found, vec![call("DELETE", "/articles/{}")]);
    }

    #[test]
    fn a_single_word_is_a_path_on_a_module_named_api() {
        let source = "import * as api from '$lib/api'
export const register = (user: User) => api.post('user', { user })
export const seen = () => session.get('user')
";

        let found = relative_calls(source);

        assert_eq!(found, vec![call("POST", "/user")]);
    }

    #[test]
    fn del_is_delete_where_delete_is_a_reserved_word() {
        let source = "export const remove = (slug: string) => api.del(`articles/${slug}`, token())
";

        let found = relative_calls(source);

        assert_eq!(found, vec![call("DELETE", "/articles/{}")]);
    }

    #[test]
    fn a_marker_further_into_the_text_does_not_make_it_a_path() {
        let source = "export const page = () => docs.get('guides/articles/intro')\n";

        let found = relative_calls(source);

        assert_eq!(found, Vec::<(String, String)>::new());
    }
}
