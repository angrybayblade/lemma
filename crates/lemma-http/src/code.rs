//! HTTP facts in TypeScript and Python source, read with Tree-sitter.
//!
//! The extractor reports what the code says, not what it means: an object
//! with `method` and `path` is an operation declaration in a service, an
//! operation table in a generated client and a request descriptor in a
//! consumer. The caller interprets it by the repository's role.

use lemma_model::{Language, Symbol};
use lemma_syntax::Extractor;
use tree_sitter::{Node, Parser};

use crate::paths::{canonical_method, canonical_path, method_before_path};

/// Where in the code a fact was found.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord)]
pub struct Location {
    /// File path, relative to the repository root.
    pub file: String,
    /// Line, counting from 1.
    pub line: u32,
    /// The innermost definition containing it, by qualified name.
    pub symbol: Option<String>,
}

/// An object or call naming an HTTP method and path together.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord)]
pub struct MethodPath {
    /// Upper-case method, or `*` if not given.
    pub method: String,
    /// Canonical path.
    pub path: String,
    /// The path as written.
    pub template: String,
    /// A client method chain declared alongside, such as `sessionOptions.list`.
    pub chain: Option<String>,
    /// Where.
    pub location: Location,
}

/// A string naming an API path outside a method-and-path object.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord)]
pub struct PathReference {
    /// Upper-case method if the surrounding call shows it, else `*`.
    pub method: String,
    /// Canonical path.
    pub path: String,
    /// Where.
    pub location: Location,
}

/// A call through a client object, such as `this.client.sessionOptions.list()`.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord)]
pub struct ClientCall {
    /// The chain after the client receiver: `sessionOptions.list`.
    pub chain: String,
    /// Where.
    pub location: Location,
}

/// Everything found in one file.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct CodeFacts {
    /// Method-and-path objects and calls.
    pub method_paths: Vec<MethodPath>,
    /// Other strings naming API paths.
    pub path_references: Vec<PathReference>,
    /// Calls through client objects.
    pub client_calls: Vec<ClientCall>,
    /// Names the file uses as object keys, member names, keyword arguments
    /// or dictionary keys: the fields it may send or read. Sorted, unique.
    pub mentions: Vec<String>,
    /// Relative import specifiers, as written (`../types/x`, `.models`).
    pub imports: Vec<String>,
}

/// What to look for.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct CodeOptions {
    /// Names of client receivers, such as `client` or `_client`; calls
    /// through them are reported as client calls.
    pub client_receivers: Vec<String>,
}

/// Why a file could not be read.
#[derive(Debug, thiserror::Error)]
pub enum CodeError {
    /// The parser or definition extractor could not be set up or failed.
    #[error("could not parse {file}")]
    Parse {
        /// The file.
        file: String,
        /// Why.
        #[source]
        source: lemma_syntax::SyntaxError,
    },
}

const VERBS: [&str; 5] = ["get", "post", "put", "patch", "delete"];

/// Reads one source file. Files in other languages give no facts.
pub fn extract(
    file: &str,
    source: &str,
    options: &CodeOptions,
    extractor: &mut Extractor,
) -> Result<CodeFacts, CodeError> {
    let extension = file.rsplit_once('.').map_or("", |(_, extension)| extension);
    let Some(language) = parse_language(extension) else {
        return Ok(CodeFacts::default());
    };
    let parse_error = |source| CodeError::Parse {
        file: file.to_owned(),
        source,
    };
    let symbols = extractor
        .extract(language, source)
        .map_err(parse_error)?
        .symbols;
    let mut parser = Parser::new();
    parser
        .set_language(&tree_sitter_language(language))
        .map_err(|source| parse_error(lemma_syntax::SyntaxError::Grammar { language, source }))?;
    let Some(tree) = parser.parse(source, None) else {
        return Err(parse_error(lemma_syntax::SyntaxError::ParseAborted {
            language,
        }));
    };
    let mut walk = Walk {
        file,
        source,
        symbols: &symbols,
        options,
        python: language == Language::Python,
        facts: CodeFacts::default(),
        consumed: Vec::new(),
    };
    walk.visit(tree.root_node());
    walk.collect_strings(tree.root_node());
    let mut facts = walk.facts;
    facts.method_paths.sort();
    facts.path_references.sort();
    facts.client_calls.sort();
    facts.mentions.sort();
    facts.mentions.dedup();
    facts.imports.sort();
    facts.imports.dedup();
    Ok(facts)
}

