//! Facts: rows of values for each input relation, and the interner that
//! turns strings into compact symbols.

use std::collections::{BTreeMap, BTreeSet, HashMap};

use crate::LogicError;
use crate::program::{ColumnType, Program, Relation, RelationRole, Value};

/// One row of a relation.
pub type Tuple = Box<[Value]>;

/// Interns strings as `u32` symbols, so facts are compact and engines
/// compare integers. Ids are assigned in first-seen order.
#[derive(Debug, Default, Clone, PartialEq, Eq)]
pub struct Symbols {
    ids: HashMap<String, u32>,
    names: Vec<String>,
}

impl Symbols {
    /// An empty interner.
    pub fn new() -> Self {
        Self::default()
    }

    /// The symbol for `name`, assigning the next id if it is new.
    pub fn intern(&mut self, name: &str) -> Value {
        if let Some(&id) = self.ids.get(name) {
            return Value::Symbol(id);
        }
        let id = u32::try_from(self.names.len()).unwrap_or(u32::MAX);
        self.ids.insert(name.to_owned(), id);
        self.names.push(name.to_owned());
        Value::Symbol(id)
    }

    /// The symbol for `name`, if it has been interned.
    pub fn get(&self, name: &str) -> Option<Value> {
        self.ids.get(name).map(|&id| Value::Symbol(id))
    }

    /// The string behind a symbol.
    pub fn name(&self, id: u32) -> Option<&str> {
        self.names.get(id as usize).map(String::as_str)
    }

    /// A value as text: a symbol's string, or a number.
    pub fn display(&self, value: Value) -> String {
        match value {
            Value::Symbol(id) => self
                .name(id)
                .map_or_else(|| format!("#{id}"), str::to_owned),
            Value::Number(number) => number.to_string(),
        }
    }

    /// How many symbols there are.
    pub fn len(&self) -> usize {
        self.names.len()
    }

    /// Whether no symbol has been interned.
    pub fn is_empty(&self) -> bool {
        self.names.is_empty()
    }
}

/// Facts for input relations. Rows are kept sorted and without duplicates,
/// so every engine sees the same input in the same order.
#[derive(Debug, Default, Clone, PartialEq, Eq)]
pub struct FactSet {
    relations: BTreeMap<String, BTreeSet<Tuple>>,
}

impl FactSet {
    /// An empty set.
    pub fn new() -> Self {
        Self::default()
    }

    /// Adds a row. Returns whether it was new.
    pub fn insert(&mut self, relation: &str, row: impl Into<Tuple>) -> bool {
        self.relations
            .entry(relation.to_owned())
            .or_default()
            .insert(row.into())
    }

    /// The rows of a relation, sorted.
    pub fn rows(&self, relation: &str) -> impl Iterator<Item = &Tuple> {
        self.relations.get(relation).into_iter().flatten()
    }

    /// Relations with at least one row, sorted by name.
    pub fn relation_names(&self) -> impl Iterator<Item = &str> {
        self.relations.keys().map(String::as_str)
    }

    /// How many rows there are in total.
    pub fn len(&self) -> usize {
        self.relations.values().map(BTreeSet::len).sum()
    }

    /// Whether there are no rows.
    pub fn is_empty(&self) -> bool {
        self.relations.values().all(BTreeSet::is_empty)
    }

    /// Checks that every row belongs to an input relation of `program` and
    /// matches its columns.
    pub fn check(&self, program: &Program) -> Result<(), LogicError> {
        for (name, rows) in &self.relations {
            let relation =
                program
                    .relation(name)
                    .ok_or_else(|| LogicError::UnknownFactRelation {
                        relation: name.clone(),
                    })?;
            if relation.role != RelationRole::Input {
                return Err(LogicError::FactsForDerived {
                    relation: name.clone(),
                });
            }
            if !rows.iter().all(|row| fits(row, relation)) {
                return Err(LogicError::MalformedFact {
                    relation: name.clone(),
                });
            }
        }
        Ok(())
    }
}

/// Whether a row has one value per column, each of the column's type.
fn fits(row: &Tuple, relation: &Relation) -> bool {
    row.len() == relation.columns.len()
        && row.iter().zip(&relation.columns).all(|(value, column)| {
            matches!(
                (value, column.kind),
                (Value::Symbol(_), ColumnType::Symbol) | (Value::Number(_), ColumnType::Number)
            )
        })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn interning_is_stable_and_reversible() {
        let mut symbols = Symbols::new();

        let first = symbols.intern("billing");
        let again = symbols.intern("billing");
        let second = symbols.intern("dashboard");

        assert_eq!(first, again);
        assert_ne!(first, second);
        assert_eq!(symbols.display(second), "dashboard");
        assert_eq!(symbols.len(), 2);
    }

    #[test]
    fn rows_are_sorted_and_deduplicated() {
        let mut facts = FactSet::new();

        facts.insert("edge", vec![Value::Number(2), Value::Number(1)]);
        facts.insert("edge", vec![Value::Number(1), Value::Number(2)]);
        let duplicate = facts.insert("edge", vec![Value::Number(1), Value::Number(2)]);

        let rows: Vec<_> = facts.rows("edge").collect();
        assert!(!duplicate);
        assert_eq!(rows.len(), 2);
        assert_eq!(rows[0][0], Value::Number(1));
    }
}
