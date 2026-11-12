//! `lm new behavior`: scaffolds a behavior for an operation of a module,
//! its checker and the checker's script, so writing one does not start
//! from a blank file. What it writes is in [`crate::scaffold`].

use std::path::Path;
use std::process::ExitCode;

use anyhow::{Context, bail};
use lemma_lang::model::{Model, Operation};
use lemma_lang::types::Value;
use lemma_lang::{Checked, Severity};

use crate::scaffold::{Code, Scaffold, ScriptLanguage, Through};

/// What to scaffold, as written on the command line.
pub(crate) struct Request<'a> {
    pub name: &'a str,
    pub handler: &'a str,
    pub through: Option<&'a str>,
    pub module: &'a Path,
}

/// Scaffolds the behavior `request` describes into its module, and its
/// script beside it; prints what it wrote and what to do next.
pub(crate) fn run(request: &Request<'_>) -> anyhow::Result<ExitCode> {
    let (checked, root) = crate::check::load(request.module)?;
    refuse_errors(&checked)?;
    let scaffold = resolve(&checked, &root, request)?;
    let script = root.join(scaffold.script_path());
    if script.exists() {
        bail!(
            "{} already exists; choose another behavior name",
            script.display()
        );
    }
    let original = std::fs::read_to_string(request.module)
        .with_context(|| format!("cannot read {}", request.module.display()))?;
    let appended = format!("{}{}", original.trim_end(), scaffold.module_text());
    let formatted = lemma_lang::syntax::format(&appended).with_context(|| {
        format!(
            "cannot format the scaffold for {}",
            request.module.display()
        )
    })?;
    write(request.module, &formatted)?;
    let (rechecked, _) = crate::check::load(request.module)?;
    if let Err(error) = refuse_errors(&rechecked) {
        write(request.module, &original)?;
        return Err(error.context("the scaffold did not check, so the module was left as it was"));
    }
    write_script(&script, &scaffold)?;
    let shown = request.module.parent().map_or_else(
        || scaffold.script_path().into(),
        |directory| directory.join(scaffold.script_path()),
    );
    print_next_steps(&scaffold, request.module, &shown);
    Ok(ExitCode::SUCCESS)
}

fn refuse_errors(checked: &Checked) -> anyhow::Result<()> {
    let errors: Vec<&str> = checked
        .diagnostics
        .iter()
        .filter(|diagnostic| diagnostic.severity == Severity::Error)
        .map(|diagnostic| diagnostic.message.as_str())
        .collect();
    if errors.is_empty() {
        return Ok(());
    }
    bail!(
        "the module has errors; `lm check` lists them: {}",
        errors.join("; ")
    )
}

/// The scaffold for `request`, from what the module declares.
fn resolve(checked: &Checked, root: &Path, request: &Request<'_>) -> anyhow::Result<Scaffold> {
    let model = &checked.model;
    let entry = checked.loaded.entry.as_str();
    is_free_name(model, entry, request.name)?;
    let handler = handler_code(model, entry, request.handler)?;
    let Some(language) = ScriptLanguage::of(&handler.file) else {
        bail!(
            "{} is implemented in {}, a language `lm new behavior` writes no script for",
            handler.reference,
            handler.file
        );
    };
    let through = request
        .through
        .map(|link| {
            let source = std::fs::read_to_string(request.module).unwrap_or_default();
            link_through(model, entry, &source, link)
        })
        .transpose()?;
    let interpreter = (language == ScriptLanguage::Python)
        .then(|| service_python(model, entry, &handler.reference))
        .flatten();
    let go_module = (language == ScriptLanguage::Go)
        .then(|| go_module(root, &handler.file))
        .flatten();
    Ok(Scaffold {
        name: request.name.to_owned(),
        handler,
        through,
        language,
        interpreter,
        go_module,
    })
}

