//! Code a description leaves out that writes the path of an operation it
//! describes. No service includes such code, so no check reads it: a
//! consumer there can break without a finding, and a clean result says
//! nothing of it. See `.specs/16-verifying-descriptions.md`, "Missing,
//! suspected: candidates".
//!
//! The files are found in two steps, so that a repository of many
//! thousands of files costs a search of their text: a file is parsed only
//! if some string in it is the path of a described operation, and what it
//! writes is then read as it would be inside a service. A path under a
//! base URL the code keeps elsewhere counts as it does there: only where
//! the code left out of one repository also names the base.
//!
//! A path of one fixed segment (`/api`, `/health`, `/users/{id}`) is
//! written in too much code that asks nothing of the service to suggest
//! anything alone: it counts only in a file that also writes a path of
//! two or more.
//!
//! Test files are searched the same way wherever they are, since no
//! service's tests are read as calls.

use std::collections::{BTreeMap, BTreeSet};

use globset::{Glob, GlobMatcher};
use lemma_http::{Base, CodeOptions, is_test_file};
use lemma_syntax::Extractor;
use rayon::prelude::*;
use regex::Regex;

use crate::GraphError;
use crate::check::{HEAD, Repo, Report};
use crate::config::{LemmaFile, Service};
use crate::repos::{Checkout, tracked_files};

/// The candidate for code no service includes.
const CALLS_OUTSIDE_SERVICES: &str = "calls_outside_services";
/// How many files are read into memory at a time.
const FILES_AT_A_TIME: usize = 512;

/// A suspected gap in a description, found by a pattern rather than shown
/// from the code. It is reported and never fails a command.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, serde::Serialize)]
pub struct Candidate {
    /// The kind of candidate, such as `calls_outside_services`.
    pub name: String,
    /// What it is about: the directory or file the description leaves out,
    /// after its repository (`#web:scripts`), or a client.
    pub location: String,
    /// How many files there write the path of a described operation, or
    /// hold the client's functions.
    pub files: usize,
    /// How many described operations including it would add calls of, or
    /// the client's functions request.
    pub operations: usize,
    /// The places that write one, in the order of their files and lines;
    /// none for a client.
    pub places: Vec<WrittenCall>,
}

/// A place in code that writes the path of a described operation.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, serde::Serialize)]
pub struct WrittenCall {
    /// The file, relative to its repository.
    pub file: String,
    /// The line.
    pub line: u32,
    /// The method the code shows, or `*`.
    pub method: String,
    /// The operation's path.
    pub path: String,
}

/// The places in one repository's tests that write the path of a described
/// operation. Tests are never read as calls, whatever a service includes.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, serde::Serialize)]
pub struct CallsInTests {
    /// The repository, as the workspace names it.
    pub repository: String,
    /// How many test files write one.
    pub files: usize,
    /// How many places in them do.
    pub places: usize,
}

/// The files a repository's services include that are in one language
/// Lemma has no reader for. Nothing in them is read.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, serde::Serialize)]
pub struct UnreadFiles {
    /// The repository, as the workspace names it.
    pub repository: String,
    /// The language, such as `Vue`.
    pub language: String,
    /// How many files, tests left out.
    pub files: usize,
}

/// For each repository, the files its services include in each language
/// Lemma has no reader for, in the order of repositories and languages.
pub(super) fn unread_files(repos: &BTreeMap<String, Repo>) -> Vec<UnreadFiles> {
    let mut found = Vec::new();
    for (name, repo) in repos {
        let Some(tracked) = tracked_files(&repo.checkouts[HEAD]) else {
            continue;
        };
        let selection = Selection::of(&repo.files[HEAD]);
        let mut counts: BTreeMap<&str, usize> = BTreeMap::new();
        for path in &tracked {
            let extension = path.rsplit_once('.').map_or("", |(_, extension)| extension);
            let Some(language) = lemma_model::unread_language(extension) else {
                continue;
            };
            if selection.includes(path) && !is_test_file(path) {
                *counts.entry(language).or_default() += 1;
            }
        }
        found.extend(counts.into_iter().map(|(language, files)| UnreadFiles {
            repository: name.clone(),
            language: language.to_owned(),
            files,
        }));
    }
    found
}

/// What was found outside what the services read.
#[derive(Debug, Default)]
pub(super) struct Outside {
    pub(super) candidates: Vec<Candidate>,
    pub(super) calls_in_tests: Vec<CallsInTests>,
}

