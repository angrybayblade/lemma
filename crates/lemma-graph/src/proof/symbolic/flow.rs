//! Control flow beyond branches: raises, `try` and `with`.

use super::super::ir::{self, Raised, Statement, Try};
use super::patterns::track_freshness;
use super::value::Term;
use super::{Evaluator, State, exceptions};

impl Evaluator<'_> {
    /// Narrows the path to where a read did not raise: only where the read
    /// is reached, and in a contract, records it as undefined instead.
    pub(super) fn narrow(&mut self, fails: &Term, state: &mut State) {
        let undefined = self.context.and(fails);
        if undefined.code == "false" {
            return;
        }
        if let Some(accumulated) = &mut self.specification {
            *accumulated = accumulated.or(&undefined);
            return;
        }
        self.raise_where(&undefined, Raised::Implicit, state);
    }

    /// The path raises where `condition` holds and goes on where it does not.
    /// Inside a `try` the raising path is kept, as it was here, for a handler.
    pub(super) fn raise_where(&mut self, condition: &Term, raised: Raised, state: &mut State) {
        if condition.code == "false" {
            return;
        }
        if self.try_depth > 0 {
            let mut raising = state.clone();
            raising.pending.clear();
            raising.guard = state.guard.and(condition);
            raising.raised = Some(raised);
            if raising.guard.code != "false" {
                state.pending.push(raising);
            }
        }
        state.guard = state.guard.and(&condition.not());
    }

    /// `with`: each context manager is entered, under its context model, and
    /// what `as` names is bound to it; then the body runs.
    pub(super) fn with_statement(
        &mut self,
        contexts: &[ir::Context],
        body: &[Statement],
        mut state: State,
    ) -> Result<Vec<State>, String> {
        for context in contexts {
            self.enter(context, &mut state)?;
        }
        let mut paths = std::mem::take(&mut state.pending);
        paths.extend(self.statements(body, vec![state])?);
        Ok(paths)
    }

    pub(super) fn enter(&mut self, context: &ir::Context, state: &mut State) -> Result<(), String> {
        if context.is_async && !self.is_async {
            return Err("`async with` outside an async function".to_owned());
        }
        let callee = context
            .text
            .split('(')
            .next()
            .unwrap_or(&context.text)
            .trim();
        let assumption = self
            .contract
            .contexts
            .get(&context.text)
            .or_else(|| self.contract.contexts.get(callee))
            .ok_or_else(|| format!("context manager `{}` needs a context model", context.text))?;
        self.assumptions.insert(assumption.clone());
        // A manager nothing binds and nothing calls to reach matters only by
        // entering and leaving, which its context model stands for.
        let Some(target) = &context.target else {
            if context.value.has_call() {
                self.expression(&context.value, state)?;
            }
            return Ok(());
        };
        let value = self.expression(&context.value, state)?;
        self.bind(target, value, false, state)?;
        track_freshness(target, &context.value, &mut state.fresh);
        Ok(())
    }

    /// `try`: the body, then a handler for what it raised, `else` if it
    /// raised nothing, and `finally` on every way out.
    pub(super) fn try_statement(
        &mut self,
        statement: &Try,
        state: State,
    ) -> Result<Vec<State>, String> {
        self.try_depth += 1;
        let body = self.statements(&statement.body, vec![state]);
        self.try_depth -= 1;
        let mut after = Vec::new();
        for path in body? {
            match (&path.raised, &path.result) {
                (Some(raised), _) => {
                    let raised = raised.clone();
                    after.extend(self.handle(statement, &raised, path)?);
                }
                (None, None) => after.extend(self.statements(&statement.otherwise, vec![path])?),
                (None, Some(_)) => after.push(path),
            }
        }
        if statement.finally.is_empty() {
            return Ok(after);
        }
        let mut finished = Vec::new();
        for path in after {
            finished.extend(self.finally(&statement.finally, path)?);
        }
        Ok(finished)
    }

    /// Runs the first handler that catches `raised`; the path raises on if
    /// none does.
    pub(super) fn handle(
        &mut self,
        statement: &Try,
        raised: &Raised,
        mut path: State,
    ) -> Result<Vec<State>, String> {
        for handler in &statement.handlers {
            match exceptions::catches(handler.classes.as_deref(), raised)? {
                exceptions::Catch::No => continue,
                exceptions::Catch::Yes => {}
                exceptions::Catch::Assuming(assumption) => {
                    self.assumptions.insert(assumption);
                }
            }
            path.raised = None;
            self.handling.push(raised.clone());
            let handled = self.statements(&handler.body, vec![path]);
            self.handling.pop();
            return handled;
        }
        Ok(vec![path])
    }

    /// `finally`: runs on every way out of a `try`, and the way out resumes
    /// unless `finally` returns or raises itself.
    pub(super) fn finally(
        &mut self,
        body: &[Statement],
        mut path: State,
    ) -> Result<Vec<State>, String> {
        let result = path.result.take();
        let raised = path.raised.take();
        let mut finished = self.statements(body, vec![path])?;
        for path in &mut finished {
            if path.result.is_none() && path.raised.is_none() {
                path.result.clone_from(&result);
                path.raised.clone_from(&raised);
            }
        }
        Ok(finished)
    }
}
