//! What a run read, as evidence is kept by it (spec 13, section 10): each
//! file, directory and missing path a recorder saw the run rest on, inside
//! the repositories the checker reads, with the state it was in. Evidence
//! holds for as long as every one of them is in that state.
//!
//! Paths are named by repository, never by where a clone is, so a read set
//! means the same on another machine.
//!
//! A read set is kept as parts: the paths under one directory, split again
//! by the directories below wherever there are many. A part is named by a
//! digest of its text, so the cases that read the same files of a
//! directory (a dependency, most often) keep that part once, and a check
//! compares it with the files once.

use std::collections::{BTreeMap, BTreeSet, HashMap};
use std::path::{Path, PathBuf};
use std::time::SystemTime;

use serde_json::{Value as Json, json};

use super::states::{ABSENT, Compared, Current, changed_since, is_of_listing};
use crate::recorder::Recorded;

/// A directory with more paths than this under it is kept as a part of
/// its own entries, each large directory below being a part in turn.
const SPLIT_ABOVE: usize = 128;
/// A directory below a split one stays in its parent's part up to this
/// many paths: a part per small directory would be a file per directory.
const INLINE_UP_TO: usize = 16;

/// Where each repository a checker reads has its files.
pub(crate) type Roots = BTreeMap<String, PathBuf>;

/// The first path under `directory` that is not in the state the part
/// named `digest` gives it, with how it differs; none if the part holds.
/// `part` gives a part's text. A part is compared once for a check.
fn part_difference(
    current: &Current,
    directory: &str,
    digest: &str,
    part: &dyn Fn(&str) -> Option<String>,
) -> Option<String> {
    match current.part(directory, digest) {
        Some(Compared::Holds) => return None,
        Some(Compared::Differs(difference)) => return Some(difference),
        None => {}
    }
    let difference = match part(digest) {
        Some(text) => text
            .lines()
            .find_map(|line| line_difference(current, directory, line, part)),
        None => Some(format!("what was read under {directory} is no longer kept")),
    };
    let compared = difference
        .clone()
        .map_or(Compared::Holds, Compared::Differs);
    current.found_part(directory, digest, compared);
    difference
}

/// How one line of a part differs from the files now: a path not in its
/// state, or a difference in the part of a directory below.
fn line_difference(
    current: &Current,
    directory: &str,
    line: &str,
    part: &dyn Fn(&str) -> Option<String>,
) -> Option<String> {
    let mut fields = line.splitn(3, '\t');
    let (Some(kind), Some(value), Some(name)) = (fields.next(), fields.next(), fields.next())
    else {
        return Some(format!("what was read under {directory} does not read"));
    };
    let name = unescaped(name);
    let path = if name.is_empty() {
        directory.to_owned()
    } else {
        format!("{directory}/{name}")
    };
    if kind == PART {
        return part_difference(current, &path, value, part);
    }
    let now = current.state(&path, is_of_listing(value));
    if now == value {
        return None;
    }
    let how = if value == ABSENT {
        "appeared"
    } else if now == ABSENT {
        "is gone"
    } else if is_of_listing(value) {
        "has other entries"
    } else {
        "changed"
    };
    Some(format!("{path} {how}"))
}

/// A line of a part that gives a path's state.
const PATH: &str = "=";
/// A line of a part that names the part of a directory below.
const PART: &str = ">";

/// A name as a part's line holds it: on one line, with no tab.
fn escaped(name: &str) -> String {
    name.replace('\\', "\\\\")
        .replace('\t', "\\t")
        .replace('\n', "\\n")
}

fn unescaped(name: &str) -> String {
    if !name.contains('\\') {
        return name.to_owned();
    }
    let mut found = String::with_capacity(name.len());
    let mut characters = name.chars();
    while let Some(character) = characters.next() {
        if character != '\\' {
            found.push(character);
            continue;
        }
        match characters.next() {
            Some('t') => found.push('\t'),
            Some('n') => found.push('\n'),
            Some(other) => found.push(other),
            None => {}
        }
    }
    found
}

