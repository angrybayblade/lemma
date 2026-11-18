//! Loops over sequences. A loop runs its body once for each item, so it is
//! read exactly where the length of the sequence is known: a literal, what
//! a call model returns, or an input whose contract states a `max_length`.

use std::collections::BTreeSet;

use super::super::ir::{ForEach, Language, Raised, Statement};
use super::patterns::pattern_names;
use super::value::{Term, Value};
use super::{Evaluator, Jump, Shadowed, State, restore};

/// What a loop runs over on the paths where a condition holds: the items of
/// a sequence, or nothing where looping raises.
pub(super) struct Iterated {
    pub when: Term,
    pub items: Option<Vec<Value>>,
}

impl Evaluator<'_> {
    /// `for item in iterable`: the body once for each item, in order.
    pub(super) fn for_each(
        &mut self,
        line: usize,
        each: &ForEach,
        mut state: State,
    ) -> Result<Vec<State>, String> {
        let iterable = self.expression(&each.iterable, &mut state)?;
        let mut finished = std::mem::take(&mut state.pending);
        let mut iterated = Vec::new();
        self.iterated(&iterable, &Term::boolean(true), &mut iterated)
            .map_err(|why| format!("the loop at line {line} {why}"))?;
        for Iterated { when, items } in iterated {
            let mut path = state.clone();
            path.guard = path.guard.and(&when);
            if path.guard.code == "false" {
                continue;
            }
            let Some(items) = items else {
                self.raise_where(&Term::boolean(true), Raised::Implicit, &mut path);
                finished.append(&mut path.pending);
                continue;
            };
            finished.extend(self.passes(each, &items, path)?);
        }
        Ok(finished)
    }

    /// The sequences a value may be, each with the condition it is that one
    /// under. Looping over null raises in Python and JavaScript. A refusal
    /// reads on from what loops: "the loop at line 3", "a comprehension".
    pub(super) fn iterated(
        &self,
        value: &Value,
        when: &Term,
        found: &mut Vec<Iterated>,
    ) -> Result<(), String> {
        match value {
            Value::Sequence(items) => found.push(Iterated {
                when: when.clone(),
                items: Some(items.clone()),
            }),
            Value::Choice(test, then, otherwise) => {
                self.iterated(then, &when.and(test), found)?;
                self.iterated(otherwise, &when.and(&test.not()), found)?;
            }
            // A nil Go slice has no items, but nothing here says a nil value
            // is a slice.
            Value::Null | Value::Undefined if self.language != Language::Go => {
                found.push(Iterated {
                    when: when.clone(),
                    items: None,
                });
            }
            Value::Unreachable => {}
            _ => {
                return Err(
                    "runs over a value whose items are not known here; a loop is read over a sequence of known length, which a contract states for an input with `max_length`"
                        .to_owned(),
                );
            }
        }
        Ok(())
    }

    /// Runs the body once for each item. A path leaves the loop where it
    /// breaks, returns or raises; Python's `else` runs where none did.
    fn passes(
        &mut self,
        each: &ForEach,
        items: &[Value],
        state: State,
    ) -> Result<Vec<State>, String> {
        let names = bound_names(each);
        // A Python loop binds variables of the function; in TypeScript and
        // Go they are the loop's own, and each pass has its own.
        let has_own_variables = self.language != Language::Python;
        let shadowed: Vec<Shadowed> = names
            .iter()
            .filter(|_| has_own_variables)
            .map(|name| Shadowed::of(name, &state))
            .collect();
        let mut running = vec![state];
        let mut left = Vec::new();
        for (position, item) in items.iter().enumerate() {
            let mut next = Vec::new();
            for mut path in running {
                self.bind_pass(each, position, item, &mut path)?;
                self.pass(&each.body, path, &mut next, &mut left)?;
            }
            if next.len() + left.len() > self.contract.max_paths {
                return Err(format!(
                    "source exceeds max_paths ({})",
                    self.contract.max_paths
                ));
            }
            running = next;
        }
        let mut finished = self.statements(&each.otherwise, running)?;
        finished.append(&mut left);
        for path in &mut finished {
            for name in &shadowed {
                restore(path, name);
            }
        }
        Ok(finished)
    }

    /// Binds what a pass names to its position and its item. In TypeScript
    /// and Go each pass has its own variables.
    fn bind_pass(
        &mut self,
        each: &ForEach,
        position: usize,
        item: &Value,
        path: &mut State,
    ) -> Result<(), String> {
        let names = bound_names(each);
        for name in &names {
            if self.language != Language::Python {
                self.declare(name, path);
            }
        }
        if let Some(index) = &each.index {
            let position = i64::try_from(position).map_err(|e| e.to_string())?;
            self.bind(index, Value::Scalar(Term::integer(position)), false, path)?;
        }
        if let Some(pattern) = &each.item {
            self.bind(pattern, item.clone(), false, path)?;
        }
        // An item may be seen elsewhere: through the sequence it came from.
        for name in &names {
            path.fresh.remove(name);
        }
        Ok(())
    }

    /// One pass of a loop's body: the paths that go on to the next pass,
    /// and those that leave the loop.
    fn pass(
        &mut self,
        body: &[Statement],
        path: State,
        next: &mut Vec<State>,
        left: &mut Vec<State>,
    ) -> Result<(), String> {
        for mut after in self.block(body, path)? {
            let jump = after.jump.take();
            if jump != Some(Jump::Break) && after.is_running() {
                next.push(after);
            } else {
                left.push(after);
            }
        }
        Ok(())
    }
}

/// The names a loop binds on each pass.
fn bound_names(each: &ForEach) -> BTreeSet<String> {
    let mut names = BTreeSet::new();
    for pattern in each.index.iter().chain(&each.item) {
        pattern_names(pattern, &mut names);
    }
    names
}
