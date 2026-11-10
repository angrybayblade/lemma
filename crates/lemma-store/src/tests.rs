use std::cell::RefCell;
use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use lemma_model::Language;
use lemma_syntax::Extractor;

use crate::{
    Change, Commit, FileStamp, Index, ReadMode, SourceRecord, StoreError, SymbolChangeKind, Writer,
    changed_symbols, changes_on_disk, read_file,
};

thread_local! {
    /// Compiling the grammars' queries is slow, so tests share them.
    static EXTRACTOR: RefCell<Extractor> = RefCell::new(Extractor::new().unwrap());
}

const MODES: [ReadMode; 2] = [ReadMode::Mapped, ReadMode::Positional];

fn source() -> SourceRecord {
    SourceRecord {
        repository: "billing".to_owned(),
        revision: "abc".to_owned(),
        profile: [7; 32],
    }
}

fn put(path: &str, code: &str) -> Change {
    let record = EXTRACTOR
        .with_borrow_mut(|extractor| read_file(extractor, Language::Python, code).unwrap());
    Change::Put {
        path: path.to_owned(),
        record,
        stamp: FileStamp::default(),
    }
}

fn commit(store: &Path, root: &str, parent: Option<&str>, changes: &[Change]) {
    let source = source();
    let commit = Commit {
        root,
        parent,
        source: &source,
    };
    Writer::open(store)
        .unwrap()
        .commit(&commit, changes)
        .unwrap();
}

fn store_in(directory: &tempfile::TempDir) -> PathBuf {
    directory.path().join("index.ls")
}

fn first_files() -> Vec<Change> {
    vec![
        put("api/routes.py", "def create():\n    return helper()\n"),
        put("api/models.py", "class Order:\n    total = 1\n"),
        put("jobs/sweep.py", "def sweep():\n    return 1\n"),
    ]
}

#[test]
fn reopening_gives_the_files_that_were_committed() {
    let directory = tempfile::tempdir().unwrap();
    let store = store_in(&directory);
    let changes = first_files();
    commit(&store, "head", None, &changes);

    for mode in MODES {
        let index = Index::open(&store, mode).unwrap();
        let root = index.root("head").unwrap();
        let files: BTreeMap<_, _> = index.files(&root).unwrap().into_iter().collect();

        assert_eq!(root.source, source());
        assert_eq!(files.len(), 3);
        for change in &changes {
            let Change::Put { path, record, .. } = change else {
                panic!("the fixture only puts");
            };
            assert_eq!(&files[path], record);
            assert_eq!(index.file(&root, path).unwrap().as_ref(), Some(record));
        }
    }
}

#[test]
fn a_missing_path_is_no_file() {
    let directory = tempfile::tempdir().unwrap();
    let store = store_in(&directory);
    commit(&store, "head", None, &first_files());

    let index = Index::open(&store, ReadMode::Mapped).unwrap();
    let root = index.root("head").unwrap();

    assert_eq!(index.file(&root, "api/none.py").unwrap(), None);
    assert_eq!(index.file(&root, "api").unwrap(), None);
    assert_eq!(index.file(&root, "api/routes.py/x").unwrap(), None);
}

#[test]
fn a_source_derived_from_another_leaves_the_other_unchanged() {
    let directory = tempfile::tempdir().unwrap();
    let store = store_in(&directory);
    commit(&store, "head", None, &first_files());
    let edit = [put("api/routes.py", "def create():\n    return other()\n")];
    commit(&store, "work", Some("head"), &edit);

    let index = Index::open(&store, ReadMode::Mapped).unwrap();
    let head = index.root("head").unwrap();
    let work = index.root("work").unwrap();

    assert_ne!(head.digest(), work.digest());
    assert_eq!(index.root_names().unwrap(), ["head", "work"]);
    let before = index.file(&head, "api/routes.py").unwrap().unwrap();
    assert!(before.symbols[0].mentions.contains(&"helper".to_owned()));
    assert_eq!(index.files(&work).unwrap().len(), 3);
}

#[test]
fn an_edit_appends_only_its_path() {
    let directory = tempfile::tempdir().unwrap();
    let store = store_in(&directory);
    let mut changes = first_files();
    for number in 0..200 {
        let path = format!("jobs/job{number}.py");
        changes.push(put(&path, "def run():\n    return 1\n"));
    }
    commit(&store, "head", None, &changes);
    let before = std::fs::metadata(&store).unwrap().len();

    let edit = [put("api/routes.py", "def create():\n    return 2\n")];
    commit(&store, "work", Some("head"), &edit);

    let grown = std::fs::metadata(&store).unwrap().len() - before;
    assert!(grown < 1_000, "one edit appended {grown} of {before} bytes");
}

