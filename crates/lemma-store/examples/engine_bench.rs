//! Times every operation of the `index.ls` engine on a directory's real
//! files, for tuning the engine. Usage:
//!
//! ```text
//! engine_bench <directory> <work directory> [rounds]
//! ```
//!
//! The directory is parsed once into `<work>/records.ls`, and later runs
//! read the records back from there; a store of another layout is parsed
//! again. Each operation prints its median and fastest time in
//! milliseconds over the rounds, on one line, so two runs compare with
//! `diff`.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::time::Instant;

use lemma_store::{
    Cache, Change, Commit, FactStore, FileRecord, FileStamp, Index, NameMatch, NewSnapshot,
    ReadMode, SnapshotUpdate, SourceRecord, StampedFile, Store, SymbolFilter, Writer,
    changes_on_disk,
};

type Failure = Box<dyn std::error::Error>;
type Files = Vec<(String, FileRecord)>;

struct Bench {
    work: PathBuf,
    rounds: usize,
}

impl Bench {
    /// Runs `operation` once to warm up, then `rounds` times, and prints
    /// the median and fastest time.
    fn time(
        &self,
        name: &str,
        mut operation: impl FnMut() -> Result<(), Failure>,
    ) -> Result<(), Failure> {
        operation()?;
        let mut times = Vec::with_capacity(self.rounds);
        for _ in 0..self.rounds {
            let started = Instant::now();
            operation()?;
            times.push(started.elapsed().as_secs_f64() * 1000.0);
        }
        times.sort_by(f64::total_cmp);
        println!(
            "{name:28} median {:9.3} ms  fastest {:9.3} ms",
            times[times.len() / 2],
            times[0]
        );
        Ok(())
    }

    fn path(&self, name: &str) -> PathBuf {
        self.work.join(name)
    }
}

fn source() -> SourceRecord {
    SourceRecord {
        repository: "bench".to_owned(),
        revision: "head".to_owned(),
        profile: [1; 32],
    }
}

fn put(path: &str, record: &FileRecord) -> Change {
    Change::Put {
        path: path.to_owned(),
        record: record.clone(),
        stamp: FileStamp::default(),
    }
}

/// The directory's files, parsed once and read back after.
fn records(directory: &Path, bench: &Bench) -> Result<Files, Failure> {
    let store = bench.path("records.ls");
    if let Ok(index) = Index::open(&store, ReadMode::Mapped)
        && let Ok(root) = index.root("head")
    {
        return Ok(index.files(&root)?);
    }
    let _ = std::fs::remove_file(&store);
    let changes = changes_on_disk(directory, &BTreeMap::new())?;
    commit(&store, "head", None, &changes)?;
    let index = Index::open(&store, ReadMode::Mapped)?;
    Ok(index.files(&index.root("head")?)?)
}

fn commit(
    store: &Path,
    root: &str,
    parent: Option<&str>,
    changes: &[Change],
) -> Result<(), Failure> {
    let source = source();
    let commit = Commit {
        root,
        parent,
        source: &source,
    };
    Writer::open(store)?.commit(&commit, changes)?;
    Ok(())
}

fn edited(record: &FileRecord, round: u8) -> FileRecord {
    let mut record = record.clone();
    record.blob[0] ^= round.wrapping_add(1);
    if let Some(symbol) = record.symbols.first_mut() {
        symbol.digest[0] ^= round.wrapping_add(1);
    }
    record
}

/// Writing: a full build, then commits of one and of a hundred files.
fn bench_writes(bench: &Bench, files: &Files) -> Result<(), Failure> {
    let changes: Vec<Change> = files
        .iter()
        .map(|(path, record)| put(path, record))
        .collect();
    let store = bench.path("full.ls");
    bench.time("write full segment", || {
        let _ = std::fs::remove_file(&store);
        commit(&store, "head", None, &changes)
    })?;
    println!(
        "{:28} {} bytes",
        "store size",
        std::fs::metadata(&store)?.len()
    );
    let mut round = 0u8;
    let middle = files.len() / 2;
    bench.time("commit one file", || {
        round = round.wrapping_add(1);
        let change = put(&files[middle].0, &edited(&files[middle].1, round));
        commit(&store, "work", Some("head"), &[change])
    })?;
    bench.time("commit 100 files", || {
        round = round.wrapping_add(1);
        let hundred: Vec<Change> = files
            .iter()
            .step_by(files.len() / 100 + 1)
            .map(|(path, record)| put(path, &edited(record, round)))
            .collect();
        commit(&store, "work", Some("head"), &hundred)
    })?;
    Ok(())
}

