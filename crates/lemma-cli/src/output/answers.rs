//! Printing answers to semantic questions and file outlines.
//!
//! Every provider-backed answer shares one JSON shape: where it came from
//! (`provenance`, `from_cache`, `skipped_providers`) and an `outcome` with
//! a `state`. What a found answer holds sits under a field named for it,
//! such as `references`.

use std::collections::BTreeMap;
use std::io::{self, Write};

use lemma_model::{Outcome, Position, Provenance, Reference, Span};
use lemma_query::{
    Answered, DefinitionReport, ImplementationReport, ReferenceReport, SignatureReport, Target,
};
use lemma_store::StoredSymbol;
use serde::Serialize;
use serde_json::{Map, Value};

use super::{Format, Linked, SymbolView, display_position, print_json, symbol_line};

/// Where an answer came from, and what it was.
#[derive(Serialize)]
struct AnsweredView<'a> {
    provenance: Option<&'a Provenance>,
    from_cache: bool,
    skipped_providers: &'a [String],
    outcome: Value,
}

impl<'a> AnsweredView<'a> {
    /// `field` names what a found answer holds, such as `references`.
    fn new<T>(answered: &'a Answered<T>, field: &str, found: impl FnOnce(&T) -> Value) -> Self {
        Self {
            provenance: answered.provenance.as_ref(),
            from_cache: answered.from_cache,
            skipped_providers: &answered.skipped_providers,
            outcome: outcome_json(&answered.outcome, field, found),
        }
    }
}

#[derive(Serialize)]
struct SymbolAnswerView<'a> {
    symbol: SymbolView<'a>,
    #[serde(flatten)]
    answer: AnsweredView<'a>,
}

#[derive(Serialize)]
struct DefinitionView<'a> {
    path: &'a str,
    position: Position,
    #[serde(flatten)]
    answer: AnsweredView<'a>,
}

#[derive(Serialize)]
struct TargetView<'a> {
    path: &'a str,
    span: Span,
    is_external: bool,
    symbol: Option<SymbolView<'a>>,
}

impl<'a> TargetView<'a> {
    fn new(target: &'a Target) -> Self {
        Self {
            path: &target.location.path,
            span: target.location.span,
            is_external: target.location.is_external,
            symbol: target.symbol.as_ref().map(SymbolView::new),
        }
    }
}

#[derive(Serialize)]
struct SignatureView<'a> {
    symbol: SymbolView<'a>,
    declaration: Option<&'a str>,
    hover: AnsweredView<'a>,
}

#[derive(Serialize)]
struct OutlineView<'a> {
    path: &'a str,
    symbols: Vec<SymbolView<'a>>,
}

pub(crate) fn outline(
    out: &mut dyn Write,
    path: &str,
    symbols: &[StoredSymbol],
    format: Format,
) -> io::Result<()> {
    if format == Format::Json {
        let symbols = symbols.iter().map(SymbolView::new).collect();
        return print_json(out, &OutlineView { path, symbols });
    }
    if symbols.is_empty() {
        return writeln!(out, "no symbols indexed in {path}");
    }
    for (depth, stored) in outline_rows(symbols) {
        writeln!(out, "{}{}", "  ".repeat(depth), symbol_line(stored))?;
    }
    Ok(())
}

