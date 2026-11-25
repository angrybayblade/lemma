//! What a change to one repository's working directory reaches, by name:
//! the functions whose lines the change touched, and every function that
//! mentions one of them, transitively.
//!
//! Experimental (the graph walk of the north star): evidence a behavior
//! keeps is reused when the change reaches none of the code the checker is
//! given. Mentions are names, not resolved references, so this
//! over-approximates, which only costs reuse; a change Lemma cannot read
//! as functions (a changed `go.mod`, a script, a deleted file, a plain
//! JavaScript file outside the code tracing reads) reaches everything.
//!
//! Plain JavaScript has no language of its own in Lemma's model, so nothing
//! says how its top-level code gives a name to a function
//! (`exports.check = function () {}`, a property of `module.exports`): that
//! code may define any name. A change to it, or a name that reaches it,
//! reaches the file's whole language family.

// TODO: a name an import gives another name is not followed under the new
// one: a default import (`import check from './glue'`), `import { a as b }`,
// `from x import a as b`. Closes when imports are resolved to what they
// name.

// TODO: when JavaScript becomes a language of its own, read the names its
// top-level code exports, and follow those instead of reaching the family.

use std::collections::{HashMap, HashSet, VecDeque};
use std::path::Path;
use std::process::Command;

use lemma_model::Language;

use crate::affects;
use crate::functions::{Functions, NameId};

/// What a change reaches in one repository.
#[derive(Debug, Clone)]
pub(crate) struct ChangeReach {
    /// The repository, as the workspace names it.
    pub(crate) repository: String,
    /// The base commit the change is measured from.
    pub(crate) base: String,
    /// Whether the change touched something not read as functions, so it
    /// may reach anything.
    pub(crate) everything: bool,
    /// The last names of the functions reached.
    reached: HashSet<String>,
    /// Files whose functions are reached, by path in the repository.
    files: HashSet<String>,
    /// The language families the reached names are in.
    families: HashSet<u8>,
    /// The language families reached whole: the change, or a name it
    /// reaches, is in top-level code that may define any name.
    whole_families: HashSet<u8>,
    /// For operations a document declares (`service.operationId`), the
    /// files their routes are declared in: where their code is found.
    pub(crate) route_files: HashMap<String, Vec<String>>,
}

impl ChangeReach {
    /// Whether the change may reach `symbol` in `file`.
    pub(crate) fn reaches(&self, file: &str, symbol: &str) -> bool {
        let family = family(file);
        if self.everything || self.whole_families.contains(&family) {
            return true;
        }
        // A file the change touched is reached whole, though the change
        // seeded no name: a route file holding only anonymous handlers.
        if symbol.is_empty() && self.files.contains(file) {
            return true;
        }
        if !self.families.contains(&family) {
            return false;
        }
        let name = symbol.rsplit(['.', ':']).next().unwrap_or(symbol);
        self.reached.contains(name)
    }

    /// How many functions' names the change reaches, for the log.
    pub(crate) fn len(&self) -> usize {
        self.reached.len()
    }
}

/// The family of languages whose names can refer to one another, as a
/// number from 1: a TypeScript change reaches Go code only through a
/// contract or a link, never by a name its code happens to share. 0 for a
/// file that is not code.
fn family(path: &str) -> u8 {
    affects::functions_language(path)
        .and_then(|language| u8::try_from(language.family().position() + 1).ok())
        .unwrap_or(0)
}

/// Whether a file is code whose functions tracing reads, so a change to it
/// can be followed by name.
fn is_read_code(path: &str) -> bool {
    affects::functions_language(path).is_some()
}

/// Whether a file's top-level code may define any name: code in no language
/// of Lemma's model, as plain JavaScript, which another language's grammar
/// reads.
fn defines_names_loosely(path: &str) -> bool {
    let extension = extension_of(path);
    Language::is_code_extension(extension) && Language::from_extension(extension).is_none()
}

fn extension_of(path: &str) -> &str {
    Path::new(path)
        .extension()
        .and_then(|extension| extension.to_str())
        .unwrap_or("")
}