/// The grammar to parse a file with. JavaScript has no language of its own
/// in Lemma's model; the TSX grammar parses it, JSX included, because
/// TypeScript's syntax is a superset of it.
fn parse_language(extension: &str) -> Option<Language> {
    if matches!(extension, "js" | "jsx" | "mjs" | "cjs") {
        return Some(Language::Tsx);
    }
    Language::from_extension(extension).filter(|language| *language != Language::Go)
}

fn tree_sitter_language(language: Language) -> tree_sitter::Language {
    match language {
        Language::Tsx => tree_sitter_typescript::LANGUAGE_TSX.into(),
        Language::Python => tree_sitter_python::LANGUAGE.into(),
        Language::TypeScript | Language::Go => tree_sitter_typescript::LANGUAGE_TYPESCRIPT.into(),
    }
}

struct Walk<'a> {
    file: &'a str,
    source: &'a str,
    symbols: &'a [Symbol],
    options: &'a CodeOptions,
    python: bool,
    facts: CodeFacts,
    /// Strings already reported as part of a method-and-path or call.
    consumed: Vec<usize>,
}

impl Walk<'_> {
    fn visit(&mut self, node: Node<'_>) {
        match node.kind() {
            "object" => self.object(node),
            "call_expression" | "call" => self.call(node),
            "import_statement" | "import_from_statement" => self.import(node),
            _ => {}
        }
        self.mention(node);
        let mut cursor = node.walk();
        for child in node.named_children(&mut cursor) {
            self.visit(child);
        }
    }

    /// `{ method: 'get', path: '/api/...', chain: ['a', 'b'] }`.
    fn object(&mut self, node: Node<'_>) {
        let mut method = None;
        let mut path = None;
        let mut chain = None;
        let mut cursor = node.walk();
        for pair in node
            .named_children(&mut cursor)
            .filter(|child| child.kind() == "pair")
        {
            let (Some(key), Some(value)) = (
                pair.child_by_field_name("key"),
                pair.child_by_field_name("value"),
            ) else {
                continue;
            };
            match self.text(key).trim_matches(['"', '\'']) {
                "method" => method = self.string_value(value),
                "path" | "url" => path = Some(value),
                "chain" => chain = self.string_list(value),
                _ => {}
            }
        }
        let Some(path_node) = path else { return };
        let Some(template) = self.string_value(path_node) else {
            return;
        };
        self.add_method_path(method.as_deref(), &template, chain, path_node);
    }

    /// `client.GET('/api/...')`, `requests.post(url)`, `fetch(url, { method })`,
    /// `Operation("get", "/api/...", ("a", "b"))`, and calls through clients.
    fn call(&mut self, node: Node<'_>) {
        let Some(function) = node.child_by_field_name("function") else {
            return;
        };
        let Some(arguments) = node.child_by_field_name("arguments") else {
            return;
        };
        let parts = self.chain_parts(function);
        self.client_call(&parts, node);
        let strings: Vec<Node<'_>> = {
            let mut cursor = arguments.walk();
            arguments
                .named_children(&mut cursor)
                .filter(|argument| self.string_value(*argument).is_some())
                .collect()
        };
        let path_argument = strings.iter().copied().find(|argument| {
            self.string_value(*argument)
                .is_some_and(|text| text.contains("/api/"))
        });
        let Some(path_node) = path_argument else {
            return;
        };
        let template = self.string_value(path_node).unwrap_or_default();
        let verb_argument = strings
            .iter()
            .filter_map(|argument| self.string_value(*argument))
            .find(|text| VERBS.contains(&text.to_ascii_lowercase().as_str()));
        if let Some(verb) = verb_argument {
            let chain = self.tuple_chain(arguments);
            self.add_method_path(Some(&verb), &template, chain, path_node);
            return;
        }
        let called = parts.last().map(String::as_str).unwrap_or_default();
        let method = if VERBS.contains(&called.to_ascii_lowercase().as_str()) {
            canonical_method(called)
        } else {
            self.method_option(arguments)
                .map_or_else(|| "*".to_owned(), |method| canonical_method(&method))
        };
        self.add_reference(&method, &template, path_node);
    }

    fn client_call(&mut self, parts: &[String], node: Node<'_>) {
        let receiver = parts
            .iter()
            .position(|part| self.options.client_receivers.contains(part));
        let Some(receiver) = receiver else { return };
        let chain = &parts[receiver + 1..];
        // `client.GET('/api/...')` is a typed fetch, reported as a path reference.
        let is_typed_fetch =
            chain.len() == 1 && VERBS.contains(&chain[0].to_ascii_lowercase().as_str());
        if chain.is_empty() || is_typed_fetch {
            return;
        }
        let location = self.location(node);
        self.facts.client_calls.push(ClientCall {
            chain: chain.join("."),
            location,
        });
    }

    fn add_method_path(
        &mut self,
        method: Option<&str>,
        template: &str,
        chain: Option<String>,
        path_node: Node<'_>,
    ) {
        let Some(path) = canonical_path(template) else {
            return;
        };
        self.consumed.push(path_node.id());
        let location = self.location(path_node);
        self.facts.method_paths.push(MethodPath {
            method: method.map_or_else(|| "*".to_owned(), canonical_method),
            path,
            template: template.to_owned(),
            chain,
            location,
        });
    }

    fn add_reference(&mut self, method: &str, template: &str, node: Node<'_>) {
        let Some(path) = canonical_path(template) else {
            return;
        };
        self.consumed.push(node.id());
        let location = self.location(node);
        self.facts.path_references.push(PathReference {
            method: method.to_owned(),
            path,
            location,
        });
    }

    /// Records a name used as a field: object keys, member names, keyword
    /// arguments, dictionary keys and subscripts.
    fn mention(&mut self, node: Node<'_>) {
        let name = match node.kind() {
            "pair" => node
                .child_by_field_name("key")
                .map(|key| self.text(key).trim_matches(['"', '\'']).to_owned()),
            "shorthand_property_identifier" | "shorthand_property_identifier_pattern" => {
                Some(self.text(node).to_owned())
            }
            "member_expression" => node
                .child_by_field_name("property")
                .map(|property| self.text(property).to_owned()),
            "attribute" => node
                .child_by_field_name("attribute")
                .map(|attribute| self.text(attribute).to_owned()),
            "keyword_argument" => node
                .child_by_field_name("name")
                .map(|name| self.text(name).to_owned()),
            "subscript" | "subscript_expression" => {
                let index =
                    node.child_by_field_name(if self.python { "subscript" } else { "index" });
                index.and_then(|index| self.string_value(index))
            }
            _ => None,
        };
        if let Some(name) = name.filter(|name| is_identifier(name)) {
            self.facts.mentions.push(name);
        }
    }

    /// A relative import's specifier: `'../types/x'` or `.models`.
    fn import(&mut self, node: Node<'_>) {
        let specifier = if self.python {
            node.child_by_field_name("module_name")
                .map(|module| self.text(module).to_owned())
        } else {
            node.child_by_field_name("source")
                .and_then(|source| self.string_value(source))
        };
        if let Some(specifier) = specifier.filter(|specifier| specifier.starts_with('.')) {
            self.facts.imports.push(specifier);
        }
    }

    /// Strings naming API paths that no object or call claimed.
    fn collect_strings(&mut self, node: Node<'_>) {
        let is_string = matches!(node.kind(), "string" | "template_string");
        if is_string && !self.consumed.contains(&node.id()) {
            if let Some(text) = self
                .string_value(node)
                .filter(|text| text.contains("/api/"))
            {
                let method = method_before_path(&text).unwrap_or_else(|| "*".to_owned());
                self.add_reference(&method, &text, node);
            }
            return;
        }
        let mut cursor = node.walk();
        for child in node.named_children(&mut cursor) {
            self.collect_strings(child);
        }
    }

    /// `this.client.sessionOptions.list` as its identifiers.
    fn chain_parts(&self, node: Node<'_>) -> Vec<String> {
        match node.kind() {
            "member_expression" | "attribute" => {
                let property_field = if self.python { "attribute" } else { "property" };
                let mut parts = node
                    .child_by_field_name("object")
                    .map(|object| self.chain_parts(object))
                    .unwrap_or_default();
                if let Some(property) = node.child_by_field_name(property_field) {
                    parts.push(self.text(property).to_owned());
                }
                parts
            }
            "identifier" | "this" | "property_identifier" => vec![self.text(node).to_owned()],
            _ => Vec::new(),
        }
    }

    /// The text of a string or template string, without quotes.
    fn string_value(&self, node: Node<'_>) -> Option<String> {
        match node.kind() {
            "string" | "template_string" => {
                let text = self.text(node);
                let text = text.trim_start_matches(['r', 'b', 'f', 'u', 'R', 'B', 'F', 'U']);
                Some(text.trim_matches(['"', '\'', '`']).to_owned())
            }
            _ => None,
        }
    }

    /// `['a', 'b']` as `a.b`.
    fn string_list(&self, node: Node<'_>) -> Option<String> {
        let mut cursor = node.walk();
        let parts: Vec<String> = node
            .named_children(&mut cursor)
            .filter_map(|child| self.string_value(child))
            .collect();
        (!parts.is_empty()).then(|| parts.join("."))
    }

    /// A Python call's tuple-of-strings argument, as a chain.
    fn tuple_chain(&self, arguments: Node<'_>) -> Option<String> {
        let mut cursor = arguments.walk();
        let tuple = arguments
            .named_children(&mut cursor)
            .find(|argument| matches!(argument.kind(), "tuple" | "array"))?;
        self.string_list(tuple)
    }

    /// `method: 'POST'` in an options object passed to a call.
    fn method_option(&self, arguments: Node<'_>) -> Option<String> {
        let mut cursor = arguments.walk();
        let options = arguments
            .named_children(&mut cursor)
            .find(|argument| argument.kind() == "object")?;
        let mut pairs = options.walk();
        let pair = options.named_children(&mut pairs).find(|pair| {
            pair.kind() == "pair"
                && pair
                    .child_by_field_name("key")
                    .is_some_and(|key| self.text(key) == "method")
        })?;
        self.string_value(pair.child_by_field_name("value")?)
    }

    fn location(&self, node: Node<'_>) -> Location {
        let line = u32::try_from(node.start_position().row).unwrap_or(u32::MAX);
        let column = u32::try_from(node.start_position().column).unwrap_or(u32::MAX);
        let point = lemma_model::Position { line, column };
        let symbol = self
            .symbols
            .iter()
            .filter(|symbol| symbol.span.start <= point && point < symbol.span.end)
            .min_by_key(|symbol| {
                (
                    symbol.span.end.line - symbol.span.start.line,
                    symbol.span.end.column,
                )
            })
            .map(|symbol| symbol.qualified_name.clone());
        Location {
            file: self.file.to_owned(),
            line: line + 1,
            symbol,
        }
    }

    fn text(&self, node: Node<'_>) -> &str {
        node.utf8_text(self.source.as_bytes()).unwrap_or_default()
    }
}

