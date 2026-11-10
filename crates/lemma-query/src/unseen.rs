//! Indexed files a language server is known to leave out of its answers,
//! so a reference list can say where uses may be missing instead of
//! looking complete.
//!
//! Two cases are recognised. gopls builds a package without custom build
//! tags, so files behind `//go:build integration` are invisible to it unless
//! its build flags name the tag. The TypeScript server does not follow a
//! module named by a string at run time: `import("./crypto")`,
//! `vi.mock("./crypto")` and `jest.mock("./crypto")`.

use std::fs;
use std::io::Read;
use std::path::Path;

use lemma_legacy_store::{FactStore, StoredSymbol};
use lemma_model::{Language, Reference};

use crate::QueryError;

/// How much of a Go file is read for its build constraint, which must come
/// before the package clause.
const GO_HEADER_BYTES: u64 = 4096;
/// How many files a warning names.
const NAMED_FILES: usize = 3;

/// Calls naming a module by a string that the TypeScript server does not
/// resolve to the module's exports.
const DYNAMIC_IMPORTS: [&str; 6] = [
    "import(",
    "vi.mock(",
    "vi.doMock(",
    "vi.importActual(",
    "jest.mock(",
    "jest.requireActual(",
];

/// Build tags that hold in an ordinary build: operating systems,
/// architectures, compilers, `cgo`, `unix`, and Go versions.
const DEFAULT_TAGS: [&str; 30] = [
    "aix",
    "android",
    "darwin",
    "dragonfly",
    "freebsd",
    "hurd",
    "illumos",
    "ios",
    "js",
    "linux",
    "netbsd",
    "openbsd",
    "plan9",
    "solaris",
    "wasip1",
    "windows",
    "unix",
    "386",
    "amd64",
    "arm",
    "arm64",
    "loong64",
    "mips",
    "mips64",
    "ppc64",
    "ppc64le",
    "riscv64",
    "s390x",
    "wasm",
    "cgo",
];

/// Warnings about indexed files the provider did not read for `symbol`'s
/// references, empty when there are none.
pub(crate) fn unseen_files(
    store: &dyn FactStore,
    snapshot_id: i64,
    root: &Path,
    symbol: &StoredSymbol,
    references: &[Reference],
) -> Result<Vec<String>, QueryError> {
    let files = store.files(snapshot_id)?;
    let warning = match symbol.language {
        Language::Go => {
            let go_files = files.iter().filter(|file| file.language == Language::Go);
            let name = &symbol.symbol.name;
            let paths = go_files.map(|file| file.path.as_str());
            tagged_go_warning(root, name, unlisted(paths, references).into_iter())
        }
        Language::TypeScript | Language::Tsx => {
            let scripts = files.iter().filter(|file| is_script(file.language));
            dynamic_import_warning(
                root,
                &symbol.path,
                scripts.map(|file| file.path.as_str()),
                references,
            )
        }
        _ => None,
    };
    Ok(warning.into_iter().collect())
}

/// The paths no reference is in. A tagged Go file the answer lists was
/// read, its tags named by the server's build flags, so it is not unseen.
fn unlisted<'p>(paths: impl Iterator<Item = &'p str>, references: &[Reference]) -> Vec<&'p str> {
    let listed = listed_paths(references);
    paths.filter(|path| !listed.contains(path)).collect()
}

/// The files the references are in, to look up without scanning them.
fn listed_paths(references: &[Reference]) -> std::collections::HashSet<&str> {
    references
        .iter()
        .map(|reference| reference.path.as_str())
        .collect()
}

fn is_script(language: Language) -> bool {
    matches!(language, Language::TypeScript | Language::Tsx)
}

/// Tagged Go files that mention `name`, the only ones whose uses can be
/// missing.
fn tagged_go_warning<'p>(
    root: &Path,
    name: &str,
    paths: impl Iterator<Item = &'p str>,
) -> Option<String> {
    let mut tagged = Vec::new();
    for path in paths {
        let file = root.join(path);
        let Some(tags) = read_header(&file).and_then(|header| custom_build_tags(&header)) else {
            continue;
        };
        let mentions = fs::read_to_string(&file).is_ok_and(|source| mentions_word(&source, name));
        if mentions {
            tagged.push(format!("{path} ({tags})"));
        }
    }
    let first = tagged.first()?;
    Some(format!(
        "{} Go file(s) that mention this name build only with tags gopls leaves out by default, \
         such as {first}; uses there are not listed unless gopls's buildFlags in \
         .lemma/providers.toml name the tags",
        tagged.len()
    ))
}

