//! Which handler catches a raise: Python's built-in exception classes and
//! their parents, and what can be said of classes defined elsewhere.

use super::super::ir::Raised;

/// Python's built-in exception classes and the class each derives from.
const BUILT_IN: &[(&str, &str)] = &[
    ("Exception", "BaseException"),
    ("SystemExit", "BaseException"),
    ("KeyboardInterrupt", "BaseException"),
    ("GeneratorExit", "BaseException"),
    ("StopIteration", "Exception"),
    ("ArithmeticError", "Exception"),
    ("ZeroDivisionError", "ArithmeticError"),
    ("OverflowError", "ArithmeticError"),
    ("AssertionError", "Exception"),
    ("AttributeError", "Exception"),
    ("EOFError", "Exception"),
    ("ImportError", "Exception"),
    ("ModuleNotFoundError", "ImportError"),
    ("LookupError", "Exception"),
    ("IndexError", "LookupError"),
    ("KeyError", "LookupError"),
    ("NameError", "Exception"),
    ("OSError", "Exception"),
    ("ConnectionError", "OSError"),
    ("TimeoutError", "OSError"),
    ("FileNotFoundError", "OSError"),
    ("PermissionError", "OSError"),
    ("RuntimeError", "Exception"),
    ("NotImplementedError", "RuntimeError"),
    ("RecursionError", "RuntimeError"),
    ("TypeError", "Exception"),
    ("ValueError", "Exception"),
    ("UnicodeError", "ValueError"),
    ("UnicodeDecodeError", "UnicodeError"),
    ("UnicodeEncodeError", "UnicodeError"),
];

/// The classes Python itself raises for what this checker reads: a field
/// of `None`, a missing key or attribute, a position past the end, a
/// sequence of another length, and a failed `assert`.
const IMPLICIT: &[&str] = &[
    "AttributeError",
    "TypeError",
    "KeyError",
    "IndexError",
    "ValueError",
];

fn parent(class: &str) -> Option<&'static str> {
    BUILT_IN
        .iter()
        .find(|(name, _)| *name == class)
        .map(|(_, parent)| *parent)
}

fn is_built_in(class: &str) -> bool {
    class == "BaseException" || parent(class).is_some()
}

/// Whether built-in `class` is `ancestor` or derives from it.
fn derives(class: &str, ancestor: &str) -> bool {
    let mut current = Some(class);
    while let Some(name) = current {
        if name == ancestor {
            return true;
        }
        current = parent(name);
    }
    false
}

/// Whether a handler catches a raise.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) enum Catch {
    No,
    Yes,
    /// It does, if an assumption about a class defined elsewhere holds.
    Assuming(String),
}

/// Whether a handler catching `classes`, or everything for `None`, catches
/// `raised`. A relation between classes defined elsewhere, which this
/// checker cannot see, is refused.
pub(super) fn catches(classes: Option<&[String]>, raised: &Raised) -> Result<Catch, String> {
    let Some(classes) = classes else {
        return Ok(Catch::Yes);
    };
    for handled in classes {
        let handled = handled.trim_start_matches("builtins.");
        let answer = match raised {
            Raised::Class(class) => catches_class(handled, class.trim_start_matches("builtins."))?,
            Raised::Implicit => catches_implicit(handled)?,
            Raised::Unknown | Raised::Again if handled == "BaseException" => Catch::Yes,
            Raised::Unknown | Raised::Again => {
                return Err(format!(
                    "a raise whose class is not written may or may not be caught by `except {handled}`"
                ));
            }
        };
        if answer != Catch::No {
            return Ok(answer);
        }
    }
    Ok(Catch::No)
}

fn catches_class(handled: &str, class: &str) -> Result<Catch, String> {
    if handled == class || handled == "BaseException" {
        return Ok(Catch::Yes);
    }
    match (is_built_in(class), is_built_in(handled)) {
        (true, true) if derives(class, handled) => Ok(Catch::Yes),
        // A built-in class never derives from one defined elsewhere.
        (true, _) => Ok(Catch::No),
        (false, true) if handled == "Exception" => Ok(Catch::Assuming(format!(
            "exception class `{class}` derives from Exception"
        ))),
        _ => Err(format!(
            "whether `{class}` derives from `{handled}` is not known here"
        )),
    }
}

fn catches_implicit(handled: &str) -> Result<Catch, String> {
    if IMPLICIT.iter().all(|class| derives(class, handled)) {
        return Ok(Catch::Yes);
    }
    let catches_none =
        !is_built_in(handled) || IMPLICIT.iter().all(|class| !derives(class, handled));
    if catches_none {
        return Ok(Catch::No);
    }
    Err(format!(
        "whether `except {handled}` catches what Python raises here depends on how a field was read"
    ))
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used, reason = "test setup should fail loudly")]

    use super::*;

    fn classes(names: &[&str]) -> Vec<String> {
        names.iter().map(|name| (*name).to_owned()).collect()
    }

    fn answer(handled: &[&str], raised: &Raised) -> Result<Catch, String> {
        catches(Some(&classes(handled)), raised)
    }

    #[test]
    fn built_in_classes_are_caught_by_their_ancestors_only() {
        let raised = Raised::Class("KeyError".to_owned());
        assert_eq!(answer(&["LookupError"], &raised).unwrap(), Catch::Yes);
        assert_eq!(answer(&["Exception"], &raised).unwrap(), Catch::Yes);
        assert_eq!(answer(&["ValueError"], &raised).unwrap(), Catch::No);
        assert_eq!(answer(&["NotFound"], &raised).unwrap(), Catch::No);
    }

    #[test]
    fn classes_defined_elsewhere_need_their_name_or_an_assumption() {
        let raised = Raised::Class("NotFound".to_owned());
        assert_eq!(answer(&["NotFound"], &raised).unwrap(), Catch::Yes);
        assert!(matches!(
            answer(&["Exception"], &raised).unwrap(),
            Catch::Assuming(_)
        ));
        assert!(answer(&["ValueError"], &raised).is_err());
        assert!(answer(&["Missing"], &raised).is_err());
    }

    #[test]
    fn what_python_raises_itself_is_caught_only_by_handlers_that_catch_it_all() {
        assert_eq!(
            answer(&["Exception"], &Raised::Implicit).unwrap(),
            Catch::Yes
        );
        assert_eq!(answer(&["OSError"], &Raised::Implicit).unwrap(), Catch::No);
        assert!(answer(&["KeyError"], &Raised::Implicit).is_err());
        assert_eq!(catches(None, &Raised::Unknown).unwrap(), Catch::Yes);
    }
}