/// The part for the sorted `paths` under one directory, added to `parts`
/// with the parts of large directories below; its digest.
fn part_of(paths: &[(&str, &str)], parts: &mut Parts) -> String {
    let mut text = String::new();
    let mut line = |kind: &str, value: &str, name: &str| {
        text.push_str(kind);
        text.push('\t');
        text.push_str(value);
        text.push('\t');
        text.push_str(&escaped(name));
        text.push('\n');
    };
    let mut index = 0;
    while index < paths.len() {
        let (path, state) = paths[index];
        let below = path.split_once('/').filter(|_| paths.len() > SPLIT_ABOVE);
        let Some((name, _)) = below else {
            line(PATH, state, path);
            index += 1;
            continue;
        };
        let group = &paths[index..index + under(&paths[index..], name)];
        index += group.len();
        if group.len() <= INLINE_UP_TO {
            for (path, state) in group {
                line(PATH, state, path);
            }
            continue;
        }
        // A directory holding nothing but one directory is no part of its
        // own: the part is the deepest directory every path is under.
        let name = &path[..shared_directory(group, name.len())];
        let inside: Vec<(&str, &str)> = group
            .iter()
            .map(|(path, state)| (&path[name.len() + 1..], *state))
            .collect();
        line(PART, &part_of(&inside, parts), name);
    }
    let digest = lemma_model::content_hash(text.as_bytes());
    parts.insert(digest.clone(), text);
    digest
}

/// How many of the sorted `paths`, from the first, are under the directory
/// `name`.
fn under(paths: &[(&str, &str)], name: &str) -> usize {
    let is_under = |(path, _): &&(&str, &str)| {
        path.strip_prefix(name)
            .is_some_and(|rest| rest.starts_with('/'))
    };
    paths.iter().take_while(is_under).count()
}

/// The length of the deepest directory all of the sorted `group` is under,
/// which its first `known` bytes already name.
fn shared_directory(group: &[(&str, &str)], known: usize) -> usize {
    let (Some((first, _)), Some((last, _))) = (group.first(), group.last()) else {
        return known;
    };
    let mut shared = known;
    loop {
        let next = first[shared + 1..].find('/').map(|at| shared + 1 + at);
        let Some(next) = next else {
            return shared;
        };
        let is_shared =
            last.len() > next && last.as_bytes()[next] == b'/' && last[..next] == first[..next];
        if !is_shared {
            return shared;
        }
        shared = next;
    }
}

/// The text of each part, by its digest.
pub(crate) type Parts = BTreeMap<String, String>;

/// A read set as it is kept: for each repository, the digest of the part
/// its paths start from.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub(crate) struct KeptReads {
    repositories: BTreeMap<String, String>,
}

impl KeptReads {
    /// The first path that is no longer in the state the run read it in,
    /// with how it differs; none while the read set holds. `part` gives a
    /// part's text.
    pub(crate) fn difference(
        &self,
        roots: &Roots,
        current: &Current,
        part: &dyn Fn(&str) -> Option<String>,
    ) -> Option<String> {
        self.repositories.iter().find_map(|(repository, digest)| {
            let Some(root) = roots.get(repository).and_then(|root| root.to_str()) else {
                return Some(format!("repository `{repository}` is not read"));
            };
            let difference = part_difference(current, root, digest, part)?;
            // Named inside its repository, as a read set names it.
            let inside = difference.strip_prefix(root).unwrap_or(&difference);
            Some(inside.trim_start_matches('/').to_owned())
        })
    }

    pub(crate) fn to_json(&self) -> Json {
        json!(self.repositories)
    }

    pub(crate) fn from_json(value: &Json) -> Option<Self> {
        let repositories = serde_json::from_value(value.clone()).ok()?;
        Some(Self { repositories })
    }
}

/// What a run read: for each repository, each path with its state.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub(crate) struct ReadSet {
    paths: BTreeMap<String, BTreeMap<String, String>>,
}

