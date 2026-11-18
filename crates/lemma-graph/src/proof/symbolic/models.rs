//! What a contract assumes about code the checked function does not contain:
//! the decorators around it and the functions it calls.

use std::collections::{BTreeMap, BTreeSet};

use serde_json::Value as Json;

use super::super::contract::{CallModel, Contract};
use super::super::ir::{self, Expression as Expr};
use super::value::Value;

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
pub(super) fn assume_decorators(
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

/// Gives each of a model's parameters the argument passed for it: in order,
/// then by name. A parameter left out is absent, rather than falling back to
/// a specification input of the same name.
pub(super) fn match_arguments(
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

pub(super) fn check_awaiting(name: &str, is_async: bool, is_awaited: bool) -> Result<(), String> {
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
pub(super) fn reads_inputs(
    model: &CallModel,
    inputs: &BTreeMap<String, Value>,
) -> Result<bool, String> {
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
        Expr::Member(inner, _)
        | Expr::OptionalMember(inner, _)
        | Expr::Not(inner)
        | Expr::Negate(inner)
        | Expr::Index(inner, _)
        | Expr::Await(inner) => {
            expression_names(inner, names);
        }
        Expr::Sequence(items) => {
            for item in items {
                expression_names(item, names);
            }
        }
        Expr::Template(parts) => {
            for part in parts {
                if let ir::Part::Value(value) = part {
                    expression_names(value, names);
                }
            }
        }
        Expr::Binary(_, left, right)
        | Expr::Coalesce(left, right)
        | Expr::Contains(left, right) => {
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
        Expr::Bool(_) | Expr::Text(_) | Expr::Integer(_) | Expr::Null | Expr::Undefined => {}
    }
}
