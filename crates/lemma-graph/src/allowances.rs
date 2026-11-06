//! Allowances in `lemma check` (spec 13, section 8). A finding an `allow`
//! covers is reported as allowed, with the reason, and is informational,
//! so it does not fail the check. Past its `until`, compared with the head
//! commit's committer date rather than the clock, an allowance covers
//! nothing and is reported as expired. An allowance that covers no finding
//! of a check of the whole system is reported, so stale ones get removed.

use std::collections::BTreeMap;
use std::fmt::Write as _;

use lemma_lang::eval::{BehaviorResult, Datum, Outcome, StatedFacts};
use lemma_lang::model::{Allowance, Model};
use lemma_lang::types::{Address, Type};

use crate::behaviors::BehaviorReport;
use crate::check::{Finding, Severity};

/// A program's allowances, and how many findings each covered.
pub(crate) struct Allowances<'m> {
    declared: &'m [Allowance],
    facts: StatedFacts<'m>,
    date: Option<String>,
    covered: Vec<usize>,
}

/// A finding's variables: typed, for a behavior's case, or as text, for a
/// grammar requirement's finding.
enum Values<'v> {
    Typed(&'v BTreeMap<String, Datum>),
    Text(&'v BTreeMap<String, String>),
}

impl<'m> Allowances<'m> {
    /// `model`'s allowances, dated by `date` (`YYYY-MM-DD`), the head
    /// commit's committer date.
    pub(crate) fn new(model: &'m Model, date: Option<String>) -> Self {
        Self {
            declared: &model.allowances,
            facts: StatedFacts::new(model, None),
            date,
            covered: vec![0; model.allowances.len()],
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
                reported.allowed = self.covering(&result.address, &Values::Typed(&values));
            }
        }
    }

    /// Marks the grammar requirements' findings an allowance covers; they
    /// become informational. Lemma check's grammar names requirements by
    /// their own names, as the standard library's `lemma.http` does.
    pub(crate) fn cover_findings(&mut self, findings: &mut [Finding]) {
        for finding in findings
            .iter_mut()
            .filter(|finding| !finding.values.is_empty())
        {
            let values = std::mem::take(&mut finding.values);
            let requirement = finding.check.clone();
            let reason = self.covering_named(&requirement, &Values::Text(&values));
            finding.values = values;
            let Some(reason) = reason else {
                continue;
            };
            finding.severity = Severity::Info;
            let _ = write!(finding.message, " (allowed: {reason})");
            finding.allowed = Some(reason);
        }
    }

    /// Findings about the allowances themselves: expired ones, and, when
    /// `whole_system`, those that covered nothing.
    pub(crate) fn findings(&self, whole_system: bool) -> Vec<Finding> {
        let mut found = Vec::new();
        for (allowance, covered) in self.declared.iter().zip(&self.covered) {
            let message = if let Some(until) = allowance
                .until
                .as_ref()
                .filter(|_| self.is_expired(allowance))
            {
                format!(
                    "the allowance for {} expired on {until}; the findings it covered count again",
                    allowance.requirement
                )
            } else if whole_system && *covered == 0 {
                format!(
                    "the allowance for {} covers no finding; remove it",
                    allowance.requirement
                )
            } else {
                continue;
            };
            found.push(Finding {
                severity: Severity::Warning,
                link: allowance.requirement.to_string(),
                check: "allow".to_owned(),
                message,
                introduced: true,
                affects: Vec::new(),
                allowed: None,
                values: BTreeMap::new(),
            });
        }
        found
    }

    fn covering(&mut self, requirement: &Address, values: &Values<'_>) -> Option<String> {
        self.first_covering(|allowance| allowance.requirement == *requirement, values)
    }

    fn covering_named(&mut self, requirement: &str, values: &Values<'_>) -> Option<String> {
        self.first_covering(
            |allowance| allowance.requirement.name() == requirement,
            values,
        )
    }

    /// The reason of the first allowance in force that `applies` to the
    /// finding and whose condition holds for its values. An expired one
    /// still counts as covering, so it is reported as expired, not unused.
    fn first_covering(
        &mut self,
        applies: impl Fn(&Allowance) -> bool,
        values: &Values<'_>,
    ) -> Option<String> {
        for (index, allowance) in self.declared.iter().enumerate() {
            if !applies(allowance) {
                continue;
            }
            let bound = typed(values, allowance);
            if !self.facts.holds(&allowance.body, &bound) {
                continue;
            }
            self.covered[index] += 1;
            if self.is_expired(allowance) {
                continue;
            }
            return Some(
                allowance
                    .reason
                    .clone()
                    .unwrap_or_else(|| "allowed".to_owned()),
            );
        }
        None
    }

    fn is_expired(&self, allowance: &Allowance) -> bool {
        match (&allowance.until, &self.date) {
            (Some(until), Some(date)) => date.as_str() > until.as_str(),
            _ => false,
        }
    }
}

/// A finding's values as the allowance's condition reads them: text
/// values of integer or boolean variables are read as those.
fn typed(values: &Values<'_>, allowance: &Allowance) -> BTreeMap<String, Datum> {
    let Values::Text(texts) = values else {
        let Values::Typed(typed) = values else {
            return BTreeMap::new();
        };
        return (*typed).clone();
    };
    texts
        .iter()
        .map(|(name, text)| {
            let column = allowance
                .variables
                .iter()
                .find(|column| column.name == *name);
            let value = match column.map(|column| &column.column_type) {
                Some(Type::Int) => text
                    .parse()
                    .map_or_else(|_| Datum::Text(text.clone()), Datum::Int),
                Some(Type::Bool) => text
                    .parse()
                    .map_or_else(|_| Datum::Text(text.clone()), Datum::Bool),
                _ => Datum::Text(text.clone()),
            };
            (name.clone(), value)
        })
        .collect()
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
        std::fs::write(directory.path().join("lemma.lm"), text).unwrap();
        let checked = lemma_lang::check_program(
            &mut lemma_lang::files::WorkingTrees::new(directory.path()),
            "lemma.lm",
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
        let mut allowances = Allowances::new(&model, Some("2026-11-06".to_owned()));
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
        assert_eq!(allowances.findings(true), []);
    }

    #[test]
    fn past_its_date_the_allowance_covers_nothing() {
        let model = model(ALLOWING);
        let mut allowances = Allowances::new(&model, Some("2027-02-01".to_owned()));
        let mut findings = vec![finding("src/legacy/sessions.ts", "12")];

        allowances.cover_findings(&mut findings);

        assert_eq!(findings[0].severity, Severity::Error);
        let reported = allowances.findings(true);
        assert!(reported[0].message.contains("expired on 2027-01-31"));
    }
}
