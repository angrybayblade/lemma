//! Lemma programs as Soufflé source.
//!
//! Relation and variable names are rewritten to identifiers Soufflé accepts
//! (`r0_reads`, `v_op`), so any Lemma name works and the mapping back is a
//! table lookup. Symbols and numbers are both Soufflé `number`s: symbols
//! are already interned, and integers compare faster than strings.

use std::collections::BTreeMap;
use std::fmt::Write as _;

use lemma_logic::{Atom, Literal, Program, RelationRole, Rule, Term, Value};

/// A program in Soufflé's language, with the names used for each relation.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Translation {
    /// The Soufflé source.
    pub source: String,
    /// Soufflé identifier of each relation, by Lemma name.
    pub identifiers: BTreeMap<String, String>,
    /// For each Lemma relation, the index in the program of its rules, in
    /// order. Soufflé numbers rules per relation (`R1`, `R2`, ...).
    pub rules_by_relation: BTreeMap<String, Vec<usize>>,
}

impl Translation {
    /// The Lemma name of a Soufflé relation identifier.
    pub fn relation_named(&self, identifier: &str) -> Option<&str> {
        self.identifiers
            .iter()
            .find(|(_, id)| id.as_str() == identifier)
            .map(|(name, _)| name.as_str())
    }

    /// The file a relation's facts are read from or written to.
    pub fn file_of(&self, relation: &str) -> Option<String> {
        self.identifiers.get(relation).map(|id| format!("{id}.tsv"))
    }
}

/// Translates a checked program. Every input relation is read from
/// `<identifier>.tsv`; every derived relation is written to one.
pub fn translate(program: &Program) -> Translation {
    let identifiers: BTreeMap<String, String> = program
        .relations()
        .enumerate()
        .map(|(position, relation)| {
            let identifier = format!("r{position}_{}", sanitize(&relation.name));
            (relation.name.clone(), identifier)
        })
        .collect();
    let mut source = String::new();
    for relation in program.relations() {
        let identifier = &identifiers[&relation.name];
        let columns: Vec<String> = relation
            .columns
            .iter()
            .enumerate()
            .map(|(position, column)| format!("c{position}_{}: number", sanitize(&column.name)))
            .collect();
        let _ = writeln!(source, ".decl {identifier}({})", columns.join(", "));
        let direction = match relation.role {
            RelationRole::Input => "input",
            RelationRole::Derived => "output",
        };
        let _ = writeln!(
            source,
            ".{direction} {identifier}(IO=file, filename=\"{identifier}.tsv\")"
        );
    }
    let mut rules_by_relation: BTreeMap<String, Vec<usize>> = BTreeMap::new();
    for (index, rule) in program.rules().iter().enumerate() {
        let _ = writeln!(source, "{}", rule_source(rule, &identifiers));
        rules_by_relation
            .entry(rule.head.relation.clone())
            .or_default()
            .push(index);
    }
    Translation {
        source,
        identifiers,
        rules_by_relation,
    }
}

fn rule_source(rule: &Rule, identifiers: &BTreeMap<String, String>) -> String {
    let body: Vec<String> = rule
        .body
        .iter()
        .map(|literal| match literal {
            Literal::Positive(atom) => atom_source(atom, identifiers),
            Literal::Negative(atom) => format!("!{}", atom_source(atom, identifiers)),
            Literal::Compare { left, op, right } => {
                format!(
                    "{} {} {}",
                    term_source(left),
                    op.symbol(),
                    term_source(right)
                )
            }
        })
        .collect();
    format!(
        "{} :- {}.",
        atom_source(&rule.head, identifiers),
        body.join(", ")
    )
}

fn atom_source(atom: &Atom, identifiers: &BTreeMap<String, String>) -> String {
    let terms: Vec<String> = atom.terms.iter().map(term_source).collect();
    format!("{}({})", identifiers[&atom.relation], terms.join(", "))
}

fn term_source(term: &Term) -> String {
    match term {
        Term::Variable(name) => format!("v_{}", sanitize(name)),
        Term::Constant(Value::Symbol(id)) => id.to_string(),
        Term::Constant(Value::Number(number)) => number.to_string(),
        Term::Wildcard => "_".to_owned(),
    }
}

/// Letters, digits and underscores only.
fn sanitize(name: &str) -> String {
    name.chars()
        .map(|c| if c.is_ascii_alphanumeric() { c } else { '_' })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use lemma_logic::{Column, ColumnType, Comparison, Relation};

    fn relation(name: &str, arity: usize, role: RelationRole) -> Relation {
        Relation {
            name: name.to_owned(),
            columns: (0..arity)
                .map(|position| Column {
                    name: format!("arg{position}"),
                    kind: ColumnType::Number,
                })
                .collect(),
            role,
        }
    }

    fn variable(name: &str) -> Term {
        Term::Variable(name.to_owned())
    }

    #[test]
    fn rules_become_souffle_clauses_with_safe_names() {
        let relations = vec![
            relation("rate@head", 2, RelationRole::Input),
            relation("over budget", 1, RelationRole::Derived),
        ];
        let rules = vec![Rule {
            head: Atom {
                relation: "over budget".to_owned(),
                terms: vec![variable("s")],
            },
            body: vec![
                Literal::Positive(Atom {
                    relation: "rate@head".to_owned(),
                    terms: vec![variable("s"), variable("r")],
                }),
                Literal::Compare {
                    left: variable("r"),
                    op: Comparison::Greater,
                    right: Term::Constant(Value::Number(120)),
                },
            ],
        }];
        let program = Program::new(relations, rules).unwrap();

        let translation = translate(&program);

        assert!(
            translation
                .source
                .contains(".decl r1_rate_head(c0_arg0: number, c1_arg1: number)")
        );
        assert!(
            translation
                .source
                .contains(".input r1_rate_head(IO=file, filename=\"r1_rate_head.tsv\")")
        );
        assert!(
            translation
                .source
                .contains("r0_over_budget(v_s) :- r1_rate_head(v_s, v_r), v_r > 120.")
        );
        assert_eq!(
            translation.relation_named("r1_rate_head"),
            Some("rate@head")
        );
        assert_eq!(translation.rules_by_relation["over budget"], [0]);
    }
}
