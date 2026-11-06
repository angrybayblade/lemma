//! `lemma lm`: checking `.lm` modules, converting `lemma.yaml` to them and
//! serving them to editors. Formatting is in [`crate::formatting`].

use std::io::Write;
use std::path::{Path, PathBuf};
use std::process::ExitCode;

use anyhow::Context;
use lemma_graph::repos::{Checkout, cache_directory};
use lemma_lang::{Checked, Severity, check_program};

use crate::cli::{LmAction, LmArgs};

pub(crate) fn run(args: &LmArgs, json: bool) -> ExitCode {
    let outcome = match &args.action {
        LmAction::Check { file } => check(file, json),
        LmAction::Fmt { paths, check } => crate::formatting::run(paths, *check),
        LmAction::Convert {
            file,
            module,
            write,
        } => convert(file, module.as_deref(), *write).map(|()| ExitCode::SUCCESS),
        LmAction::Lsp => serve().map(|()| ExitCode::SUCCESS),
    };
    match outcome {
        Ok(code) => code,
        Err(error) => {
            eprintln!("error: {error:#}");
            ExitCode::FAILURE
        }
    }
}

/// Serves an editor on standard input and output until it exits.
fn serve() -> anyhow::Result<()> {
    let input = std::io::BufReader::new(std::io::stdin());
    let stdout = std::io::stdout();
    lemma_lang_server::serve(input, &mut stdout.lock(), &cache_directory())?;
    Ok(())
}

/// Loads and checks the program `file` is the entry module of.
pub(crate) fn load(file: &Path) -> anyhow::Result<(Checked, PathBuf)> {
    let absolute =
        std::path::absolute(file).with_context(|| format!("cannot find {}", file.display()))?;
    let root = absolute.parent().map(Path::to_path_buf).unwrap_or_default();
    let name = absolute
        .file_name()
        .map(|name| name.to_string_lossy().into_owned())
        .unwrap_or_default();
    let checkout = Checkout {
        root: root.clone(),
        commit: None,
    };
    let cache = cache_directory();
    let mut sources = lemma_graph::lm::CheckoutSources::new(checkout, &cache);
    Ok((check_program(&mut sources, &name), root))
}

fn check(file: &Path, json: bool) -> anyhow::Result<ExitCode> {
    let (checked, root) = load(file)?;
    let mut stdout = std::io::stdout().lock();
    if json {
        print_json(&mut stdout, &checked, &root)?;
    } else {
        print_text(&mut stdout, &checked, &root)?;
    }
    let has_errors = checked
        .diagnostics
        .iter()
        .any(|diagnostic| diagnostic.severity == Severity::Error);
    Ok(if has_errors {
        ExitCode::FAILURE
    } else {
        ExitCode::SUCCESS
    })
}

/// Where a diagnostic's module is, for messages: a path under the entry's
/// directory when the module is in the entry's repository.
fn location(checked: &Checked, root: &Path, module: &str, start: u32) -> (String, u32, u32) {
    let Some(loaded) = checked.loaded.modules.get(module) else {
        return (module.to_owned(), 1, 1);
    };
    let position = loaded.lines.position(start);
    let entry_repository = checked
        .loaded
        .modules
        .get(&checked.loaded.entry)
        .map(|entry| entry.repository.as_str());
    let shown = if Some(loaded.repository.as_str()) == entry_repository {
        root.join(&loaded.path).display().to_string()
    } else {
        format!("{} ({module})", loaded.path)
    };
    (shown, position.line + 1, position.column + 1)
}

fn print_text(out: &mut impl Write, checked: &Checked, root: &Path) -> anyhow::Result<()> {
    for diagnostic in &checked.diagnostics {
        let (path, line, column) =
            location(checked, root, &diagnostic.module, diagnostic.span.start);
        let severity = match diagnostic.severity {
            Severity::Error => "error",
            Severity::Warning => "warning",
            Severity::Note => "note",
        };
        writeln!(
            out,
            "{path}:{line}:{column}: {severity}: {}",
            diagnostic.message
        )?;
    }
    let modules = checked.loaded.modules.len();
    let errors = checked
        .diagnostics
        .iter()
        .filter(|diagnostic| diagnostic.severity == Severity::Error)
        .count();
    let model = &checked.model;
    writeln!(
        out,
        "{modules} modules: {} services, {} relations, {} rules, {} requirements, {} graphs, {} behaviors, {} checkers, {} allowances; {errors} errors",
        model.services.len(),
        model.relations.len(),
        model.rules.len(),
        model.requirements.len(),
        model.graphs.len(),
        model.behaviors.len(),
        model.checkers.len(),
        model.allowances.len()
    )?;
    Ok(())
}

fn print_json(out: &mut impl Write, checked: &Checked, root: &Path) -> anyhow::Result<()> {
    let diagnostics: Vec<serde_json::Value> = checked
        .diagnostics
        .iter()
        .map(|diagnostic| {
            let (path, line, column) =
                location(checked, root, &diagnostic.module, diagnostic.span.start);
            serde_json::json!({
                "severity": format!("{:?}", diagnostic.severity).to_lowercase(),
                "module": diagnostic.module,
                "path": path,
                "line": line,
                "column": column,
                "message": diagnostic.message,
            })
        })
        .collect();
    let report = serde_json::json!({
        "entry": checked.loaded.entry,
        "modules": checked.loaded.modules.keys().collect::<Vec<_>>(),
        "diagnostics": diagnostics,
    });
    writeln!(out, "{}", serde_json::to_string_pretty(&report)?)?;
    Ok(())
}

fn convert(file: &Path, module: Option<&str>, write: bool) -> anyhow::Result<()> {
    let text =
        std::fs::read_to_string(file).with_context(|| format!("cannot read {}", file.display()))?;
    let parsed =
        lemma_graph::config::parse(&text).with_context(|| format!("invalid {}", file.display()))?;
    let directory = std::path::absolute(file)?
        .parent()
        .map(Path::to_path_buf)
        .unwrap_or_default();
    let name = match module {
        Some(name) => name.to_owned(),
        None => module_name(&directory),
    };
    let converted = lemma_graph::lm::convert(&parsed, &name);
    if !write {
        print!("{converted}");
        return Ok(());
    }
    let target = directory.join(lemma_lang::load::ROOT_FILE);
    anyhow::ensure!(
        !target.exists(),
        "{} exists; remove it or print instead",
        target.display()
    );
    std::fs::write(&target, converted)
        .with_context(|| format!("cannot write {}", target.display()))?;
    eprintln!("wrote {}", target.display());
    Ok(())
}

/// A module name from a directory's name: letters, digits and `_`.
fn module_name(directory: &Path) -> String {
    let raw = directory.file_name().map_or_else(
        || "workspace".to_owned(),
        |name| name.to_string_lossy().into_owned(),
    );
    lemma_graph::lm::repository_name(&raw)
}
