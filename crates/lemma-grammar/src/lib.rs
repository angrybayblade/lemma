//! User-defined grammars, compiled to Lemma's Datalog.
//!
//! A grammar names the kinds of things a system has (entities), the
//! relations between them, rules deriving new relations, requirements whose
//! results are findings, and facts declared by hand. It is written in YAML;
//! `.ls` files with the same meaning come later. See
//! `.specs/08-relational-grammar-and-verification.md`.
//!
//! ```yaml
//! grammar: http-contracts
//! entities:
//!   Operation: { method: Text, path: Text }
//! relations:
//!   returns: { args: [Operation], attributes: { field: Text }, sources: [syntax, spec] }
//!   reads:   { args: [Symbol, Operation], attributes: { field: Text } }
//! rules:
//!   - removed_field(op, f) :- returns@base(op, f), !returns@head(op, f).
//! requirements:
//!   consumer_reads_removed_field:
//!     when: reads(c, op, f), removed_field(op, f)
//!     severity: error
//!     explain: "{c} reads `{f}` from {op}, which no longer returns it"
//! ```

mod compile;
mod rules;
mod twins;

use std::collections::BTreeMap;

use serde::Deserialize;

pub use compile::{CompiledGrammar, RelationInfo, Requirement, Severity, compile};
pub use rules::{ParseError, RawAtom, RawLiteral, RawRule, RawTerm, parse_body, parse_rule};

/// Snapshot names a rule may select with `@`.
pub const SNAPSHOTS: [&str; 2] = ["base", "head"];

/// A grammar as written in YAML.
#[derive(Debug, Clone, Deserialize, PartialEq)]
#[serde(deny_unknown_fields)]
pub struct GrammarFile {
    /// The grammar's name.
    pub grammar: String,
    /// Entity kinds and their attributes, in declaration order.
    #[serde(default)]
    pub entities: yaml_serde::Mapping,
    /// Input relations: facts come from extractors, specs, declarations or
    /// observations.
    #[serde(default)]
    pub relations: yaml_serde::Mapping,
    /// Rules, in Lemma's Datalog syntax.
    #[serde(default)]
    pub rules: Vec<String>,
    /// Requirements: conditions whose matches are findings.
    #[serde(default)]
    pub requirements: BTreeMap<String, RequirementSpec>,
    /// Facts declared by hand.
    #[serde(default)]
    pub facts: Vec<DeclaredFact>,
}

/// An input relation as written.
#[derive(Debug, Clone, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct RelationSpec {
    /// Entity kinds of the relation's arguments, in order.
    #[serde(default)]
    pub args: Vec<String>,
    /// Further columns and their types, in order.
    #[serde(default)]
    pub attributes: yaml_serde::Mapping,
    /// Which sources may produce its facts: syntax, semantic, spec,
    /// declared, observed.
    #[serde(default)]
    pub sources: Vec<String>,
    /// What it means, for people reading findings.
    #[serde(default)]
    pub description: Option<String>,
}

/// A requirement as written.
#[derive(Debug, Clone, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct RequirementSpec {
    /// The condition, a rule body; every match is a finding.
    pub when: String,
    /// How serious a finding is.
    #[serde(default)]
    pub severity: Severity,
    /// A message with `{variable}` placeholders.
    #[serde(default)]
    pub explain: Option<String>,
}

/// A fact written by hand.
#[derive(Debug, Clone, Deserialize, PartialEq)]
#[serde(deny_unknown_fields)]
pub struct DeclaredFact {
    /// The relation.
    pub relation: String,
    /// One value per column: strings for symbols, integers for numbers.
    pub values: Vec<yaml_serde::Value>,
    /// The snapshot it holds in; the head snapshot if absent.
    #[serde(default)]
    pub snapshot: Option<String>,
}

