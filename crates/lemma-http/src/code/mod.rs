//! HTTP facts in TypeScript, JavaScript, Python, Go and Rust source, read
//! with Tree-sitter.
//!
//! The extractor reports what the code says, not what it means: an object
//! with `method` and `path` is an operation declaration in a service, an
//! operation table in a generated client and a request descriptor in a
//! consumer. The caller interprets it by the repository's role.

mod go;
mod rust;
mod zod;

use std::collections::BTreeMap;

pub use zod::{SchemaUnion, SchemaUse};

use lemma_model::{Language, Symbol, SymbolKind};
use lemma_syntax::Extractor;
use tree_sitter::{Node, Parser};

use crate::paths::{
    canonical_method, canonical_path, canonical_route, has_marker, method_before_path,
};

/// Where in the code a fact was found.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, serde::Serialize, serde::Deserialize)]
pub struct Location {
    /// File path, relative to the repository root.
    pub file: String,
    /// Line, counting from 1.
    pub line: u32,
    /// The innermost definition containing it, by qualified name.
    pub symbol: Option<String>,
    /// The innermost function or method containing it, by qualified name;
    /// `None` outside functions, as in a table of constants.
    pub function: Option<String>,
}

/// An object or call naming an HTTP method and path together.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, serde::Serialize, serde::Deserialize)]
pub struct MethodPath {
    /// Upper-case method, or `*` if not given.
    pub method: String,
    /// Canonical path.
    pub path: String,
    /// The path as written.
    pub template: String,
    /// Where.
    pub location: Location,
}

/// A string naming an API path outside a method-and-path object.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, serde::Serialize, serde::Deserialize)]
pub struct PathReference {
    /// Upper-case method if the surrounding call shows it, else `*`.
    pub method: String,
    /// Canonical path.
    pub path: String,
    /// Where.
    pub location: Location,
}

/// A call through a client object, such as `this.client.invoices.list()`.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, serde::Serialize, serde::Deserialize)]
pub struct ClientCall {
    /// The chain after the client receiver: `invoices.list`.
    pub chain: String,
    /// Where.
    pub location: Location,
}

/// A call through a chain of at least three names, such as
/// `this.client.authConfigs.list()`, or through a name an import binds
/// (`api.listTools()`, `listTools()`): what a call through a client looks
/// like when the client is not known in advance.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, serde::Serialize, serde::Deserialize)]
pub struct MemberCall {
    /// The names, root first: `["this", "client", "authConfigs", "list"]`.
    /// A chain starting from a call's result has `()` as its root.
    pub parts: Vec<String>,
    /// Where.
    pub location: Location,
}

/// A name an import binds, and where from.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, serde::Serialize, serde::Deserialize)]
pub struct ImportBinding {
    /// The name the file uses: `c` in `import { b as c } from 'x'`.
    pub local: String,
    /// The name the module exports it as (`b`); `None` for a namespace or
    /// default import, which binds the module itself.
    pub imported: Option<String>,
    /// The module, as written.
    pub module: String,
}

/// A module re-exported whole under a name: `export * as api from './gen'`.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, serde::Serialize, serde::Deserialize)]
pub struct Reexport {
    /// The name it is exported as.
    pub name: String,
    /// The module, as written.
    pub module: String,
}

/// A name the code uses as a field.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, serde::Serialize, serde::Deserialize)]
pub struct Mention {
    /// The name.
    pub name: String,
    /// The innermost function or method it is in, by qualified name; `None`
    /// outside functions, as in a schema, type or constant.
    pub function: Option<String>,
}

/// Everything found in one file.
#[derive(Debug, Clone, Default, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct CodeFacts {
    /// Method-and-path objects and calls.
    pub method_paths: Vec<MethodPath>,
    /// Other strings naming API paths.
    pub path_references: Vec<PathReference>,
    /// Calls through client objects.
    pub client_calls: Vec<ClientCall>,
    /// Calls through chains of at least three names, or starting from an
    /// imported name.
    pub member_calls: Vec<MemberCall>,
    /// The names imports bind in the file. Sorted, unique.
    pub imported_names: Vec<String>,
    /// The names imports bind, with their modules. Sorted, unique.
    pub import_bindings: Vec<ImportBinding>,
    /// Modules re-exported whole under a name. Sorted, unique.
    pub reexports: Vec<Reexport>,
    /// Every module imported, as written, packages included
    /// (`@acme/client`, `acme_client.types`). Sorted, unique.
    pub imported_modules: Vec<String>,
    /// Names the file uses as object keys, member names, keyword arguments
    /// or dictionary keys: the fields it may send or read. Sorted, unique.
    pub mentions: Vec<Mention>,
    /// Import specifiers as written: relative ones (`../types/x`, `.models`)
    /// and, outside Python, bare ones that may be path aliases (`@/lib/x`).
    pub imports: Vec<String>,
    /// The schemas routes use for their request body and query.
    pub schema_uses: Vec<SchemaUse>,
    /// Schemas defined here that reject keys they do not list.
    pub strict_schemas: Vec<String>,
    /// Unions of schemas defined here.
    pub schema_unions: Vec<SchemaUnion>,
}

