//! Soufflé must compute what the reference engine computes. These tests run
//! `souffle`, so they are ignored by default:
//!
//!     cargo test -p lemma-souffle -- --ignored

#![allow(clippy::unwrap_used, reason = "test setup should fail loudly")]

use lemma_logic::{
    Atom, Column, ColumnType, Comparison, Derivation, Fact, FactSet, Literal, Program,
    ReferenceEngine, Relation, RelationRole, Request, Rule, Term, Value, evaluate,
};
use lemma_souffle::SouffleEngine;

fn relation(name: &str, kinds: &[ColumnType], role: RelationRole) -> Relation {
    Relation {
        name: name.to_owned(),
        columns: kinds
            .iter()
            .enumerate()
            .map(|(position, kind)| Column {
                name: format!("c{position}"),
                kind: *kind,
            })
            .collect(),
        role,
    }
}

fn atom(relation: &str, terms: &[&str]) -> Atom {
    Atom {
        relation: relation.to_owned(),
        terms: terms
            .iter()
            .map(|name| Term::Variable((*name).to_owned()))
            .collect(),
    }
}

/// A consumer breaks when it reads a field the operation returned before a
/// change and no longer returns; a rate rule exercises comparisons.
fn breaking_change() -> (Program, FactSet) {
    use ColumnType::{Number, Symbol};
    let relations = vec![
        relation("returns@base", &[Symbol, Symbol], RelationRole::Input),
        relation("returns@head", &[Symbol, Symbol], RelationRole::Input),
        relation("reads", &[Symbol, Symbol, Symbol], RelationRole::Input),
        relation("rate", &[Symbol, Number], RelationRole::Input),
        relation("removed", &[Symbol, Symbol], RelationRole::Derived),
        relation("broken", &[Symbol, Symbol, Symbol], RelationRole::Derived),
        relation("over_budget", &[Symbol], RelationRole::Derived),
    ];
    let rules = vec![
        Rule {
            head: atom("removed", &["op", "f"]),
            body: vec![
                Literal::Positive(atom("returns@base", &["op", "f"])),
                Literal::Negative(atom("returns@head", &["op", "f"])),
            ],
        },
        Rule {
            head: atom("broken", &["c", "op", "f"]),
            body: vec![
                Literal::Positive(atom("reads", &["c", "op", "f"])),
                Literal::Positive(atom("removed", &["op", "f"])),
            ],
        },
        Rule {
            head: atom("over_budget", &["c"]),
            body: vec![
                Literal::Positive(atom("rate", &["c", "r"])),
                Literal::Compare {
                    left: Term::Variable("r".to_owned()),
                    op: Comparison::Greater,
                    right: Term::Constant(Value::Number(120)),
                },
            ],
        },
    ];
    let program = Program::new(relations, rules).unwrap();
    let (op, status, state, dashboard, sdk) = (
        Value::Symbol(0),
        Value::Symbol(1),
        Value::Symbol(2),
        Value::Symbol(3),
        Value::Symbol(4),
    );
    let mut facts = FactSet::new();
    facts.insert("returns@base", vec![op, status]);
    facts.insert("returns@base", vec![op, state]);
    facts.insert("returns@head", vec![op, state]);
    facts.insert("reads", vec![dashboard, op, status]);
    facts.insert("reads", vec![sdk, op, state]);
    facts.insert("rate", vec![dashboard, Value::Number(188)]);
    facts.insert("rate", vec![sdk, Value::Number(40)]);
    (program, facts)
}

#[test]
#[ignore = "needs souffle on PATH"]
fn derived_relations_match() {
    let (program, facts) = breaking_change();
    let souffle = SouffleEngine::find().unwrap();

    let expected = evaluate(&ReferenceEngine, &program, &facts, &Request::default()).unwrap();
    let actual = evaluate(&souffle, &program, &facts, &Request::default()).unwrap();

    assert_eq!(actual.relations, expected.relations);
    assert_eq!(actual.rows("broken").count(), 1);
}

#[test]
#[ignore = "needs souffle on PATH"]
fn derivations_match() {
    let (program, facts) = breaking_change();
    let souffle = SouffleEngine::find().unwrap();
    let wanted = Fact {
        relation: "broken".to_owned(),
        row: vec![Value::Symbol(3), Value::Symbol(0), Value::Symbol(1)].into(),
    };
    let missing = Fact {
        relation: "broken".to_owned(),
        row: vec![Value::Symbol(4), Value::Symbol(0), Value::Symbol(2)].into(),
    };
    let request = Request {
        explain: vec![wanted, missing],
        ..Request::default()
    };

    let expected = evaluate(&ReferenceEngine, &program, &facts, &request).unwrap();
    let actual = evaluate(&souffle, &program, &facts, &request).unwrap();

    assert_eq!(actual.derivations.len(), 1);
    assert_eq!(actual.derivations, expected.derivations);
    assert!(matches!(
        &actual.derivations[0],
        Derivation::Derived { rule: 1, .. }
    ));
}