/// Why a grammar could not be compiled.
#[derive(Debug, thiserror::Error)]
pub enum GrammarError {
    /// The YAML is malformed or has unknown fields.
    #[error("invalid grammar file")]
    Yaml(#[from] yaml_serde::Error),
    /// An entity, relation or attribute is declared wrongly.
    #[error("{what}: {message}")]
    Declaration {
        /// What was being declared, such as `relation reads`.
        what: String,
        /// What is wrong.
        message: String,
    },
    /// A type name is neither built in nor a declared entity.
    #[error("{what} uses unknown type `{name}`; use Text, Number, Symbol or a declared entity")]
    UnknownType {
        /// Where.
        what: String,
        /// The type.
        name: String,
    },
    /// Rule or requirement text does not parse.
    #[error("cannot parse `{text}`")]
    Parse {
        /// The text.
        text: String,
        /// Where and why.
        #[source]
        source: ParseError,
    },
    /// A rule refers to something the grammar does not declare.
    #[error("in `{text}`: {message}")]
    Rule {
        /// The rule's text.
        text: String,
        /// What is wrong.
        message: String,
    },
    /// The compiled program is not valid Datalog.
    #[error("the grammar's rules are not valid")]
    Logic(#[from] lemma_logic::LogicError),
}

/// Parses a grammar from YAML text.
pub fn parse_grammar(text: &str) -> Result<GrammarFile, GrammarError> {
    Ok(yaml_serde::from_str(text)?)
}

#[cfg(test)]
mod tests {
    use super::*;
    use lemma_logic::{ReferenceEngine, Request, evaluate};

    const BREAKING_CHANGE: &str = r#"
grammar: http-contracts
entities:
  Operation: { method: Text, path: Text }
relations:
  returns: { args: [Operation], attributes: { field: Text }, sources: [syntax, spec] }
  reads:   { args: [Symbol, Operation], attributes: { field: Text }, sources: [syntax] }
rules:
  - removed_field(op, f) :- returns@base(op, f), !returns@head(op, f).
requirements:
  consumer_reads_removed_field:
    when: reads(c, op, f), removed_field(op, f)
    severity: error
    explain: "{c} reads `{f}` from {op}, which no longer returns it"
facts:
  - { relation: returns, values: [get_account, status], snapshot: base }
  - { relation: returns, values: [get_account, state], snapshot: base }
  - { relation: returns, values: [get_account, state] }
  - { relation: reads, values: [dashboard_card, get_account, status] }
  - { relation: reads, values: [sdk_get, get_account, state] }
"#;

    fn compiled(text: &str) -> Result<CompiledGrammar, GrammarError> {
        compile(&parse_grammar(text)?)
    }

    #[test]
    fn a_breaking_change_grammar_finds_the_broken_consumer() {
        let grammar = compiled(BREAKING_CHANGE).unwrap();
        let requirement = &grammar.requirements[0];

        let derived = evaluate(
            &ReferenceEngine,
            &grammar.program,
            &grammar.facts,
            &Request::default(),
        )
        .unwrap();

        let findings: Vec<Vec<String>> = derived
            .rows(&requirement.relation)
            .map(|row| {
                row.iter()
                    .map(|value| grammar.symbols.display(*value))
                    .collect()
            })
            .collect();
        assert_eq!(findings, [["dashboard_card", "get_account", "status"]]);
        assert_eq!(requirement.variables, ["c", "op", "f"]);
    }

    #[test]
    fn reading_the_base_snapshot_adds_a_base_input() {
        let grammar = compiled(BREAKING_CHANGE).unwrap();

        assert_eq!(
            grammar.input_relation("returns", "base").as_deref(),
            Some("returns@base")
        );
        assert_eq!(grammar.input_relation("reads", "base"), None);
        assert_eq!(grammar.inputs["returns"].columns, ["operation", "field"]);
    }

    #[test]
    fn derived_columns_are_inferred_from_rule_bodies() {
        let grammar = compiled(BREAKING_CHANGE).unwrap();

        let removed = grammar.program.relation("removed_field").unwrap();

        let names: Vec<&str> = removed
            .columns
            .iter()
            .map(|column| column.name.as_str())
            .collect();
        assert_eq!(names, ["op", "f"]);
    }

    #[test]
    fn unknown_types_are_reported() {
        let text = "grammar: g\nrelations:\n  reads: { args: [Widget] }\n";

        let error = compiled(text).unwrap_err();

        assert!(matches!(error, GrammarError::UnknownType { name, .. } if name == "Widget"));
    }

    #[test]
    fn snapshots_on_derived_relations_are_rejected() {
        let text = "grammar: g\nrelations:\n  edge: { args: [Text, Text] }\nrules:\n  - path(a, b) :- edge(a, b).\n  - again(a) :- path@base(a, _).\n";

        let error = compiled(text).unwrap_err();

        assert!(error.to_string().contains("path@base"));
    }

    const UNDECLARED_ROUTES: &str = r"
grammar: g
relations:
  route:     { args: [Text], attributes: { path: Text } }
  specified: { args: [Text], attributes: { path: Text } }
rules:
  - declared(s, p) :- specified(s, p).
requirements:
  route_not_in_spec:
    when: route(s, p), !declared(s, p)
    severity: info
facts:
  - { relation: route, values: [api, /old], snapshot: base }
  - { relation: route, values: [api, /old] }
  - { relation: route, values: [api, /new] }
";

    #[test]
    fn a_requirement_over_one_snapshot_is_also_found_at_the_base() {
        let grammar = compiled(UNDECLARED_ROUTES).unwrap();
        let requirement = &grammar.requirements[0];
        let base_relation = requirement.base_relation.clone().unwrap();

        let derived = evaluate(
            &ReferenceEngine,
            &grammar.program,
            &grammar.facts,
            &Request::default(),
        )
        .unwrap();

        let paths = |relation: &str| -> Vec<String> {
            let mut paths: Vec<String> = derived
                .rows(relation)
                .map(|row| grammar.symbols.display(row[1]))
                .collect();
            paths.sort();
            paths
        };
        assert_eq!(paths(&requirement.relation), ["/new", "/old"]);
        assert_eq!(paths(&base_relation), ["/old"]);
    }

    #[test]
    fn a_requirement_comparing_snapshots_has_no_base_twin() {
        let grammar = compiled(BREAKING_CHANGE).unwrap();

        let requirement = &grammar.requirements[0];

        assert_eq!(requirement.base_relation, None);
    }

    #[test]
    fn unknown_relations_in_rules_are_reported() {
        let text = "grammar: g\nrelations:\n  edge: { args: [Text, Text] }\nrules:\n  - path(a, b) :- edgee(a, b).\n";

        let error = compiled(text).unwrap_err();

        assert!(matches!(error, GrammarError::Rule { .. }));
    }
}
