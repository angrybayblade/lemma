//! `.lm` modules as `lemma check` reads them: a checked program lowered into
//! one [`LemmaFile`] per repository, one set of structures, so checks, links and inference run unchanged.
//!
//! Services nested in a service that holds nothing else (no operations,
//! providers or client) are containers: their fields pass down to the
//! services inside, and only those are lowered, named by their path joined
//! with `/` (`Functions/serverless`). See `.specs/13-lm-grammar.md` §13.

mod sources;

use std::collections::BTreeMap;

use lemma_lang::Checked;
use lemma_lang::load::RepositorySource;
use lemma_lang::model::{self, Model};
use lemma_lang::types::{Address, Value};

use crate::config::{
    DocumentLocation, Exports, GeneratedClient, Graph, IndexSettings, LemmaFile, Level, Operation,
    OperationsFrom, Resolver, Routes, Runtime, SchemaSource, Schemas, Service,
};

pub use sources::{CheckoutSources, read_manifest};

/// The relation links are facts of.
const LINK: (&str, &str) = ("lemma.links", "Link");

/// Why a checked program cannot be lowered for `lemma check`.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum LowerError {
    /// The program says something `lemma check` cannot express yet.
    #[error("{what} cannot be checked by `lemma check` yet")]
    Unsupported {
        /// What, in a phrase naming the declaration.
        what: String,
    },
    /// A value is not what its field needs, which checking should have
    /// reported first.
    #[error("{what} is not a {expected}")]
    Shape {
        /// The field.
        what: String,
        /// What it should be.
        expected: &'static str,
    },
}

/// The structures `repository`'s modules describe: its
/// services, the links its graphs declare, and the repositories its
/// modules pin.
pub fn repository_file(checked: &Checked, repository: &str) -> Result<LemmaFile, LowerError> {
    let model = &checked.model;
    let mut file = LemmaFile::default();
    for service in model
        .services
        .iter()
        .filter(|service| service.repository == repository)
    {
        if is_container(model, service) {
            continue;
        }
        let fields = inherited_fields(model, service);
        let lowered = lower_service(service, &fields)?;
        file.services.insert(service_key(&service.address), lowered);
    }
    for (name, external) in externals(checked, repository) {
        file.services.insert(name, external);
    }
    file.graph = Graph {
        links: links(checked, repository)?,
        level: level(checked, repository),
    };
    Ok(file)
}

/// The name a repository's root module has: its directory's name as an
/// identifier, letters, digits and `_`.
pub fn repository_name(name: &str) -> String {
    let mut identifier: String = name
        .chars()
        .map(|character| {
            if character.is_ascii_alphanumeric() {
                character.to_ascii_lowercase()
            } else {
                '_'
            }
        })
        .collect();
    if identifier.is_empty() || identifier.starts_with(|character: char| character.is_ascii_digit())
    {
        identifier.insert(0, '_');
    }
    if lemma_lang::syntax::lexer::is_reserved(&identifier) {
        identifier.push('_');
    }
    identifier
}

/// The name a service gets in a lowered description: its path joined with `/`.
pub fn service_key(address: &Address) -> String {
    address.path.join("/")
}

fn is_container(model: &Model, service: &model::Service) -> bool {
    let has_children = model
        .services
        .iter()
        .any(|other| is_parent(&service.address, &other.address));
    let holds_operations = !service.operations.is_empty()
        || service.fields.contains_key("operations")
        || service.fields.contains_key("client");
    has_children && !holds_operations
}

fn is_parent(parent: &Address, child: &Address) -> bool {
    child.module == parent.module
        && child.path.len() == parent.path.len() + 1
        && child.path.starts_with(&parent.path)
}

/// A service's fields over those of the services around it.
fn inherited_fields(model: &Model, service: &model::Service) -> BTreeMap<String, Value> {
    let mut fields = BTreeMap::new();
    for depth in 1..service.address.path.len() {
        let ancestor = Address {
            module: service.address.module.clone(),
            path: service.address.path[..depth].to_vec(),
        };
        let found = model
            .services
            .iter()
            .find(|other| other.address == ancestor);
        for (name, value) in found.map(|other| &other.fields).into_iter().flatten() {
            if name != "operations" && name != "client" {
                fields.insert(name.clone(), value.clone());
            }
        }
    }
    fields.extend(service.fields.clone());
    fields
}

