//! Base-snapshot twins of requirements that read a single snapshot.
//!
//! A requirement such as "a route the spec does not declare" holds or not
//! in one snapshot; a change did not cause a finding that already held at
//! the base. Its twin evaluates the same rules over the base snapshot's
//! inputs, so a finding can be reported as introduced only when its twin
//! does not hold. Requirements that compare snapshots (anything reading
//! `@base`) are about the change by construction and get no twin.

use std::collections::{BTreeMap, BTreeSet};

use crate::compile::RelationInfo;
use crate::rules::{RawAtom, RawLiteral, RawRule};

/// Twin rules to add to a program, and each twinned requirement's twin
/// relation.
pub(crate) struct Twins {
    /// The twin rules, each with the text of the rule it copies.
    pub rules: Vec<(String, RawRule)>,
    /// The relation holding each twinned requirement's base findings, by
    /// requirement relation (`requirement:<name>`).
    pub relations: BTreeMap<String, String>,
}

/// Twins for every requirement in `rules` (requirement rules included)
/// that reads no `@base` input, directly or through derived relations.
pub(crate) fn base_twins(
    rules: &[(String, RawRule)],
    requirements: &[String],
    inputs: &BTreeMap<String, RelationInfo>,
) -> Twins {
    let mut by_head: BTreeMap<&str, Vec<&(String, RawRule)>> = BTreeMap::new();
    for rule in rules {
        by_head
            .entry(rule.1.head.relation.as_str())
            .or_default()
            .push(rule);
    }
    let reading_base = relations_reading_base(rules);
    let mut twins = Twins {
        rules: Vec::new(),
        relations: BTreeMap::new(),
    };
    let mut twinned: BTreeSet<&str> = BTreeSet::new();
    for requirement in requirements {
        if reading_base.contains(requirement) {
            continue;
        }
        for derived in derived_closure(requirement, &by_head) {
            if !twinned.insert(derived) {
                continue;
            }
            for (text, rule) in &by_head[derived] {
                twins
                    .rules
                    .push((format!("{text} (at base)"), twin_rule(rule, inputs)));
            }
        }
        twins
            .relations
            .insert(requirement.clone(), base_name(requirement));
    }
    twins
}

/// The derived relations whose rules read a `@base` input, directly or
/// through another such relation.
fn relations_reading_base(rules: &[(String, RawRule)]) -> BTreeSet<String> {
    let mut reading = BTreeSet::new();
    loop {
        let mut grew = false;
        for (_, rule) in rules {
            if reading.contains(&rule.head.relation) {
                continue;
            }
            let reads_base = rule.body.iter().filter_map(atom).any(|atom| {
                atom.snapshot.as_deref() == Some("base") || reading.contains(&atom.relation)
            });
            if reads_base {
                reading.insert(rule.head.relation.clone());
                grew = true;
            }
        }
        if !grew {
            return reading;
        }
    }
}

/// `relation` and every derived relation its rules read, transitively.
fn derived_closure<'a>(
    relation: &'a str,
    by_head: &BTreeMap<&'a str, Vec<&'a (String, RawRule)>>,
) -> BTreeSet<&'a str> {
    let mut closure = BTreeSet::new();
    let mut pending = vec![relation];
    while let Some(next) = pending.pop() {
        let Some(defining) = by_head.get(next) else {
            continue;
        };
        if !closure.insert(next) {
            continue;
        }
        for (_, rule) in defining {
            pending.extend(
                rule.body
                    .iter()
                    .filter_map(atom)
                    .map(|atom| atom.relation.as_str()),
            );
        }
    }
    closure
}

/// `rule` over the base snapshot: inputs read `@base`, derived relations
/// replaced by their twins.
fn twin_rule(rule: &RawRule, inputs: &BTreeMap<String, RelationInfo>) -> RawRule {
    let twin_atom = |atom: &RawAtom| {
        if inputs.contains_key(&atom.relation) {
            return RawAtom {
                snapshot: Some("base".to_owned()),
                ..atom.clone()
            };
        }
        RawAtom {
            relation: base_name(&atom.relation),
            ..atom.clone()
        }
    };
    let body = rule
        .body
        .iter()
        .map(|literal| match literal {
            RawLiteral::Positive(atom) => RawLiteral::Positive(twin_atom(atom)),
            RawLiteral::Negative(atom) => RawLiteral::Negative(twin_atom(atom)),
            RawLiteral::Compare(..) => literal.clone(),
        })
        .collect();
    RawRule {
        head: RawAtom {
            relation: base_name(&rule.head.relation),
            ..rule.head.clone()
        },
        body,
    }
}

/// A derived relation's twin: `name@base`, as base inputs are named.
fn base_name(relation: &str) -> String {
    format!("{relation}@base")
}

fn atom(literal: &RawLiteral) -> Option<&RawAtom> {
    match literal {
        RawLiteral::Positive(atom) | RawLiteral::Negative(atom) => Some(atom),
        RawLiteral::Compare(..) => None,
    }
}
