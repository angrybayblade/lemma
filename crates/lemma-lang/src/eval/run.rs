//! The interpreter: statements, cases and expressions.

use std::collections::HashMap;

use super::datum::{Absence, Datum, Evidence, FactDatum, Truth};
use super::facts::compares;
use super::{BehaviorResult, CaseResult, Demand, EvidenceUse, Outcome, Problem, World};
use crate::model::{
    Behavior, CallArgument, Callable, Check, Checker, Expression, ExpressionKind, Function, Model,
    Predicate, Statement,
};
use crate::span::Span;
use crate::syntax::ast::{BinaryOperator, Snapshot, UnaryOperator};
use crate::types::{Address, Value};

/// How deep function calls may nest. Checking rejects recursion, so only a
/// program that failed checking gets here.
const CALL_DEPTH: usize = 64;

/// Runs the behaviors `check` selects, or, without one, every behavior that
/// takes no parameters, in the model's order.
pub fn run_check(
    model: &Model,
    check: Option<&Check>,
    world: &mut dyn World,
) -> Vec<BehaviorResult> {
    let behaviors: HashMap<&Address, &Behavior> = model
        .behaviors
        .iter()
        .map(|behavior| (&behavior.address, behavior))
        .collect();
    let Some(check) = check else {
        return model
            .behaviors
            .iter()
            .filter(|behavior| behavior.parameters.is_empty())
            .map(|behavior| run_behavior(model, behavior, Vec::new(), world))
            .collect();
    };
    let mut results = Vec::new();
    for run in &check.runs {
        let Some(behavior) = behaviors.get(&run.requirement) else {
            continue;
        };
        let names = behavior.parameters_names();
        let arguments = Interpreter::new(model, world, &run.requirement.module).arguments(
            &names,
            &run.arguments,
            &Env::default(),
        );
        // Arguments come from constants and lookups; nothing in them can
        // end a case, but an unknown argument makes the behavior unknown.
        let arguments = arguments.unwrap_or_else(|ending| {
            let why = ending.into_problem().message;
            names
                .iter()
                .map(|name| (name.clone(), Datum::Unknown(why.clone())))
                .collect()
        });
        let mut result = run_behavior(model, behavior, arguments, world);
        if let Some(severity) = run.severity {
            result.severity = severity;
        }
        results.push(result);
    }
    results
}

/// Runs one behavior with its arguments.
pub fn run_behavior(
    model: &Model,
    behavior: &Behavior,
    arguments: Vec<(String, Datum)>,
    world: &mut dyn World,
) -> BehaviorResult {
    let identity = behavior_identity(&behavior.address, &arguments);
    let mut interpreter = Interpreter::new(model, world, &behavior.address.module);
    let env = Env { locals: arguments };
    interpreter.run_case(&identity, Vec::new(), env, &behavior.body);
    let Interpreter {
        cases,
        expects_ran,
        incomplete,
        evidence,
        ..
    } = interpreter;
    let mut outcome = cases
        .iter()
        .map(|case| case.outcome)
        .max()
        .unwrap_or(Outcome::Satisfied);
    let mut notes = Vec::new();
    if expects_ran == 0 && !behavior.allows_empty {
        notes.push("no assertion ran".to_owned());
        outcome = outcome.max(Outcome::Unknown);
    }
    for why in incomplete {
        notes.push(format!("a loop read incompletely covered facts: {why}"));
        outcome = outcome.max(Outcome::Unknown);
    }
    BehaviorResult {
        address: behavior.address.clone(),
        identity,
        severity: behavior.severity,
        outcome,
        cases,
        notes,
        evidence,
    }
}

impl Behavior {
    fn parameters_names(&self) -> Vec<String> {
        self.parameters
            .iter()
            .map(|column| column.name.clone())
            .collect()
    }
}

