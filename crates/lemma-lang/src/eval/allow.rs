//! Allowances (spec 13, section 8): which findings an `allow` covers.
//!
//! A finding is covered when an allowance names its requirement and the
//! allowance's condition holds for the finding's variables. Past its
//! `until`, compared with a date the caller gives (the head commit's
//! committer date, never the clock), an allowance covers nothing, though
//! it still counts as matching, so it is reported as expired rather than
//! unused.

use std::collections::BTreeMap;

use super::datum::Datum;
use super::facts::StatedFacts;
use crate::model::{Allowance, Model};
use crate::types::{Address, Type};

/// A program's allowances, and how many findings each matched.
pub struct Allowances<'m> {
    declared: &'m [Allowance],
    facts: StatedFacts<'m>,
    date: Option<String>,
    matched: Vec<usize>,
}

/// A finding's variables: typed, as a behavior's case has them, or as
/// text, as a grammar's findings have them.
#[derive(Debug, Clone, Copy)]
pub enum Values<'v> {
    /// Values as behaviors see them.
    Typed(&'v BTreeMap<String, Datum>),
    /// Values written as text; those of integer or boolean variables are
    /// read as such.
    Text(&'v BTreeMap<String, String>),
}

impl<'m> Allowances<'m> {
    /// `model`'s allowances, dated by `date` (`YYYY-MM-DD`).
    pub fn new(model: &'m Model, date: Option<String>) -> Self {
        Self {
            declared: &model.allowances,
            facts: StatedFacts::new(model, None),
            date,
            matched: vec![0; model.allowances.len()],
        }
    }

    /// The reason of the first allowance in force whose requirement
    /// `names` accepts and whose condition holds for `values`.
    pub fn covering(
        &mut self,
        names: impl Fn(&Address) -> bool,
        values: Values<'_>,
    ) -> Option<String> {
        for (index, allowance) in self.declared.iter().enumerate() {
            if !names(&allowance.requirement) {
                continue;
            }
            let bound = typed(values, allowance);
            if !self.facts.holds(&allowance.body, &bound) {
                continue;
            }
            self.matched[index] += 1;
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

    /// Allowances past their `until`.
    pub fn expired(&self) -> Vec<&'m Allowance> {
        self.declared
            .iter()
            .filter(|allowance| self.is_expired(allowance))
            .collect()
    }

    /// Allowances that matched no finding so far.
    pub fn unused(&self) -> Vec<&'m Allowance> {
        self.declared
            .iter()
            .zip(&self.matched)
            .filter(|(allowance, matched)| **matched == 0 && !self.is_expired(allowance))
            .map(|(allowance, _)| allowance)
            .collect()
    }

    fn is_expired(&self, allowance: &Allowance) -> bool {
        match (&allowance.until, &self.date) {
            (Some(until), Some(date)) => date.as_str() > until.as_str(),
            _ => false,
        }
    }
}

/// A finding's values as the allowance's condition reads them.
fn typed(values: Values<'_>, allowance: &Allowance) -> BTreeMap<String, Datum> {
    let texts = match values {
        Values::Typed(typed) => return typed.clone(),
        Values::Text(texts) => texts,
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
