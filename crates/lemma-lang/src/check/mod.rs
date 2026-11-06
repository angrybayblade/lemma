//! Checking a loaded program: binding every name, giving every expression a
//! type, and evaluating what can be known without running anything.
//!
//! The result keeps three things apart: the [`Model`] lowering reads, the
//! [`SymbolIndex`] editors read (where each name is defined and used), and
//! the diagnostics both show.

mod declarations;
mod expressions;
mod rules;
mod statements;

use std::collections::{BTreeMap, BTreeSet, HashMap};

use crate::diagnostic::Diagnostic;
use crate::load::Loaded;
use crate::model::Model;
use crate::span::Span;
use crate::stdlib::{self, Builtin};
use crate::syntax::ast::{self, Declaration as Node, ImportSelection, Item, ServiceMember};
use crate::types::{Address, Type, Value};

/// A checked program.
#[derive(Debug, Clone)]
pub struct Checked {
    /// The modules, as loaded.
    pub loaded: Loaded,
    /// What the program means.
    pub model: Model,
    /// Where names are defined and used.
    pub index: SymbolIndex,
    /// Every problem: loading, parsing and checking.
    pub diagnostics: Vec<Diagnostic>,
}

/// Where each name is defined and used, for hovers and go-to-definition.
#[derive(Debug, Clone, Default)]
pub struct SymbolIndex {
    /// Declarations, by address.
    pub definitions: BTreeMap<Address, Definition>,
    /// Uses of names, in module then source order.
    pub references: Vec<Reference>,
    /// The names each module sees, with what each is, for completion.
    pub scopes: BTreeMap<String, Vec<(String, String)>>,
}

/// A declaration, for editors.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Definition {
    /// The module it is in.
    pub module: String,
    /// Its name's span.
    pub span: Span,
    /// What it is: `service`, `relation`, `rule` and so on.
    pub kind: &'static str,
    /// Its type or signature, for hovers.
    pub detail: String,
}

/// A use of a name.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Reference {
    /// The module it is in.
    pub module: String,
    /// The name's span.
    pub span: Span,
    /// What it names.
    pub target: Target,
}

/// What a name refers to.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Target {
    /// A declaration.
    Declaration(Address),
    /// A module.
    Module(String),
    /// A built-in function, with its signature and documentation.
    Builtin {
        /// `module.name`, or the name alone for the prelude.
        name: String,
        /// What it is for, and its signature.
        detail: String,
    },
    /// A local variable or parameter, with its type.
    Local {
        /// Where it is bound.
        definition: Span,
        /// Its type.
        detail: String,
    },
}

/// Checks a loaded program.
pub fn check(loaded: Loaded) -> Checked {
    let builtins = stdlib::builtins();
    let mut checker = Checker::new(&loaded, &builtins);
    checker.collect();
    checker.build_scopes();
    checker.check_all();
    checker.export_scopes();
    let Checker {
        model,
        index,
        diagnostics: found,
        ..
    } = checker;
    let mut diagnostics = loaded.diagnostics.clone();
    diagnostics.extend(found);
    diagnostics.sort_by(|left, right| (&left.module, left.span).cmp(&(&right.module, right.span)));
    diagnostics.dedup();
    Checked {
        loaded,
        model,
        index,
        diagnostics,
    }
}

/// An index into [`Checker::declarations`].
pub(crate) type DeclarationId = usize;

/// A declaration and where its syntax is.
#[derive(Debug, Clone)]
pub(crate) struct Declared<'a> {
    pub address: Address,
    pub module: &'a str,
    pub is_exported: bool,
    pub name_span: Span,
    pub node: NodeRef<'a>,
}

/// The syntax of a declaration.
#[derive(Debug, Clone, Copy)]
pub(crate) enum NodeRef<'a> {
    Top(&'a Node),
    Service(&'a ast::ServiceDeclaration),
    Operation(&'a ast::OperationDeclaration),
    ServiceBinding(&'a ast::Binding),
}

impl NodeRef<'_> {
    fn kind(self) -> &'static str {
        match self {
            NodeRef::Top(node) => match node {
                Node::Binding(_) => "binding",
                Node::Type(_) => "type",
                Node::Entity(_) => "entity",
                Node::Relation(_) => "relation",
                Node::Service(_) => "service",
                Node::Graph(_) => "graph",
                Node::Rule(_) => "rule",
                Node::Require(_) => "requirement",
                Node::Allow(_) => "allowance",
                Node::Behavior(_) => "behavior",
                Node::Function(_) => "function",
                Node::Checker(_) => "checker",
                Node::Check(_) => "check",
            },
            NodeRef::Service(_) => "service",
            NodeRef::Operation(operation) if operation.is_client => "client operation",
            NodeRef::Operation(_) => "operation",
            NodeRef::ServiceBinding(_) => "binding",
        }
    }
}

/// What a name in scope is.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum Entry {
    Declaration(DeclarationId),
    Namespace(String),
    Builtin(usize),
}

