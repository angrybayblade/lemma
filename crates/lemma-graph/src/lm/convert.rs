//! Writing a `lemma.yaml` as an `.lm` module, so an existing configuration
//! moves to the language mechanically and lowers back to what it was.
//!
//! An operation that calls through a link is written as a `client
//! operation` (`sends`/`accepts`); `lemma.yaml` says the role only by the
//! link's direction. Code files, documents and commands are bound once with
//! `let` and named after what they are.

use std::collections::{BTreeMap, BTreeSet};
use std::fmt::Write as _;

use crate::address::{Address as LinkAddress, parse_link};
use crate::config::{LemmaFile, Level, Operation, OperationsFrom, Resolver, SchemaSource, Service};

/// `file` as the `.lm` root module `module`. A file of external services
/// only is a workspace.
pub fn convert(file: &LemmaFile, module: &str) -> String {
    let local: Vec<(&String, &Service)> = file
        .services
        .iter()
        .filter(|(name, _)| !name.starts_with('#'))
        .collect();
    if local.is_empty() {
        return convert_workspace(file, module);
    }
    Writer::new(file, module).module(&local)
}

/// `file` as a workspace module: its external services as the workspace's
/// repositories, and its level.
pub fn convert_workspace(file: &LemmaFile, module: &str) -> String {
    let mut text = format!("module {module}\n\nworkspace {{\n    language: \"0.2\"\n");
    for (name, service) in &file.services {
        if let Some(name) = name.strip_prefix('#') {
            let _ = writeln!(
                text,
                "    repo {} = {}",
                repository_name(name),
                repository_source(service)
            );
        }
    }
    if let Some(level) = file.graph.level {
        let _ = writeln!(text, "    config: {{ level: {} }}", level_name(level));
    }
    text.push_str("}\n");
    text
}

fn level_name(level: Level) -> &'static str {
    match level {
        Level::Audit => "audit",
        Level::Check => "check",
        Level::Strict => "strict",
    }
}

fn repository_source(service: &Service) -> String {
    if let Some(url) = &service.git {
        return match &service.reference {
            Some(reference) => format!("git(url: {}, rev: {})", quoted(url), quoted(reference)),
            None => format!("git(url: {})", quoted(url)),
        };
    }
    let path = service.path.as_deref().unwrap_or(".");
    format!("local({})", quoted(path))
}

struct Writer<'f> {
    file: &'f LemmaFile,
    module: &'f str,
    /// Operations that call through a link, by service and name.
    clients: BTreeSet<(String, String)>,
    /// `let` bindings, by the expression they name.
    bindings: BTreeMap<String, String>,
    /// Binding names in use.
    names: BTreeSet<String>,
    /// Standard-library namespaces used.
    namespaces: BTreeSet<&'static str>,
}

impl<'f> Writer<'f> {
    fn new(file: &'f LemmaFile, module: &'f str) -> Self {
        let clients = file
            .graph
            .links
            .iter()
            .filter_map(|text| parse_link(text).ok())
            .filter(|link| link.caller.repository.is_none())
            .map(|link| (link.caller.service, link.caller.operation))
            .collect();
        Self {
            file,
            module,
            clients,
            bindings: BTreeMap::new(),
            names: BTreeSet::new(),
            namespaces: BTreeSet::new(),
        }
    }

    fn module(mut self, local: &[(&String, &Service)]) -> String {
        let mut services = String::new();
        for (name, service) in local {
            services.push_str(&self.service(name, service));
        }
        let links = self.links();
        let mut text = format!("module {}\n\n", self.module);
        text.push_str(&self.imports());
        text.push_str(&self.repositories());
        for (expression, name) in self.sorted_bindings() {
            let _ = writeln!(text, "let {name} = {expression}");
        }
        if !self.bindings.is_empty() {
            text.push('\n');
        }
        text.push_str(&services);
        text.push_str(&links);
        text.trim_end().to_owned() + "\n"
    }

    fn sorted_bindings(&self) -> Vec<(String, String)> {
        let mut bindings: Vec<(String, String)> = self
            .bindings
            .iter()
            .map(|(expression, name)| (expression.clone(), name.clone()))
            .collect();
        bindings.sort_by(|left, right| left.1.cmp(&right.1));
        bindings
    }

    fn imports(&self) -> String {
        let mut text = String::new();
        for namespace in &self.namespaces {
            let _ = writeln!(text, "import * as {namespace} from lemma.{namespace}");
        }
        if !self.file.graph.links.is_empty() {
            text.push_str("import { Link } from lemma.links\n");
        }
        for (repository, names) in self.imported_services() {
            let written: Vec<String> = names
                .iter()
                .map(|name| {
                    format!(
                        "{} as {}",
                        identifier(name),
                        imported_name(&repository, name)
                    )
                })
                .collect();
            let _ = writeln!(
                text,
                "import {{ {} }} from {}",
                written.join(", "),
                repository_name(&repository)
            );
        }
        if !text.is_empty() {
            text.push('\n');
        }
        text
    }

