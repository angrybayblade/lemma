//! Grammars, compiled to Lemma's Datalog.
//!
//! A grammar names relations, rules deriving new relations, requirements
//! whose results are findings, and facts declared by hand. It is written in
//! `.lm` modules, which [`lm`] lowers to a [`GrammarFile`]. See
//! `.specs/08-relational-grammar-and-verification.md` and
//! `.specs/13-lm-grammar.md`.

mod compile;
pub mod lm;
mod rules;
mod twins;

use std::collections::BTreeMap;

use lemma_logic::ColumnType;

pub use compile::{CompiledGrammar, RelationInfo, Requirement, Severity, compile};
pub use rules::{ParseError, RawAtom, RawLiteral, RawRule, RawTerm, parse_body, parse_rule};

/// Snapshot names a rule may select with `@`.
pub const SNAPSHOTS: [&str; 2] = ["base", "head"];

/// A grammar, as `.lm` lowers to it.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct GrammarFile {
    /// The grammar's name.
    pub grammar: String,
    /// Input relations by name, in declaration order: facts come from
    /// extractors, specs, declarations or observations.
    pub relations: Vec<(String, RelationSpec)>,
    /// Rules, in Lemma's Datalog syntax.
    pub rules: Vec<String>,
    /// Requirements: conditions whose matches are findings.
    pub requirements: BTreeMap<String, RequirementSpec>,
    /// Facts declared by hand.
    pub facts: Vec<DeclaredFact>,
}

/// An input relation.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RelationSpec {
    /// Its columns and their types, in order.
    pub columns: Vec<(String, ColumnType)>,
    /// Which sources may produce its facts: syntax, semantic, spec,
    /// declared, observed.
    pub sources: Vec<String>,
    /// What it means, for people reading findings.
    pub description: Option<String>,
}

/// A requirement.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RequirementSpec {
    /// The condition, a rule body; every match is a finding.
    pub when: String,
    /// How serious a finding is.
    pub severity: Severity,
    /// A message with `{variable}` placeholders.
    pub explain: Option<String>,
}

/// One value of a declared fact.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum FactValue {
    /// A symbol.
    Text(String),
    /// A number.
    Number(i64),
}

/// A fact written by hand.
#[derive(Debug, Clone, PartialEq)]
pub struct DeclaredFact {
    /// The relation.
    pub relation: String,
    /// One value per column.
    pub values: Vec<FactValue>,
    /// The snapshot it holds in; the head snapshot if absent.
    pub snapshot: Option<String>,
}

