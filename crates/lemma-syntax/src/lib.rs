//! Extracts symbol definitions from source text using Tree-sitter.
//!
//! Extraction is purely syntactic. It finds where things are defined and how
//! they nest, but does not resolve types, imports, or calls; those facts come
//! from semantic providers.

mod classify;
mod declaration;
mod nesting;
mod outline;
mod pool;
mod repair;

use lemma_model::{Language, Position, Span, Symbol};
use tree_sitter::{Node, Parser, Query, QueryCursor, QueryMatch, StreamingIterator};

pub use crate::declaration::declaration_text;
use crate::nesting::Definition;
pub use crate::outline::OutlinedSymbol;
pub use crate::pool::ExtractorPool;
pub use crate::repair::parse as parse_source;

/// Changes whenever extraction output could change, so snapshots built by an
/// older extractor get a different fingerprint.
pub const EXTRACTOR_VERSION: &str = "3";

/// Why the extractor could not be set up or could not parse a file.
#[derive(Debug, thiserror::Error)]
pub enum SyntaxError {
    /// The bundled grammar does not match the Tree-sitter runtime.
    #[error("tree-sitter grammar for {language} is incompatible with this build")]
    Grammar {
        /// Language whose grammar failed to load.
        language: Language,
        /// The runtime's explanation.
        #[source]
        source: tree_sitter::LanguageError,
    },
    /// A bundled definition query does not compile against its grammar.
    #[error("definition query for {language} is invalid")]
    Query {
        /// Language whose query failed.
        language: Language,
        /// Where and why the query failed.
        #[source]
        source: tree_sitter::QueryError,
    },
    /// A bundled definition query is missing a capture the extractor needs.
    #[error("definition query for {language} has no @{capture} capture")]
    MissingCapture {
        /// Language whose query is incomplete.
        language: Language,
        /// The capture that was expected.
        capture: &'static str,
    },
    /// The parser stopped without producing a tree.
    #[error("parser gave up on {language} source")]
    ParseAborted {
        /// Language being parsed.
        language: Language,
    },
}

/// The symbols found in one file.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Extraction {
    /// Definitions in source order.
    pub symbols: Vec<Symbol>,
    /// Whether the parser recovered from syntax errors. Symbols near an error
    /// may be missing.
    pub has_syntax_errors: bool,
}

/// One file's definitions and what each is made of.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Outline {
    /// Definitions in source order.
    pub symbols: Vec<OutlinedSymbol>,
    /// Whether the parser recovered from syntax errors.
    pub has_syntax_errors: bool,
}

/// Parses source files and pulls out their definitions.
///
/// Holds one parser and compiled query per language, so reuse a single
/// extractor across many files.
pub struct Extractor {
    typescript: Grammar,
    tsx: Grammar,
    python: Grammar,
    go: Grammar,
    rust: Grammar,
}

impl Extractor {
    /// Loads every grammar and compiles its definition query.
    pub fn new() -> Result<Self, SyntaxError> {
        Ok(Self {
            typescript: Grammar::load(Language::TypeScript)?,
            tsx: Grammar::load(Language::Tsx)?,
            python: Grammar::load(Language::Python)?,
            go: Grammar::load(Language::Go)?,
            rust: Grammar::load(Language::Rust)?,
        })
    }

    /// Extracts the definitions from one file's source.
    pub fn extract(&mut self, language: Language, source: &str) -> Result<Extraction, SyntaxError> {
        let grammar = self.grammar_mut(language);
        let tree = repair::parse(&mut grammar.parser, language, source)
            .ok_or(SyntaxError::ParseAborted { language })?;

        let definitions = grammar.find_definitions(tree.root_node(), source);
        Ok(Extraction {
            symbols: nesting::into_symbols(definitions),
            has_syntax_errors: tree.root_node().has_error(),
        })
    }

