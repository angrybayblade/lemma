//! Allowances for the findings of a `.lm` grammar (spec 13, section 8): a
//! finding an `allow` covers is informational and says why; an allowance
//! past its `until`, dated by the change's head commit, covers nothing and
//! is reported as expired. A run reads only what a change touches, so an
//! allowance that covers nothing is not reported here; `lemma check` of a
//! whole system reports those.

use std::collections::BTreeMap;
use std::fmt::Write as _;
use std::path::Path;
use std::process::Command;

use lemma_lang::eval::{Allowances, Values};
use lemma_lang::model::Model;
use lemma_lang::types::Address;

use crate::Finding;
use crate::manifest::Manifest;

/// A `.lm` grammar's program: its allowances, and the name each
/// requirement's findings carry.
#[derive(Debug)]
pub(crate) struct Program {
    pub(crate) model: Model,
    pub(crate) names: BTreeMap<Address, String>,
}

/// Marks the findings `program`'s allowances cover; returns a finding for
/// each expired allowance.
pub(crate) fn apply<'f>(
    program: &Program,
    findings: impl Iterator<Item = &'f mut Finding>,
    date: Option<String>,
) -> Vec<Finding> {
    let mut allowances = Allowances::new(&program.model, date);
    for finding in findings {
        let requirement = &finding.requirement;
        let reason = allowances.covering(
            |address| program.names.get(address) == Some(requirement),
            Values::Text(&finding.values),
        );
        let Some(reason) = reason else {
            continue;
        };
        finding.severity = "info";
        let _ = write!(finding.message, " (allowed: {reason})");
        finding.allowed = Some(reason);
    }
    let mut expired = Vec::new();
    for allowance in allowances.expired() {
        expired.push(Finding {
            requirement: "allow".to_owned(),
            severity: "warning",
            message: format!(
                "the allowance for {} expired on {}; the findings it covered count again",
                allowance.requirement,
                allowance.until.as_deref().unwrap_or_default()
            ),
            values: BTreeMap::new(),
            introduced: true,
            derivation: None,
            allowed: None,
        });
    }
    expired
}

/// The committer date of the change's head, `YYYY-MM-DD`: of `HEAD` in
/// the changed repository when the head is its working directory.
pub(crate) fn committer_date(manifest: &Manifest) -> Option<String> {
    let change = &manifest.change;
    let repository = manifest.repositories.get(&change.repository)?;
    let commit = change.head.as_deref().unwrap_or("HEAD");
    git_date(&repository.path, commit)
}

fn git_date(root: &Path, commit: &str) -> Option<String> {
    let output = Command::new("git")
        .arg("-C")
        .arg(root)
        .args(["log", "-1", "--format=%cs", commit])
        .output()
        .ok()?;
    let date = String::from_utf8_lossy(&output.stdout).trim().to_owned();
    (output.status.success() && !date.is_empty()).then_some(date)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn program(text: &str) -> Program {
        let directory = tempfile::tempdir().unwrap();
        std::fs::write(directory.path().join("contracts.lm"), text).unwrap();
        let checked = lemma_lang::check_program(
            &mut lemma_lang::files::WorkingTrees::new(directory.path()),
            "contracts.lm",
        );
        assert_eq!(checked.error_lines(), Vec::<String>::new());
        Program {
            names: lemma_grammar::lm::requirement_names(&checked.model),
            model: checked.model,
        }
    }

    fn finding(consumer: &str) -> Finding {
        Finding {
            requirement: "consumer_calls_removed_operation".to_owned(),
            severity: "error",
            message: format!("{consumer} calls GET /sessions, which no longer exists"),
            values: [
                ("c", consumer),
                ("op", "GET /sessions"),
                ("f", "a.ts"),
                ("l", "3"),
            ]
            .iter()
            .map(|(name, value)| ((*name).to_owned(), (*value).to_owned()))
            .collect(),
            introduced: true,
            derivation: None,
            allowed: None,
        }
    }

    const GRAMMAR: &str = r#"module contracts
import lemma.http
allow lemma.http.consumer_calls_removed_operation {
    when: c == "legacy::load"
    reason: "the legacy client is going away"
    until: "2027-01-31"
}
"#;

    #[test]
    fn a_covered_finding_is_informational_and_says_why() {
        let program = program(GRAMMAR);
        let mut findings = [finding("legacy::load"), finding("web::load")];

        let expired = apply(&program, findings.iter_mut(), Some("2026-11-06".to_owned()));

        assert_eq!(findings[0].severity, "info");
        assert_eq!(
            findings[0].allowed.as_deref(),
            Some("the legacy client is going away")
        );
        assert_eq!(findings[1].severity, "error");
        assert_eq!(expired, []);
    }

    #[test]
    fn an_expired_allowance_covers_nothing_and_is_reported() {
        let program = program(GRAMMAR);
        let mut findings = [finding("legacy::load")];

        let expired = apply(&program, findings.iter_mut(), Some("2027-02-01".to_owned()));

        assert_eq!(findings[0].severity, "error");
        assert!(expired[0].message.contains("expired on 2027-01-31"));
    }
}