/// The code of `repos` that no service reads and that writes the path of
/// an operation among `report`'s `operation` rows.
pub(super) fn written_outside(
    repos: &BTreeMap<String, Repo>,
    report: &Report,
) -> Result<Outside, GraphError> {
    let rows = report
        .relations
        .get("operation")
        .map_or(&[][..], Vec::as_slice);
    let operations = Operations::of(rows);
    let mut found = Outside::default();
    if operations.paths.is_empty() {
        return Ok(found);
    }
    for (name, repo) in repos {
        let checkout = &repo.checkouts[HEAD];
        let Some(tracked) = tracked_files(checkout) else {
            continue;
        };
        let selection = Selection::of(&repo.files[HEAD]);
        let is_code = |path: &&String| lemma_verify::is_code_path(path);
        let (tests, code): (Vec<&String>, Vec<&String>) = tracked
            .iter()
            .filter(is_code)
            .partition(|path| is_test_file(path));
        let left_out: Vec<String> = code
            .into_iter()
            .filter(|path| !selection.reads(path))
            .cloned()
            .collect();
        let written = written_calls(checkout, &left_out, &operations)?;
        found
            .candidates
            .extend(candidates(name, &selection, &operations, &written));
        let tests: Vec<String> = tests.into_iter().cloned().collect();
        let in_tests = written_calls(checkout, &tests, &operations)?;
        found.calls_in_tests.extend(calls_in_tests(name, &in_tests));
    }
    found.candidates.sort();
    Ok(found)
}

/// The described operations, as code is matched against them.
struct Operations {
    /// The methods of each canonical path.
    paths: BTreeMap<String, BTreeSet<String>>,
    /// What makes a string a path (see [`lemma_http::path_markers`]).
    markers: Vec<String>,
    /// The prefixes a consumer may keep in a base URL (see
    /// [`lemma_http::document_bases`]). Each service's paths are taken for
    /// one document's.
    bases: Vec<Base>,
    /// Finds the segments a path is written from, at the service's root or
    /// under a base (`v1` for `/v1/`): text holding none writes no path.
    /// `None` when they make too large a pattern, and all text is looked
    /// at.
    segments: Option<Regex>,
}

/// What the text of one file shows before it is parsed.
#[derive(Debug, Default, PartialEq, Eq)]
struct Shown {
    /// Whether some string in it is the path of an operation, of two or
    /// more fixed segments.
    writes_a_path: bool,
    /// The bases some string in it names, as a base URL does.
    named_bases: BTreeSet<String>,
}

impl Operations {
    /// From `operation` rows: service, operation, method, path.
    fn of(rows: &[Vec<String>]) -> Self {
        let mut paths: BTreeMap<String, BTreeSet<String>> = BTreeMap::new();
        let mut services: BTreeMap<&str, BTreeSet<&str>> = BTreeMap::new();
        for row in rows {
            let [service, _, method, path] = row.as_slice() else {
                continue;
            };
            paths
                .entry(path.clone())
                .or_default()
                .insert(method.clone());
            services.entry(service).or_default().insert(path);
        }
        let markers = lemma_http::path_markers(paths.keys().map(String::as_str));
        let bases =
            lemma_http::bases_of_documents(services.values().map(|paths| paths.iter().copied()));
        let segments: BTreeSet<String> = bases
            .iter()
            .flat_map(|base| &base.markers)
            .chain(&markers)
            .map(|marker| regex::escape(marker.trim_matches('/')))
            .collect();
        let segments: Vec<String> = segments.into_iter().collect();
        Self {
            paths,
            markers,
            bases,
            segments: Regex::new(&segments.join("|")).ok(),
        }
    }

    /// The operations a call of `path` with `method` is of: every one on
    /// the path when the code shows no method.
    fn called(&self, method: &str, path: &str) -> Vec<(&str, &str)> {
        let Some((path, methods)) = self.paths.get_key_value(path) else {
            return Vec::new();
        };
        methods
            .iter()
            .filter(|described| method == "*" || *described == method)
            .map(|described| (described.as_str(), path.as_str()))
            .collect()
    }

    /// What `text` shows: each run of it without quotes or spaces that
    /// holds a segment is looked at as a string the code writes.
    fn shown_in(&self, text: &str) -> Shown {
        let mut shown = Shown::default();
        for run in self.runs_with_a_segment(text) {
            shown.writes_a_path = shown.writes_a_path || self.is_specific_path(run);
            let named = self
                .bases
                .iter()
                .filter(|base| lemma_http::names_base(run, &base.prefix))
                .map(|base| base.prefix.clone());
            shown.named_bases.extend(named);
        }
        shown
    }