/// Why a grammar could not be compiled.
#[derive(Debug, thiserror::Error)]
pub enum GrammarError {
    /// An entity, relation or attribute is declared wrongly.
    #[error("{what}: {message}")]
    Declaration {
        /// What was being declared, such as `relation reads`.
        what: String,
        /// What is wrong.
        message: String,
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

#[cfg(test)]
mod tests {
    use super::*;
    use lemma_logic::{ReferenceEngine, Request, evaluate};

    fn relation(name: &str, columns: &[&str]) -> (String, RelationSpec) {
        let spec = RelationSpec {
            columns: columns
                .iter()
                .map(|column| ((*column).to_owned(), ColumnType::Symbol))
                .collect(),
            sources: vec!["declared".to_owned()],
            description: None,
        };
        (name.to_owned(), spec)
    }

    fn fact(relation: &str, values: &[&str], snapshot: Option<&str>) -> DeclaredFact {
        DeclaredFact {
            relation: relation.to_owned(),
            values: values
                .iter()
                .map(|value| FactValue::Text((*value).to_owned()))
                .collect(),
            snapshot: snapshot.map(str::to_owned),
        }
    }

    fn requirement(when: &str, severity: Severity) -> RequirementSpec {
        RequirementSpec {
            when: when.to_owned(),
            severity,
            explain: None,
        }
    }

    fn breaking_change() -> GrammarFile {
        GrammarFile {
            grammar: "http-contracts".to_owned(),
            relations: vec![
                relation("returns", &["operation", "field"]),
                relation("reads", &["symbol", "operation", "field"]),
            ],
            rules: vec![
                "removed_field(op, f) :- returns@base(op, f), !returns@head(op, f).".to_owned(),
            ],
            requirements: BTreeMap::from([(
                "consumer_reads_removed_field".to_owned(),
                requirement("reads(c, op, f), removed_field(op, f)", Severity::Error),
            )]),
            facts: vec![
                fact("returns", &["get_account", "status"], Some("base")),
                fact("returns", &["get_account", "state"], Some("base")),
                fact("returns", &["get_account", "state"], None),
                fact("reads", &["dashboard_card", "get_account", "status"], None),
                fact("reads", &["sdk_get", "get_account", "state"], None),
            ],
        }
    }

    #[test]
    fn a_breaking_change_grammar_finds_the_broken_consumer() {
        let grammar = compile(&breaking_change()).unwrap();
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
        let grammar = compile(&breaking_change()).unwrap();

        assert_eq!(
            grammar.input_relation("returns", "base").as_deref(),
            Some("returns@base")
        );
        assert_eq!(grammar.input_relation("reads", "base"), None);
        assert_eq!(grammar.inputs["returns"].columns, ["operation", "field"]);
    }

    #[test]
    fn derived_columns_are_inferred_from_rule_bodies() {
        let grammar = compile(&breaking_change()).unwrap();

        let removed = grammar.program.relation("removed_field").unwrap();

        let names: Vec<&str> = removed
            .columns
            .iter()
            .map(|column| column.name.as_str())
            .collect();
        assert_eq!(names, ["op", "f"]);
    }

    fn edges_with(rule: &str) -> GrammarFile {
        GrammarFile {
            grammar: "g".to_owned(),
            relations: vec![relation("edge", &["a", "b"])],
            rules: vec![rule.to_owned()],
            ..GrammarFile::default()
        }
    }

    #[test]
    fn snapshots_on_derived_relations_are_rejected() {
        let mut file = edges_with("path(a, b) :- edge(a, b).");
        file.rules.push("far(a, b) :- path@base(a, b).".to_owned());

        let error = compile(&file).unwrap_err();

        assert!(error.to_string().contains("path@base"));
    }

    fn undeclared_routes() -> GrammarFile {
        GrammarFile {
            grammar: "g".to_owned(),
            relations: vec![
                relation("route", &["service", "path"]),
                relation("specified", &["service", "path"]),
            ],
            rules: vec!["declared(s, p) :- specified(s, p).".to_owned()],
            requirements: BTreeMap::from([(
                "route_not_in_spec".to_owned(),
                requirement("route(s, p), !declared(s, p)", Severity::Info),
            )]),
            facts: vec![
                fact("route", &["api", "/old"], Some("base")),
                fact("route", &["api", "/old"], None),
                fact("route", &["api", "/new"], None),
            ],
        }
    }

    #[test]
    fn a_requirement_over_one_snapshot_is_also_found_at_the_base() {
        let grammar = compile(&undeclared_routes()).unwrap();
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
    fn facts_the_grammar_declares_hold_at_the_base_for_a_twin() {
        let file = GrammarFile {
            grammar: "g".to_owned(),
            relations: vec![
                relation("route", &["service", "path"]),
                relation("deprecated", &["path"]),
            ],
            requirements: BTreeMap::from([(
                "serves_deprecated".to_owned(),
                requirement("route(s, p), deprecated(p)", Severity::Warning),
            )]),
            facts: vec![
                fact("route", &["api", "/old"], Some("base")),
                fact("route", &["api", "/old"], None),
                fact("deprecated", &["/old"], None),
            ],
            ..GrammarFile::default()
        };
        let grammar = compile(&file).unwrap();
        let base_relation = grammar.requirements[0].base_relation.clone().unwrap();

        let derived = evaluate(
            &ReferenceEngine,
            &grammar.program,
            &grammar.facts,
            &Request::default(),
        )
        .unwrap();

        assert_eq!(derived.rows(&base_relation).count(), 1);
    }

    #[test]
    fn a_requirement_comparing_snapshots_has_no_base_twin() {
        let grammar = compile(&breaking_change()).unwrap();

        let requirement = &grammar.requirements[0];

        assert_eq!(requirement.base_relation, None);
    }

    #[test]
    fn unknown_relations_in_rules_are_reported() {
        let file = edges_with("path(a, b) :- edgee(a, b).");

        let error = compile(&file).unwrap_err();

        assert!(matches!(error, GrammarError::Rule { .. }));
    }
}
