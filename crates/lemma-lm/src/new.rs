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
    let scaffold = resolve(&checked, request)?;
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
fn resolve(checked: &Checked, request: &Request<'_>) -> anyhow::Result<Scaffold> {
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
        .map(|link| link_through(model, entry, link))
        .transpose()?;
    let interpreter = (language == ScriptLanguage::Python)
        .then(|| service_python(model, entry, &handler.reference))
        .flatten();
    Ok(Scaffold {
        name: request.name.to_owned(),
        handler,
        through,
        language,
        interpreter,
    })
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
        let known: Vec<String> = own
            .iter()
            .filter(|operation| !operation.is_client)
            .map(|operation| operation.address.dotted_path())
            .collect();
        bail!(
            "the module declares no operation `{reference}`; its operations: {}. An operation \
             only a document describes says nothing of its code: declare it in its service with \
             `export operation <name> {{ implementation: <file>.symbol(\"<handler>\") }}` to run it",
            if known.is_empty() {
                "none".to_owned()
            } else {
                known.join(", ")
            }
        );
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

/// The link `written` names (`orders.charge -> api.createCharge`), in a
/// graph of the module.
fn link_through(model: &Model, entry: &str, written: &str) -> anyhow::Result<Through> {
    let Some((caller, provider)) = written.split_once("->") else {
        bail!("`{written}` is not a link: write `caller -> provider`, as a graph does");
    };
    let (caller, provider) = (caller.trim(), provider.trim());
    // A declared operation, or one a service's documents describe
    // (`api.createCharge`, or `api["POST /v1/charges"]` by route).
    let names = |value: &Value, name: &str| match value {
        Value::Operation(address) => address.dotted_path() == name,
        Value::Discovered { service, key } => {
            let service = service.dotted_path();
            name == format!("{service}.{key}") || name == format!("{service}[\"{key}\"]")
        }
        _ => false,
    };
    let graph = model
        .graphs
        .iter()
        .filter(|graph| graph.address.module == entry)
        .find(|graph| {
            graph.rows.iter().any(|row| {
                let [from, to] = &row[..] else {
                    return false;
                };
                names(from, caller) && names(to, provider)
            })
        });
    let Some(graph) = graph else {
        bail!(
            "no graph of the module holds the link {caller} -> {provider}; declare it first, as \
             `export graph links : Link {{ {caller} -> {provider} }}`"
        );
    };
    Ok(Through {
        graph: graph.address.dotted_path(),
        caller: caller.to_owned(),
        provider: provider.to_owned(),
    })
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

        run(&request(&module, Some("orders.charge -> api.createCharge"))).unwrap();

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
        run(&request(&module, None)).unwrap();

        let error = run(&request(&module, None)).unwrap_err().to_string();

        assert!(error.contains("already"), "{error}");
    }
}