    /// The runs of `text` between quotes and spaces that hold one of the
    /// segments, each once, in order.
    fn runs_with_a_segment<'t>(&self, text: &'t str) -> Vec<&'t str> {
        let ends_run =
            |byte: &u8| byte.is_ascii_whitespace() || matches!(byte, b'"' | b'\'' | b'`');
        let Some(segments) = &self.segments else {
            return text
                .split(|c: char| u8::try_from(c).is_ok_and(|byte| ends_run(&byte)))
                .collect();
        };
        let bytes = text.as_bytes();
        let mut runs = Vec::new();
        let mut looked_to = 0;
        for found in segments.find_iter(text) {
            if found.start() < looked_to {
                continue;
            }
            let start = bytes[..found.start()]
                .iter()
                .rposition(ends_run)
                .map_or(0, |at| at + 1);
            let end = bytes[found.end()..]
                .iter()
                .position(ends_run)
                .map_or(bytes.len(), |at| found.end() + at);
            // Runs end at ASCII bytes, so both ends are character
            // boundaries.
            runs.push(&text[start..end]);
            looked_to = end;
        }
        runs
    }

    /// Whether `run` is the path of an operation, of two or more fixed
    /// segments: with its leading slash or without (`articles/${slug}`,
    /// given to a client that knows where the service is), whole or from a
    /// base on.
    fn is_specific_path(&self, run: &str) -> bool {
        let with_slash = format!("/{run}");
        let whole = lemma_http::canonical_path(run, &self.markers)
            .or_else(|| lemma_http::canonical_route(&with_slash));
        let under_bases = self.bases.iter().filter_map(|base| {
            lemma_http::path_under_base(run, base)
                .or_else(|| lemma_http::path_under_base(&with_slash, base))
        });
        whole
            .into_iter()
            .chain(under_bases)
            .any(|path| is_specific(&path) && self.paths.contains_key(&path))
    }
}

/// Whether a canonical path has two or more fixed segments.
fn is_specific(path: &str) -> bool {
    let fixed = path
        .split('/')
        .filter(|segment| !segment.is_empty() && *segment != "{}");
    fixed.count() >= 2
}

/// What a repository's services read of it.
struct Selection {
    /// The directories and files they include; `None` when one reads the
    /// whole repository.
    includes: Option<Vec<String>>,
    /// What they say never to read.
    excludes: Vec<GlobMatcher>,
}

impl Selection {
    fn of(file: &LemmaFile) -> Self {
        let local: Vec<&Service> = file
            .services
            .iter()
            .filter(|(name, _)| !name.starts_with('#'))
            .map(|(_, service)| service)
            .collect();
        // A service that declares nothing but code, alone in its
        // repository, has all of it read.
        let reads_everything =
            local.len() == 1 && local[0].index.include.is_empty() && local[0].exports.is_empty();
        let includes: Vec<String> = local
            .iter()
            .flat_map(|service| &service.index.include)
            .map(|include| normal(include).to_owned())
            .collect();
        let reads_everything = reads_everything || includes.iter().any(String::is_empty);
        let excludes = local
            .iter()
            .flat_map(|service| &service.index.exclude)
            .filter_map(|pattern| Glob::new(pattern).ok())
            .map(|glob| glob.compile_matcher())
            .collect();
        Self {
            includes: (!reads_everything).then_some(includes),
            excludes,
        }
    }

    /// Whether a service reads `path`, or says never to: either way the
    /// description has spoken of it.
    fn reads(&self, path: &str) -> bool {
        let Some(includes) = &self.includes else {
            return true;
        };
        includes.iter().any(|include| is_at_or_under(path, include))
            || self.excludes.iter().any(|exclude| exclude.is_match(path))
    }

    /// Whether a service includes `path` and none says never to read it.
    fn includes(&self, path: &str) -> bool {
        let included = self
            .includes
            .as_ref()
            .is_none_or(|includes| includes.iter().any(|include| is_at_or_under(path, include)));
        included && !self.excludes.iter().any(|exclude| exclude.is_match(path))
    }

