//! What the server answers from one checked program: diagnostics, hovers,
//! definitions, symbols and completions. Pure functions of the program and
//! where its repositories are; the server feeds them and sends the results.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use lemma_lang::syntax::ast::{Declaration, Item, ServiceMember};
use lemma_lang::{Checked, Severity, Span, Target};
use lemma_lsp::lsp_types::{
    self, CompletionItem, CompletionItemKind, DiagnosticSeverity, DocumentSymbol, Hover,
    HoverContents, Location, MarkupContent, MarkupKind, Position, Range, SymbolKind, Url,
};
use lemma_lsp::{PositionEncoding, line_text};

/// A checked program and where its repositories are on disk.
pub struct Analysis {
    /// The program, checked.
    pub checked: Checked,
    /// Each repository's root, by name.
    pub roots: BTreeMap<String, PathBuf>,
    /// Where standard-library modules are written for go-to-definition.
    pub library: PathBuf,
}

/// The words a module may use where a declaration or statement starts.
const KEYWORDS: [&str; 30] = [
    "module",
    "import",
    "export",
    "from",
    "as",
    "let",
    "fn",
    "if",
    "else",
    "for",
    "in",
    "return",
    "expect",
    "because",
    "need",
    "true",
    "false",
    "null",
    "self",
    "workspace",
    "repo",
    "service",
    "operation",
    "relation",
    "graph",
    "rule",
    "require",
    "allow",
    "behavior",
    "checker",
];

impl Analysis {
    /// A module's file, if it has one: under its repository's root, or the
    /// written-out standard library.
    pub fn path_of(&self, module: &str) -> Option<PathBuf> {
        let loaded = self.checked.loaded.modules.get(module)?;
        if loaded.repository == lemma_lang::stdlib::REPOSITORY {
            return Some(self.library.join(&loaded.path));
        }
        Some(self.roots.get(&loaded.repository)?.join(&loaded.path))
    }

    /// The module whose file is `path`.
    pub fn module_at(&self, path: &Path) -> Option<&str> {
        self.checked
            .loaded
            .modules
            .keys()
            .find(|module| self.path_of(module).as_deref() == Some(path))
            .map(String::as_str)
    }

    /// Diagnostics by file, every module with a file included, so files
    /// that became clean are cleared.
    pub fn diagnostics(
        &self,
        encoding: PositionEncoding,
    ) -> BTreeMap<PathBuf, Vec<lsp_types::Diagnostic>> {
        let mut by_file: BTreeMap<PathBuf, Vec<lsp_types::Diagnostic>> = BTreeMap::new();
        let files = self
            .checked
            .loaded
            .modules
            .values()
            .filter(|module| module.repository != lemma_lang::stdlib::REPOSITORY)
            .filter_map(|module| self.path_of(&module.name));
        for path in files {
            by_file.entry(path).or_default();
        }
        for diagnostic in &self.checked.diagnostics {
            let Some(path) = self.path_of(&diagnostic.module) else {
                continue;
            };
            let range = self.range(&diagnostic.module, diagnostic.span, encoding);
            by_file
                .entry(path)
                .or_default()
                .push(lsp_types::Diagnostic {
                    range,
                    severity: Some(match diagnostic.severity {
                        Severity::Error => DiagnosticSeverity::ERROR,
                        Severity::Warning => DiagnosticSeverity::WARNING,
                        Severity::Note => DiagnosticSeverity::INFORMATION,
                    }),
                    source: Some("lemma".to_owned()),
                    message: diagnostic.message.clone(),
                    ..lsp_types::Diagnostic::default()
                });
        }
        by_file
    }

    /// A span of a module as an LSP range.
    pub fn range(&self, module: &str, span: Span, encoding: PositionEncoding) -> Range {
        let Some(loaded) = self.checked.loaded.modules.get(module) else {
            return Range::default();
        };
        let position = |byte: u32| {
            let found = loaded.lines.position(byte);
            let text = line_text(&loaded.text, found.line);
            Position {
                line: found.line,
                character: encoding.encode_column(text, found.column),
            }
        };
        Range {
            start: position(span.start),
            end: position(span.end),
        }
    }

    /// The byte offset of an LSP position in a module.
    pub fn offset(
        &self,
        module: &str,
        position: Position,
        encoding: PositionEncoding,
    ) -> Option<u32> {
        let loaded = self.checked.loaded.modules.get(module)?;
        let text = line_text(&loaded.text, position.line);
        let column = encoding.decode_column(text, position.character);
        Some(loaded.lines.offset(lemma_lang::Position {
            line: position.line,
            column,
        }))
    }

