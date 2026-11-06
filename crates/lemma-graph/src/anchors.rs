//! Checking that a link's caller is anchored at code that is called.
//!
//! A caller operation's anchor (`path: client/charges.go:Charges.Create`)
//! is where traces start: what calls it is what a broken link affects. An
//! anchor at a method nothing calls makes every trace from it empty, and
//! no check notices. The usual cause is a generated client whose real
//! entry point is a sibling method (`Invoke` versus
//! `InvokeWithRuntimeEnv`). Names cannot tell the two apart when the name
//! is common, so this asks a language server, and only when the user opts
//! in: starting one costs seconds per repository.

use std::collections::{BTreeMap, BTreeSet};
use std::error::Error;
use std::path::Path;

use lemma_model::{Language, Location, Position, Span, Symbol, SymbolKind};
use lemma_syntax::Extractor;

use crate::GraphError;
use crate::address::{Address, Link};
use crate::check::{Finding, HEAD, Repo, Severity};
use crate::repos::Checkout;

/// Where a symbol is used: `None` when no provider can say, an error when
/// one failed to.
pub type Uses = Result<Option<Vec<Location>>, Box<dyn Error + Send + Sync>>;

/// Finds the uses of a symbol through a semantic provider, such as a
/// language server.
pub trait References {
    /// Where the symbol whose name starts at `position` in `file` (relative
    /// to `root`) is used, its own declaration possibly included. `Ok(None)`
    /// when no provider serves `language`.
    fn uses(&mut self, root: &Path, file: &str, language: Language, position: Position) -> Uses;
}

/// The method an anchor names and its siblings: the other methods of the
/// same type in the same file.
#[derive(Debug, Clone, PartialEq, Eq)]
struct Methods<'a> {
    file: &'a str,
    anchor: &'a Symbol,
    siblings: Vec<&'a Symbol>,
}

impl Methods<'_> {
    /// Whether `location` is inside one of the type's methods here: the
    /// type calling itself, which says nothing of who calls the type.
    fn is_inside(&self, location: &Location) -> bool {
        location.path == self.file
            && std::iter::once(self.anchor)
                .chain(self.siblings.iter().copied())
                .any(|method| contains(method.span, location.span.start))
    }
}

fn contains(span: Span, position: Position) -> bool {
    span.start <= position && position < span.end
}

/// How an anchor's uses compare with its siblings'.
#[derive(Debug, Clone, PartialEq, Eq)]
enum Verdict {
    /// The anchor is used, or nothing suggests another method was meant.
    Plausible,
    /// Nothing uses the anchor, while these siblings are used, with how
    /// often.
    Uncalled(Vec<(String, usize)>),
}

/// Checks the caller anchor of each of `links` that names a method,
/// reading code at the head and asking `references` who uses it.
pub(crate) fn check_anchors(
    repos: &BTreeMap<String, Repo>,
    links: &BTreeSet<Link>,
    cache: &Path,
    references: &mut dyn References,
) -> Result<Vec<Finding>, GraphError> {
    let mut extractor = Extractor::new().map_err(|source| GraphError::Syntax { source })?;
    let mut findings = Vec::new();
    let mut seen = BTreeSet::new();
    for link in links {
        if !seen.insert(&link.caller) {
            continue;
        }
        let label = format!("{} -> {}", link.caller, link.provider);
        let Some((checkout, anchor)) = caller_anchor(repos, &link.caller) else {
            continue;
        };
        let asked = Asked {
            checkout,
            anchor,
            cache,
        };
        if let Some(finding) =
            check_anchor(&asked, &label, &link.caller, &mut extractor, references)?
        {
            findings.push(finding);
        }
    }
    Ok(findings)
}

/// One anchor to check and where its code is.
struct Asked<'a> {
    checkout: &'a Checkout,
    anchor: &'a str,
    cache: &'a Path,
}

/// The head checkout and anchor (`file:Symbol`) of the operation at
/// `address`, if it is declared there.
fn caller_anchor<'a>(
    repos: &'a BTreeMap<String, Repo>,
    address: &Address,
) -> Option<(&'a Checkout, &'a str)> {
    let repo = repos.get(address.repository.as_deref()?)?;
    let service = repo.files[HEAD].services.get(&address.service)?;
    let operation = service.exports.operations.get(&address.operation)?;
    Some((&repo.checkouts[HEAD], operation.path.as_str()))
}