impl ReadSet {
    /// What `recorded` shows a run rested on inside `roots`, in the state
    /// it is in now, the run having started at `started` and just ended.
    /// Refused when a file changed under the run: its state now is not
    /// what the run read.
    pub(crate) fn of(
        recorded: &Recorded,
        roots: &Roots,
        started: SystemTime,
        current: &Current,
    ) -> Result<Self, String> {
        // Found afresh: states found before the run ended may be stale.
        let current = &current.afresh();
        let mut paths: BTreeMap<String, BTreeMap<String, String>> = BTreeMap::new();
        let mut keep = |path: &Path, state: String| {
            if let Some((repository, relative)) = place(path, roots) {
                paths.entry(repository).or_default().insert(relative, state);
            }
        };
        // What the run itself wrote is its product, not what it rests on.
        let rests_on = |path: &&PathBuf| !recorded.written.contains(*path);
        for path in recorded.absent.iter().filter(rests_on) {
            if place(path, roots).is_some() {
                keep(path, ABSENT.to_owned());
            }
        }
        for path in recorded.read.iter().filter(rests_on) {
            if place(path, roots).is_none() {
                continue;
            }
            let state = current.state(&path.to_string_lossy(), false);
            // A directory's own time changes with its entries, which a
            // listing covers; a file's says its contents changed, and a
            // link's that it was pointed elsewhere.
            if state == ABSENT || changed_since(path, started) {
                return Err(format!("{} changed while the checker ran", path.display()));
            }
            keep(path, state);
        }
        for path in &recorded.listed {
            if place(path, roots).is_some() {
                keep(path, current.state(&path.to_string_lossy(), true));
            }
        }
        Ok(Self { paths })
    }

    /// The read set as it is kept, and the text of each of its parts.
    pub(crate) fn kept(&self) -> (KeptReads, Parts) {
        let mut parts = Parts::new();
        let repositories = self
            .paths
            .iter()
            .map(|(repository, paths)| {
                let paths: Vec<(&str, &str)> = paths
                    .iter()
                    .map(|(path, state)| (path.as_str(), state.as_str()))
                    .collect();
                (repository.clone(), part_of(&paths, &mut parts))
            })
            .collect();
        (KeptReads { repositories }, parts)
    }

    /// How many paths the run rested on.
    pub(crate) fn len(&self) -> usize {
        self.paths.values().map(BTreeMap::len).sum()
    }
}

/// The paths a run both read and wrote, by the root of the repository each
/// is in: its own products, unless the repository keeps them, which only
/// Git can say.
pub(crate) fn rewritten(recorded: &Recorded, roots: &Roots) -> BTreeMap<PathBuf, Vec<String>> {
    let mut found: BTreeMap<PathBuf, Vec<String>> = BTreeMap::new();
    for path in recorded.written.intersection(&recorded.read) {
        let Some((repository, relative)) = place(path, roots) else {
            continue;
        };
        if let Some(root) = roots.get(&repository) {
            found.entry(root.clone()).or_default().push(relative);
        }
    }
    found
}

/// `recorded` with each path that reaches a repository through a link
/// outside it named by where it is in the repository: `/tmp` is a link on
/// some systems, and a clone can be reached by more than one name. The
/// rest of such a path is kept as it was asked for, so that a link inside
/// the repository still counts as read.
pub(crate) fn inside_repositories(recorded: &Recorded, roots: &Roots) -> Recorded {
    let mut resolved: HashMap<PathBuf, Option<PathBuf>> = HashMap::new();
    let mut inside = |paths: &BTreeSet<PathBuf>| -> BTreeSet<PathBuf> {
        let through = |path: &PathBuf| through_links(path, roots, &mut resolved);
        paths.iter().map(through).collect()
    };
    Recorded {
        read: inside(&recorded.read),
        listed: inside(&recorded.listed),
        absent: inside(&recorded.absent),
        written: inside(&recorded.written),
    }
}

/// `path` from the first directory on its way that is, by another name,
/// a repository's root or inside one; `path` itself when there is none or
/// it is inside a repository as named.
fn through_links(
    path: &Path,
    roots: &Roots,
    resolved: &mut HashMap<PathBuf, Option<PathBuf>>,
) -> PathBuf {
    let is_inside = |path: &Path| roots.values().any(|root| path.starts_with(root));
    if is_inside(path) {
        return path.to_path_buf();
    }
    let mut ancestors: Vec<&Path> = path.ancestors().skip(1).collect();
    ancestors.reverse();
    for ancestor in ancestors {
        let real = resolved
            .entry(ancestor.to_path_buf())
            .or_insert_with(|| ancestor.canonicalize().ok());
        let Some(real) = real else {
            break;
        };
        if !is_inside(real) {
            continue;
        }
        return match path.strip_prefix(ancestor) {
            Ok(rest) => real.join(rest),
            Err(_) => path.to_path_buf(),
        };
    }
    path.to_path_buf()
}