/// Whether `name` appears in `source` as a whole identifier.
fn mentions_word(source: &str, name: &str) -> bool {
    let is_identifier = |c: char| c.is_alphanumeric() || c == '_';
    source.match_indices(name).any(|(at, _)| {
        let before = source[..at].chars().next_back();
        let after = source[at + name.len()..].chars().next();
        !before.is_some_and(is_identifier) && !after.is_some_and(is_identifier)
    })
}

fn read_header(path: &Path) -> Option<String> {
    let mut header = Vec::new();
    fs::File::open(path)
        .ok()?
        .take(GO_HEADER_BYTES)
        .read_to_end(&mut header)
        .ok()?;
    Some(String::from_utf8_lossy(&header).into_owned())
}

/// The tags a Go file's `//go:build` line requires beyond an ordinary
/// build, joined with spaces, or `None` when an ordinary build includes it.
fn custom_build_tags(header: &str) -> Option<String> {
    let constraint = header
        .lines()
        .map(str::trim)
        .take_while(|line| !line.starts_with("package "))
        .find_map(|line| line.strip_prefix("//go:build "))?;
    let mut required = Vec::new();
    let mut is_negated = false;
    for token in
        constraint.split(|c: char| !(c.is_alphanumeric() || c == '_' || c == '.' || c == '!'))
    {
        for part in token.split_inclusive('!') {
            if part == "!" {
                is_negated = true;
                continue;
            }
            if !part.is_empty() && !is_negated && !is_default_tag(part) {
                required.push(part);
            }
            is_negated = false;
        }
    }
    // `ignore` marks a file no build includes, such as a generator script:
    // its uses are not uses of the package.
    if required.is_empty() || required.contains(&"ignore") {
        return None;
    }
    Some(required.join(" "))
}

fn is_default_tag(tag: &str) -> bool {
    DEFAULT_TAGS.contains(&tag) || tag.starts_with("go1.") || tag == "gc" || tag == "gccgo"
}

fn dynamic_import_warning<'p>(
    root: &Path,
    target: &str,
    paths: impl Iterator<Item = &'p str>,
    references: &[Reference],
) -> Option<String> {
    let listed = listed_paths(references);
    let module = module_name(target);
    let mut importers = Vec::new();
    for path in paths {
        if path == target || listed.contains(path) {
            continue;
        }
        let Ok(source) = fs::read_to_string(root.join(path)) else {
            continue;
        };
        // Any specifier naming the target names its file or directory: a
        // file without that name is passed over with one search.
        if !source.contains(module) {
            continue;
        }
        if imports_dynamically(path, &source, target) {
            importers.push(path);
        }
    }
    if importers.is_empty() {
        return None;
    }
    let named = importers[..importers.len().min(NAMED_FILES)].join(", ");
    let unnamed = importers.len().saturating_sub(NAMED_FILES);
    let more = if unnamed > 0 {
        format!(" and {unnamed} more")
    } else {
        String::new()
    };
    let verb = if importers.len() == 1 {
        "names"
    } else {
        "name"
    };
    Some(format!(
        "{named}{more} {verb} {target} in import(), vi.mock() or jest.mock(), which the server \
         does not resolve; uses there are not listed"
    ))
}

/// Whether `source`, the file at `importer`, names `target` in a call the
/// TypeScript server does not resolve.
fn imports_dynamically(importer: &str, source: &str, target: &str) -> bool {
    let target = without_extension(target);
    DYNAMIC_IMPORTS.iter().any(|call| {
        source
            .match_indices(call)
            .filter_map(|(at, _)| string_argument(&source[at + call.len()..]))
            .any(|specifier| names_module(importer, specifier, target))
    })
}

/// The string literal a call's argument list opens with.
fn string_argument(after_parenthesis: &str) -> Option<&str> {
    let rest = after_parenthesis.trim_start();
    let quote = rest
        .chars()
        .next()
        .filter(|c| matches!(c, '"' | '\'' | '`'))?;
    let body = &rest[1..];
    Some(&body[..body.find(quote)?])
}