/// The names one module sees, with where each import wrote it.
#[derive(Debug, Clone, Default)]
pub(crate) struct Scope {
    pub entries: BTreeMap<String, Entry>,
}

pub(crate) struct Checker<'a> {
    pub loaded: &'a Loaded,
    pub builtins: &'a [Builtin],
    pub declarations: Vec<Declared<'a>>,
    pub by_address: HashMap<Address, DeclarationId>,
    /// Each module's own top-level names.
    pub own: BTreeMap<&'a str, BTreeMap<String, DeclarationId>>,
    /// Each service's members, by name.
    pub members: HashMap<DeclarationId, BTreeMap<String, DeclarationId>>,
    /// Allowances, which have no name of their own.
    pub allowances: Vec<(&'a str, &'a ast::AllowDeclaration)>,
    pub scopes: BTreeMap<&'a str, Scope>,
    /// Bindings evaluated so far, and those being evaluated (cycles).
    pub values: HashMap<DeclarationId, (Type, Option<Value>)>,
    pub evaluating: BTreeSet<DeclarationId>,
    /// Columns of relations, rules and graphs, once resolved.
    pub columns: HashMap<DeclarationId, Vec<crate::model::Column>>,
    /// Each requirement's finding variables, for its allowances.
    pub requirement_variables: HashMap<Address, Vec<(String, Type)>>,
    pub model: Model,
    pub index: SymbolIndex,
    pub diagnostics: Vec<Diagnostic>,
}

impl<'a> Checker<'a> {
    fn new(loaded: &'a Loaded, builtins: &'a [Builtin]) -> Self {
        Self {
            loaded,
            builtins,
            declarations: Vec::new(),
            by_address: HashMap::new(),
            own: BTreeMap::new(),
            members: HashMap::new(),
            allowances: Vec::new(),
            scopes: BTreeMap::new(),
            values: HashMap::new(),
            evaluating: BTreeSet::new(),
            columns: HashMap::new(),
            requirement_variables: HashMap::new(),
            model: Model::default(),
            index: SymbolIndex::default(),
            diagnostics: Vec::new(),
        }
    }

    pub(crate) fn error(&mut self, module: &str, span: Span, message: impl Into<String>) {
        self.diagnostics
            .push(Diagnostic::error(span, message).in_module(module));
    }

    pub(crate) fn warning(&mut self, module: &str, span: Span, message: impl Into<String>) {
        self.diagnostics
            .push(Diagnostic::warning(span, message).in_module(module));
    }

    pub(crate) fn refer(&mut self, module: &str, span: Span, target: Target) {
        self.index.references.push(Reference {
            module: module.to_owned(),
            span,
            target,
        });
    }

    /// Records every declaration of every module, nested ones included.
    fn collect(&mut self) {
        let loaded = self.loaded;
        for module in loaded.modules.values() {
            self.own.entry(module.name.as_str()).or_default();
            let declarations = module.file.items.iter().filter_map(|item| match item {
                Item::Declaration(declared) => Some(declared),
                _ => None,
            });
            for declared in declarations {
                self.collect_top(&module.name, declared);
            }
        }
    }

    fn collect_top(&mut self, module: &'a str, declared: &'a ast::Declared) {
        let node = &declared.declaration;
        if let Node::Allow(allow) = node {
            self.allowances.push((module, allow));
            return;
        }
        let Some(name) = node.name() else {
            return;
        };
        let address = Address::top(module, &name.text);
        let Some(id) = self.declare(Declared {
            address: address.clone(),
            module,
            is_exported: declared.is_exported,
            name_span: name.span,
            node: NodeRef::Top(node),
        }) else {
            self.error(
                module,
                name.span,
                format!("`{}` is declared twice in this module", name.text),
            );
            return;
        };
        self.own
            .entry(module)
            .or_default()
            .insert(name.text.clone(), id);
        if let Node::Service(service) = node {
            self.collect_members(id, &address, module, service);
        }
    }

