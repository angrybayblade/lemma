//! Symbolic execution of the shared, immutable-value subset.
//!
//! Every path through the function is followed with its inputs as solver
//! terms. A path that raises, as a field read of null does in every
//! language, has no normal return and is dropped: the contract speaks of
//! normal returns only.

mod calls;
mod exceptions;
mod expressions;
mod flow;
mod loops;
mod models;
mod patterns;
mod scopes;
mod value;

use patterns::{pattern_names, set_field, track_freshness};

use std::collections::{BTreeMap, BTreeSet};

use super::contract::{Contract, Input, MAX_SEQUENCE_LENGTH, Sequence};
use super::ir::{
    self, Branch, Expression as Expr, Function, Language, Raised, Statement, StatementKind as Stmt,
};
use models::assume_decorators;
use value::integer_range;
pub(super) use value::{Sort, Term, Value};

#[derive(Debug, Clone)]
pub(super) struct State {
    env: BTreeMap<String, Value>,
    pub guard: Term,
    pub branches: Vec<Branch>,
    effects: BTreeMap<String, Vec<Value>>,
    /// How often each model was called on this path.
    calls: BTreeMap<String, usize>,
    /// Variables holding an object built here from a literal that nothing
    /// else can see yet: the only ones whose fields may be assigned.
    fresh: BTreeSet<String>,
    pub result: Option<Value>,
    /// Which variable each name is. A declaration makes a new one, so a
    /// function defined here can tell whether a name it reads still means
    /// the variable it was defined with.
    variables: BTreeMap<String, u64>,
    /// What the path raised inside a `try`, until a handler catches it.
    raised: Option<Raised>,
    /// How the path is leaving the pass of the loop it is in, until that
    /// loop takes it up.
    jump: Option<Jump>,
    /// Paths that raised inside a `try` while the current statement ran,
    /// each as it was where it raised.
    pending: Vec<State>,
}

/// How a path leaves a pass of a loop early.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Jump {
    Break,
    Continue,
}

