//! Symbolic values, and the operations every supported language agrees on.
//!
//! A value is a solver term, a record, null or undefined, or a choice between
//! two values decided by a term: what a source value is when its shape
//! depends on the inputs, such as a text that may be `None`.

use std::collections::BTreeMap;
use std::fmt::Write as _;

use lemma_lang::syntax::ast::BinaryOperator as Op;

use super::super::ir::Language;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(in crate::proof) enum Sort {
    Bool,
    Text,
    Integer,
}

impl Sort {
    pub(in crate::proof) fn smt(self) -> &'static str {
        match self {
            Self::Bool => "Bool",
            Self::Text => "String",
            Self::Integer => "Int",
        }
    }
}

/// An SMT-LIB term and its sort.
#[derive(Debug, Clone)]
pub(in crate::proof) struct Term {
    pub sort: Sort,
    pub code: String,
}

impl Term {
    pub(in crate::proof) fn boolean(value: bool) -> Self {
        Self {
            sort: Sort::Bool,
            code: value.to_string(),
        }
    }

    pub(in crate::proof) fn text(value: &str) -> Self {
        Self {
            sort: Sort::Text,
            code: quote(value),
        }
    }

    pub(in crate::proof) fn integer(value: i64) -> Self {
        Self {
            sort: Sort::Integer,
            code: if value < 0 {
                format!("(- {})", value.unsigned_abs())
            } else {
                value.to_string()
            },
        }
    }

    pub(in crate::proof) fn not(&self) -> Self {
        match self.code.as_str() {
            "true" => Self::boolean(false),
            "false" => Self::boolean(true),
            _ => Self {
                sort: Sort::Bool,
                code: format!("(not {})", self.code),
            },
        }
    }

    pub(in crate::proof) fn and(&self, other: &Self) -> Self {
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

    pub(in crate::proof) fn or(&self, other: &Self) -> Self {
        self.not().and(&other.not()).not()
    }

    /// `then` where `self` holds, else `otherwise`; both of one sort.
    fn ite(&self, then: &Self, otherwise: &Self) -> Self {
        match self.code.as_str() {
            "true" => then.clone(),
            "false" => otherwise.clone(),
            _ if then.code == otherwise.code => then.clone(),
            _ => Self {
                sort: then.sort,
                code: format!("(ite {} {} {})", self.code, then.code, otherwise.code),
            },
        }
    }
}

#[derive(Debug, Clone)]
pub(in crate::proof) enum Value {
    Scalar(Term),
    Record(BTreeMap<String, Value>),
    /// A function a call model stands for.
    Model(String),
    /// Python `None`, JavaScript `null` and Go `nil`.
    Null,
    /// JavaScript `undefined`, and what is absent: an argument left out, or
    /// `effects.<name>.last` before any call.
    Undefined,
    /// The first value where the term holds, else the second.
    Choice(Term, Box<Value>, Box<Value>),
    /// What a read of null yields where it raised: no path observes it, so
    /// any operation on it may give any answer.
    Unreachable,
}

impl Value {
    pub(in crate::proof) fn boolean(value: bool) -> Self {
        Self::Scalar(Term::boolean(value))
    }

    /// `then` where `test` holds, else `otherwise`, folded where the test is
    /// constant or both are terms of one sort.
    pub(in crate::proof) fn choice(test: &Term, then: Value, otherwise: Value) -> Value {
        match (test.code.as_str(), then, otherwise) {
            ("true", then, _) => then,
            ("false", _, otherwise) => otherwise,
            (_, Value::Scalar(then), Value::Scalar(otherwise)) if then.sort == otherwise.sort => {
                Value::Scalar(test.ite(&then, &otherwise))
            }
            (_, then, otherwise) => {
                Value::Choice(test.clone(), Box::new(then), Box::new(otherwise))
            }
        }
    }

    pub(in crate::proof) fn is_absent(&self) -> bool {
        matches!(self, Self::Null | Self::Undefined)
    }