fn is_identifier(name: &str) -> bool {
    let mut characters = name.chars();
    characters
        .next()
        .is_some_and(|first| first.is_ascii_alphabetic() || first == '_')
        && characters.all(|c| c.is_ascii_alphanumeric() || c == '_')
}

#[cfg(test)]
mod tests {
    use super::*;

    fn facts(file: &str, source: &str) -> CodeFacts {
        let options = CodeOptions {
            client_receivers: vec!["client".to_owned(), "_client".to_owned()],
        };
        extract(file, source, &options, &mut Extractor::new().unwrap()).unwrap()
    }

    #[test]
    fn operation_tables_and_route_declarations_are_method_paths() {
        let source = "export const OPS = [\n  { method: 'get', path: '/api/v3.1/session_options', chain: ['sessionOptions', 'list'] },\n]\n";

        let found = facts("ops.ts", source);

        let item = &found.method_paths[0];
        assert_eq!(
            (item.method.as_str(), item.path.as_str()),
            ("GET", "/api/v3.1/session_options")
        );
        assert_eq!(item.chain.as_deref(), Some("sessionOptions.list"));
        assert_eq!(found.path_references, []);
    }

    #[test]
    fn typed_fetch_calls_carry_their_method_and_symbol() {
        let source = "export async function createSession(gateway) {\n  return gateway.POST(\"/api/v3.1/task_router/session\", { body: {} })\n}\n";

        let found = facts("a.ts", source);

        let reference = &found.path_references[0];
        assert_eq!(reference.method, "POST");
        assert_eq!(reference.path, "/api/v3.1/task_router/session");
        assert_eq!(reference.location.symbol.as_deref(), Some("createSession"));
        assert_eq!(reference.location.line, 2);
    }