/// Lines a change touched in each changed file, at the working directory,
/// against `base`; `None` when Git cannot say.
/// Each changed file's path with the line ranges, 1-based and inclusive,
/// a change touched in it.
type ChangedLines = Vec<(String, Vec<(u32, u32)>)>;

/// Each changed file against `base` with the lines changed in it, new code
/// among `untracked` counting whole; `None` when Git cannot say, or the
/// untracked files are not known.
fn changed_lines(root: &Path, base: &str, untracked: Option<&[String]>) -> Option<ChangedLines> {
    let output = Command::new("git")
        .arg("-C")
        .arg(root)
        .args(["diff", "-U0", "--no-color", "--no-ext-diff", base, "--"])
        .output()
        .ok()?;
    if !output.status.success() {
        return None;
    }
    let text = String::from_utf8_lossy(&output.stdout);
    let mut files: Vec<(String, Vec<(u32, u32)>)> = Vec::new();
    for line in text.lines() {
        if let Some(path) = line.strip_prefix("+++ b/") {
            files.push((path.to_owned(), Vec::new()));
            continue;
        }
        if line.starts_with("+++ /dev/null") {
            // A deleted file: its functions are gone, and what used them is
            // not found by name in the head. Reaches everything.
            files.push((String::new(), Vec::new()));
            continue;
        }
        let Some(rest) = line.strip_prefix("@@ ") else {
            continue;
        };
        let Some(range) = rest.split(' ').find(|part| part.starts_with('+')) else {
            continue;
        };
        let (start, count) = match range[1..].split_once(',') {
            Some((start, count)) => (start.parse().ok()?, count.parse::<u32>().ok()?),
            None => (range[1..].parse().ok()?, 1),
        };
        // A pure deletion (`+12,0`) touches the lines on either side.
        let (first, last) = if count == 0 {
            (start, start + 1)
        } else {
            (start, start + count - 1)
        };
        if let Some((_, ranges)) = files.last_mut() {
            ranges.push((first, last));
        }
    }
    // New files that are not code (notes, a description being written) are
    // read by nothing a checker is given; new code counts whole.
    for path in untracked? {
        if Language::is_code_extension(extension_of(path)) {
            files.push((path.clone(), vec![(1, u32::MAX)]));
        }
    }
    Some(files)
}

/// What a change touches in one file.
#[derive(Default)]
struct Touched {
    /// The names of the symbols touched, each with its language family.
    names: Vec<(String, u8)>,
    /// Whether the change is on a line of code no symbol holds.
    is_outside_symbols: bool,
}

/// What a change on `ranges` (1-based lines) touches in `path`, at the
/// working directory: each symbol overlapping a range and its container, or
/// every symbol of the file when a range overlaps none. A file that cannot
/// be read counts as changed outside its symbols.
fn symbols_touched(root: &Path, path: &str, ranges: &[(u32, u32)]) -> Touched {
    let language = family(path);
    let unread = Touched {
        names: Vec::new(),
        is_outside_symbols: true,
    };
    let Some(kind) = affects::functions_language(path) else {
        return unread;
    };
    let Ok(source) = std::fs::read_to_string(root.join(path)) else {
        return unread;
    };
    let Ok(mut extractor) = lemma_syntax::Extractor::new() else {
        return unread;
    };
    let Ok(extraction) = extractor.extract(kind, &source) else {
        return unread;
    };
    let lines =
        |symbol: &lemma_model::Symbol| (symbol.span.start.line + 1, symbol.span.end.line + 1);
    let spans: Vec<(u32, u32)> = extraction.symbols.iter().map(lines).collect();
    let mut names = Vec::new();
    for (start, end) in ranges {
        let touched: Vec<&lemma_model::Symbol> = extraction
            .symbols
            .iter()
            .filter(|symbol| {
                let (first, last) = lines(symbol);
                *start <= last && first <= *end
            })
            .collect();
        let chosen: Vec<&lemma_model::Symbol> = if touched.is_empty() {
            extraction.symbols.iter().collect()
        } else {
            touched
        };
        for symbol in chosen {
            names.push((symbol.name.clone(), language));
            if let Some((container, _)) = symbol.qualified_name.rsplit_once('.') {
                let container = container.rsplit('.').next().unwrap_or(container);
                names.push((container.to_owned(), language));
            }
        }
    }
    Touched {
        names,
        is_outside_symbols: is_outside(&source, ranges, &spans),
    }
}