    /// The outermost directory holding `path` and nothing a service
    /// includes, or `path` itself where every directory above it holds
    /// something included: what an `include` would have to add.
    fn left_out_with<'p>(&self, path: &'p str) -> &'p str {
        let includes = self.includes.as_deref().unwrap_or_default();
        path.match_indices('/')
            .map(|(end, _)| &path[..end])
            .find(|directory| {
                !includes
                    .iter()
                    .any(|include| is_at_or_under(include, directory))
            })
            .unwrap_or(path)
    }
}

/// An `include` as paths are listed: without a leading `./` or a trailing
/// `/`, and empty for the whole repository (`.`).
fn normal(include: &str) -> &str {
    let include = include.strip_prefix("./").unwrap_or(include);
    let include = include.trim_end_matches('/');
    if include == "." { "" } else { include }
}

fn is_at_or_under(path: &str, directory: &str) -> bool {
    path.strip_prefix(directory)
        .is_some_and(|rest| rest.is_empty() || rest.starts_with('/'))
}

/// The places in `paths` at `checkout` that write the path of one of
/// `operations`, in the order of their files and lines.
fn written_calls(
    checkout: &Checkout,
    paths: &[String],
    operations: &Operations,
) -> Result<Vec<WrittenCall>, GraphError> {
    let commit = checkout.commit.as_deref();
    let read: Result<Vec<Vec<Written>>, GraphError> = paths
        .par_chunks(FILES_AT_A_TIME)
        .map(|chunk| {
            let files = lemma_http::read_listed(&checkout.root, commit, chunk)
                .map_err(|source| GraphError::Source { source })?;
            let mut extractor = None;
            let mut written = Vec::new();
            for file in &files {
                let shown = operations.shown_in(&file.contents);
                let places = if shown.writes_a_path {
                    let extractor = match &mut extractor {
                        Some(extractor) => extractor,
                        empty => empty.insert(
                            Extractor::new().map_err(|source| GraphError::Syntax { source })?,
                        ),
                    };
                    places_in(&file.path, &file.contents, operations, extractor)
                } else {
                    Vec::new()
                };
                if !places.is_empty() || !shown.named_bases.is_empty() {
                    written.push(Written {
                        places,
                        named_bases: shown.named_bases,
                    });
                }
            }
            Ok(written)
        })
        .collect();
    let read: Vec<Written> = read?.into_iter().flatten().collect();
    let named_bases: BTreeSet<&str> = read
        .iter()
        .flat_map(|written| &written.named_bases)
        .map(String::as_str)
        .collect();
    let mut calls: Vec<WrittenCall> = read
        .iter()
        .flat_map(|written| written.calls(&named_bases))
        .collect();
    calls.sort();
    calls.dedup();
    Ok(calls)
}

/// What one file writes: the places that may be calls of described
/// operations, each with the base its path is read under (empty for a
/// path read as written), and the bases the file names.
struct Written {
    places: Vec<(WrittenCall, String)>,
    named_bases: BTreeSet<String>,
}

impl Written {
    /// The places that are calls where the code read with this file names
    /// `named_bases`. A path under a base is one only if the base is
    /// named, and under two named bases on one line it is none: which the
    /// request goes to cannot be told. And none is, unless one of them
    /// has a path of two or more fixed segments.
    fn calls(&self, named_bases: &BTreeSet<&str>) -> Vec<WrittenCall> {
        let mut bases_at: BTreeMap<u32, BTreeSet<&str>> = BTreeMap::new();
        for (call, base) in &self.places {
            if named_bases.contains(base.as_str()) {
                bases_at.entry(call.line).or_default().insert(base);
            }
        }
        let is_call = |call: &WrittenCall, base: &str| {
            base.is_empty()
                || bases_at
                    .get(&call.line)
                    .is_some_and(|bases| bases.len() == 1 && bases.contains(base))
        };
        let calls: Vec<WrittenCall> = self
            .places
            .iter()
            .filter(|(call, base)| is_call(call, base))
            .map(|(call, _)| call.clone())
            .collect();
        if calls.iter().any(|call| is_specific(&call.path)) {
            calls
        } else {
            Vec::new()
        }
    }
}