/// Orders symbols as a tree, each under the symbol its qualified name is
/// prefixed by, with its depth in that tree.
///
/// Source order alone misplaces members: a Go method can follow an unrelated
/// function, and counting the dots in its name would indent it under that
/// function. A symbol whose container is not in the file stays at the top.
fn outline_rows(symbols: &[StoredSymbol]) -> Vec<(usize, &StoredSymbol)> {
    let mut first_by_name: BTreeMap<&str, usize> = BTreeMap::new();
    for (index, stored) in symbols.iter().enumerate() {
        first_by_name
            .entry(stored.symbol.qualified_name.as_str())
            .or_insert(index);
    }
    let mut children: Vec<Vec<usize>> = vec![Vec::new(); symbols.len()];
    let mut roots = Vec::new();
    for (index, stored) in symbols.iter().enumerate() {
        let container = stored
            .symbol
            .qualified_name
            .rsplit_once('.')
            .and_then(|(prefix, _)| first_by_name.get(prefix));
        match container {
            Some(&parent) => children[parent].push(index),
            None => roots.push(index),
        }
    }
    let mut rows = Vec::with_capacity(symbols.len());
    let mut pending: Vec<(usize, usize)> = roots.iter().rev().map(|&root| (0, root)).collect();
    while let Some((depth, index)) = pending.pop() {
        rows.push((depth, &symbols[index]));
        pending.extend(
            children[index]
                .iter()
                .rev()
                .map(|&child| (depth + 1, child)),
        );
    }
    rows
}

pub(crate) fn reference_report(
    out: &mut dyn Write,
    report: &ReferenceReport,
    linked: &[Linked],
    root: &str,
    format: Format,
) -> io::Result<()> {
    let answer = &report.answer;
    if format == Format::Json {
        let found = |references: &Vec<Reference>| json_value(references);
        let view = SymbolAnswerView {
            symbol: SymbolView::new(&report.symbol),
            answer: AnsweredView::new(answer, "references", found),
        };
        let mut value = serde_json::to_value(&view).map_err(io::Error::other)?;
        if !linked.is_empty() {
            value["linked"] = serde_json::to_value(linked).map_err(io::Error::other)?;
        }
        value["warnings"] = serde_json::to_value(&report.warnings).map_err(io::Error::other)?;
        return print_json(out, &value);
    }
    references_text(out, report, root)?;
    for warning in &report.warnings {
        writeln!(out, "warning: {warning}")?;
    }
    linked_text(out, linked)
}

/// The symbols the description links to or from a symbol, beside what the
/// language server found.
fn linked_text(out: &mut dyn Write, linked: &[Linked]) -> io::Result<()> {
    if linked.is_empty() {
        return Ok(());
    }
    writeln!(out, "linked by the description")?;
    for link in linked {
        let implemented = match (&link.symbol, &link.path) {
            (Some(symbol), Some(path)) => format!("  {symbol}  ({path})"),
            _ => "  (resolved in a workspace index)".to_owned(),
        };
        writeln!(out, "  {} {}{implemented}", link.direction, link.address)?;
    }
    Ok(())
}