    /// Services of other repositories that links name, by repository.
    fn imported_services(&self) -> BTreeMap<String, BTreeSet<String>> {
        let mut imported: BTreeMap<String, BTreeSet<String>> = BTreeMap::new();
        let ends = self
            .file
            .graph
            .links
            .iter()
            .filter_map(|text| parse_link(text).ok())
            .flat_map(|link| [link.caller, link.provider])
            .filter_map(|address| Some((address.repository?, address.service)));
        for (repository, service) in ends {
            let repository = repository.trim_start_matches('#').to_owned();
            imported.entry(repository).or_default().insert(service);
        }
        imported
    }

    fn repositories(&self) -> String {
        let mut text = String::new();
        for (name, service) in &self.file.services {
            if let Some(name) = name.strip_prefix('#') {
                let _ = writeln!(
                    text,
                    "repo {} = {}",
                    repository_name(name),
                    repository_source(service)
                );
            }
        }
        if !text.is_empty() {
            text.push('\n');
        }
        text
    }

    fn service(&mut self, name: &str, service: &Service) -> String {
        let mut text = format!("export service {} {{\n", identifier(name));
        for line in self.service_fields(service) {
            let _ = writeln!(text, "    {line}");
        }
        for (operation_name, operation) in &service.exports.operations {
            let is_client = self
                .clients
                .contains(&(name.to_owned(), operation_name.clone()));
            text.push('\n');
            text.push_str(&self.operation(operation_name, operation, is_client));
        }
        text.push_str("}\n\n");
        text
    }

    fn service_fields(&mut self, service: &Service) -> Vec<String> {
        let mut lines = Vec::new();
        let runtime = &service.runtime;
        let mut runtime_fields = Vec::new();
        for (key, value) in [
            ("python", &runtime.python),
            ("node", &runtime.node),
            ("bun", &runtime.bun),
            ("go", &runtime.go),
            ("cargo", &runtime.cargo),
        ] {
            if let Some(value) = value {
                runtime_fields.push(format!("{key}: {}", quoted(value)));
            }
        }
        if !runtime.env.is_empty() {
            runtime_fields.push(format!("env: {}", record(&runtime.env)));
        }
        if !runtime_fields.is_empty() {
            lines.push(format!("runtime: {{ {} }}", runtime_fields.join(", ")));
        }
        let index = &service.index;
        let mut index_fields = Vec::new();
        for (key, values) in [
            ("include", &index.include),
            ("exclude", &index.exclude),
            ("languages", &index.languages),
        ] {
            if !values.is_empty() {
                index_fields.push(format!("{key}: {}", list(values)));
            }
        }
        if !index_fields.is_empty() {
            lines.push(format!("index: {{ {} }}", index_fields.join(", ")));
        }
        lines.extend(self.export_fields(service));
        if service.rejects_unknown_fields {
            lines.push("rejects_unknown_fields: true".to_owned());
        }
        lines
    }

    fn export_fields(&mut self, service: &Service) -> Vec<String> {
        let exports = &service.exports;
        let mut lines = Vec::new();
        let mut providers: Vec<String> = Vec::new();
        for document in &exports.openapi {
            self.namespaces.insert("http");
            providers.push(format!("http.openapi({})", quoted(document)));
        }
        if let Some(from) = &exports.operations_from {
            providers.push(self.operations_from(from));
        }
        match providers.as_slice() {
            [] => {}
            [single] => lines.push(format!("operations: {single}")),
            _ => lines.push(format!("operations: [{}]", providers.join(", "))),
        }
        if let Some(client) = &exports.client {
            self.namespaces.insert("http");
            let mut arguments = vec![format!("generated_from: {}", list(&client.generated_from))];
            if !client.specs.is_empty() {
                arguments.push(format!("specs: {}", list(&client.specs)));
            }
            if !client.call_names.is_empty() {
                arguments.push(format!("call_names: {}", list(&client.call_names)));
            }
            lines.push(format!(
                "client: http.generated_client({})",
                arguments.join(", ")
            ));
        }
        if !exports.symbols.is_empty() {
            lines.push(format!("symbols: {}", list(&exports.symbols)));
        }
        if !exports.routes.register.is_empty() || !exports.routes.define.is_empty() {
            let mut fields = Vec::new();
            if !exports.routes.register.is_empty() {
                fields.push(format!("register: {}", list(&exports.routes.register)));
            }
            if !exports.routes.define.is_empty() {
                fields.push(format!("define: {}", list(&exports.routes.define)));
            }
            lines.push(format!("routes: {{ {} }}", fields.join(", ")));
        }
        lines
    }

