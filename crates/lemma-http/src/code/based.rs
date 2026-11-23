//! Paths written under a base the code keeps elsewhere: `get('/articles')`
//! over a base URL ending in `/api`, set once, for the operation
//! `/api/articles`. The string holds no marker of its own, since the
//! leading segment the service serves is in the base.
//!
//! A segment found under a base (`/articles/`) is far commoner in code
//! than a service's leading segment is: page routes, file paths and keys
//! are written the same way. So such a path is read only where the code
//! plainly makes a request (a call named for a verb, one given a verb or
//! a `method`, `fetch`, or an object naming a method beside its path), and
//! each is kept with its base ([`PathReference::base`]). Whether it is a
//! call is decided where the whole repository is known: only a repository
//! that names the base ([`CodeFacts::named_bases`](super::CodeFacts)) writes
//! its paths under it.

use tree_sitter::Node;

use super::{Location, PathReference, Walk, called_method};
use crate::paths::{canonical_method, has_marker, names_base, path_under_base};

impl Walk<'_> {
    /// The paths `template` is under the bases, each with its base's
    /// prefix: none when it has a marker of its own.
    pub(super) fn under_bases(&self, template: &str) -> Vec<(String, String)> {
        if has_marker(template, &self.options.path_markers) {
            return Vec::new();
        }
        self.options
            .bases
            .iter()
            .filter_map(|base| Some((path_under_base(template, base)?, base.prefix.clone())))
            .collect()
    }

    /// Whether `template` is a path under one of the bases.
    pub(super) fn is_under_a_base(&self, template: &str) -> bool {
        !self.under_bases(template).is_empty()
    }

    /// A request of `template` under each base it may be under.
    pub(super) fn based_references(
        &self,
        method: &str,
        template: &str,
        location: &Location,
        is_relative: bool,
    ) -> Vec<PathReference> {
        self.under_bases(template)
            .into_iter()
            .map(|(path, base)| PathReference {
                method: method.to_owned(),
                path,
                location: location.clone(),
                is_relative,
                base,
            })
            .collect()
    }

    /// Reads a request given a path under a base: `http.get('/articles')`,
    /// `send('post', '/users', body)`, `fetch('/tags')`. Whether the call
    /// was one.
    pub(super) fn based_request(
        &mut self,
        parts: &[String],
        arguments: Node<'_>,
        strings: &[Node<'_>],
    ) -> bool {
        if self.options.bases.is_empty() {
            return false;
        }
        let method = self.request_method(parts, arguments, strings);
        let called = parts.last().map(String::as_str).unwrap_or_default();
        if method == "*" && called != "fetch" {
            return false;
        }
        let written = strings.iter().copied().find(|argument| {
            self.string_value(*argument)
                .is_some_and(|text| self.is_under_a_base(&text))
        });
        let Some(node) = written.filter(|node| !self.consumed.contains(&node.id())) else {
            return self.glued_based_request(arguments);
        };
        // A wrapper may join the next argument to the path, as it may for
        // a path written without its slash.
        if called_method(called).is_some() && self.may_be_joined(called, arguments) {
            let unread = self.unread(super::relative::JOINED, node);
            self.facts.unread_requests.push(unread);
            return true;
        }
        let template = self.string_value(node).unwrap_or_default();
        let references = self.based_references(&method, &template, &self.location(node), false);
        self.consumed.push(node.id());
        self.facts.path_references.extend(references);
        true
    }

    /// Reads the path of an object naming its method (`{ method: 'GET',
    /// path: '/articles' }`) under each base it may be under.
    pub(super) fn add_described_request(
        &mut self,
        method: Option<&str>,
        template: &str,
        node: Node<'_>,
    ) {
        let Some(method) = method.map(canonical_method).filter(|method| method != "*") else {
            return;
        };
        let references = self.based_references(&method, template, &self.location(node), false);
        self.facts.path_references.extend(references);
    }

    /// Notes a request whose first argument starts with a path under a
    /// base and goes on with a piece glued to it: what the piece adds is
    /// unknown. Whether there was one.
    fn glued_based_request(&mut self, arguments: Node<'_>) -> bool {
        let Some(first) = arguments.named_child(0) else {
            return false;
        };
        let starts_under_a_base = matches!(
            self.first_written(first),
            Some((text, false)) if self.is_under_a_base(&text)
        );
        if starts_under_a_base {
            let unread = self.unread(super::relative::JOINED, first);
            self.facts.unread_requests.push(unread);
        }
        starts_under_a_base
    }

    /// Notes each base a string names, as a base URL does.
    pub(super) fn note_named_bases(&mut self, node: Node<'_>) {
        if self.options.bases.is_empty() {
            return;
        }
        let Some(text) = self.string_value(node) else {
            return;
        };
        let named = self
            .options
            .bases
            .iter()
            .filter(|base| names_base(&text, &base.prefix))
            .map(|base| base.prefix.clone());
        self.facts.named_bases.extend(named);
    }
}