fn references_text(out: &mut dyn Write, report: &ReferenceReport, root: &str) -> io::Result<()> {
    let answer = &report.answer;
    writeln!(out, "{}", symbol_line(&report.symbol))?;
    write_provenance(out, answer)?;
    let Outcome::Found(references) = &answer.outcome else {
        let nothing = format!(
            "no references found by {}. This covers only what it could see from {root}; \
             it does not prove the symbol is unused.",
            provider_name(answer)
        );
        return write_unanswered(out, answer, &report.symbol, "reference lookup", &nothing);
    };
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

pub(crate) fn definition_report(
    out: &mut dyn Write,
    report: &DefinitionReport,
    format: Format,
) -> io::Result<()> {
    let answer = &report.answer;
    if format == Format::Json {
        return print_json(
            out,
            &DefinitionView {
                path: &report.path,
                position: report.position,
                answer: AnsweredView::new(answer, "definitions", targets_json),
            },
        );
    }

    writeln!(out, "{}:{}", report.path, display_position(report.position))?;
    write_provenance(out, answer)?;
    let Outcome::Found(targets) = &answer.outcome else {
        let nothing = format!("no definition found by {}", provider_name(answer));
        return write_outcome(out, answer, &report.path, "definition lookup", &nothing);
    };
    write_targets(out, targets, ["definition", "definitions"])
}

pub(crate) fn implementation_report(
    out: &mut dyn Write,
    report: &ImplementationReport,
    format: Format,
) -> io::Result<()> {
    let answer = &report.answer;
    if format == Format::Json {
        return print_json(
            out,
            &SymbolAnswerView {
                symbol: SymbolView::new(&report.symbol),
                answer: AnsweredView::new(answer, "implementations", targets_json),
            },
        );
    }

    writeln!(out, "{}", symbol_line(&report.symbol))?;
    write_provenance(out, answer)?;
    let Outcome::Found(targets) = &answer.outcome else {
        let nothing = format!("no implementations found by {}", provider_name(answer));
        return write_unanswered(
            out,
            answer,
            &report.symbol,
            "implementation lookup",
            &nothing,
        );
    };
    write_targets(out, targets, ["implementation", "implementations"])
}

pub(crate) fn signature_report(
    out: &mut dyn Write,
    report: &SignatureReport,
    format: Format,
) -> io::Result<()> {
    let hover = &report.hover;
    if format == Format::Json {
        let text = |text: &String| Value::from(text.as_str());
        return print_json(
            out,
            &SignatureView {
                symbol: SymbolView::new(&report.symbol),
                declaration: report.declaration.as_deref(),
                hover: AnsweredView::new(hover, "text", text),
            },
        );
    }

    writeln!(out, "{}", symbol_line(&report.symbol))?;
    if let Some(declaration) = &report.declaration {
        writeln!(out)?;
        for line in declaration.lines() {
            writeln!(out, "    {line}")?;
        }
        writeln!(out)?;
    }
    let Outcome::Found(text) = &hover.outcome else {
        write_provenance(out, hover)?;
        let nothing = format!("{} has nothing to show for it", provider_name(hover));
        return write_unanswered(out, hover, &report.symbol, "hover", &nothing);
    };
    let shown = hover_beyond_declaration(text, report.declaration.as_deref());
    if shown.is_empty() {
        return Ok(());
    }
    write_provenance(out, hover)?;
    writeln!(out, "{shown}")
}

/// The part of a hover worth showing next to the declaration.
///
/// A server's hover often opens with its own rendering of the header, and
/// some render less than the source says: pyright shows a class's bases as
/// `class submit_task()`. When every word of that rendering is already in
/// the declaration, it adds nothing and is dropped; inferred types or
/// documentation are kept.
fn hover_beyond_declaration<'t>(hover: &'t str, declaration: Option<&str>) -> &'t str {
    let hover = hover.trim();
    let Some(declaration) = declaration else {
        return hover;
    };
    let Some((code, rest)) = leading_code_block(hover) else {
        return hover;
    };
    if !is_shorter_form(code, declaration) {
        return hover;
    }
    rest.trim_start().trim_start_matches("---").trim()
}

/// A Markdown text's opening fenced block and what follows it.
fn leading_code_block(text: &str) -> Option<(&str, &str)> {
    let after_fence = text.strip_prefix("```")?;
    let (_language, body) = after_fence.split_once('\n')?;
    let end = body.find("```")?;
    Some((&body[..end], &body[end + 3..]))
}

/// Whether `code` is no longer than `declaration` and every word in it is
/// a word of `declaration`.
fn is_shorter_form(code: &str, declaration: &str) -> bool {
    let words = |text: &str| -> Vec<String> {
        text.split(|c: char| !(c.is_alphanumeric() || c == '_'))
            .filter(|word| !word.is_empty())
            .map(str::to_owned)
            .collect()
    };
    let declared = words(declaration);
    let code = code.trim();
    code.len() <= declaration.trim().len() && words(code).iter().all(|word| declared.contains(word))
}

/// `nouns` is the singular and plural of what the targets are.
fn write_targets(out: &mut dyn Write, targets: &[Target], nouns: [&str; 2]) -> io::Result<()> {
    let noun = if targets.len() == 1 {
        nouns[0]
    } else {
        nouns[1]
    };
    writeln!(out, "{} {noun}", targets.len())?;
    for target in targets {
        let location = format!(
            "{}:{}",
            target.location.path,
            display_position(target.location.span.start)
        );
        match (&target.symbol, target.location.is_external) {
            (_, true) => writeln!(out, "  {location}  (external)")?,
            (Some(symbol), false) => writeln!(
                out,
                "  {location}  {} {}",
                symbol.symbol.qualified_name,
                crate::output::qualified_id(symbol)
            )?,
            (None, false) => writeln!(out, "  {location}")?,
        }
    }
    Ok(())
}

