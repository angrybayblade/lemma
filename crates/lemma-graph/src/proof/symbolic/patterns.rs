//! Bindings: patterns, unpacking, and which objects the checked code may
//! assign fields of.

use std::collections::{BTreeMap, BTreeSet};

use super::super::ir::{Expression as Expr, Language, Pattern};
use super::value::{Value, read_field};
use super::{Evaluator, State};

impl Evaluator<'_> {
    /// Binds what `pattern` names to the parts of `value`; an assignment
    /// only to variables already declared.
    pub(super) fn bind(
        &mut self,
        pattern: &Pattern,
        value: Value,
        is_assignment: bool,
        state: &mut State,
    ) -> Result<(), String> {
        match pattern {
            Pattern::Ignore => Ok(()),
            Pattern::Name(name) => {
                if is_assignment && !state.env.contains_key(name) {
                    return Err(format!("`{name}` is assigned before it is declared"));
                }
                state.env.insert(name.clone(), value);
                Ok(())
            }
            Pattern::Record(fields) => {
                for (field, part) in fields {
                    let (read, fails) = read_field(&value, field, false, self.missing())?;
                    self.narrow(&fails, state);
                    self.bind(part, read, is_assignment, state)?;
                }
                Ok(())
            }
            Pattern::RecordRest(fields, rest) => {
                for (field, part) in fields {
                    let (read, fails) = read_field(&value, field, false, self.missing())?;
                    self.narrow(&fails, state);
                    self.bind(part, read, is_assignment, state)?;
                }
                let taken: BTreeSet<&str> =
                    fields.iter().map(|(field, _)| field.as_str()).collect();
                let remaining = match value {
                    Value::Object(all) => Value::Object(without(all, &taken)),
                    Value::Record(all) => Value::Record(without(all, &taken)),
                    _ => return Err("only an object's remaining fields can be gathered".to_owned()),
                };
                self.bind(rest, remaining, is_assignment, state)
            }
            Pattern::Rest(_) => Err("a rest pattern belongs inside a sequence pattern".to_owned()),
            Pattern::Sequence(parts) => {
                let Value::Sequence(items) = value else {
                    return Err("only a sequence of known length can be unpacked".to_owned());
                };
                if parts.iter().any(|part| matches!(part, Pattern::Rest(_))) {
                    return self.unpack_rest(parts, items, is_assignment, state);
                }
                if items.len() != parts.len() {
                    return self.unpack_mismatch(parts, items, is_assignment, state);
                }
                for (part, item) in parts.iter().zip(items) {
                    self.bind(part, item, is_assignment, state)?;
                }
                Ok(())
            }
        }
    }

    /// Unpacks a sequence where one part gathers the items the others leave.
    pub(super) fn unpack_rest(
        &mut self,
        parts: &[Pattern],
        mut items: Vec<Value>,
        is_assignment: bool,
        state: &mut State,
    ) -> Result<(), String> {
        let at = parts
            .iter()
            .position(|part| matches!(part, Pattern::Rest(_)))
            .ok_or("missing rest pattern")?;
        let after = parts.len() - at - 1;
        if items.len() < at + after {
            // Python raises; JavaScript reads undefined for what is missing.
            if self.language != Language::TypeScript {
                self.raise_here("ValueError", state);
                return Ok(());
            }
            items.resize(at + after, Value::Undefined);
        }
        let tail = items.split_off(items.len() - after);
        let gathered = items.split_off(at);
        let Pattern::Rest(rest) = &parts[at] else {
            return Err("missing rest pattern".to_owned());
        };
        for (part, item) in parts[..at].iter().zip(items) {
            self.bind(part, item, is_assignment, state)?;
        }
        self.bind(rest, Value::Sequence(gathered), is_assignment, state)?;
        for (part, item) in parts[at + 1..].iter().zip(tail) {
            self.bind(part, item, is_assignment, state)?;
        }
        Ok(())
    }

    /// Unpacking a sequence of another length: Python raises, JavaScript
    /// reads undefined past the end, and Go does not compile.
    pub(super) fn unpack_mismatch(
        &mut self,
        parts: &[Pattern],
        items: Vec<Value>,
        is_assignment: bool,
        state: &mut State,
    ) -> Result<(), String> {
        match self.language {
            Language::Python => {
                self.raise_here("ValueError", state);
                Ok(())
            }
            Language::TypeScript => {
                let mut items = items.into_iter();
                for part in parts {
                    let item = items.next().unwrap_or(Value::Undefined);
                    self.bind(part, item, is_assignment, state)?;
                }
                Ok(())
            }
            Language::Go => Err("Go assigns as many values as there are names".to_owned()),
        }
    }
}

/// After binding `pattern` to `expression`: a variable bound to a record or
/// sequence literal holds an object nothing else can see.
pub(super) fn track_freshness(pattern: &Pattern, expression: &Expr, fresh: &mut BTreeSet<String>) {
    let mut names = BTreeSet::new();
    pattern_names(pattern, &mut names);
    let is_literal = matches!(
        expression,
        Expr::Record(_) | Expr::Sequence(_) | Expr::Merge(_)
    );
    for name in names {
        if is_literal && matches!(pattern, Pattern::Name(_)) {
            fresh.insert(name);
        } else {
            fresh.remove(&name);
        }
    }
}

pub(super) fn pattern_names(pattern: &Pattern, names: &mut BTreeSet<String>) {
    match pattern {
        Pattern::Name(name) => {
            names.insert(name.clone());
        }
        Pattern::Ignore => {}
        Pattern::Sequence(parts) => {
            for part in parts {
                pattern_names(part, names);
            }
        }
        Pattern::Record(fields) => {
            for (_, part) in fields {
                pattern_names(part, names);
            }
        }
        Pattern::RecordRest(fields, rest) => {
            for (_, part) in fields {
                pattern_names(part, names);
            }
            pattern_names(rest, names);
        }
        Pattern::Rest(inner) => pattern_names(inner, names),
    }
}

/// Assigns a field of a variable's object, which only an object nothing
/// else can see allows: the assignment is then invisible elsewhere.
pub(super) fn set_field(
    name: &str,
    field: &str,
    value: Value,
    state: &mut State,
) -> Result<(), String> {
    if !state.fresh.contains(name) {
        return Err(format!(
            "`{name}` may be seen elsewhere, so assigning its field `{field}` is unsupported"
        ));
    }
    let Some(Value::Object(fields)) = state.env.get_mut(name) else {
        return Err(format!(
            "`{name}` is not a record, so `{field}` cannot be assigned"
        ));
    };
    fields.insert(field.to_owned(), value);
    Ok(())
}

/// The fields of `all` not in `taken`.
pub(super) fn without(
    all: BTreeMap<String, Value>,
    taken: &BTreeSet<&str>,
) -> BTreeMap<String, Value> {
    all.into_iter()
        .filter(|(field, _)| !taken.contains(field.as_str()))
        .collect()
}
