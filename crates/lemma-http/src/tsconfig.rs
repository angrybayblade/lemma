//! Path aliases from `tsconfig.json` files (`compilerOptions.paths`), so an
//! import such as `@/lib/schemas` resolves to the file it names the way the
//! TypeScript compiler resolves it.
//!
//! Each config's aliases apply to files under its directory; the nearest
//! config wins. A relative `extends` is followed for configs that declare no
//! `paths` of their own.

use std::collections::BTreeMap;
use std::path::{Component, Path, PathBuf};

use serde_json::Value as Json;

use crate::SourceFile;

/// How many `extends` links to follow.
const MAX_EXTENDS: usize = 8;

/// The path aliases of every `tsconfig` in a repository.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct PathAliases {
    /// Configs with aliases, by the directory they apply to.
    configs: BTreeMap<String, Vec<Alias>>,
}

/// One `paths` entry: `"@/*": ["./src/*"]`.
#[derive(Debug, Clone, PartialEq, Eq)]
struct Alias {
    /// The pattern before its `*`, or the whole pattern without one.
    prefix: String,
    /// Whether the pattern ends with `*`.
    is_wildcard: bool,
    /// Targets relative to the repository root, before their `*`.
    targets: Vec<String>,
}

/// A config as read: its own options, before `extends` is applied.
struct Config {
    paths: Option<serde_json::Map<String, Json>>,
    base_url: Option<String>,
    extends: Option<String>,
}

impl PathAliases {
    /// Reads every `tsconfig*.json` among `files`; others are ignored.
    pub fn from_files(files: &[SourceFile]) -> Self {
        let configs: BTreeMap<String, Config> = files
            .iter()
            .filter(|file| is_tsconfig(&file.path))
            .filter_map(|file| Some((file.path.clone(), read_config(&file.contents)?)))
            .collect();
        let mut aliases = BTreeMap::new();
        for path in configs
            .keys()
            .filter(|path| file_name(path) == "tsconfig.json")
        {
            let resolved = resolve_aliases(path, &configs);
            if !resolved.is_empty() {
                aliases.insert(parent(path), resolved);
            }
        }
        Self { configs: aliases }
    }

    /// The module paths an import may name, relative to the repository root
    /// and without an extension, or none if no alias applies.
    pub fn candidates(&self, importing_file: &str, specifier: &str) -> Vec<String> {
        let Some(aliases) = self.nearest(importing_file) else {
            return Vec::new();
        };
        let mut found = Vec::new();
        for alias in aliases {
            let rest = if alias.is_wildcard {
                specifier.strip_prefix(alias.prefix.as_str())
            } else {
                (specifier == alias.prefix).then_some("")
            };
            let Some(rest) = rest else { continue };
            found.extend(alias.targets.iter().map(|target| format!("{target}{rest}")));
        }
        found
    }

    /// The aliases of the nearest config above `file`.
    fn nearest(&self, file: &str) -> Option<&Vec<Alias>> {
        let mut directory = Path::new(file).parent();
        while let Some(current) = directory {
            let key = current.to_string_lossy();
            if let Some(aliases) = self.configs.get(key.as_ref()) {
                return Some(aliases);
            }
            directory = current.parent();
        }
        None
    }
}

fn is_tsconfig(path: &str) -> bool {
    let name = file_name(path);
    name.starts_with("tsconfig") && is_json(name)
}

/// A config's aliases, from it or the nearest config it extends that has
/// `paths`.
fn resolve_aliases(path: &str, configs: &BTreeMap<String, Config>) -> Vec<Alias> {
    let mut current = path.to_owned();
    for _ in 0..MAX_EXTENDS {
        let Some(config) = configs.get(&current) else {
            return Vec::new();
        };
        if let Some(paths) = &config.paths {
            let base = base_directory(&current, config.base_url.as_deref());
            return aliases(paths, &base);
        }
        let Some(extends) = config
            .extends
            .as_deref()
            .filter(|target| target.starts_with('.'))
        else {
            return Vec::new();
        };
        let mut next = join(&parent(&current), extends);
        if !is_json(&next) {
            next.push_str(".json");
        }
        current = next;
    }
    Vec::new()
}

fn aliases(paths: &serde_json::Map<String, Json>, base: &str) -> Vec<Alias> {
    paths
        .iter()
        .map(|(pattern, targets)| {
            let is_wildcard = pattern.ends_with('*');
            let targets = targets
                .as_array()
                .into_iter()
                .flatten()
                .filter_map(Json::as_str)
                .map(|target| join(base, target.trim_end_matches('*')))
                .collect();
            Alias {
                prefix: pattern.trim_end_matches('*').to_owned(),
                is_wildcard,
                targets,
            }
        })
        .collect()
}

/// Where `paths` targets are relative to: `baseUrl` if set, otherwise the
/// config's directory.
fn base_directory(config: &str, base_url: Option<&str>) -> String {
    let directory = parent(config);
    match base_url {
        Some(base_url) => join(&directory, base_url),
        None => directory,
    }
}