/// What to look for.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct CodeOptions {
    /// Names of client receivers, such as `client` or `_client`; calls
    /// through them are reported as client calls.
    pub client_receivers: Vec<String>,
    /// Modules of generated clients, such as `generated/billing`: functions
    /// imported from a module whose specifier ends with one are client
    /// methods, so calling them is a client call.
    pub client_modules: Vec<String>,
    /// What makes a string a path: leading segments the services being
    /// checked serve, such as `/v1/` (see [`crate::path_markers`]). Paths
    /// written beside a method, as in route declarations, need none.
    pub path_markers: Vec<String>,
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

/// `facts` with every list sorted and, where order carries no meaning,
/// without repeats, so the same file always gives the same facts.
fn sorted(mut facts: CodeFacts) -> CodeFacts {
    facts.method_paths.sort();
    facts.path_references.sort();
    facts.client_calls.sort();
    facts.mentions.sort();
    facts.mentions.dedup();
    facts.imports.sort();
    facts.imports.dedup();
    facts.imported_names = facts
        .import_bindings
        .iter()
        .map(|binding| binding.local.clone())
        .collect();
    facts.imported_names.sort();
    facts.imported_names.dedup();
    facts.import_bindings.sort();
    facts.import_bindings.dedup();
    facts.reexports.sort();
    facts.reexports.dedup();
    facts.imported_modules.sort();
    facts.imported_modules.dedup();
    facts.schema_uses.sort();
    facts.strict_schemas.sort();
    facts.strict_schemas.dedup();
    facts.schema_unions.sort();
    facts
}

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
        dialect: Dialect::of(language),
        facts: CodeFacts::default(),
        consumed: Vec::new(),
        prefixes: BTreeMap::new(),
        client_functions: BTreeMap::new(),
        examples: 0,
    };
    walk.visit(tree.root_node());
    walk.collect_strings(tree.root_node());
    Ok(sorted(walk.facts))
}

/// The grammar to parse a file with. JavaScript has no language of its own
/// in Lemma's model; the TSX grammar parses it, JSX included, because
/// TypeScript's syntax is a superset of it.
fn parse_language(extension: &str) -> Option<Language> {
    if matches!(extension, "js" | "jsx" | "mjs" | "cjs") {
        return Some(Language::Tsx);
    }
    Language::from_extension(extension)
}

fn tree_sitter_language(language: Language) -> tree_sitter::Language {
    match language {
        Language::Tsx => tree_sitter_typescript::LANGUAGE_TSX.into(),
        Language::Python => tree_sitter_python::LANGUAGE.into(),
        Language::Go => tree_sitter_go::LANGUAGE.into(),
        Language::Rust => tree_sitter_rust::LANGUAGE.into(),
        Language::TypeScript => tree_sitter_typescript::LANGUAGE_TYPESCRIPT.into(),
    }
}

/// Which family of syntax a file is in, for the few node kinds and fields
/// that differ between grammars.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Dialect {
    /// TypeScript, TSX and JavaScript.
    Script,
    Python,
    Go,
    Rust,
}

impl Dialect {
    fn of(language: Language) -> Self {
        match language {
            Language::Python => Self::Python,
            Language::Go => Self::Go,
            Language::Rust => Self::Rust,
            Language::TypeScript | Language::Tsx => Self::Script,
        }
    }
}

struct Walk<'a> {
    file: &'a str,
    source: &'a str,
    symbols: &'a [Symbol],
    options: &'a CodeOptions,
    dialect: Dialect,
    facts: CodeFacts,
    /// Strings already reported as part of a method-and-path or call.
    consumed: Vec<usize>,
    /// Go router variables and the path prefix each adds, from
    /// `api := r.PathPrefix("/api").Subrouter()`.
    prefixes: BTreeMap<String, String>,
    /// Functions imported from a generated client's module, by local name,
    /// with the name the client exports them under.
    client_functions: BTreeMap<String, String>,
    /// How many `example` or `examples` values the walk is inside: their
    /// objects are sample data in a document, not routes or requests.
    examples: usize,
}

/// Keys whose value is sample data in an API document.
const EXAMPLE_KEYS: [&str; 2] = ["example", "examples"];

/// Keys that name the host a request goes to: an object with one describes
/// a request to another server (such as one being signed), never a route.
const HOST_KEYS: [&str; 4] = ["host", "hostname", "protocol", "port"];

