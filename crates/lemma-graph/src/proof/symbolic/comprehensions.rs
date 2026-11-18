//! Python comprehensions. A comprehension runs its loops inside an
//! expression, so all of it is read on the path it is written on: the
//! collection it builds is one value, which differs between inputs where a
//! test or the length of a sequence does.
//!
//! A pass that only some inputs reach is read like a branch of a conditional
//! value, so it may not call a function. Where every pass is reached on every
//! input, as over a sequence of one known length without a test, calls run
//! in order as a loop's would.

use std::collections::{BTreeMap, BTreeSet};

use super::super::ir::{Clause, Collects, Comprehension, Expression as Expr, Pattern};
use super::loops::Iterated;
use super::patterns::pattern_names;
use super::value::{Term, Value};
use super::{Evaluator, Shadowed, State, restore};

/// What a comprehension adds, on the inputs where a condition holds. The
/// conditions of the alternatives for one comprehension exclude each other.
struct Collected {
    when: Term,
    items: Vec<Item>,
}

/// One item a pass adds, with its key in a dict.
#[derive(Clone)]
struct Item {
    key: Option<String>,
    value: Value,
}

impl Collected {
    /// Nothing added, on every input.
    fn nothing() -> Self {
        Self {
            when: Term::boolean(true),
            items: Vec::new(),
        }
    }

    /// The same items, only on the inputs where `condition` holds too.
    fn only_where(self, condition: &Term) -> Self {
        Self {
            when: condition.and(&self.when),
            items: self.items,
        }
    }

    /// These items and then those of `later`, where both conditions hold.
    fn followed_by(&self, later: &Self) -> Self {
        let mut items = self.items.clone();
        items.extend(later.items.iter().cloned());
        Self {
            when: self.when.and(&later.when),
            items,
        }
    }
}

/// The passes of one `for` clause: what each binds, the clauses that follow
/// it, and where the passes are reached.
#[derive(Clone, Copy)]
struct Passes<'a> {
    comprehension: &'a Comprehension,
    pattern: &'a Pattern,
    rest: &'a [Clause],
    reached: &'a Term,
}

impl Evaluator<'_> {
    pub(super) fn comprehension(
        &mut self,
        comprehension: &Comprehension,
        state: &mut State,
    ) -> Result<Value, String> {
        let Some((Clause::For(pattern, iterable), rest)) = comprehension.clauses.split_first()
        else {
            return Err("a comprehension starts with a `for` clause".to_owned());
        };
        // Python evaluates the first iterable where the comprehension is
        // written. Everything else runs in a scope of the comprehension's
        // own, whose names are unbound until a pass binds them.
        let first = self.expression(iterable, state)?;
        let is_lazy = matches!(comprehension.collects, Collects::Generator);
        if is_lazy && comprehension.parts().into_iter().any(Expr::has_call) {
            return Err(
                "a generator expression that calls a function is unsupported: the calls happen when its items are read, which may be later than here, or never"
                    .to_owned(),
            );
        }
        let shadowed: Vec<Shadowed> = bound_names(comprehension)
            .iter()
            .map(|name| Shadowed::of(name, state))
            .collect();
        for name in &shadowed {
            self.unbind(&name.name, state);
        }
        let guard_before = state.guard.code.clone();
        let raising_before = state.pending.len();
        let everywhere = Term::boolean(true);
        let passes = Passes {
            comprehension,
            pattern,
            rest,
            reached: &everywhere,
        };
        let collected = self.for_clause(&passes, &first, state)?;
        // The comprehension's names end with it, also on the paths that
        // raised inside it and go on in a handler.
        for name in &shadowed {
            restore(state, name);
            for raising in &mut state.pending[raising_before..] {
                restore(raising, name);
            }
        }
        let raises = state.guard.code != guard_before || state.pending.len() != raising_before;
        if is_lazy && raises {
            return Err(
                "a generator expression is read only where making its items cannot raise: they are made when something reads them, which may be later than here, or never"
                    .to_owned(),
            );
        }
        Ok(collect(&comprehension.collects, collected))
    }

    /// What a `for` clause and the clauses after it add where the clause
    /// is reached: the passes over each sequence the iterable may be.
    fn for_clause(
        &mut self,
        clause: &Passes<'_>,
        iterable: &Value,
        state: &mut State,
    ) -> Result<Vec<Collected>, String> {
        let mut sequences = Vec::new();
        self.iterated(iterable, &Term::boolean(true), &mut sequences)
            .map_err(|why| format!("a comprehension {why}"))?;
        let mut collected = Vec::new();
        for Iterated { when, items } in sequences {
            let reached = clause.reached.and(&when);
            if reached.code == "false" {
                continue;
            }
            let Some(items) = items else {
                // Looping over `None` raises, where the clause is reached.
                self.narrow(&reached, state);
                continue;
            };
            let passes = Passes {
                reached: &reached,
                ..*clause
            };
            let added = self.passes_over(&passes, items, state)?;
            collected.extend(
                added
                    .into_iter()
                    .map(|alternative| alternative.only_where(&when)),
            );
        }
        Ok(collected)
    }

    /// What the passes over the items of one sequence add, in order.
    fn passes_over(
        &mut self,
        passes: &Passes<'_>,
        items: Vec<Value>,
        state: &mut State,
    ) -> Result<Vec<Collected>, String> {
        let mut added = vec![Collected::nothing()];
        for item in items {
            self.within(passes.reached, |evaluator| {
                evaluator.bind(passes.pattern, item, false, state)
            })?;
            let more = self.clauses(passes.comprehension, passes.rest, passes.reached, state)?;
            added = concatenated(&added, &more, self.contract.max_paths)?;
        }
        Ok(added)
    }

    /// What the clauses of one pass add where `reached` holds, ending with
    /// the item itself.
    fn clauses(
        &mut self,
        comprehension: &Comprehension,
        clauses: &[Clause],
        reached: &Term,
        state: &mut State,
    ) -> Result<Vec<Collected>, String> {
        let Some((clause, rest)) = clauses.split_first() else {
            return Ok(vec![Collected {
                when: Term::boolean(true),
                items: vec![self.item(comprehension, reached, state)?],
            }]);
        };
        let test = match clause {
            Clause::For(pattern, iterable) => {
                let iterable = self.pass_expression(iterable, reached, state)?;
                let passes = Passes {
                    comprehension,
                    pattern,
                    rest,
                    reached,
                };
                return self.for_clause(&passes, &iterable, state);
            }
            Clause::If(test) => self.pass_expression(test, reached, state)?.as_bool()?,
        };
        match test.code.as_str() {
            "true" => return self.clauses(comprehension, rest, reached, state),
            "false" => return Ok(vec![Collected::nothing()]),
            _ => {}
        }
        // The clauses after the test add their items where it holds, and
        // nothing where it does not.
        let passing = reached.and(&test);
        let mut collected: Vec<Collected> = self
            .clauses(comprehension, rest, &passing, state)?
            .into_iter()
            .map(|alternative| alternative.only_where(&test))
            .collect();
        collected.push(Collected::nothing().only_where(&test.not()));
        Ok(collected)
    }

    /// The item a pass adds: a dict's key is evaluated before its value.
    fn item(
        &mut self,
        comprehension: &Comprehension,
        reached: &Term,
        state: &mut State,
    ) -> Result<Item, String> {
        let key = match &comprehension.collects {
            Collects::Record(key) => {
                let key = self.pass_expression(key, reached, state)?;
                Some(constant_key(&key)?)
            }
            _ => None,
        };
        let value = self.pass_expression(&comprehension.element, reached, state)?;
        Ok(Item { key, value })
    }

    /// An expression of a pass. A pass only some inputs reach is evaluated
    /// as reached under that condition, which a call cannot be.
    fn pass_expression(
        &mut self,
        expression: &Expr,
        reached: &Term,
        state: &mut State,
    ) -> Result<Value, String> {
        if reached.code == "true" {
            return self.expression(expression, state);
        }
        if expression.has_call() {
            return Err(
                "a comprehension calls a function on a pass that only some inputs reach: after a test, or over a sequence that may have several lengths"
                    .to_owned(),
            );
        }
        self.guarded(reached, expression, state)
    }

    /// Runs `evaluate` as reached only where `reached` holds.
    fn within<T>(&mut self, reached: &Term, evaluate: impl FnOnce(&mut Self) -> T) -> T {
        let outer = self.context.clone();
        self.context = outer.and(reached);
        let outcome = evaluate(self);
        self.context = outer;
        outcome
    }
}

