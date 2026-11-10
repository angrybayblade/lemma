//! A repository's index terms as one segment (spec 12): for each term,
//! which of the repository's consumer files may make a call it names. A
//! run choosing consumer files in an unchanged repository reads this one
//! object instead of asking the unit cache about every file.
//!
//! Files are named by their position in the order the segment's inputs
//! list them, which the caller lists again to turn positions into paths.

use std::collections::{BTreeMap, BTreeSet};
use std::path::Path;

use lemma_legacy_store::{RecordError, RecordReader, RecordWriter, SegmentKey};

const MAGIC: [u8; 4] = *b"LMTS";
const VERSION: u16 = 1;

/// Names what a terms segment holds, so a change of what goes into one
/// never reads an old one.
const INPUTS: &str = "terms segment 1";

/// The segment of the files whose unit keys are `keys`, in that order, in
/// `repository`. Each key names the build, the path, the extraction
/// options and the contents, so the keys alone say what the terms are.
pub(crate) fn key(build: &str, repository: &Path, keys: &[String]) -> SegmentKey {
    let group = format!("{build}\0terms\0{}", repository.display());
    let mut inputs = String::from(INPUTS);
    for key in keys {
        inputs.push('\0');
        inputs.push_str(key);
    }
    SegmentKey {
        group: lemma_model::content_hash(group.as_bytes())[..16].to_owned(),
        inputs: lemma_model::content_hash(inputs.as_bytes()),
    }
}

/// The segment of files whose terms are `file_terms`, in order.
pub(crate) fn encode(file_terms: &[&[String]]) -> Vec<u8> {
    let mut files_by_term: BTreeMap<&str, Vec<u32>> = BTreeMap::new();
    for (file, terms) in file_terms.iter().enumerate() {
        let file = u32::try_from(file).unwrap_or(u32::MAX);
        for term in *terms {
            files_by_term.entry(term).or_default().push(file);
        }
    }
    let mut ends = Vec::with_capacity(files_by_term.len());
    let mut files = Vec::new();
    for listed in files_by_term.values_mut() {
        listed.dedup();
        files.extend_from_slice(listed);
        ends.push(u32::try_from(files.len()).unwrap_or(u32::MAX));
    }
    let terms: Vec<&str> = files_by_term.into_keys().collect();
    let mut writer = RecordWriter::new(MAGIC, VERSION);
    writer.u32(u32::try_from(file_terms.len()).unwrap_or(u32::MAX));
    writer.strings(&terms);
    writer.column(&ends);
    writer.column(&files);
    writer.finish()
}

/// The positions of the files any of `wanted` indexes, from a segment of
/// `file_count` files; an error for a segment that does not check or was
/// made for another number of files.
pub(crate) fn files_with(
    bytes: &[u8],
    wanted: &BTreeSet<String>,
    file_count: usize,
) -> Result<BTreeSet<usize>, RecordError> {
    let mut reader = RecordReader::open(bytes, MAGIC, VERSION)?;
    let count = reader.u32()? as usize;
    let terms = reader.strings()?;
    let ends = reader.column()?;
    let files = reader.column()?;
    reader.finish()?;
    if count != file_count || ends.len() != terms.len() {
        return Err(RecordError::OutOfRange { field: "files" });
    }
    if ends.last().map_or(0, |end| *end as usize) != files.len() {
        return Err(RecordError::OutOfRange { field: "ends" });
    }
    let mut found = BTreeSet::new();
    for term in wanted {
        let Ok(position) = terms.binary_search(&term.as_str()) else {
            continue;
        };
        let start = if position == 0 {
            0
        } else {
            ends[position - 1] as usize
        };
        let listed = files
            .get(start..ends[position] as usize)
            .ok_or(RecordError::OutOfRange { field: "ends" })?;
        for file in listed {
            let file = *file as usize;
            if file >= count {
                return Err(RecordError::OutOfRange { field: "files" });
            }
            found.insert(file);
        }
    }
    Ok(found)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn terms(texts: &[&str]) -> Vec<String> {
        texts.iter().map(|text| (*text).to_owned()).collect()
    }

    #[test]
    fn files_are_found_by_any_wanted_term() {
        let first = terms(&["path:/a", "path:/b"]);
        let second = terms(&[]);
        let third = terms(&["path:/b"]);
        let bytes = encode(&[&first, &second, &third]);
        let wanted = BTreeSet::from(["path:/b".to_owned(), "path:/z".to_owned()]);

        let found = files_with(&bytes, &wanted, 3).unwrap();

        assert_eq!(found, BTreeSet::from([0, 2]));
    }

    #[test]
    fn a_segment_for_another_number_of_files_is_rejected() {
        let bytes = encode(&[&terms(&["path:/a"])]);

        let found = files_with(&bytes, &BTreeSet::new(), 2);

        assert_eq!(
            found.err(),
            Some(RecordError::OutOfRange { field: "files" })
        );
    }
}