impl Walk<'_> {
    fn visit(&mut self, node: Node<'_>) {
        match node.kind() {
            "object" => self.object(node),
            "variable_declarator" => self.zod_definition(node),
            "call_expression" | "call" => self.call(node),
            "import_statement" | "import_from_statement" => self.import(node),
            "export_statement" => self.export(node),
            "short_var_declaration" | "assignment_statement" => self.go_router_prefix(node),
            "attribute_item" if self.dialect == Dialect::Rust => self.rust_route_attribute(node),
            "use_declaration" => self.rust_use(node),
            _ => {}
        }
        self.mention(node);
        let is_example = self.is_example_pair(node);
        self.examples += usize::from(is_example);
        let mut cursor = node.walk();
        for child in node.named_children(&mut cursor) {
            self.visit(child);
        }
        self.examples -= usize::from(is_example);
    }

    /// Whether `node` is a pair whose key is `example` or `examples`.
    fn is_example_pair(&self, node: Node<'_>) -> bool {
        if node.kind() != "pair" {
            return false;
        }
        node.child_by_field_name("key")
            .is_some_and(|key| EXAMPLE_KEYS.contains(&self.text(key).trim_matches(['"', '\''])))
    }

    /// `{ method: 'get', path: '/v1/users' }`: a route declaration in a
    /// service, a request descriptor in a consumer.
    fn object(&mut self, node: Node<'_>) {
        if self.examples > 0 {
            return;
        }
        let mut method = None;
        let mut path = None;
        let mut request = None;
        let mut names_a_host = false;
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
                "request" => request = Some(value),
                key => names_a_host |= HOST_KEYS.contains(&key),
            }
        }
        let Some(path_node) = path else { return };
        let Some(template) = self.string_value(path_node) else {
            return;
        };
        if names_a_host {
            let method = method
                .as_deref()
                .map_or_else(|| "*".to_owned(), canonical_method);
            self.add_reference(&method, &template, path_node);
            return;
        }
        if let (Some(method), Some(request)) = (method.as_deref(), request) {
            self.zod_route_request(method, &template, request);
        }
        self.add_method_path(method.as_deref(), &template, path_node);
    }

    /// `client.GET('/v1/x')`, `requests.post(url)`, `fetch(url, { method })`,
    /// `http.NewRequest("POST", url)`, and calls through clients.
    fn call(&mut self, node: Node<'_>) {
        let Some(function) = node.child_by_field_name("function") else {
            return;
        };
        let Some(arguments) = node.child_by_field_name("arguments") else {
            return;
        };
        let parts = self.chain_parts(function);
        if self.loads_module(function, arguments) {
            return;
        }
        self.member_call(function, &parts, node);
        self.client_call(&parts, node);
        if self.dialect == Dialect::Go && self.go_call(&parts, function, arguments) {
            return;
        }
        if self.dialect == Dialect::Rust && self.rust_call(&parts, arguments) {
            return;
        }
        let strings: Vec<Node<'_>> = {
            let mut cursor = arguments.walk();
            arguments
                .named_children(&mut cursor)
                .filter(|argument| self.string_value(*argument).is_some())
                .collect()
        };
        let path_argument = strings
            .iter()
            .copied()
            .find(|argument| {
                self.string_value(*argument)
                    .is_some_and(|text| has_marker(&text, &self.options.path_markers))
            })
            .or_else(|| self.nested_path_argument(arguments));
        let Some(path_node) = path_argument.filter(|node| !self.consumed.contains(&node.id()))
        else {
            return;
        };
        let template = self.string_value(path_node).unwrap_or_default();
        let verb_argument = strings
            .iter()
            .filter_map(|argument| self.string_value(*argument))
            .find(|text| VERBS.contains(&text.to_ascii_lowercase().as_str()))
            .or_else(|| self.go_method_constant(arguments))
            .or_else(|| self.rust_method_constant(arguments));
        // A method argument beside the path builds a request:
        // `http.NewRequest("POST", url)`, `request("GET", "/v1/x")`.
        if let Some(verb) = verb_argument {
            self.add_reference(&canonical_method(&verb), &template, path_node);
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

    /// Records a call through a chain of at least three names.
    fn member_call(&mut self, function: Node<'_>, parts: &[String], node: Node<'_>) {
        let mut chain = Vec::with_capacity(parts.len() + 1);
        if Self::starts_from_call(function) {
            chain.push("()".to_owned());
        }
        chain.extend_from_slice(parts);
        let starts_from_import = chain.first().is_some_and(|root| {
            self.facts
                .import_bindings
                .iter()
                .any(|binding| &binding.local == root)
        });
        if chain.len() < 3 && !starts_from_import {
            return;
        }
        let location = self.location(node);
        self.facts.member_calls.push(MemberCall {
            parts: chain,
            location,
        });
    }

    /// Whether the leftmost object of a member chain is a call's result.
    fn starts_from_call(node: Node<'_>) -> bool {
        let mut current = node;
        loop {
            let inner = ["object", "operand", "value"]
                .iter()
                .find_map(|field| current.child_by_field_name(field));
            let Some(inner) = inner else {
                return matches!(current.kind(), "call_expression" | "call");
            };
            current = inner;
        }
    }

    /// Whether a call is `import('./x')` or `require('./x')`, which loads a
    /// module rather than requesting a path; its arguments are consumed.
    fn loads_module(&mut self, function: Node<'_>, arguments: Node<'_>) -> bool {
        if !matches!(self.text(function), "import" | "require") {
            return false;
        }
        let mut cursor = arguments.walk();
        let loaded: Vec<usize> = arguments
            .named_children(&mut cursor)
            .map(|argument| argument.id())
            .collect();
        self.consumed.extend(loaded);
        true
    }

    fn client_call(&mut self, parts: &[String], node: Node<'_>) {
        if let [name] = parts {
            if let Some(exported) = self.client_functions.get(name) {
                let location = self.location(node);
                self.facts.client_calls.push(ClientCall {
                    chain: exported.clone(),
                    location,
                });
                return;
            }
        }
        let receiver = parts
            .iter()
            .position(|part| self.options.client_receivers.contains(part));
        let Some(receiver) = receiver else { return };
        let chain = &parts[receiver + 1..];
        // `client.GET('/v1/x')` is a typed fetch, reported as a path reference.
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

    fn add_method_path(&mut self, method: Option<&str>, template: &str, path_node: Node<'_>) {
        let method = method.map_or_else(|| "*".to_owned(), canonical_method);
        // A path written beside its method is surely a path; alone it needs
        // a marker, since objects with a `path` or `url` are common.
        let path = if method == "*" {
            canonical_path(template, &self.options.path_markers)
        } else {
            canonical_route(template)
        };
        let Some(path) = path else {
            return;
        };
        self.consumed.push(path_node.id());
        let location = self.location(path_node);
        self.facts.method_paths.push(MethodPath {
            method,
            path,
            template: template.to_owned(),
            location,
        });
    }

    fn add_reference(&mut self, method: &str, template: &str, node: Node<'_>) {
        let Some(path) = canonical_path(template, &self.options.path_markers) else {
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
                let index = node.child_by_field_name(if self.dialect == Dialect::Python {
                    "subscript"
                } else {
                    "index"
                });
                index.and_then(|index| self.string_value(index))
            }
            _ if self.dialect == Dialect::Rust => self.rust_field_name(node),
            "field_declaration" | "keyed_element" => self.go_field_name(node),
            _ => None,
        };
        if let Some(name) = name.filter(|name| is_identifier(name)) {
            let function = self.enclosing_function(node);
            self.facts.mentions.push(Mention { name, function });
        }
    }

    /// A relative import's specifier: `'../types/x'` or `.models`. Names
    /// imported from a generated client's module are remembered as client
    /// functions.
    fn import(&mut self, node: Node<'_>) {
        self.consume_source(node);
        let specifier = if self.dialect == Dialect::Python {
            node.child_by_field_name("module_name")
                .map(|module| self.text(module).to_owned())
        } else {
            node.child_by_field_name("source")
                .and_then(|source| self.string_value(source))
        };
        // `import m.n as o` names its modules in its bindings.
        self.import_bindings(node, specifier.as_deref().unwrap_or_default());
        let Some(specifier) = specifier else { return };
        self.facts.imported_modules.push(specifier.clone());
        let is_client_module = self
            .options
            .client_modules
            .iter()
            .any(|module| specifier.ends_with(module.as_str()));
        if is_client_module {
            self.client_imports(node);
        }
        // Python imports outside the package are other packages; a script's
        // bare specifier may be a path alias (`tsconfig` `paths`), resolved
        // or dropped when the repository's files are known.
        if specifier.starts_with('.') || self.dialect != Dialect::Python {
            self.facts.imports.push(specifier);
        }
    }

    /// `export * as api from './gen'` re-exports a module; any export with a
    /// source names a module, not a request.
    fn export(&mut self, node: Node<'_>) {
        self.consume_source(node);
        let Some(module) = node
            .child_by_field_name("source")
            .and_then(|source| self.string_value(source))
        else {
            return;
        };
        let mut cursor = node.walk();
        let namespace = node
            .named_children(&mut cursor)
            .find(|child| child.kind() == "namespace_export")
            .and_then(|namespace| namespace.named_child(0));
        if let Some(name) = namespace {
            let name = self.text(name).trim_matches(['"', '\'']).to_owned();
            self.facts.reexports.push(Reexport { name, module });
        }
    }

    /// The names an import binds, with what each is in `module`: `a`
    /// (default), `c` (`b`) and `ns` (the module) in `import a, { b as c }
    /// from 'x'` and `import * as ns from 'y'`; `p` and `q` (`r`) in
    /// `from z import p, r as q`.
    fn import_bindings(&mut self, node: Node<'_>, module: &str) {
        let mut stack = vec![node];
        while let Some(current) = stack.pop() {
            if let Some(binding) = self.binding(current, node, module) {
                self.facts.import_bindings.push(binding);
            }
            let mut cursor = current.walk();
            stack.extend(current.named_children(&mut cursor));
        }
    }

    /// The binding a node of an import statement makes. An import without a
    /// `from` (`import m.n as o`) binds the module its node names.
    fn binding(
        &self,
        current: Node<'_>,
        statement: Node<'_>,
        module: &str,
    ) -> Option<ImportBinding> {
        let (local, imported) = self.bound_name(current, statement)?;
        if module.is_empty() {
            return Some(ImportBinding {
                local,
                module: imported.unwrap_or_default(),
                imported: None,
            });
        }
        Some(ImportBinding {
            local,
            imported,
            module: module.to_owned(),
        })
    }

    /// The name a node of an import statement binds, and what it is in the
    /// module (`None` for the module itself).
    fn bound_name(
        &self,
        current: Node<'_>,
        statement: Node<'_>,
    ) -> Option<(String, Option<String>)> {
        let named = |node: Node<'_>| self.text(node).to_owned();
        match current.kind() {
            "import_specifier" | "aliased_import" => {
                let name = current.child_by_field_name("name")?;
                let local = current.child_by_field_name("alias").unwrap_or(name);
                Some((last_segment(&named(local)), Some(named(name))))
            }
            "namespace_import" => Some((named(current.named_child(0)?), None)),
            "import_clause" => {
                let mut cursor = current.walk();
                let default = current
                    .named_children(&mut cursor)
                    .find(|child| child.kind() == "identifier")?;
                Some((named(default), None))
            }
            "dotted_name"
                if current
                    .parent()
                    .is_some_and(|parent| parent.id() == statement.id()) =>
            {
                let is_module = statement
                    .child_by_field_name("module_name")
                    .is_some_and(|module| module.id() == current.id());
                if is_module {
                    return None;
                }
                let local = last_segment(&named(current));
                // `from z import p` binds `p` as exported; `import m.n` the module.
                let imported = (statement.kind() == "import_from_statement").then(|| local.clone());
                Some((local, imported))
            }
            _ => None,
        }
    }

    /// Marks a module specifier (`from './internal/x'`) as no request.
    fn consume_source(&mut self, node: Node<'_>) {
        if let Some(source) = node.child_by_field_name("source") {
            self.consumed.push(source.id());
        }
    }

    /// `import { a, b as c } from '...'`: `a` and `c` call `a` and `b`.
    fn client_imports(&mut self, node: Node<'_>) {
        let mut stack = vec![node];
        while let Some(current) = stack.pop() {
            if current.kind() == "import_specifier" {
                self.client_import(current);
                continue;
            }
            let mut cursor = current.walk();
            stack.extend(current.named_children(&mut cursor));
        }
    }

    fn client_import(&mut self, specifier: Node<'_>) {
        let Some(name) = specifier.child_by_field_name("name") else {
            return;
        };
        let exported = self.text(name).to_owned();
        let local = specifier
            .child_by_field_name("alias")
            .map_or_else(|| exported.clone(), |alias| self.text(alias).to_owned());
        self.client_functions.insert(local, exported);
    }

    /// Strings naming API paths that no object or call claimed.
    fn collect_strings(&mut self, node: Node<'_>) {
        let is_string = is_string_kind(node.kind());
        if is_string && !self.consumed.contains(&node.id()) {
            if let Some(text) = self
                .string_value(node)
                .filter(|text| has_marker(text, &self.options.path_markers))
            {
                let method = method_before_path(&text, &self.options.path_markers)
                    .unwrap_or_else(|| "*".to_owned());
                self.add_reference(&method, &text, node);
            }
            return;
        }
        let mut cursor = node.walk();
        for child in node.named_children(&mut cursor) {
            self.collect_strings(child);
        }
    }

    /// `this.client.invoices.list` as its identifiers.
    fn chain_parts(&self, node: Node<'_>) -> Vec<String> {
        match node.kind() {
            "selector_expression" => {
                let mut parts = node
                    .child_by_field_name("operand")
                    .map(|operand| self.chain_parts(operand))
                    .unwrap_or_default();
                if let Some(field) = node.child_by_field_name("field") {
                    parts.push(self.text(field).to_owned());
                }
                parts
            }
            "member_expression" | "attribute" => {
                let property_field = if self.dialect == Dialect::Python {
                    "attribute"
                } else {
                    "property"
                };
                let mut parts = node
                    .child_by_field_name("object")
                    .map(|object| self.chain_parts(object))
                    .unwrap_or_default();
                if let Some(property) = node.child_by_field_name(property_field) {
                    parts.push(self.text(property).to_owned());
                }
                parts
            }
            "field_expression" => {
                let mut parts = node
                    .child_by_field_name("value")
                    .map(|value| self.chain_parts(value))
                    .unwrap_or_default();
                if let Some(field) = node.child_by_field_name("field") {
                    parts.push(self.text(field).to_owned());
                }
                parts
            }
            "scoped_identifier" => {
                let mut parts = node
                    .child_by_field_name("path")
                    .map(|path| self.chain_parts(path))
                    .unwrap_or_default();
                if let Some(name) = node.child_by_field_name("name") {
                    parts.push(self.text(name).to_owned());
                }
                parts
            }
            "identifier" | "this" | "self" | "property_identifier" => {
                vec![self.text(node).to_owned()]
            }
            _ => Vec::new(),
        }
    }

    /// The text of a string or template string, without quotes.
    fn string_value(&self, node: Node<'_>) -> Option<String> {
        match node.kind() {
            kind if is_string_kind(kind) => {
                let text = self.text(node);
                let text = text.trim_start_matches(['r', 'b', 'f', 'u', 'R', 'B', 'F', 'U']);
                Some(text.trim_matches(['"', '\'', '`']).to_owned())
            }
            _ => None,
        }
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
            function: self.enclosing_function(node),
        }
    }

    /// The innermost function or method containing a node.
    fn enclosing_function(&self, node: Node<'_>) -> Option<String> {
        let line = u32::try_from(node.start_position().row).unwrap_or(u32::MAX);
        let column = u32::try_from(node.start_position().column).unwrap_or(u32::MAX);
        let point = lemma_model::Position { line, column };
        self.symbols
            .iter()
            .filter(|symbol| matches!(symbol.kind, SymbolKind::Function | SymbolKind::Method))
            .filter(|symbol| symbol.span.start <= point && point < symbol.span.end)
            .min_by_key(|symbol| {
                (
                    symbol.span.end.line - symbol.span.start.line,
                    symbol.span.end.column,
                )
            })
            .map(|symbol| symbol.qualified_name.clone())
    }

    fn text(&self, node: Node<'_>) -> &str {
        node.utf8_text(self.source.as_bytes()).unwrap_or_default()
    }
}

