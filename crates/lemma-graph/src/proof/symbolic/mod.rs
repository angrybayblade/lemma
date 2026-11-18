//! Symbolic execution of the shared, immutable-value subset.
//!
//! Every path through the function is followed with its inputs as solver
//! terms. A path that raises, as a field read of null does in every
//! language, has no normal return and is dropped: the contract speaks of
//! normal returns only.

mod models;
mod value;

use std::collections::{BTreeMap, BTreeSet};

use lemma_lang::syntax::ast::BinaryOperator as Op;
use serde_json::Value as Json;

use super::contract::{Contract, Input};
use super::ir::{self, Branch, Expression as Expr, Function, Statement, StatementKind as Stmt};
use models::{assume_decorators, check_awaiting, match_arguments, reads_inputs};
pub(super) use value::{Sort, Term, Value};
use value::{equal, order, read_field};

#[derive(Debug, Clone)]
pub(super) struct State {
    env: BTreeMap<String, Value>,
    pub guard: Term,
    pub branches: Vec<Branch>,
    effects: BTreeMap<String, Vec<Value>>,
    /// How often each model was called on this path.
    calls: BTreeMap<String, usize>,
    pub result: Option<Value>,
}

impl State {
    /// Whether the path can still reach a statement: it has not returned,
    /// and no read of null has ended it.
    fn is_running(&self) -> bool {
        self.result.is_none() && self.guard.code != "false"
    }
}

pub(super) struct Execution {
    pub symbols: BTreeMap<String, (String, Sort)>,
    pub requires: Term,
    /// Where a precondition reads a field of null or undefined: a contract
    /// whose preconditions are not defined for every input is refused.
    pub requires_undefined: Term,
    pub paths: Vec<State>,
    pub assumptions: BTreeSet<String>,
    inputs: BTreeMap<String, Value>,
}

pub(super) fn execute(function: &Function, contract: &Contract) -> Result<Execution, String> {
    let mut symbols = BTreeMap::new();
    let mut inputs = BTreeMap::new();
    for (name, shape) in &contract.inputs {
        inputs.insert(name.clone(), input(shape, name, &mut symbols)?);
    }
    let mut evaluator = Evaluator {
        contract,
        inputs: &inputs,
        is_async: function.is_async,
        context: Term::boolean(true),
        specification: None,
        symbols,
        assumptions: BTreeSet::new(),
        calls: 0,
    };
    assume_decorators(&function.decorators, contract, &mut evaluator.assumptions)?;
    let (mut state, requires, requires_undefined) = prepare(&inputs, &mut evaluator)?;
    bind_parameters(function, &inputs, &mut state, &mut evaluator)?;
    state.guard = requires.clone();
    let paths = evaluator.statements(&function.body, vec![state])?;
    if paths.iter().any(|path| path.result.is_none()) {
        return Err("a source path falls through without an explicit return".to_owned());
    }
    Ok(Execution {
        symbols: evaluator.symbols,
        requires,
        requires_undefined,
        paths,
        assumptions: evaluator.assumptions,
        inputs,
    })
}

/// Names every contract expression can read besides the inputs.
fn specification_names(inputs: &BTreeMap<String, Value>) -> BTreeMap<String, Value> {
    let mut names = inputs.clone();
    names.insert("null".to_owned(), Value::Null);
    names.insert("undefined".to_owned(), Value::Undefined);
    names
}

fn prepare(
    inputs: &BTreeMap<String, Value>,
    evaluator: &mut Evaluator<'_>,
) -> Result<(State, Term, Term), String> {
    let mut state = State {
        env: specification_names(inputs),
        guard: Term::boolean(true),
        branches: Vec::new(),
        effects: BTreeMap::new(),
        calls: BTreeMap::new(),
        result: None,
    };
    for (name, template) in &evaluator.contract.bindings {
        if state.env.contains_key(name) {
            return Err(format!("binding `{name}` shadows an input"));
        }
        let value = evaluator.template(template, &mut state)?;
        state.env.insert(name.clone(), value);
        evaluator
            .assumptions
            .insert(format!("constant `{name}` has the declared binding"));
    }
    let mut requires = Term::boolean(true);
    let mut requires_undefined = Term::boolean(false);
    for expression in &evaluator.contract.requires {
        let (condition, undefined) =
            evaluator.specification(&ir::condition(expression)?, &mut state)?;
        requires = requires.and(&condition.as_bool()?);
        requires_undefined = requires_undefined.or(&undefined);
    }
    for name in evaluator.contract.models.keys() {
        insert_model(&mut state.env, name, name)?;
    }
    state.env.remove("null");
    state.env.remove("undefined");
    Ok((state, requires, requires_undefined))
}