/// Whether one of `ranges` (1-based lines) holds a line of `source` that is
/// not blank and that none of `spans` holds.
fn is_outside(source: &str, ranges: &[(u32, u32)], spans: &[(u32, u32)]) -> bool {
    let is_held = |line: u32| {
        spans
            .iter()
            .any(|(first, last)| *first <= line && line <= *last)
    };
    source.lines().zip(1u32..).any(|(text, line)| {
        !text.trim().is_empty()
            && !is_held(line)
            && ranges
                .iter()
                .any(|(start, end)| *start <= line && line <= *end)
    })
}

/// What the working directory's change against `base` reaches among
/// `functions`, the repository's functions at the working directory, given
/// its `untracked` files (see [`lemma_http::Listings::untracked`]): when
/// those are not known, everything.
pub(crate) fn change_reach(
    repository: &str,
    (root, base): (&Path, &str),
    untracked: Option<&[String]>,
    functions: &Functions,
) -> ChangeReach {
    let mut reach = ChangeReach {
        repository: repository.to_owned(),
        base: base.to_owned(),
        everything: false,
        reached: HashSet::new(),
        files: HashSet::new(),
        families: HashSet::new(),
        whole_families: HashSet::new(),
        route_files: HashMap::new(),
    };
    let changed = changed_lines(root, base, untracked)
        .filter(|changed| is_read_as_functions(changed, functions));
    let Some(changed) = changed else {
        reach.everything = true;
        return reach;
    };
    // A changed file none of whose symbols hold the change still counts as
    // touched.
    reach
        .files
        .extend(changed.iter().map(|(path, _)| path.clone()));
    let seeds = seed_names(root, &changed, functions);
    reach.whole_families.extend(seeds.whole_families);
    walk(&mut reach, seeds.names, functions);
    reach
}

/// Whether every changed file is code whose functions are read, or a
/// description whose changes the evidence's key holds already: anything
/// else (a deleted file, a changed `go.mod` or script) may reach anything.
/// So may a file whose top-level code defines names loosely when
/// `functions` hold nothing of it, since that code is then not known.
fn is_read_as_functions(changed: &ChangedLines, functions: &Functions) -> bool {
    let is_description = |path: &str| {
        Path::new(path)
            .extension()
            .is_some_and(|extension| extension.eq_ignore_ascii_case("lm"))
    };
    let is_followed = changed
        .iter()
        .all(|(path, _)| !path.is_empty() && (is_read_code(path) || is_description(path)));
    if !is_followed {
        return false;
    }
    let loose: HashSet<&str> = changed
        .iter()
        .map(|(path, _)| path.as_str())
        .filter(|path| defines_names_loosely(path))
        .collect();
    if loose.is_empty() {
        return true;
    }
    let held: HashSet<&str> = (0..functions.len())
        .map(|function| functions.file_text(function))
        .filter(|file| loose.contains(file))
        .collect();
    held.len() == loose.len()
}

/// What a walk starts from.
#[derive(Default)]
struct Seeds {
    /// The names to follow, each with its language family.
    names: Vec<(String, u8)>,
    /// The language families the change reaches whole.
    whole_families: HashSet<u8>,
}