    fn operations_from(&mut self, from: &OperationsFrom) -> String {
        let (function, mut arguments) = match (from.adapter.as_deref(), &from.command) {
            (Some("fastapi"), _) => {
                self.namespaces.insert("fastapi");
                ("fastapi.app", Vec::new())
            }
            (Some("hono-zod-openapi") | None, None) => {
                self.namespaces.insert("hono");
                ("hono.app", Vec::new())
            }
            (Some(adapter), _) => {
                self.namespaces.insert("http");
                ("http.adapter", vec![quoted(adapter)])
            }
            (None, Some(command)) => {
                self.namespaces.insert("http");
                (
                    "http.adapter_command",
                    vec![format!("command: {}", quoted(command))],
                )
            }
        };
        arguments.push(format!("entry: {}", list(&from.entry)));
        if let Some(directory) = &from.directory {
            arguments.push(format!("directory: {}", quoted(directory)));
        }
        if let Some(setup) = &from.setup {
            arguments.push(format!("setup: {}", quoted(setup)));
        }
        if !from.env.is_empty() {
            arguments.push(format!("env: {}", record(&from.env)));
        }
        if !from.exclude.is_empty() {
            arguments.push(format!("exclude: {}", list(&from.exclude)));
        }
        if let Some(timeout) = from.timeout_ms {
            arguments.push(format!("timeout_ms: {timeout}"));
        }
        format!("{function}({})", arguments.join(", "))
    }

    fn operation(&mut self, name: &str, operation: &Operation, is_client: bool) -> String {
        let keyword = if is_client {
            "client operation"
        } else {
            "operation"
        };
        let mut text = format!("    export {keyword} {} {{\n", identifier(name));
        if let Some((file, symbol)) = operation.path.split_once(':') {
            let binding = self.code_file(file);
            let _ = writeln!(
                text,
                "        implementation: {binding}.symbol({})",
                quoted(symbol)
            );
        }
        let (request_name, response_name) = if is_client {
            ("sends", "accepts")
        } else {
            ("request", "response")
        };
        if let Some(schema) = &operation.schema {
            let sources = [
                (request_name, &schema.request),
                (response_name, &schema.response),
            ];
            let written = sources
                .into_iter()
                .filter_map(|(field, source)| Some((field, source.as_ref()?)));
            for (field, source) in written {
                let written = self.schema(source, schema.resolver.as_ref());
                let _ = writeln!(text, "        {field}: {written}");
            }
        }
        if !operation.previously.is_empty() {
            let addresses: Vec<String> = operation
                .previously
                .iter()
                .map(|old| quoted(&format!("{}::{}", self.module, old.replace('/', "."))))
                .collect();
            let _ = writeln!(text, "        previously: [{}]", addresses.join(", "));
        }
        text.push_str("    }\n");
        text
    }

    fn schema(&mut self, source: &SchemaSource, resolver: Option<&Resolver>) -> String {
        match source {
            SchemaSource::Code(code) => {
                let (file, type_name) = code.split_once('#').unwrap_or((code, ""));
                let binding = self.code_file(file);
                format!("{binding}.type({})", quoted(type_name))
            }
            SchemaSource::Document(location) => {
                let document = match (&location.file, resolver) {
                    (Some(file), _) => self.document(file, &location.format),
                    (None, Some(resolver)) => self.command(resolver, &location.format),
                    (None, None) => self.document("", &location.format),
                };
                format!("{document}.at({})", quoted(&location.path))
            }
        }
    }

    /// The binding for a code file, made on first use.
    fn code_file(&mut self, file: &str) -> String {
        let namespace = language(file);
        self.namespaces.insert(namespace);
        let expression = format!("{namespace}.file({})", quoted(file));
        let stem = file.rsplit('/').next().unwrap_or(file);
        let stem = stem.split('.').next().unwrap_or(stem);
        self.bind(expression, stem)
    }

    fn document(&mut self, file: &str, format: &str) -> String {
        self.namespaces.insert("json");
        let expression = if format == "json-schema" {
            format!("json.document({})", quoted(file))
        } else {
            format!(
                "json.document({}, format: {})",
                quoted(file),
                quoted(format)
            )
        };
        let stem = file.rsplit('/').next().unwrap_or(file);
        let stem = stem.split('.').next().unwrap_or("document");
        self.bind(expression, stem)
    }

