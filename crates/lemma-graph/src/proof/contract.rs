//! The language-neutral contract carried by a proof checker's arguments.

use std::collections::BTreeMap;

use serde::Deserialize;
use serde_json::Value;

/// A contract for one source-bound operation.
#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub(super) struct Contract {
    pub inputs: BTreeMap<String, Input>,
    #[serde(default)]
    pub parameters: Vec<String>,
    #[serde(default)]
    pub bindings: BTreeMap<String, Value>,
    #[serde(default)]
    pub models: BTreeMap<String, CallModel>,
    #[serde(default)]
    pub requires: Vec<String>,
    pub ensures: Vec<Assertion>,
    #[serde(default = "path_limit")]
    pub max_paths: usize,
}

fn path_limit() -> usize {
    256
}

/// A symbolic scalar or an immutable record of symbolic inputs.
#[derive(Debug, Clone, Deserialize)]
#[serde(untagged)]
pub(super) enum Input {
    Scalar(String),
    Record(BTreeMap<String, Input>),
}

/// A named property required at every normal return.
#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub(super) struct Assertion {
    pub name: String,
    pub condition: String,
}

/// An explicitly assumed interpretation of a dependency.
#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub(super) struct CallModel {
    pub assumption: String,
    #[serde(default)]
    pub parameters: Vec<String>,
    pub returns: Value,
    #[serde(default)]
    pub effect: Option<Effect>,
}

/// One modeled effect, recorded with the arguments of its call.
#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub(super) struct Effect {
    pub name: String,
    pub fields: BTreeMap<String, Value>,
}

impl Contract {
    pub(super) fn validate(&self) -> Result<(), String> {
        if self.inputs.is_empty() || self.ensures.is_empty() {
            return Err("a proof contract needs inputs and at least one postcondition".to_owned());
        }
        if !(1..=4096).contains(&self.max_paths) {
            return Err("max_paths must be between 1 and 4096".to_owned());
        }
        let mut names = std::collections::BTreeSet::new();
        for assertion in &self.ensures {
            if assertion.name.is_empty() || !names.insert(&assertion.name) {
                return Err("postconditions need nonempty, distinct names".to_owned());
            }
        }
        for name in self.inputs.keys().chain(self.bindings.keys()) {
            if matches!(name.as_str(), "result" | "effects") {
                return Err(format!("`{name}` is reserved in a proof contract"));
            }
        }
        for name in self
            .inputs
            .keys()
            .chain(self.bindings.keys())
            .chain(self.parameters.iter())
        {
            if !identifier(name) {
                return Err(format!("invalid contract name `{name}`"));
            }
        }
        for (name, model) in &self.models {
            if !name.split('.').all(identifier) {
                return Err(format!("invalid model name `{name}`"));
            }
            model.validate(name)?;
        }
        Ok(())
    }
}

impl CallModel {
    fn validate(&self, name: &str) -> Result<(), String> {
        let mut parameters = std::collections::BTreeSet::new();
        for parameter in &self.parameters {
            if !identifier(parameter) || !parameters.insert(parameter) {
                return Err(format!("model `{name}` needs distinct parameter names"));
            }
        }
        if let Some(effect) = &self.effect {
            if !identifier(&effect.name) {
                return Err(format!("invalid effect name `{}`", effect.name));
            }
        }
        if self.assumption.trim().is_empty() {
            return Err(format!(
                "dependency model `{name}` needs a named assumption"
            ));
        }
        Ok(())
    }
}

fn identifier(name: &str) -> bool {
    let mut characters = name.chars();
    characters
        .next()
        .is_some_and(|c| c.is_ascii_alphabetic() || c == '_')
        && characters.all(|c| c.is_ascii_alphanumeric() || c == '_')
}
