//! The names in code a finding is about, and where text mentions them: how
//! a finding is tied to the lines of a change without reading the code
//! again.

use std::collections::BTreeMap;

/// The names in code a finding with these `values` is about, longest
/// first: what its requirement bound that reads as a field, a parameter or
/// a function, leaving out where its code is (`f`, `l`).
pub fn named_in(values: &BTreeMap<String, String>) -> Vec<&str> {
    let mut names: Vec<&str> = values
        .iter()
        .filter(|(variable, _)| !["f", "l"].contains(&variable.as_str()))
        .map(|(_, value)| value.as_str())
        .filter(|value| is_identifier(value))
        .collect();
    names.sort_by_key(|name| std::cmp::Reverse(name.len()));
    names.dedup();
    names
}

/// Whether `value` reads as a name in code: a field, a parameter or a
/// function, rather than a path, an operation or a number.
fn is_identifier(value: &str) -> bool {
    let mut characters = value.chars();
    characters
        .next()
        .is_some_and(|first| first.is_ascii_alphabetic() || first == '_')
        && characters.all(|character| character.is_ascii_alphanumeric() || character == '_')
}

/// Whether `text` holds `name` as a word of its own.
pub fn mentions(text: &str, name: &str) -> bool {
    let is_word = |character: char| character.is_ascii_alphanumeric() || character == '_';
    text.match_indices(name).any(|(start, _)| {
        let before = text[..start].chars().next_back();
        let after = text[start + name.len()..].chars().next();
        !before.is_some_and(is_word) && !after.is_some_and(is_word)
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_name_inside_a_longer_word_is_not_a_mention() {
        assert!(mentions("  user_id: z.string(),", "user_id"));
        assert!(!mentions("  owner_user_id: z.string(),", "user_id"));
    }

    #[test]
    fn a_findings_names_leave_out_its_place_and_what_is_not_a_name() {
        let values = BTreeMap::from([
            ("f".to_owned(), "src".to_owned()),
            ("l".to_owned(), "2".to_owned()),
            ("op".to_owned(), "GET /v1/invoices".to_owned()),
            ("fld".to_owned(), "customer".to_owned()),
            ("leaf".to_owned(), "customer_email".to_owned()),
        ]);

        let names = named_in(&values);

        assert_eq!(names, ["customer_email", "customer"]);
    }
}
