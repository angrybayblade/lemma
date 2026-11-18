//! Calls: under a call model, or by running a function the checked code
//! defines.

use std::collections::{BTreeMap, BTreeSet};

use serde_json::Value as Json;

use super::super::contract::CallModel;
use super::super::ir::{
    self, Expression as Expr, Function, Language, Statement, StatementKind as Stmt,
};
use super::models::{check_awaiting, match_arguments, reads_inputs};
use super::patterns::pattern_names;
use super::scopes::check_captured;
use super::value::{Defined, Term, Value};
use super::{Evaluator, State, input, ranges, specification_names};

impl Evaluator<'_> {
    pub(super) fn call(
        &mut self,
        callee: &Expr,
        arguments: &[ir::Argument],
        is_awaited: bool,
        state: &mut State,
    ) -> Result<Value, String> {
        let name = match self.expression(callee, state)? {
            Value::Model(name) => name,
            Value::Closure(defined) => {
                check_captured(&defined.captured, state)?;
                return self.call_closure(&defined, arguments, is_awaited, state);
            }
            _ => return Err("call target has no call model".to_owned()),
        };
        self.call_model(&name, arguments, is_awaited, state)
    }

    /// A call answered by the call model `name`.
    pub(super) fn call_model(
        &mut self,
        name: &str,
        arguments: &[ir::Argument],
        is_awaited: bool,
        state: &mut State,
    ) -> Result<Value, String> {
        let mut values = Vec::new();
        for argument in arguments {
            values.push((
                argument.name.as_deref(),
                self.expression(&argument.value, state)?,
            ));
        }
        self.calls += 1;
        if self.calls > 16384 {
            return Err("call limit exceeded".to_owned());
        }
        let model = self
            .contract
            .models
            .get(name)
            .ok_or_else(|| format!("missing model `{name}`"))?
            .clone();
        check_awaiting(name, model.is_async, is_awaited)?;
        check_callbacks(name, &model, &values)?;
        let arguments = match_arguments(name, &model.parameters, values)?;
        self.apply_model(name, model, arguments, state)
    }

    /// What a call model says a call does: its effect, and what it returns.
    pub(super) fn apply_model(
        &mut self,
        name: &str,
        model: CallModel,
        arguments: BTreeMap<String, Value>,
        state: &mut State,
    ) -> Result<Value, String> {
        let count = state.calls.entry(name.to_owned()).or_default();
        *count += 1;
        if *count > 1 && reads_inputs(&model, self.inputs)? {
            // A specification input names one value. Two calls reading it
            // would be assumed to answer alike, which the model never said.
            return Err(format!(
                "model `{name}` is called more than once on a path, and its templates read specification inputs; declare `outcomes` for what may differ between calls"
            ));
        }
        let call_number = *count;
        self.assumptions.insert(model.assumption.clone());
        let mut local = state.clone();
        local.env = specification_names(self.inputs);
        local.env.extend(arguments);
        if !model.outcomes.is_empty() {
            let mut outcome = BTreeMap::new();
            for (field, shape) in &model.outcomes {
                let path = format!("calls.{name}.{call_number}.{field}");
                outcome.insert(field.clone(), input(shape, &path, &mut self.symbols)?);
            }
            let outcome = Value::Record(outcome);
            state.guard = state.guard.and(&ranges(&outcome, self.language));
            local.env.insert("outcome".to_owned(), outcome);
        }
        if let Some(effect) = model.effect {
            let fields = effect
                .fields
                .iter()
                .map(|(key, template)| Ok((key.clone(), self.template(template, &mut local)?)))
                .collect::<Result<_, String>>()?;
            state
                .effects
                .entry(effect.name)
                .or_default()
                .push(Value::Record(fields));
        }
        self.template(&model.returns, &mut local)
    }

    /// Calls a function defined in the checked code by running its body
    /// here: what it reads from around it is what it holds now, as late
    /// binding reads it. The caller goes on from the callee's path; several
    /// paths are joined into one value only where they differ in nothing
    /// else, so a branching callee with effects is refused here.
    pub(super) fn call_closure(
        &mut self,
        defined: &Defined,
        arguments: &[ir::Argument],
        is_awaited: bool,
        state: &mut State,
    ) -> Result<Value, String> {
        let function = &defined.function;
        check_awaiting("the function", function.is_async, is_awaited)?;
        if self.inlined >= 8 {
            return Err("functions call each other too deeply to follow".to_owned());
        }
        let mut callee = state.clone();
        callee.result = None;
        callee.pending.clear();
        self.bind_arguments(function, arguments, state, &mut callee)?;
        self.enter_function(function, &mut callee);
        let locals = local_names(function);
        let outer_async = std::mem::replace(&mut self.is_async, function.is_async);
        self.inlined += 1;
        let paths = self.statements(&function.body, vec![callee]);
        self.inlined -= 1;
        self.is_async = outer_async;
        let mut returned = Vec::new();
        for mut path in paths? {
            if path.raised.is_some() {
                return Err(
                    "a raise inside a called function, within a `try`, is unsupported".to_owned(),
                );
            }
            if path.result.is_none() {
                // Falling off the end returns `None` in Python and
                // `undefined` in TypeScript; Go functions with results cannot.
                path.result = Some(match function.language {
                    Language::Python => Value::Null,
                    Language::TypeScript | Language::Go => Value::Undefined,
                });
            }
            returned.push(path);
        }
        join_paths(returned, &locals, state)
    }

    /// Gives a closure's parameters the arguments of a call, in order then
    /// by name, and defaults for the rest.
    pub(super) fn bind_arguments(
        &mut self,
        function: &Function,
        arguments: &[ir::Argument],
        state: &mut State,
        callee: &mut State,
    ) -> Result<(), String> {
        let mut values = Vec::new();
        for argument in arguments {
            let value = self.expression(&argument.value, state)?;
            values.push((argument.name.clone(), value));
        }
        // What the arguments evaluated may have narrowed or effected.
        *callee = state.clone();
        callee.result = None;
        callee.pending.clear();
        let mut bound: BTreeMap<String, Value> = BTreeMap::new();
        let mut position = 0;
        for (name, value) in values {
            let parameter = if let Some(name) = name {
                function
                    .parameters
                    .iter()
                    .find(|parameter| parameter.name == name)
                    .ok_or_else(|| format!("the function has no parameter `{name}`"))?
            } else {
                position += 1;
                function
                    .parameters
                    .get(position - 1)
                    .ok_or("the function takes fewer arguments")?
            };
            bound.insert(parameter.name.clone(), value);
        }
        for parameter in &function.parameters {
            let value = match (bound.remove(&parameter.name), &parameter.default) {
                (Some(value), _) => value,
                (None, Some(Ok(default))) => self.pure(default, callee)?,
                (None, _) => return Err(format!("argument `{}` is missing", parameter.name)),
            };
            callee.env.insert(parameter.name.clone(), value);
            callee.fresh.remove(&parameter.name);
        }
        if function.has_own_this {
            callee.env.insert("this".to_owned(), Value::Undefined);
        }
        Ok(())
    }

    pub(super) fn template(&mut self, template: &Json, state: &mut State) -> Result<Value, String> {
        Ok(match template {
            Json::Bool(value) => Value::boolean(*value),
            Json::String(value) => Value::Scalar(Term::text(value)),
            Json::Number(value) => {
                let integer = value
                    .as_i64()
                    .filter(|n| n.unsigned_abs() <= 9_007_199_254_740_991)
                    .ok_or("only exactly representable integer constants are supported")?;
                Value::Scalar(Term::integer(integer))
            }
            Json::Null => Value::Null,
            Json::Object(fields) if fields.len() == 1 && fields.contains_key("expr") => {
                let text = fields["expr"]
                    .as_str()
                    .ok_or("expr template must contain text")?;
                let (value, undefined) = self.specification(&ir::condition(text)?, state)?;
                // A template is an assumption: it may not quietly narrow the
                // paths where it reads null.
                if undefined.code != "false" {
                    return Err(format!(
                        "template `{text}` reads a field of a value that may be null or undefined"
                    ));
                }
                value
            }
            Json::Object(fields) => Value::Record(
                fields
                    .iter()
                    .map(|(key, value)| Ok((key.clone(), self.template(value, state)?)))
                    .collect::<Result<_, String>>()?,
            ),
            Json::Array(items) => Value::Sequence(
                items
                    .iter()
                    .map(|item| self.template(item, state))
                    .collect::<Result<_, _>>()?,
            ),
        })
    }
}

