//! Allowances in `lemma check` (spec 13, section 8). A finding an `allow`
//! covers is reported as allowed, with the reason, and is informational,
//! so it does not fail the check. Past its `until`, compared with the head
//! commit's committer date rather than the clock, an allowance covers
//! nothing and is reported as expired. An allowance that covers no finding
//! of a check of the whole system is reported, so stale ones get removed.

use std::collections::{BTreeMap, BTreeSet};
use std::fmt::Write as _;

use lemma_lang::eval::{Allowances, BehaviorResult, Datum, Outcome, Values};
use lemma_lang::model::Model;
use lemma_lang::types::Address;

use crate::behaviors::BehaviorReport;
use crate::check::{Finding, Severity};

/// Applies a program's allowances to `lemma check`'s findings.
pub(crate) struct Allowed<'m> {
    allowances: Allowances<'m>,
    /// The name each requirement's findings carry.
    names: BTreeMap<Address, String>,
}

impl<'m> Allowed<'m> {
    /// `model`'s allowances, dated by `date` (`YYYY-MM-DD`), the head
    /// commit's committer date.
    pub(crate) fn new(model: &'m Model, date: Option<String>) -> Self {
        Self {
            allowances: Allowances::new(model, date),
            names: lemma_grammar::lm::requirement_names(model),
        }
    }

    /// Marks the behaviors' cases an allowance covers. Reports are in the
    /// order of `results`.
    pub(crate) fn cover_cases(
        &mut self,
        results: &[BehaviorResult],
        reports: &mut [BehaviorReport],
    ) {
        for (result, report) in results.iter().zip(reports) {
            let cases = result.cases.iter().zip(&mut report.cases);
            for (case, reported) in cases.filter(|(case, _)| case.outcome != Outcome::Satisfied) {
                let values: BTreeMap<String, Datum> = case.values.iter().cloned().collect();
                reported.allowed = self
                    .allowances
                    .covering(|address| *address == result.address, Values::Typed(&values));
            }
        }
    }

    /// Marks the grammar requirements' findings an allowance covers; they
    /// become informational. Findings name requirements as lowering does.
    pub(crate) fn cover_findings(&mut self, findings: &mut [Finding]) {
        let names = &self.names;
        for finding in findings
            .iter_mut()
            .filter(|finding| !finding.values.is_empty())
        {
            let requirement = &finding.check;
            let reason = self.allowances.covering(
                |address| names.get(address) == Some(requirement),
                Values::Text(&finding.values),
            );
            let Some(reason) = reason else {
                continue;
            };
            finding.severity = Severity::Info;
            let _ = write!(finding.message, " (allowed: {reason})");
            finding.allowed = Some(reason);
        }
    }

    /// Findings about the allowances themselves: expired ones, and those
    /// for a behavior in `ran` that covered none of its cases. A behavior
    /// runs whole, so an allowance for one that ran and covered nothing is
    /// stale. A grammar's requirements are evaluated only over what a
    /// change touches, or not at all without one, so covering nothing says
    /// nothing about allowances for them.
    pub(crate) fn findings(&self, ran: &BTreeSet<Address>) -> Vec<Finding> {
        let expired = self.allowances.expired().into_iter().map(|allowance| {
            let until = allowance.until.as_deref().unwrap_or_default();
            let message = format!(
                "the allowance for {} expired on {until}; the findings it covered count again",
                allowance.requirement
            );
            (allowance, message)
        });
        let unused = self
            .allowances
            .unused()
            .into_iter()
            .filter(|allowance| ran.contains(&allowance.requirement))
            .map(|allowance| {
                let message = format!(
                    "the allowance for {} covers no finding; remove it",
                    allowance.requirement
                );
                (allowance, message)
            });
        expired
            .chain(unused)
            .map(|(allowance, message)| Finding {
                severity: Severity::Warning,
                link: allowance.requirement.to_string(),
                check: "allow".to_owned(),
                message,
                introduced: true,
                affects: Vec::new(),
                allowed: None,
                values: BTreeMap::new(),
            })
            .collect()
    }
}

/// The committer date of the commit a checkout reads, `YYYY-MM-DD`: of
/// `HEAD` for a working directory.
pub(crate) fn committer_date(checkout: &crate::repos::Checkout) -> Option<String> {
    let commit = checkout.commit.as_deref().unwrap_or("HEAD");
    crate::repos::git(&checkout.root, &["log", "-1", "--format=%cs", commit])
        .ok()
        .map(|date| date.trim().to_owned())
        .filter(|date| !date.is_empty())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn model(text: &str) -> Model {
        let directory = tempfile::tempdir().unwrap();
        std::fs::write(directory.path().join("mod.lm"), text).unwrap();
        let checked = lemma_lang::check_program(
            &mut lemma_lang::files::WorkingTrees::new(directory.path()),
            "mod.lm",
        );
        assert_eq!(checked.error_lines(), Vec::<String>::new());
        checked.model
    }

    fn finding(file: &str, line: &str) -> Finding {
        let values = [
            ("c", "web::load"),
            ("op", "GET /sessions"),
            ("f", file),
            ("l", line),
        ];
        Finding {
            severity: Severity::Error,
            link: "web::load -> GET /sessions".to_owned(),
            check: "consumer_calls_removed_operation".to_owned(),
            message: "web::load calls GET /sessions, which no longer exists".to_owned(),
            introduced: true,
            affects: Vec::new(),
            allowed: None,
            values: values
                .iter()
                .map(|(name, value)| ((*name).to_owned(), (*value).to_owned()))
                .collect(),
        }
    }

    const ALLOWING: &str = r#"module web
import lemma.http
allow lemma.http.consumer_calls_removed_operation {
    when: f == "src/legacy/sessions.ts", l < 100
    reason: "the legacy client goes with the next major version"
    until: "2027-01-31"
}
"#;

    #[test]
    fn a_grammar_finding_the_condition_selects_is_allowed_and_informational() {
        let model = model(ALLOWING);
        let mut allowances = Allowed::new(&model, Some("2026-11-06".to_owned()));
        let mut findings = vec![
            finding("src/legacy/sessions.ts", "12"),
            finding("src/legacy/sessions.ts", "120"),
            finding("src/app.ts", "12"),
        ];

        allowances.cover_findings(&mut findings);

        let severities: Vec<Severity> = findings.iter().map(|finding| finding.severity).collect();
        assert_eq!(
            severities,
            [Severity::Info, Severity::Error, Severity::Error]
        );
        assert_eq!(
            findings[0].allowed.as_deref(),
            Some("the legacy client goes with the next major version")
        );
        assert_eq!(allowances.findings(&BTreeSet::new()), []);
    }

    #[test]
    fn past_its_date_the_allowance_covers_nothing() {
        let model = model(ALLOWING);
        let mut allowances = Allowed::new(&model, Some("2027-02-01".to_owned()));
        let mut findings = vec![finding("src/legacy/sessions.ts", "12")];

        allowances.cover_findings(&mut findings);

        assert_eq!(findings[0].severity, Severity::Error);
        let reported = allowances.findings(&BTreeSet::new());
        assert!(reported[0].message.contains("expired on 2027-01-31"));
    }
}
