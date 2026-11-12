//! What a change to one repository's working directory reaches, by name:
//! the functions whose lines the change touched, and every function that
//! mentions one of them, transitively.
//!
//! Experimental (the graph walk of the north star): evidence a behavior
//! keeps is reused when the change reaches none of the code the checker is
//! given. Mentions are names, not resolved references, so this
//! over-approximates, which only costs reuse; a change Lemma cannot read
//! as functions (a changed `go.mod`, a script, a deleted file) reaches
//! everything.

use std::collections::{HashMap, HashSet, VecDeque};
use std::path::Path;
use std::process::Command;

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
    /// For operations a document declares (`service.operationId`), the
    /// files their routes are declared in: where their code is found.
    pub(crate) route_files: HashMap<String, Vec<String>>,
}

impl ChangeReach {
    /// Whether the change may reach `symbol` in `file`.
    pub(crate) fn reaches(&self, file: &str, symbol: &str) -> bool {
        if self.everything {
            return true;
        }
        if !self.families.contains(&family(file)) {
            return false;
        }
        let name = symbol.rsplit(['.', ':']).next().unwrap_or(symbol);
        self.reached.contains(name) || (symbol.is_empty() && self.files.contains(file))
    }

    /// How many functions' names the change reaches, for the log.
    pub(crate) fn len(&self) -> usize {
        self.reached.len()
    }
}

const CODE_EXTENSIONS: [&str; 9] = ["ts", "tsx", "mts", "cts", "js", "jsx", "py", "go", "rs"];

/// The family of languages whose names can refer to one another: a
/// TypeScript change reaches Go code only through a contract or a link,
/// never by a name its code happens to share.
fn family(path: &str) -> u8 {
    let extension = Path::new(path)
        .extension()
        .and_then(|extension| extension.to_str())
        .unwrap_or("");
    match extension {
        "ts" | "tsx" | "mts" | "cts" | "js" | "jsx" => 1,
        "py" => 2,
        "go" => 3,
        "rs" => 4,
        _ => 0,
    }
}

fn is_code(path: &str) -> bool {
    Path::new(path)
        .extension()
        .and_then(|extension| extension.to_str())
        .is_some_and(|extension| CODE_EXTENSIONS.contains(&extension))
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
        if is_code(path) {
            files.push((path.clone(), vec![(1, u32::MAX)]));
        }
    }
    Some(files)
}

/// The names of the symbols in `path`, at the working directory, that a
/// change on `ranges` (1-based lines) touches, with their language family:
/// each symbol overlapping a range and its container, or every symbol of
/// the file when a range overlaps none.
fn symbols_touched(root: &Path, path: &str, ranges: &[(u32, u32)]) -> Vec<(String, u8)> {
    let language = family(path);
    let extension = Path::new(path)
        .extension()
        .and_then(|extension| extension.to_str())
        .unwrap_or("");
    let Some(kind) = lemma_model::Language::from_extension(extension) else {
        return Vec::new();
    };
    let Ok(source) = std::fs::read_to_string(root.join(path)) else {
        return Vec::new();
    };
    let Ok(mut extractor) = lemma_syntax::Extractor::new() else {
        return Vec::new();
    };
    let Ok(extraction) = extractor.extract(kind, &source) else {
        return Vec::new();
    };
    let lines =
        |symbol: &lemma_model::Symbol| (symbol.span.start.line + 1, symbol.span.end.line + 1);
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
    names
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
        route_files: HashMap::new(),
    };
    let Some(changed) = changed_lines(root, base, untracked).filter(is_read_as_functions) else {
        reach.everything = true;
        return reach;
    };
    // A changed file none of whose symbols hold the change still counts as
    // touched.
    reach
        .files
        .extend(changed.iter().map(|(path, _)| path.clone()));
    let seeds = seed_names(root, &changed, functions);
    walk(&mut reach, seeds, functions);
    reach
}

/// Whether every changed file is code, or a description whose changes the
/// evidence's key holds already: anything else (a deleted file, a changed
/// `go.mod` or script) may reach anything.
fn is_read_as_functions(changed: &ChangedLines) -> bool {
    let is_description = |path: &str| {
        Path::new(path)
            .extension()
            .is_some_and(|extension| extension.eq_ignore_ascii_case("lm"))
    };
    changed
        .iter()
        .all(|(path, _)| !path.is_empty() && (is_code(path) || is_description(path)))
}

/// The names a walk starts from, each with its language family: every
/// symbol Tree-sitter finds where the change is (types, fields and constants
/// included, which no function record holds) with its container, and the
/// callable names of each function whose lines the change touched (its own,
/// a method's type, and the names top-level code binds).
fn seed_names(root: &Path, changed: &ChangedLines, functions: &Functions) -> Vec<(String, u8)> {
    let mut seeds = Vec::new();
    for (path, ranges) in changed {
        seeds.extend(symbols_touched(root, path, ranges));
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
            seeds.push((functions.text(*name).to_owned(), language));
        }
    }
    seeds
}

/// Follows names outwards from `seeds` into `reach`: every function that
/// mentions a reached name, in the same language family, is reached by its
/// callable names in turn.
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
}
