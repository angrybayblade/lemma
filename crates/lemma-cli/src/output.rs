//! Printing results as text for people or JSON for agents.
//!
//! The JSON shapes are defined here rather than borrowed from storage types,
//! so the output contract does not change when internals do.

use std::collections::BTreeMap;
use std::io::{self, Write};
use std::path::Path;

use lemma_index::IndexSummary;
use lemma_model::{Language, Position, Span, SymbolKind};
use lemma_providers::ProviderRegistry;
use lemma_store::StoredSymbol;
use serde::Serialize;

mod answers;
mod graph;

pub(crate) use answers::{
    definition_report, implementation_report, outline, reference_report, signature_report,
};
pub(crate) use graph::{Drawing, use_graph};

/// How results are printed.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Format {
    Text,
    Json,
}

#[derive(Serialize)]
struct SymbolView<'a> {
    id: i64,
    /// The id to pass back: `#607`, or `#billing:607` in a workspace,
    /// where ids are numbered per repository.
    qualified_id: String,
    name: &'a str,
    qualified_name: &'a str,
    kind: SymbolKind,
    language: Language,
    path: &'a str,
    span: Span,
    name_span: Span,
}

impl<'a> SymbolView<'a> {
    fn new(stored: &'a StoredSymbol) -> Self {
        Self {
            id: stored.id,
            qualified_id: qualified_id(stored),
            name: &stored.symbol.name,
            qualified_name: &stored.symbol.qualified_name,
            kind: stored.symbol.kind,
            language: stored.language,
            path: &stored.path,
            span: stored.symbol.span,
            name_span: stored.symbol.name_span,
        }
    }
}

#[derive(Serialize)]
struct IndexView<'a> {
    database: String,
    snapshot_id: i64,
    root: &'a str,
    revision: Option<&'a str>,
    is_dirty: bool,
    fingerprint: &'a str,
    is_unchanged: bool,
    files_by_language: &'a BTreeMap<Language, usize>,
    symbol_count: usize,
    files_with_syntax_errors: &'a [String],
    unreadable_files: &'a [String],
    exports: &'a ExportCounts,
    #[serde(skip_serializing_if = "<[String]>::is_empty")]
    warnings: &'a [String],
}

/// How many exports and links an index holds.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize)]
pub(crate) struct ExportCounts {
    /// Operations declared in a description.
    pub operations: usize,
    /// Operations documents describe.
    pub endpoints: usize,
    /// Those whose route handler was found.
    pub endpoints_at_handlers: usize,
    /// How those handlers were found (`adapter`, `code`, `declared`,
    /// `directive`), with how many each.
    pub handlers_found_by: std::collections::BTreeMap<String, usize>,
    /// Definitions in public modules.
    pub symbols: usize,
    /// Declared links.
    pub links: usize,
    /// What the index leaves out or read more widely than described.
    #[serde(skip)]
    pub warnings: Vec<String>,
}

/// A symbol the description links to (`to`) or from (`from`) the one asked
/// about, by its export's address, with the code implementing it when the
/// index knows it.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub(crate) struct Linked {
    /// `to` (the asked symbol's operation uses it) or `from` (it uses the
    /// asked symbol's operation).
    pub direction: &'static str,
    /// Its export's address.
    pub address: String,
    /// The file implementing it, as `#member:path` in a workspace.
    pub path: Option<String>,
    /// The qualified name of the symbol implementing it.
    pub symbol: Option<String>,
}

/// One member of a workspace, as indexing it went.
#[derive(Debug, Clone, Serialize)]
pub(crate) struct MemberLine {
    name: String,
    revision: Option<String>,
    indexed_root: Option<String>,
    files: usize,
    symbols: usize,
    exports: Option<ExportCounts>,
    #[serde(skip_serializing_if = "Vec::is_empty")]
    warnings: Vec<String>,
    skipped: Option<String>,
}

impl MemberLine {
    pub(crate) fn indexed(
        member: &lemma_store::StoredMember,
        summary: &IndexSummary,
        exports: &ExportCounts,
    ) -> Self {
        Self {
            name: member.name.clone(),
            revision: member.revision.clone(),
            indexed_root: Some(member.indexed_root.clone()),
            files: summary.files_by_language.values().sum(),
            symbols: summary.symbol_count,
            exports: Some(exports.clone()),
            warnings: exports.warnings.clone(),
            skipped: None,
        }
    }