fn behavior_identity(address: &Address, arguments: &[(String, Datum)]) -> String {
    if arguments.is_empty() {
        return address.to_string();
    }
    let written: Vec<String> = arguments
        .iter()
        .map(|(name, value)| format!("{name}={value}"))
        .collect();
    format!("{address}({})", written.join(", "))
}

/// Local variables, innermost last.
#[derive(Debug, Clone, Default)]
struct Env {
    locals: Vec<(String, Datum)>,
}

impl Env {
    fn get(&self, name: &str) -> Option<&Datum> {
        self.locals
            .iter()
            .rev()
            .find(|(local, _)| local == name)
            .map(|(_, value)| value)
    }

    fn with(&self, bindings: impl IntoIterator<Item = (String, Datum)>) -> Env {
        let mut extended = self.clone();
        extended.locals.extend(bindings);
        extended
    }
}

/// Why a case ended before its last statement.
enum Ending {
    Violated(Box<Problem>),
    Unknown(Box<Problem>),
    Stale(Box<Problem>),
}

impl Ending {
    fn violated(problem: Problem) -> Self {
        Ending::Violated(Box::new(problem))
    }

    fn unknown(problem: Problem) -> Self {
        Ending::Unknown(Box::new(problem))
    }

    fn stale(problem: Problem) -> Self {
        Ending::Stale(Box::new(problem))
    }

    fn outcome(&self) -> Outcome {
        match self {
            Ending::Violated(_) => Outcome::Violated,
            Ending::Unknown(_) => Outcome::Unknown,
            Ending::Stale(_) => Outcome::Stale,
        }
    }

    fn into_problem(self) -> Problem {
        match self {
            Ending::Violated(problem) | Ending::Unknown(problem) | Ending::Stale(problem) => {
                *problem
            }
        }
    }
}

type Flow = Result<(), Ending>;

struct Interpreter<'m, 'w> {
    world: &'w mut dyn World,
    module: String,
    functions: HashMap<&'m Address, &'m Function>,
    checkers: HashMap<&'m Address, &'m Checker>,
    cases: Vec<CaseResult>,
    case: String,
    expects_ran: usize,
    incomplete: Vec<String>,
    evidence: Vec<EvidenceUse>,
    depth: usize,
}

impl<'m, 'w> Interpreter<'m, 'w> {
    fn new(model: &'m Model, world: &'w mut dyn World, module: &str) -> Self {
        Self {
            world,
            module: module.to_owned(),
            functions: model
                .functions
                .iter()
                .map(|function| (&function.address, function))
                .collect(),
            checkers: model
                .checkers
                .iter()
                .map(|checker| (&checker.address, checker))
                .collect(),
            cases: Vec::new(),
            case: String::new(),
            expects_ran: 0,
            incomplete: Vec::new(),
            evidence: Vec::new(),
            depth: 0,
        }
    }

    /// Runs one case and records it before the cases nested in it.
    fn run_case(
        &mut self,
        identity: &str,
        values: Vec<(String, Datum)>,
        mut env: Env,
        statements: &[Statement],
    ) {
        let index = self.cases.len();
        self.cases.push(CaseResult {
            identity: identity.to_owned(),
            values,
            outcome: Outcome::Satisfied,
            problem: None,
        });
        let enclosing = std::mem::replace(&mut self.case, identity.to_owned());
        let flow = self.block(&mut env, statements);
        self.case = enclosing;
        if let Err(ending) = flow {
            let case = &mut self.cases[index];
            case.outcome = ending.outcome();
            case.problem = Some(ending.into_problem());
        }
    }

    fn block(&mut self, env: &mut Env, statements: &[Statement]) -> Flow {
        let depth = env.locals.len();
        for statement in statements {
            let flow = self.statement(env, statement);
            if flow.is_err() {
                env.locals.truncate(depth);
                return flow;
            }
        }
        env.locals.truncate(depth);
        Ok(())
    }

