//! `lm check`: loading a module and everything it imports, and reporting
//! every problem with its file, line and column.

use std::io::Write;
use std::path::{Path, PathBuf};
use std::process::ExitCode;

use anyhow::Context;
use lemma_graph::repos::{Checkout, cache_directory};
use lemma_lang::{Checked, Severity, check_program};

/// Loads and checks the program `file` is the entry module of.
fn load(file: &Path) -> anyhow::Result<(Checked, PathBuf)> {
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

/// Checks the program `file` is the entry module of and prints what it
/// found: as `file:line:column` lines, or as JSON.
pub(crate) fn run(file: &Path, json: bool) -> anyhow::Result<ExitCode> {
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