fn lower_service(
    service: &model::Service,
    fields: &BTreeMap<String, Value>,
) -> Result<Service, LowerError> {
    let what = |field: &str| format!("`{field}` of `{}`", service.address);
    let mut lowered = Service {
        index: fields
            .get("index")
            .map(|value| index(value, &what("index")))
            .transpose()?
            .unwrap_or_default(),
        runtime: fields
            .get("runtime")
            .map(|value| runtime(value, &what("runtime")))
            .transpose()?
            .unwrap_or_default(),
        rejects_unknown_fields: matches!(
            fields.get("rejects_unknown_fields"),
            Some(Value::Bool(true))
        ),
        exports: Exports::default(),
        ..Service::default()
    };
    if let Some(value) = fields.get("operations") {
        providers(&mut lowered.exports, value, &what("operations"))?;
    }
    if let Some(value) = fields.get("client") {
        lowered.exports.client = Some(client(value, &what("client"))?);
    }
    if let Some(value) = fields.get("symbols") {
        lowered.exports.symbols = texts(value, &what("symbols"))?;
    }
    if let Some(value) = fields.get("routes") {
        lowered.exports.routes = Routes {
            register: optional_texts(value.field("register"), &what("routes"))?,
            define: optional_texts(value.field("define"), &what("routes"))?,
        };
    }
    for operation in &service.operations {
        let lowered_operation = lower_operation(operation)?;
        lowered
            .exports
            .operations
            .insert(operation.address.name().to_owned(), lowered_operation);
    }
    Ok(lowered)
}

fn index(value: &Value, what: &str) -> Result<IndexSettings, LowerError> {
    Ok(IndexSettings {
        include: optional_texts(value.field("include"), what)?,
        exclude: optional_texts(value.field("exclude"), what)?,
        languages: optional_texts(value.field("languages"), what)?,
    })
}

fn runtime(value: &Value, what: &str) -> Result<Runtime, LowerError> {
    let text = |name: &str| {
        value
            .field(name)
            .and_then(Value::as_text)
            .map(str::to_owned)
    };
    Ok(Runtime {
        python: text("python"),
        node: text("node"),
        bun: text("bun"),
        go: text("go"),
        cargo: text("cargo"),
        env: environment(value.field("env"), what)?,
    })
}

/// A service's operation providers, laid into its exports.
fn providers(exports: &mut Exports, value: &Value, what: &str) -> Result<(), LowerError> {
    let Value::List(providers) = value else {
        return Err(shape(what, "list of providers"));
    };
    for provider in providers {
        let Value::Request { function, .. } = provider else {
            return Err(shape(what, "provider"));
        };
        match function.as_str() {
            "lemma.http.openapi" => exports
                .openapi
                .push(text_argument(provider, "document", what)?),
            "lemma.hono.app"
            | "lemma.fastapi.app"
            | "lemma.http.adapter"
            | "lemma.http.adapter_command" => {
                if exports.operations_from.is_some() {
                    return Err(unsupported(&format!("more than one adapter in {what}")));
                }
                exports.operations_from = Some(operations_from(provider, function, what)?);
            }
            other => return Err(unsupported(&format!("the provider `{other}` in {what}"))),
        }
    }
    Ok(())
}

fn operations_from(
    provider: &Value,
    function: &str,
    what: &str,
) -> Result<OperationsFrom, LowerError> {
    let adapter = match function {
        "lemma.hono.app" => Some("hono-zod-openapi".to_owned()),
        "lemma.fastapi.app" => Some("fastapi".to_owned()),
        "lemma.http.adapter" => Some(text_argument(provider, "name", what)?),
        _ => None,
    };
    let adapter_command = provider
        .argument("command")
        .map(|value| command(value, what))
        .transpose()?;
    Ok(OperationsFrom {
        adapter,
        command: adapter_command,
        entry: optional_texts(provider.argument("entry"), what)?,
        directory: provider
            .argument("directory")
            .and_then(Value::as_text)
            .map(str::to_owned),
        setup: provider
            .argument("setup")
            .map(|value| command(value, what))
            .transpose()?,
        env: environment(provider.argument("env"), what)?,
        exclude: optional_texts(provider.argument("exclude"), what)?,
        timeout_ms: match provider.argument("timeout_ms") {
            Some(Value::Int(milliseconds)) => u64::try_from(*milliseconds).ok(),
            _ => None,
        },
    })
}

