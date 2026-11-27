//! A repository's past changes, each with the other repositories as they
//! stood when it was made: what checking history reads before a team lets
//! findings fail a build. Git is asked here; the check of each past change
//! is an ordinary [`crate::check`] with [`CheckOptions::pins`].

use std::collections::{BTreeMap, BTreeSet};
use std::path::Path;

use crate::GraphError;
use crate::check::{
    CheckOptions, Finding, HEAD, Repo, ResolvedRepository, SELF, load_naming_change,
};
use crate::config::LemmaFile;
use crate::names::mentions;
use crate::repos::git;

/// The separator between the fields of one printed commit: a character no
/// commit's subject holds.
const FIELD: char = '\u{1f}';

/// One commit, as Git names and dates it.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize)]
pub struct Commit {
    /// Its full hash.
    pub hash: String,
    /// When it was committed, in seconds since 1970.
    pub committed_at_seconds: i64,
    /// The day it was committed, as `2026-03-01`, in the committer's zone.
    pub committed_on: String,
    /// The first line of its message.
    pub subject: String,
}

/// One past change of a repository, and the system around it then.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize)]
pub struct PastChange {
    /// The commit that made the change; its first parent is the change's
    /// base.
    pub commit: Commit,
    /// The commit each other repository is read at, by name as loading
    /// names it: its last one before the change.
    pub pins: BTreeMap<String, String>,
    /// The other repositories that had no commit yet. The change cannot be
    /// checked against code that did not exist, so it is not checked.
    pub not_yet_there: Vec<String>,
}

/// The last `last` changes of the repository `options` names as its change,
/// newest first: the commits on its checked-out history that touched a file
/// its description reads a contract from, or every commit when it reads
/// none from a file. Each comes with the other repositories' commits of the
/// time.
pub fn past_changes(
    root_file: &Path,
    options: &CheckOptions,
    last: usize,
) -> Result<Vec<PastChange>, GraphError> {
    let (repos, options) = load_naming_change(root_file, &at_checked_out_commit(options))?;
    let Some(changed) = options.change.as_ref().map(|change| &change.repository) else {
        return Ok(Vec::new());
    };
    let repo = &repos[changed];
    let root = &repo.checkouts[HEAD].root;
    let commits = commits_touching(root, &contract_files(&repo.files[HEAD]), last)?;
    commits
        .into_iter()
        .map(|commit| {
            let (pins, not_yet_there) = others_as_of(&repos, changed, &commit)?;
            Ok(PastChange {
                commit,
                pins,
                not_yet_there,
            })
        })
        .collect()
}

/// `options` with its change read from the checked-out commit on: enough
/// to load the repositories, whatever branch the changed one is on.
fn at_checked_out_commit(options: &CheckOptions) -> CheckOptions {
    let mut options = options.clone();
    if let Some(change) = &mut options.change {
        change.base = Some("HEAD".to_owned());
        change.head = None;
    }
    options
}

/// The files and directories a repository's own services' contracts are
/// read from, relative to its root; empty when a contract is read from the
/// whole repository, or none from a file, so that every commit counts.
fn contract_files(file: &LemmaFile) -> Vec<String> {
    let mut files = BTreeSet::new();
    let own = file
        .services
        .iter()
        .filter(|(name, _)| !name.starts_with('#'));
    for (_, service) in own {
        files.extend(service.exports.openapi.iter().cloned());
        let Some(from_code) = &service.exports.operations_from else {
            continue;
        };
        if from_code.inputs.is_empty() {
            return Vec::new();
        }
        files.extend(from_code.inputs.iter().cloned());
    }
    files.into_iter().collect()
}

/// The last `last` commits on the checked-out history of the clone at
/// `root` that changed any of `files` (any file at all, if empty), newest
/// first. Merges count by what they brought in, and the first commit of
/// the repository, which has nothing before it to compare with, does not.
fn commits_touching(root: &Path, files: &[String], last: usize) -> Result<Vec<Commit>, GraphError> {
    let count = format!("--max-count={last}");
    let mut arguments = vec![
        "log",
        "--first-parent",
        "--min-parents=1",
        COMMIT_FORMAT,
        &count,
        "HEAD",
        "--",
    ];
    arguments.extend(files.iter().map(String::as_str));
    Ok(parse_commits(&git(root, &arguments)?))
}

