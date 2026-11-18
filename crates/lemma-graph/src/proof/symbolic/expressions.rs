//! Expressions: reads, operators, literals and formatted strings.

use std::collections::BTreeMap;

use lemma_lang::syntax::ast::BinaryOperator as Op;

use super::super::ir::{self, Expression as Expr, Language, Part};
use super::calls::is_generator;
use super::models::assume_decorators;
use super::scopes::captured;
use super::value::{
    Absent, Defined, Missing, Sort, Term, Value, arithmetic, contains, element, equal, length,
    negate, order, read_field, to_text,
};
use super::{Evaluator, State, is_pure, lookup};

impl Evaluator<'_> {
    pub(super) fn expression(
        &mut self,
        expression: &Expr,
        state: &mut State,
    ) -> Result<Value, String> {
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
            Expr::Subscript(target, key) => self.subscript(target, key, state)?,
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
            Expr::Sequence(items) => Value::Sequence(self.elements(items, state)?),
            Expr::Spread(_) => return Err("a spread belongs inside a literal".to_owned()),
            Expr::Merge(entries) => self.merge(entries, state)?,
            Expr::New(constructor, arguments) => {
                self.call_model(&format!("new {constructor}"), arguments, false, state)?
            }
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
            Expr::Closure(function) => {
                assume_decorators(&function.decorators, self.contract, &mut self.assumptions)?;
                Value::Closure(std::rc::Rc::new(Defined {
                    captured: captured(function, state),
                    defaults: self.defaults(function, state),
                    function: (**function).clone(),
                }))
            }
            Expr::Await(inner) => self.awaited(inner, state)?,
            Expr::Comprehension(comprehension) => self.comprehension(comprehension, state)?,
        })
    }

    /// The items of a sequence literal, with spread sequences flattened in.
    pub(super) fn elements(
        &mut self,
        items: &[Expr],
        state: &mut State,
    ) -> Result<Vec<Value>, String> {
        let mut values = Vec::new();
        for item in items {
            let Expr::Spread(inner) = item else {
                values.push(self.expression(item, state)?);
                continue;
            };
            let Value::Sequence(spread) = self.expression(inner, state)? else {
                return Err("only a sequence of known length can be spread".to_owned());
            };
            values.extend(spread);
        }
        Ok(values)
    }

    /// An object literal with spreads, read in order: later parts replace
    /// earlier fields. Spreading a record a contract declared leaves fields
    /// it never named, so the result is open too.
    pub(super) fn merge(
        &mut self,
        entries: &[ir::Entry],
        state: &mut State,
    ) -> Result<Value, String> {
        let mut fields = BTreeMap::new();
        let mut is_exact = true;
        for entry in entries {
            match entry {
                ir::Entry::Field(key, value) => {
                    let value = self.expression(value, state)?;
                    fields.insert(key.clone(), value);
                }
                ir::Entry::Spread(value) => match self.expression(value, state)? {
                    Value::Object(spread) => fields.extend(spread),
                    Value::Record(spread) => {
                        fields.extend(spread);
                        is_exact = false;
                    }
                    // JavaScript spreads nothing from null; Python raises.
                    Value::Null | Value::Undefined if self.language == Language::TypeScript => {}
                    Value::Null | Value::Undefined => self.raise_here("TypeError", state),
                    _ => {
                        return Err(
                            "only an object or record can be spread into an object".to_owned()
                        );
                    }
                },
            }
        }
        Ok(if is_exact {
            Value::Object(fields)
        } else {
            Value::Record(fields)
        })
    }

    pub(super) fn expressions(
        &mut self,
        items: &[Expr],
        state: &mut State,
    ) -> Result<Vec<Value>, String> {
        items
            .iter()
            .map(|item| self.expression(item, state))
            .collect()
    }

    /// What reading a field an object lacks does here: a contract counts it
    /// as undefined, the source as its language does.
    pub(super) fn missing(&self) -> Missing {
        if self.specification.is_some() {
            return Missing::Raises;
        }
        Missing::of(self.language)
    }

    /// What a field or element is read from: reading a part of a variable
    /// does not let anything else see the variable's object.
    pub(super) fn target(&mut self, target: &Expr, state: &mut State) -> Result<Value, String> {
        if let Expr::Name(name) = target {
            return lookup(name, state);
        }
        self.expression(target, state)
    }

    /// `test ? then : otherwise`; only the branch the test picks is reached.
    pub(super) fn conditional(
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

    pub(super) fn awaited(&mut self, inner: &Expr, state: &mut State) -> Result<Value, String> {
        if !self.is_async {
            return Err("`await` outside an async function".to_owned());
        }
        let Expr::Call(callee, arguments) = inner else {
            return Err("only a call can be awaited".to_owned());
        };
        self.call(callee, arguments, true, state)
    }

    pub(super) fn element(
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
    pub(super) fn field(
        &mut self,
        target: &Expr,
        name: &str,
        is_optional: bool,
        state: &mut State,
    ) -> Result<Value, String> {
        let target = self.target(target, state)?;
        if self.specification.is_none()
            && self.language == Language::Python
            && holds_object(&target)
        {
            return Err(format!(
                "`.{name}` reads an attribute of a dict, which holds its entries under keys"
            ));
        }
        // A sequence has a `length` in JavaScript, and in a contract.
        let reads_length = name == "length"
            && (self.specification.is_some() || self.language == Language::TypeScript);
        let absent = if is_optional {
            Absent::IsUndefined
        } else {
            Absent::Raises
        };
        let counted = if reads_length {
            length(&target, absent)?
        } else {
            None
        };
        let (value, fails) = match counted {
            Some(counted) => counted,
            None => read_field(&target, name, is_optional, self.missing())?,
        };
        self.narrow(&fails, state);
        Ok(value)
    }

    /// `target[key]` in Python and Go: the entry of a dict or a declared
    /// record under a constant key.
    pub(super) fn subscript(
        &mut self,
        target: &Expr,
        key: &Expr,
        state: &mut State,
    ) -> Result<Value, String> {
        let target = self.target(target, state)?;
        let key = self.expression(key, state)?;
        let name = match &key {
            Value::Scalar(term) => term.constant_text(),
            _ => None,
        };
        let Some(name) = name else {
            return Err("a subscript is read with a constant key".to_owned());
        };
        let (value, fails) = read_field(&target, &name, false, self.missing())?;
        self.narrow(&fails, state);
        Ok(value)
    }

    /// Python's and Go's built-in `len`, of a sequence of known length or
    /// of a Python text. Go counts a string's bytes, which are not known.
    pub(super) fn built_in_length(
        &mut self,
        arguments: &[ir::Argument],
        state: &mut State,
    ) -> Result<Value, String> {
        let [argument] = arguments else {
            return Err("`len` takes one argument".to_owned());
        };
        if argument.name.is_some() {
            return Err("`len` takes no keyword argument".to_owned());
        }
        if is_generator(&argument.value) {
            return Err(
                "`len` of a generator expression is unsupported: Python raises there".to_owned(),
            );
        }
        let value = self.expression(&argument.value, state)?;
        if let (Value::Scalar(text), Language::Python) = (&value, self.language) {
            if text.sort == Sort::Text {
                return Ok(Value::Scalar(Term {
                    sort: Sort::Integer,
                    code: format!("(str.len {})", text.code),
                }));
            }
        }
        let absent = if self.language == Language::Go {
            Absent::IsUnsupported
        } else {
            Absent::Raises
        };
        let Some((count, fails)) = length(&value, absent)? else {
            return Err("`len` is read of a sequence of known length".to_owned());
        };
        self.narrow(&fails, state);
        Ok(count)
    }

    /// `left ?? right`: `right` where `left` is null or undefined.
    pub(super) fn coalesce(
        &mut self,
        left: &Expr,
        right: &Expr,
        state: &mut State,
    ) -> Result<Value, String> {
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

    pub(super) fn binary(
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
    pub(super) fn template_text(
        &mut self,
        parts: &[Part],
        state: &mut State,
    ) -> Result<Value, String> {
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

    pub(super) fn arithmetic(
        &mut self,
        operator: Op,
        left: &Value,
        right: &Value,
    ) -> Result<Term, String> {
        let term = arithmetic(operator, left, right)?;
        if term.sort == Sort::Integer {
            self.assume_integer_arithmetic();
        }
        Ok(term)
    }

    /// Source integer arithmetic is exact only within the language's range:
    /// TypeScript numbers lose precision past 2^53, Go integers wrap.
    pub(super) fn assume_integer_arithmetic(&mut self) {
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
}

/// Whether `value` may be an object the checked code built from a literal:
/// in Python a dict, whose entries are not attributes.
fn holds_object(value: &Value) -> bool {
    match value {
        Value::Object(_) => true,
        Value::Choice(_, then, otherwise) => holds_object(then) || holds_object(otherwise),
        _ => false,
    }
}

/// Where `value` is null or undefined.
pub(super) fn absent(value: &Value) -> Term {
    match value {
        Value::Null | Value::Undefined => Term::boolean(true),
        Value::Choice(test, then, otherwise) => test
            .and(&absent(then))
            .or(&test.not().and(&absent(otherwise))),
        _ => Term::boolean(false),
    }
}

/// `value`, with `fallback` wherever it is null or undefined.
pub(super) fn fill_absent(value: Value, fallback: &Value) -> Value {
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