fn client(value: &Value, what: &str) -> Result<GeneratedClient, LowerError> {
    Ok(GeneratedClient {
        generated_from: optional_texts(value.argument("generated_from"), what)?,
        specs: optional_texts(value.argument("specs"), what)?,
        call_names: optional_texts(value.argument("call_names"), what)?,
    })
}

fn lower_operation(operation: &model::Operation) -> Result<Operation, LowerError> {
    let what = format!("operation `{}`", operation.address);
    let path = match operation.fields.get("implementation") {
        Some(implementation) => code_path(implementation, &what)?,
        None => return Err(unsupported(&format!("{what} without an `implementation`"))),
    };
    let (request_field, response_field) = if operation.is_client {
        ("sends", "accepts")
    } else {
        ("request", "response")
    };
    let request = operation.fields.get(request_field);
    let response = operation.fields.get(response_field);
    let schema = if request.is_none() && response.is_none() {
        None
    } else {
        Some(schemas(request, response, &what)?)
    };
    let previously = optional_texts(operation.fields.get("previously"), &what)?
        .iter()
        .filter_map(|old| old_place(old))
        .collect();
    Ok(Operation {
        path,
        schema,
        previously,
    })
}

/// Where an old address (`module::Service.op`) puts an operation, as
/// `previously` in a description writes it: `Service.op`.
fn old_place(address: &str) -> Option<String> {
    let path = address.split_once("::").map_or(address, |(_, path)| path);
    let (service, operation) = path.rsplit_once('.')?;
    Some(format!("{}.{operation}", service.replace('.', "/")))
}

/// `file:Symbol` for an operation's `implementation`, if it names one.
pub(crate) fn implementation_path(implementation: &Value) -> Option<String> {
    code_path(implementation, "an implementation").ok()
}

/// The file and symbol of a declared operation's `implementation`, as a
/// checker receives them; `None` without one.
pub fn operation_code(operation: &lemma_lang::model::Operation) -> Option<(String, String)> {
    let path = implementation_path(operation.fields.get("implementation")?)?;
    let (file, symbol) = path.split_once(':')?;
    Some((file.to_owned(), symbol.to_owned()))
}

/// `file:Symbol` for a resolution request.
fn code_path(implementation: &Value, what: &str) -> Result<String, LowerError> {
    let Value::Request { function, .. } = implementation else {
        return Err(shape(what, "resolution request"));
    };
    let file = source_file(implementation.argument("receiver"), what)?;
    let symbol = match function.as_str() {
        "symbol" => text_argument(implementation, "name", what)?,
        "method" => format!(
            "{}.{}",
            text_argument(implementation, "type", what)?,
            text_argument(implementation, "name", what)?
        ),
        _ => return Err(shape(what, "symbol or method")),
    };
    Ok(format!("{file}:{symbol}"))
}

/// The path a `file(...)` or `module(...)` request names.
fn source_file(receiver: Option<&Value>, what: &str) -> Result<String, LowerError> {
    let Some(receiver @ Value::Request { function, .. }) = receiver else {
        return Err(shape(what, "source file"));
    };
    if function == "lemma.python.module" {
        let name = text_argument(receiver, "name", what)?;
        return Ok(format!("{}.py", name.replace('.', "/")));
    }
    if function.rsplit('.').next() == Some("file") {
        return text_argument(receiver, "path", what);
    }
    Err(unsupported(&format!("`{function}` in {what}")))
}