/// The directory, relative to `root`, of the Go module holding `file`:
/// the nearest directory above it with a `go.mod`.
fn go_module(root: &Path, file: &str) -> Option<String> {
    let mut directory = Path::new(file).parent();
    while let Some(candidate) = directory {
        if root.join(candidate).join("go.mod").exists() {
            return Some(candidate.to_string_lossy().into_owned());
        }
        directory = candidate.parent();
    }
    None
}

/// The `runtime.python` of the service holding the operation `reference`
/// names, which has the service's dependencies installed.
fn service_python(model: &Model, entry: &str, reference: &str) -> Option<String> {
    let (service, _) = reference.rsplit_once('.')?;
    let declared = model.services.iter().find(|declared| {
        declared.address.module == entry && declared.address.dotted_path() == service
    })?;
    let Value::Record(runtime) = declared.fields.get("runtime")? else {
        return None;
    };
    runtime
        .iter()
        .find_map(|(name, value)| match (name.as_str(), value) {
            ("python", Value::Text(python)) => Some(python.clone()),
            _ => None,
        })
}

/// Fails unless `name` is an identifier no behavior or checker of the
/// module has, nor the checker it implies.
fn is_free_name(model: &Model, entry: &str, name: &str) -> anyhow::Result<()> {
    let is_identifier = name
        .chars()
        .next()
        .is_some_and(|first| first.is_ascii_lowercase())
        && name
            .chars()
            .all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '_');
    if !is_identifier {
        bail!("`{name}` is not a behavior name: use lowercase letters, digits and `_`");
    }
    let checker = format!("{name}_run");
    let taken = model
        .behaviors
        .iter()
        .map(|behavior| &behavior.address)
        .chain(model.checkers.iter().map(|declared| &declared.address))
        .filter(|address| address.module == entry)
        .find(|address| [name, checker.as_str()].contains(&address.dotted_path().as_str()));
    if let Some(taken) = taken {
        bail!("{taken} is already declared; choose another behavior name");
    }
    Ok(())
}

/// The module's operation `reference` names, with its implementation.
fn handler_code(model: &Model, entry: &str, reference: &str) -> anyhow::Result<Code> {
    let own: Vec<&Operation> = model
        .services
        .iter()
        .filter(|service| service.address.module == entry)
        .flat_map(|service| &service.operations)
        .collect();
    let Some(operation) = own
        .iter()
        .find(|operation| operation.address.dotted_path() == reference)
    else {
        bail!("{}", unknown_handler(reference, &own));
    };
    let Some((file, symbol)) = lemma_graph::lm::operation_code(operation) else {
        bail!("{reference} has no `implementation`, so there is no code to run");
    };
    Ok(Code {
        reference: reference.to_owned(),
        file,
        symbol,
    })
}

/// Why `reference` names none of the module's operations, listing them.
fn unknown_handler(reference: &str, own: &[&Operation]) -> String {
    let listed = |is_client: bool| {
        let names: Vec<String> = own
            .iter()
            .filter(|operation| operation.is_client == is_client)
            .map(|operation| operation.address.dotted_path())
            .collect();
        if names.is_empty() {
            return "none".to_owned();
        }
        names.join(", ")
    };
    format!(
        "the module declares no operation `{reference}`. Its operations: {}. Its client \
         operations, calls it makes to other services, which can be a handler too: {}. An \
         operation only a document describes says nothing of its code: declare it in its service \
         with `export operation <name> {{ implementation: <file>.symbol(\"<handler>\") }}` to run it",
        listed(false),
        listed(true)
    )
}