    #[test]
    fn templates_and_fetch_options_are_understood() {
        let source = "const submit = (t) => fetch(`${BACKEND}/api/v3.1/internal/customer_accounts/link/${t}`, { method: 'POST' })\n";

        let found = facts("a.ts", source);

        let reference = &found.path_references[0];
        assert_eq!(reference.method, "POST");
        assert_eq!(
            reference.path,
            "/api/v3.1/internal/customer_accounts/link/{}"
        );
    }

    #[test]
    fn javascript_modules_are_read() {
        let source = "export async function provision(api) {\n  return api('/api/v3.1/api_keys', { method: 'POST' })\n}\n";

        let found = facts("scripts/provision.mjs", source);

        let reference = &found.path_references[0];
        assert_eq!(reference.path, "/api/v3.1/api_keys");
        assert_eq!(reference.location.symbol.as_deref(), Some("provision"));
    }

    #[test]
    fn calls_through_clients_are_chains() {
        let source = "class SessionOptions {\n  async list(p) {\n    return this.client.sessionOptions.list(p)\n  }\n}\n";

        let found = facts("SessionOptions.ts", source);

        let call = &found.client_calls[0];
        assert_eq!(call.chain, "sessionOptions.list");
        assert_eq!(call.location.symbol.as_deref(), Some("SessionOptions.list"));
    }

