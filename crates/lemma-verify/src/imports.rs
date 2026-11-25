//! Which repository file an import names, by the importing file's language.
//!
//! Each language finds a module its own way: TypeScript and JavaScript try
//! extensions and index files and may go through path aliases, Python
//! counts leading dots, Rust follows `crate`, `self` and `super`, and Go
//! shares names across the files of a package without importing them.

use std::collections::BTreeSet;
use std::path::Path;

use lemma_http::{CodeFacts, PathAliases};

pub(crate) fn has_extension(path: &str, extension: &str) -> bool {
    Path::new(path)
        .extension()
        .is_some_and(|found| found.eq_ignore_ascii_case(extension))
}

/// The files `path` imports, resolved, and for Go the other files of its
/// package, which share its names without importing them.
pub(crate) fn imported_files(
    path: &str,
    facts: &CodeFacts,
    known: &BTreeSet<String>,
    imports: &PathAliases,
) -> Vec<String> {
    let mut imported: Vec<String> = facts
        .imports
        .iter()
        .filter_map(|specifier| resolve_import(path, specifier, known, imports))
        .collect();
    imported.extend(same_package(path, known));
    imported
}

/// For a Go file, the other files of its package: Go code uses names from
/// its whole package without importing them.
fn same_package(path: &str, known: &BTreeSet<String>) -> Vec<String> {
    if !has_extension(path, "go") {
        return Vec::new();
    }
    let directory = Path::new(path).parent().unwrap_or(Path::new(""));
    known
        .iter()
        .filter(|other| {
            other.as_str() != path
                && has_extension(other, "go")
                && !lemma_http::is_test_file(other)
                && Path::new(other.as_str()).parent() == Some(directory)
        })
        .cloned()
        .collect()
}

/// The repository file a relative import names, if it is one of `known`.
pub(crate) fn resolve_import(
    file: &str,
    specifier: &str,
    known: &BTreeSet<String>,
    aliases: &PathAliases,
) -> Option<String> {
    if has_extension(file, "rs") {
        return rust_module(file, specifier, known);
    }
    if !specifier.starts_with('.') {
        return aliases
            .candidates(file, specifier)
            .iter()
            .find_map(|base| module_file(base, known));
    }
    let directory = Path::new(file).parent().unwrap_or(Path::new(""));
    let is_python = Path::new(file)
        .extension()
        .is_some_and(|extension| extension.eq_ignore_ascii_case("py"));
    let base = if is_python {
        python_module(directory, specifier)?
    } else {
        join(directory, specifier.trim_end_matches(".js"))?
    };
    module_file(&base, known)
}

/// The file a module path names, trying source extensions and index files.
/// A TypeScript source is imported under the name it compiles to, so
/// `commands/auth.js` also names `commands/auth.ts`.
pub(crate) fn module_file(base: &str, known: &BTreeSet<String>) -> Option<String> {
    named_file(base, known).or_else(|| named_file(base.strip_suffix(".js")?, known))
}

/// The file `base` names as written, or with a source extension or an index
/// file added.
fn named_file(base: &str, known: &BTreeSet<String>) -> Option<String> {
    let candidates = [
        base.to_owned(),
        format!("{base}.ts"),
        format!("{base}.tsx"),
        format!("{base}.js"),
        format!("{base}.jsx"),
        format!("{base}/index.ts"),
        format!("{base}/index.tsx"),
        format!("{base}/index.js"),
        format!("{base}.py"),
        format!("{base}/__init__.py"),
    ];
    candidates
        .into_iter()
        .find(|candidate| known.contains(candidate))
}

/// The file of the module a Rust `use` path names: `crate::models::Tool`
/// is `src/models.rs` or `src/models/mod.rs` (the last segments may be
/// items, so shorter paths are tried too).
fn rust_module(file: &str, specifier: &str, known: &BTreeSet<String>) -> Option<String> {
    let path = Path::new(file);
    let directory = path.parent().unwrap_or(Path::new(""));
    let is_module_root = matches!(
        path.file_name().and_then(|name| name.to_str()),
        Some("mod.rs" | "lib.rs" | "main.rs")
    );
    let mut segments = specifier.split("::");
    let base = match segments.next()? {
        "crate" => {
            let source_root = file.rfind("src/").map_or("", |index| &file[..index + 4]);
            Path::new(source_root).to_path_buf()
        }
        "self" if is_module_root => directory.to_path_buf(),
        "self" => directory.join(path.file_stem()?),
        "super" if is_module_root => directory.parent()?.to_path_buf(),
        "super" => directory.to_path_buf(),
        _ => return None,
    };
    let segments: Vec<&str> = segments.collect();
    for length in (1..=segments.len()).rev() {
        let module = segments[..length]
            .iter()
            .fold(base.clone(), |module, segment| module.join(segment));
        let module = module.to_string_lossy();
        let candidates = [format!("{module}.rs"), format!("{module}/mod.rs")];
        if let Some(found) = candidates
            .into_iter()
            .find(|candidate| known.contains(candidate))
        {
            return Some(found);
        }
    }
    None
}

