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
//! when what it is called on is known to make requests: an HTTP library by
//! its usual name, or a receiver the same file calls with a longer path.

use std::collections::BTreeSet;

use tree_sitter::Node;

use super::{PathReference, VERBS, Walk};
use crate::paths::{canonical_method, canonical_path, canonical_route};

/// HTTP libraries whose verb functions take a path under a configured base:
/// a single word given to one of them is a path.
const LIBRARIES: [&str; 6] = ["axios", "requests", "httpx", "ky", "got", "superagent"];

/// Paths read without a leading slash: the receivers that proved to make
/// requests, and the single words waiting on one.
#[derive(Default)]
pub(super) struct Relative {
    receivers: BTreeSet<String>,
    waiting: Vec<(String, PathReference)>,
}

impl Walk<'_> {
    /// Reads `receiver.verb('path/without/slash', ...)`; whether the call
    /// was one.
    pub(super) fn relative_request(&mut self, parts: &[String], arguments: Node<'_>) -> bool {
        let Some((called, receiver)) = parts.split_last() else {
            return false;
        };
        if !VERBS.contains(&called.to_ascii_lowercase().as_str()) {
            return false;
        }
        let Some(first) = arguments.named_child(0) else {
            return false;
        };
        let Some(written) = self.string_value(first) else {
            return false;
        };
        let starts_a_path = written
            .chars()
            .next()
            .is_some_and(|first| first.is_ascii_alphanumeric());
        if !starts_a_path || written.contains("://") || written.contains(char::is_whitespace) {
            return false;
        }
        if self.may_be_joined(called, arguments) {
            return false;
        }
        let rooted = format!("/{written}");
        let Some(path) = canonical_path(&rooted, &self.options.path_markers) else {
            return false;
        };
        // The marker has to start the path: `docs/v1/users` is not `/v1/users`.
        if canonical_route(&rooted).as_deref() != Some(path.as_str()) {
            return false;
        }
        let reference = PathReference {
            method: canonical_method(called),
            path,
            location: self.location(first),
            is_relative: true,
        };
        let receiver = receiver.join(".");
        let is_library = receiver
            .split('.')
            .next()
            .is_some_and(|root| LIBRARIES.contains(&root.to_ascii_lowercase().as_str()));
        if reference.path.matches('/').count() >= 2 {
            self.relative.receivers.insert(receiver);
        } else if !is_library {
            self.relative.waiting.push((receiver, reference));
            return true;
        }
        self.consumed.push(first.id());
        self.facts.path_references.push(reference);
        true
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

    /// Takes the single words given to receivers that proved, elsewhere in
    /// the file, to make requests.
    pub(super) fn finish_relative(&mut self) {
        let relative = std::mem::take(&mut self.relative);
        for (receiver, reference) in relative.waiting {
            if relative.receivers.contains(&receiver) {
                self.facts.path_references.push(reference);
            }
        }
    }
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
    fn a_marker_further_into_the_text_does_not_make_it_a_path() {
        let source = "export const page = () => docs.get('guides/articles/intro')\n";

        let found = relative_calls(source);

        assert_eq!(found, Vec::<(String, String)>::new());
    }
}