/// The places in `file` that would be read as calls of `operations` were
/// it a service's, each with the base its path is read under: paths in
/// strings and requests, read as they are there. A route the file
/// registers is served, not called, and a file that does not parse writes
/// nothing that can be read.
fn places_in(
    file: &str,
    text: &str,
    operations: &Operations,
    extractor: &mut Extractor,
) -> Vec<(WrittenCall, String)> {
    let options = CodeOptions {
        path_markers: operations.markers.clone(),
        bases: operations.bases.clone(),
        ..CodeOptions::default()
    };
    let Ok(facts) = lemma_http::extract(file, text, &options, extractor) else {
        return Vec::new();
    };
    let references = facts.path_references.iter().map(|reference| {
        let place = (&reference.method, &reference.path, reference.location.line);
        (place, reference.base.as_str())
    });
    let described = facts
        .method_paths
        .iter()
        .filter(|item| !item.is_registration)
        .map(|item| ((&item.method, &item.path, item.location.line), ""));
    references
        .chain(described)
        .filter(|((method, path, _), _)| !operations.called(method, path).is_empty())
        .map(|((method, path, line), base)| {
            let call = WrittenCall {
                file: file.to_owned(),
                line,
                method: method.clone(),
                path: path.clone(),
            };
            (call, base.to_owned())
        })
        .collect()
}

/// One candidate for each directory of `repository` that `written` is in
/// and no service includes.
fn candidates(
    repository: &str,
    selection: &Selection,
    operations: &Operations,
    written: &[WrittenCall],
) -> Vec<Candidate> {
    let mut by_directory: BTreeMap<&str, Vec<&WrittenCall>> = BTreeMap::new();
    for call in written {
        by_directory
            .entry(selection.left_out_with(&call.file))
            .or_default()
            .push(call);
    }
    by_directory
        .into_iter()
        .map(|(directory, places)| {
            let files: BTreeSet<&str> = places.iter().map(|call| call.file.as_str()).collect();
            let called: BTreeSet<(&str, &str)> = places
                .iter()
                .flat_map(|call| operations.called(&call.method, &call.path))
                .collect();
            Candidate {
                name: CALLS_OUTSIDE_SERVICES.to_owned(),
                location: format!("{repository}:{directory}"),
                files: files.len(),
                operations: called.len(),
                places: places.into_iter().cloned().collect(),
            }
        })
        .collect()
}