/// Continues the caller from the paths a called function returned on.
pub(super) fn join_paths(
    mut returned: Vec<State>,
    locals: &BTreeSet<String>,
    state: &mut State,
) -> Result<Value, String> {
    let caller_env = state.env.clone();
    let outer = |path: &State| -> BTreeMap<String, Value> {
        caller_env
            .keys()
            .filter(|name| !locals.contains(*name))
            .filter_map(|name| {
                path.env
                    .get(name)
                    .map(|value| (name.clone(), value.clone()))
            })
            .collect()
    };
    let Some(first) = returned.first() else {
        state.guard = Term::boolean(false);
        return Ok(Value::Unreachable);
    };
    let shared = format!("{:?}", (&first.effects, &first.calls, outer(first)));
    if returned
        .iter()
        .any(|path| format!("{:?}", (&path.effects, &path.calls, outer(path))) != shared)
    {
        return Err("a called function with several paths and different effects is unsupported inside an expression".to_owned());
    }
    // Paths are exclusive: the value is each path's where its guard holds.
    let mut value = Value::Unreachable;
    let mut guard = Term::boolean(false);
    for path in returned.iter().rev() {
        let result = path.result.clone().unwrap_or(Value::Undefined);
        value = Value::choice(&path.guard, result, value);
        guard = guard.or(&path.guard);
    }
    let first = returned.remove(0);
    let assigned = outer(&first);
    state.guard = guard;
    state.effects = first.effects;
    state.calls = first.calls;
    state.branches = first.branches;
    state.env.extend(assigned);
    Ok(value)
}

