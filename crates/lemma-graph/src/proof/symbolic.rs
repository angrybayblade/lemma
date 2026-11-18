//! Symbolic execution of the shared, immutable-value subset.

use std::collections::{BTreeMap, BTreeSet};
use std::fmt::Write as _;

use lemma_lang::syntax::ast::BinaryOperator as Op;
use serde_json::Value as Json;

use super::contract::{CallModel, Contract, Input};
use super::ir::{self, Branch, Expression as Expr, Function, Statement, StatementKind as Stmt};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum Sort {
    Bool,
    Text,
    Integer,
}

impl Sort {
    pub(super) fn smt(self) -> &'static str {
        match self {
            Self::Bool => "Bool",
            Self::Text => "String",
            Self::Integer => "Int",
        }
    }
}

#[derive(Debug, Clone)]
pub(super) struct Term {
    pub sort: Sort,
    pub code: String,
}

impl Term {
    pub(super) fn boolean(value: bool) -> Self {
        Self {
            sort: Sort::Bool,
            code: value.to_string(),
        }
    }
    pub(super) fn not(&self) -> Self {
        match self.code.as_str() {
            "true" => Self::boolean(false),
            "false" => Self::boolean(true),
            _ => Self {
                sort: Sort::Bool,
                code: format!("(not {})", self.code),
            },
        }
    }
    pub(super) fn and(&self, other: &Self) -> Self {
        match (self.code.as_str(), other.code.as_str()) {
            ("false", _) | (_, "false") => Self::boolean(false),
            ("true", _) => other.clone(),
            (_, "true") => self.clone(),
            _ => Self {
                sort: Sort::Bool,
                code: format!("(and {} {})", self.code, other.code),
            },
        }
    }
}

#[derive(Debug, Clone)]
pub(super) enum Value {
    Scalar(Term),
    Record(BTreeMap<String, Value>),
    Model(String),
    Undefined,
}

impl Value {
    fn boolean(value: bool) -> Self {
        Self::Scalar(Term::boolean(value))
    }
    fn integer(value: usize) -> Self {
        Self::Scalar(Term {
            sort: Sort::Integer,
            code: value.to_string(),
        })
    }
    pub(super) fn as_bool(&self) -> Result<Term, String> {
        match self {
            Self::Scalar(term) if term.sort == Sort::Bool => Ok(term.clone()),
            _ => Err("a condition must be Boolean; implicit coercion is unsupported".to_owned()),
        }
    }
}

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

pub(super) struct Execution {
    pub symbols: BTreeMap<String, (String, Sort)>,
    pub requires: Term,
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
        symbols,
        assumptions: BTreeSet::new(),
        calls: 0,
    };
    assume_decorators(&function.decorators, contract, &mut evaluator.assumptions)?;
    let (mut state, requires) = prepare(&inputs, &mut evaluator)?;
    bind_parameters(function, &inputs, &mut state, &mut evaluator)?;
    state.guard = requires.clone();
    let paths = evaluator.statements(&function.body, vec![state])?;
    if paths.iter().any(|path| path.result.is_none()) {
        return Err("a source path falls through without an explicit return".to_owned());
    }
    Ok(Execution {
        symbols: evaluator.symbols,
        requires,
        paths,
        assumptions: evaluator.assumptions,
        inputs,
    })
}

/// Decorators the languages define to return the function they decorate, or
/// to mark it without changing how a call runs.
const UNCHANGED_BY: &[&str] = &[
    "staticmethod",
    "classmethod",
    "property",
    "abstractmethod",
    "abc.abstractmethod",
    "override",
    "typing.override",
    "typing_extensions.override",
    "final",
    "typing.final",
    "dataclass",
    "dataclasses.dataclass",
];

/// Requires a decorator model for every decorator the language does not
/// define, and records its assumption.
fn assume_decorators(
    decorators: &[String],
    contract: &Contract,
    assumptions: &mut BTreeSet<String>,
) -> Result<(), String> {
    for decorator in decorators {
        let callee = decorator.split('(').next().unwrap_or(decorator).trim();
        if UNCHANGED_BY.contains(&decorator.as_str()) || is_property_accessor(decorator) {
            continue;
        }
        let assumption = contract
            .decorators
            .get(decorator)
            .or_else(|| contract.decorators.get(callee))
            .ok_or_else(|| format!("decorator `{decorator}` needs a decorator model"))?;
        assumptions.insert(assumption.clone());
    }
    Ok(())
}