/// The repository `path` is in and its path there; none for a path outside
/// every repository, or inside one's Git directory, which holds history
/// and not code.
fn place(path: &Path, roots: &Roots) -> Option<(String, String)> {
    let (repository, relative) = roots
        .iter()
        .filter_map(|(repository, root)| Some((repository, path.strip_prefix(root).ok()?)))
        .min_by_key(|(_, relative)| relative.as_os_str().len())?;
    if relative.components().any(|part| part.as_os_str() == ".git") {
        return None;
    }
    Some((repository.clone(), relative.to_string_lossy().into_owned()))
}

#[cfg(test)]
mod tests {
    use std::collections::BTreeSet;
    use std::time::Duration;

    use super::*;

    fn write(path: &Path, text: &str) {
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(path, text).unwrap();
    }

    /// A repository with two files and a directory, and a run that read
    /// one file, listed the directory and looked for a file not there.
    fn repository() -> (tempfile::TempDir, Roots, Recorded) {
        let directory = tempfile::tempdir().unwrap();
        let root = directory.path().canonicalize().unwrap();
        write(&root.join("src/used.py"), "x = 1\n");
        write(&root.join("src/unused.py"), "y = 1\n");
        write(&root.join(".git/index"), "history");
        let recorded = Recorded {
            read: BTreeSet::from([root.join("src/used.py"), root.join(".git/index")]),
            listed: BTreeSet::from([root.join("src")]),
            absent: BTreeSet::from([root.join("conftest.py")]),
            written: BTreeSet::new(),
        };
        let roots = Roots::from([("app".to_owned(), root)]);
        (directory, roots, recorded)
    }

    fn read_set(roots: &Roots, recorded: &Recorded) -> ReadSet {
        let later = SystemTime::now() + Duration::from_secs(60);
        ReadSet::of(recorded, roots, later, &Current::default()).unwrap()
    }

    /// Whether a read set, kept and read back, holds for the files now.
    fn holds(reads: &ReadSet, roots: &Roots) -> bool {
        let (kept, parts) = reads.kept();
        let kept = KeptReads::from_json(&kept.to_json()).unwrap();
        let part = |digest: &str| parts.get(digest).cloned();
        kept.difference(roots, &Current::default(), &part).is_none()
    }

    #[test]
    fn it_holds_until_a_file_the_run_read_changes() {
        let (directory, roots, recorded) = repository();
        let reads = read_set(&roots, &recorded);

        write(&directory.path().join("src/unused.py"), "y = 2\n");
        let after_unread = holds(&reads, &roots);
        write(&directory.path().join("src/used.py"), "x = 2\n");
        let after_read = holds(&reads, &roots);

        assert_eq!(reads.len(), 3, "{reads:?}");
        assert!(after_unread);
        assert!(!after_read);
    }

    #[test]
    fn it_stops_holding_when_a_missing_path_appears_or_a_listing_changes() {
        let (directory, roots, recorded) = repository();
        let reads = read_set(&roots, &recorded);

        write(&directory.path().join("conftest.py"), "");
        let after_appearing = holds(&reads, &roots);
        std::fs::remove_file(directory.path().join("conftest.py")).unwrap();
        let after_removing = holds(&reads, &roots);
        write(&directory.path().join("src/new.py"), "");
        let after_listing = holds(&reads, &roots);

        assert!(!after_appearing);
        assert!(after_removing);
        assert!(!after_listing);
    }

    #[test]
    fn a_read_set_that_no_longer_holds_says_which_path_differs_and_how() {
        let (directory, roots, recorded) = repository();
        let reads = read_set(&roots, &recorded);
        let (kept, parts) = reads.kept();
        let part = |digest: &str| parts.get(digest).cloned();
        let difference = || kept.difference(&roots, &Current::default(), &part);

        write(&directory.path().join("conftest.py"), "");
        let appeared = difference();
        std::fs::remove_file(directory.path().join("conftest.py")).unwrap();
        write(&directory.path().join("src/used.py"), "x = 2\n");
        let changed = difference();
        std::fs::remove_file(directory.path().join("src/used.py")).unwrap();
        let gone = difference();

        assert_eq!(appeared.as_deref(), Some("conftest.py appeared"));
        assert_eq!(changed.as_deref(), Some("src/used.py changed"));
        assert_eq!(gone.as_deref(), Some("src has other entries"));
    }

