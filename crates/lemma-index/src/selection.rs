//! Which files an index covers: the code each service in a description says
//! it is made of.

use std::collections::BTreeSet;

use globset::{Glob, GlobSet, GlobSetBuilder};
use lemma_model::Language;

/// The files an index covers: those any one rule covers.
#[derive(Debug, Clone, Default)]
pub struct Selection {
    rules: Vec<Rule>,
    /// Whether there are no rules and everything is covered, as when no
    /// configuration narrows the index.
    is_everything: bool,
    /// Whether directories whose names start with a dot count. A check
    /// reads what Git lists, which includes them; an index does not.
    enters_hidden: bool,
}

/// One service's part: files under `include`, not matching `exclude`, in
/// `languages`.
#[derive(Debug, Clone)]
struct Rule {
    include: Vec<String>,
    exclude: GlobSet,
    exclude_patterns: Vec<String>,
    languages: Option<BTreeSet<Language>>,
}

/// A rule as written: directories or files to include, glob patterns to
/// exclude, and languages (all of them if empty).
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct RuleSpec {
    /// Directories or files, relative to the indexed root.
    pub include: Vec<String>,
    /// Glob patterns, relative to the indexed root.
    pub exclude: Vec<String>,
    /// Languages; empty for all.
    pub languages: Vec<Language>,
}

/// An exclude pattern that is not a valid glob.
#[derive(Debug, thiserror::Error)]
#[error("`{pattern}` is not a valid glob pattern")]
pub struct InvalidPattern {
    /// The pattern.
    pub pattern: String,
    /// Why.
    #[source]
    pub source: globset::Error,
}

impl Selection {
    /// Every supported file, as before indexes were configured.
    pub fn everything() -> Self {
        Self {
            rules: Vec::new(),
            is_everything: true,
            enters_hidden: false,
        }
    }

    /// Everything a check can read: every supported file, and directories
    /// whose names start with a dot, such as `.github`.
    pub fn everything_git_lists() -> Self {
        Self {
            enters_hidden: true,
            ..Self::everything()
        }
    }

    /// Whether directories whose names start with a dot are entered.
    pub(crate) fn enters_hidden(&self) -> bool {
        self.enters_hidden
    }

    /// The files the rules cover. No rules cover nothing.
    pub fn from_rules(specs: &[RuleSpec]) -> Result<Self, InvalidPattern> {
        let rules = specs.iter().map(Rule::new).collect::<Result<_, _>>()?;
        Ok(Self {
            rules,
            is_everything: false,
            enters_hidden: false,
        })
    }

    /// Whether a file, relative to the root, is covered.
    pub fn covers(&self, path: &str, language: Language) -> bool {
        self.is_everything || self.rules.iter().any(|rule| rule.covers(path, language))
    }

    /// Whether a directory, relative to the root (`""` for the root), can
    /// hold covered files, so a walk should enter it.
    pub fn enters(&self, directory: &str) -> bool {
        self.is_everything || self.rules.iter().any(|rule| rule.enters(directory))
    }

    /// A stable description, to tell whether two selections differ.
    pub fn description(&self) -> String {
        if self.is_everything {
            return "everything".to_owned();
        }
        self.rules
            .iter()
            .map(Rule::description)
            .collect::<Vec<_>>()
            .join("; ")
    }
}

impl Rule {
    fn new(spec: &RuleSpec) -> Result<Self, InvalidPattern> {
        let mut builder = GlobSetBuilder::new();
        for pattern in &spec.exclude {
            let glob = Glob::new(pattern).map_err(|source| InvalidPattern {
                pattern: pattern.clone(),
                source,
            })?;
            builder.add(glob);
        }
        let exclude = builder.build().map_err(|source| InvalidPattern {
            pattern: spec.exclude.join(", "),
            source,
        })?;
        let languages = (!spec.languages.is_empty())
            .then(|| spec.languages.iter().copied().collect::<BTreeSet<_>>());
        Ok(Self {
            include: spec.include.iter().map(|path| normalize(path)).collect(),
            exclude,
            exclude_patterns: spec.exclude.clone(),
            languages,
        })
    }