    pub(crate) fn skipped(name: &str, reason: String) -> Self {
        Self {
            name: name.to_owned(),
            revision: None,
            indexed_root: None,
            files: 0,
            symbols: 0,
            exports: None,
            warnings: Vec::new(),
            skipped: Some(reason),
        }
    }
}

#[derive(Serialize)]
struct WorkspaceView<'a> {
    database: String,
    members: &'a [MemberLine],
}

/// What indexing a workspace did, member by member.
pub(crate) fn workspace_summary(
    out: &mut dyn Write,
    members: &[MemberLine],
    database: &Path,
    format: Format,
) -> io::Result<()> {
    if format == Format::Json {
        let view = WorkspaceView {
            database: database.display().to_string(),
            members,
        };
        return print_json(out, &view);
    }
    let indexed = members
        .iter()
        .filter(|member| member.skipped.is_none())
        .count();
    writeln!(
        out,
        "workspace {}: {indexed} of {} repositories indexed",
        database.display(),
        members.len()
    )?;
    for member in members {
        let at = member
            .revision
            .as_deref()
            .map_or_else(String::new, |revision| {
                format!(" at {}", &revision[..revision.len().min(10)])
            });
        if let Some(reason) = &member.skipped {
            writeln!(out, "  {}{at}: skipped, {reason}", member.name)?;
            continue;
        }
        write!(
            out,
            "  {}{at}: {} files, {} symbols; ",
            member.name, member.files, member.symbols
        )?;
        let Some(exports) = &member.exports else {
            continue;
        };
        export_counts(out, exports)?;
        for warning in &member.warnings {
            writeln!(out, "    warning: {warning}")?;
        }
    }
    Ok(())
}

fn export_counts(out: &mut dyn Write, exports: &ExportCounts) -> io::Result<()> {
    writeln!(
        out,
        "exports: {} operations, {} endpoints ({} at their handlers), {} symbols; {} links",
        exports.operations,
        exports.endpoints,
        exports.endpoints_at_handlers,
        exports.symbols,
        exports.links
    )?;
    if exports.handlers_found_by.is_empty() {
        return Ok(());
    }
    let found_by: Vec<String> = exports
        .handlers_found_by
        .iter()
        .map(|(how, count)| format!("{how} {count}"))
        .collect();
    writeln!(out, "handlers found by: {}", found_by.join(", "))
}

#[derive(Serialize)]
struct AmbiguousView<'a> {
    state: &'static str,
    query: &'a str,
    candidates: Vec<SymbolView<'a>>,
}

#[derive(Serialize)]
struct ProviderView<'a> {
    id: &'a str,
    languages: &'a [Language],
    command: &'a str,
    executable: Option<String>,
    /// Where it is installed off `PATH`, when it is not on it.
    #[serde(skip_serializing_if = "Option::is_none")]
    off_path: Option<String>,
    /// How to install it, when it is not found anywhere.
    #[serde(skip_serializing_if = "Option::is_none")]
    install: Option<&'a str>,
}

impl<'a> IndexView<'a> {
    fn new(report: &IndexReport<'a>, database: &Path) -> Self {
        let summary = report.summary;
        let snapshot = &summary.snapshot;
        Self {
            database: database.display().to_string(),
            snapshot_id: summary.snapshot_id,
            root: &snapshot.root,
            revision: snapshot.revision.as_deref(),
            is_dirty: snapshot.is_dirty,
            fingerprint: &snapshot.fingerprint,
            is_unchanged: summary.is_unchanged,
            files_by_language: &summary.files_by_language,
            symbol_count: summary.symbol_count,
            files_with_syntax_errors: &summary.files_with_syntax_errors,
            unreadable_files: &summary.unreadable_files,
            exports: report.exports,
            warnings: report.warnings,
        }
    }
}

/// What one `lemma index` of a repository produced.
pub(crate) struct IndexReport<'a> {
    pub summary: &'a IndexSummary,
    pub exports: &'a ExportCounts,
    /// What the person should know, such as a missing the description.
    pub warnings: &'a [String],
}