impl State {
    /// Whether the path can still reach a statement: it has not returned,
    /// is not leaving a loop's pass, and no read of null has ended it.
    fn is_running(&self) -> bool {
        self.result.is_none()
            && self.raised.is_none()
            && self.jump.is_none()
            && self.guard.code != "false"
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
    let mut domain = Term::boolean(true);
    for (name, shape) in &contract.inputs {
        inputs.insert(name.clone(), input(shape, name, &mut symbols, &mut domain)?);
    }
    let mut evaluator = Evaluator {
        contract,
        inputs: &inputs,
        language: function.language,
        is_async: function.is_async,
        context: Term::boolean(true),
        try_depth: 0,
        handling: Vec::new(),
        inlined: 0,
        specification: None,
        symbols,
        assumptions: BTreeSet::new(),
        calls: 0,
        declared: 0,
    };
    assume_decorators(&function.decorators, contract, &mut evaluator.assumptions)?;
    let (mut state, requires, requires_undefined) = prepare(&inputs, &mut evaluator)?;
    let requires = inputs.values().fold(requires.and(&domain), |all, value| {
        all.and(&ranges(value, function.language))
    });
    bind_parameters(function, &inputs, &mut state, &mut evaluator)?;
    evaluator.enter_function(function, &mut state);
    state.guard = requires.clone();
    let mut paths = evaluator.statements(&function.body, vec![state])?;
    // A raise no handler caught leaves the function without a normal return.
    paths.retain(|path| path.raised.is_none());
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
        fresh: BTreeSet::new(),
        variables: BTreeMap::new(),
        result: None,
        raised: None,
        jump: None,
        pending: Vec::new(),
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
    // A constructor's model is found by `new`, not by a name in the source.
    for name in evaluator.contract.models.keys() {
        if !name.starts_with("new ") {
            insert_model(&mut state.env, name, name)?;
        }
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
    // What a nested function reads from around it may hold any value of
    // its input when the function runs; its own parameters shadow it.
    for (name, input) in &contract.captures {
        let value = inputs.get(input).cloned().ok_or_else(|| {
            format!("capture `{name}` names `{input}`, which is not a declared input")
        })?;
        state.env.insert(name.clone(), value);
    }
    for (parameter, value) in function.parameters.iter().zip(values) {
        if state.env.contains_key(&parameter.name)
            && !contract.captures.contains_key(&parameter.name)
        {
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
            language: Language::Python,
            is_async: false,
            context: Term::boolean(true),
            try_depth: 0,
            handling: Vec::new(),
            inlined: 0,
            specification: None,
            symbols: BTreeMap::new(),
            assumptions: BTreeSet::new(),
            calls: 0,
            declared: 0,
        }
        .specification(&ir::condition(expression)?, &mut state)?;
        Ok(condition.as_bool()?.and(&undefined.not()))
    }
}

/// The symbolic value of an input of `shape`. What the shape itself says of
/// its symbols, as a sequence's length is at most its `max_length`, is added
/// to `domain`.
fn input(
    shape: &Input,
    path: &str,
    symbols: &mut BTreeMap<String, (String, Sort)>,
    domain: &mut Term,
) -> Result<Value, String> {
    match shape {
        Input::Record(fields) => Ok(Value::Record(
            fields
                .iter()
                .map(|(name, shape)| {
                    Ok((
                        name.clone(),
                        input(shape, &format!("{path}.{name}"), symbols, domain)?,
                    ))
                })
                .collect::<Result<_, String>>()?,
        )),
        Input::Sequence(shape) => sequence_input(shape, path, symbols, domain),
        Input::Union(alternatives) => {
            let Some((last, rest)) = alternatives.split_last() else {
                return Err(format!("input `{path}` is an empty union"));
            };
            let mut value = input(last, &format!("{path}|{}", rest.len()), symbols, domain)?;
            for (index, alternative) in rest.iter().enumerate().rev() {
                let selector = symbol(&format!("{path}?{index}"), Sort::Bool, symbols)?;
                let alternative = input(alternative, &format!("{path}|{index}"), symbols, domain)?;
                value = Value::choice(&selector, alternative, value);
            }
            Ok(value)
        }
        Input::Scalar(name) => {
            let sort = match name.as_str() {
                "Bool" => Sort::Bool,
                "Text" => Sort::Text,
                "Int" => Sort::Integer,
                "Null" => return Ok(Value::Null),
                "Undefined" => return Ok(Value::Undefined),
                _ => {
                    return Err(format!(
                        "input `{path}` has unsupported sort `{name}`; use Bool, Text, Int, Null or Undefined"
                    ));
                }
            };
            Ok(Value::Scalar(symbol(path, sort, symbols)?))
        }
    }
}

/// A sequence input: one sequence for each length up to its `max_length`,
/// chosen by a symbolic length. Every length shares the items it has.
fn sequence_input(
    shape: &Sequence,
    path: &str,
    symbols: &mut BTreeMap<String, (String, Sort)>,
    domain: &mut Term,
) -> Result<Value, String> {
    if shape.max_length > MAX_SEQUENCE_LENGTH {
        return Err(format!(
            "input `{path}` states a max_length above {MAX_SEQUENCE_LENGTH}"
        ));
    }
    let length = symbol(&format!("{path}.length"), Sort::Integer, symbols)?;
    let longest = i64::try_from(shape.max_length).map_err(|e| e.to_string())?;
    *domain = domain.and(&Term {
        sort: Sort::Bool,
        code: format!("(and (<= 0 {0}) (<= {0} {longest}))", length.code),
    });
    let mut items = Vec::new();
    for position in 0..shape.max_length {
        items.push(input(
            &shape.sequence,
            &format!("{path}[{position}]"),
            symbols,
            domain,
        )?);
    }
    let mut value = Value::Sequence(items.clone());
    for shorter in (0..longest).rev() {
        let is_this_long = Term {
            sort: Sort::Bool,
            code: format!("(= {} {shorter})", length.code),
        };
        let kept = usize::try_from(shorter).map_err(|e| e.to_string())?;
        value = Value::choice(
            &is_this_long,
            Value::Sequence(items[..kept].to_vec()),
            value,
        );
    }
    Ok(value)
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

/// That every integer in `value` is one `language` can hold.
fn ranges(value: &Value, language: Language) -> Term {
    match value {
        Value::Scalar(term) if term.sort == Sort::Integer => integer_range(term, language),
        Value::Record(fields) | Value::Object(fields) => {
            fields.values().fold(Term::boolean(true), |all, field| {
                all.and(&ranges(field, language))
            })
        }
        Value::Choice(_, then, otherwise) => {
            ranges(then, language).and(&ranges(otherwise, language))
        }
        Value::Sequence(items) => items.iter().fold(Term::boolean(true), |all, item| {
            all.and(&ranges(item, language))
        }),
        _ => Term::boolean(true),
    }
}

struct Evaluator<'a> {
    contract: &'a Contract,
    inputs: &'a BTreeMap<String, Value>,
    /// The checked function's language, for what the languages disagree on.
    language: Language,
    /// Whether the function under check is `async`, where `await` is allowed.
    is_async: bool,
    /// What must hold for the expression being evaluated to be reached at
    /// all: the left of `&&` for its right, a ternary's test for a branch.
    context: Term,
    /// While a contract expression is evaluated: where it reads a field of
    /// null or undefined, rather than paths that raise.
    specification: Option<Term>,
    /// How many `try` bodies enclose the statement running, where a raise
    /// may be caught and so must be followed rather than dropped.
    try_depth: usize,
    /// What the handlers running caught, which a bare `raise` raises again.
    handling: Vec<Raised>,
    /// How many called functions are being run inside one another.
    inlined: usize,
    /// Every symbolic value: the contract's inputs, then each call's outcomes.
    symbols: BTreeMap<String, (String, Sort)>,
    assumptions: BTreeSet<String>,
    calls: usize,
    /// How many variables were declared, which numbers the next one.
    declared: u64,
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

    fn step(&mut self, statement: &Statement, state: State) -> Result<Vec<State>, String> {
        if !state.is_running() {
            return Ok(vec![state]);
        }
        let mut paths = self.run(statement, state)?;
        let mut raised = Vec::new();
        for path in &mut paths {
            raised.append(&mut path.pending);
        }
        paths.extend(raised);
        Ok(paths)
    }

    fn run(&mut self, statement: &Statement, mut state: State) -> Result<Vec<State>, String> {
        match &statement.kind {
            Stmt::Bind(pattern, expression) => {
                self.assign(pattern, expression, false, &mut state)?;
            }
            Stmt::Assign(pattern, expression) => {
                self.assign(pattern, expression, true, &mut state)?;
            }
            Stmt::Update(name, operator, expression) => {
                self.update(name, *operator, expression, &mut state)?;
            }
            Stmt::SetField(name, field, expression) => {
                let value = self.expression(expression, &mut state)?;
                set_field(name, field, value, &mut state)?;
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
            Stmt::Raise(raised) => {
                let raised = match raised {
                    Raised::Again => self.handling.last().cloned().unwrap_or(Raised::Unknown),
                    other => other.clone(),
                };
                if self.try_depth == 0 {
                    state.guard = Term::boolean(false);
                } else {
                    state.raised = Some(raised);
                }
            }
            Stmt::Assert(condition) => {
                let condition = self.expression(condition, &mut state)?.as_bool()?;
                self.raise_where(
                    &condition.not(),
                    Raised::Class("AssertionError".to_owned()),
                    &mut state,
                );
                self.assumptions
                    .insert("Python runs with assertions enabled, not with -O".to_owned());
            }
            Stmt::ForEach(each) => return self.for_each(statement.line, each, state),
            Stmt::Break => state.jump = Some(Jump::Break),
            Stmt::Continue => state.jump = Some(Jump::Continue),
            Stmt::Try(statement) => return self.try_statement(statement, state),
            Stmt::With(contexts, body) => return self.with_statement(contexts, body, state),
            Stmt::If(expression, then, otherwise) => {
                return self.branch(statement.line, expression, [then, otherwise], state);
            }
        }
        Ok(vec![state])
    }

    /// Binds what `pattern` names to the value of `expression`: a
    /// declaration, or an assignment to variables already declared.
    fn assign(
        &mut self,
        pattern: &ir::Pattern,
        expression: &Expr,
        is_assignment: bool,
        state: &mut State,
    ) -> Result<(), String> {
        let value = self.expression(expression, state)?;
        self.bind(pattern, value, is_assignment, state)?;
        // A Go `:=` declares where it runs; Python and TypeScript declare
        // for the whole function or block.
        if !is_assignment && self.language == Language::Go {
            self.declare_pattern(pattern, state);
        }
        track_freshness(pattern, expression, &mut state.fresh);
        Ok(())
    }

    /// `name op= value` on a variable.
    fn update(
        &mut self,
        name: &str,
        operator: lemma_lang::syntax::ast::BinaryOperator,
        expression: &Expr,
        state: &mut State,
    ) -> Result<(), String> {
        let value = self.expression(expression, state)?;
        let current = state
            .env
            .get(name)
            .cloned()
            .ok_or_else(|| format!("`{name}` is updated before it is assigned"))?;
        let updated = self.arithmetic(operator, &current, &value)?;
        state.env.insert(name.to_owned(), Value::Scalar(updated));
        state.fresh.remove(name);
        Ok(())
    }

    fn branch(
        &mut self,
        line: usize,
        expression: &Expr,
        bodies: [&[Statement]; 2],
        mut state: State,
    ) -> Result<Vec<State>, String> {
        let test = self.expression(expression, &mut state)?.as_bool()?;
        let mut paths = std::mem::take(&mut state.pending);
        for (taken, body) in [(true, bodies[0]), (false, bodies[1])] {
            let guard = if taken { test.clone() } else { test.not() };
            let mut branch = state.clone();
            branch.guard = branch.guard.and(&guard);
            branch.branches.push(Branch { line, taken });
            if branch.guard.code != "false" {
                paths.extend(self.block(body, branch)?);
            }
        }
        Ok(paths)
    }

    /// Runs a block. TypeScript and Go declarations last until its end, where
    /// what they shadowed is visible again; Python's last for the function.
    /// A TypeScript `let` or `const` is a new variable from the start of its
    /// block, unbound until it is declared.
    fn block(&mut self, body: &[Statement], mut state: State) -> Result<Vec<State>, String> {
        if self.language == Language::Python {
            return self.statements(body, vec![state]);
        }
        let mut declared = BTreeSet::new();
        for statement in body {
            if let Stmt::Bind(pattern, _) = &statement.kind {
                pattern_names(pattern, &mut declared);
            }
        }
        let outer: Vec<Shadowed> = declared
            .iter()
            .map(|name| Shadowed::of(name, &state))
            .collect();
        if self.language == Language::TypeScript {
            for shadowed in &outer {
                self.unbind(&shadowed.name, &mut state);
            }
        }
        let mut paths = self.statements(body, vec![state])?;
        for path in &mut paths {
            for shadowed in &outer {
                restore(path, shadowed);
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
}

/// What a name was before a block declared its own.
struct Shadowed {
    name: String,
    value: Option<Value>,
    is_fresh: bool,
    variable: Option<u64>,
}

impl Shadowed {
    /// What `name` is on a path now.
    fn of(name: &str, state: &State) -> Self {
        Self {
            name: name.to_owned(),
            value: state.env.get(name).cloned(),
            is_fresh: state.fresh.contains(name),
            variable: state.variables.get(name).copied(),
        }
    }
}

/// Makes a name again what it was before a block declared its own.
fn restore(state: &mut State, shadowed: &Shadowed) {
    let name = &shadowed.name;
    match &shadowed.value {
        Some(value) => state.env.insert(name.clone(), value.clone()),
        None => state.env.remove(name),
    };
    match shadowed.variable {
        Some(variable) => state.variables.insert(name.clone(), variable),
        None => state.variables.remove(name),
    };
    if shadowed.is_fresh {
        state.fresh.insert(name.clone());
    } else {
        state.fresh.remove(name);
    }
}

fn lookup(name: &str, state: &State) -> Result<Value, String> {
    state
        .env
        .get(name)
        .cloned()
        .ok_or_else(|| format!("unmodeled name `{name}`"))
}

fn is_pure(expression: &Expr) -> bool {
    !expression.has_call()
}