#[test]
fn committing_identical_content_changes_no_digest() {
    let directory = tempfile::tempdir().unwrap();
    let store = store_in(&directory);
    commit(&store, "head", None, &first_files());
    commit(&store, "work", Some("head"), &first_files());

    let index = Index::open(&store, ReadMode::Mapped).unwrap();
    let head = index.root("head").unwrap();
    let work = index.root("work").unwrap();

    assert_eq!(head.digest(), work.digest());
    assert_eq!(index.changed_files(&head, &work).unwrap(), []);
}

#[test]
fn the_order_of_changes_does_not_change_the_bytes() {
    let directory = tempfile::tempdir().unwrap();
    let forward = directory.path().join("forward.ls");
    let backward = directory.path().join("backward.ls");
    let mut changes = first_files();
    commit(&forward, "head", None, &changes);
    changes.reverse();
    commit(&backward, "head", None, &changes);

    assert_eq!(
        std::fs::read(forward).unwrap(),
        std::fs::read(backward).unwrap()
    );
}

#[test]
fn removing_the_last_file_of_a_directory_removes_the_directory() {
    let directory = tempfile::tempdir().unwrap();
    let store = store_in(&directory);
    commit(&store, "head", None, &first_files());
    let removal = [Change::Remove {
        path: "jobs/sweep.py".to_owned(),
    }];
    commit(&store, "work", Some("head"), &removal);

    let index = Index::open(&store, ReadMode::Mapped).unwrap();
    let work = index.root("work").unwrap();

    let paths: Vec<_> = index
        .files(&work)
        .unwrap()
        .into_iter()
        .map(|f| f.0)
        .collect();
    assert_eq!(paths, ["api/models.py", "api/routes.py"]);
}

#[test]
fn comparing_sources_names_the_symbols_that_changed() {
    let directory = tempfile::tempdir().unwrap();
    let store = store_in(&directory);
    let before =
        "def kept():\n    return 1\n\ndef edited():\n    return 1\n\ndef gone():\n    pass\n";
    let after =
        "\n\ndef kept():\n    return 1\n\ndef edited():\n    return 2\n\ndef new():\n    pass\n";
    commit(&store, "head", None, &[put("a.py", before)]);
    commit(&store, "work", Some("head"), &[put("a.py", after)]);

    let index = Index::open(&store, ReadMode::Mapped).unwrap();
    let head = index.root("head").unwrap();
    let work = index.root("work").unwrap();
    let files = index.changed_files(&head, &work).unwrap();
    let changes = changed_symbols(&files[0]);

    let found: Vec<_> = changes
        .iter()
        .map(|change| (change.qualified_name.as_str(), change.change))
        .collect();
    assert_eq!(
        found,
        [
            ("edited", SymbolChangeKind::Changed),
            ("gone", SymbolChangeKind::Removed),
            ("new", SymbolChangeKind::Added),
        ]
    );
}

#[test]
fn a_write_cut_at_any_byte_leaves_the_last_complete_commit() {
    let directory = tempfile::tempdir().unwrap();
    let store = store_in(&directory);
    commit(&store, "head", None, &first_files());
    let complete = std::fs::read(&store).unwrap().len();
    let edit = [put("api/routes.py", "def create():\n    return 2\n")];
    commit(&store, "work", Some("head"), &edit);
    let bytes = std::fs::read(&store).unwrap();
    let cut_store = directory.path().join("cut.ls");

    for cut in complete..bytes.len() {
        std::fs::write(&cut_store, &bytes[..cut]).unwrap();
        for mode in MODES {
            let index = Index::open(&cut_store, mode).unwrap();
            assert_eq!(index.root_names().unwrap(), ["head"], "cut at {cut}");
            let root = index.root("head").unwrap();
            assert_eq!(index.files(&root).unwrap().len(), 3);
        }
    }
}