    /// The value as a condition. Every language would coerce other values
    /// in its own way, so only Booleans are conditions.
    pub(in crate::proof) fn as_bool(&self) -> Result<Term, String> {
        match self {
            Self::Scalar(term) if term.sort == Sort::Bool => Ok(term.clone()),
            Self::Choice(test, then, otherwise) => {
                Ok(test.ite(&then.as_bool()?, &otherwise.as_bool()?))
            }
            Self::Unreachable => Ok(Term::boolean(false)),
            _ => Err("a condition must be Boolean; implicit coercion is unsupported".to_owned()),
        }
    }
}

/// A field read: the value read, and the condition under which the read
/// reaches null or undefined. An optional read yields undefined there
/// instead, as JavaScript's `?.` does.
pub(in crate::proof) fn read_field(
    value: &Value,
    name: &str,
    is_optional: bool,
) -> Result<(Value, Term), String> {
    match value {
        Value::Record(fields) => fields
            .get(name)
            .cloned()
            .map(|field| (field, Term::boolean(false)))
            .ok_or_else(|| format!("record has no declared field `{name}`")),
        Value::Null | Value::Undefined if is_optional => {
            Ok((Value::Undefined, Term::boolean(false)))
        }
        Value::Null | Value::Undefined => Ok((Value::Unreachable, Term::boolean(true))),
        Value::Unreachable => Ok((Value::Unreachable, Term::boolean(false))),
        Value::Choice(test, then, otherwise) => {
            let (then, then_fails) = read_field(then, name, is_optional)?;
            let (otherwise, otherwise_fails) = read_field(otherwise, name, is_optional)?;
            let fails = test.and(&then_fails).or(&test.not().and(&otherwise_fails));
            Ok((Value::choice(test, then, otherwise), fails))
        }
        Value::Scalar(_) | Value::Model(_) => Err(format!("cannot read `{name}` on a non-record")),
    }
}

/// Whether two values are equal, as `===` in TypeScript, `==` in Python and
/// Go compare plain data, and `is` compares with `None`, `True` or `False`.
pub(in crate::proof) fn equal(left: &Value, right: &Value) -> Result<Term, String> {
    match (left, right) {
        (Value::Choice(test, then, otherwise), other)
        | (other, Value::Choice(test, then, otherwise)) => {
            Ok(test.ite(&equal(then, other)?, &equal(otherwise, other)?))
        }
        (Value::Null, Value::Null) | (Value::Undefined, Value::Undefined) => {
            Ok(Term::boolean(true))
        }
        (Value::Null, Value::Undefined) | (Value::Undefined, Value::Null) => {
            Ok(Term::boolean(false))
        }
        (absent, Value::Scalar(_) | Value::Record(_))
        | (Value::Scalar(_) | Value::Record(_), absent)
            if absent.is_absent() =>
        {
            Ok(Term::boolean(false))
        }
        (Value::Scalar(left), Value::Scalar(right)) => equal_terms(left, right),
        _ => Err("only scalars, null and undefined can be compared".to_owned()),
    }
}

fn equal_terms(left: &Term, right: &Term) -> Result<Term, String> {
    if left.sort == right.sort {
        return Ok(Term {
            sort: Sort::Bool,
            code: format!("(= {} {})", left.code, right.code),
        });
    }
    // Text never equals a Boolean or a number in any of the languages, but
    // Python's `True == 1` disagrees with TypeScript's `true === 1`.
    if left.sort == Sort::Text || right.sort == Sort::Text {
        return Ok(Term::boolean(false));
    }
    Err("comparing a Boolean with a number differs between languages".to_owned())
}

/// An ordering of two integers.
pub(in crate::proof) fn order(operator: Op, left: &Value, right: &Value) -> Result<Term, String> {
    if matches!(left, Value::Unreachable) || matches!(right, Value::Unreachable) {
        return Ok(Term::boolean(false));
    }
    let (Value::Scalar(left), Value::Scalar(right)) = (left, right) else {
        return Err("only integers can be ordered".to_owned());
    };
    if left.sort != Sort::Integer || right.sort != Sort::Integer {
        return Err("ordered comparisons of text or Booleans are unsupported".to_owned());
    }
    let symbol = match operator {
        Op::Less => "<",
        Op::LessOrEqual => "<=",
        Op::Greater => ">",
        Op::GreaterOrEqual => ">=",
        _ => return Err("unsupported comparison".to_owned()),
    };
    Ok(Term {
        sort: Sort::Bool,
        code: format!("({symbol} {} {})", left.code, right.code),
    })
}

/// `+`, `-` or `*` of two integers, or `+` of two texts. Mixing them is left
/// out: Python raises where JavaScript coerces.
pub(in crate::proof) fn arithmetic(
    operator: Op,
    left: &Value,
    right: &Value,
) -> Result<Term, String> {
    let (Value::Scalar(left), Value::Scalar(right)) = (left, right) else {
        return Err("arithmetic needs two integers, or two texts to join".to_owned());
    };
    let (sort, function) = match (left.sort, right.sort, operator) {
        (Sort::Text, Sort::Text, Op::Add) => (Sort::Text, "str.++"),
        (Sort::Integer, Sort::Integer, Op::Add) => (Sort::Integer, "+"),
        (Sort::Integer, Sort::Integer, Op::Subtract) => (Sort::Integer, "-"),
        (Sort::Integer, Sort::Integer, Op::Multiply) => (Sort::Integer, "*"),
        _ => return Err("arithmetic needs two integers, or two texts to join".to_owned()),
    };
    Ok(Term {
        sort,
        code: format!("({function} {} {})", left.code, right.code),
    })
}

pub(in crate::proof) fn negate(value: &Value) -> Result<Term, String> {
    match value {
        Value::Scalar(term) if term.sort == Sort::Integer => Ok(Term {
            sort: Sort::Integer,
            code: format!("(- {})", term.code),
        }),
        _ => Err("only an integer can be negated".to_owned()),
    }
}

/// How `language` spells a value inside a formatted string.
pub(in crate::proof) fn to_text(value: &Value, language: Language) -> Result<Term, String> {
    let spelled = |python: &str, typescript: &str| match language {
        Language::Python => Ok(Term::text(python)),
        Language::TypeScript => Ok(Term::text(typescript)),
        Language::Go => Err("Go has no formatted string literals".to_owned()),
    };
    match value {
        Value::Scalar(term) => match term.sort {
            Sort::Text => Ok(term.clone()),
            Sort::Bool => {
                let truth = spelled("True", "true")?;
                let falsity = spelled("False", "false")?;
                Ok(term.ite(&truth, &falsity))
            }
            Sort::Integer => Ok(Term {
                sort: Sort::Text,
                code: format!(
                    "(ite (< {0} 0) (str.++ \"-\" (str.from_int (- {0}))) (str.from_int {0}))",
                    term.code
                ),
            }),
        },
        Value::Null => spelled("None", "null"),
        Value::Undefined if language == Language::TypeScript => Ok(Term::text("undefined")),
        Value::Choice(test, then, otherwise) => {
            Ok(test.ite(&to_text(then, language)?, &to_text(otherwise, language)?))
        }
        Value::Unreachable => Ok(Term::text("")),
        _ => Err("only scalars, null and undefined are formatted into text".to_owned()),
    }
}

/// The integers a language's integer can hold: TypeScript numbers are exact
/// up to 2^53 - 1, and Go's `int` has 64 bits.
pub(in crate::proof) fn integer_range(term: &Term, language: Language) -> Term {
    let bound = |low: &str, high: &str| Term {
        sort: Sort::Bool,
        code: format!("(and (<= {low} {0}) (<= {0} {high}))", term.code),
    };
    match language {
        Language::Python => Term::boolean(true),
        Language::TypeScript => bound("(- 9007199254740991)", "9007199254740991"),
        Language::Go => bound("(- 9223372036854775808)", "9223372036854775807"),
    }
}

/// An SMT-LIB string literal.
pub(in crate::proof) fn quote(text: &str) -> String {
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