/// Prints a commit as [`parse_commits`] reads it.
const COMMIT_FORMAT: &str = "--format=%H%x1f%ct%x1f%cs%x1f%s";

/// The commits Git printed with [`COMMIT_FORMAT`], a line each.
fn parse_commits(printed: &str) -> Vec<Commit> {
    printed
        .lines()
        .filter_map(|line| {
            let mut fields = line.splitn(4, FIELD);
            let hash = fields.next()?.to_owned();
            let committed_at_seconds = fields.next()?.parse().ok()?;
            let committed_on = fields.next()?.to_owned();
            let subject = fields.next()?.to_owned();
            Some(Commit {
                hash,
                committed_at_seconds,
                committed_on,
                subject,
            })
        })
        .collect()
}

/// The commit each repository but `changed` is read at for `commit`, and
/// the repositories that had none yet. One that shares the changed
/// repository's clone is read at the commit itself.
fn others_as_of(
    repos: &BTreeMap<String, Repo>,
    changed: &str,
    commit: &Commit,
) -> Result<(BTreeMap<String, String>, Vec<String>), GraphError> {
    let changed_root = &repos[changed].checkouts[HEAD].root;
    let before = format!("--before={}", commit.committed_at_seconds);
    let mut pins = BTreeMap::new();
    let mut not_yet_there = Vec::new();
    let others = repos
        .iter()
        .filter(|(name, _)| name.as_str() != changed && name.as_str() != SELF);
    for (name, repo) in others {
        let checkout = &repo.checkouts[HEAD];
        if &checkout.root == changed_root {
            pins.insert(name.clone(), commit.hash.clone());
            continue;
        }
        // Never later than the commit the description pins it to.
        let newest = checkout.commit.as_deref().unwrap_or("HEAD");
        let found = git(
            &checkout.root,
            &[
                "rev-list",
                "--first-parent",
                "--max-count=1",
                &before,
                newest,
            ],
        )?;
        match found.trim() {
            "" => not_yet_there.push(name.clone()),
            pinned => {
                pins.insert(name.clone(), pinned.to_owned());
            }
        }
    }
    Ok((pins, not_yet_there))
}

/// The code a finding rests on: the repository among `repositories` its
/// file is in and the file's path from that repository's root. `None` for
/// a finding that names no file, or one in a repository not read.
///
/// A file is written with its repository and service (`#web:app:src/a.ts`),
/// or bare (`src/a.ts`) beside another value that says whose it is, as a
/// client's file is beside `#sdk:client`.
pub fn rests_on<'r>(
    finding: &Finding,
    repositories: &'r [ResolvedRepository],
) -> Option<(&'r ResolvedRepository, String)> {
    let file = finding.values.get("f")?;
    let is_within = |value: &str, repository: &ResolvedRepository| {
        value
            .strip_prefix(&repository.name)
            .is_some_and(|rest| rest.starts_with(':'))
    };
    let addressed = repositories.iter().find_map(|repository| {
        let in_repository = file.strip_prefix(&repository.name)?.strip_prefix(':')?;
        let path = in_repository
            .split_once(':')
            .map_or(in_repository, |(_, path)| path);
        Some((repository, path.to_owned()))
    });
    if addressed.is_some() || file.starts_with('#') {
        return addressed;
    }
    let mut owners = repositories.iter().filter(|repository| {
        finding
            .values
            .values()
            .any(|value| is_within(value, repository))
    });
    let owner = owners.next()?;
    if owners.next().is_some() {
        return None;
    }
    Some((owner, file.clone()))
}

/// A later commit that changed the lines of a file naming something.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize)]
pub struct LaterChange {
    /// The commit.
    pub commit: Commit,
    /// How many lines naming it the commit removed.
    pub lines_removed: usize,
    /// How many lines naming it the commit added.
    pub lines_added: usize,
}