#[test]
fn a_commit_after_an_interrupted_one_is_read() {
    let directory = tempfile::tempdir().unwrap();
    let store = store_in(&directory);
    commit(&store, "head", None, &first_files());
    let complete = std::fs::read(&store).unwrap().len();
    let edit = [put("api/routes.py", "def create():\n    return 2\n")];
    commit(&store, "lost", Some("head"), &edit);
    let bytes = std::fs::read(&store).unwrap();
    std::fs::write(&store, &bytes[..complete + 40]).unwrap();

    commit(&store, "work", Some("head"), &edit);

    let index = Index::open(&store, ReadMode::Positional).unwrap();
    assert_eq!(index.root_names().unwrap(), ["head", "work"]);
    let work = index.root("work").unwrap();
    assert_eq!(index.files(&work).unwrap().len(), 3);
}

#[test]
fn a_store_interrupted_while_being_created_has_no_commits() {
    let directory = tempfile::tempdir().unwrap();
    let store = store_in(&directory);
    std::fs::write(&store, b"LML").unwrap();

    for mode in MODES {
        let index = Index::open(&store, mode).unwrap();
        assert_eq!(index.root_names().unwrap(), Vec::<String>::new());
    }
    commit(&store, "head", None, &first_files());
    let index = Index::open(&store, ReadMode::Mapped).unwrap();
    assert_eq!(index.root_names().unwrap(), ["head"]);
}

#[test]
fn damage_since_the_last_flush_ends_the_store_before_it() {
    let directory = tempfile::tempdir().unwrap();
    let store = store_in(&directory);
    commit(&store, "head", None, &first_files());
    let complete = std::fs::read(&store).unwrap().len();
    let edit = [put("api/routes.py", "def create():\n    return 2\n")];
    commit(&store, "work", Some("head"), &edit);
    let mut bytes = std::fs::read(&store).unwrap();
    bytes[complete + 20] ^= 0xff;
    std::fs::write(&store, bytes).unwrap();

    for mode in MODES {
        let index = Index::open(&store, mode).unwrap();
        assert_eq!(index.root_names().unwrap(), ["head"]);
    }
}

#[test]
fn a_flushed_commit_is_trusted_and_damage_that_breaks_it_is_an_error() {
    let directory = tempfile::tempdir().unwrap();
    let store = store_in(&directory);
    commit(&store, "head", None, &first_files());
    Writer::open(&store).unwrap().checkpoint().unwrap();
    let mut bytes = std::fs::read(&store).unwrap();
    // The segment's first section offset, just past its node header and
    // four counts: pointing it outside the segment breaks its shape.
    bytes[8 + 5 + 16] = 0xff;
    bytes[8 + 5 + 19] = 0x7f;
    std::fs::write(&store, bytes).unwrap();

    for mode in MODES {
        let index = Index::open(&store, mode).unwrap();
        let root = index.root("head").unwrap();
        assert!(matches!(
            index.files(&root),
            Err(StoreError::Damaged { .. })
        ));
    }
}

#[test]
fn deltas_past_the_limit_merge_without_changing_what_is_read() {
    let directory = tempfile::tempdir().unwrap();
    let store = store_in(&directory);
    let mut changes = first_files();
    for number in 0..40 {
        changes.push(put(
            &format!("jobs/job{number}.py"),
            "def run():\n    return 1\n",
        ));
    }
    commit(&store, "head", None, &changes);
    for round in 0..30 {
        let edit = [put(
            &format!("jobs/job{round}.py"),
            &format!("def run():\n    return {round}\n"),
        )];
        commit(&store, "head", Some("head"), &edit);
    }

    let index = Index::open(&store, ReadMode::Mapped).unwrap();
    let head = index.root("head").unwrap();
    assert!(head.segments.len() <= 9, "{} segments", head.segments.len());
    let files = index.files(&head).unwrap();
    assert_eq!(files.len(), 43);
    let job = index.file(&head, "jobs/job29.py").unwrap().unwrap();
    let fresh = put("jobs/job29.py", "def run():\n    return 29\n");
    let Change::Put { record, .. } = fresh else {
        panic!("put makes a put");
    };
    assert_eq!(job, record);
}

#[test]
fn the_same_files_give_the_same_digest_however_committed() {
    let directory = tempfile::tempdir().unwrap();
    let store = store_in(&directory);
    let edit = [put("api/routes.py", "def create():\n    return 2\n")];
    commit(&store, "head", None, &first_files());
    commit(&store, "work", Some("head"), &edit);
    let mut fresh = first_files();
    fresh[0] = edit[0].clone();
    commit(&store, "fresh", None, &fresh);

    let index = Index::open(&store, ReadMode::Mapped).unwrap();

    assert_eq!(
        index.root("work").unwrap().digest(),
        index.root("fresh").unwrap().digest()
    );
}