fn bind_parameters(
    function: &Function,
    inputs: &BTreeMap<String, Value>,
    state: &mut State,
    evaluator: &mut Evaluator<'_>,
) -> Result<(), String> {
    let contract = evaluator.contract;
    let given: Vec<Option<&String>> = if contract.parameters.is_empty() {
        function.parameters.iter().map(|p| Some(&p.name)).collect()
    } else {
        contract.parameters.iter().map(Option::as_ref).collect()
    };
    if given.len() != function.parameters.len() {
        return Err("contract parameter count differs from the implementation".to_owned());
    }
    // Source locals are separate from specification inputs; reassigning a local
    // must never change what a postcondition means by an input name.
    state.env.retain(|name, _| !inputs.contains_key(name));
    let mut values = Vec::new();
    for (parameter, input) in function.parameters.iter().zip(given) {
        let value = match (input, &parameter.default) {
            (Some(name), _) => inputs
                .get(name)
                .cloned()
                .ok_or_else(|| format!("parameter `{name}` is not a declared input"))?,
            (None, Some(Ok(default))) => evaluator.pure(default, state)?,
            (None, Some(Err(why))) => {
                return Err(format!(
                    "parameter `{}` is left out, and its default is unsupported: {why}",
                    parameter.name
                ));
            }
            (None, None) => {
                return Err(format!(
                    "parameter `{}` has no default, so the contract must give it an input",
                    parameter.name
                ));
            }
        };
        values.push(value);
    }
    for (parameter, value) in function.parameters.iter().zip(values) {
        if state.env.contains_key(&parameter.name) {
            return Err(format!(
                "parameter `{}` shadows a called function",
                parameter.name
            ));
        }
        state.env.insert(parameter.name.clone(), value);
    }
    Ok(())
}

impl Execution {
    /// A postcondition on one path: it holds, and every field it reads on
    /// the way is defined, so a read of null can never satisfy it.
    pub(super) fn postcondition(
        &self,
        path: &State,
        expression: &str,
        contract: &Contract,
    ) -> Result<Term, String> {
        let mut state = path.clone();
        state.env = specification_names(&self.inputs);
        state.env.insert(
            "result".to_owned(),
            path.result.clone().ok_or("return value is missing")?,
        );
        let mut effects = BTreeMap::new();
        for effect in contract
            .models
            .values()
            .filter_map(|model| model.effect.as_ref())
        {
            let events = path
                .effects
                .get(&effect.name)
                .map_or(&[][..], Vec::as_slice);
            let count = i64::try_from(events.len()).map_err(|e| e.to_string())?;
            effects.insert(
                effect.name.clone(),
                Value::Record(BTreeMap::from([
                    ("called".to_owned(), Value::boolean(!events.is_empty())),
                    ("count".to_owned(), Value::Scalar(Term::integer(count))),
                    (
                        "last".to_owned(),
                        events.last().cloned().unwrap_or(Value::Undefined),
                    ),
                ])),
            );
        }
        state
            .env
            .insert("effects".to_owned(), Value::Record(effects));
        let (condition, undefined) = Evaluator {
            contract,
            inputs: &self.inputs,
            is_async: false,
            context: Term::boolean(true),
            specification: None,
            symbols: BTreeMap::new(),
            assumptions: BTreeSet::new(),
            calls: 0,
        }
        .specification(&ir::condition(expression)?, &mut state)?;
        Ok(condition.as_bool()?.and(&undefined.not()))
    }
}