    /// Extracts one file's definitions with what each is made of, from a
    /// single parse: the eager layer of the store (spec 17).
    pub fn outline(&mut self, language: Language, source: &str) -> Result<Outline, SyntaxError> {
        let grammar = self.grammar_mut(language);
        let tree = repair::parse(&mut grammar.parser, language, source)
            .ok_or(SyntaxError::ParseAborted { language })?;
        let root = tree.root_node();
        let symbols = nesting::into_symbols(grammar.find_definitions(root, source))
            .into_iter()
            .map(|symbol| outline::read_symbol(root, source, symbol))
            .collect();
        Ok(Outline {
            symbols,
            has_syntax_errors: root.has_error(),
        })
    }

    fn grammar_mut(&mut self, language: Language) -> &mut Grammar {
        match language {
            Language::TypeScript => &mut self.typescript,
            Language::Tsx => &mut self.tsx,
            Language::Python => &mut self.python,
            Language::Go => &mut self.go,
            Language::Rust => &mut self.rust,
        }
    }
}

/// A parser and definition query for one language.
struct Grammar {
    parser: Parser,
    query: Query,
    definition_capture: u32,
    name_capture: u32,
}

impl Grammar {
    fn load(language: Language) -> Result<Self, SyntaxError> {
        let grammar = tree_sitter_language(language);
        let mut parser = Parser::new();
        parser
            .set_language(&grammar)
            .map_err(|source| SyntaxError::Grammar { language, source })?;

        let query = Query::new(&grammar, definition_query(language))
            .map_err(|source| SyntaxError::Query { language, source })?;
        let capture_index = |capture| {
            query
                .capture_index_for_name(capture)
                .ok_or(SyntaxError::MissingCapture { language, capture })
        };
        let definition_capture = capture_index("definition")?;
        let name_capture = capture_index("name")?;

        Ok(Self {
            parser,
            query,
            definition_capture,
            name_capture,
        })
    }

    fn find_definitions(&self, root: Node<'_>, source: &str) -> Vec<Definition> {
        let mut cursor = QueryCursor::new();
        let mut matches = cursor.matches(&self.query, root, source.as_bytes());
        let mut definitions = Vec::new();
        while let Some(query_match) = matches.next() {
            if let Some(definition) = self.to_definition(query_match, source) {
                definitions.push(definition);
            }
        }
        definitions
    }

    fn to_definition(&self, query_match: &QueryMatch<'_, '_>, source: &str) -> Option<Definition> {
        let definition = captured_node(query_match, self.definition_capture)?;
        let name = captured_node(query_match, self.name_capture)?;
        let kind = classify::kind_of(definition, source)?;

        Some(Definition {
            name: source[name.byte_range()].to_owned(),
            receiver: classify::receiver_type(definition, source),
            kind,
            span: span_of(definition),
            name_span: span_of(name),
            byte_range: definition.byte_range(),
        })
    }
}

fn tree_sitter_language(language: Language) -> tree_sitter::Language {
    match language {
        Language::TypeScript => tree_sitter_typescript::LANGUAGE_TYPESCRIPT.into(),
        Language::Tsx => tree_sitter_typescript::LANGUAGE_TSX.into(),
        Language::Python => tree_sitter_python::LANGUAGE.into(),
        Language::Go => tree_sitter_go::LANGUAGE.into(),
        Language::Rust => tree_sitter_rust::LANGUAGE.into(),
    }
}

fn definition_query(language: Language) -> &'static str {
    match language {
        Language::TypeScript | Language::Tsx => include_str!("../queries/typescript.scm"),
        Language::Python => include_str!("../queries/python.scm"),
        Language::Go => include_str!("../queries/go.scm"),
        Language::Rust => include_str!("../queries/rust.scm"),
    }
}

fn captured_node<'tree>(query_match: &QueryMatch<'_, 'tree>, index: u32) -> Option<Node<'tree>> {
    query_match
        .captures
        .iter()
        .find(|capture| capture.index == index)
        .map(|capture| capture.node)
}

