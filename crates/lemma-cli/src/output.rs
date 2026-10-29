//! Printing results as text for people or JSON for agents.
//!
//! The JSON shapes are defined here rather than borrowed from storage types,
//! so the output contract does not change when internals do.

use std::collections::BTreeMap;
use std::io::{self, Write};
use std::path::Path;

use lemma_index::IndexSummary;
use lemma_model::{Language, Position, Provenance, ReferenceOutcome, Span, SymbolKind};
use lemma_providers::ProviderRegistry;
use lemma_query::ReferenceReport;
use lemma_store::StoredSymbol;
use serde::Serialize;

/// How results are printed.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Format {
    Text,
    Json,
}

#[derive(Serialize)]
struct SymbolView<'a> {
    id: i64,
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
}

#[derive(Serialize)]
struct ReferenceView<'a> {
    symbol: SymbolView<'a>,
    provenance: Option<&'a Provenance>,
    from_cache: bool,
    skipped_providers: &'a [String],
    outcome: &'a ReferenceOutcome,
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
}

pub(crate) fn index_summary(
    out: &mut dyn Write,
    summary: &IndexSummary,
    database: &Path,
    format: Format,
) -> io::Result<()> {
    let snapshot = &summary.snapshot;
    if format == Format::Json {
        return print_json(
            out,
            &IndexView {
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
            },
        );
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
    if !summary.files_with_syntax_errors.is_empty() {
        writeln!(
            out,
            "{} files had syntax errors; some of their symbols may be missing",
            summary.files_with_syntax_errors.len()
        )?;
    }
    for path in &summary.unreadable_files {
        writeln!(out, "skipped {path}: not valid UTF-8")?;
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

pub(crate) fn reference_report(
    out: &mut dyn Write,
    report: &ReferenceReport,
    root: &str,
    format: Format,
) -> io::Result<()> {
    if format == Format::Json {
        return print_json(
            out,
            &ReferenceView {
                symbol: SymbolView::new(&report.symbol),
                provenance: report.provenance.as_ref(),
                from_cache: report.from_cache,
                skipped_providers: &report.skipped_providers,
                outcome: &report.outcome,
            },
        );
    }

    writeln!(out, "{}", symbol_line(&report.symbol))?;
    if let Some(provenance) = &report.provenance {
        let version = provenance
            .provider_version
            .as_deref()
            .unwrap_or("unknown version");
        let cached = if report.from_cache { ", cached" } else { "" };
        writeln!(out, "via {} {version}{cached}", provenance.provider_id)?;
    }
    for skipped in &report.skipped_providers {
        writeln!(out, "skipped {skipped}")?;
    }
    write_outcome(out, report, root)
}

fn write_outcome(out: &mut dyn Write, report: &ReferenceReport, root: &str) -> io::Result<()> {
    let provider = report
        .provenance
        .as_ref()
        .map_or("the provider", |p| p.provider_id.as_str());

    match &report.outcome {
        ReferenceOutcome::Found { references } => {
            writeln!(out, "{} references", references.len())?;
            for reference in references {
                let location = format!(
                    "{}:{}",
                    reference.path,
                    display_position(reference.span.start)
                );
                match (&reference.enclosing_symbol, reference.is_external) {
                    (_, true) => writeln!(out, "  {location}  (external)")?,
                    (Some(enclosing), false) => writeln!(out, "  {location}  in {enclosing}")?,
                    (None, false) => writeln!(out, "  {location}")?,
                }
            }
            Ok(())
        }
        ReferenceOutcome::Empty => writeln!(
            out,
            "no references found by {provider}. This covers only what it could see from {root}; \
             it does not prove the symbol is unused."
        ),
        ReferenceOutcome::Unsupported => {
            writeln!(out, "{provider} does not support reference lookup")
        }
        ReferenceOutcome::ProviderUnavailable { candidates } => writeln!(
            out,
            "no {} provider is installed (tried: {}). Install one or configure it in .lemma/providers.toml",
            report.symbol.language,
            candidates.join(", ")
        ),
        ReferenceOutcome::Stale => writeln!(
            out,
            "{} changed since it was indexed; run `lemma index` again",
            report.symbol.path
        ),
        ReferenceOutcome::ProviderFailed { message } => writeln!(out, "lookup failed: {message}"),
    }
}

pub(crate) fn providers(
    out: &mut dyn Write,
    registry: &ProviderRegistry,
    format: Format,
) -> io::Result<()> {
    let installed = |command: &str| registry.resolve_executable(command);
    if format == Format::Json {
        let views: Vec<_> = registry
            .definitions()
            .iter()
            .map(|definition| ProviderView {
                id: &definition.id,
                languages: &definition.languages,
                command: &definition.command,
                executable: installed(&definition.command).map(|path| path.display().to_string()),
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
        let status = installed(&definition.command).map_or_else(
            || format!("not found: {}", definition.command),
            |path| path.display().to_string(),
        );
        writeln!(
            out,
            "{:<28} {:<16} {status}",
            definition.id,
            languages.join(",")
        )?;
    }
    writeln!(out)?;
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

fn symbol_line(stored: &StoredSymbol) -> String {
    format!(
        "#{:<7} {:<10} {}  {}:{}",
        stored.id,
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