/// The link `written` names (`orders.charge -> api.createCharge`), in a
/// graph of the module.
///
/// The provider may be another repository's, which the module names
/// through an import alias (`functions_serverless.submit_task` for
/// functions' `serverless.submit_task`), so it may be given by its own
/// name, and is written in the scaffold as the module's graph writes it.
/// A provider written as the graph writes it is that row; one given by its
/// own name must name one row only.
fn link_through(
    model: &Model,
    entry: &str,
    source: &str,
    written: &str,
) -> anyhow::Result<Through> {
    let Some((caller, provider)) = written.split_once("->") else {
        bail!("`{written}` is not a link: write `caller -> provider`, as a graph does");
    };
    let (caller, provider) = (caller.trim(), provider.trim());
    let wanted = operation_name(provider);
    let graphs = graphs_holding(model, entry, caller, wanted);
    if graphs.is_empty() {
        bail!(
            "no graph of the module holds the link {caller} -> {provider}; declare it first, as \
             `export graph links : Link {{ {caller} -> {provider} }}`"
        );
    }
    let spellings: Vec<String> = graph_rows(source)
        .into_iter()
        .filter(|(from, to)| from == caller && operation_name(to) == wanted)
        .map(|(_, to)| to)
        .collect();
    let chosen = spellings
        .iter()
        .position(|spelling| spelling == provider)
        .or_else(|| (graphs.len() == 1).then_some(0));
    let Some(chosen) = chosen else {
        bail!(
            "{caller} -> {provider} could be any of the links {}; write the provider as the \
             graph does",
            spellings
                .iter()
                .map(|spelling| format!("{caller} -> {spelling}"))
                .collect::<Vec<_>>()
                .join(", ")
        );
    };
    // The rows the source writes are the model's, in order; if they are
    // not (rows written another way), the first graph and the provider as
    // given are the best guess.
    let is_aligned = spellings.len() == graphs.len();
    let graph = graphs[if is_aligned { chosen } else { 0 }].clone();
    let provider = spellings
        .get(chosen)
        .cloned()
        .unwrap_or_else(|| provider.to_owned());
    Ok(Through {
        graph,
        caller: caller.to_owned(),
        provider,
    })
}

/// The graph of each row of the module's graphs from `caller` to an
/// operation named `wanted`, in the order the rows are written. The caller
/// is the module's own operation; the provider a declared operation, or
/// one a service's documents describe (`api.createCharge`, or
/// `api["POST /v1/charges"]` by route).
fn graphs_holding(model: &Model, entry: &str, caller: &str, wanted: &str) -> Vec<String> {
    let names = |value: &Value| match value {
        Value::Operation(address) => address.dotted_path() == caller,
        _ => false,
    };
    let is_wanted = |value: &Value| match value {
        Value::Operation(address) => address.path.last().map(String::as_str) == Some(wanted),
        Value::Discovered { key, .. } => key == wanted,
        _ => false,
    };
    model
        .graphs
        .iter()
        .filter(|graph| graph.address.module == entry)
        .flat_map(|graph| {
            graph.rows.iter().filter_map(move |row| {
                let [from, to] = &row[..] else {
                    return None;
                };
                (names(from) && is_wanted(to)).then(|| graph.address.dotted_path())
            })
        })
        .collect()
}

/// An operation's own name in a reference: `submit_task` in
/// `functions_serverless.submit_task`, the route in `api["POST /x"]`.
fn operation_name(reference: &str) -> &str {
    if let Some(route) = reference
        .split_once("[\"")
        .and_then(|(_, rest)| rest.strip_suffix("\"]"))
    {
        return route;
    }
    reference.rsplit('.').next().unwrap_or(reference)
}

/// The `caller -> provider` rows the module's source writes, in order,
/// without those commented out; rows may share a line with their graph's
/// braces or each other.
fn graph_rows(source: &str) -> Vec<(String, String)> {
    source
        .lines()
        .map(|line| line.split("//").next().unwrap_or_default())
        .flat_map(|line| line.split(['{', '}', ',']))
        .filter_map(|part| {
            let (from, to) = part.split_once("->")?;
            Some((from.trim().to_owned(), to.trim().to_owned()))
        })
        .collect()
}

fn write(path: &Path, text: &str) -> anyhow::Result<()> {
    std::fs::write(path, text).with_context(|| format!("cannot write {}", path.display()))
}