fn write_provenance<T>(out: &mut dyn Write, answer: &Answered<T>) -> io::Result<()> {
    if let Some(provenance) = &answer.provenance {
        let version = provenance
            .provider_version
            .as_deref()
            .map(|version| format!(" {version}"))
            .unwrap_or_default();
        let cached = if answer.from_cache { ", cached" } else { "" };
        writeln!(out, "via {}{version}{cached}", provenance.provider_id)?;
    }
    // A provider tried before the one that answered matters only when the
    // answer holds nothing: the one skipped might have found something.
    // Otherwise its failure, repeated on every query, is noise.
    if matches!(answer.outcome, Outcome::Found(_)) {
        return Ok(());
    }
    for skipped in &answer.skipped_providers {
        writeln!(out, "skipped {skipped}")?;
    }
    Ok(())
}

/// Explains an outcome that holds nothing, for a question about a symbol.
fn write_unanswered<T>(
    out: &mut dyn Write,
    answer: &Answered<T>,
    symbol: &StoredSymbol,
    operation: &str,
    nothing: &str,
) -> io::Result<()> {
    if let Outcome::ProviderUnavailable { candidates } = &answer.outcome {
        return writeln!(
            out,
            "no {} provider is installed (tried: {}). Install one or configure it in .lemma/providers.toml",
            symbol.language,
            candidates.join(", ")
        );
    }
    write_outcome(out, answer, &symbol.path, operation, nothing)
}

/// Explains an outcome that holds nothing, for a question about a file.
fn write_outcome<T>(
    out: &mut dyn Write,
    answer: &Answered<T>,
    path: &str,
    operation: &str,
    nothing: &str,
) -> io::Result<()> {
    let provider = provider_name(answer);
    match &answer.outcome {
        Outcome::Found(_) | Outcome::Empty => writeln!(out, "{nothing}"),
        Outcome::Unsupported => writeln!(out, "{provider} does not support {operation}"),
        Outcome::ProviderUnavailable { candidates } => writeln!(
            out,
            "no provider is installed for {path} (tried: {}). Install one or configure it in .lemma/providers.toml",
            candidates.join(", ")
        ),
        Outcome::Stale => writeln!(
            out,
            "{path} changed since it was indexed, or is not indexed; run `lemma index` again"
        ),
        Outcome::ProviderFailed { message } => writeln!(out, "{operation} failed: {message}"),
    }
}

fn provider_name<T>(answer: &Answered<T>) -> &str {
    answer
        .provenance
        .as_ref()
        .map_or("the provider", |provenance| provenance.provider_id.as_str())
}

/// An outcome as JSON: its `state`, plus what it holds under `field` when
/// found, or the details of why there is no answer.
fn outcome_json<T>(outcome: &Outcome<T>, field: &str, found: impl FnOnce(&T) -> Value) -> Value {
    let mut object = Map::new();
    object.insert("state".to_owned(), Value::from(outcome.state()));
    match outcome {
        Outcome::Found(value) => {
            object.insert(field.to_owned(), found(value));
        }
        Outcome::ProviderUnavailable { candidates } => {
            object.insert("candidates".to_owned(), json_value(candidates));
        }
        Outcome::ProviderFailed { message } => {
            object.insert("message".to_owned(), Value::from(message.as_str()));
        }
        Outcome::Empty | Outcome::Unsupported | Outcome::Stale => {}
    }
    Value::Object(object)
}

#[expect(
    clippy::ptr_arg,
    reason = "matches the `Answered<Vec<Target>>` it renders"
)]
fn targets_json(targets: &Vec<Target>) -> Value {
    json_value(&targets.iter().map(TargetView::new).collect::<Vec<_>>())
}