/// The first commit after `from`, on the checked-out history of the clone
/// at `root`, that changed the lines of `path` mentioning `names`: it
/// removed or added such a line. `None` when no later commit did.
///
/// A finding rests on code that names a field or a parameter. A later
/// commit that changed the lines naming it is evidence that someone acted
/// on what was found, and no more: a fix may drop the name or handle it,
/// and an unrelated edit may touch it too. A line a commit only moves,
/// removed in one place and added unchanged in another, does not count.
pub fn later_change(
    root: &Path,
    from: &str,
    path: &str,
    names: &[&str],
) -> Result<Option<LaterChange>, GraphError> {
    if names.is_empty() {
        return Ok(None);
    }
    let after = format!("{from}..HEAD");
    let patches = git(
        root,
        &[
            "log",
            "--first-parent",
            "--reverse",
            "--patch",
            "--unified=0",
            PATCHED_COMMIT_FORMAT,
            &after,
            "--",
            path,
        ],
    )?;
    Ok(first_changing(&patches, names))
}

/// Starts each commit of a printed history of patches, so a line of a
/// patch is never taken for one.
const COMMIT_START: char = '\u{1e}';

/// [`COMMIT_FORMAT`] after [`COMMIT_START`].
const PATCHED_COMMIT_FORMAT: &str = "--format=%x1e%H%x1f%ct%x1f%cs%x1f%s";