    /// The innermost name used or declared at `offset`.
    fn target_at(&self, module: &str, offset: u32) -> Option<(Span, Target)> {
        let index = &self.checked.index;
        let used = index
            .references
            .iter()
            .filter(|reference| reference.module == module && reference.span.touches(offset))
            .min_by_key(|reference| reference.span.end - reference.span.start)
            .map(|reference| (reference.span, reference.target.clone()));
        used.or_else(|| {
            index
                .definitions
                .iter()
                .find(|(_, definition)| {
                    definition.module == module && definition.span.touches(offset)
                })
                .map(|(address, definition)| {
                    (definition.span, Target::Declaration(address.clone()))
                })
        })
    }

    /// What the name at `offset` is.
    pub fn hover(&self, module: &str, offset: u32, encoding: PositionEncoding) -> Option<Hover> {
        let (span, target) = self.target_at(module, offset)?;
        let text = match &target {
            Target::Declaration(address) => {
                let definition = self.checked.index.definitions.get(address)?;
                let detail = if definition.detail.is_empty() {
                    String::new()
                } else {
                    format!(": {}", definition.detail)
                };
                format!(
                    "```lm\n{} {}{detail}\n```\n`{address}`",
                    definition.kind,
                    address.name()
                )
            }
            Target::Module(name) => format!("```lm\nmodule {name}\n```"),
            Target::Builtin { detail, .. } => {
                let (signature, documentation) = detail.split_once("\n\n").unwrap_or((detail, ""));
                format!("```lm\nfn {signature}\n```\n{documentation}")
            }
            Target::Local { detail, .. } => {
                format!("```lm\n{}: {detail}\n```", span_text(self, module, span))
            }
        };
        Some(Hover {
            contents: HoverContents::Markup(MarkupContent {
                kind: MarkupKind::Markdown,
                value: text,
            }),
            range: Some(self.range(module, span, encoding)),
        })
    }

    /// Where the name at `offset` is declared.
    pub fn definition(
        &self,
        module: &str,
        offset: u32,
        encoding: PositionEncoding,
    ) -> Option<Location> {
        let (_, target) = self.target_at(module, offset)?;
        let (target_module, span) = match target {
            Target::Declaration(address) => {
                let definition = self.checked.index.definitions.get(&address)?;
                (definition.module.clone(), definition.span)
            }
            Target::Module(name) => (name, Span::default()),
            Target::Local { definition, .. } => (module.to_owned(), definition),
            Target::Builtin { .. } => return None,
        };
        let path = self.path_of(&target_module)?;
        Some(Location {
            uri: Url::from_file_path(path).ok()?,
            range: self.range(&target_module, span, encoding),
        })
    }

    /// A module's declarations, services holding their members.
    pub fn symbols(&self, module: &str, encoding: PositionEncoding) -> Vec<DocumentSymbol> {
        let Some(loaded) = self.checked.loaded.modules.get(module) else {
            return Vec::new();
        };
        loaded
            .file
            .items
            .iter()
            .filter_map(|item| match item {
                Item::Declaration(declared) => {
                    self.declaration_symbol(module, &declared.declaration, declared.span, encoding)
                }
                _ => None,
            })
            .collect()
    }

    fn declaration_symbol(
        &self,
        module: &str,
        declaration: &Declaration,
        span: Span,
        encoding: PositionEncoding,
    ) -> Option<DocumentSymbol> {
        let name = declaration.name()?;
        let (kind, children) = match declaration {
            Declaration::Service(service) => (
                SymbolKind::MODULE,
                self.service_symbols(module, &service.members, encoding),
            ),
            Declaration::Relation(_) | Declaration::Entity(_) | Declaration::Type(_) => {
                (SymbolKind::STRUCT, Vec::new())
            }
            Declaration::Rule(_) | Declaration::Graph(_) => (SymbolKind::INTERFACE, Vec::new()),
            Declaration::Require(_) | Declaration::Behavior(_) | Declaration::Check(_) => {
                (SymbolKind::EVENT, Vec::new())
            }
            Declaration::Function(_) | Declaration::Checker(_) => {
                (SymbolKind::FUNCTION, Vec::new())
            }
            Declaration::Binding(_) | Declaration::Allow(_) => (SymbolKind::VARIABLE, Vec::new()),
        };
        Some(symbol(
            name.text.clone(),
            kind,
            self.range(module, span, encoding),
            self.range(module, name.span, encoding),
            children,
        ))
    }