/// What a walk starts from: every symbol Tree-sitter finds where the change
/// is (types, fields and constants included, which no function record
/// holds) with its container, and the callable names of each function whose
/// lines the change touched (its own, a method's type, and the names
/// top-level code binds). A change outside the symbols of a file whose
/// top-level code may define any name reaches the file's family whole.
fn seed_names(root: &Path, changed: &ChangedLines, functions: &Functions) -> Seeds {
    let mut seeds = Seeds::default();
    for (path, ranges) in changed {
        let touched = symbols_touched(root, path, ranges);
        seeds.names.extend(touched.names);
        if touched.is_outside_symbols && defines_names_loosely(path) {
            seeds.whole_families.insert(family(path));
        }
    }
    let touched: HashMap<&str, &Vec<(u32, u32)>> = changed
        .iter()
        .map(|(path, ranges)| (path.as_str(), ranges))
        .collect();
    for function in 0..functions.len() {
        let Some(ranges) = touched.get(functions.file_text(function)) else {
            continue;
        };
        let (first, last) = functions.lines(function);
        if !ranges
            .iter()
            .any(|(start, end)| *start <= last && first <= *end)
        {
            continue;
        }
        let language = family(functions.file_text(function));
        for name in functions.callable_names(function) {
            seeds
                .names
                .push((functions.text(*name).to_owned(), language));
        }
    }
    seeds
}

/// Follows names outwards from `seeds` into `reach`: every function that
/// mentions a reached name, in the same language family, is reached by its
/// callable names in turn. Top-level code that may define any name, once
/// reached, reaches its family whole.
fn walk(reach: &mut ChangeReach, seeds: Vec<(String, u8)>, functions: &Functions) {
    // Names are followed by their ids in the table; the work is per
    // mention, so strings and hashing stay out of it. A name only travels
    // in its seed's family, so only those families' functions are indexed.
    let family_of: Vec<u8> = (0..functions.len())
        .map(|function| family(functions.file_text(function)))
        .collect();
    let seed_families: HashSet<u8> = seeds.iter().map(|(_, language)| *language).collect();
    let mut mentioned_by: Vec<Vec<u32>> = vec![Vec::new(); functions.name_count()];
    for (function, language) in family_of.iter().enumerate() {
        if !seed_families.contains(language) {
            continue;
        }
        let Ok(index) = u32::try_from(function) else {
            continue;
        };
        for name in functions.mentions(function) {
            mentioned_by[*name as usize].push(index);
        }
    }
    let mut walked = Walked {
        reach,
        functions,
        seen: vec![0; functions.name_count()],
        queue: VecDeque::new(),
    };
    for (name, language) in seeds {
        walked.reach_seed(&name, language);
    }
    let mut files: HashSet<NameId> = HashSet::new();
    while let Some((name, language)) = walked.queue.pop_front() {
        for function in &mentioned_by[name as usize] {
            let function = *function as usize;
            if family_of[function] != language {
                continue;
            }
            files.insert(functions.file(function));
            if functions.is_unnamed(function)
                && defines_names_loosely(functions.file_text(function))
            {
                walked.reach.whole_families.insert(language);
            }
            for reached in functions.callable_names(function) {
                walked.reach_name(*reached, language);
            }
        }
    }
    let reach = walked.reach;
    reach.files.extend(
        files
            .into_iter()
            .map(|file| functions.text(file).to_owned()),
    );
}

/// A walk in progress: what it reached, and the names still to follow.
struct Walked<'a> {
    reach: &'a mut ChangeReach,
    functions: &'a Functions,
    /// For each name, a bit for each language family it was reached in.
    seen: Vec<u8>,
    queue: VecDeque<(NameId, u8)>,
}

impl Walked<'_> {
    /// Reaches the seed `name`, which the table may not hold (a type or
    /// field no function record names), and follows it if it does.
    fn reach_seed(&mut self, name: &str, language: u8) {
        if name.is_empty() {
            return;
        }
        if let Some(id) = self.functions.lookup(name) {
            self.reach_name(id, language);
            return;
        }
        self.reach.reached.insert(name.to_owned());
        self.reach.families.insert(language);
    }

    /// Reaches the name `id` in `language`'s family, once, and queues it
    /// to follow.
    fn reach_name(&mut self, id: NameId, language: u8) {
        let text = self.functions.text(id);
        let bit = 1u8 << language;
        let seen = &mut self.seen[id as usize];
        if text.is_empty() || *seen & bit != 0 {
            return;
        }
        *seen |= bit;
        self.reach.reached.insert(text.to_owned());
        self.reach.families.insert(language);
        self.queue.push_back((id, language));
    }
}

#[cfg(test)]
mod measure;

