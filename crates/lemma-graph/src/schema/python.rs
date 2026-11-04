//! Python types as JSON Schema, by Pydantic: `Model.model_json_schema()`
//! for models, `TypeAdapter(T).json_schema()` for `TypedDict`s, dataclasses
//! and other types. The repository's own interpreter imports the module, so
//! aliases, inheritance and generics come out as Python sees them.

use std::collections::BTreeMap;
use std::path::Path;

use serde_json::Value;

use crate::GraphError;
use crate::repos;

/// Imports `symbol` (dotted: `module_class.Request`) from `file` and prints
/// its schema.
pub(crate) const SCRIPT: &str = r#"
import importlib, json, sys
sys.path.insert(0, ".")
module = importlib.import_module(sys.argv[1])
value = module
for part in sys.argv[2].split("."):
    value = getattr(value, part)
from pydantic import BaseModel, TypeAdapter
if isinstance(value, type) and issubclass(value, BaseModel):
    schema = value.model_json_schema()
else:
    schema = TypeAdapter(value).json_schema()
print(json.dumps(schema))
"#;

/// The schema of `symbol` in `file` (relative to `root`), using `python`.
/// `env` is the service's; its `PYTHONPATH` (the repository root if unset)
/// says where imports start.
pub fn type_schema(
    root: &Path,
    python: &str,
    file: &str,
    symbol: &str,
    env: &BTreeMap<String, String>,
) -> Result<Value, GraphError> {
    let mut env = env.clone();
    let python_path = env
        .entry("PYTHONPATH".to_owned())
        .or_insert_with(|| ".".to_owned())
        .clone();
    let module = module_name(file, &python_path);
    let command = format!(
        "{} -c {} {} {}",
        quote(python),
        quote(SCRIPT),
        quote(&module),
        quote(symbol)
    );
    let output = repos::run(root, &command, &env)?;
    serde_json::from_str(output.trim()).map_err(|source| GraphError::ResolverOutput {
        command: format!("{python} (schema of {file}#{symbol})"),
        source,
    })
}

/// `pkg/mod.py` → `pkg.mod`; `pkg/__init__.py` → `pkg`. A file under a
/// relative `PYTHONPATH` entry (`src`) is named from there.
fn module_name(file: &str, python_path: &str) -> String {
    let relative = python_path
        .split(':')
        .map(|entry| entry.trim_start_matches("./").trim_end_matches('/'))
        .filter(|entry| !entry.is_empty() && *entry != "." && !entry.starts_with('/'))
        .find_map(|entry| file.strip_prefix(entry)?.strip_prefix('/'))
        .unwrap_or(file);
    let without = relative.strip_suffix(".py").unwrap_or(relative);
    let without = without.strip_suffix("/__init__").unwrap_or(without);
    without.replace('/', ".")
}

fn quote(text: &str) -> String {
    format!("'{}'", text.replace('\'', r"'\''"))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn files_become_module_names() {
        assert_eq!(
            module_name("billing/api/charges.py", "."),
            "billing.api.charges"
        );
        assert_eq!(module_name("pkg/__init__.py", "."), "pkg");
        assert_eq!(module_name("src/pkg/m.py", "src"), "pkg.m");
    }

    #[test]
    fn shell_quoting_survives_single_quotes() {
        assert_eq!(quote("it's"), r"'it'\''s'");
    }
}