    fn statement(&mut self, env: &mut Env, statement: &Statement) -> Flow {
        match statement {
            Statement::Let { name, value } => {
                let value = self.evaluate(env, value)?;
                env.locals.push((name.clone(), value));
                Ok(())
            }
            Statement::Expect {
                condition,
                because,
                span,
                text,
            } => {
                self.expects_ran += 1;
                let written = Written { span: *span, text };
                self.expect(env, condition, because.as_ref(), written)
            }
            // A behavior has no `return`; checking reports one.
            Statement::Return(_) => Ok(()),
            Statement::If {
                condition,
                then,
                otherwise,
                span,
            } => {
                let truth = self.evaluate(env, condition)?.truth();
                match truth {
                    Truth::True => self.block(env, then),
                    Truth::False => self.block(env, otherwise),
                    Truth::Missing(name) => Err(Ending::violated(self.problem(
                        *span,
                        "if",
                        format!("the condition reads `{name}`, which is missing"),
                    ))),
                    Truth::Unknown(why) => Err(Ending::unknown(self.problem(
                        *span,
                        "if",
                        format!("the condition is unknown: {why}"),
                    ))),
                }
            }
            Statement::For {
                pattern,
                domain,
                body,
                span,
            } => {
                let written = Loop {
                    pattern,
                    domain,
                    body,
                    span: *span,
                };
                self.for_cases(env, written)
            }
        }
    }

    fn expect(
        &mut self,
        env: &Env,
        condition: &Expression,
        because: Option<&Expression>,
        written: Written<'_>,
    ) -> Flow {
        let (truth, operands) = self.condition_with_operands(env, condition)?;
        let because = match because
            .map(|reason| self.evaluate(env, reason))
            .transpose()?
        {
            Some(Datum::Text(reason)) => Some(reason),
            _ => None,
        };
        let problem = |message: String| Problem {
            module: self.module.clone(),
            span: written.span,
            text: written.text.to_owned(),
            message,
            operands: operands.clone(),
            because: because.clone(),
        };
        match truth {
            Truth::True => Ok(()),
            Truth::False => Err(Ending::violated(problem(format!(
                "`{}` does not hold",
                written.text
            )))),
            Truth::Missing(name) => Err(Ending::violated(problem(format!(
                "`{}`: `{name}` is missing",
                written.text
            )))),
            Truth::Unknown(why) => Err(Ending::unknown(problem(format!(
                "`{}` is unknown: {why}",
                written.text
            )))),
        }
    }

    /// A condition's truth and, for a comparison, each operand's value, so
    /// a failure can say what it compared.
    fn condition_with_operands(
        &mut self,
        env: &Env,
        condition: &Expression,
    ) -> Result<(Truth, Vec<(String, String)>), Ending> {
        let ExpressionKind::Binary {
            operator,
            left,
            right,
        } = &condition.kind
        else {
            return Ok((self.evaluate(env, condition)?.truth(), Vec::new()));
        };
        if matches!(operator, BinaryOperator::And | BinaryOperator::Or) {
            return Ok((self.evaluate(env, condition)?.truth(), Vec::new()));
        }
        let left_value = self.evaluate(env, left)?;
        let right_value = self.evaluate(env, right)?;
        let truth = binary(*operator, &left_value, &right_value).truth();
        let operands = vec![
            (operand_label(left), left_value.to_string()),
            (operand_label(right), right_value.to_string()),
        ];
        Ok((truth, operands))
    }