#[cfg(test)]
mod tests {
    use lemma_syntax::Extractor;

    use crate::code::{CodeFacts, CodeOptions, extract};
    use crate::paths::Base;

    fn facts(source: &str) -> CodeFacts {
        let options = CodeOptions {
            path_markers: vec!["/api/".to_owned()],
            bases: vec![Base {
                prefix: "/api".to_owned(),
                markers: ["/articles/", "/tags/", "/user/", "/users/"]
                    .map(str::to_owned)
                    .to_vec(),
            }],
            ..CodeOptions::default()
        };
        let mut extractor = Extractor::new().unwrap();
        extract("src/api.ts", source, &options, &mut extractor).unwrap()
    }

    /// The paths `source` calls under a base, as method and path.
    fn based_calls(source: &str) -> Vec<(String, String)> {
        facts(source)
            .path_references
            .into_iter()
            .filter(|reference| !reference.base.is_empty())
            .map(|reference| (reference.method, reference.path))
            .collect()
    }

    fn call(method: &str, path: &str) -> (String, String) {
        (method.to_owned(), path.to_owned())
    }

    #[test]
    fn a_path_given_to_a_verb_is_read_under_the_base() {
        let source = "export const show = (slug: string) => http.get(`/articles/${slug}`)\nexport const login = (user: User) => http.post('/users/login', { user })\n";

        let found = based_calls(source);

        assert_eq!(
            found,
            vec![
                call("GET", "/api/articles/{}"),
                call("POST", "/api/users/login")
            ]
        );
    }

    #[test]
    fn a_path_beside_a_verb_or_given_to_fetch_is_read_under_the_base() {
        let source = "const current = () => send('get', '/user')\nconst tags = () => fetch(`${ROOT}/tags`)\nconst save = (user) => request({ path: '/user', method: 'PUT', body: user })\n";

        let found = based_calls(source);

        assert_eq!(
            found,
            vec![
                call("*", "/api/tags"),
                call("GET", "/api/user"),
                call("PUT", "/api/user")
            ]
        );
    }

    #[test]
    fn a_path_joined_or_written_without_its_slash_is_read_under_the_base() {
        let source = "const remove = (slug, id) => http.delete('/articles/' + slug + '/comments/' + id)\nconst feed = () => axios.get('articles/feed')\n";

        let found = based_calls(source);

        assert_eq!(
            found,
            vec![
                call("DELETE", "/api/articles/{}/comments/{}"),
                call("GET", "/api/articles/feed")
            ]
        );
    }

    #[test]
    fn a_path_outside_a_request_is_not_read_under_a_base() {
        let source = "router.push('/articles/' + slug)\nconst routes = [{ path: '/articles', component: List }]\nconst link = `/tags/${tag}`\nstore.dispatch('/user')\n";

        let found = based_calls(source);

        assert_eq!(found, Vec::<(String, String)>::new());
    }

    #[test]
    fn a_path_to_another_host_or_with_its_own_marker_is_not_under_a_base() {
        let source = "const other = () => http.get('https://other.example/articles')\nconst whole = () => http.get('/api/articles')\n";

        let found = facts(source);

        assert!(
            found
                .path_references
                .iter()
                .all(|reference| reference.base.is_empty())
        );
        assert!(
            found
                .path_references
                .iter()
                .any(|reference| reference.path == "/api/articles")
        );
    }

    #[test]
    fn a_path_another_argument_may_go_on_from_is_declined_aloud() {
        let source = "const show = (slug) => Api.get('/articles', slug)\nconst list = (query) => http.get('/articles' + query)\n";

        let found = facts(source);

        assert_eq!(based_calls(source), Vec::<(String, String)>::new());
        let reasons: Vec<&str> = found
            .unread_requests
            .iter()
            .map(|unread| unread.reason.as_str())
            .collect();
        assert_eq!(reasons, ["joined", "joined"]);
    }

    #[test]
    fn the_bases_a_file_names_are_kept() {
        let source = "const ROOT = 'https://api.example.com/api'\nconst docs = 'https://example.com/docs/api'\nconst users = `${ROOT}/users`\n";

        let found = facts(source);

        assert_eq!(found.named_bases, ["/api"]);
    }
}