    fn collect_members(
        &mut self,
        service_id: DeclarationId,
        address: &Address,
        module: &'a str,
        service: &'a ast::ServiceDeclaration,
    ) {
        for member in &service.members {
            let (name, is_exported, node) = match member {
                ServiceMember::Service {
                    is_exported,
                    service,
                } => (&service.name, *is_exported, NodeRef::Service(service)),
                ServiceMember::Operation {
                    is_exported,
                    operation,
                } => (&operation.name, *is_exported, NodeRef::Operation(operation)),
                ServiceMember::Binding(binding) => {
                    (&binding.name, false, NodeRef::ServiceBinding(binding))
                }
                ServiceMember::Field(_) => continue,
            };
            let child = address.child(&name.text);
            let Some(id) = self.declare(Declared {
                address: child.clone(),
                module,
                is_exported,
                name_span: name.span,
                node,
            }) else {
                self.error(
                    module,
                    name.span,
                    format!("`{}` is declared twice in this service", name.text),
                );
                continue;
            };
            self.members
                .entry(service_id)
                .or_default()
                .insert(name.text.clone(), id);
            if let ServiceMember::Service { service, .. } = member {
                self.collect_members(id, &child, module, service);
            }
        }
    }

    /// Records a declaration; `None` if its address is taken.
    fn declare(&mut self, declared: Declared<'a>) -> Option<DeclarationId> {
        if self.by_address.contains_key(&declared.address) {
            return None;
        }
        let id = self.declarations.len();
        self.by_address.insert(declared.address.clone(), id);
        self.declarations.push(declared);
        Some(id)
    }

    /// Each module's scope: its own declarations, then its imports.
    fn build_scopes(&mut self) {
        let loaded = self.loaded;
        for module in loaded.modules.values() {
            let scope = self.scope_of(module);
            self.scopes.insert(module.name.as_str(), scope);
        }
        self.report_cycles();
    }

    fn scope_of(&mut self, module: &crate::load::Module) -> Scope {
        let own = self
            .own
            .get(module.name.as_str())
            .cloned()
            .unwrap_or_default();
        let mut scope = Scope {
            entries: own
                .into_iter()
                .map(|(name, id)| (name, Entry::Declaration(id)))
                .collect(),
        };
        let imports = module.file.items.iter().filter_map(|item| match item {
            Item::Import(import) => Some(import),
            _ => None,
        });
        for import in imports {
            self.import_into(&mut scope, &module.name, import);
        }
        scope
    }

    fn import_into(&mut self, scope: &mut Scope, module: &str, import: &ast::Import) {
        let source = import.module.dotted();
        if !self.loaded.modules.contains_key(&source) {
            return;
        }
        self.refer(module, import.module.span, Target::Module(source.clone()));
        match &import.selection {
            ImportSelection::Module => {}
            ImportSelection::Namespace(alias) => {
                self.bind_import(scope, module, alias, Entry::Namespace(source));
            }
            ImportSelection::Names(names) => {
                for name in names {
                    self.import_name(scope, module, &source, name);
                }
            }
        }
    }

    fn import_name(
        &mut self,
        scope: &mut Scope,
        module: &str,
        source: &str,
        name: &ast::ImportName,
    ) {
        let Some(entry) = self.export_of(source, &name.name.text, &mut BTreeSet::new()) else {
            self.error(
                module,
                name.name.span,
                format!("module `{source}` exports no `{}`", name.name.text),
            );
            return;
        };
        self.refer_entry(module, name.name.span, &entry);
        self.bind_import(scope, module, name.local(), entry);
    }

    fn bind_import(&mut self, scope: &mut Scope, module: &str, name: &ast::Name, entry: Entry) {
        if scope.entries.contains_key(&name.text) {
            self.error(
                module,
                name.span,
                format!("`{}` is already a name in this module", name.text),
            );
            return;
        }
        scope.entries.insert(name.text.clone(), entry);
    }

    /// What module `source` exports as `name`: an exported declaration, a
    /// re-export, or a built-in function.
    pub(crate) fn export_of(
        &self,
        source: &str,
        name: &str,
        visiting: &mut BTreeSet<String>,
    ) -> Option<Entry> {
        if !visiting.insert(source.to_owned()) {
            return None;
        }
        if let Some(id) = self.own.get(source).and_then(|names| names.get(name)) {
            if self.declarations[*id].is_exported {
                return Some(Entry::Declaration(*id));
            }
        }
        if let Some(index) = self
            .builtins
            .iter()
            .position(|builtin| builtin.module == source && builtin.name == name)
        {
            return Some(Entry::Builtin(index));
        }
        let module = self.loaded.modules.get(source)?;
        for item in &module.file.items {
            let Item::Reexport(reexport) = item else {
                continue;
            };
            let Some(found) = reexport
                .names
                .iter()
                .find(|written| written.local().text == name)
            else {
                continue;
            };
            return self.export_of(&reexport.module.dotted(), &found.name.text, visiting);
        }
        None
    }

    pub(crate) fn refer_entry(&mut self, module: &str, span: Span, entry: &Entry) {
        let target = match entry {
            Entry::Declaration(id) => Target::Declaration(self.declarations[*id].address.clone()),
            Entry::Namespace(name) => Target::Module(name.clone()),
            Entry::Builtin(index) => builtin_target(&self.builtins[*index]),
        };
        self.refer(module, span, target);
    }