fn input(
    shape: &Input,
    path: &str,
    symbols: &mut BTreeMap<String, (String, Sort)>,
) -> Result<Value, String> {
    match shape {
        Input::Record(fields) => Ok(Value::Record(
            fields
                .iter()
                .map(|(name, shape)| {
                    Ok((
                        name.clone(),
                        input(shape, &format!("{path}.{name}"), symbols)?,
                    ))
                })
                .collect::<Result<_, String>>()?,
        )),
        Input::Union(alternatives) => {
            let Some((last, rest)) = alternatives.split_last() else {
                return Err(format!("input `{path}` is an empty union"));
            };
            let mut value = input(last, &format!("{path}|{}", rest.len()), symbols)?;
            for (index, alternative) in rest.iter().enumerate().rev() {
                let selector = symbol(&format!("{path}?{index}"), Sort::Bool, symbols)?;
                let alternative = input(alternative, &format!("{path}|{index}"), symbols)?;
                value = Value::choice(&selector, alternative, value);
            }
            Ok(value)
        }
        Input::Scalar(name) => {
            let sort = match name.as_str() {
                "Bool" => Sort::Bool,
                "Text" => Sort::Text,
                "Null" => return Ok(Value::Null),
                "Undefined" => return Ok(Value::Undefined),
                _ => {
                    return Err(format!(
                        "input `{path}` has unsupported sort `{name}`; use Bool, Text, Null or Undefined"
                    ));
                }
            };
            Ok(Value::Scalar(symbol(path, sort, symbols)?))
        }
    }
}

/// A fresh solver constant for the input at `path`.
fn symbol(
    path: &str,
    sort: Sort,
    symbols: &mut BTreeMap<String, (String, Sort)>,
) -> Result<Term, String> {
    let code = format!("input_{}", symbols.len());
    if symbols
        .insert(path.to_owned(), (code.clone(), sort))
        .is_some()
    {
        return Err("ambiguous symbolic input path".to_owned());
    }
    Ok(Term { sort, code })
}

fn insert_model(env: &mut BTreeMap<String, Value>, path: &str, name: &str) -> Result<(), String> {
    if let Some((head, tail)) = path.split_once('.') {
        let node = env
            .entry(head.to_owned())
            .or_insert_with(|| Value::Record(BTreeMap::new()));
        let Value::Record(fields) = node else {
            return Err(format!("model `{name}` shadows another value"));
        };
        insert_model(fields, tail, name)
    } else if env
        .insert(path.to_owned(), Value::Model(name.to_owned()))
        .is_some()
    {
        Err(format!("model `{name}` shadows another value"))
    } else {
        Ok(())
    }
}

struct Evaluator<'a> {
    contract: &'a Contract,
    inputs: &'a BTreeMap<String, Value>,
    /// Whether the function under check is `async`, where `await` is allowed.
    is_async: bool,
    /// What must hold for the expression being evaluated to be reached at
    /// all: the left of `&&` for its right, a ternary's test for a branch.
    context: Term,
    /// While a contract expression is evaluated: where it reads a field of
    /// null or undefined, rather than paths that raise.
    specification: Option<Term>,
    /// Every symbolic value: the contract's inputs, then each call's outcomes.
    symbols: BTreeMap<String, (String, Sort)>,
    assumptions: BTreeSet<String>,
    calls: usize,
}

