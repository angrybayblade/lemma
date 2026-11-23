//! Paths joined from pieces with `+`: `'/profiles/' + username + '/follow'`.
//! Each computed piece stands where a parameter would, when slashes set it
//! apart: `/profiles/{}/follow`. A piece glued to a word
//! (`'/articles' + suffix`) may add anything, so the path is then left to
//! be read from its first string alone, as before.

use tree_sitter::Node;

use super::Walk;
use crate::paths::has_marker;

/// One piece of a joined path.
enum Piece<'tree> {
    Written(String, Node<'tree>),
    Computed,
}

impl Walk<'_> {
    /// Reads a request whose path argument is joined with `+`; whether the
    /// call was one.
    pub(super) fn joined_request(
        &mut self,
        parts: &[String],
        arguments: Node<'_>,
        strings: &[Node<'_>],
    ) -> bool {
        let Some((template, written)) = self.joined_path(arguments) else {
            self.note_glued(arguments);
            return false;
        };
        let method = self.request_method(parts, arguments, strings);
        if has_marker(&template, &self.options.path_markers) {
            self.add_reference(&method, &template, written[0]);
        } else {
            // A path under a base is read only where a request is plain.
            let called = parts.last().map(String::as_str).unwrap_or_default();
            if method == "*" && called != "fetch" {
                return false;
            }
            let location = self.location(written[0]);
            let references = self.based_references(&method, &template, &location, false);
            self.facts.path_references.extend(references);
        }
        self.consumed.extend(written.iter().map(Node::id));
        true
    }

    /// Notes a join that starts with a path and was not read whole: what a
    /// glued piece adds is unknown, so the path may go on.
    fn note_glued(&mut self, arguments: Node<'_>) {
        let mut cursor = arguments.walk();
        let joins: Vec<Node<'_>> = arguments
            .named_children(&mut cursor)
            .filter(|argument| matches!(argument.kind(), "binary_expression" | "binary_operator"))
            .collect();
        for join in joins {
            let mut pieces = Vec::new();
            self.join_pieces(join, &mut pieces);
            let starts_a_path = matches!(
                pieces.first(),
                Some(Piece::Written(first, _)) if has_marker(first, &self.options.path_markers)
            );
            if starts_a_path {
                let unread = self.unread(super::relative::JOINED, join);
                self.facts.unread_requests.push(unread);
            }
        }
    }

    /// The first argument joined with `+` from a path's pieces: its
    /// template, with `{}` for each computed piece, and its written pieces.
    fn joined_path<'tree>(&self, arguments: Node<'tree>) -> Option<(String, Vec<Node<'tree>>)> {
        let mut cursor = arguments.walk();
        arguments
            .named_children(&mut cursor)
            .filter(|argument| matches!(argument.kind(), "binary_expression" | "binary_operator"))
            .find_map(|argument| self.joined_template(argument))
    }

    fn joined_template<'tree>(&self, node: Node<'tree>) -> Option<(String, Vec<Node<'tree>>)> {
        let mut pieces = Vec::new();
        self.join_pieces(node, &mut pieces);
        let Some(Piece::Written(first, _)) = pieces.first() else {
            return None;
        };
        if !has_marker(first, &self.options.path_markers) && !self.is_under_a_base(first) {
            return None;
        }
        let mut template = String::new();
        let mut written = Vec::new();
        for (index, piece) in pieces.iter().enumerate() {
            match piece {
                Piece::Written(text, node) => {
                    template.push_str(text);
                    written.push(*node);
                }
                Piece::Computed if is_set_apart(&template, pieces.get(index + 1)) => {
                    template.push_str("{}");
                }
                Piece::Computed => return None,
            }
        }
        Some((template, written))
    }

    /// The pieces `node` joins with `+`, in order; anything but a string or
    /// another join is one computed piece.
    fn join_pieces<'tree>(&self, node: Node<'tree>, pieces: &mut Vec<Piece<'tree>>) {
        let is_join = matches!(node.kind(), "binary_expression" | "binary_operator")
            && node
                .child_by_field_name("operator")
                .is_some_and(|operator| self.text(operator) == "+");
        if is_join {
            for side in ["left", "right"] {
                match node.child_by_field_name(side) {
                    Some(side) => self.join_pieces(side, pieces),
                    None => pieces.push(Piece::Computed),
                }
            }
            return;
        }
        match self.string_value(node) {
            Some(text) => pieces.push(Piece::Written(text, node)),
            None => pieces.push(Piece::Computed),
        }
    }
}

/// Whether a computed piece between `before` and `next` stands alone as a
/// segment: a slash before it, and after it a slash, a query or the end.
fn is_set_apart(before: &str, next: Option<&Piece<'_>>) -> bool {
    let is_closed = match next {
        Some(Piece::Written(text, _)) => text.starts_with(['/', '?']),
        Some(Piece::Computed) => false,
        None => true,
    };
    before.ends_with('/') && is_closed
}

#[cfg(test)]
mod tests {
    use lemma_syntax::Extractor;

    use crate::code::{CodeOptions, extract};

    /// The paths `source` in `file` calls, as method and path.
    fn calls(file: &str, source: &str) -> Vec<(String, String)> {
        let options = CodeOptions {
            path_markers: ["/profiles/", "/articles/"].map(str::to_owned).to_vec(),
            ..CodeOptions::default()
        };
        let mut extractor = Extractor::new().unwrap();
        let facts = extract(file, source, &options, &mut extractor).unwrap();
        facts
            .path_references
            .into_iter()
            .map(|reference| (reference.method, reference.path))
            .collect()
    }

    fn call(method: &str, path: &str) -> (String, String) {
        (method.to_owned(), path.to_owned())
    }

    #[test]
    fn pieces_joined_at_slashes_make_one_path() {
        let source = "export const follow = (username: string) => http.post('/profiles/' + username + '/follow', {})\n";

        let found = calls("src/profile.ts", source);

        assert_eq!(found, vec![call("POST", "/profiles/{}/follow")]);
    }

    #[test]
    fn a_python_path_joined_with_plus_is_read_the_same_way() {
        let source = "def show(name):\n    return requests.get(\"/profiles/\" + name)\n";

        let found = calls("app/profiles.py", source);

        assert_eq!(found, vec![call("GET", "/profiles/{}")]);
    }

    #[test]
    fn a_piece_glued_to_a_word_leaves_the_first_string_as_the_path() {
        let source = "export const list = (feed: boolean) => http.get('/articles' + (feed ? '/feed' : ''), {})\n";

        let found = calls("src/articles.ts", source);

        assert_eq!(found, vec![call("GET", "/articles")]);
    }

    #[test]
    fn a_path_with_a_glued_piece_is_also_declined_as_joined() {
        let source = "export const list = (feed: boolean) => http.get('/articles' + (feed ? '/feed' : ''), {})
";
        let options = CodeOptions {
            path_markers: vec!["/articles/".to_owned()],
            ..CodeOptions::default()
        };
        let mut extractor = Extractor::new().unwrap();

        let facts = extract("src/articles.ts", source, &options, &mut extractor).unwrap();

        let declined: Vec<&str> = facts
            .unread_requests
            .iter()
            .map(|unread| unread.reason.as_str())
            .collect();
        assert_eq!(declined, ["joined"]);
    }
}