pub(crate) fn index_summary(
    out: &mut dyn Write,
    report: &IndexReport<'_>,
    database: &Path,
    format: Format,
) -> io::Result<()> {
    let IndexReport {
        summary,
        exports,
        warnings,
    } = *report;
    let snapshot = &summary.snapshot;
    if format == Format::Json {
        return print_json(out, &IndexView::new(report, database));
    }

    let state = if summary.is_unchanged {
        "unchanged"
    } else {
        "indexed"
    };
    let file_count: usize = summary.files_by_language.values().sum();
    writeln!(
        out,
        "{state} {} ({file_count} files, {} symbols)",
        snapshot.root, summary.symbol_count
    )?;
    for (language, count) in &summary.files_by_language {
        writeln!(out, "  {language:<12} {count} files")?;
    }
    let revision = snapshot.revision.as_deref().unwrap_or("no git revision");
    let dirty = if snapshot.is_dirty {
        ", uncommitted changes"
    } else {
        ""
    };
    writeln!(out, "revision {revision}{dirty}")?;
    export_counts(out, exports)?;
    unparsed_files(out, &summary.files_with_syntax_errors)?;
    for path in &summary.unreadable_files {
        writeln!(out, "skipped {path}: not valid UTF-8")?;
    }
    for warning in warnings {
        writeln!(out, "warning: {warning}")?;
    }
    writeln!(out, "index {}", database.display())
}

pub(crate) fn symbols(
    out: &mut dyn Write,
    found: &[StoredSymbol],
    format: Format,
) -> io::Result<()> {
    if format == Format::Json {
        return print_json(out, &found.iter().map(SymbolView::new).collect::<Vec<_>>());
    }
    if found.is_empty() {
        return writeln!(out, "no matching symbols");
    }
    for stored in found {
        writeln!(out, "{}", symbol_line(stored))?;
    }
    Ok(())
}

pub(crate) fn ambiguous(
    out: &mut dyn Write,
    query: &str,
    candidates: &[StoredSymbol],
    format: Format,
) -> io::Result<()> {
    if format == Format::Json {
        return print_json(
            out,
            &AmbiguousView {
                state: "ambiguous",
                query,
                candidates: candidates.iter().map(SymbolView::new).collect(),
            },
        );
    }
    writeln!(
        out,
        "`{query}` matches {} symbols; pick one by qualified name, filter, or #id:",
        candidates.len()
    )?;
    for stored in candidates {
        writeln!(out, "  {}", symbol_line(stored))?;
    }
    Ok(())
}

pub(crate) fn providers(
    out: &mut dyn Write,
    registry: &ProviderRegistry,
    home: Option<&Path>,
    format: Format,
) -> io::Result<()> {
    let installed = |command: &str| registry.resolve_executable(command);
    let off_path = |command: &str| {
        installed(command).map_or_else(
            || home.and_then(|home| registry.installed_off_path(command, home)),
            |_| None,
        )
    };
    if format == Format::Json {
        let views: Vec<_> = registry
            .definitions()
            .iter()
            .map(|definition| {
                let executable = installed(&definition.command);
                let off_path = off_path(&definition.command);
                let is_missing = executable.is_none() && off_path.is_none();
                ProviderView {
                    id: &definition.id,
                    languages: &definition.languages,
                    command: &definition.command,
                    executable: executable.map(|path| path.display().to_string()),
                    off_path: off_path.map(|path| path.display().to_string()),
                    install: is_missing
                        .then_some(definition.install.as_deref())
                        .flatten(),
                }
            })
            .collect();
        return print_json(out, &views);
    }

    for definition in registry.definitions() {
        let languages: Vec<_> = definition
            .languages
            .iter()
            .map(|language| language.as_str())
            .collect();
        let status = match (
            installed(&definition.command),
            off_path(&definition.command),
        ) {
            (Some(path), _) => path.display().to_string(),
            (None, Some(path)) => off_path_status(&path),
            (None, None) => missing_status(definition),
        };
        writeln!(
            out,
            "{:<28} {:<16} {status}",
            definition.id,
            languages.join(",")
        )?;
    }
    writeln!(out)?;
    print_provider_order(out, registry)
}