fn schemas(
    request: Option<&Value>,
    response: Option<&Value>,
    what: &str,
) -> Result<Schemas, LowerError> {
    let mut resolver = None;
    let mut source = |value: Option<&Value>| -> Result<Option<SchemaSource>, LowerError> {
        let Some(value) = value else {
            return Ok(None);
        };
        let (schema, command) = schema_source(value, what)?;
        if let Some(command) = command {
            if resolver
                .as_ref()
                .is_some_and(|existing| existing != &command)
            {
                return Err(unsupported(&format!(
                    "two different commands for the schemas of {what}"
                )));
            }
            resolver = Some(command);
        }
        Ok(Some(schema))
    };
    let request = source(request)?;
    let response = source(response)?;
    Ok(Schemas {
        resolver,
        request,
        response,
    })
}

/// A schema source, and the command it reads when it reads one.
fn schema_source(
    value: &Value,
    what: &str,
) -> Result<(SchemaSource, Option<Resolver>), LowerError> {
    let Value::Request { function, .. } = value else {
        return Err(shape(what, "schema"));
    };
    match function.as_str() {
        "type" => {
            let file = source_file(value.argument("receiver"), what)?;
            let name = text_argument(value, "name", what)?;
            Ok((SchemaSource::Code(format!("{file}#{name}")), None))
        }
        "at" => document_location(value, what),
        _ => Err(shape(what, "schema")),
    }
}

fn document_location(
    value: &Value,
    what: &str,
) -> Result<(SchemaSource, Option<Resolver>), LowerError> {
    let path = text_argument(value, "path", what)?;
    let Some(document @ Value::Request { function, .. }) = value.argument("receiver") else {
        return Err(shape(what, "document"));
    };
    let format = document
        .argument("format")
        .and_then(Value::as_text)
        .unwrap_or("json-schema")
        .to_owned();
    if function == "lemma.json.document" {
        let location = DocumentLocation {
            format,
            path,
            file: Some(text_argument(document, "file", what)?),
        };
        return Ok((SchemaSource::Document(location), None));
    }
    let resolver = Resolver {
        command: command(document.argument("command").unwrap_or(&Value::Null), what)?,
        output: document
            .argument("output")
            .and_then(Value::as_text)
            .map(str::to_owned),
        env: environment(document.argument("env"), what)?,
        cache: !matches!(document.argument("cache"), Some(Value::Bool(false))),
    };
    let location = DocumentLocation {
        format,
        path,
        file: None,
    };
    Ok((SchemaSource::Document(location), Some(resolver)))
}

/// A command as `sh -c` text: a string as written, or an argument list
/// quoted for the shell.
fn command(value: &Value, what: &str) -> Result<String, LowerError> {
    match value {
        Value::Text(text) => Ok(text.clone()),
        Value::List(arguments) => {
            let quoted: Vec<String> = arguments
                .iter()
                .map(|argument| {
                    argument
                        .as_text()
                        .map(shell_quote)
                        .ok_or_else(|| shape(what, "command"))
                })
                .collect::<Result<_, _>>()?;
            Ok(quoted.join(" "))
        }
        _ => Err(shape(what, "command")),
    }
}

fn shell_quote(argument: &str) -> String {
    let is_plain = !argument.is_empty()
        && argument
            .chars()
            .all(|character| character.is_ascii_alphanumeric() || "-_./=:@%+,".contains(character));
    if is_plain {
        return argument.to_owned();
    }
    format!("'{}'", argument.replace('\'', "'\\''"))
}

fn environment(value: Option<&Value>, what: &str) -> Result<BTreeMap<String, String>, LowerError> {
    let Some(value) = value else {
        return Ok(BTreeMap::new());
    };
    let Value::Record(fields) = value else {
        return Err(shape(what, "record of environment variables"));
    };
    fields
        .iter()
        .map(|(name, value)| {
            let text = match value {
                Value::Text(text) => text.clone(),
                Value::Int(number) => number.to_string(),
                Value::Bool(flag) => flag.to_string(),
                _ => return Err(shape(what, "record of strings")),
            };
            Ok((name.clone(), text))
        })
        .collect()
}