    fn command(&mut self, resolver: &Resolver, format: &str) -> String {
        self.namespaces.insert("json");
        let mut arguments = vec![quoted(&resolver.command)];
        if let Some(output) = &resolver.output {
            arguments.push(format!("output: {}", quoted(output)));
        }
        if !resolver.env.is_empty() {
            arguments.push(format!("env: {}", record(&resolver.env)));
        }
        if !resolver.cache {
            arguments.push("cache: false".to_owned());
        }
        // The format belongs to where the schema is read, but `lemma.yaml`
        // sets it per location; a document reads one format.
        if format != "json-schema" {
            arguments.push(format!("format: {}", quoted(format)));
        }
        self.bind(format!("json.command({})", arguments.join(", ")), "spec")
    }

    fn bind(&mut self, expression: String, stem: &str) -> String {
        if let Some(name) = self.bindings.get(&expression) {
            return name.clone();
        }
        let base = binding_name(stem);
        let mut name = base.clone();
        let mut counter = 2;
        while self.names.contains(&name) {
            name = format!("{base}{counter}");
            counter += 1;
        }
        self.names.insert(name.clone());
        self.bindings.insert(expression, name.clone());
        name
    }

    fn links(&self) -> String {
        if self.file.graph.links.is_empty() {
            return String::new();
        }
        let mut text = "export graph links : Link {\n".to_owned();
        for written in &self.file.graph.links {
            match parse_link(written) {
                Ok(link) => {
                    let _ = writeln!(
                        text,
                        "    {} -> {}",
                        link_end(&link.caller),
                        link_end(&link.provider)
                    );
                }
                Err(_) => {
                    let _ = writeln!(text, "    // could not convert: {written}");
                }
            }
        }
        text.push_str("}\n");
        text
    }
}

/// A link's end as `.lm` writes it: an imported service's local name, or a
/// service of this repository, then the operation.
fn link_end(address: &LinkAddress) -> String {
    let service = match &address.repository {
        Some(repository) => imported_name(repository.trim_start_matches('#'), &address.service),
        None => identifier(&address.service),
    };
    if crate::address::is_route(&address.operation) {
        return format!("{service}[{:?}]", address.operation);
    }
    format!("{service}.{}", identifier(&address.operation))
}

/// A repository's name as a module name: `payments-client` is
/// `payments_client`, since a repository is named by its root module.
pub fn repository_name(name: &str) -> String {
    binding_name(name)
}

/// The local name an imported service gets: its repository and its name.
fn imported_name(repository: &str, service: &str) -> String {
    binding_name(&format!("{repository}_{service}"))
}

fn language(file: &str) -> &'static str {
    match file.rsplit('.').next() {
        Some("py") => "python",
        Some("go") => "go",
        Some("rs") => "rust",
        _ => "typescript",
    }
}

/// A snake-case binding name from a file stem or other text.
fn binding_name(stem: &str) -> String {
    let mut name: String = stem
        .chars()
        .map(|character| {
            if character.is_ascii_alphanumeric() {
                character.to_ascii_lowercase()
            } else {
                '_'
            }
        })
        .collect();
    if name.is_empty() || name.starts_with(|character: char| character.is_ascii_digit()) {
        name.insert(0, '_');
    }
    if is_reserved(&name) {
        name.push('_');
    }
    name
}

fn is_reserved(name: &str) -> bool {
    lemma_lang::syntax::lexer::is_reserved(name)
}

/// A name as written: bare when it is an identifier, in backticks if not.
fn identifier(name: &str) -> String {
    let is_plain = name
        .chars()
        .next()
        .is_some_and(|first| first.is_ascii_alphabetic() || first == '_')
        && name
            .chars()
            .all(|character| character.is_ascii_alphanumeric() || character == '_')
        && !is_reserved(name);
    if is_plain {
        return name.to_owned();
    }
    format!("`{name}`")
}

/// A string as a JSON-escaped literal.
fn quoted(text: &str) -> String {
    let mut written = String::with_capacity(text.len() + 2);
    written.push('"');
    for character in text.chars() {
        match character {
            '"' => written.push_str("\\\""),
            '\\' => written.push_str("\\\\"),
            '\n' => written.push_str("\\n"),
            '\t' => written.push_str("\\t"),
            '\r' => written.push_str("\\r"),
            control if control.is_control() => {
                let _ = write!(written, "\\u{:04x}", u32::from(control));
            }
            other => written.push(other),
        }
    }
    written.push('"');
    written
}

fn list(values: &[String]) -> String {
    let written: Vec<String> = values.iter().map(|value| quoted(value)).collect();
    format!("[{}]", written.join(", "))
}

fn record(values: &BTreeMap<String, String>) -> String {
    let written: Vec<String> = values
        .iter()
        .map(|(key, value)| format!("{}: {}", identifier(key), quoted(value)))
        .collect();
    format!("{{ {} }}", written.join(", "))
}