    fn for_cases(&mut self, env: &Env, written: Loop<'_>) -> Flow {
        let Loop {
            pattern,
            domain,
            body,
            span,
        } = written;
        let items: Vec<Vec<Datum>> = match self.evaluate(env, domain)? {
            Datum::List(items) => items.into_iter().map(|item| vec![item]).collect(),
            Datum::Facts(predicate, snapshot) => {
                let facts = self.world.facts(&predicate, snapshot);
                if let Some(why) = facts.incomplete {
                    self.incomplete.push(why);
                }
                facts.rows.into_iter().map(|row| row.values).collect()
            }
            Datum::Missing(absence) => {
                return Err(Ending::violated(self.problem(
                    span,
                    "for",
                    format!(
                        "the loop's domain reads `{}`, which is missing",
                        absence.name
                    ),
                )));
            }
            other => {
                return Err(Ending::unknown(self.problem(
                    span,
                    "for",
                    format!("the loop's domain is not a list or a relation: {other}"),
                )));
            }
        };
        let enclosing = self.case.clone();
        let outer_values = self
            .cases
            .iter()
            .find(|case| case.identity == enclosing)
            .map(|case| case.values.clone())
            .unwrap_or_default();
        for item in items {
            let bindings: Vec<(String, Datum)> = pattern.iter().cloned().zip(item).collect();
            let identity = case_identity(&enclosing, &bindings);
            let mut values = outer_values.clone();
            values.extend(bindings.iter().cloned());
            self.run_case(&identity, values, env.with(bindings), body);
        }
        Ok(())
    }

    fn problem(&self, span: Span, text: &str, message: String) -> Problem {
        Problem {
            module: self.module.clone(),
            span,
            text: text.to_owned(),
            message,
            operands: Vec::new(),
            because: None,
        }
    }

    fn evaluate(&mut self, env: &Env, expression: &Expression) -> Result<Datum, Ending> {
        let value = match &expression.kind {
            ExpressionKind::Constant(value) => Datum::from_value(value),
            ExpressionKind::Local(name) => env
                .get(name)
                .cloned()
                .unwrap_or_else(|| Datum::Unknown(format!("`{name}` has no value"))),
            ExpressionKind::Facts(predicate) => Datum::Facts(predicate.clone(), Snapshot::Head),
            ExpressionKind::Callable(_) => {
                Datum::Unknown("a function is called, not used as a value".to_owned())
            }
            ExpressionKind::List(items) => Datum::List(
                items
                    .iter()
                    .map(|item| self.evaluate(env, item))
                    .collect::<Result<_, _>>()?,
            ),
            ExpressionKind::Record(fields) => Datum::Record(
                fields
                    .iter()
                    .map(|(name, value)| Ok((name.clone(), self.evaluate(env, value)?)))
                    .collect::<Result<_, Ending>>()?,
            ),
            ExpressionKind::Member { target, name } => member(&self.evaluate(env, target)?, name),
            ExpressionKind::Index { target, keys } => {
                let target = self.evaluate(env, target)?;
                let keys = keys
                    .iter()
                    .map(|key| self.evaluate(env, key))
                    .collect::<Result<Vec<_>, _>>()?;
                self.index(&target, keys)
            }
            ExpressionKind::Call { callee, arguments } => self.call(env, callee, arguments)?,
            ExpressionKind::Method {
                receiver,
                name,
                arguments,
            } => {
                let receiver = self.evaluate(env, receiver)?;
                let arguments = arguments
                    .iter()
                    .map(|argument| self.evaluate(env, argument))
                    .collect::<Result<Vec<_>, _>>()?;
                method(&receiver, name, &arguments)
            }
            ExpressionKind::Unary { operator, operand } => {
                let operand_value = self.evaluate(env, operand)?;
                self.unary(*operator, operand_value, expression.span)?
            }
            ExpressionKind::Binary {
                operator,
                left,
                right,
            } => self.binary_expression(env, *operator, left, right)?,
            ExpressionKind::Snapshot { target, snapshot } => match self.evaluate(env, target)? {
                Datum::Facts(predicate, _) => Datum::Facts(predicate, *snapshot),
                other => other,
            },
            ExpressionKind::Invalid => Datum::Unknown("this expression failed checking".to_owned()),
        };
        Ok(value)
    }