#[cfg(test)]
mod tests {
    use std::collections::BTreeSet;
    use std::process::Command;

    use super::*;
    use crate::impact::Function;

    fn git(root: &Path, args: &[&str]) {
        let status = Command::new("git")
            .arg("-C")
            .arg(root)
            .args([
                "-c",
                "user.name=t",
                "-c",
                "user.email=t@t",
                "-c",
                "commit.gpgsign=false",
            ])
            .args(args)
            .status()
            .unwrap();
        assert!(status.success(), "git {args:?}");
    }

    fn write(root: &Path, path: &str, text: &str) {
        let file = root.join(path);
        std::fs::create_dir_all(file.parent().unwrap()).unwrap();
        std::fs::write(file, text).unwrap();
    }

    fn function(file: &str, name: &str, lines: (u32, u32), mentions: &[&str]) -> Function {
        Function {
            file: file.to_owned(),
            name: name.to_owned(),
            qualified_name: name.to_owned(),
            kind: "function".to_owned(),
            lines,
            mentions: mentions
                .iter()
                .map(|name| (*name).to_owned())
                .collect::<BTreeSet<_>>(),
            binds: Vec::new(),
        }
    }

    #[test]
    fn a_walk_follows_mentions_transitively_within_one_family() {
        let functions = Functions::from_functions(&[
            function("api/handle.ts", "handle", (1, 5), &["Request"]),
            function("api/route.ts", "route", (1, 5), &["handle"]),
            function("api/other.ts", "other", (1, 5), &["unrelated"]),
            function("server/setup.go", "Setup", (1, 5), &["Request"]),
        ]);
        let mut reach = ChangeReach {
            repository: "#billing".to_owned(),
            base: "abc".to_owned(),
            everything: false,
            reached: HashSet::new(),
            files: HashSet::new(),
            families: HashSet::new(),
            whole_families: HashSet::new(),
            route_files: HashMap::new(),
        };
        let seeds = vec![("Request".to_owned(), 1), ("NotInTable".to_owned(), 1)];

        walk(&mut reach, seeds, &functions);

        let reached: BTreeSet<&str> = reach.reached.iter().map(String::as_str).collect();
        let files: BTreeSet<&str> = reach.files.iter().map(String::as_str).collect();
        assert_eq!(
            reached,
            BTreeSet::from(["NotInTable", "Request", "handle", "route"])
        );
        assert_eq!(files, BTreeSet::from(["api/handle.ts", "api/route.ts"]));
    }

    const SCHEMA: &str =
        "import { z } from 'zod'\nexport const Request = z.object({\n  id: z.string(),\n})\n";
    const ROUTE: &str = "import { Request } from './schema'\nexport function handle(body) {\n  return Request.parse(body)\n}\n";
    const GO: &str = "package handlers\n\ntype Request struct {\n\tID string\n}\n\nfunc Setup() Request {\n\treturn Request{}\n}\n";

    /// A repository with a TypeScript schema and its route, and a Go
    /// handler using a type of the same name, committed.
    fn repository() -> tempfile::TempDir {
        let root = tempfile::tempdir().unwrap();
        write(root.path(), "src/schema.ts", SCHEMA);
        write(root.path(), "src/route.ts", ROUTE);
        write(root.path(), "handlers/setup.go", GO);
        write(root.path(), "go.mod", "module example.test/handlers\n");
        git(root.path(), &["init", "-q"]);
        git(root.path(), &["add", "."]);
        git(root.path(), &["commit", "-q", "-m", "base"]);
        root
    }

    /// The untracked files a listing of `root`'s working directory names.
    fn untracked(root: &Path) -> Vec<String> {
        lemma_http::Listings::default()
            .untracked(root)
            .unwrap()
            .to_vec()
    }

    fn functions() -> Functions {
        Functions::from_functions(&[
            function(
                "src/route.ts",
                "handle",
                (2, 4),
                &["Request", "parse", "body"],
            ),
            function("handlers/setup.go", "Setup", (7, 9), &["Request"]),
        ])
    }

