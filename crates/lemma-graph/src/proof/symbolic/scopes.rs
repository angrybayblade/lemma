//! Which variable a name means: a declaration makes a new variable, and a
//! function defined in the checked code reads the variables it was defined
//! with, wherever it is called.
//!
//! A called function runs in the environment of its caller, so a name it
//! reads from around it is looked up there. That is the right variable only
//! while the name still means what it meant where the function was defined;
//! a call anywhere else is refused.

use std::collections::{BTreeMap, BTreeSet};

use super::super::ir::{
    Clause, Comprehension, Expression as Expr, Function, Language, Pattern, Statement,
    StatementKind as Stmt,
};
use super::calls::local_names;
use super::comprehensions::bound_names;
use super::patterns::pattern_names;
use super::value::Value;
use super::{Evaluator, State};

impl Evaluator<'_> {
    /// Makes `name` a new variable from here on.
    pub(super) fn declare(&mut self, name: &str, state: &mut State) {
        self.declared += 1;
        state.variables.insert(name.to_owned(), self.declared);
    }

    /// Makes what `pattern` names new variables, as a Go `:=` does where it
    /// runs.
    pub(super) fn declare_pattern(&mut self, pattern: &Pattern, state: &mut State) {
        let mut names = BTreeSet::new();
        pattern_names(pattern, &mut names);
        for name in names {
            self.declare(&name, state);
        }
    }

    /// Enters a function whose parameters are bound: each is a new variable,
    /// and so is every name the function declares for its whole body.
    /// Python makes every name a function assigns local to it, and
    /// TypeScript every `let` and `const` of the body; both are unbound
    /// until assigned, so a read before that never sees a variable of the
    /// same name from around the function. A Go `:=` declares where it runs.
    pub(super) fn enter_function(&mut self, function: &Function, state: &mut State) {
        let mut parameters: BTreeSet<&str> = function
            .parameters
            .iter()
            .map(|parameter| parameter.name.as_str())
            .collect();
        if function.has_own_this {
            parameters.insert("this");
        }
        for name in local_names(function) {
            if parameters.contains(name.as_str()) {
                self.declare(&name, state);
            } else if name != "this" && function.language != Language::Go {
                self.unbind(&name, state);
            }
        }
    }

    /// The values of a Python function's parameter defaults, evaluated here
    /// where the function is defined. TypeScript evaluates a default at each
    /// call that needs it.
    pub(super) fn defaults(
        &mut self,
        function: &Function,
        state: &mut State,
    ) -> BTreeMap<String, Result<Value, String>> {
        let mut defaults = BTreeMap::new();
        if function.language != Language::Python {
            return defaults;
        }
        for parameter in &function.parameters {
            if let Some(Ok(default)) = &parameter.default {
                defaults.insert(parameter.name.clone(), self.pure(default, state));
            }
        }
        defaults
    }

    /// Declares `name` without a value: it is unbound until assigned.
    pub(super) fn unbind(&mut self, name: &str, state: &mut State) {
        state.env.remove(name);
        state.fresh.remove(name);
        self.declare(name, state);
    }
}

/// Which variable each name `function` reads from around it is, here
/// where the function is defined.
pub(super) fn captured(function: &Function, state: &State) -> BTreeMap<String, Option<u64>> {
    free_names(function)
        .into_iter()
        .map(|name| {
            let variable = state.variables.get(&name).copied();
            (name, variable)
        })
        .collect()
}

/// Refuses a call where a name the function reads from around it means
/// another variable than where the function was defined.
pub(super) fn check_captured(
    captured: &BTreeMap<String, Option<u64>>,
    state: &State,
) -> Result<(), String> {
    for (name, variable) in captured {
        if state.variables.get(name) != variable.as_ref() {
            return Err(format!(
                "a called function reads `{name}`, which means another variable here than where the function was defined"
            ));
        }
    }
    Ok(())
}

/// The names a function reads or assigns that it does not declare itself.
pub(super) fn free_names(function: &Function) -> BTreeSet<String> {
    let mut names = BTreeSet::new();
    for parameter in &function.parameters {
        if let Some(Ok(default)) = &parameter.default {
            expression_names(default, &mut names);
        }
    }
    block_names(&function.body, &mut names);
    let mut locals = local_names(function);
    if !function.has_own_this {
        // An arrow function's `this` is the one around it.
        locals.remove("this");
    }
    names.retain(|name| !locals.contains(name));
    names
}

fn block_names(body: &[Statement], names: &mut BTreeSet<String>) {
    for statement in body {
        match &statement.kind {
            // A declaration's names are its own; what it reads may not be.
            Stmt::Bind(_, value) => expression_names(value, names),
            Stmt::Assign(pattern, value) => {
                pattern_names(pattern, names);
                expression_names(value, names);
            }
            Stmt::Update(name, _, value) | Stmt::SetField(name, _, value) => {
                names.insert(name.clone());
                expression_names(value, names);
            }
            Stmt::If(test, _, _) => expression_names(test, names),
            Stmt::Return(value) | Stmt::Evaluate(value) | Stmt::Assert(value) => {
                expression_names(value, names);
            }
            Stmt::With(contexts, _) => {
                for context in contexts {
                    expression_names(&context.value, names);
                }
            }
            // What a loop binds is its own inside it.
            Stmt::ForEach(each) => {
                expression_names(&each.iterable, names);
                let mut inside = BTreeSet::new();
                block_names(&each.body, &mut inside);
                let mut bound = BTreeSet::new();
                for pattern in each.index.iter().chain(&each.item) {
                    pattern_names(pattern, &mut bound);
                }
                names.extend(inside.difference(&bound).cloned());
                block_names(&each.otherwise, names);
                continue;
            }
            Stmt::Raise(_) | Stmt::Try(_) | Stmt::Break | Stmt::Continue => {}
        }
        for block in statement.kind.blocks() {
            block_names(block, names);
        }
    }
}

fn expression_names(expression: &Expr, names: &mut BTreeSet<String>) {
    match expression {
        Expr::Name(name) => {
            names.insert(name.clone());
        }
        Expr::Closure(function) => names.extend(free_names(function)),
        Expr::Comprehension(comprehension) => {
            comprehension_names(comprehension, names);
            return;
        }
        _ => {}
    }
    for part in expression.parts() {
        expression_names(part, names);
    }
}

/// The names a comprehension reads from around it: all its first iterable
/// reads, and elsewhere those its own `for` clauses do not bind.
fn comprehension_names(comprehension: &Comprehension, names: &mut BTreeSet<String>) {
    let mut inside = BTreeSet::new();
    for part in comprehension.parts() {
        expression_names(part, &mut inside);
    }
    let bound = bound_names(comprehension);
    names.extend(inside.difference(&bound).cloned());
    if let Some(Clause::For(_, first)) = comprehension.clauses.first() {
        expression_names(first, names);
    }
}