fn check_anchor(
    asked: &Asked<'_>,
    label: &str,
    address: &Address,
    extractor: &mut Extractor,
    references: &mut dyn References,
) -> Result<Option<Finding>, GraphError> {
    let (file, symbol) = asked.anchor.rsplit_once(':').unwrap_or((asked.anchor, ""));
    let extension = file.rsplit_once('.').map_or("", |(_, extension)| extension);
    let Some(language) = Language::from_extension(extension) else {
        return Ok(None);
    };
    let Some(source) = asked.checkout.read(file)? else {
        return Ok(None);
    };
    let extraction = extractor
        .extract(language, &source)
        .map_err(|source| GraphError::Syntax { source })?;
    let Some(methods) = methods(file, &extraction.symbols, &symbol.replace("()", "")) else {
        return Ok(None);
    };
    let root = asked.checkout.worktree(asked.cache)?;
    let mut find = |symbol: &Symbol| references.uses(&root, file, language, symbol.name_span.start);
    let verdict = match judge(&methods, &mut find) {
        Ok(verdict) => verdict,
        Err(Unanswered::NoProvider) => {
            let message = format!(
                "the caller operation {address}'s anchor `{}` was not checked: no language server for {language:?} is installed (see `lemma providers`)",
                asked.anchor
            );
            return Ok(Some(finding(label, Severity::Info, message)));
        }
        Err(Unanswered::Failed(error)) => {
            let message = format!(
                "the caller operation {address}'s anchor `{}` was not checked: the language server failed: {}",
                asked.anchor,
                describe(error.as_ref())
            );
            return Ok(Some(finding(label, Severity::Info, message)));
        }
    };
    let Verdict::Uncalled(used) = verdict else {
        return Ok(None);
    };
    let message = format!(
        "the caller operation {address}'s anchor `{}` is never called, while {} of the same type {}: the anchor may name the wrong method, and traces from it find nothing",
        asked.anchor,
        used_siblings(&used),
        if used.len() == 1 { "is" } else { "are" },
    );
    Ok(Some(finding(label, Severity::Warning, message)))
}

/// Why a count is missing.
enum Unanswered {
    NoProvider,
    Failed(Box<dyn Error + Send + Sync>),
}

/// The anchor `symbol` (a qualified name such as `Charges.Create`) among
/// `symbols`, with its sibling methods, when it is a method that has some.
fn methods<'a>(file: &'a str, symbols: &'a [Symbol], symbol: &str) -> Option<Methods<'a>> {
    let anchor = symbols.iter().find(|found| {
        found.kind == SymbolKind::Method
            && (found.qualified_name == symbol
                || found.qualified_name.ends_with(&format!(".{symbol}")))
    })?;
    let (owner, _) = anchor.qualified_name.rsplit_once('.')?;
    let siblings: Vec<&Symbol> = symbols
        .iter()
        .filter(|found| found.kind == SymbolKind::Method && found.name != anchor.name)
        .filter(|found| found.qualified_name.rsplit_once('.').map(|(of, _)| of) == Some(owner))
        .collect();
    if siblings.is_empty() {
        return None;
    }
    Some(Methods {
        file,
        anchor,
        siblings,
    })
}

/// Whether `methods`' anchor looks misplaced, by the uses `find` finds
/// outside the type's own methods: the anchor's first, and its siblings'
/// only when the anchor has none.
fn judge(
    methods: &Methods<'_>,
    find: &mut dyn FnMut(&Symbol) -> Uses,
) -> Result<Verdict, Unanswered> {
    let mut ask = |symbol: &Symbol| match find(symbol) {
        Ok(Some(uses)) => Ok(uses
            .iter()
            .filter(|location| !methods.is_inside(location))
            .count()),
        Ok(None) => Err(Unanswered::NoProvider),
        Err(error) => Err(Unanswered::Failed(error)),
    };
    if ask(methods.anchor)? > 0 {
        return Ok(Verdict::Plausible);
    }
    let mut called = Vec::new();
    for sibling in &methods.siblings {
        let calls = ask(sibling)?;
        if calls > 0 {
            called.push((sibling.name.clone(), calls));
        }
    }
    if called.is_empty() {
        return Ok(Verdict::Plausible);
    }
    called.sort();
    Ok(Verdict::Uncalled(called))
}

/// `` `InvokeWithRuntimeEnv` (called 3 times)``, joined.
fn used_siblings(used: &[(String, usize)]) -> String {
    let named: Vec<String> = used
        .iter()
        .map(|(name, uses)| {
            let plural = if *uses == 1 { "" } else { "s" };
            format!("`{name}` (called {uses} time{plural})")
        })
        .collect();
    named.join(", ")
}