/// Serializes plain data. Lemma's own views always serialize, so a failure
/// would be a bug; it becomes `null` rather than a panic.
fn json_value(value: &impl Serialize) -> Value {
    serde_json::to_value(value).unwrap_or(Value::Null)
}

#[cfg(test)]
mod tests {
    use lemma_model::{Language, Symbol, SymbolKind};

    use super::*;

    fn stored(qualified_name: &str, kind: SymbolKind, line: u32) -> StoredSymbol {
        let position = Position { line, column: 0 };
        let span = Span {
            start: position,
            end: position,
        };
        StoredSymbol {
            id: i64::from(line),
            path: "server.go".to_owned(),
            language: Language::Go,
            symbol: Symbol {
                name: qualified_name
                    .rsplit('.')
                    .next()
                    .unwrap_or_default()
                    .to_owned(),
                qualified_name: qualified_name.to_owned(),
                kind,
                span,
                name_span: span,
            },
        }
    }

    #[test]
    fn a_hover_rendering_less_than_the_declaration_is_dropped() {
        let hover = "```python\nclass submit_task()\n```\n---\nRuns one action.";

        let shown = hover_beyond_declaration(hover, Some("class submit_task(TaskFunction):"));

        assert_eq!(shown, "Runs one action.");
    }

    #[test]
    fn a_hover_adding_inferred_types_is_kept() {
        let hover = "```python\ndef total(items) -> int\n```";

        let shown = hover_beyond_declaration(hover, Some("def total(items):"));

        assert_eq!(shown, hover);
    }

    #[test]
    fn methods_sit_under_their_type_even_after_an_unrelated_function() {
        let symbols = [
            stored("Server", SymbolKind::Struct, 1),
            stored("NewRouter", SymbolKind::Function, 5),
            stored("Server.listItems", SymbolKind::Method, 9),
            stored("Server.tagItem", SymbolKind::Method, 14),
        ];

        let rows: Vec<(usize, &str)> = outline_rows(&symbols)
            .into_iter()
            .map(|(depth, stored)| (depth, stored.symbol.qualified_name.as_str()))
            .collect();

        assert_eq!(
            rows,
            [
                (0, "Server"),
                (1, "Server.listItems"),
                (1, "Server.tagItem"),
                (0, "NewRouter"),
            ]
        );
    }

    #[test]
    fn a_member_whose_container_is_not_in_the_file_stays_at_the_top() {
        let symbols = [stored("Elsewhere.method", SymbolKind::Method, 3)];

        let rows = outline_rows(&symbols);

        assert_eq!(rows[0].0, 0);
    }

    fn provenance_text(outcome: Outcome<Vec<u32>>) -> String {
        let answer = Answered {
            outcome,
            provenance: None,
            from_cache: false,
            skipped_providers: vec![
                "typescript-language-server: no TypeScript installation".to_owned(),
            ],
        };
        let mut out = Vec::new();
        write_provenance(&mut out, &answer).unwrap();
        String::from_utf8(out).unwrap()
    }

    #[test]
    fn a_provider_skipped_before_an_answer_is_named_only_when_the_answer_is_empty() {
        let found = provenance_text(Outcome::Found(vec![1]));
        let empty = provenance_text(Outcome::Empty);

        assert_eq!(found, "");
        assert!(
            empty.contains("skipped typescript-language-server"),
            "{empty}"
        );
    }

    #[test]
    fn found_outcomes_hold_their_answer_under_the_named_field() {
        let outcome: Outcome<Vec<u32>> = Outcome::Found(vec![1, 2]);

        let json = outcome_json(&outcome, "references", json_value);

        assert_eq!(
            json,
            serde_json::json!({"state": "found", "references": [1, 2]})
        );
    }

    #[test]
    fn failures_carry_their_message() {
        let outcome: Outcome<Vec<u32>> = Outcome::ProviderFailed {
            message: "timed out".to_owned(),
        };

        let json = outcome_json(&outcome, "references", json_value);

        assert_eq!(
            json,
            serde_json::json!({"state": "provider_failed", "message": "timed out"})
        );
    }
}
