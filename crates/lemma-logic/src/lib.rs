//! Lemma's Datalog: programs, facts, the interface engines implement, and a
//! reference engine.
//!
//! Programs are engine-neutral and checked once here, so every engine can
//! assume safe, stratified rules over typed relations. See
//! `.specs/08-relational-grammar-and-verification.md`.

mod engine;
mod facts;
mod program;
mod reference;

pub use engine::{Derivation, Derived, Engine, EvaluationError, Fact, Request, evaluate};
pub use facts::{FactSet, Symbols, Tuple};
pub use program::{
    Atom, Column, ColumnType, Comparison, Features, Literal, Program, Relation, RelationRole, Rule,
    Term, Value,
};
pub use reference::ReferenceEngine;

/// Why a program or its facts are not valid.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum LogicError {
    /// Two relations share a name.
    #[error("relation `{relation}` is declared twice")]
    DuplicateRelation {
        /// The name.
        relation: String,
    },
    /// A relation has no columns.
    #[error("relation `{relation}` has no columns")]
    EmptyRelation {
        /// The name.
        relation: String,
    },
    /// A rule uses a relation that is not declared.
    #[error("rule {rule} uses undeclared relation `{relation}`")]
    UnknownRelation {
        /// The rule's index.
        rule: usize,
        /// The name.
        relation: String,
    },
    /// An atom has the wrong number of terms.
    #[error("rule {rule} gives `{relation}` {found} terms; it has {expected} columns")]
    Arity {
        /// The rule's index.
        rule: usize,
        /// The relation.
        relation: String,
        /// Its column count.
        expected: usize,
        /// The terms given.
        found: usize,
    },
    /// A rule derives an input relation.
    #[error("rule {rule} derives `{relation}`, whose facts are supplied, not derived")]
    DerivesInput {
        /// The rule's index.
        rule: usize,
        /// The relation.
        relation: String,
    },
    /// A variable is not bound by any positive atom.
    #[error("rule {rule} uses `{variable}`, which no positive condition binds")]
    Unbound {
        /// The rule's index.
        rule: usize,
        /// The variable.
        variable: String,
    },
    /// A negated atom contains `_`.
    #[error("rule {rule} negates `{relation}` with `_`; name every value or use a helper relation")]
    WildcardInNegation {
        /// The rule's index.
        rule: usize,
        /// The relation.
        relation: String,
    },
    /// A variable or constant does not match its column's type.
    #[error("rule {rule} puts a value of the wrong type in `{relation}.{column}`")]
    TypeMismatch {
        /// The rule's index.
        rule: usize,
        /// The relation.
        relation: String,
        /// The column.
        column: String,
    },
    /// An ordering comparison involves symbols.
    #[error("rule {rule} orders symbols; only numbers can be compared with <, <=, > or >=")]
    OrderedSymbols {
        /// The rule's index.
        rule: usize,
    },
    /// Relations depend on each other through negation.
    #[error("these relations depend on each other through negation: {}", relations.join(", "))]
    NegationCycle {
        /// The relations involved.
        relations: Vec<String>,
    },
    /// Facts were given for a relation the program does not declare.
    #[error("facts were given for undeclared relation `{relation}`")]
    UnknownFactRelation {
        /// The name.
        relation: String,
    },
    /// Facts were given for a derived relation.
    #[error("facts were given for `{relation}`, which rules derive")]
    FactsForDerived {
        /// The name.
        relation: String,
    },
    /// A fact's row does not match its relation's columns.
    #[error("a fact for `{relation}` does not match its columns")]
    MalformedFact {
        /// The name.
        relation: String,
    },
}