fn finding(link: &str, severity: Severity, message: String) -> Finding {
    Finding {
        severity,
        link: link.to_owned(),
        check: "anchor".to_owned(),
        message,
        introduced: false,
        affects: Vec::new(),
        allowed: None,
        values: std::collections::BTreeMap::new(),
    }
}

fn describe(error: &(dyn Error + 'static)) -> String {
    let mut message = error.to_string();
    let mut source = error.source();
    while let Some(cause) = source {
        message.push_str(": ");
        message.push_str(&cause.to_string());
        source = cause.source();
    }
    message
}

#[cfg(test)]
mod tests {
    use super::*;

    const FILE: &str = "client.go";

    /// A method spanning lines `index * 10` to `index * 10 + 9`.
    fn method(qualified_name: &str, index: u32) -> Symbol {
        let start = Position {
            line: index * 10,
            column: 0,
        };
        let end = Position {
            line: index * 10 + 9,
            column: 0,
        };
        Symbol {
            name: qualified_name.rsplit('.').next().unwrap().to_owned(),
            qualified_name: qualified_name.to_owned(),
            kind: SymbolKind::Method,
            span: Span { start, end },
            name_span: Span { start, end: start },
        }
    }

    fn at(path: &str, line: u32) -> Location {
        let position = Position { line, column: 4 };
        Location {
            path: path.to_owned(),
            span: Span {
                start: position,
                end: position,
            },
            is_external: false,
        }
    }

    /// Uses of each method, by its index in the file.
    fn uses(by_method: Vec<Vec<Location>>) -> impl FnMut(&Symbol) -> Uses {
        move |symbol| {
            Ok(Some(
                by_method[(symbol.span.start.line / 10) as usize].clone(),
            ))
        }
    }

    #[test]
    fn an_uncalled_anchor_with_a_sibling_called_from_outside_is_flagged() {
        let symbols = [
            method("ExecuteAction.Invoke", 0),
            method("ExecuteAction.InvokeWithRuntimeEnv", 1),
            method("Other.Invoke", 2),
        ];
        let methods = methods(FILE, &symbols, "ExecuteAction.Invoke").unwrap();
        let found = vec![
            vec![at(FILE, 0)],
            vec![at(FILE, 5), at("app.go", 3)],
            vec![at("app.go", 9)],
        ];

        let verdict = judge(&methods, &mut uses(found)).ok().unwrap();

        assert_eq!(
            verdict,
            Verdict::Uncalled(vec![("InvokeWithRuntimeEnv".to_owned(), 1)])
        );
    }

    #[test]
    fn siblings_called_only_by_the_type_itself_say_nothing() {
        let symbols = [
            method("Validate.Invoke", 0),
            method("Validate.invokeWithAws", 1),
        ];
        let methods = methods(FILE, &symbols, "Validate.Invoke").unwrap();
        let found = vec![vec![at(FILE, 0)], vec![at(FILE, 10), at(FILE, 7)]];

        let verdict = judge(&methods, &mut uses(found)).ok().unwrap();

        assert_eq!(verdict, Verdict::Plausible);
    }

    #[test]
    fn a_called_anchor_is_plausible() {
        let symbols = [method("Charges.Create", 0), method("Charges.List", 1)];
        let methods = methods(FILE, &symbols, "Charges.Create").unwrap();
        let found = vec![vec![at("app.go", 1)], vec![at("app.go", 2)]];

        let verdict = judge(&methods, &mut uses(found)).ok().unwrap();

        assert_eq!(verdict, Verdict::Plausible);
    }

    #[test]
    fn a_method_without_siblings_or_a_function_is_not_judged() {
        let symbols = [method("Charges.Create", 0), method("Refunds.Create", 1)];

        assert!(methods(FILE, &symbols, "Charges.Create").is_none());
        assert!(methods(FILE, &symbols, "create_charge").is_none());
    }

    #[test]
    fn a_missing_provider_leaves_the_anchor_unchecked() {
        let symbols = [method("Charges.Create", 0), method("Charges.List", 1)];
        let methods = methods(FILE, &symbols, "Charges.Create").unwrap();

        let verdict = judge(&methods, &mut |_: &Symbol| Ok(None));

        assert!(matches!(verdict, Err(Unanswered::NoProvider)));
    }
}