    /// `&&` and `||` look at their right side only when the left does not
    /// decide (spec 13, section 5).
    fn binary_expression(
        &mut self,
        env: &Env,
        operator: BinaryOperator,
        left: &Expression,
        right: &Expression,
    ) -> Result<Datum, Ending> {
        let left_value = self.evaluate(env, left)?;
        let decided = match (operator, left_value.truth()) {
            (BinaryOperator::And, Truth::False) => Some(Datum::Bool(false)),
            (BinaryOperator::Or, Truth::True) => Some(Datum::Bool(true)),
            _ => None,
        };
        if let Some(value) = decided {
            return Ok(value);
        }
        let right_value = self.evaluate(env, right)?;
        Ok(binary(operator, &left_value, &right_value))
    }

    fn unary(
        &mut self,
        operator: UnaryOperator,
        operand: Datum,
        span: Span,
    ) -> Result<Datum, Ending> {
        let value = match (operator, operand) {
            (_, absent) if absent.is_absent() && operator != UnaryOperator::Need => absent,
            (UnaryOperator::Not, Datum::Bool(value)) => Datum::Bool(!value),
            (UnaryOperator::Negate, Datum::Int(value)) => value.checked_neg().map_or_else(
                || Datum::Unknown("the negation overflows".to_owned()),
                Datum::Int,
            ),
            (UnaryOperator::Need, operand) => return self.need(operand, span),
            (_, other) => Datum::Unknown(format!("{other} has the wrong type for this operator")),
        };
        Ok(value)
    }

    /// The evidence's value, or the end of the case when there is none.
    fn need(&mut self, operand: Datum, span: Span) -> Result<Datum, Ending> {
        let Datum::Evidence(evidence) = operand else {
            let message = format!("`need` takes evidence, not {operand}");
            return Err(Ending::unknown(self.problem(span, "need", message)));
        };
        match *evidence {
            Evidence::Observed { value, .. } => Ok(value),
            Evidence::Unavailable(why) => Err(Ending::unknown(self.problem(
                span,
                "need",
                format!("the evidence is unavailable: {why}"),
            ))),
            Evidence::Stale(why) => Err(Ending::stale(self.problem(
                span,
                "need",
                format!("the stored evidence is stale: {why}"),
            ))),
        }
    }

    fn index(&mut self, target: &Datum, keys: Vec<Datum>) -> Datum {
        match target {
            Datum::Facts(predicate, snapshot) => self.lookup(predicate, *snapshot, keys),
            Datum::List(items) => match keys.as_slice() {
                [Datum::Int(position)] => usize::try_from(*position)
                    .ok()
                    .and_then(|position| items.get(position))
                    .cloned()
                    .unwrap_or_else(|| {
                        Datum::Missing(Absence {
                            name: format!("[{position}]"),
                        })
                    }),
                _ => Datum::Unknown("a list is indexed by one integer".to_owned()),
            },
            Datum::Record(_) => match keys.as_slice() {
                [Datum::Text(name)] => member(target, name),
                _ => Datum::Unknown("an object is indexed by one field name".to_owned()),
            },
            absent if absent.is_absent() => absent.clone(),
            other => Datum::Unknown(format!("{other} cannot be indexed")),
        }
    }

    /// One fact of a relation: found, absent, or unknown when absent from a
    /// relation that is not completely covered.
    fn lookup(&mut self, predicate: &Predicate, snapshot: Snapshot, key: Vec<Datum>) -> Datum {
        if let Some(absent) = key.iter().find(|value| value.is_absent()) {
            return absent.clone();
        }
        let facts = self.world.facts(predicate, snapshot);
        let found = facts.rows.iter().find(|row| {
            row.values.len() == key.len()
                && row
                    .values
                    .iter()
                    .zip(&key)
                    .all(|(value, wanted)| value.same_as(wanted))
        });
        let (exists, supports) = match (found, &facts.incomplete) {
            (Some(row), _) => (Ok(true), row.supports.clone()),
            (None, Some(why)) => (Err(why.clone()), Vec::new()),
            (None, None) => (Ok(false), Vec::new()),
        };
        Datum::Fact(Box::new(FactDatum {
            predicate: predicate.clone(),
            key,
            columns: facts.columns,
            exists,
            supports,
        }))
    }