/// `@<property>.setter`, `.getter` and `.deleter` register an accessor of a
/// property and return it; the accessor's body is what runs.
fn is_property_accessor(decorator: &str) -> bool {
    decorator.split_once('.').is_some_and(|(owner, accessor)| {
        !owner.contains(['(', ' ']) && matches!(accessor, "setter" | "getter" | "deleter")
    })
}

fn prepare(
    inputs: &BTreeMap<String, Value>,
    evaluator: &mut Evaluator<'_>,
) -> Result<(State, Term), String> {
    let mut state = State {
        env: inputs.clone(),
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
    for name in evaluator.contract.models.keys() {
        insert_model(&mut state.env, name, name)?;
    }
    let mut requires = Term::boolean(true);
    for expression in &evaluator.contract.requires {
        requires = requires.and(
            &evaluator
                .pure(&ir::condition(expression)?, &mut state)?
                .as_bool()?,
        );
    }
    Ok((state, requires))
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
    pub(super) fn postcondition(
        &self,
        path: &State,
        expression: &str,
        contract: &Contract,
    ) -> Result<Term, String> {
        let mut state = path.clone();
        state.env = self.inputs.clone();
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
            effects.insert(
                effect.name.clone(),
                Value::Record(BTreeMap::from([
                    ("called".to_owned(), Value::boolean(!events.is_empty())),
                    ("count".to_owned(), Value::integer(events.len())),
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
        Evaluator {
            contract,
            inputs: &self.inputs,
            is_async: false,
            symbols: BTreeMap::new(),
            assumptions: BTreeSet::new(),
            calls: 0,
        }
        .pure(&ir::condition(expression)?, &mut state)?
        .as_bool()
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
        Input::Scalar(name) => {
            let sort = match name.as_str() {
                "Bool" => Sort::Bool,
                "Text" => Sort::Text,
                _ => {
                    return Err(format!(
                        "input `{path}` has unsupported sort `{name}`; use Bool or Text"
                    ));
                }
            };
            let symbol = format!("input_{}", symbols.len());
            if symbols
                .insert(path.to_owned(), (symbol.clone(), sort))
                .is_some()
            {
                return Err("ambiguous symbolic input path".to_owned());
            }
            Ok(Value::Scalar(Term { sort, code: symbol }))
        }
    }
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
            next.extend(self.step(statement, state)?);
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
        if state.result.is_some() || state.guard.code == "false" {
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
                state.result = Some(self.expression(expression, &mut state)?);
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

    fn expression(&mut self, expression: &Expr, state: &mut State) -> Result<Value, String> {
        Ok(match expression {
            Expr::Bool(value) => Value::boolean(*value),
            Expr::Text(value) => Value::Scalar(Term {
                sort: Sort::Text,
                code: quote(value),
            }),
            Expr::Integer(value) => Value::Scalar(Term {
                sort: Sort::Integer,
                code: value.to_string(),
            }),
            Expr::Undefined => Value::Undefined,
            Expr::Name(name) => state
                .env
                .get(name)
                .cloned()
                .ok_or_else(|| format!("unmodeled name `{name}`"))?,
            Expr::Member(target, name) => {
                let Value::Record(fields) = self.expression(target, state)? else {
                    return Err(format!("cannot read `{name}` on a non-record"));
                };
                fields
                    .get(name)
                    .cloned()
                    .ok_or_else(|| format!("record has no declared field `{name}`"))?
            }
            Expr::Record(fields) => Value::Record(
                fields
                    .iter()
                    .map(|(name, expression)| Ok((name.clone(), self.pure(expression, state)?)))
                    .collect::<Result<_, String>>()?,
            ),
            Expr::Not(operand) => Value::Scalar(self.expression(operand, state)?.as_bool()?.not()),
            Expr::Binary(operator, left, right) => self.binary(*operator, left, right, state)?,
            Expr::Conditional(test, then, otherwise) => {
                let test = self.expression(test, state)?.as_bool()?;
                match test.code.as_str() {
                    "true" => self.expression(then, state)?,
                    "false" => self.expression(otherwise, state)?,
                    _ => merge(&test, self.pure(then, state)?, self.pure(otherwise, state)?)?,
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
            return compare(operator, left, self.expression(right, state)?);
        }
        let left = left.as_bool()?;
        if (operator == Op::And && left.code == "false")
            || (operator == Op::Or && left.code == "true")
        {
            return Ok(Value::Scalar(left));
        }
        let right = self.pure(right, state)?.as_bool()?;
        Ok(Value::Scalar(if operator == Op::And {
            left.and(&right)
        } else {
            left.not().and(&right.not()).not()
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
        local.env = self.inputs.clone();
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
            Json::String(value) => Value::Scalar(Term {
                sort: Sort::Text,
                code: quote(value),
            }),
            Json::Number(value) => {
                let integer = value
                    .as_i64()
                    .filter(|n| n.unsigned_abs() <= 9_007_199_254_740_991)
                    .ok_or("only exactly representable integer constants are supported")?;
                Value::Scalar(Term {
                    sort: Sort::Integer,
                    code: if integer < 0 {
                        format!("(- {})", integer.unsigned_abs())
                    } else {
                        integer.to_string()
                    },
                })
            }
            Json::Object(fields) if fields.len() == 1 && fields.contains_key("expr") => self.pure(
                &ir::condition(
                    fields["expr"]
                        .as_str()
                        .ok_or("expr template must contain text")?,
                )?,
                state,
            )?,
            Json::Object(fields) => Value::Record(
                fields
                    .iter()
                    .map(|(key, value)| Ok((key.clone(), self.template(value, state)?)))
                    .collect::<Result<_, String>>()?,
            ),
            _ => {
                return Err(
                    "templates support Bool, Text, integer constants and immutable records"
                        .to_owned(),
                );
            }
        })
    }
}

/// Gives each of a model's parameters the argument passed for it: in order,
/// then by name. A parameter left out is absent, rather than falling back to
/// a specification input of the same name.
fn match_arguments(
    name: &str,
    parameters: &[String],
    arguments: Vec<(Option<&str>, Value)>,
) -> Result<BTreeMap<String, Value>, String> {
    let mut bound = BTreeMap::new();
    let mut position = 0;
    for (keyword, value) in arguments {
        let parameter = if let Some(keyword) = keyword {
            parameters
                .iter()
                .find(|parameter| parameter.as_str() == keyword)
                .ok_or_else(|| format!("model `{name}` has no parameter `{keyword}`"))?
        } else {
            position += 1;
            parameters
                .get(position - 1)
                .ok_or_else(|| format!("model `{name}` takes fewer arguments"))?
        };
        if bound.insert(parameter.clone(), value).is_some() {
            return Err(format!("model `{name}` is given `{parameter}` twice"));
        }
    }
    for parameter in parameters {
        bound.entry(parameter.clone()).or_insert(Value::Undefined);
    }
    Ok(bound)
}

fn check_awaiting(name: &str, is_async: bool, is_awaited: bool) -> Result<(), String> {
    if is_async && !is_awaited {
        return Err(format!(
            "the call to async `{name}` is not awaited, so its result is a pending promise or coroutine"
        ));
    }
    if !is_async && is_awaited {
        return Err(format!("`{name}` is awaited, but its model is not `async`"));
    }
    Ok(())
}

/// Whether a model's templates read a specification input, rather than only
/// its own arguments and outcomes.
fn reads_inputs(model: &CallModel, inputs: &BTreeMap<String, Value>) -> Result<bool, String> {
    let mut names = BTreeSet::new();
    template_names(&model.returns, &mut names)?;
    if let Some(effect) = &model.effect {
        for template in effect.fields.values() {
            template_names(template, &mut names)?;
        }
    }
    Ok(names
        .iter()
        .any(|name| inputs.contains_key(name) && !model.parameters.contains(name)))
}

fn template_names(template: &Json, names: &mut BTreeSet<String>) -> Result<(), String> {
    let Json::Object(fields) = template else {
        return Ok(());
    };
    if let (1, Some(Json::String(text))) = (fields.len(), fields.get("expr")) {
        expression_names(&ir::condition(text)?, names);
        return Ok(());
    }
    for value in fields.values() {
        template_names(value, names)?;
    }
    Ok(())
}

fn expression_names(expression: &Expr, names: &mut BTreeSet<String>) {
    match expression {
        Expr::Name(name) => {
            names.insert(name.clone());
        }
        Expr::Member(inner, _) | Expr::Not(inner) | Expr::Await(inner) => {
            expression_names(inner, names);
        }
        Expr::Binary(_, left, right) => {
            expression_names(left, names);
            expression_names(right, names);
        }
        Expr::Conditional(test, then, otherwise) => {
            for part in [test, then, otherwise] {
                expression_names(part, names);
            }
        }
        Expr::Record(fields) => {
            for value in fields.values() {
                expression_names(value, names);
            }
        }
        Expr::Call(callee, arguments) => {
            expression_names(callee, names);
            for argument in arguments {
                expression_names(&argument.value, names);
            }
        }
        Expr::Bool(_) | Expr::Text(_) | Expr::Integer(_) | Expr::Undefined => {}
    }
}

fn compare(operator: Op, left: Value, right: Value) -> Result<Value, String> {
    let (Value::Scalar(left), Value::Scalar(right)) = (left, right) else {
        return Err("only scalar comparisons are supported".to_owned());
    };
    if left.sort != right.sort {
        return Err("cross-type comparisons are unsupported".to_owned());
    }
    let symbol = match operator {
        Op::Equal | Op::NotEqual => "=",
        Op::Less if left.sort == Sort::Integer => "<",
        Op::LessOrEqual if left.sort == Sort::Integer => "<=",
        Op::Greater if left.sort == Sort::Integer => ">",
        Op::GreaterOrEqual if left.sort == Sort::Integer => ">=",
        _ => {
            return Err(
                "arithmetic and ordered symbolic text comparisons are unsupported".to_owned(),
            );
        }
    };
    let term = Term {
        sort: Sort::Bool,
        code: format!("({symbol} {} {})", left.code, right.code),
    };
    Ok(Value::Scalar(if operator == Op::NotEqual {
        term.not()
    } else {
        term
    }))
}

fn merge(test: &Term, then: Value, otherwise: Value) -> Result<Value, String> {
    match (then, otherwise) {
        (Value::Scalar(then), Value::Scalar(otherwise)) if then.sort == otherwise.sort => {
            Ok(Value::Scalar(Term {
                sort: then.sort,
                code: format!("(ite {} {} {})", test.code, then.code, otherwise.code),
            }))
        }
        _ => Err("conditional values must be scalars of the same sort".to_owned()),
    }
}

fn is_pure(expression: &Expr) -> bool {
    match expression {
        Expr::Call(_, _) | Expr::Await(_) => false,
        Expr::Not(inner) | Expr::Member(inner, _) => is_pure(inner),
        Expr::Binary(_, left, right) => is_pure(left) && is_pure(right),
        Expr::Conditional(test, then, otherwise) => {
            is_pure(test) && is_pure(then) && is_pure(otherwise)
        }
        Expr::Record(fields) => fields.values().all(is_pure),
        _ => true,
    }
}

fn quote(text: &str) -> String {
    let mut encoded = String::from("\"");
    for character in text.chars() {
        match character {
            '"' => encoded.push_str("\"\""),
            // Escape backslashes too: an input containing a literal \u sequence
            // must not be reinterpreted as an SMT-LIB Unicode escape.
            '\\' => encoded.push_str("\\u{5c}"),
            c if !c.is_ascii() || c.is_control() => {
                let _ = write!(encoded, "\\u{{{:x}}}", u32::from(c));
            }
            c => encoded.push(c),
        }
    }
    encoded.push('"');
    encoded
}