    #[test]
    fn a_changed_schema_reaches_the_code_using_it_in_its_own_language_only() {
        let root = repository();
        write(
            root.path(),
            "src/schema.ts",
            &SCHEMA.replace(
                "  id: z.string(),\n",
                "  id: z.string(),\n  uuid: z.string(),\n",
            ),
        );

        let reach = change_reach(
            "#r",
            (root.path(), "HEAD"),
            Some(&untracked(root.path())),
            &functions(),
        );

        assert!(!reach.everything);
        assert!(reach.reaches("src/route.ts", "handle"));
        assert!(!reach.reaches("handlers/setup.go", "Setup"));
    }

    #[test]
    fn a_changed_struct_no_function_holds_reaches_the_code_using_it() {
        let root = repository();
        write(
            root.path(),
            "handlers/setup.go",
            &GO.replace("\tID string\n", "\tID string\n\tURL string\n"),
        );

        let reach = change_reach(
            "#r",
            (root.path(), "HEAD"),
            Some(&untracked(root.path())),
            &functions(),
        );

        assert!(reach.reaches("handlers/setup.go", "Setup"));
        assert!(!reach.reaches("src/route.ts", "handle"));
    }

    #[test]
    fn new_code_counts_whole_and_reaches_what_mentions_it() {
        let root = repository();
        write(
            root.path(),
            "src/extra.ts",
            "export function parse(input) {\n  return input\n}\n",
        );

        let reach = change_reach(
            "#r",
            (root.path(), "HEAD"),
            Some(&untracked(root.path())),
            &functions(),
        );

        assert!(!reach.everything);
        assert!(reach.reaches("src/route.ts", "handle"));
    }

    #[test]
    fn untracked_files_not_known_reach_everything() {
        let root = repository();

        let reach = change_reach("#r", (root.path(), "HEAD"), None, &functions());

        assert!(reach.everything);
    }

    #[test]
    fn a_changed_file_that_is_not_code_reaches_everything() {
        let root = repository();
        write(
            root.path(),
            "go.mod",
            "module example.test/handlers\n\ngo 1.22\n",
        );

        let reach = change_reach(
            "#r",
            (root.path(), "HEAD"),
            Some(&untracked(root.path())),
            &functions(),
        );

        assert!(reach.everything);
        assert!(reach.reaches("src/route.ts", "anything"));
    }

    const PLAIN_JAVASCRIPT: [&str; 4] = [
        "src/parse.js",
        "src/parse.jsx",
        "src/parse.mjs",
        "src/parse.cjs",
    ];
    const PARSE: &str = "export function parse(input) {\n  return input\n}\n";

    /// `repository()` with more files committed.
    fn repository_with(files: &[(&str, &str)]) -> tempfile::TempDir {
        let root = repository();
        for (path, text) in files {
            write(root.path(), path, text);
        }
        git(root.path(), &["add", "."]);
        git(root.path(), &["commit", "-q", "-m", "more"]);
        root
    }

    /// The functions tracing reads from `paths` in `root`'s working
    /// directory.
    fn functions_read(root: &Path, paths: &[&str]) -> Functions {
        measure::read_tree(root, &paths.join("\n"))
    }

    fn reach_of(root: &Path, functions: &Functions) -> ChangeReach {
        change_reach("#r", (root, "HEAD"), Some(&untracked(root)), functions)
    }

    #[test]
    fn a_changed_function_in_plain_javascript_reaches_what_mentions_it() {
        for path in PLAIN_JAVASCRIPT {
            let root = repository_with(&[(path, PARSE)]);
            write(
                root.path(),
                path,
                &PARSE.replace("input\n}", "input.trim()\n}"),
            );
            let functions =
                functions_read(root.path(), &[path, "src/route.ts", "handlers/setup.go"]);

            let reach = reach_of(root.path(), &functions);

            assert!(!reach.everything, "{path}");
            assert!(reach.reaches("src/route.ts", "handle"), "{path}");
            assert!(!reach.reaches("handlers/setup.go", "Setup"), "{path}");
        }
    }