/// Refuses passing a function to a call model that may call it: what the
/// function would do there is outside the model.
pub(super) fn check_callbacks(
    name: &str,
    model: &CallModel,
    values: &[(Option<&str>, Value)],
) -> Result<(), String> {
    if model.ignores_callbacks || !values.iter().any(|(_, value)| value.holds_closure()) {
        return Ok(());
    }
    Err(format!(
        "`{name}` is passed a function it may call; declare `ignores_callbacks` if it never does"
    ))
}

/// The names a function binds itself: its parameters, and in Python every
/// name it assigns, which Python makes local to it.
pub(super) fn local_names(function: &Function) -> BTreeSet<String> {
    let mut names: BTreeSet<String> = function.parameters.iter().map(|p| p.name.clone()).collect();
    names.insert("this".to_owned());
    collect_bound(&function.body, function.language, &mut names);
    names
}

pub(super) fn collect_bound(body: &[Statement], language: Language, names: &mut BTreeSet<String>) {
    for statement in body {
        match &statement.kind {
            Stmt::Bind(pattern, _) => pattern_names(pattern, names),
            Stmt::Assign(pattern, _) if language == Language::Python => {
                pattern_names(pattern, names);
            }
            Stmt::Update(name, _, _) if language == Language::Python => {
                names.insert(name.clone());
            }
            Stmt::With(contexts, body) => {
                for target in contexts
                    .iter()
                    .filter_map(|context| context.target.as_ref())
                {
                    pattern_names(target, names);
                }
                collect_bound(body, language, names);
            }
            // A TypeScript or Go declaration in a nested block ends with it.
            kind if language == Language::Python => {
                for block in kind.blocks() {
                    collect_bound(block, language, names);
                }
            }
            _ => {}
        }
    }
}