/// Reading a root: opening, lookups, decoding, comparing, reaching.
fn bench_reads(bench: &Bench, files: &Files) -> Result<(), Failure> {
    let store = bench.path("reads.ls");
    let _ = std::fs::remove_file(&store);
    let changes: Vec<Change> = files
        .iter()
        .map(|(path, record)| put(path, record))
        .collect();
    commit(&store, "head", None, &changes)?;
    let (path, record) = &files[files.len() / 2];
    commit(
        &store,
        "work",
        Some("head"),
        &[put(path, &edited(record, 7))],
    )?;
    let sample: Vec<&str> = files
        .iter()
        .step_by(10)
        .map(|(path, _)| path.as_str())
        .collect();
    bench.time("open", || {
        let index = Index::open(&store, ReadMode::Mapped)?;
        index.root("head")?;
        Ok(())
    })?;
    let index = Index::open(&store, ReadMode::Mapped)?;
    let head = index.root("head")?;
    let work = index.root("work")?;
    bench.time(&format!("{} file lookups", sample.len()), || {
        for path in &sample {
            index.file(&head, path)?;
        }
        Ok(())
    })?;
    bench.time("blobs of every file", || {
        index.blobs(&head).map(drop).map_err(Into::into)
    })?;
    bench.time("decode every file", || {
        index.files(&head).map(drop).map_err(Into::into)
    })?;
    bench.time("compare after one edit", || {
        index
            .changed_files(&head, &work)
            .map(drop)
            .map_err(Into::into)
    })?;
    let start = vec![most_mentioned(files)];
    bench.time("reach from a common name", || {
        index.reaching(&work, &start).map(drop).map_err(Into::into)
    })?;
    Ok(())
}

/// The definition whose name the most symbols mention, as a change would
/// start reaching from it.
fn most_mentioned(files: &Files) -> (String, String) {
    let mut counts: BTreeMap<&str, usize> = BTreeMap::new();
    for (_, record) in files {
        for symbol in &record.symbols {
            for mention in &symbol.mentions {
                *counts.entry(mention.as_str()).or_default() += 1;
            }
        }
    }
    let mut definitions = files
        .iter()
        .flat_map(|(path, record)| record.symbols.iter().map(move |symbol| (path, symbol)));
    let best = definitions
        .by_ref()
        .max_by_key(|(_, symbol)| counts.get(symbol.name.as_str()).copied().unwrap_or(0));
    best.map(|(path, symbol)| (path.clone(), symbol.qualified_name.clone()))
        .unwrap_or_default()
}

fn stamped(files: &Files) -> Vec<StampedFile> {
    files
        .iter()
        .map(|(path, record)| StampedFile {
            path: path.clone(),
            record: record.clone(),
            stamp: FileStamp::default(),
        })
        .collect()
}

fn filter(text: &str, name_match: NameMatch) -> SymbolFilter {
    SymbolFilter {
        text: text.to_owned(),
        name_match,
        kind: None,
        language: None,
        path_contains: None,
        limit: 50,
    }
}

/// The most common symbol name, and a name only one symbol has.
fn names(files: &Files) -> (String, String) {
    let mut counts: BTreeMap<&str, usize> = BTreeMap::new();
    for (_, record) in files {
        for symbol in &record.symbols {
            *counts.entry(symbol.name.as_str()).or_default() += 1;
        }
    }
    let common = counts
        .iter()
        .max_by_key(|(_, count)| **count)
        .map(|(name, _)| (*name).to_owned());
    let rare = counts
        .iter()
        .find(|(_, count)| **count == 1)
        .map(|(name, _)| (*name).to_owned());
    (common.unwrap_or_default(), rare.unwrap_or_default())
}

/// Navigation: indexing, updating and the queries the CLI makes.
fn bench_navigation(bench: &Bench, files: &Files) -> Result<(), Failure> {
    let stamped = stamped(files);
    let snapshot = lemma_model::Snapshot {
        root: "/bench".to_owned(),
        revision: None,
        is_dirty: false,
        fingerprint: "bench".to_owned(),
    };
    let store_path = bench.path("navigation.ls");
    let fresh = NewSnapshot {
        snapshot: &snapshot,
        extractor_version: "1",
        indexed_at_ns: 0,
        files: &stamped,
        uncommitted_paths: &[],
    };
    bench.time("index from scratch", || {
        let _ = std::fs::remove_file(&store_path);
        Store::open(&store_path)?.replace_snapshot(&fresh)?;
        Ok(())
    })?;
    let mut store = Store::open(&store_path)?;
    let id = store
        .snapshot_for_root("/bench")?
        .map(|stored| stored.id)
        .unwrap_or_default();
    let mut round = 0u8;
    bench.time("update one file", || {
        round = round.wrapping_add(1);
        let mut changed = stamped[stamped.len() / 3].clone();
        changed.record = edited(&changed.record, round);
        let update = SnapshotUpdate {
            snapshot: &snapshot,
            indexed_at_ns: 0,
            uncommitted_paths: &[],
            changed: std::slice::from_ref(&changed),
            removed: &[],
            restamped: &[],
        };
        store.update_snapshot(id, &update)?;
        Ok(())
    })?;
    bench_queries(bench, &store, id, files)
}