/// `.models` or `..core.models` relative to a Python package directory.
pub(crate) fn python_module(directory: &Path, specifier: &str) -> Option<String> {
    let dots = specifier.chars().take_while(|c| *c == '.').count();
    let mut base = directory.to_path_buf();
    for _ in 1..dots {
        base = base.parent()?.to_path_buf();
    }
    let module = specifier[dots..].replace('.', "/");
    let joined = if module.is_empty() {
        base
    } else {
        base.join(module)
    };
    Some(joined.to_string_lossy().replace('\\', "/"))
}

/// Joins a relative specifier to a directory, resolving `.` and `..`.
pub(crate) fn join(directory: &Path, specifier: &str) -> Option<String> {
    let mut parts: Vec<String> = directory
        .iter()
        .map(|part| part.to_string_lossy().into_owned())
        .collect();
    for part in specifier.split('/') {
        match part {
            "." | "" => {}
            ".." => {
                parts.pop()?;
            }
            name => parts.push(name.to_owned()),
        }
    }
    Some(parts.join("/"))
}

#[cfg(test)]
mod tests {
    use lemma_http::SourceFile;

    use super::*;

    fn known(paths: &[&str]) -> BTreeSet<String> {
        paths.iter().map(|path| (*path).to_owned()).collect()
    }

    #[test]
    fn typescript_imports_resolve_with_extensions_and_index_files() {
        let files = known(&["src/types/configs.ts", "src/models/index.ts"]);

        assert_eq!(
            resolve_import(
                "src/models/a.ts",
                "../types/configs.js",
                &files,
                &PathAliases::default()
            )
            .as_deref(),
            Some("src/types/configs.ts")
        );
        assert_eq!(
            resolve_import("src/b.ts", "./models", &files, &PathAliases::default()).as_deref(),
            Some("src/models/index.ts")
        );
    }

    #[test]
    fn a_module_named_as_it_compiles_is_its_typescript_source() {
        let files = known(&["cli/src/commands/auth.ts", "cli/src/view.tsx"]);

        assert_eq!(
            module_file("cli/src/commands/auth.js", &files).as_deref(),
            Some("cli/src/commands/auth.ts")
        );
        assert_eq!(
            module_file("cli/src/view.js", &files).as_deref(),
            Some("cli/src/view.tsx")
        );
        assert_eq!(module_file("cli/src/missing.js", &files), None);
    }

    #[test]
    fn rust_use_paths_resolve_to_module_files() {
        let files = known(&[
            "svc/src/models.rs",
            "svc/src/api/mod.rs",
            "svc/src/api/types.rs",
        ]);

        assert_eq!(
            resolve_import(
                "svc/src/api/client.rs",
                "crate::models::Tool",
                &files,
                &PathAliases::default()
            )
            .as_deref(),
            Some("svc/src/models.rs")
        );
        assert_eq!(
            resolve_import(
                "svc/src/api/client.rs",
                "super::types",
                &files,
                &PathAliases::default()
            )
            .as_deref(),
            Some("svc/src/api/types.rs")
        );
        assert_eq!(
            resolve_import(
                "svc/src/api/mod.rs",
                "self::types",
                &files,
                &PathAliases::default()
            )
            .as_deref(),
            Some("svc/src/api/types.rs")
        );
    }

    #[test]
    fn path_aliases_resolve_through_tsconfig_paths() {
        let files = known(&[
            "apps/web/src/server/trpc.ts",
            "apps/web/src/server/index.ts",
        ]);
        let aliases = PathAliases::from_files(&[SourceFile {
            path: "apps/web/tsconfig.json".to_owned(),
            contents: r#"{ "compilerOptions": { "paths": { "~/*": ["./src/*"] } } }"#.to_owned(),
        }]);

        assert_eq!(
            resolve_import(
                "apps/web/src/app/page.tsx",
                "~/server/trpc",
                &files,
                &aliases
            )
            .as_deref(),
            Some("apps/web/src/server/trpc.ts")
        );
        assert_eq!(
            resolve_import("apps/web/src/app/page.tsx", "~/server", &files, &aliases).as_deref(),
            Some("apps/web/src/server/index.ts")
        );
        assert_eq!(
            resolve_import("apps/web/src/a.ts", "react", &files, &aliases),
            None
        );
    }

    #[test]
    fn python_relative_imports_resolve_to_modules_and_packages() {
        let files = known(&["pkg/core/models.py", "pkg/types/__init__.py"]);

        assert_eq!(
            resolve_import("pkg/core/a.py", ".models", &files, &PathAliases::default()).as_deref(),
            Some("pkg/core/models.py")
        );
        assert_eq!(
            resolve_import("pkg/core/a.py", "..types", &files, &PathAliases::default()).as_deref(),
            Some("pkg/types/__init__.py")
        );
    }
}
