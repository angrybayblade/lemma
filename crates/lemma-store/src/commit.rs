//! What a commit appends for a source (spec 17): a full segment when it
//! starts from nothing, a delta segment with only the changed files when
//! it has a parent, and merged segments once a root's deltas pile up.

use std::borrow::Cow;
use std::collections::BTreeMap;

use crate::ids::Lanes;
use crate::read::{self, Index, Root};
use crate::segment::{self, Entry, Segment};
use crate::view::{At, View};
use crate::write::{Commit, Pending};
use rayon::prelude::*;

use crate::{Change, FileRecord, FileStamp, StoreError};

/// How many delta segments a root stacks on its full segment before they
/// are merged.
const MAX_DELTAS: usize = 8;

/// A delta segment's entries, sorted by path.
type Entries<'c> = Vec<(&'c str, Entry<'c>)>;

/// A change to one path, borrowing what it puts, so a caller holding its
/// records need not copy them into [`Change`]s.
#[derive(Clone, Copy)]
pub(crate) enum Edit<'a> {
    Put {
        record: &'a FileRecord,
        stamp: FileStamp,
    },
    Remove,
    Restamp {
        stamp: FileStamp,
    },
}

/// Edits sorted by path, one per path.
pub(crate) type Edits<'a> = Vec<(&'a str, Edit<'a>)>;

/// The edits `changes` make, each path's last change kept.
pub(crate) fn edits_of(changes: &[Change]) -> Edits<'_> {
    let edits = changes
        .iter()
        .map(|change| {
            let edit = match change {
                Change::Put { record, stamp, .. } => Edit::Put {
                    record,
                    stamp: *stamp,
                },
                Change::Remove { .. } => Edit::Remove,
                Change::Restamp { stamp, .. } => Edit::Restamp { stamp: *stamp },
            };
            (change.path(), edit)
        })
        .collect();
    latest_by_path(edits)
}