fn read_config(text: &str) -> Option<Config> {
    let json: Json = serde_json::from_str(&strip_jsonc(text)).ok()?;
    let options = json.get("compilerOptions");
    Some(Config {
        paths: options
            .and_then(|options| options.get("paths"))
            .and_then(Json::as_object)
            .cloned(),
        base_url: options
            .and_then(|options| options.get("baseUrl"))
            .and_then(Json::as_str)
            .map(str::to_owned),
        extends: json
            .get("extends")
            .and_then(Json::as_str)
            .map(str::to_owned),
    })
}

/// `tsconfig` files allow comments and trailing commas; JSON does not.
fn strip_jsonc(text: &str) -> String {
    let mut out = String::with_capacity(text.len());
    let mut characters = text.chars().peekable();
    let mut in_string = false;
    while let Some(character) = characters.next() {
        if in_string {
            out.push(character);
            if character == '\\' {
                out.extend(characters.next());
            } else if character == '"' {
                in_string = false;
            }
            continue;
        }
        match (character, characters.peek()) {
            ('"', _) => {
                in_string = true;
                out.push(character);
            }
            ('/', Some('/')) => {
                skip_line(&mut characters);
                out.push('\n');
            }
            ('/', Some('*')) => {
                characters.next();
                skip_block(&mut characters);
            }
            _ => out.push(character),
        }
    }
    remove_trailing_commas(&out)
}

/// Skips past the end of a `//` comment's line.
fn skip_line(characters: &mut std::iter::Peekable<std::str::Chars<'_>>) {
    for skipped in characters.by_ref() {
        if skipped == '\n' {
            return;
        }
    }
}

/// Skips past the `*/` ending a block comment.
fn skip_block(characters: &mut std::iter::Peekable<std::str::Chars<'_>>) {
    let mut previous = ' ';
    for skipped in characters.by_ref() {
        if previous == '*' && skipped == '/' {
            return;
        }
        previous = skipped;
    }
}

fn remove_trailing_commas(text: &str) -> String {
    let characters: Vec<char> = text.chars().collect();
    let mut out = String::with_capacity(text.len());
    let mut in_string = false;
    for (index, &character) in characters.iter().enumerate() {
        if character == '"' && (index == 0 || characters[index - 1] != '\\') {
            in_string = !in_string;
        }
        if character == ',' && !in_string {
            let next = characters[index + 1..].iter().find(|c| !c.is_whitespace());
            if matches!(next, Some('}' | ']')) {
                continue;
            }
        }
        out.push(character);
    }
    out
}

fn is_json(path: &str) -> bool {
    Path::new(path)
        .extension()
        .is_some_and(|extension| extension.eq_ignore_ascii_case("json"))
}

fn file_name(path: &str) -> &str {
    path.rsplit('/').next().unwrap_or(path)
}

fn parent(path: &str) -> String {
    Path::new(path)
        .parent()
        .map(|parent| parent.to_string_lossy().into_owned())
        .unwrap_or_default()
}

/// Joins a relative path to a directory, resolving `.` and `..`.
fn join(directory: &str, relative: &str) -> String {
    let mut parts: Vec<String> = Vec::new();
    for component in PathBuf::from(directory).join(relative).components() {
        match component {
            Component::Normal(part) => parts.push(part.to_string_lossy().into_owned()),
            Component::ParentDir => {
                parts.pop();
            }
            _ => {}
        }
    }
    let joined = parts.join("/");
    if relative.ends_with('/') && !joined.is_empty() {
        return format!("{joined}/");
    }
    joined
}

#[cfg(test)]
mod tests {
    use super::*;

    fn file(path: &str, contents: &str) -> SourceFile {
        SourceFile {
            path: path.to_owned(),
            contents: contents.to_owned(),
        }
    }

    #[test]
    fn the_nearest_config_maps_aliases_to_its_directories() {
        let files = [
            file(
                "apps/web/tsconfig.json",
                r#"{
                  // comments and trailing commas are allowed
                  "compilerOptions": { "paths": { "~/*": ["./src/*"], "@config": ["./config.ts"], }, },
                }"#,
            ),
            file(
                "apps/api/tsconfig.json",
                r#"{ "extends": "../../tsconfig.base.json" }"#,
            ),
            file(
                "tsconfig.base.json",
                r#"{ "compilerOptions": { "baseUrl": ".", "paths": { "@/*": ["apps/api/*"] } } }"#,
            ),
        ];

        let aliases = PathAliases::from_files(&files);

        assert_eq!(
            aliases.candidates("apps/web/src/app/page.tsx", "~/server/trpc"),
            ["apps/web/src/server/trpc"]
        );
        assert_eq!(
            aliases.candidates("apps/web/src/a.ts", "@config"),
            ["apps/web/config.ts"]
        );
        assert_eq!(
            aliases.candidates("apps/api/routes/x.ts", "@/lib/schemas"),
            ["apps/api/lib/schemas"]
        );
        assert_eq!(
            aliases.candidates("other/x.ts", "~/server"),
            Vec::<String>::new()
        );
    }
}