    fn service_symbols(
        &self,
        module: &str,
        members: &[ServiceMember],
        encoding: PositionEncoding,
    ) -> Vec<DocumentSymbol> {
        members
            .iter()
            .filter_map(|member| match member {
                ServiceMember::Service { service, .. } => Some(symbol(
                    service.name.text.clone(),
                    SymbolKind::MODULE,
                    self.range(module, service.span, encoding),
                    self.range(module, service.name.span, encoding),
                    self.service_symbols(module, &service.members, encoding),
                )),
                ServiceMember::Operation { operation, .. } => Some(symbol(
                    operation.name.text.clone(),
                    SymbolKind::METHOD,
                    self.range(module, operation.span, encoding),
                    self.range(module, operation.name.span, encoding),
                    Vec::new(),
                )),
                ServiceMember::Binding(_) | ServiceMember::Field(_) => None,
            })
            .collect()
    }

    /// Names that fit at `offset`: a namespace's or service's members after
    /// `name.`, otherwise the module's names and the keywords.
    pub fn completions(&self, module: &str, offset: u32) -> Vec<CompletionItem> {
        if let Some(owner) = self.word_before_dot(module, offset) {
            return self.member_completions(module, &owner);
        }
        let mut items: Vec<CompletionItem> = self
            .checked
            .index
            .scopes
            .get(module)
            .into_iter()
            .flatten()
            .map(|(name, kind)| completion(name, kind))
            .collect();
        items.extend(KEYWORDS.iter().map(|keyword| CompletionItem {
            label: (*keyword).to_owned(),
            kind: Some(CompletionItemKind::KEYWORD),
            ..CompletionItem::default()
        }));
        items
    }

    /// The name just before a `.` ending at `offset`, as in `python.|`.
    fn word_before_dot(&self, module: &str, offset: u32) -> Option<String> {
        let text = &self.checked.loaded.modules.get(module)?.text;
        let before = text.get(..offset as usize)?;
        let before_dot = before.strip_suffix('.')?;
        let word: String = before_dot
            .chars()
            .rev()
            .take_while(|character| character.is_ascii_alphanumeric() || *character == '_')
            .collect::<Vec<_>>()
            .into_iter()
            .rev()
            .collect();
        (!word.is_empty()).then_some(word)
    }

    fn member_completions(&self, module: &str, owner: &str) -> Vec<CompletionItem> {
        let scope = self.checked.index.scopes.get(module);
        let kind = scope
            .into_iter()
            .flatten()
            .find(|(name, _)| name == owner)
            .map(|(_, kind)| kind.as_str());
        if let Some(namespace) = kind.and_then(|kind| kind.strip_prefix("module ")) {
            return self.namespace_completions(namespace);
        }
        self.checked
            .index
            .definitions
            .iter()
            .filter(|(address, _)| {
                address.path.len() >= 2 && address.path[address.path.len() - 2] == owner
            })
            .map(|(address, definition)| completion(address.name(), definition.kind))
            .collect()
    }

    fn namespace_completions(&self, namespace: &str) -> Vec<CompletionItem> {
        let mut items: Vec<CompletionItem> = self
            .checked
            .index
            .definitions
            .iter()
            .filter(|(address, _)| address.module == namespace && address.path.len() == 1)
            .map(|(address, definition)| completion(address.name(), definition.kind))
            .collect();
        let builtins = lemma_lang::stdlib::builtins();
        items.extend(
            builtins
                .iter()
                .filter(|builtin| builtin.module == namespace)
                .map(|builtin| completion(builtin.name, "function")),
        );
        items
    }
}

fn span_text<'a>(analysis: &'a Analysis, module: &str, span: Span) -> &'a str {
    analysis
        .checked
        .loaded
        .modules
        .get(module)
        .map_or("", |loaded| span.text(&loaded.text))
}

fn completion(name: &str, kind: &str) -> CompletionItem {
    let item_kind = match kind {
        "service" => CompletionItemKind::MODULE,
        "operation" | "client operation" => CompletionItemKind::METHOD,
        "relation" | "entity" | "type" => CompletionItemKind::STRUCT,
        "rule" | "graph" => CompletionItemKind::INTERFACE,
        "function" | "checker" => CompletionItemKind::FUNCTION,
        "requirement" | "behavior" | "check" => CompletionItemKind::EVENT,
        _ if kind.starts_with("module") => CompletionItemKind::MODULE,
        _ => CompletionItemKind::VARIABLE,
    };
    CompletionItem {
        label: name.to_owned(),
        kind: Some(item_kind),
        detail: Some(kind.to_owned()),
        ..CompletionItem::default()
    }
}

#[expect(
    deprecated,
    reason = "lsp-types requires the deprecated field to build a symbol"
)]
fn symbol(
    name: String,
    kind: SymbolKind,
    range: Range,
    selection_range: Range,
    children: Vec<DocumentSymbol>,
) -> DocumentSymbol {
    DocumentSymbol {
        name,
        detail: None,
        kind,
        tags: None,
        deprecated: None,
        range,
        selection_range,
        children: (!children.is_empty()).then_some(children),
    }
}