    #[test]
    fn what_the_run_wrote_and_what_lies_outside_is_left_out() {
        let (directory, roots, mut recorded) = repository();
        let root = directory.path().canonicalize().unwrap();
        recorded.written.insert(root.join("src/used.py"));
        recorded.read.insert(PathBuf::from("/etc/hostname"));

        let reads = read_set(&roots, &recorded);

        assert_eq!(
            reads.paths["app"].keys().collect::<Vec<_>>(),
            ["conftest.py", "src"]
        );
    }

    #[test]
    fn a_path_through_a_link_outside_is_named_inside_its_repository() {
        let (directory, roots, _) = repository();
        let root = directory.path().canonicalize().unwrap();
        let outside = tempfile::tempdir().unwrap();
        let link = outside.path().join("clone");
        std::os::unix::fs::symlink(&root, &link).unwrap();
        let recorded = Recorded {
            absent: BTreeSet::from([link.join("src/new.py"), outside.path().join("none.py")]),
            ..Recorded::default()
        };

        let inside = inside_repositories(&recorded, &roots);

        assert_eq!(
            inside.absent,
            BTreeSet::from([root.join("src/new.py"), outside.path().join("none.py")])
        );
    }

    #[test]
    fn what_a_run_read_and_wrote_is_listed_by_repository() {
        let (directory, roots, mut recorded) = repository();
        let root = directory.path().canonicalize().unwrap();
        recorded.written.insert(root.join("src/used.py"));
        recorded.written.insert(root.join("src/made.py"));

        let found = rewritten(&recorded, &roots);

        assert_eq!(
            found,
            BTreeMap::from([(root, vec!["src/used.py".to_owned()])])
        );
    }

    #[test]
    fn a_file_changed_while_the_run_went_on_is_refused() {
        let (_directory, roots, recorded) = repository();
        let before = SystemTime::now() - Duration::from_secs(60);

        let refused = ReadSet::of(&recorded, &roots, before, &Current::default());

        assert!(
            refused
                .unwrap_err()
                .contains("changed while the checker ran")
        );
    }

    /// A repository whose dependency has `count` files, and a run that
    /// read them all and one source file.
    fn with_a_dependency(count: usize, source: &str) -> (tempfile::TempDir, Roots, ReadSet) {
        let directory = tempfile::tempdir().unwrap();
        let root = directory.path().canonicalize().unwrap();
        let mut recorded = Recorded::default();
        for index in 0..count {
            let path = root.join(format!("deps/big/lib/file{index}.py"));
            write(&path, "pass\n");
            recorded.read.insert(path);
        }
        write(&root.join(source), "x = 1\n");
        recorded.read.insert(root.join(source));
        let roots = Roots::from([("app".to_owned(), root)]);
        let reads = read_set(&roots, &recorded);
        (directory, roots, reads)
    }

    #[test]
    fn runs_that_read_the_same_large_directory_keep_its_part_once() {
        let (_one, _, first) = with_a_dependency(SPLIT_ABOVE + 1, "src/a.py");
        let (_other, _, second) = with_a_dependency(SPLIT_ABOVE + 1, "src/b.py");

        let (kept_first, parts_first) = first.kept();
        let (kept_second, parts_second) = second.kept();

        let shared: Vec<&String> = parts_first
            .keys()
            .filter(|digest| parts_second.contains_key(*digest))
            .collect();
        assert_ne!(kept_first, kept_second);
        assert_eq!(shared.len(), 1, "{parts_first:#?}");
        assert!(parts_first[shared[0]].contains("\tfile0.py\n"));
        let starts: Vec<&String> = parts_first
            .values()
            .filter(|text| text.contains(PART))
            .collect();
        assert!(starts[0].contains("\tdeps/big/lib\n"), "{starts:#?}");
    }

    #[test]
    fn a_change_inside_a_part_of_its_own_is_noticed() {
        let (directory, roots, reads) = with_a_dependency(SPLIT_ABOVE + 1, "src/a.py");

        let before = holds(&reads, &roots);
        write(&directory.path().join("deps/big/lib/file7.py"), "raise\n");
        let after = holds(&reads, &roots);

        assert!(before);
        assert!(!after);
    }

    #[test]
    fn a_name_with_a_tab_or_a_line_break_keeps_its_line() {
        let name = "odd\tna\\me\n.py";

        assert_eq!(escaped(name), "odd\\tna\\\\me\\n.py");
        assert_eq!(unescaped(&escaped(name)), name);
    }
}