    fn call(
        &mut self,
        env: &Env,
        callee: &Expression,
        arguments: &[CallArgument],
    ) -> Result<Datum, Ending> {
        let ExpressionKind::Callable(callable) = &callee.kind else {
            return Ok(Datum::Unknown(
                "only functions and checkers are called".to_owned(),
            ));
        };
        match callable {
            Callable::Function(address) => {
                let Some(function) = self.functions.get(address).copied() else {
                    return Ok(Datum::Unknown(format!("there is no function `{address}`")));
                };
                let names: Vec<String> = function
                    .parameters
                    .iter()
                    .map(|column| column.name.clone())
                    .collect();
                let given = self.arguments(&names, arguments, env)?;
                Ok(self.call_function(function, given))
            }
            Callable::Checker(address) => {
                let Some(checker) = self.checkers.get(address).copied() else {
                    return Ok(Datum::Unknown(format!("there is no checker `{address}`")));
                };
                let names: Vec<String> = checker
                    .parameters
                    .iter()
                    .map(|column| column.name.clone())
                    .collect();
                let given = self.arguments(&names, arguments, env)?;
                Ok(self.demand(checker, given))
            }
            Callable::Entity(address) => {
                let mut given = Vec::new();
                for argument in arguments {
                    let value = self.evaluate(env, &argument.value)?;
                    given.push((argument.name.clone().unwrap_or_default(), value));
                }
                Ok(entity(address, given))
            }
        }
    }

    /// Arguments by parameter name, in `parameters`' order: positional ones
    /// fill parameters in order, named ones fill theirs.
    fn arguments(
        &mut self,
        parameters: &[String],
        arguments: &[CallArgument],
        env: &Env,
    ) -> Result<Vec<(String, Datum)>, Ending> {
        let mut given: Vec<(String, Datum)> = Vec::new();
        for (position, argument) in arguments.iter().enumerate() {
            let name = match &argument.name {
                Some(name) => name.clone(),
                None => parameters.get(position).cloned().unwrap_or_default(),
            };
            given.push((name, self.evaluate(env, &argument.value)?));
        }
        Ok(parameters
            .iter()
            .filter_map(|parameter| given.iter().find(|(name, _)| name == parameter).cloned())
            .collect())
    }

    fn call_function(&mut self, function: &Function, arguments: Vec<(String, Datum)>) -> Datum {
        if self.depth >= CALL_DEPTH {
            return Datum::Unknown(format!("`{}` calls itself", function.address.name()));
        }
        self.depth += 1;
        let mut env = Env { locals: arguments };
        let returned = self.function_block(&mut env, &function.body);
        self.depth -= 1;
        returned.unwrap_or_else(|| {
            Datum::Unknown(format!("`{}` returned nothing", function.address.name()))
        })
    }

    /// Runs a function's statements; `Some` once one returns, or once a
    /// condition it needs is missing or unknown, which it returns.
    fn function_block(&mut self, env: &mut Env, statements: &[Statement]) -> Option<Datum> {
        let depth = env.locals.len();
        let mut returned = None;
        for statement in statements {
            returned = self.function_statement(env, statement);
            if returned.is_some() {
                break;
            }
        }
        env.locals.truncate(depth);
        returned
    }

    fn function_statement(&mut self, env: &mut Env, statement: &Statement) -> Option<Datum> {
        let value = |interpreter: &mut Self, env: &Env, expression: &Expression| {
            interpreter
                .evaluate(env, expression)
                .unwrap_or_else(|ending| Datum::Unknown(ending.into_problem().message))
        };
        match statement {
            Statement::Let {
                name,
                value: written,
            } => {
                let evaluated = value(self, env, written);
                env.locals.push((name.clone(), evaluated));
                None
            }
            Statement::Return(written) => Some(value(self, env, written)),
            Statement::If {
                condition,
                then,
                otherwise,
                ..
            } => match value(self, env, condition).truth() {
                Truth::True => self.function_block(env, then),
                Truth::False => self.function_block(env, otherwise),
                absent => Some(Datum::from(absent)),
            },
            Statement::For {
                pattern,
                domain,
                body,
                ..
            } => {
                let Datum::List(items) = value(self, env, domain) else {
                    return Some(Datum::Unknown(
                        "a function's loop iterates a list".to_owned(),
                    ));
                };
                items.into_iter().find_map(|item| {
                    let bindings = pattern.iter().cloned().zip(std::iter::once(item));
                    self.function_block(&mut env.with(bindings), body)
                })
            }
            Statement::Expect { .. } => None,
        }
    }