#[test]
fn compaction_keeps_what_the_roots_and_runs_hold() {
    let directory = tempfile::tempdir().unwrap();
    let store = store_in(&directory);
    commit(&store, "head", None, &first_files());
    commit(&store, "old", None, &first_files());
    let mut writer = Writer::open(&store).unwrap();
    writer
        .put_attributes(&[([1; 32], b"one".to_vec())])
        .unwrap();
    writer
        .put_attributes(&[([1; 32], b"two".to_vec())])
        .unwrap();
    writer
        .write(&crate::Write {
            commit: None,
            removed_roots: &["old".to_owned()],
            attributes: &[],
        })
        .unwrap();
    let before = std::fs::metadata(&store).unwrap().len();

    writer.compact().unwrap();
    drop(writer);

    let after = std::fs::metadata(&store).unwrap().len();
    assert!(after < before, "{after} of {before} bytes");
    let index = Index::open(&store, ReadMode::Mapped).unwrap();
    assert_eq!(index.root_names().unwrap(), ["head"]);
    assert_eq!(index.files(&index.root("head").unwrap()).unwrap().len(), 3);
    let runs = index.runs().unwrap();
    assert_eq!(
        index.attribute_in(&runs, &[1; 32]).unwrap(),
        Some(b"two".to_vec())
    );
}

#[test]
fn a_store_of_another_layout_is_started_again_by_its_writer() {
    let directory = tempfile::tempdir().unwrap();
    let store = store_in(&directory);
    std::fs::write(&store, b"LMLS\x01\x00\x00\x00old bytes").unwrap();

    assert!(matches!(
        Index::open(&store, ReadMode::Mapped),
        Err(StoreError::UnsupportedVersion { .. })
    ));
    commit(&store, "head", None, &first_files());

    let index = Index::open(&store, ReadMode::Mapped).unwrap();
    assert_eq!(index.root_names().unwrap(), ["head"]);
}

#[test]
fn a_foreign_file_is_not_a_store() {
    let directory = tempfile::tempdir().unwrap();
    let store = store_in(&directory);
    std::fs::write(&store, b"not a store at all").unwrap();

    let opened = Index::open(&store, ReadMode::Positional);

    assert!(matches!(opened, Err(StoreError::NotAStore { .. })));
}

#[test]
fn a_store_has_one_writer_at_a_time() {
    let directory = tempfile::tempdir().unwrap();
    let store = store_in(&directory);
    let _first = Writer::open(&store).unwrap();

    let second = Writer::open(&store);

    assert!(matches!(second, Err(StoreError::Busy { .. })));
}

#[test]
fn scanning_again_reports_only_what_changed() {
    let code = tempfile::tempdir().unwrap();
    std::fs::write(code.path().join("a.py"), "def a():\n    return 1\n").unwrap();
    std::fs::write(code.path().join("b.py"), "def b():\n    return 1\n").unwrap();
    let directory = tempfile::tempdir().unwrap();
    let store = store_in(&directory);
    let first = changes_on_disk(code.path(), &BTreeMap::new()).unwrap();
    commit(&store, "head", None, &first);

    std::fs::write(code.path().join("a.py"), "def a():\n    return 2\n").unwrap();
    std::fs::remove_file(code.path().join("b.py")).unwrap();
    let index = Index::open(&store, ReadMode::Mapped).unwrap();
    let known = index.blobs(&index.root("head").unwrap()).unwrap();
    let mut second = changes_on_disk(code.path(), &known).unwrap();
    second.sort_by(|left, right| left.path().cmp(right.path()));

    assert_eq!(second.len(), 2);
    assert!(matches!(&second[0], Change::Put { path, .. } if path == "a.py"));
    assert!(matches!(&second[1], Change::Remove { path } if path == "b.py"));
}

#[test]
fn small_commits_are_flushed_before_their_unchecked_stretch_grows_long() {
    let directory = tempfile::tempdir().unwrap();
    let store = store_in(&directory);
    commit(&store, "head", None, &first_files());
    let filler = vec![7u8; 200_000];
    for round in 0..40u8 {
        let mut writer = Writer::open(&store).unwrap();
        writer
            .put_attributes(&[([round; 32], filler.clone())])
            .unwrap();
    }

    let index = Index::open(&store, ReadMode::Mapped).unwrap();
    let footer = index.footer().unwrap();
    assert!(
        footer.offset - footer.synced < 5 << 20,
        "{} unflushed bytes",
        footer.offset - footer.synced
    );
}