impl Evaluator<'_> {
    fn statements(
        &mut self,
        statements: &[Statement],
        mut paths: Vec<State>,
    ) -> Result<Vec<State>, String> {
        for statement in statements {
            paths = self.steps(statement, paths)?;
        }
        Ok(paths)
    }

    fn steps(&mut self, statement: &Statement, paths: Vec<State>) -> Result<Vec<State>, String> {
        let mut next = Vec::new();
        for state in paths {
            // A path that raised has no normal return to check.
            next.extend(
                self.step(statement, state)?
                    .into_iter()
                    .filter(|state| state.result.is_some() || state.guard.code != "false"),
            );
            if next.len() > self.contract.max_paths {
                return Err(format!(
                    "source exceeds max_paths ({})",
                    self.contract.max_paths
                ));
            }
        }
        Ok(next)
    }

    fn step(&mut self, statement: &Statement, mut state: State) -> Result<Vec<State>, String> {
        if !state.is_running() {
            return Ok(vec![state]);
        }
        match &statement.kind {
            Stmt::Bind(name, expression) => {
                if state.env.contains_key(name) {
                    return Err(format!("rebinding or shadowing `{name}` is unsupported"));
                }
                let value = self.expression(expression, &mut state)?;
                state.env.insert(name.clone(), value);
            }
            Stmt::Return(expression) => {
                let value = self.expression(expression, &mut state)?;
                if state.is_running() {
                    state.result = Some(value);
                }
            }
            Stmt::Evaluate(expression) => {
                self.expression(expression, &mut state)?;
            }
            Stmt::If(expression, then, otherwise) => {
                return self.branch(statement.line, expression, [then, otherwise], state);
            }
        }
        Ok(vec![state])
    }

    fn branch(
        &mut self,
        line: usize,
        expression: &Expr,
        bodies: [&[Statement]; 2],
        mut state: State,
    ) -> Result<Vec<State>, String> {
        let test = self.expression(expression, &mut state)?.as_bool()?;
        let mut paths = Vec::new();
        for (taken, body) in [(true, bodies[0]), (false, bodies[1])] {
            let guard = if taken { test.clone() } else { test.not() };
            let mut branch = state.clone();
            branch.guard = branch.guard.and(&guard);
            branch.branches.push(Branch { line, taken });
            if branch.guard.code != "false" {
                paths.extend(self.statements(body, vec![branch])?);
            }
        }
        Ok(paths)
    }

    fn pure(&mut self, expression: &Expr, state: &mut State) -> Result<Value, String> {
        if !is_pure(expression) {
            return Err("calls in contract expressions, conditional values or short-circuit operands are unsupported".to_owned());
        }
        self.expression(expression, state)
    }

    /// Evaluates a contract expression, and where it reads a field of null.
    fn specification(
        &mut self,
        expression: &Expr,
        state: &mut State,
    ) -> Result<(Value, Term), String> {
        let outer = self.specification.replace(Term::boolean(false));
        let outer_context = std::mem::replace(&mut self.context, Term::boolean(true));
        let value = self.pure(expression, state);
        let undefined = std::mem::replace(&mut self.specification, outer);
        self.context = outer_context;
        Ok((value?, undefined.unwrap_or_else(|| Term::boolean(false))))
    }

    /// Evaluates `expression` as reached only where `condition` holds, as the
    /// right of `&&` and a ternary's branches are.
    fn guarded(
        &mut self,
        condition: &Term,
        expression: &Expr,
        state: &mut State,
    ) -> Result<Value, String> {
        let outer = self.context.clone();
        self.context = outer.and(condition);
        let value = self.pure(expression, state);
        self.context = outer;
        value
    }

    fn expression(&mut self, expression: &Expr, state: &mut State) -> Result<Value, String> {
        Ok(match expression {
            Expr::Bool(value) => Value::boolean(*value),
            Expr::Text(value) => Value::Scalar(Term::text(value)),
            Expr::Integer(value) => Value::Scalar(Term::integer(*value)),
            Expr::Null => Value::Null,
            Expr::Undefined => Value::Undefined,
            Expr::Name(name) => state
                .env
                .get(name)
                .cloned()
                .ok_or_else(|| format!("unmodeled name `{name}`"))?,
            Expr::Member(target, name) => self.field(target, name, false, state)?,
            Expr::OptionalMember(target, name) => self.field(target, name, true, state)?,
            Expr::Record(fields) => Value::Record(
                fields
                    .iter()
                    .map(|(name, expression)| Ok((name.clone(), self.pure(expression, state)?)))
                    .collect::<Result<_, String>>()?,
            ),
            Expr::Not(operand) => Value::Scalar(self.expression(operand, state)?.as_bool()?.not()),
            Expr::Binary(operator, left, right) => self.binary(*operator, left, right, state)?,
            Expr::Coalesce(left, right) => self.coalesce(left, right, state)?,
            Expr::Conditional(test, then, otherwise) => {
                let test = self.expression(test, state)?.as_bool()?;
                match test.code.as_str() {
                    "true" => self.expression(then, state)?,
                    "false" => self.expression(otherwise, state)?,
                    _ => {
                        let then = self.guarded(&test, then, state)?;
                        let otherwise = self.guarded(&test.not(), otherwise, state)?;
                        Value::choice(&test, then, otherwise)
                    }
                }
            }
            Expr::Call(callee, arguments) => self.call(callee, arguments, false, state)?,
            Expr::Await(inner) => {
                if !self.is_async {
                    return Err("`await` outside an async function".to_owned());
                }
                let Expr::Call(callee, arguments) = inner.as_ref() else {
                    return Err("only a call can be awaited".to_owned());
                };
                self.call(callee, arguments, true, state)?
            }
        })
    }

    /// Reads a field. Where the target may be null, the source path raises
    /// there and continues only where it is not; a contract may not read it.
    fn field(
        &mut self,
        target: &Expr,
        name: &str,
        is_optional: bool,
        state: &mut State,
    ) -> Result<Value, String> {
        let target = self.expression(target, state)?;
        let (value, fails) = read_field(&target, name, is_optional)?;
        let undefined = self.context.and(&fails);
        if undefined.code == "false" {
            return Ok(value);
        }
        if let Some(accumulated) = &mut self.specification {
            *accumulated = accumulated.or(&undefined);
            return Ok(value);
        }
        // The source raises here, so only paths that do not reach the read of
        // null go on.
        state.guard = state.guard.and(&undefined.not());
        Ok(value)
    }

    /// `left ?? right`: `right` where `left` is null or undefined.
    fn coalesce(&mut self, left: &Expr, right: &Expr, state: &mut State) -> Result<Value, String> {
        let left = self.expression(left, state)?;
        if left.is_absent() {
            return self.expression(right, state);
        }
        if !matches!(left, Value::Choice(..)) {
            return Ok(left);
        }
        let right = self.guarded(&absent(&left), right, state)?;
        Ok(fill_absent(left, &right))
    }

    fn binary(
        &mut self,
        operator: Op,
        left: &Expr,
        right: &Expr,
        state: &mut State,
    ) -> Result<Value, String> {
        if matches!(operator, Op::And | Op::Or) && !is_pure(right) {
            return Err("effectful short-circuit operands are unsupported".to_owned());
        }
        let left = self.expression(left, state)?;
        if !matches!(operator, Op::And | Op::Or) {
            let right = self.expression(right, state)?;
            return Ok(Value::Scalar(match operator {
                Op::Equal => equal(&left, &right)?,
                Op::NotEqual => equal(&left, &right)?.not(),
                _ => order(operator, &left, &right)?,
            }));
        }
        let left = left.as_bool()?;
        if (operator == Op::And && left.code == "false")
            || (operator == Op::Or && left.code == "true")
        {
            return Ok(Value::Scalar(left));
        }
        let reached_when = if operator == Op::And {
            left.clone()
        } else {
            left.not()
        };
        let right = self.guarded(&reached_when, right, state)?.as_bool()?;
        Ok(Value::Scalar(if operator == Op::And {
            left.and(&right)
        } else {
            left.or(&right)
        }))
    }

    fn call(
        &mut self,
        callee: &Expr,
        arguments: &[ir::Argument],
        is_awaited: bool,
        state: &mut State,
    ) -> Result<Value, String> {
        let Value::Model(name) = self.expression(callee, state)? else {
            return Err("call target has no call model".to_owned());
        };
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
            .get(&name)
            .ok_or_else(|| format!("missing model `{name}`"))?
            .clone();
        check_awaiting(&name, model.is_async, is_awaited)?;
        let arguments = match_arguments(&name, &model.parameters, values)?;
        let count = state.calls.entry(name.clone()).or_default();
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
            local
                .env
                .insert("outcome".to_owned(), Value::Record(outcome));
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

    fn template(&mut self, template: &Json, state: &mut State) -> Result<Value, String> {
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
            Json::Array(_) => {
                return Err(
                    "templates support Bool, Text, null, integer constants and immutable records"
                        .to_owned(),
                );
            }
        })
    }
}