fn calls_in_tests(repository: &str, written: &[WrittenCall]) -> Option<CallsInTests> {
    let files: BTreeSet<&str> = written.iter().map(|call| call.file.as_str()).collect();
    (!written.is_empty()).then(|| CallsInTests {
        repository: repository.to_owned(),
        files: files.len(),
        places: written.len(),
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A billing service under `/v1`, and another serving one path of one
    /// segment.
    fn operations() -> Operations {
        let row = |service: &str, method: &str, path: &str| {
            vec![
                service.to_owned(),
                format!("{method} {path}"),
                method.to_owned(),
                path.to_owned(),
            ]
        };
        Operations::of(&[
            row("#billing:api", "GET", "/v1/invoices/{}"),
            row("#billing:api", "DELETE", "/v1/invoices/{}"),
            row("#billing:api", "GET", "/v1/invoices"),
            row("#billing:probe", "GET", "/status"),
        ])
    }

    fn selection(includes: &[&str]) -> Selection {
        Selection {
            includes: Some(
                includes
                    .iter()
                    .map(|include| (*include).to_owned())
                    .collect(),
            ),
            excludes: Vec::new(),
        }
    }

    /// The calls `text` writes, read alone, as line, method and path.
    fn calls(file: &str, text: &str) -> Vec<(u32, String, String)> {
        let operations = operations();
        let mut extractor = Extractor::new().unwrap();
        let written = Written {
            places: places_in(file, text, &operations, &mut extractor),
            named_bases: operations.shown_in(text).named_bases,
        };
        let named: BTreeSet<&str> = written.named_bases.iter().map(String::as_str).collect();
        written
            .calls(&named)
            .into_iter()
            .map(|call| (call.line, call.method, call.path))
            .collect()
    }

    fn call(line: u32, method: &str, path: &str) -> (u32, String, String) {
        (line, method.to_owned(), path.to_owned())
    }

    #[test]
    fn a_file_is_parsed_only_if_a_string_in_it_is_an_operations_path() {
        let operations = operations();
        let writes = |text: &str| operations.shown_in(text).writes_a_path;

        assert!(writes("fetch(`${BASE}/v1/invoices/${id}`)"));
        assert!(writes("requests.get(f\"{base}/v1/invoices\")"));
        assert!(writes("api.get(`v1/invoices/${id}`)"));
        assert!(writes("http.get('/invoices')"));
        assert!(!writes("fetch('/v1/customers')"));
        assert!(!writes("import x from '@/lib/v1/invoices'"));
        assert!(!writes("const total = invoices.length"));
    }

    #[test]
    fn a_path_of_one_fixed_segment_does_not_have_a_file_parsed() {
        let operations = operations();

        let shown = operations.shown_in("const page = 'https://vendor.example/status'");

        assert!(!shown.writes_a_path);
    }

    #[test]
    fn a_string_ending_in_a_base_names_it() {
        let operations = operations();

        let shown = operations.shown_in("baseURL = 'https://billing.example/v1'");

        assert_eq!(shown.named_bases, BTreeSet::from(["/v1".to_owned()]));
    }

    #[test]
    fn a_request_of_a_described_path_is_a_place_and_another_path_is_not() {
        let source = "export async function show(id: string) {\n  await fetch(`${BASE}/v1/invoices/${id}`)\n  await fetch(`${BASE}/v1/customers/${id}`)\n}\n";

        let found = calls("scripts/show.ts", source);

        assert_eq!(found, [call(2, "GET", "/v1/invoices/{}")]);
    }

    #[test]
    fn a_method_no_operation_on_the_path_has_is_not_a_place() {
        let source = "export const save = (id: string) =>\n  fetch(`/v1/invoices/${id}`, { method: 'PUT' })\n";

        let found = calls("scripts/save.ts", source);

        assert_eq!(found, []);
    }

    #[test]
    fn a_path_under_a_base_is_a_place_only_where_the_base_is_named() {
        let request = "export const list = () => http.get('/invoices')\n";
        let naming = format!("http.defaults.baseURL = 'https://billing.example/v1'\n{request}");

        let unnamed = calls("scripts/list.ts", request);
        let named = calls("scripts/list.ts", &naming);

        assert_eq!(unnamed, []);
        assert_eq!(named, [call(2, "GET", "/v1/invoices")]);
    }

    #[test]
    fn a_path_of_one_fixed_segment_is_a_place_only_beside_a_longer_one() {
        let alone = "export const up = () => fetch(`${BASE}/status`)\n";
        let beside = format!("{alone}export const list = () => fetch(`${{BASE}}/v1/invoices`)\n");

        let found_alone = calls("scripts/up.ts", alone);
        let found_beside = calls("scripts/up.ts", &beside);

        assert_eq!(found_alone, []);
        assert_eq!(
            found_beside,
            [call(1, "GET", "/status"), call(2, "GET", "/v1/invoices")]
        );
    }

    #[test]
    fn a_route_the_file_registers_is_not_a_place() {
        let source = "app.get('/v1/invoices', listInvoices)\n";

        let found = calls("server/routes.ts", source);

        assert_eq!(found, []);
    }

    #[test]
    fn what_is_left_out_is_the_outermost_directory_holding_nothing_included() {
        let selection = selection(&["apps/web/src", "packages/api"]);

        assert_eq!(selection.left_out_with("docs/lib/api.ts"), "docs");
        assert_eq!(
            selection.left_out_with("apps/web/scripts/seed.ts"),
            "apps/web/scripts"
        );
        assert_eq!(selection.left_out_with("apps/admin/src/a.ts"), "apps/admin");
        assert_eq!(selection.left_out_with("seed.ts"), "seed.ts");
    }

    #[test]
    fn a_service_reads_what_it_includes_and_has_spoken_of_what_it_excludes() {
        let mut selection = selection(&["src"]);
        selection
            .excludes
            .push(Glob::new("examples/**").unwrap().compile_matcher());

        assert!(selection.reads("src/api.ts"));
        assert!(selection.reads("examples/quickstart.ts"));
        assert!(!selection.reads("scripts/seed.ts"));
        assert!(!selection.reads("srcs/api.ts"));
    }

    #[test]
    fn a_candidate_counts_the_files_and_the_operations_including_it_would_add() {
        let place = |file: &str, line: u32, method: &str, path: &str| WrittenCall {
            file: file.to_owned(),
            line,
            method: method.to_owned(),
            path: path.to_owned(),
        };
        let written = [
            place("docs/lib/api.ts", 3, "GET", "/v1/invoices"),
            place("docs/lib/api.ts", 9, "*", "/v1/invoices/{}"),
            place("docs/pages/list.tsx", 4, "GET", "/v1/invoices"),
        ];

        let found = candidates("#web", &selection(&["src"]), &operations(), &written);

        assert_eq!(found.len(), 1);
        assert_eq!(found[0].location, "#web:docs");
        assert_eq!((found[0].files, found[0].operations), (2, 3));
        assert_eq!(found[0].places.len(), 3);
    }
}