/// Whether `specifier`, written in `importer`, names the module at `target`
/// (a path without its extension). A relative specifier is resolved; any
/// other, such as `@/lib/crypto`, matches when its path ends the target's.
fn names_module(importer: &str, specifier: &str, target: &str) -> bool {
    let specifier = without_extension(specifier);
    let candidates = [target.to_owned(), format!("{target}/index")];
    if specifier.starts_with('.') {
        let directory = importer
            .rsplit_once('/')
            .map_or("", |(directory, _)| directory);
        let resolved = normalize(&format!("{directory}/{specifier}"));
        return candidates.contains(&resolved)
            || target.strip_suffix("/index") == Some(resolved.as_str());
    }
    // A bare package name, such as `vitest`, is not a file of this repository.
    let Some((_, path)) = specifier.split_once('/') else {
        return false;
    };
    let ending = format!("/{path}");
    let module = target.strip_suffix("/index").unwrap_or(target);
    target.ends_with(&ending) || module.ends_with(&ending)
}

/// The name a specifier naming `target` ends with: its file's name, or its
/// directory's for an index file (`./charges` for `charges/index.ts`).
fn module_name(target: &str) -> &str {
    let stem = without_extension(target);
    let (directory, name) = stem.rsplit_once('/').unwrap_or(("", stem));
    if name != "index" {
        return name;
    }
    directory.rsplit('/').next().unwrap_or(name)
}

fn without_extension(path: &str) -> &str {
    for extension in [".ts", ".tsx", ".mts", ".cts", ".js", ".jsx", ".mjs", ".cjs"] {
        if let Some(stem) = path.strip_suffix(extension) {
            return stem;
        }
    }
    path
}

/// `a/b/../c/./d` as `a/c/d`.
fn normalize(path: &str) -> String {
    let mut parts: Vec<&str> = Vec::new();
    for part in path.split('/') {
        match part {
            "" | "." => {}
            ".." => {
                parts.pop();
            }
            part => parts.push(part),
        }
    }
    parts.join("/")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_custom_build_tag_is_reported() {
        let header = "//go:build integration && linux\n\npackage store\n";

        assert_eq!(custom_build_tags(header).as_deref(), Some("integration"));
    }

    #[test]
    fn platform_and_negated_tags_hold_in_an_ordinary_build() {
        let header = "// Copyright\n//go:build (linux || darwin) && !integration && go1.21\n\npackage store\n";

        assert_eq!(custom_build_tags(header), None);
    }

    #[test]
    fn a_file_no_build_includes_is_not_reported() {
        let header = "//go:build ignore\n\npackage main\n";

        assert_eq!(custom_build_tags(header), None);
    }

    #[test]
    fn a_constraint_after_the_package_clause_does_not_count() {
        let header = "package store\n//go:build integration\n";

        assert_eq!(custom_build_tags(header), None);
    }

    #[test]
    fn a_file_the_answer_lists_is_not_unseen() {
        let reference = Reference {
            path: "store/integration_test.go".to_owned(),
            span: lemma_model::Span {
                start: lemma_model::Position { line: 3, column: 4 },
                end: lemma_model::Position { line: 3, column: 9 },
            },
            is_external: false,
            enclosing_symbol: None,
        };
        let paths = ["store/integration_test.go", "store/other_test.go"];

        let left = unlisted(paths.into_iter(), &[reference]);

        assert_eq!(left, ["store/other_test.go"]);
    }

    #[test]
    fn a_name_inside_a_longer_identifier_is_not_mentioned() {
        assert!(mentions_word("x := NewStore(db)", "NewStore"));
        assert!(!mentions_word("x := NewStoreFor(db)", "NewStore"));
    }

    #[test]
    fn a_relative_mock_names_the_module_it_resolves_to() {
        let source = "vi.mock('../lib/crypto', () => ({ safeEncrypt: vi.fn() }));";

        assert!(imports_dynamically(
            "src/api/routes.test.ts",
            source,
            "src/lib/crypto.ts"
        ));
        assert!(!imports_dynamically(
            "src/api/routes.test.ts",
            source,
            "src/api/crypto.ts"
        ));
    }

    #[test]
    fn an_aliased_dynamic_import_matches_by_its_path() {
        let source = "const { safeEncrypt } = await import(\"@/lib/crypto\");";

        assert!(imports_dynamically(
            "src/app.ts",
            source,
            "src/lib/crypto.ts"
        ));
    }

    #[test]
    fn a_static_import_is_not_dynamic() {
        let source = "import { safeEncrypt } from './crypto';";

        assert!(!imports_dynamically(
            "src/lib/use.ts",
            source,
            "src/lib/crypto.ts"
        ));
    }
}