    /// Reports each import that closes a cycle of imports.
    fn report_cycles(&mut self) {
        let loaded = self.loaded;
        let edges: BTreeMap<&str, Vec<(&str, Span)>> = loaded
            .modules
            .values()
            .map(|module| {
                let imported = module
                    .file
                    .items
                    .iter()
                    .filter_map(|item| match item {
                        Item::Import(import) => Some(&import.module),
                        Item::Reexport(reexport) => Some(&reexport.module),
                        _ => None,
                    })
                    .filter_map(|name| {
                        let dotted = name.dotted();
                        loaded
                            .modules
                            .get_key_value(&dotted)
                            .map(|(key, _)| (key.as_str(), name.span))
                    })
                    .collect();
                (module.name.as_str(), imported)
            })
            .collect();
        let cycles: Vec<(&str, &str, Span)> = edges
            .iter()
            .flat_map(|(module, imports)| {
                imports
                    .iter()
                    .map(move |(imported, span)| (*module, *imported, *span))
            })
            .filter(|(module, imported, _)| reaches(&edges, imported, module))
            .collect();
        for (module, imported, span) in cycles {
            self.error(
                module,
                span,
                format!(
                    "importing `{imported}` makes a cycle: it imports this module, directly or not"
                ),
            );
        }
    }

    /// Records each module's visible names for completion: its scope, then
    /// the prelude.
    fn export_scopes(&mut self) {
        let mut scopes = BTreeMap::new();
        for (module, scope) in &self.scopes {
            let mut names: Vec<(String, String)> = scope
                .entries
                .iter()
                .map(|(name, entry)| (name.clone(), self.entry_kind(entry)))
                .collect();
            let prelude = self
                .builtins
                .iter()
                .filter(|builtin| builtin.module.is_empty());
            names.extend(prelude.map(|builtin| (builtin.name.to_owned(), "function".to_owned())));
            scopes.insert((*module).to_owned(), names);
        }
        self.index.scopes = scopes;
    }

    /// What a name in scope is; a namespace names its module, as in
    /// `module lemma.python`.
    fn entry_kind(&self, entry: &Entry) -> String {
        match entry {
            Entry::Declaration(id) => self.declarations[*id].node.kind().to_owned(),
            Entry::Namespace(module) => format!("module {module}"),
            Entry::Builtin(_) => "function".to_owned(),
        }
    }

    /// Looks up `name` as module `module` sees it, then the prelude.
    pub(crate) fn lookup(&self, module: &str, name: &str) -> Option<Entry> {
        if let Some(entry) = self
            .scopes
            .get(module)
            .and_then(|scope| scope.entries.get(name))
        {
            return Some(entry.clone());
        }
        self.builtins
            .iter()
            .position(|builtin| builtin.module.is_empty() && builtin.name == name)
            .map(Entry::Builtin)
    }

    fn check_all(&mut self) {
        let loaded = self.loaded;
        for id in 0..self.declarations.len() {
            self.check_declaration(id);
        }
        let allowances = self.allowances.clone();
        for (module, allow) in allowances {
            self.check_allow(module, allow);
        }
        if let Some(entry) = loaded.modules.get(&loaded.entry) {
            self.check_workspace(entry);
        }
    }
}

fn reaches(edges: &BTreeMap<&str, Vec<(&str, Span)>>, from: &str, target: &str) -> bool {
    let mut stack = vec![from];
    let mut seen = BTreeSet::new();
    while let Some(module) = stack.pop() {
        if module == target {
            return true;
        }
        if !seen.insert(module) {
            continue;
        }
        if let Some(next) = edges.get(module) {
            stack.extend(next.iter().map(|(imported, _)| *imported));
        }
    }
    false
}

pub(crate) fn builtin_target(builtin: &Builtin) -> Target {
    let name = if builtin.module.is_empty() {
        builtin.name.to_owned()
    } else {
        format!("{}.{}", builtin.module, builtin.name)
    };
    let signature = Type::Function(Box::new(builtin.signature.clone()));
    Target::Builtin {
        detail: format!(
            "{name}{}\n\n{}",
            signature_text(&signature),
            builtin.documentation
        ),
        name,
    }
}

/// `(a: T, b: U): R` for a function type.
pub(crate) fn signature_text(function: &Type) -> String {
    let Type::Function(signature) = function else {
        return format!(": {function}");
    };
    let parameters: Vec<String> = signature
        .parameters
        .iter()
        .map(|parameter| {
            let marker = if parameter.is_optional { "?" } else { "" };
            format!("{}{marker}: {}", parameter.name, parameter.parameter_type)
        })
        .collect();
    format!("({}): {}", parameters.join(", "), signature.returns)
}

#[cfg(test)]
mod tests;