fn write_script(script: &Path, scaffold: &Scaffold) -> anyhow::Result<()> {
    if let Some(directory) = script.parent() {
        std::fs::create_dir_all(directory)
            .with_context(|| format!("cannot create {}", directory.display()))?;
    }
    write(script, &scaffold.script_text())
}

/// What was written, `script` as the caller would open it, and what to do
/// next.
fn print_next_steps(scaffold: &Scaffold, module: &Path, script: &Path) {
    let name = &scaffold.name;
    println!(
        "added checker {} and behavior {name} to {}",
        scaffold.checker(),
        module.display()
    );
    println!("wrote {}", script.display());
    println!("next:");
    println!(
        "  1. in {}, fill in the TODOs: the input a case sends{}",
        script.display(),
        if scaffold.through.is_some() {
            " and what the replaced call answers"
        } else {
            ""
        }
    );
    println!("  2. in the behavior, say what must hold, then remove `severity: warning`");
    println!("  3. run it alone: lemma check --select {name}");
}

#[cfg(test)]
mod tests {
    use super::*;

    const BILLING: &str = "module billing\n\nimport * as http from lemma.http\n\nexport service api {\n    operations: http.openapi(\"openapi.json\")\n}\n";

    const SPEC: &str = r#"{ "openapi": "3.0.0", "info": { "title": "B", "version": "1" }, "paths": { "/v1/charges": { "post": { "operationId": "createCharge", "responses": { "200": { "description": "ok" } } } } } }"#;

    const ORDERS: &str = "module orders\n\nimport * as python from lemma.python\nimport { Link } from lemma.links\nimport { api } from billing\n\nrepo billing = local(\"../billing\")\n\nlet client = python.file(\"orders/billing.py\")\nlet handlers = python.file(\"orders/handlers.py\")\n\nexport service orders {\n    client operation charge {\n        implementation: client.symbol(\"charge\")\n    }\n\n    export operation place_order {\n        implementation: handlers.symbol(\"place_order\")\n    }\n}\n\nexport graph links : Link {\n    orders.charge -> api.createCharge\n}\n";

    /// Billing and orders side by side; the orders module's path.
    fn shop(directory: &Path) -> std::path::PathBuf {
        let billing = directory.join("billing");
        std::fs::create_dir_all(&billing).unwrap();
        std::fs::write(billing.join("mod.lm"), BILLING).unwrap();
        std::fs::write(billing.join("openapi.json"), SPEC).unwrap();
        let orders = directory.join("orders");
        std::fs::create_dir_all(orders.join("orders")).unwrap();
        std::fs::write(
            orders.join("orders/billing.py"),
            "def charge(c):\n    pass\n",
        )
        .unwrap();
        std::fs::write(
            orders.join("orders/handlers.py"),
            "def place_order(o):\n    pass\n",
        )
        .unwrap();
        std::fs::write(orders.join("mod.lm"), ORDERS).unwrap();
        orders.join("mod.lm")
    }

    fn request<'a>(module: &'a Path, through: Option<&'a str>) -> Request<'a> {
        Request {
            name: "declined",
            handler: "orders.place_order",
            through,
            module,
        }
    }

    #[test]
    fn a_scaffolded_behavior_checks_and_its_script_is_written() {
        let directory = tempfile::tempdir().unwrap();
        let module = shop(directory.path());

        let _ = run(&request(&module, Some("orders.charge -> api.createCharge"))).unwrap();

        let (checked, root) = crate::check::load(&module).unwrap();
        assert!(refuse_errors(&checked).is_ok());
        assert!(root.join("tools/declined.py").exists());
        let text = std::fs::read_to_string(&module).unwrap();
        assert!(text.contains("let link = links[orders.charge, api.createCharge]"));
    }

    #[test]
    fn an_operation_the_module_does_not_declare_is_refused_and_nothing_is_written() {
        let directory = tempfile::tempdir().unwrap();
        let module = shop(directory.path());
        let missing = Request {
            handler: "orders.refund",
            ..request(&module, None)
        };

        let error = run(&missing).unwrap_err().to_string();

        assert!(error.contains("no operation `orders.refund`"), "{error}");
        assert_eq!(std::fs::read_to_string(&module).unwrap(), ORDERS);
    }

