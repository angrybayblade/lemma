//! Times `index.ls` on a directory, reading it mapped and positionally.
//! Usage: `store_bench <directory> <store file>`.

use std::collections::BTreeMap;
use std::path::Path;
use std::time::Instant;

use lemma_store::{
    Change, Commit, FileRecord, Index, ReadMode, SourceRecord, Writer, changes_on_disk,
};

const RUNS: usize = 7;
const LOOKUPS: usize = 2_000;

type Failure = Box<dyn std::error::Error>;

fn median_ms(mut run: impl FnMut() -> Result<(), Failure>) -> Result<f64, Failure> {
    let mut times = Vec::new();
    for _ in 0..RUNS {
        let started = Instant::now();
        run()?;
        times.push(started.elapsed().as_secs_f64() * 1000.0);
    }
    times.sort_by(f64::total_cmp);
    Ok(times[RUNS / 2])
}

fn report_reads(store: &Path, mode: ReadMode, sample: &[String]) -> Result<(), Failure> {
    let open = median_ms(|| Index::open(store, mode).map(drop).map_err(Into::into))?;
    let walk = median_ms(|| {
        let index = Index::open(store, mode)?;
        index.files(&index.root("head")?)?;
        Ok(())
    })?;
    let lookups = median_ms(|| {
        let index = Index::open(store, mode)?;
        let root = index.root("head")?;
        for path in sample {
            index.file(&root, path)?;
        }
        Ok(())
    })?;
    let compare = median_ms(|| {
        let index = Index::open(store, mode)?;
        let changes = index.changed_files(&index.root("head")?, &index.root("work")?)?;
        assert_eq!(changes.len(), 1);
        Ok(())
    })?;
    println!(
        "{mode:?}: open {open:.2} ms, decode every file {walk:.1} ms, {} lookups {lookups:.1} ms, compare after one edit {compare:.2} ms",
        sample.len()
    );
    Ok(())
}

fn edited(record: &FileRecord) -> FileRecord {
    let mut record = record.clone();
    record.blob[0] ^= 0xff;
    if let Some(symbol) = record.symbols.first_mut() {
        symbol.digest[0] ^= 0xff;
    }
    record
}

fn main() -> Result<(), Failure> {
    let arguments: Vec<String> = std::env::args().collect();
    let [_, directory, store] = arguments.as_slice() else {
        return Err("usage: store_bench <directory> <store file>".into());
    };
    let store = Path::new(store);
    let source = SourceRecord {
        repository: "bench".to_owned(),
        revision: "head".to_owned(),
        profile: [0; 32],
    };
    let head = Commit {
        root: "head",
        parent: None,
        source: &source,
    };

    let started = Instant::now();
    let changes = changes_on_disk(Path::new(directory), &BTreeMap::new())?;
    let parsed = started.elapsed();
    let started = Instant::now();
    Writer::open(store)?.commit(&head, &changes)?;
    let written = started.elapsed();
    let size = std::fs::metadata(store)?.len();
    println!(
        "{} files: parse {:.2} s, write {:.2} s, {} MiB",
        changes.len(),
        parsed.as_secs_f64(),
        written.as_secs_f64(),
        size >> 20
    );

    let index = Index::open(store, ReadMode::Mapped)?;
    let all = index.files(&index.root("head")?)?;
    let symbols: usize = all.iter().map(|(_, record)| record.symbols.len()).sum();
    let step = (all.len() / LOOKUPS).max(1);
    let sample: Vec<String> = all
        .iter()
        .step_by(step)
        .map(|(path, _)| path.clone())
        .collect();
    println!("{symbols} symbols");

    let (path, record) = &all[all.len() / 2];
    let change = Change::Put {
        path: path.clone(),
        record: edited(record),
    };
    let work = Commit {
        root: "work",
        parent: Some("head"),
        source: &source,
    };
    let mut writer = Writer::open(store)?;
    let started = Instant::now();
    writer.commit(&work, &[change])?;
    let commit_ms = started.elapsed().as_secs_f64() * 1000.0;
    drop(writer);
    let grown = std::fs::metadata(store)?.len() - size;
    println!("one-file commit: {commit_ms:.1} ms, {grown} bytes appended");

    report_reads(store, ReadMode::Mapped, &sample)?;
    report_reads(store, ReadMode::Positional, &sample)?;
    Ok(())
}