/// The installed providers each language tries, in order.
fn print_provider_order(out: &mut dyn Write, registry: &ProviderRegistry) -> io::Result<()> {
    writeln!(out, "Tried in order for each language:")?;
    for language in Language::ALL {
        let installed = registry.installed(language);
        let ids: Vec<_> = installed
            .iter()
            .map(|provider| provider.definition.id.as_str())
            .collect();
        let chosen = if ids.is_empty() {
            "none installed".to_owned()
        } else {
            ids.join(", then ")
        };
        writeln!(out, "  {language:<12} {chosen}")?;
    }
    Ok(())
}

/// How many unparsed files to name.
const NAMED_UNPARSED_FILES: usize = 5;

/// The files Tree-sitter could not parse whole, whose symbols after the
/// failure may be missing: how many, how many of them are tests, and the
/// first few others by name, since those are where missing symbols
/// matter. Valid code can fail too, where the grammar lags the language.
fn unparsed_files(out: &mut dyn Write, files: &[String]) -> io::Result<()> {
    if files.is_empty() {
        return Ok(());
    }
    let (tests, code): (Vec<&String>, Vec<&String>) = files
        .iter()
        .partition(|path| lemma_http::is_test_file(path));
    let of_which = if tests.is_empty() {
        String::new()
    } else {
        format!(" ({} of them tests)", tests.len())
    };
    writeln!(
        out,
        "{} files could not be parsed whole{of_which}; symbols past the failure may be missing",
        files.len()
    )?;
    for path in code.iter().take(NAMED_UNPARSED_FILES) {
        writeln!(out, "  {path}")?;
    }
    if code.len() > NAMED_UNPARSED_FILES || !tests.is_empty() {
        writeln!(out, "  (--json lists them all as files_with_syntax_errors)")?;
    }
    Ok(())
}

/// A provider installed where `PATH` does not reach, and the fix.
fn off_path_status(path: &Path) -> String {
    let directory = path.parent().unwrap_or(path);
    format!(
        "not on PATH: installed at {}; add {} to PATH",
        path.display(),
        directory.display()
    )
}

/// A provider found nowhere, and how to install it when that is known.
fn missing_status(definition: &lemma_providers::ProviderDefinition) -> String {
    match &definition.install {
        Some(install) => format!("not found: {} (install: {install})", definition.command),
        None => format!("not found: {}", definition.command),
    }
}

/// How a symbol's id is written for passing back: `#607`, or, for a symbol
/// a workspace found in one of its repositories (its path then starts with
/// the repository, `#billing:src/charges.go`), `#billing:607`.
pub(crate) fn qualified_id(stored: &StoredSymbol) -> String {
    match stored.path.split_once(':') {
        Some((repository, _)) if repository.starts_with('#') => {
            format!("{repository}:{}", stored.id)
        }
        _ => format!("#{}", stored.id),
    }
}

fn symbol_line(stored: &StoredSymbol) -> String {
    format!(
        "{:<8} {:<10} {}  {}:{}",
        qualified_id(stored),
        stored.symbol.kind.as_str(),
        stored.symbol.qualified_name,
        stored.path,
        display_position(stored.symbol.name_span.start)
    )
}

/// Editors count lines and columns from 1.
fn display_position(position: Position) -> String {
    format!("{}:{}", position.line + 1, position.column + 1)
}

fn print_json(out: &mut dyn Write, value: &impl Serialize) -> io::Result<()> {
    serde_json::to_writer_pretty(&mut *out, value)?;
    writeln!(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn unparsed_files_name_code_before_tests() {
        let files = [
            "src/a.test.ts".to_owned(),
            "src/db.ts".to_owned(),
            "src/__tests__/b.ts".to_owned(),
        ];
        let mut printed = Vec::new();

        unparsed_files(&mut printed, &files).unwrap();

        assert_eq!(
            String::from_utf8(printed).unwrap(),
            "3 files could not be parsed whole (2 of them tests); symbols past the failure \
             may be missing\n  src/db.ts\n  (--json lists them all as files_with_syntax_errors)\n"
        );
    }
}