    /// The orders module importing billing's service under an alias.
    fn aliased_shop(directory: &Path) -> std::path::PathBuf {
        let module = shop(directory);
        let aliased = ORDERS
            .replace(
                "import { api } from billing",
                "import { api as billing_api } from billing",
            )
            .replace(
                "orders.charge -> api.createCharge",
                "orders.charge -> billing_api.createCharge",
            );
        std::fs::write(&module, aliased).unwrap();
        module
    }

    #[test]
    fn a_link_to_another_repository_is_found_through_its_import_alias() {
        let directory = tempfile::tempdir().unwrap();
        let module = aliased_shop(directory.path());

        let _ = run(&request(
            &module,
            Some("orders.charge -> billing_api.createCharge"),
        ))
        .unwrap();

        let text = std::fs::read_to_string(&module).unwrap();
        assert!(
            text.contains("links[orders.charge, billing_api.createCharge]"),
            "{text}"
        );
    }

    #[test]
    fn a_provider_written_by_its_own_name_is_written_as_the_graph_writes_it() {
        let directory = tempfile::tempdir().unwrap();
        let module = aliased_shop(directory.path());

        let _ = run(&request(&module, Some("orders.charge -> api.createCharge"))).unwrap();

        let text = std::fs::read_to_string(&module).unwrap();
        assert!(
            text.contains("links[orders.charge, billing_api.createCharge]"),
            "{text}"
        );
    }

    #[test]
    fn a_provider_written_as_the_graph_does_is_that_row_though_another_shares_its_name() {
        let directory = tempfile::tempdir().unwrap();
        let module = shop(directory.path());
        let two = ORDERS
            .replace(
                "import { api } from billing",
                "import { api } from billing\nimport { api as ledger_api } from billing",
            )
            .replace(
                "    orders.charge -> api.createCharge\n",
                "    orders.charge -> ledger_api.createCharge\n    // orders.charge -> old.createCharge\n    orders.charge -> api.createCharge\n",
            );
        std::fs::write(&module, two).unwrap();

        let _ = run(&request(&module, Some("orders.charge -> api.createCharge"))).unwrap();

        let text = std::fs::read_to_string(&module).unwrap();
        assert!(
            text.contains("links[orders.charge, api.createCharge]"),
            "{text}"
        );
    }

    #[test]
    fn a_graph_on_one_line_holds_its_link() {
        let directory = tempfile::tempdir().unwrap();
        let module = aliased_shop(directory.path());
        let text = std::fs::read_to_string(&module).unwrap().replace(
            "export graph links : Link {\n    orders.charge -> billing_api.createCharge\n}",
            "export graph links : Link { orders.charge -> billing_api.createCharge }",
        );
        std::fs::write(&module, text).unwrap();

        let _ = run(&request(&module, Some("orders.charge -> api.createCharge"))).unwrap();

        let text = std::fs::read_to_string(&module).unwrap();
        assert!(
            text.contains("links[orders.charge, billing_api.createCharge]"),
            "{text}"
        );
    }

    #[test]
    fn a_link_no_graph_holds_is_refused() {
        let directory = tempfile::tempdir().unwrap();
        let module = shop(directory.path());

        let error = run(&request(&module, Some("orders.charge -> api.refund")))
            .unwrap_err()
            .to_string();

        assert!(error.contains("no graph of the module holds"), "{error}");
    }

    #[test]
    fn a_name_already_taken_is_refused() {
        let directory = tempfile::tempdir().unwrap();
        let module = shop(directory.path());
        let _ = run(&request(&module, None)).unwrap();

        let error = run(&request(&module, None)).unwrap_err().to_string();

        assert!(error.contains("already"), "{error}");
    }
}