    #[test]
    fn python_operation_tables_and_client_calls_are_read() {
        let source = "OPERATIONS = (\n    Operation(\"get\", \"/api/v3.1/session_options\", (\"session_options\", \"list\")),\n)\n\ndef run(self):\n    return self._client.session_options.list()\n";

        let found = facts("_operations.py", source);

        assert_eq!(
            found.method_paths[0].chain.as_deref(),
            Some("session_options.list")
        );
        assert_eq!(found.method_paths[0].method, "GET");
        assert_eq!(found.client_calls[0].chain, "session_options.list");
    }

    #[test]
    fn field_names_and_relative_imports_are_recorded() {
        let source = "import { Params } from '../types/configs'\nimport z from 'zod'\nconst p = { search: 'x', limit }\nconsole.log(p.cursor, p['archived'])\n";

        let found = facts("a.ts", source);

        for name in ["search", "limit", "cursor", "archived"] {
            assert!(found.mentions.contains(&name.to_owned()), "{name}");
        }
        assert_eq!(found.imports, ["../types/configs"]);
    }

    #[test]
    fn python_keywords_and_relative_imports_are_recorded() {
        let source = "from .models import Hook\n\ndef f(client):\n    client.create(on_match='block', **{'prompt': 'x'})\n";

        let found = facts("a.py", source);

        assert!(found.mentions.contains(&"on_match".to_owned()));
        assert_eq!(found.imports, [".models"]);
    }

    #[test]
    fn other_strings_with_api_paths_are_references_of_unknown_method() {
        let source = "const PATH = \"/api/v3.1/internal/customer_accounts/link\";\n";

        let found = facts("a.ts", source);

        assert_eq!(found.path_references[0].method, "*");
    }
}