/// Where `value` is null or undefined.
fn absent(value: &Value) -> Term {
    match value {
        Value::Null | Value::Undefined => Term::boolean(true),
        Value::Choice(test, then, otherwise) => test
            .and(&absent(then))
            .or(&test.not().and(&absent(otherwise))),
        _ => Term::boolean(false),
    }
}

/// `value`, with `fallback` wherever it is null or undefined.
fn fill_absent(value: Value, fallback: &Value) -> Value {
    match value {
        Value::Null | Value::Undefined => fallback.clone(),
        Value::Choice(test, then, otherwise) => Value::choice(
            &test,
            fill_absent(*then, fallback),
            fill_absent(*otherwise, fallback),
        ),
        other => other,
    }
}

fn is_pure(expression: &Expr) -> bool {
    match expression {
        Expr::Call(_, _) | Expr::Await(_) => false,
        Expr::Not(inner) | Expr::Member(inner, _) | Expr::OptionalMember(inner, _) => {
            is_pure(inner)
        }
        Expr::Binary(_, left, right) | Expr::Coalesce(left, right) => {
            is_pure(left) && is_pure(right)
        }
        Expr::Conditional(test, then, otherwise) => {
            is_pure(test) && is_pure(then) && is_pure(otherwise)
        }
        Expr::Record(fields) => fields.values().all(is_pure),
        Expr::Bool(_)
        | Expr::Text(_)
        | Expr::Integer(_)
        | Expr::Null
        | Expr::Undefined
        | Expr::Name(_) => true,
    }
}