/// The repositories `repository`'s modules pin, as external services.
fn externals(checked: &Checked, repository: &str) -> Vec<(String, Service)> {
    let mut found = Vec::new();
    for declared in checked.loaded.repositories.values() {
        let external = match &declared.source {
            Some(RepositorySource::Local {
                path,
                rev,
                declared_in,
                declaring_file,
            }) if declared_in == repository => Service {
                path: Some(relative_to_root(declaring_file, path)),
                reference: rev.clone(),
                ..Service::default()
            },
            Some(RepositorySource::Git {
                url,
                rev,
                declared_in,
            }) if declared_in == repository => Service {
                git: Some(url.clone()),
                reference: rev.clone(),
                ..Service::default()
            },
            _ => continue,
        };
        found.push((format!("#{}", declared.name), external));
    }
    found
}

/// A path written relative to a declaring file, made relative to its
/// repository's root.
fn relative_to_root(declaring_file: &str, path: &str) -> String {
    match declaring_file.rsplit_once('/') {
        Some((directory, _)) => format!("{directory}/{path}"),
        None => path.to_owned(),
    }
}

/// The links graphs in `repository`'s modules declare, as a description
/// writes them.
fn links(checked: &Checked, repository: &str) -> Result<Vec<String>, LowerError> {
    let link = Address::top(LINK.0, LINK.1);
    let mut written = Vec::new();
    for graph in &checked.model.graphs {
        let in_repository = checked
            .loaded
            .modules
            .get(&graph.address.module)
            .is_some_and(|module| module.repository == repository);
        if graph.relation != link || !in_repository {
            continue;
        }
        for row in &graph.rows {
            let [caller, provider] = row.as_slice() else {
                continue;
            };
            let caller = operation_address(checked, caller, repository)?;
            let provider = operation_address(checked, provider, repository)?;
            written.push(format!("{caller} -> {provider}"));
        }
    }
    Ok(written)
}

/// `[#repository:]services.<service>.<operation>` for an operation value.
fn operation_address(
    checked: &Checked,
    value: &Value,
    repository: &str,
) -> Result<String, LowerError> {
    let (service, operation) = match value {
        Value::Operation(address) => {
            let mut service = address.clone();
            service.path.pop();
            (service, address.name().to_owned())
        }
        Value::Discovered { service, key } => (service.clone(), key.clone()),
        _ => return Err(shape("a link's end", "operation")),
    };
    // A module of a repository that is not here is known only by its name.
    let owner = checked.loaded.modules.get(&service.module).map_or_else(
        || service.module.split('.').next().unwrap_or(repository),
        |module| module.repository.as_str(),
    );
    let address = format!("services.{}.{operation}", service_key(&service));
    if owner == repository {
        return Ok(address);
    }
    Ok(format!("#{owner}:{address}"))
}

fn level(checked: &Checked, repository: &str) -> Option<Level> {
    let entry_repository = checked
        .loaded
        .modules
        .get(&checked.loaded.entry)
        .map(|module| module.repository.as_str());
    if entry_repository != Some(repository) {
        return None;
    }
    match checked.model.workspace.as_ref()?.level.as_deref()? {
        "audit" => Some(Level::Audit),
        "strict" => Some(Level::Strict),
        _ => Some(Level::Check),
    }
}

fn text_argument(request: &Value, name: &str, what: &str) -> Result<String, LowerError> {
    request
        .argument(name)
        .and_then(Value::as_text)
        .map(str::to_owned)
        .ok_or_else(|| shape(what, "text"))
}

fn texts(value: &Value, what: &str) -> Result<Vec<String>, LowerError> {
    let Value::List(items) = value else {
        return Err(shape(what, "list of strings"));
    };
    items
        .iter()
        .map(|item| {
            item.as_text()
                .map(str::to_owned)
                .ok_or_else(|| shape(what, "list of strings"))
        })
        .collect()
}

fn optional_texts(value: Option<&Value>, what: &str) -> Result<Vec<String>, LowerError> {
    value.map_or_else(|| Ok(Vec::new()), |value| texts(value, what))
}

fn shape(what: &str, expected: &'static str) -> LowerError {
    LowerError::Shape {
        what: what.to_owned(),
        expected,
    }
}

fn unsupported(what: &str) -> LowerError {
    LowerError::Unsupported {
        what: what.to_owned(),
    }
}

#[cfg(test)]
mod tests;