/// `edits` sorted by path with only the last edit of each path. Edits
/// already sorted, as a fresh index gives them, are not sorted again.
pub(crate) fn latest_by_path(mut edits: Edits<'_>) -> Edits<'_> {
    if edits.windows(2).all(|pair| pair[0].0 < pair[1].0) {
        return edits;
    }
    // Stable, so of two edits to one path the later stays later.
    edits.sort_by(|left, right| left.0.cmp(right.0));
    let mut latest: Edits<'_> = Vec::with_capacity(edits.len());
    for edit in edits {
        match latest.last_mut() {
            Some(last) if last.0 == edit.0 => *last = edit,
            _ => latest.push(edit),
        }
    }
    latest
}

impl Pending {
    /// Appends what `commit` needs and returns the root it describes.
    pub(crate) fn commit_root(
        &mut self,
        index: &Index,
        roots: &BTreeMap<String, Root>,
        commit: &Commit<'_>,
        changes: &Edits<'_>,
    ) -> Result<Root, StoreError> {
        let Some(parent_name) = commit.parent else {
            return self.full_root(commit, changes);
        };
        let parent = roots
            .get(parent_name)
            .ok_or_else(|| StoreError::UnknownRoot {
                name: parent_name.to_owned(),
            })?;
        let view = index.view(parent)?;
        let (entries, lanes) = delta_entries(&view, Lanes::from_digest(&parent.digest), changes)?;
        let mut root = Root {
            source: commit.source.clone(),
            digest: lanes.to_digest(),
            segments: parent.segments.clone(),
        };
        if entries.is_empty() {
            return Ok(root);
        }
        let delta = segment::build(&entries)?;
        if parent.segments.len() <= MAX_DELTAS {
            root.segments
                .push(self.append_node(read::SEGMENT_NODE, &delta)?);
            return Ok(root);
        }
        root.segments = self.merged(index, parent, &delta)?;
        Ok(root)
    }

    fn full_root(&mut self, commit: &Commit<'_>, changes: &Edits<'_>) -> Result<Root, StoreError> {
        let entries: Entries<'_> = changes
            .iter()
            .filter_map(|(path, edit)| match edit {
                Edit::Put { record, stamp } => Some((
                    *path,
                    Entry::Put {
                        record: Cow::Borrowed(*record),
                        stamp: *stamp,
                    },
                )),
                Edit::Remove | Edit::Restamp { .. } => None,
            })
            .collect();
        let lanes = entries
            .par_iter()
            .fold(Lanes::default, |mut lanes, (path, entry)| {
                if let Entry::Put { record, .. } = entry {
                    lanes.add(path, &record.blob);
                }
                lanes
            })
            .reduce(Lanes::default, Lanes::plus);
        let offset = self.append_segment(&entries)?;
        Ok(Root {
            source: commit.source.clone(),
            digest: lanes.to_digest(),
            segments: vec![offset],
        })
    }

    /// The segments of `parent` with `delta` on top, merged: its deltas
    /// into one, or everything into a new full segment once the deltas
    /// hold more than a quarter of the full segment's files.
    fn merged(
        &mut self,
        index: &Index,
        parent: &Root,
        delta: &[u8],
    ) -> Result<Vec<u64>, StoreError> {
        let mut segments = parent
            .segments
            .iter()
            .map(|offset| index.segment(*offset))
            .collect::<Result<Vec<_>, _>>()?;
        segments.push(Segment::parse(Cow::Borrowed(delta))?);
        let view = View::new(segments);
        let full = view.segment(0)?;
        let rows = view.delta_rows()?;
        if rows.len() * 4 > full.file_count() {
            let files = view.files()?;
            let records = decoded(&view, files)?;
            let offset = self.append_segment(&entries_of(&records))?;
            return Ok(vec![offset]);
        }
        let mut removed = Vec::new();
        let mut kept = Vec::new();
        for (path, at) in rows {
            if !view.row(at)?.is_removed {
                kept.push(at);
                continue;
            }
            if full.find_path(path)?.is_some() {
                removed.push(path);
            }
        }
        let records = decoded(&view, kept)?;
        let mut entries = entries_of(&records);
        entries.extend(removed.into_iter().map(|path| (path, Entry::Removed)));
        entries.sort_by(|left, right| left.0.cmp(right.0));
        let first = parent
            .segments
            .first()
            .copied()
            .ok_or(StoreError::Damaged {
                what: "a root has no segments",
            })?;
        if entries.is_empty() {
            return Ok(vec![first]);
        }
        let offset = self.append_segment(&entries)?;
        Ok(vec![first, offset])
    }
}

/// What a delta holds for `changes` against `view`, and the root digest
/// with them applied. A change that changes nothing is left out.
fn delta_entries<'c>(
    view: &View<'_>,
    mut lanes: Lanes,
    changes: &Edits<'c>,
) -> Result<(Entries<'c>, Lanes), StoreError> {
    let mut entries = Vec::new();
    for (path, change) in changes {
        let current = view.locate(path)?;
        let Some(entry) = delta_entry(view, current, change)? else {
            continue;
        };
        if let Some(at) = current {
            lanes.subtract(path, &view.row(at)?.blob);
        }
        if let Entry::Put { record, .. } = &entry {
            lanes.add(path, &record.blob);
        }
        entries.push((*path, entry));
    }
    Ok((entries, lanes))
}

fn delta_entry<'c>(
    view: &View<'_>,
    current: Option<At>,
    change: &Edit<'c>,
) -> Result<Option<Entry<'c>>, StoreError> {
    let put = |record: &'c FileRecord, stamp: FileStamp| Entry::Put {
        record: Cow::Borrowed(record),
        stamp,
    };
    let Some(at) = current else {
        return Ok(match change {
            Edit::Put { record, stamp } => Some(put(record, *stamp)),
            Edit::Remove | Edit::Restamp { .. } => None,
        });
    };
    let row = view.row(at)?;
    match change {
        Edit::Put { record, stamp } => {
            let unchanged =
                row.blob == record.blob && row.stamp == *stamp && view.record(at)? == **record;
            Ok((!unchanged).then(|| put(record, *stamp)))
        }
        Edit::Remove => Ok(Some(Entry::Removed)),
        Edit::Restamp { stamp } if row.stamp != *stamp => Ok(Some(Entry::Put {
            record: Cow::Owned(view.record(at)?),
            stamp: *stamp,
        })),
        Edit::Restamp { .. } => Ok(None),
    }
}

/// The paths, records and stamps of `files`.
fn decoded<'v>(
    view: &'v View<'_>,
    files: Vec<At>,
) -> Result<Vec<(&'v str, FileRecord, FileStamp)>, StoreError> {
    files
        .into_iter()
        .map(|at| Ok((view.path(at)?, view.record(at)?, view.row(at)?.stamp)))
        .collect()
}

fn entries_of<'r>(records: &'r [(&'r str, FileRecord, FileStamp)]) -> Vec<(&'r str, Entry<'r>)> {
    records
        .iter()
        .map(|(path, record, stamp)| {
            let entry = Entry::Put {
                record: Cow::Borrowed(record),
                stamp: *stamp,
            };
            (*path, entry)
        })
        .collect()
}