    #[test]
    fn a_new_plain_javascript_file_reaches_what_mentions_its_names() {
        for path in PLAIN_JAVASCRIPT {
            let root = repository();
            write(root.path(), path, PARSE);
            let functions =
                functions_read(root.path(), &[path, "src/route.ts", "handlers/setup.go"]);

            let reach = reach_of(root.path(), &functions);

            assert!(!reach.everything, "{path}");
            assert!(reach.reaches("src/route.ts", "handle"), "{path}");
            assert!(!reach.reaches("handlers/setup.go", "Setup"), "{path}");
        }
    }

    #[test]
    fn a_changed_plain_javascript_file_tracing_does_not_read_reaches_everything() {
        let root = repository_with(&[("scripts/build.js", "module.exports = { minify: true }\n")]);
        write(
            root.path(),
            "scripts/build.js",
            "module.exports = { minify: false }\n",
        );

        let reach = reach_of(root.path(), &functions());

        assert!(reach.everything);
    }

    #[test]
    fn code_in_a_plain_javascript_file_is_reached_only_by_a_change_that_names_it() {
        let root = repository_with(&[("src/parse.js", PARSE)]);
        write(
            root.path(),
            "handlers/setup.go",
            &GO.replace("\tID string\n", "\tID string\n\tURL string\n"),
        );
        let functions = functions_read(
            root.path(),
            &["src/parse.js", "src/route.ts", "handlers/setup.go"],
        );

        let reach = reach_of(root.path(), &functions);

        assert!(reach.reaches("handlers/setup.go", "Setup"));
        assert!(!reach.reaches("src/parse.js", "parse"));
    }

    const GLUE: &str = "const { Request } = require('./schema')\n\nexports.check = function (body) {\n  return Request.parse(body)\n}\n";
    const HANDLER: &str =
        "import { check } from './glue'\nexport function accept(body) {\n  return check(body)\n}\n";
    const GLUED: [&str; 3] = ["src/schema.ts", "src/glue.js", "src/handler.ts"];

    #[test]
    fn a_name_is_followed_through_a_plain_javascript_file() {
        let root = repository_with(&[("src/glue.js", GLUE), ("src/handler.ts", HANDLER)]);
        write(
            root.path(),
            "src/schema.ts",
            &SCHEMA.replace(
                "  id: z.string(),\n",
                "  id: z.string(),\n  uuid: z.string(),\n",
            ),
        );
        let functions = functions_read(root.path(), &GLUED);

        let reach = reach_of(root.path(), &functions);

        assert!(!reach.everything);
        assert!(reach.reaches("src/glue.js", "check"));
        assert!(reach.reaches("src/handler.ts", "accept"));
        assert!(!reach.reaches("handlers/setup.go", "Setup"));
    }

    #[test]
    fn a_change_outside_a_plain_javascript_file_s_symbols_reaches_its_whole_family() {
        let root = repository_with(&[("src/glue.js", GLUE), ("src/handler.ts", HANDLER)]);
        write(
            root.path(),
            "src/glue.js",
            &GLUE.replace("parse(body)", "parse(body ?? {})"),
        );
        let functions = functions_read(root.path(), &GLUED);

        let reach = reach_of(root.path(), &functions);

        assert!(!reach.everything);
        assert!(reach.reaches("src/handler.ts", "accept"));
        assert!(reach.reaches("src/route.ts", "handle"));
        assert!(!reach.reaches("handlers/setup.go", "Setup"));
    }

    #[test]
    fn a_changed_file_holding_no_symbol_is_itself_reached() {
        const ROUTES: &str = "import { router } from './router'\nrouter.get('/items', (request, response) => {\n  response.json([])\n})\n";
        let root = repository_with(&[("src/routes.ts", ROUTES)]);
        write(root.path(), "src/routes.ts", &ROUTES.replace("[]", "[1]"));

        let reach = change_reach(
            "#r",
            (root.path(), "HEAD"),
            Some(&untracked(root.path())),
            &functions(),
        );

        assert!(!reach.everything);
        assert!(reach.reaches("src/routes.ts", ""));
        assert!(!reach.reaches("src/route.ts", ""));
    }
}
