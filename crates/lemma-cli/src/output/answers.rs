//! Printing answers to semantic questions and file outlines.
//!
//! Every provider-backed answer shares one JSON shape: where it came from
//! (`provenance`, `from_cache`, `skipped_providers`) and an `outcome` with
//! a `state`. What a found answer holds sits under a field named for it,
//! such as `references`.

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
    for stored in symbols {
        let depth = stored.symbol.qualified_name.matches('.').count();
        writeln!(out, "{}{}", "  ".repeat(depth), symbol_line(stored))?;
    }
    Ok(())
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
        return print_json(out, &value);
    }
    references_text(out, report, root)?;
    linked_text(out, linked)
}

/// The symbols `lemma.yaml` links to or from a symbol, beside what the
/// language server found.
fn linked_text(out: &mut dyn Write, linked: &[Linked]) -> io::Result<()> {
    if linked.is_empty() {
        return Ok(());
    }
    writeln!(out, "linked by lemma.yaml")?;
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
    write_provenance(out, hover)?;
    let Outcome::Found(text) = &hover.outcome else {
        let nothing = format!("{} has nothing to show for it", provider_name(hover));
        return write_unanswered(out, hover, &report.symbol, "hover", &nothing);
    };
    writeln!(out, "{}", text.trim_end())
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
            .unwrap_or("unknown version");
        let cached = if answer.from_cache { ", cached" } else { "" };
        writeln!(out, "via {} {version}{cached}", provenance.provider_id)?;
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
    use super::*;

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