fn is_string_kind(kind: &str) -> bool {
    matches!(
        kind,
        "string"
            | "template_string"
            | "interpreted_string_literal"
            | "raw_string_literal"
            | "string_literal"
    )
}

/// The last dot-separated part of a name: `n` in `m.n`.
fn last_segment(name: &str) -> String {
    name.rsplit('.').next().unwrap_or(name).to_owned()
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

    fn names(found: &CodeFacts) -> Vec<&str> {
        found
            .mentions
            .iter()
            .map(|mention| mention.name.as_str())
            .collect()
    }

    fn facts(file: &str, source: &str) -> CodeFacts {
        let options = CodeOptions {
            client_receivers: vec!["client".to_owned(), "_client".to_owned()],
            client_modules: vec!["generated/billing".to_owned()],
            path_markers: vec!["/v1/".to_owned()],
        };
        extract(file, source, &options, &mut Extractor::new().unwrap()).unwrap()
    }

    #[test]
    fn method_and_path_objects_are_method_paths() {
        let source = "export const ROUTES = [\n  { method: 'get', path: '/v1/users' },\n]\n";

        let found = facts("ops.ts", source);

        let item = &found.method_paths[0];
        assert_eq!(
            (item.method.as_str(), item.path.as_str()),
            ("GET", "/v1/users")
        );
        assert_eq!(found.path_references, []);
    }

    #[test]
    fn objects_in_examples_are_sample_data_not_method_paths() {
        let source = "export const route = createRoute({\n  method: 'patch',\n  path: '/v1/configs/{id}',\n  examples: { enable: { path: '/v1/configs/ac_1a2b/ENABLED', method: 'PATCH' } },\n})\n";

        let found = facts("route.ts", source);

        let paths: Vec<&str> = found
            .method_paths
            .iter()
            .map(|item| item.path.as_str())
            .collect();
        assert_eq!(paths, ["/v1/configs/{}"]);
    }

    #[test]
    fn a_request_to_another_host_is_a_call_not_a_method_path() {
        let source = "const request = await signer.sign({\n  method: 'GET',\n  protocol: 'http:',\n  hostname: host,\n  path: '/v1/token',\n})\n";

        let found = facts("iam.ts", source);

        assert_eq!(found.method_paths, []);
        let calls: Vec<(&str, &str)> = found
            .path_references
            .iter()
            .map(|reference| (reference.method.as_str(), reference.path.as_str()))
            .collect();
        assert_eq!(calls, [("GET", "/v1/token")]);
    }

    #[test]
    fn typed_fetch_calls_carry_their_method_and_symbol() {
        let source = "export async function createSession(api) {\n  return api.POST(\"/v1/sessions\", { body: {} })\n}\n";

        let found = facts("a.ts", source);

        let reference = &found.path_references[0];
        assert_eq!(reference.method, "POST");
        assert_eq!(reference.path, "/v1/sessions");
        assert_eq!(reference.location.symbol.as_deref(), Some("createSession"));
        assert_eq!(reference.location.line, 2);
    }

    #[test]
    fn templates_and_fetch_options_are_understood() {
        let source =
            "const submit = (t) => fetch(`${BASE}/v1/accounts/${t}/link`, { method: 'POST' })\n";

        let found = facts("a.ts", source);

        let reference = &found.path_references[0];
        assert_eq!(reference.method, "POST");
        assert_eq!(reference.path, "/v1/accounts/{}/link");
    }

    #[test]
    fn javascript_modules_are_read() {
        let source = "export async function provision(api) {\n  return api('/v1/invoices', { method: 'POST' })\n}\n";

        let found = facts("scripts/provision.mjs", source);

        let reference = &found.path_references[0];
        assert_eq!(reference.path, "/v1/invoices");
        assert_eq!(reference.location.symbol.as_deref(), Some("provision"));
    }

    #[test]
    fn go_routes_join_subrouter_prefixes_and_methods() {
        let source = r#"package tools

func SetupToolRoutes(r *mux.Router) {
	api := r.PathPrefix("/v1/tools").Subrouter()
	api.Handle("/{slug}/execute", NewExecute()).Methods("POST")
	api.Handle("/enums", NewEnums()).Methods(http.MethodGet, "HEAD")
	r.HandleFunc("GET /v1/health", health)
	r.Get("/v1/skills", listSkills)
}
"#;

        let found = facts("router.go", source);

        let routes: Vec<(&str, &str)> = found
            .method_paths
            .iter()
            .map(|route| (route.method.as_str(), route.path.as_str()))
            .collect();
        assert_eq!(
            routes,
            [
                ("GET", "/v1/health"),
                ("GET", "/v1/skills"),
                ("GET", "/v1/tools/enums"),
                ("HEAD", "/v1/tools/enums"),
                ("POST", "/v1/tools/{}/execute"),
            ]
        );
        assert_eq!(found.path_references, []);
    }

    #[test]
    fn go_requests_carry_their_method_and_json_names_are_mentions() {
        let source = r#"package callbacks

type Payload struct {
	TriggerID string `json:"trigger_id"`
	Ignored   string `json:"-"`
}

func notify(ctx context.Context, base string) error {
	req, err := http.NewRequestWithContext(ctx, http.MethodPost, fmt.Sprintf("%s/v1/triggers/%s/callback", base, id), nil)
	_, err = http.Get(base + "/v1/status")
	return err
}
"#;

        let found = facts("callbacks/notify.go", source);

        let references: Vec<(&str, &str)> = found
            .path_references
            .iter()
            .map(|reference| (reference.method.as_str(), reference.path.as_str()))
            .collect();
        assert_eq!(
            references,
            [("GET", "/v1/status"), ("POST", "/v1/triggers/{}/callback")]
        );
        assert_eq!(
            found.path_references[1].location.symbol.as_deref(),
            Some("notify")
        );
        assert_eq!(names(&found), ["trigger_id"]);
    }

    #[test]
    fn rust_routes_from_axum_and_attributes() {
        let source = r#"
pub fn router() -> Router {
    Router::new()
        .route("/v1/tools", get(list_tools).post(create_tool))
        .route("/v1/tools/{slug}", routing::delete(delete_tool))
}

#[get("/v1/health")]
async fn health() -> &'static str { "ok" }
"#;

        let found = facts("src/routes.rs", source);

        let routes: Vec<(&str, &str)> = found
            .method_paths
            .iter()
            .map(|route| (route.method.as_str(), route.path.as_str()))
            .collect();
        assert_eq!(
            routes,
            [
                ("DELETE", "/v1/tools/{}"),
                ("GET", "/v1/health"),
                ("GET", "/v1/tools"),
                ("POST", "/v1/tools"),
            ]
        );
        assert_eq!(found.path_references, []);
    }

    #[test]
    fn rust_requests_fields_and_crate_imports() {
        let source = r#"
use crate::models::{Tool, Toolkit};
use serde::Serialize;

#[derive(Serialize)]
struct Execute {
    tool_slug: String,
    #[serde(rename = "connectedAccountId")]
    account: String,
}

impl Client {
    pub async fn execute(&self, id: &str) -> Result<()> {
        self.http.post(format!("{}/v1/tools/{}/execute", self.base, id)).json(&json!({ "arguments": 1 })).send().await?;
        self.http.request(Method::DELETE, &format!("{}/v1/tools/{id}", self.base)).send().await?;
        Ok(())
    }
}
"#;

        let found = facts("src/client.rs", source);

        let references: Vec<(&str, &str)> = found
            .path_references
            .iter()
            .map(|reference| (reference.method.as_str(), reference.path.as_str()))
            .collect();
        assert_eq!(
            references,
            [("DELETE", "/v1/tools/{}"), ("POST", "/v1/tools/{}/execute")]
        );
        assert_eq!(
            found.path_references[1].location.symbol.as_deref(),
            Some("Client.execute")
        );
        for name in ["tool_slug", "connectedAccountId", "arguments"] {
            assert!(names(&found).contains(&name), "{name}");
        }
        assert_eq!(found.imports, ["crate::models"]);
    }

    #[test]
    fn zod_strictness_and_route_schemas() {
        let source = r"
const Lenient = z.object({ a: z.string() })
const Strict = z.object({ a: z.string() }).strict().openapi('Strict')
const Loosened = z.object({}).strict().passthrough()
const Either = z.discriminatedUnion('kind', [Strict, z.object({}).strict()])
const Mixed = z.union([Strict, z.object({})])
const Route = router.createRoute({
  method: 'patch',
  path: '/v1/x/{id}',
  request: {
    body: { content: { 'application/json': { schema: Either } } },
    query: z.object({ q: z.string() }).strict(),
  },
})
";

        let found = facts("routes.ts", source);

        assert_eq!(found.strict_schemas, ["Strict"]);
        let unions: Vec<(&str, Vec<&str>)> = found
            .schema_unions
            .iter()
            .map(|union| {
                (
                    union.name.as_str(),
                    union.members.iter().map(String::as_str).collect(),
                )
            })
            .collect();
        assert_eq!(
            unions,
            [("Either", vec!["Strict"]), ("Mixed", vec!["Strict", "*"])]
        );
        let uses: Vec<(&str, &str, Option<&str>, bool)> = found
            .schema_uses
            .iter()
            .map(|used| {
                (
                    used.path.as_str(),
                    used.part.as_str(),
                    used.schema.as_deref(),
                    used.is_inline_strict,
                )
            })
            .collect();
        assert_eq!(
            uses,
            [
                ("/v1/x/{}", "body", Some("Either"), false),
                ("/v1/x/{}", "query", None, true)
            ]
        );
    }

    #[test]
    fn functions_imported_from_a_generated_client_are_client_calls() {
        let source = "import { listInvoices as list, OpenAPI } from '../../generated/billing'\nexport const load = (id) => list({ customerId: id })\n";

        let found = facts("a.ts", source);

        assert_eq!(found.client_calls.len(), 1);
        assert_eq!(found.client_calls[0].chain, "listInvoices");
    }

    #[test]
    fn calls_through_clients_are_chains() {
        let source = "class Invoices {\n  async list(p) {\n    return this.client.invoices.list(p)\n  }\n}\n";

        let found = facts("SessionConfigs.ts", source);

        let call = &found.client_calls[0];
        assert_eq!(call.chain, "invoices.list");
        assert_eq!(call.location.symbol.as_deref(), Some("Invoices.list"));
    }

    #[test]
    fn python_client_calls_are_chains() {
        let source = "def run(self):\n    return self._client.invoices.list()\n";

        let found = facts("a.py", source);

        assert_eq!(found.client_calls[0].chain, "invoices.list");
    }

    #[test]
    fn field_names_and_relative_imports_are_recorded() {
        let source = "import { Params } from '../types/configs'\nimport z from 'zod'\nconst p = { search: 'x', limit }\nconsole.log(p.cursor, p['archived'])\n";

        let found = facts("a.ts", source);

        for name in ["search", "limit", "cursor", "archived"] {
            assert!(names(&found).contains(&name), "{name}");
        }
        assert_eq!(found.imports, ["../types/configs", "zod"]);
    }

    #[test]
    fn python_keywords_and_relative_imports_are_recorded() {
        let source = "from .models import Hook\n\ndef f(client):\n    client.create(on_conflict='skip', **{'note': 'x'})\n";

        let found = facts("a.py", source);

        assert!(names(&found).contains(&"on_conflict"));
        assert_eq!(found.imports, [".models"]);
    }

    #[test]
    fn other_strings_with_api_paths_are_references_of_unknown_method() {
        let source = "const PATH = \"/v1/accounts/link\";\nconst LOGO = \"/static/logo.svg\";\n";

        let found = facts("a.ts", source);

        assert_eq!(found.path_references[0].method, "*");
    }

    #[test]
    fn module_specifiers_are_not_requests() {
        let source = "import { a } from './v1/a'\nexport { b } from './v1/b'\nconst c = await import('./v1/c')\nconst d = require('/v1/d')\nfetch('/v1/users')\n";

        let found = facts("a.ts", source);

        let paths: Vec<&str> = found
            .path_references
            .iter()
            .map(|reference| reference.path.as_str())
            .collect();
        assert_eq!(paths, ["/v1/users"]);
    }

    #[test]
    fn a_path_built_inside_a_call_takes_the_calls_method() {
        let typescript = "client.get(path`/v1/users/${id}`, options)\n";
        let python =
            "self._client._request(\"patch\", build_path(\"/v1/users/{id}\", {\"id\": id}))\n";

        let typed = facts("a.ts", typescript);
        let python = facts("a.py", python);

        assert_eq!(typed.path_references[0].method, "GET");
        assert_eq!(typed.path_references[0].path, "/v1/users/{}");
        assert_eq!(python.path_references[0].method, "PATCH");
    }

    #[test]
    fn calls_through_long_chains_and_imported_names_are_recorded() {
        let typescript = "import a, { b as c } from 'x'\nimport * as ns from 'y'\nthis.client.authConfigs.list()\nthis.client.withOptions({}).mcp.generate.url()\nconsole.log(1)\n";
        let python = "from z import p, r as q\nimport m.n as o\nself._client.auth_configs.list()\n";

        let typed = facts("a.ts", typescript);
        let python = facts("a.py", python);

        let chains: Vec<String> = typed
            .member_calls
            .iter()
            .map(|call| call.parts.join("."))
            .collect();
        assert_eq!(
            chains,
            [
                "this.client.authConfigs.list",
                "().mcp.generate.url",
                "this.client.withOptions"
            ]
        );
        assert_eq!(typed.imported_names, ["a", "c", "ns"]);
        assert_eq!(python.imported_names, ["o", "p", "q"]);
        assert_eq!(
            python.member_calls[0].parts,
            ["self", "_client", "auth_configs", "list"]
        );
    }

    #[test]
    fn bindings_reexports_and_calls_through_imports_are_recorded() {
        let source = "import * as api from '../generated/api'\nimport { listTools as list } from '../generated/api'\nexport * as client from '../generated/api'\napi.listTools()\nlist()\nlocal()\n";

        let found = facts("a.ts", source);

        let bindings: Vec<(&str, Option<&str>)> = found
            .import_bindings
            .iter()
            .map(|binding| (binding.local.as_str(), binding.imported.as_deref()))
            .collect();
        assert_eq!(bindings, [("api", None), ("list", Some("listTools"))]);
        assert_eq!(found.reexports[0].name, "client");
        let chains: Vec<String> = found
            .member_calls
            .iter()
            .map(|call| call.parts.join("."))
            .collect();
        assert_eq!(chains, ["api.listTools", "list"]);
    }
}
