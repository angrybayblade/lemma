//! The standard library's reference, as Markdown: each module's purpose,
//! its functions and methods with their signatures, and what it declares.
//! `docs/standard-library.md` is this text; a test keeps the two equal.

use std::fmt::Write as _;

use super::{Builtin, builtins, method, module_names, source};
use crate::check::signature_text;
use crate::load::{Repository, SourceError, Sources};
use crate::types::Type;

/// The methods the standard library's values have, by receiver.
const METHODS: [(&str, &str); 5] = [
    ("SourceFile", "symbol"),
    ("SourceFile", "method"),
    ("SourceFile", "type"),
    ("Document", "at"),
    ("Json", "has"),
];

/// The reference, as Markdown.
pub fn reference() -> String {
    let all = builtins();
    let mut text = String::from(
        "# The standard library\n\n\
         Every module can use the standard library's modules: `import * as http from lemma.http`. \
         This page is generated from the library itself (`cargo test -p lemma-lang` checks it; \
         `LEMMA_BLESS=1` rewrites it). Hovering a name in an editor shows the same signatures.\n\n\
         ## Everywhere\n\n",
    );
    write_functions(
        &mut text,
        all.iter().filter(|builtin| builtin.module.is_empty()),
    );
    text.push_str("\n### Methods\n\n");
    for (receiver, name) in METHODS {
        let receiver_type = match receiver {
            "Json" => Type::Json,
            other => Type::Builtin(other),
        };
        if let Some(found) = method(&receiver_type, name) {
            let signature = signature_text(&Type::Function(Box::new(found.signature.clone())));
            let _ = writeln!(
                text,
                "- `{receiver}.{name}{signature}`: {}",
                found.documentation
            );
        }
    }
    let declared = declarations();
    for module in module_names() {
        let _ = write!(text, "\n## `{module}`\n\n");
        let purpose = purpose(source(module).unwrap_or_default());
        if !purpose.is_empty() {
            let _ = write!(text, "{purpose}\n\n");
        }
        let functions: Vec<&Builtin> = all
            .iter()
            .filter(|builtin| builtin.module == module)
            .collect();
        if !functions.is_empty() {
            text.push_str("### Functions\n\n");
            write_functions(&mut text, functions.into_iter());
            text.push('\n');
        }
        let exported: Vec<&String> = declared
            .iter()
            .filter(|(owner, _)| owner == module)
            .map(|(_, line)| line)
            .collect();
        if !exported.is_empty() {
            text.push_str("### Declarations\n\n");
            for line in exported {
                let _ = writeln!(text, "- {line}");
            }
        }
    }
    text
}

fn write_functions<'b>(text: &mut String, functions: impl Iterator<Item = &'b Builtin>) {
    for builtin in functions {
        let signature = signature_text(&Type::Function(Box::new(builtin.signature.clone())));
        let _ = writeln!(
            text,
            "- `{}{signature}`: {}",
            builtin.name, builtin.documentation
        );
    }
}

/// A module's leading comment, up to its first blank comment line.
fn purpose(text: &str) -> String {
    text.lines()
        .skip_while(|line| !line.starts_with("//"))
        .map_while(|line| line.strip_prefix("//"))
        .map(str::trim)
        .take_while(|line| !line.is_empty())
        .collect::<Vec<&str>>()
        .join(" ")
}

/// What every standard-library module declares, as `(module, line)` in
/// source order: its kind, name and type or signature.
fn declarations() -> Vec<(String, String)> {
    let mut entry = String::from("module reference\n");
    for module in module_names() {
        let _ = writeln!(entry, "import {module}");
    }
    let mut program = Program(entry);
    let checked = crate::check_program(&mut program, "lemma.lm");
    let mut found: Vec<(&str, u32, String)> = checked
        .index
        .definitions
        .iter()
        .filter(|(_, definition)| definition.module.starts_with("lemma."))
        .map(|(address, definition)| {
            let line = format!(
                "{} `{}`: `{}`",
                definition.kind,
                address.dotted_path(),
                definition.detail
            );
            (definition.module.as_str(), definition.span.start, line)
        })
        .collect();
    found.sort();
    found
        .into_iter()
        .map(|(module, _, line)| (module.to_owned(), line))
        .collect()
}

/// A program of one entry module, held in memory.
struct Program(String);

impl Sources for Program {
    fn read(&mut self, repository: &Repository, path: &str) -> Result<Option<String>, SourceError> {
        let is_entry = repository.source.is_none() && path == "lemma.lm";
        Ok(is_entry.then(|| self.0.clone()))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_documented_reference_is_the_librarys() {
        let path = concat!(
            env!("CARGO_MANIFEST_DIR"),
            "/../../docs/standard-library.md"
        );
        let generated = reference();
        if std::env::var_os("LEMMA_BLESS").is_some() {
            std::fs::write(path, &generated).unwrap();
        }
        let documented = std::fs::read_to_string(path).unwrap_or_default();

        assert!(
            documented == generated,
            "docs/standard-library.md is out of date; run `LEMMA_BLESS=1 cargo test -p lemma-lang`"
        );
    }
}
