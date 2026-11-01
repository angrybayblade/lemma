//! Soufflé's JSON provenance, as Lemma derivations.
//!
//! In `-t explain` mode with `format json`, Soufflé prints one object per
//! `explain` command: a proof tree whose nodes name facts as text
//! (`r3_reads(100, 1, 10)`), with `!` for facts a negation required absent,
//! and rule numbers counted per relation (`(R2)` is the relation's second
//! rule). Comparisons and unknown nodes carry no fact and are dropped, as
//! Lemma derivations list only atoms.

use lemma_logic::{ColumnType, Derivation, Fact, Program};
use serde_json::Value as Json;

use crate::parse_row;
use crate::translate::Translation;

/// Every proof in Soufflé's output, in order, skipping facts that do not hold.
pub fn parse(output: &str, program: &Program, translation: &Translation) -> Vec<Derivation> {
    serde_json::Deserializer::from_str(json_start(output))
        .into_iter::<Json>()
        .map_while(Result::ok)
        .filter_map(|document| proof(document.get("proof")?, program, translation))
        .collect()
}

/// Soufflé may print a banner before the first object.
fn json_start(output: &str) -> &str {
    output.find('{').map_or("", |start| &output[start..])
}

fn proof(node: &Json, program: &Program, translation: &Translation) -> Option<Derivation> {
    if let Some(axiom) = node.get("axiom").and_then(Json::as_str) {
        return axiom_derivation(axiom, program, translation);
    }
    let fact = parse_fact(node.get("premises")?.as_str()?, program, translation)?;
    let rule = rule_index(
        node.get("rule-number")?.as_str()?,
        &fact.relation,
        translation,
    )?;
    let premises = node
        .get("children")
        .and_then(Json::as_array)
        .into_iter()
        .flatten()
        .filter_map(|child| proof(child, program, translation))
        .collect();
    Some(Derivation::Derived {
        fact,
        rule,
        premises,
    })
}

fn axiom_derivation(
    text: &str,
    program: &Program,
    translation: &Translation,
) -> Option<Derivation> {
    match text.strip_prefix('!') {
        Some(negated) => parse_fact(negated, program, translation).map(Derivation::Absent),
        None => parse_fact(text, program, translation).map(Derivation::Given),
    }
}

/// `r3_reads(100, 1, 10)` as a fact of the Lemma relation behind `r3_reads`.
fn parse_fact(text: &str, program: &Program, translation: &Translation) -> Option<Fact> {
    let (identifier, rest) = text.trim().split_once('(')?;
    let arguments = rest.strip_suffix(')')?;
    let relation = translation.relation_named(identifier.trim())?;
    let kinds: Vec<ColumnType> = program
        .relation(relation)?
        .columns
        .iter()
        .map(|column| column.kind)
        .collect();
    let row = parse_row(arguments.split(','), &kinds)?;
    Some(Fact {
        relation: relation.to_owned(),
        row,
    })
}

/// `(R2)` for `relation` as the index of that relation's second rule.
fn rule_index(label: &str, relation: &str, translation: &Translation) -> Option<usize> {
    let number: usize = label
        .trim()
        .trim_start_matches("(R")
        .trim_end_matches(')')
        .parse()
        .ok()?;
    translation
        .rules_by_relation
        .get(relation)?
        .get(number.checked_sub(1)?)
        .copied()
}

#[cfg(test)]
mod tests {
    use super::*;
    use lemma_logic::{Atom, Column, Literal, Relation, RelationRole, Rule, Term, Value};

    fn program() -> Program {
        let relation = |name: &str, arity: usize, role| Relation {
            name: name.to_owned(),
            columns: (0..arity)
                .map(|position| Column {
                    name: format!("c{position}"),
                    kind: ColumnType::Number,
                })
                .collect(),
            role,
        };
        let atom = |name: &str, variables: &[&str]| Atom {
            relation: name.to_owned(),
            terms: variables
                .iter()
                .map(|v| Term::Variable((*v).to_owned()))
                .collect(),
        };
        Program::new(
            vec![
                relation("edge", 1, RelationRole::Input),
                relation("gone", 1, RelationRole::Input),
                relation("kept", 1, RelationRole::Derived),
            ],
            vec![Rule {
                head: atom("kept", &["a"]),
                body: vec![
                    Literal::Positive(atom("edge", &["a"])),
                    Literal::Negative(atom("gone", &["a"])),
                ],
            }],
        )
        .unwrap()
    }

    #[test]
    fn souffle_proofs_become_derivations() {
        let program = program();
        let translation = crate::translate(&program);
        let output = r#"Enter command > { "proof":
            { "premises": "r2_kept(7)", "rule-number": "(R1)",
              "children": [ { "axiom": "r0_edge(7)"}, { "axiom": "!r1_gone(7)"} ] },
          "rules": [] }
        { "proof": { "axiom": "Tuple not found"}, "rules": [] }"#;

        let derivations = parse(output, &program, &translation);

        assert_eq!(derivations.len(), 1);
        let Derivation::Derived {
            fact,
            rule,
            premises,
        } = &derivations[0]
        else {
            panic!("expected a derived fact");
        };
        assert_eq!(fact.relation, "kept");
        assert_eq!(*rule, 0);
        assert!(matches!(&premises[0], Derivation::Given(f) if f.row[0] == Value::Number(7)));
        assert!(matches!(&premises[1], Derivation::Absent(f) if f.relation == "gone"));
    }
}
