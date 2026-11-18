//! Symbolic execution of the shared, immutable-value subset.
//!
//! Every path through the function is followed with its inputs as solver
//! terms. A path that raises, as a field read of null does in every
//! language, has no normal return and is dropped: the contract speaks of
//! normal returns only.

mod exceptions;
mod models;
mod value;

use std::collections::{BTreeMap, BTreeSet};

use lemma_lang::syntax::ast::BinaryOperator as Op;
use serde_json::Value as Json;

use super::contract::{Contract, Input};
use super::ir::{
    self, Branch, Expression as Expr, Function, Language, Part, Pattern, Raised, Statement,
    StatementKind as Stmt, Try,
};
use models::{assume_decorators, check_awaiting, match_arguments, reads_inputs};
use value::{
    Missing, arithmetic, contains, element, equal, integer_range, negate, order, read_field,
    to_text,
};
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
    /// What the path raised inside a `try`, until a handler catches it.
    raised: Option<Raised>,
    /// Paths that raised inside a `try` while the current statement ran,
    /// each as it was where it raised.
    pending: Vec<State>,
}

impl State {
    /// Whether the path can still reach a statement: it has not returned,
    /// and no read of null has ended it.
    fn is_running(&self) -> bool {
        self.result.is_none() && self.raised.is_none() && self.guard.code != "false"
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
        language: function.language,
        is_async: function.is_async,
        context: Term::boolean(true),
        try_depth: 0,
        handling: Vec::new(),
        specification: None,
        symbols,
        assumptions: BTreeSet::new(),
        calls: 0,
    };
    assume_decorators(&function.decorators, contract, &mut evaluator.assumptions)?;
    let (mut state, requires, requires_undefined) = prepare(&inputs, &mut evaluator)?;
    let requires = inputs.values().fold(requires, |all, value| {
        all.and(&ranges(value, function.language))
    });
    bind_parameters(function, &inputs, &mut state, &mut evaluator)?;
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
        result: None,
        raised: None,
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
            language: Language::Python,
            is_async: false,
            context: Term::boolean(true),
            try_depth: 0,
            handling: Vec::new(),
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
            Stmt::Bind(pattern, expression) | Stmt::Assign(pattern, expression) => {
                let value = self.expression(expression, &mut state)?;
                let is_assignment = matches!(statement.kind, Stmt::Assign(..));
                self.bind(pattern, value, is_assignment, &mut state)?;
                track_freshness(pattern, expression, &mut state.fresh);
            }
            Stmt::Update(name, operator, expression) => {
                let value = self.expression(expression, &mut state)?;
                let current = state
                    .env
                    .get(name)
                    .cloned()
                    .ok_or_else(|| format!("`{name}` is updated before it is assigned"))?;
                let updated = self.arithmetic(*operator, &current, &value)?;
                state.env.insert(name.clone(), Value::Scalar(updated));
                state.fresh.remove(name);
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
            Stmt::Try(statement) => return self.try_statement(statement, state),
            Stmt::If(expression, then, otherwise) => {
                return self.branch(statement.line, expression, [then, otherwise], state);
            }
        }
        Ok(vec![state])
    }

    /// Binds what `pattern` names to the parts of `value`; an assignment
    /// only to variables already declared.
    fn bind(
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
            Pattern::Sequence(parts) => {
                let Value::Sequence(items) = value else {
                    return Err("only a sequence of known length can be unpacked".to_owned());
                };
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

    /// Unpacking a sequence of another length: Python raises, JavaScript
    /// reads undefined past the end, and Go does not compile.
    fn unpack_mismatch(
        &mut self,
        parts: &[Pattern],
        items: Vec<Value>,
        is_assignment: bool,
        state: &mut State,
    ) -> Result<(), String> {
        match self.language {
            Language::Python => {
                state.guard = Term::boolean(false);
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

    /// Narrows the path to where a read did not raise: only where the read
    /// is reached, and in a contract, records it as undefined instead.
    fn narrow(&mut self, fails: &Term, state: &mut State) {
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
    fn raise_where(&mut self, condition: &Term, raised: Raised, state: &mut State) {
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

    /// `try`: the body, then a handler for what it raised, `else` if it
    /// raised nothing, and `finally` on every way out.
    fn try_statement(&mut self, statement: &Try, state: State) -> Result<Vec<State>, String> {
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
    fn handle(
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
    fn finally(&mut self, body: &[Statement], mut path: State) -> Result<Vec<State>, String> {
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
    fn block(&mut self, body: &[Statement], state: State) -> Result<Vec<State>, String> {
        if self.language == Language::Python {
            return self.statements(body, vec![state]);
        }
        let mut declared = BTreeSet::new();
        for statement in body {
            if let Stmt::Bind(pattern, _) = &statement.kind {
                pattern_names(pattern, &mut declared);
            }
        }
        let outer: Vec<(String, Option<Value>, bool)> = declared
            .into_iter()
            .map(|name| {
                let value = state.env.get(&name).cloned();
                let is_fresh = state.fresh.contains(&name);
                (name, value, is_fresh)
            })
            .collect();
        let mut paths = self.statements(body, vec![state])?;
        for path in &mut paths {
            for (name, value, is_fresh) in &outer {
                restore(path, name, value.as_ref(), *is_fresh);
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
            Expr::Name(name) => {
                // A variable used as a whole may now be seen elsewhere.
                state.fresh.remove(name);
                lookup(name, state)?
            }
            Expr::Member(target, name) => self.field(target, name, false, state)?,
            Expr::OptionalMember(target, name) => self.field(target, name, true, state)?,
            Expr::Record(fields) => Value::Object(
                fields
                    .iter()
                    .map(|(name, expression)| Ok((name.clone(), self.pure(expression, state)?)))
                    .collect::<Result<_, String>>()?,
            ),
            Expr::Not(operand) => Value::Scalar(self.expression(operand, state)?.as_bool()?.not()),
            Expr::Negate(operand) => {
                let operand = self.expression(operand, state)?;
                self.assume_integer_arithmetic();
                Value::Scalar(negate(&operand)?)
            }
            Expr::Template(parts) => self.template_text(parts, state)?,
            Expr::Sequence(items) => Value::Sequence(self.expressions(items, state)?),
            Expr::Set(items) => Value::Set(self.expressions(items, state)?),
            Expr::Index(target, position) => self.element(target, *position, state)?,
            Expr::Contains(needle, haystack) => {
                let needle = self.expression(needle, state)?;
                let haystack = self.expression(haystack, state)?;
                Value::Scalar(contains(&needle, &haystack)?)
            }
            Expr::Binary(operator, left, right) => self.binary(*operator, left, right, state)?,
            Expr::Coalesce(left, right) => self.coalesce(left, right, state)?,
            Expr::Conditional(test, then, otherwise) => {
                self.conditional(test, then, otherwise, state)?
            }
            Expr::Call(callee, arguments) => self.call(callee, arguments, false, state)?,
            Expr::Await(inner) => self.awaited(inner, state)?,
        })
    }

    fn expressions(&mut self, items: &[Expr], state: &mut State) -> Result<Vec<Value>, String> {
        items
            .iter()
            .map(|item| self.expression(item, state))
            .collect()
    }

    /// What reading a field an object lacks does here: a contract counts it
    /// as undefined, the source as its language does.
    fn missing(&self) -> Missing {
        if self.specification.is_some() {
            return Missing::Raises;
        }
        Missing::of(self.language)
    }

    /// What a field or element is read from: reading a part of a variable
    /// does not let anything else see the variable's object.
    fn target(&mut self, target: &Expr, state: &mut State) -> Result<Value, String> {
        if let Expr::Name(name) = target {
            return lookup(name, state);
        }
        self.expression(target, state)
    }

    /// `test ? then : otherwise`; only the branch the test picks is reached.
    fn conditional(
        &mut self,
        test: &Expr,
        then: &Expr,
        otherwise: &Expr,
        state: &mut State,
    ) -> Result<Value, String> {
        let test = self.expression(test, state)?.as_bool()?;
        match test.code.as_str() {
            "true" => self.expression(then, state),
            "false" => self.expression(otherwise, state),
            _ => {
                let then = self.guarded(&test, then, state)?;
                let otherwise = self.guarded(&test.not(), otherwise, state)?;
                Ok(Value::choice(&test, then, otherwise))
            }
        }
    }

    fn awaited(&mut self, inner: &Expr, state: &mut State) -> Result<Value, String> {
        if !self.is_async {
            return Err("`await` outside an async function".to_owned());
        }
        let Expr::Call(callee, arguments) = inner else {
            return Err("only a call can be awaited".to_owned());
        };
        self.call(callee, arguments, true, state)
    }

    fn element(
        &mut self,
        target: &Expr,
        position: i64,
        state: &mut State,
    ) -> Result<Value, String> {
        let target = self.expression(target, state)?;
        let (value, fails) = element(&target, position, self.language)?;
        self.narrow(&fails, state);
        Ok(value)
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
        let target = self.target(target, state)?;
        let (value, fails) = read_field(&target, name, is_optional, self.missing())?;
        self.narrow(&fails, state);
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
                Op::Add | Op::Subtract | Op::Multiply => {
                    self.arithmetic(operator, &left, &right)?
                }
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

    /// A formatted string, with each value spelled as the language spells it.
    fn template_text(&mut self, parts: &[Part], state: &mut State) -> Result<Value, String> {
        let mut text = Term::text("");
        for part in parts {
            let piece = match part {
                Part::Text(piece) => Term::text(piece),
                Part::Value(expression) => {
                    to_text(&self.expression(expression, state)?, self.language)?
                }
            };
            text = Term {
                sort: Sort::Text,
                code: format!("(str.++ {} {})", text.code, piece.code),
            };
        }
        Ok(Value::Scalar(text))
    }

    fn arithmetic(&mut self, operator: Op, left: &Value, right: &Value) -> Result<Term, String> {
        let term = arithmetic(operator, left, right)?;
        if term.sort == Sort::Integer {
            self.assume_integer_arithmetic();
        }
        Ok(term)
    }

    /// Source integer arithmetic is exact only within the language's range:
    /// TypeScript numbers lose precision past 2^53, Go integers wrap.
    fn assume_integer_arithmetic(&mut self) {
        if self.specification.is_some() {
            return;
        }
        let assumption = match self.language {
            Language::Python => return,
            Language::TypeScript => {
                "TypeScript integer arithmetic stays within 2^53 - 1, where numbers are exact"
            }
            Language::Go => "Go integer arithmetic does not overflow",
        };
        self.assumptions.insert(assumption.to_owned());
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
            Json::Array(items) => Value::Sequence(
                items
                    .iter()
                    .map(|item| self.template(item, state))
                    .collect::<Result<_, _>>()?,
            ),
        })
    }
}

/// Makes `name` again what it was before a block declared its own.
fn restore(state: &mut State, name: &str, value: Option<&Value>, is_fresh: bool) {
    match value {
        Some(value) => state.env.insert(name.to_owned(), value.clone()),
        None => state.env.remove(name),
    };
    if is_fresh {
        state.fresh.insert(name.to_owned());
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

/// After binding `pattern` to `expression`: a variable bound to a record or
/// sequence literal holds an object nothing else can see.
fn track_freshness(pattern: &Pattern, expression: &Expr, fresh: &mut BTreeSet<String>) {
    let mut names = BTreeSet::new();
    pattern_names(pattern, &mut names);
    let is_literal = matches!(expression, Expr::Record(_) | Expr::Sequence(_));
    for name in names {
        if is_literal && matches!(pattern, Pattern::Name(_)) {
            fresh.insert(name);
        } else {
            fresh.remove(&name);
        }
    }
}

fn pattern_names(pattern: &Pattern, names: &mut BTreeSet<String>) {
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
    }
}

/// Assigns a field of a variable's object, which only an object nothing
/// else can see allows: the assignment is then invisible elsewhere.
fn set_field(name: &str, field: &str, value: Value, state: &mut State) -> Result<(), String> {
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
        Expr::Not(inner)
        | Expr::Negate(inner)
        | Expr::Index(inner, _)
        | Expr::Member(inner, _)
        | Expr::OptionalMember(inner, _) => is_pure(inner),
        Expr::Sequence(items) | Expr::Set(items) => items.iter().all(is_pure),
        Expr::Contains(left, right) => is_pure(left) && is_pure(right),
        Expr::Template(parts) => parts.iter().all(|part| match part {
            Part::Text(_) => true,
            Part::Value(value) => is_pure(value),
        }),
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