/// The first of the commits printed with their patches, oldest first,
/// whose patch removes or adds a line mentioning one of `names`, lines it
/// only moves aside.
fn first_changing(patches: &str, names: &[&str]) -> Option<LaterChange> {
    patches.split(COMMIT_START).find_map(|printed| {
        let (header, patch) = printed.split_once('\n')?;
        let mentioning = |sign: char, file_header: &str| -> Vec<&str> {
            patch
                .lines()
                .filter(|line| line.starts_with(sign) && !line.starts_with(file_header))
                .map(|line| line[1..].trim())
                .filter(|line| names.iter().any(|name| mentions(line, name)))
                .collect()
        };
        let mut removed = mentioning('-', "---");
        let mut added = mentioning('+', "+++");
        // A line removed and added with the same text was moved.
        removed.retain(|line| {
            let moved = added.iter().position(|other| other == line);
            moved.map(|at| added.swap_remove(at)).is_none()
        });
        if removed.is_empty() && added.is_empty() {
            return None;
        }
        Some(LaterChange {
            commit: parse_commits(header).pop()?,
            lines_removed: removed.len(),
            lines_added: added.len(),
        })
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::{OperationsFrom, Service};

    fn described(services: &[(&str, Service)]) -> LemmaFile {
        LemmaFile {
            services: services
                .iter()
                .map(|(name, service)| ((*name).to_owned(), service.clone()))
                .collect(),
            ..LemmaFile::default()
        }
    }

    fn with_documents(documents: &[&str]) -> Service {
        let mut service = Service::default();
        service.exports.openapi = documents.iter().map(|path| (*path).to_owned()).collect();
        service
    }

    #[test]
    fn contracts_are_read_from_documents_and_a_generators_inputs() {
        let mut from_code = with_documents(&["openapi.json"]);
        from_code.exports.operations_from = Some(OperationsFrom {
            inputs: vec!["src/routes".to_owned()],
            ..OperationsFrom::default()
        });
        let file = described(&[
            ("api", from_code),
            ("admin", with_documents(&["admin.json"])),
            ("#other", with_documents(&["theirs.json"])),
        ]);

        let files = contract_files(&file);

        assert_eq!(files, ["admin.json", "openapi.json", "src/routes"]);
    }

    #[test]
    fn a_generator_reading_the_whole_repository_makes_every_commit_count() {
        let mut from_code = with_documents(&["openapi.json"]);
        from_code.exports.operations_from = Some(OperationsFrom::default());
        let file = described(&[("api", from_code)]);

        let files = contract_files(&file);

        assert_eq!(files, Vec::<String>::new());
    }

    fn finding_with(values: &[(&str, &str)]) -> Finding {
        Finding {
            severity: crate::Severity::Error,
            link: String::new(),
            check: String::new(),
            message: String::new(),
            introduced: true,
            affects: Vec::new(),
            allowed: None,
            values: values
                .iter()
                .map(|(variable, value)| ((*variable).to_owned(), (*value).to_owned()))
                .collect(),
            derivation: None,
            description_name: None,
        }
    }

    fn read(name: &str) -> ResolvedRepository {
        ResolvedRepository {
            name: name.to_owned(),
            root: format!("/clones/{}", name.trim_start_matches('#')),
            base: None,
            head: None,
            manifest_from_working_directory: false,
        }
    }

    #[test]
    fn a_file_written_with_its_repository_and_service_is_placed_there() {
        let finding = finding_with(&[("f", "#web:app:src/a.ts"), ("l", "3")]);
        let repositories = [read("#billing"), read("#web")];

        let place = rests_on(&finding, &repositories);

        let (repository, path) = place.unwrap();
        assert_eq!(repository.name, "#web");
        assert_eq!(path, "src/a.ts");
    }

    #[test]
    fn a_bare_file_belongs_to_the_one_repository_the_finding_names() {
        let finding = finding_with(&[("f", "src/sdk.gen.ts"), ("cl", "#sdk:client")]);
        let repositories = [read("#billing"), read("#sdk")];

        let place = rests_on(&finding, &repositories);

        let (repository, path) = place.unwrap();
        assert_eq!(repository.name, "#sdk");
        assert_eq!(path, "src/sdk.gen.ts");
    }

    #[test]
    fn a_bare_file_between_two_named_repositories_is_not_placed() {
        let finding = finding_with(&[
            ("f", "src/a.ts"),
            ("c", "#web:app:src/b.ts::load"),
            ("cl", "#sdk:client"),
        ]);
        let repositories = [read("#sdk"), read("#web")];

        let place = rests_on(&finding, &repositories);

        assert_eq!(place.map(|(_, path)| path), None);
    }

    #[test]
    fn a_commit_that_moves_a_named_line_is_passed_over_for_one_that_changes_it() {
        let patches = "\u{1e}aaa\u{1f}10\u{1f}2026-01-01\u{1f}move\n\n--- a/x.ts\n+++ b/x.ts\n@@ -1 +9 @@\n-  query: { total },\n+    query: { total },\n\
                       \u{1e}bbb\u{1f}20\u{1f}2026-01-02\u{1f}stop reading total\n\n--- a/x.ts\n+++ b/x.ts\n@@ -4,2 +4 @@\n-  const total = body.total\n-  return total\n+  return body.paid\n\
                       \u{1e}ccc\u{1f}30\u{1f}2026-01-03\u{1f}later\n\n--- a/x.ts\n+++ b/x.ts\n@@ -9 +0,0 @@\n-    query: { total },\n";

        let found = first_changing(patches, &["total"]);

        let found = found.unwrap();
        assert_eq!(found.commit.hash, "bbb");
        assert_eq!((found.lines_removed, found.lines_added), (2, 0));
    }

    #[test]
    fn a_commit_that_comes_to_handle_the_name_changed_its_lines() {
        let patches = "\u{1e}aaa\u{1f}10\u{1f}2026-01-01\u{1f}ignore search\n\n--- a/x.ts\n+++ b/x.ts\n@@ -1 +1,2 @@\n- * @param search - text to look for\n+  const { search, ...query } = params\n+  if (search !== undefined) warn()\n";

        let found = first_changing(patches, &["search"]);

        let found = found.unwrap();
        assert_eq!((found.lines_removed, found.lines_added), (1, 2));
    }

    #[test]
    fn a_commit_that_changes_other_lines_is_not_a_change_of_the_named_ones() {
        let patches = "\u{1e}aaa\u{1f}10\u{1f}2026-01-01\u{1f}rename\n\n--- a/x.ts\n+++ b/x.ts\n@@ -1 +1 @@\n-const subtotal = 1\n+const sum = 1\n";

        let found = first_changing(patches, &["total"]);

        assert_eq!(found, None);
    }

    #[test]
    fn a_printed_commit_keeps_separators_its_subject_writes() {
        let printed = "abc\u{1f}1700000000\u{1f}2023-11-14\u{1f}fix: a | b\ndef\u{1f}1690000000\u{1f}2023-07-22\u{1f}init\n";

        let commits = parse_commits(printed);

        assert_eq!(commits.len(), 2);
        assert_eq!(commits[0].hash, "abc");
        assert_eq!(commits[0].committed_at_seconds, 1_700_000_000);
        assert_eq!(commits[0].committed_on, "2023-11-14");
        assert_eq!(commits[0].subject, "fix: a | b");
    }
}