    /// Asks the world for a checker's evidence; arguments that are missing
    /// or unknown make it unavailable without asking.
    fn demand(&mut self, checker: &Checker, arguments: Vec<(String, Datum)>) -> Datum {
        let absent = arguments.iter().find(|(_, value)| value.is_absent());
        let evidence = if let Some((name, value)) = absent {
            Evidence::Unavailable(format!("its argument `{name}` is {value}"))
        } else {
            let case = self.case.clone();
            let demand = Demand {
                checker,
                arguments,
                case: &case,
            };
            self.world.evidence(&demand)
        };
        self.evidence.push(EvidenceUse {
            checker: checker.address.clone(),
            case: self.case.clone(),
            key: match &evidence {
                Evidence::Observed { key, .. } => Some(key.clone()),
                _ => None,
            },
            was_reused: matches!(
                evidence,
                Evidence::Observed {
                    was_reused: true,
                    ..
                }
            ),
            unavailable: match &evidence {
                Evidence::Unavailable(why) | Evidence::Stale(why) => Some(why.clone()),
                Evidence::Observed { .. } => None,
            },
        });
        Datum::Evidence(Box::new(evidence))
    }
}

/// A `for` statement's parts.
#[derive(Clone, Copy)]
struct Loop<'s> {
    pattern: &'s [String],
    domain: &'s Expression,
    body: &'s [Statement],
    span: Span,
}

/// An `expect` statement's place and text.
#[derive(Clone, Copy)]
struct Written<'t> {
    span: Span,
    text: &'t str,
}

/// How a failed comparison names an operand: a local's name, or the kind
/// of expression.
fn operand_label(expression: &Expression) -> String {
    match &expression.kind {
        ExpressionKind::Local(name) => name.clone(),
        ExpressionKind::Member { target, name } => format!("{}.{name}", operand_label(target)),
        ExpressionKind::Constant(_) => "expected".to_owned(),
        _ => "value".to_owned(),
    }
}

fn case_identity(enclosing: &str, bindings: &[(String, Datum)]) -> String {
    let written: Vec<String> = bindings
        .iter()
        .map(|(name, value)| format!("{name}={value}"))
        .collect();
    format!("{enclosing}[{}]", written.join(", "))
}

/// `target.name`: a field, a fact's column or existence, or missing.
fn member(target: &Datum, name: &str) -> Datum {
    match target {
        Datum::Record(_) => target.field(name).cloned().unwrap_or_else(|| {
            Datum::Missing(Absence {
                name: name.to_owned(),
            })
        }),
        Datum::Fact(fact) => fact_member(fact, name),
        Datum::Declared(Value::Entity { fields, .. }) => {
            fields.iter().find(|(field, _)| field == name).map_or_else(
                || {
                    Datum::Missing(Absence {
                        name: name.to_owned(),
                    })
                },
                |(_, value)| Datum::from_value(value),
            )
        }
        absent if absent.is_absent() => absent.clone(),
        other => Datum::Unknown(format!("{other} has no `{name}`")),
    }
}

