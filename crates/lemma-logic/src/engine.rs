//! The interface every Datalog engine implements.
//!
//! Engines are swapped at compile time: callers are generic over [`Engine`],
//! so only enabled engines are built and calls are static. The boundary is
//! "evaluate this program over these facts", so even a dynamic call would
//! cost nothing measurable; the cost that matters is moving facts, which is
//! why facts are compact rows of interned values.

use std::collections::{BTreeMap, BTreeSet};

use crate::LogicError;
use crate::facts::{FactSet, Tuple};
use crate::program::{Features, Program};

/// A fact: a relation and a row.
#[derive(Debug, Clone, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct Fact {
    /// The relation's name.
    pub relation: String,
    /// The row.
    pub row: Tuple,
}

/// Why a derived fact holds: the rule that produced it and the facts it
/// used, each explained in turn down to input facts.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Derivation {
    /// An input fact.
    Given(Fact),
    /// A fact that does not hold, as a negated condition required.
    Absent(Fact),
    /// A fact derived by a rule.
    Derived {
        /// The fact.
        fact: Fact,
        /// The index of the rule in the program.
        rule: usize,
        /// Explanations of the rule's conditions, in body order (comparisons
        /// have none).
        premises: Vec<Derivation>,
    },
}

impl Derivation {
    /// The fact this derivation explains.
    pub fn fact(&self) -> &Fact {
        match self {
            Self::Given(fact) | Self::Absent(fact) | Self::Derived { fact, .. } => fact,
        }
    }
}

/// What to compute.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Request {
    /// Derived relations to return. Empty means all of them.
    pub outputs: BTreeSet<String>,
    /// Facts to explain, if the engine supports derivations.
    pub explain: Vec<Fact>,
}

/// What an engine computed.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Derived {
    /// Rows of each requested relation, sorted, whatever order the engine
    /// produced them in.
    pub relations: BTreeMap<String, BTreeSet<Tuple>>,
    /// One derivation per explained fact that holds, in request order.
    pub derivations: Vec<Derivation>,
}

impl Derived {
    /// The rows of a relation, sorted.
    pub fn rows(&self, relation: &str) -> impl Iterator<Item = &Tuple> {
        self.relations.get(relation).into_iter().flatten()
    }
}

/// A Datalog engine that evaluates Lemma programs.
pub trait Engine {
    /// A program prepared for this engine, such as generated source or a
    /// compiled binary. Reusable across evaluations.
    type Prepared;
    /// Why preparation or evaluation failed.
    type Error: std::error::Error + Send + Sync + 'static;

    /// The engine's name, for evidence and output.
    fn name(&self) -> &str;

    /// The optional language features the engine supports.
    fn capabilities(&self) -> Features;

    /// Prepares a checked program.
    fn prepare(&self, program: &Program) -> Result<Self::Prepared, Self::Error>;

    /// Evaluates a prepared program over checked facts.
    fn evaluate(
        &self,
        prepared: &Self::Prepared,
        facts: &FactSet,
        request: &Request,
    ) -> Result<Derived, Self::Error>;
}

/// Why a program could not be evaluated: it needs a feature the engine
/// lacks, its facts do not fit it, or the engine failed.
#[derive(Debug, thiserror::Error)]
pub enum EvaluationError<E: std::error::Error + 'static> {
    /// The program or its facts are not valid.
    #[error(transparent)]
    Invalid(#[from] LogicError),
    /// The program uses a feature the engine does not support.
    #[error("the {engine} engine does not support {feature}")]
    Unsupported {
        /// The engine.
        engine: String,
        /// The missing feature.
        feature: &'static str,
    },
    /// The engine failed.
    #[error("the {engine} engine failed")]
    Engine {
        /// The engine.
        engine: String,
        /// What went wrong.
        #[source]
        source: E,
    },
}

/// Checks that `engine` can run `program` over `facts`, then runs it.
pub fn evaluate<E: Engine>(
    engine: &E,
    program: &Program,
    facts: &FactSet,
    request: &Request,
) -> Result<Derived, EvaluationError<E::Error>> {
    facts.check(program)?;
    let needed = Features {
        derivations: !request.explain.is_empty(),
        ..program.features()
    };
    if let Some(feature) = missing_feature(needed, engine.capabilities()) {
        return Err(EvaluationError::Unsupported {
            engine: engine.name().to_owned(),
            feature,
        });
    }
    let wrap = |source| EvaluationError::Engine {
        engine: engine.name().to_owned(),
        source,
    };
    let prepared = engine.prepare(program).map_err(wrap)?;
    engine.evaluate(&prepared, facts, request).map_err(wrap)
}

fn missing_feature(needed: Features, offered: Features) -> Option<&'static str> {
    let checks = [
        (needed.ordering && !offered.ordering, "ordering comparisons"),
        (needed.derivations && !offered.derivations, "derivations"),
        (needed.negation && !offered.negation, "negation"),
        (needed.recursion && !offered.recursion, "recursion"),
    ];
    checks
        .into_iter()
        .find(|(is_missing, _)| *is_missing)
        .map(|(_, feature)| feature)
}
