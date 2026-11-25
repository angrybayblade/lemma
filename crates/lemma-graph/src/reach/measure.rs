//! An explicit, ignored probe of what a change reaches in repositories the
//! user selects: for each job, the files the walk reaches from the lines a
//! change touched, with how many names away each is. No source is executed.
//!
//! `LEMMA_REACH_JOBS` names a file of JSON lines, one job each:
//! `{ "id", "root", "files", "changed": [[path, [[first, last], ...]], ...] }`,
//! where `files` names a list of the tree's paths, one a line. Each job's
//! answer is a line of `LEMMA_REACH_OUT`.

use std::collections::{BTreeMap, BTreeSet, HashMap, VecDeque};
use std::io::Write as _;
use std::path::Path;
use std::time::Instant;

use rayon::prelude::*;
use serde::{Deserialize, Serialize};

use super::*;
use crate::affects::file_functions;
use crate::impact::Function;

#[derive(Deserialize)]
struct Job {
    id: String,
    root: String,
    files: String,
    changed: Vec<(String, Vec<(u32, u32)>)>,
}

#[derive(Serialize)]
struct Answer {
    id: String,
    /// Functions read, top-level code included.
    functions: usize,
    /// Whether the walk gives up and reaches everything.
    everything: bool,
    /// How many files are in the language families reached whole.
    whole_family_files: usize,
    seeds: BTreeSet<String>,
    reached_names: usize,
    /// Each reached file with the fewest names followed to reach it.
    files: BTreeMap<String, u32>,
    read_ms: u128,
    walk_ms: u128,
}

/// The functions of every code file a tree lists.
pub(super) fn read_tree(root: &Path, list: &str) -> Functions {
    let paths: Vec<&str> = list.lines().filter(|path| is_read_code(path)).collect();
    let functions: Vec<Function> = paths
        .par_iter()
        .map_init(
            || lemma_syntax::Extractor::new().unwrap(),
            |extractor, path| {
                let language = crate::affects::functions_language(path)?;
                let contents = std::fs::read_to_string(root.join(path)).ok()?;
                let extraction = extractor.extract(language, &contents).ok()?;
                Some(file_functions(path, &contents, extraction.symbols))
            },
        )
        .flatten()
        .flatten()
        .collect();
    Functions::from_functions(&functions)
}

/// Names still to follow, each with its language family and how many
/// names were followed to reach it.
#[derive(Default)]
struct Frontier {
    seen: BTreeSet<(NameId, u8)>,
    queue: VecDeque<(NameId, u8, u32)>,
}

impl Frontier {
    fn reach(&mut self, functions: &Functions, name: NameId, language: u8, depth: u32) {
        if !functions.text(name).is_empty() && self.seen.insert((name, language)) {
            self.queue.push_back((name, language, depth));
        }
    }
}

/// The walk of [`walk`], keeping how many names were followed to each file.
fn depths(seeds: &[(String, u8)], functions: &Functions) -> BTreeMap<String, u32> {
    let mut mentioned_by: HashMap<NameId, Vec<usize>> = HashMap::new();
    for function in 0..functions.len() {
        for name in functions.mentions(function) {
            mentioned_by.entry(*name).or_default().push(function);
        }
    }
    let mut frontier = Frontier::default();
    for (name, language) in seeds {
        if let Some(id) = functions.lookup(name) {
            frontier.reach(functions, id, *language, 0);
        }
    }
    let mut files: BTreeMap<String, u32> = BTreeMap::new();
    while let Some((name, language, depth)) = frontier.queue.pop_front() {
        let mentioners = mentioned_by.get(&name).map_or(&[][..], Vec::as_slice);
        for function in mentioners {
            let file = functions.file_text(*function);
            if family(file) != language {
                continue;
            }
            files.entry(file.to_owned()).or_insert(depth + 1);
            for reached in functions.callable_names(*function) {
                frontier.reach(functions, *reached, language, depth + 1);
            }
        }
    }
    files
}

/// How many of `functions`' files are in a family `reach` reaches whole.
fn whole_family_files(reach: &ChangeReach, functions: &Functions) -> usize {
    let files: BTreeSet<&str> = (0..functions.len())
        .map(|function| functions.file_text(function))
        .filter(|file| reach.whole_families.contains(&family(file)))
        .collect();
    files.len()
}

fn answer(job: &Job, functions: &Functions, read_ms: u128) -> Answer {
    let root = Path::new(&job.root);
    let started = Instant::now();
    let mut reach = ChangeReach {
        repository: String::new(),
        base: String::new(),
        everything: !is_read_as_functions(&job.changed, functions),
        reached: HashSet::new(),
        files: HashSet::new(),
        families: HashSet::new(),
        whole_families: HashSet::new(),
        route_files: HashMap::new(),
    };
    let Seeds {
        names: seeds,
        whole_families,
    } = seed_names(root, &job.changed, functions);
    reach.whole_families.extend(whole_families);
    walk(&mut reach, seeds.clone(), functions);
    let walk_ms = started.elapsed().as_millis();
    let files = depths(&seeds, functions);
    // The probe's walk must be the product's: the same files.
    let product: BTreeSet<&str> = reach.files.iter().map(String::as_str).collect();
    let probe: BTreeSet<&str> = files.keys().map(String::as_str).collect();
    assert_eq!(product, probe, "job {}", job.id);
    Answer {
        id: job.id.clone(),
        functions: functions.len(),
        everything: reach.everything,
        whole_family_files: whole_family_files(&reach, functions),
        seeds: seeds.into_iter().map(|(name, _)| name).collect(),
        reached_names: reach.len(),
        files,
        read_ms,
        walk_ms,
    }
}

#[test]
#[ignore = "requires LEMMA_REACH_JOBS and LEMMA_REACH_OUT; reads selected repositories without executing them"]
fn reach_of_selected_changes() {
    let jobs = std::fs::read_to_string(std::env::var("LEMMA_REACH_JOBS").unwrap()).unwrap();
    let mut out = std::fs::File::create(std::env::var("LEMMA_REACH_OUT").unwrap()).unwrap();
    // Jobs on one tree follow one another and share its reading.
    let mut tree: Option<(String, Functions, u128)> = None;
    for line in jobs.lines().filter(|line| !line.trim().is_empty()) {
        let job: Job = serde_json::from_str(line).unwrap();
        if tree
            .as_ref()
            .is_none_or(|(files, _, _)| *files != job.files)
        {
            let started = Instant::now();
            let list = std::fs::read_to_string(&job.files).unwrap();
            let functions = read_tree(Path::new(&job.root), &list);
            tree = Some((job.files.clone(), functions, started.elapsed().as_millis()));
        }
        let (_, functions, read_ms) = tree.as_ref().unwrap();
        let answer = answer(&job, functions, *read_ms);
        writeln!(out, "{}", serde_json::to_string(&answer).unwrap()).unwrap();
    }
}