fn fact_member(fact: &FactDatum, name: &str) -> Datum {
    match name {
        "exists" => match &fact.exists {
            Ok(exists) => Datum::Bool(*exists),
            Err(why) => Datum::Unknown(why.clone()),
        },
        "supports" => Datum::List(fact.supports.iter().cloned().map(Datum::Text).collect()),
        column => {
            let position = fact.columns.iter().position(|known| known == column);
            match (position, &fact.exists) {
                (Some(position), Ok(true)) => fact
                    .key
                    .get(position)
                    .cloned()
                    .unwrap_or_else(|| Datum::Unknown(format!("the fact has no `{column}`"))),
                (_, Err(why)) => Datum::Unknown(why.clone()),
                _ => Datum::Unknown(format!("there is no such fact to read `{column}` from")),
            }
        }
    }
}

fn method(receiver: &Datum, name: &str, arguments: &[Datum]) -> Datum {
    match (name, receiver, arguments) {
        ("has", Datum::Unknown(why), _) => Datum::Unknown(why.clone()),
        ("has", _, [Datum::Text(field)]) => Datum::Bool(receiver.field(field).is_some()),
        _ => Datum::Unknown(format!("there is no method `{name}` for {receiver}")),
    }
}

fn entity(address: &Address, fields: Vec<(String, Datum)>) -> Datum {
    let values: Option<Vec<(String, Value)>> = fields
        .into_iter()
        .map(|(name, value)| Some((name, value.to_value()?)))
        .collect();
    match values {
        Some(fields) => Datum::Declared(Value::Entity {
            entity: address.clone(),
            fields,
        }),
        None => Datum::Unknown(format!("a field of `{}` is unknown", address.name())),
    }
}

/// A binary operator's value; missing and unknown operands pass on,
/// unknown before missing (spec 13, section 5).
fn binary(operator: BinaryOperator, left: &Datum, right: &Datum) -> Datum {
    for side in [left, right] {
        if let Datum::Unknown(why) = side {
            return Datum::Unknown(why.clone());
        }
    }
    for side in [left, right] {
        if let Datum::Missing(absence) = side {
            return Datum::Missing(absence.clone());
        }
    }
    match operator {
        BinaryOperator::And | BinaryOperator::Or => logical(operator, left, right),
        BinaryOperator::Equal
        | BinaryOperator::NotEqual
        | BinaryOperator::Less
        | BinaryOperator::LessOrEqual
        | BinaryOperator::Greater
        | BinaryOperator::GreaterOrEqual => {
            let is_ordering = !matches!(operator, BinaryOperator::Equal | BinaryOperator::NotEqual);
            let are_integers = matches!((left, right), (Datum::Int(_), Datum::Int(_)));
            if is_ordering && !are_integers {
                return Datum::Unknown(format!("{left} and {right} are not both integers"));
            }
            Datum::Bool(compares(left, operator, right))
        }
        _ => arithmetic(operator, left, right),
    }
}

fn logical(operator: BinaryOperator, left: &Datum, right: &Datum) -> Datum {
    match (left, right) {
        (Datum::Bool(left), Datum::Bool(right)) => Datum::Bool(match operator {
            BinaryOperator::And => *left && *right,
            _ => *left || *right,
        }),
        _ => Datum::Unknown(format!("{left} and {right} are not both booleans")),
    }
}

fn arithmetic(operator: BinaryOperator, left: &Datum, right: &Datum) -> Datum {
    if let (BinaryOperator::Add, Datum::Text(left), Datum::Text(right)) = (operator, left, right) {
        return Datum::Text(format!("{left}{right}"));
    }
    let (Datum::Int(left), Datum::Int(right)) = (left, right) else {
        return Datum::Unknown(format!("{left} and {right} are not both integers"));
    };
    let result = match operator {
        BinaryOperator::Add => left.checked_add(*right),
        BinaryOperator::Subtract => left.checked_sub(*right),
        BinaryOperator::Multiply => left.checked_mul(*right),
        BinaryOperator::Divide => left.checked_div(*right),
        BinaryOperator::Remainder => left.checked_rem(*right),
        _ => None,
    };
    result.map_or_else(
        || Datum::Unknown("the arithmetic overflows or divides by zero".to_owned()),
        Datum::Int,
    )
}