fn bench_queries(bench: &Bench, store: &Store, id: i64, files: &Files) -> Result<(), Failure> {
    let (common, rare) = names(files);
    bench.time("symbols, common exact name", || {
        store
            .find_symbols(id, &filter(&common, NameMatch::Exact))
            .map(drop)
            .map_err(Into::into)
    })?;
    bench.time("symbols, rare exact name", || {
        store
            .find_symbols(id, &filter(&rare, NameMatch::Exact))
            .map(drop)
            .map_err(Into::into)
    })?;
    let fragment: String = common.chars().take(4).collect();
    bench.time("symbols containing text", || {
        store
            .find_symbols(id, &filter(&fragment, NameMatch::Contains))
            .map(drop)
            .map_err(Into::into)
    })?;
    bench.time("symbol keys", || {
        store.symbol_keys(id).map(drop).map_err(Into::into)
    })?;
    bench.time("files", || store.files(id).map(drop).map_err(Into::into))?;
    let paths: Vec<&str> = files
        .iter()
        .step_by(100)
        .map(|(path, _)| path.as_str())
        .collect();
    bench.time(&format!("{} outlines", paths.len()), || {
        for path in &paths {
            store.symbols_in_file(id, path)?;
        }
        Ok(())
    })?;
    let keys = store.symbol_keys(id)?;
    let ids: Vec<_> = keys
        .iter()
        .step_by(keys.len() / 1000 + 1)
        .map(|key| key.id)
        .collect();
    bench.time(&format!("{} symbols by id", ids.len()), || {
        store.symbols_by_ids(id, &ids).map(drop).map_err(Into::into)
    })?;
    Ok(())
}

/// The check cache: writing units, reading some back, missing others.
fn bench_cache(bench: &Bench, count: usize) -> Result<(), Failure> {
    let path = bench.path("cache.ls");
    let value = |unit: usize| -> Vec<u8> {
        let length = 2_000 + (unit * 7919) % 4_000;
        (0..length)
            .map(|at| u8::try_from((at * 31 + unit) % 251).unwrap_or(0))
            .collect()
    };
    let units: Vec<(String, Vec<u8>)> = (0..count)
        .map(|unit| (format!("unit-{unit}"), value(unit)))
        .collect();
    bench.time(&format!("put {count} units"), || {
        let _ = std::fs::remove_file(&path);
        Cache::open(&path)?.put_many(&units)?;
        Ok(())
    })?;
    let cache = Cache::open(&path)?;
    let wanted: Vec<String> = (0..count)
        .step_by(10)
        .map(|unit| format!("unit-{unit}"))
        .collect();
    bench.time(&format!("get {} units", wanted.len()), || {
        cache
            .get_decoded(&wanted, |bytes| Some(bytes.len()))
            .map(drop)
            .map_err(Into::into)
    })?;
    let missing: Vec<String> = (0..wanted.len())
        .map(|unit| format!("missing-{unit}"))
        .collect();
    bench.time(&format!("miss {} units", missing.len()), || {
        cache
            .get_decoded(&missing, |bytes| Some(bytes.len()))
            .map(drop)
            .map_err(Into::into)
    })?;
    let terms: Vec<(String, Vec<String>)> = units
        .iter()
        .map(|(key, _)| (key.clone(), vec![format!("path:/v1/{}", key.len() % 50)]))
        .collect();
    let mut writer = Cache::open(&path)?;
    writer.put_terms_later(terms);
    drop(writer);
    let all: Vec<String> = units.iter().map(|(key, _)| key.clone()).collect();
    bench.time(&format!("terms of {count} units"), || {
        cache.terms_of(&all).map(drop).map_err(Into::into)
    })?;
    Ok(())
}

fn main() -> Result<(), Failure> {
    let arguments: Vec<String> = std::env::args().collect();
    let (Some(directory), Some(work)) = (arguments.get(1), arguments.get(2)) else {
        return Err("usage: engine_bench <directory> <work directory> [rounds]".into());
    };
    let rounds = arguments
        .get(3)
        .and_then(|text| text.parse().ok())
        .unwrap_or(5);
    let bench = Bench {
        work: PathBuf::from(work),
        rounds,
    };
    std::fs::create_dir_all(&bench.work)?;
    let files = records(Path::new(directory), &bench)?;
    let symbols: usize = files.iter().map(|(_, record)| record.symbols.len()).sum();
    println!("{} files, {symbols} symbols, {rounds} rounds", files.len());
    bench_writes(&bench, &files)?;
    bench_reads(&bench, &files)?;
    bench_navigation(&bench, &files)?;
    bench_cache(&bench, files.len())?;
    Ok(())
}