fn span_of(node: Node<'_>) -> Span {
    Span {
        start: position_of(node.start_position()),
        end: position_of(node.end_position()),
    }
}

fn position_of(point: tree_sitter::Point) -> Position {
    // Source files beyond u32::MAX lines or bytes per line are not realistic;
    // saturate rather than wrap if one ever appears.
    Position {
        line: u32::try_from(point.row).unwrap_or(u32::MAX),
        column: u32::try_from(point.column).unwrap_or(u32::MAX),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use lemma_model::SymbolKind;

    fn extract(language: Language, source: &str) -> Vec<(String, SymbolKind)> {
        let mut extractor = Extractor::new().unwrap();
        extractor
            .extract(language, source)
            .unwrap()
            .symbols
            .into_iter()
            .map(|symbol| (symbol.qualified_name, symbol.kind))
            .collect()
    }

    fn named(pairs: &[(&str, SymbolKind)]) -> Vec<(String, SymbolKind)> {
        pairs
            .iter()
            .map(|(name, kind)| ((*name).to_owned(), *kind))
            .collect()
    }

    #[test]
    fn typescript_definitions_are_qualified_by_their_class() {
        let source = r"
export class TaskRouter {
  async delete(id: string) {}
}
export const deleteSession = async (id: string) => {};
const RETRIES = 3;
interface Options { id: string }
type Id = string;
enum Mode { A }
function helper() { const local = 1; }
";
        assert_eq!(
            extract(Language::TypeScript, source),
            named(&[
                ("TaskRouter", SymbolKind::Class),
                ("TaskRouter.delete", SymbolKind::Method),
                ("deleteSession", SymbolKind::Function),
                ("RETRIES", SymbolKind::Constant),
                ("Options", SymbolKind::Interface),
                ("Options.id", SymbolKind::Field),
                ("Id", SymbolKind::TypeAlias),
                ("Mode", SymbolKind::Enum),
                ("helper", SymbolKind::Function),
            ])
        );
    }

    #[test]
    fn tsx_uses_the_typescript_query() {
        let source = "export function Button() { return <button />; }\n";
        assert_eq!(
            extract(Language::Tsx, source),
            named(&[("Button", SymbolKind::Function)])
        );
    }

    #[test]
    fn python_functions_inside_classes_become_methods() {
        let source = r"
TIMEOUT = 30
client = None

class TaskRouter:
    def delete(self, session_id):
        def inner():
            pass

@decorator
def delete_task_router_session(client, session_id):
    local = 1
";
        assert_eq!(
            extract(Language::Python, source),
            named(&[
                ("TIMEOUT", SymbolKind::Constant),
                ("client", SymbolKind::Variable),
                ("TaskRouter", SymbolKind::Class),
                ("TaskRouter.delete", SymbolKind::Method),
                ("TaskRouter.delete.inner", SymbolKind::Function),
                ("delete_task_router_session", SymbolKind::Function),
            ])
        );
    }

    #[test]
    fn python_class_attributes_are_fields_of_their_class() {
        let source = r#"
class AuthSchemaField(BaseModel):
    name: str
    required: bool = False
    KIND = "field"

    def describe(self):
        local = 1
"#;

        let found = extract(Language::Python, source);

        assert_eq!(
            found,
            named(&[
                ("AuthSchemaField", SymbolKind::Class),
                ("AuthSchemaField.name", SymbolKind::Field),
                ("AuthSchemaField.required", SymbolKind::Field),
                ("AuthSchemaField.KIND", SymbolKind::Field),
                ("AuthSchemaField.describe", SymbolKind::Method),
            ])
        );
    }

    #[test]
    fn go_struct_fields_and_typescript_class_fields_are_fields() {
        let go = "package p\ntype Item struct {\n\tName string\n}\n";
        let typescript =
            "class Item {\n  name: string = ''\n}\nfunction f(p: { inline: string }) {}\n";

        let go_found = extract(Language::Go, go);
        let typescript_found = extract(Language::TypeScript, typescript);

        assert!(go_found.contains(&("Item.Name".to_owned(), SymbolKind::Field)));
        assert!(typescript_found.contains(&("Item.name".to_owned(), SymbolKind::Field)));
        assert!(
            !typescript_found
                .iter()
                .any(|(name, _)| name.ends_with("inline"))
        );
    }

    #[test]
    fn rust_methods_are_qualified_by_their_impl_type() {
        let source = r"
pub struct Router { routes: Vec<Route> }
pub trait Service { fn call(&self); }
impl Service for Router<'_> {
    fn call(&self) { fn helper() {} }
}
pub enum Mode { A }
const RETRIES: u32 = 3;
pub fn start() {}
";
        assert_eq!(
            extract(Language::Rust, source),
            named(&[
                ("Router", SymbolKind::Struct),
                ("Router.routes", SymbolKind::Field),
                ("Service", SymbolKind::Interface),
                ("Service.call", SymbolKind::Method),
                ("Router.call", SymbolKind::Method),
                ("Router.call.helper", SymbolKind::Function),
                ("Mode", SymbolKind::Enum),
                ("RETRIES", SymbolKind::Constant),
                ("start", SymbolKind::Function),
            ])
        );
    }

    #[test]
    fn go_methods_are_qualified_by_their_receiver() {
        let source = r"
package router

const DefaultTimeout = 30
var client *Client

type Router struct{}
type Deleter interface{ Delete() }
type ID = string
type Count int

func (r *Router) Delete(id ID) error { return nil }
func New() *Router { return &Router{} }
";
        assert_eq!(
            extract(Language::Go, source),
            named(&[
                ("DefaultTimeout", SymbolKind::Constant),
                ("client", SymbolKind::Variable),
                ("Router", SymbolKind::Struct),
                ("Deleter", SymbolKind::Interface),
                ("Deleter.Delete", SymbolKind::Method),
                ("ID", SymbolKind::TypeAlias),
                ("Count", SymbolKind::TypeAlias),
                ("Router.Delete", SymbolKind::Method),
                ("New", SymbolKind::Function),
            ])
        );
    }

    #[test]
    fn interface_methods_are_qualified_by_their_interface() {
        let typescript = "interface Shape {\n  area(): number;\n}\nabstract class Base {\n  abstract size(): number;\n}\n";
        let go = "package shapes\n\ntype Shape interface {\n\tArea() float64\n}\n";

        let typescript_symbols = extract(Language::TypeScript, typescript);
        let go_symbols = extract(Language::Go, go);

        assert_eq!(
            typescript_symbols,
            named(&[
                ("Shape", SymbolKind::Interface),
                ("Shape.area", SymbolKind::Method),
                ("Base", SymbolKind::Class),
                ("Base.size", SymbolKind::Method),
            ])
        );
        assert_eq!(
            go_symbols,
            named(&[
                ("Shape", SymbolKind::Interface),
                ("Shape.Area", SymbolKind::Method),
            ])
        );
    }

    #[test]
    fn name_span_points_at_the_identifier() {
        let mut extractor = Extractor::new().unwrap();
        let extraction = extractor
            .extract(Language::Python, "def run():\n    pass\n")
            .unwrap();

        let name_span = extraction.symbols[0].name_span;
        assert_eq!(name_span.start, Position { line: 0, column: 4 });
        assert_eq!(name_span.end, Position { line: 0, column: 7 });
    }

    #[test]
    fn syntax_errors_are_reported_without_losing_other_symbols() {
        let mut extractor = Extractor::new().unwrap();
        let extraction = extractor
            .extract(Language::Python, "def ok():\n    pass\n\ndef (:\n")
            .unwrap();

        assert!(extraction.has_syntax_errors);
        assert_eq!(extraction.symbols[0].name, "ok");
    }
}