    fn covers(&self, path: &str, language: Language) -> bool {
        let is_included = self
            .include
            .iter()
            .any(|include| include.is_empty() || path == include || is_below(path, include));
        is_included
            && !self.exclude.is_match(path)
            && self
                .languages
                .as_ref()
                .is_none_or(|languages| languages.contains(&language))
    }

    fn enters(&self, directory: &str) -> bool {
        // A directory is excluded when everything in it would be: `x/**`
        // matches what is inside `x`, not `x` itself, so a placeholder
        // child stands in for its contents.
        let is_excluded = self.exclude.is_match(directory)
            || self.exclude.is_match(format!("{directory}/{CONTENTS}"));
        if !directory.is_empty() && is_excluded {
            return false;
        }
        self.include.iter().any(|include| {
            include.is_empty()
                || directory.is_empty()
                || directory == include
                || is_below(directory, include)
                || is_below(include, directory)
        })
    }

    fn description(&self) -> String {
        let languages = self
            .languages
            .as_ref()
            .map_or_else(String::new, |languages| {
                let names: Vec<&str> = languages.iter().map(|language| language.as_str()).collect();
                format!(" in {}", names.join(","))
            });
        format!(
            "{} except {}{languages}",
            self.include.join(","),
            self.exclude_patterns.join(",")
        )
    }
}

/// A file name no real file has, standing for a directory's contents when
/// matching exclude patterns.
const CONTENTS: &str = "\u{1}";

/// Whether `path` is inside the directory `directory`.
fn is_below(path: &str, directory: &str) -> bool {
    path.strip_prefix(directory)
        .is_some_and(|rest| rest.starts_with('/'))
}

/// A path relative to the root, without `./` or a trailing `/`; `.` is the
/// root itself (`""`).
fn normalize(path: &str) -> String {
    let path = path.trim_start_matches("./").trim_end_matches('/');
    if path == "." {
        return String::new();
    }
    path.to_owned()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn selection() -> Selection {
        Selection::from_rules(&[
            RuleSpec {
                include: vec!["apps/api/".to_owned(), "libs/common".to_owned()],
                exclude: vec!["**/generated/**".to_owned()],
                languages: vec![Language::TypeScript],
            },
            RuleSpec {
                include: vec!["./tools/build.go".to_owned()],
                exclude: Vec::new(),
                languages: Vec::new(),
            },
        ])
        .unwrap()
    }

    #[test]
    fn files_are_covered_by_any_service_that_includes_them() {
        let selection = selection();

        assert!(selection.covers("apps/api/src/app.ts", Language::TypeScript));
        assert!(selection.covers("libs/common/x.ts", Language::TypeScript));
        assert!(selection.covers("tools/build.go", Language::Go));
        assert!(!selection.covers("apps/api/src/generated/client.ts", Language::TypeScript));
        assert!(!selection.covers("apps/api/src/app.py", Language::Python));
        assert!(!selection.covers("apps/apiary/x.ts", Language::TypeScript));
        assert!(!selection.covers("tools/other.go", Language::Go));
    }

    #[test]
    fn walks_enter_only_directories_that_can_hold_covered_files() {
        let selection = selection();

        assert!(selection.enters(""));
        assert!(selection.enters("apps"));
        assert!(selection.enters("apps/api/src"));
        assert!(selection.enters("tools"));
        assert!(!selection.enters("docs"));
        assert!(!selection.enters("apps/api/src/generated"));
    }

    #[test]
    fn no_rules_cover_nothing_and_everything_covers_all() {
        let none = Selection::from_rules(&[]).unwrap();
        let all = Selection::everything();

        assert!(!none.covers("a.ts", Language::TypeScript));
        assert!(all.covers("a.ts", Language::TypeScript));
        assert!(all.enters("anything"));
    }
}
