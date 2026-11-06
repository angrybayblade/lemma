//! Running behaviors: their statements, cases and outcomes (spec 13,
//! sections 5, 9 and 11).
//!
//! Evaluation is pure. Facts and evidence come through a [`World`], so the
//! command line can run checkers and keep their evidence while tests hand
//! in answers. Iteration is sorted, and nothing reads a clock, so the same
//! facts and evidence give the same outcome.

mod allow;
mod datum;
mod facts;
mod run;

#[cfg(test)]
mod tests;

pub use allow::{Allowances, Values};
pub use datum::{Absence, Datum, Evidence, FactDatum, Truth};
pub use facts::{Facts, Row, StatedFacts};
pub use run::{run_behavior, run_check};

use crate::model::{Checker, Predicate, Severity};
use crate::span::Span;
use crate::syntax::ast::Snapshot;
use crate::types::Address;

/// Where a running behavior gets facts and evidence.
pub trait World {
    /// The facts of a relation, graph or rule at a snapshot.
    fn facts(&mut self, predicate: &Predicate, snapshot: Snapshot) -> Facts;

    /// A checker's evidence for one set of arguments.
    fn evidence(&mut self, demand: &Demand<'_>) -> Evidence;
}

/// A behavior's request for evidence.
#[derive(Debug, Clone)]
pub struct Demand<'a> {
    /// The checker.
    pub checker: &'a Checker,
    /// Its arguments, by parameter name, in the checker's order.
    pub arguments: Vec<(String, Datum)>,
    /// The case asking, for logs.
    pub case: &'a str,
}

/// A requirement's outcome, worst last.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum Outcome {
    /// Its assertions hold within the evidence's scope.
    Satisfied,
    /// Stored evidence no longer matches its inputs, and running was not
    /// allowed.
    Stale,
    /// Evidence, resolution or coverage it needs is unavailable.
    Unknown,
    /// The evidence holds a counterexample.
    Violated,
}

impl Outcome {
    /// Its name, as reports write it.
    pub fn as_str(self) -> &'static str {
        match self {
            Outcome::Satisfied => "satisfied",
            Outcome::Stale => "stale",
            Outcome::Unknown => "unknown",
            Outcome::Violated => "violated",
        }
    }
}

/// What running one behavior found.
#[derive(Debug, Clone, PartialEq)]
pub struct BehaviorResult {
    /// The behavior.
    pub address: Address,
    /// Its identity: its address, with its arguments when it has
    /// parameters, such as `m::forwards(field="retriable")`.
    pub identity: String,
    /// How serious a violation is, after the check's override.
    pub severity: Severity,
    /// The worst of its cases, or unknown when no assertion ran or a loop
    /// read an incompletely covered relation.
    pub outcome: Outcome,
    /// Every case, the root first, each before the cases nested in it.
    pub cases: Vec<CaseResult>,
    /// Why the outcome is worse than its cases': no assertion ran, or a
    /// loop's domain is incompletely covered.
    pub notes: Vec<String>,
    /// The evidence its cases used.
    pub evidence: Vec<EvidenceUse>,
}

/// One case: the root, or one iteration of a `for`.
#[derive(Debug, Clone, PartialEq)]
pub struct CaseResult {
    /// The behavior's identity and the values of every enclosing loop,
    /// such as `m::forwards_retriable[flag=true]`.
    pub identity: String,
    /// The loop values, outermost first.
    pub values: Vec<(String, Datum)>,
    /// How it ended.
    pub outcome: Outcome,
    /// What ended it, when it did not hold.
    pub problem: Option<Problem>,
}

/// Why a case did not hold.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Problem {
    /// The module of the statement that ended it.
    pub module: String,
    /// Where the statement is.
    pub span: Span,
    /// The expression as written.
    pub text: String,
    /// What happened, in a sentence.
    pub message: String,
    /// Each operand of a failed comparison and its value.
    pub operands: Vec<(String, String)>,
    /// The `because` text.
    pub because: Option<String>,
}

/// Evidence a case used.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct EvidenceUse {
    /// The checker.
    pub checker: Address,
    /// The case that demanded it.
    pub case: String,
    /// The evidence's key, when there was evidence.
    pub key: Option<String>,
    /// Whether stored evidence was reused.
    pub was_reused: bool,
    /// Why there was none, when there was none.
    pub unavailable: Option<String>,
}