/// Every way of following one of `before` by one of `after`, of which a
/// comprehension may build at most `limit`.
fn concatenated(
    before: &[Collected],
    after: &[Collected],
    limit: usize,
) -> Result<Vec<Collected>, String> {
    let joined: Vec<Collected> = before
        .iter()
        .flat_map(|earlier| after.iter().map(|later| earlier.followed_by(later)))
        .filter(|alternative| alternative.when.code != "false")
        .collect();
    if joined.len() > limit {
        return Err(format!(
            "a comprehension builds more different collections than max_paths ({limit})"
        ));
    }
    Ok(joined)
}

/// The names the `for` clauses of a comprehension bind.
pub(super) fn bound_names(comprehension: &Comprehension) -> BTreeSet<String> {
    let mut names = BTreeSet::new();
    for clause in &comprehension.clauses {
        if let Clause::For(pattern, _) = clause {
            pattern_names(pattern, &mut names);
        }
    }
    names
}

/// The collection a comprehension builds: each alternative's where its
/// condition holds.
fn collect(collects: &Collects, collected: Vec<Collected>) -> Value {
    let mut alternatives = collected.into_iter().rev();
    // Where no alternative is left, every input raised before building one.
    let Some(last) = alternatives.next() else {
        return Value::Unreachable;
    };
    let mut value = built(collects, last.items);
    for alternative in alternatives {
        value = Value::choice(&alternative.when, built(collects, alternative.items), value);
    }
    value
}

fn built(collects: &Collects, items: Vec<Item>) -> Value {
    match collects {
        Collects::Sequence | Collects::Generator => {
            Value::Sequence(items.into_iter().map(|item| item.value).collect())
        }
        Collects::Set => Value::Set(items.into_iter().map(|item| item.value).collect()),
        // A later entry replaces an earlier one of the same key.
        Collects::Record(_) => Value::Object(
            items
                .into_iter()
                .filter_map(|item| Some((item.key?, item.value)))
                .collect::<BTreeMap<_, _>>(),
        ),
    }
}

/// A dict's key, which an object here names only as a constant text.
fn constant_key(key: &Value) -> Result<String, String> {
    let text = match key {
        Value::Scalar(term) => term.constant_text(),
        _ => None,
    };
    text.ok_or_else(|| {
        "a dict comprehension is read where each key is a constant text; a key that depends on an input names a field no contract declares"
            .to_owned()
    })
}
