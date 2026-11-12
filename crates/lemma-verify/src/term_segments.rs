//! A repository's index terms as one segment (spec 12): for each term,
//! which of the repository's consumer files may make a call it names. A
//! run choosing consumer files in an unchanged repository reads this one
//! object instead of asking the unit cache about every file.
//!
//! Files are named by their position in the order the segment's inputs
//! list them, which the caller lists again to turn positions into paths.

use std::collections::{BTreeMap, BTreeSet};
use std::path::Path;

use lemma_store::{RecordError, RecordReader, RecordWriter, SegmentKey};

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

/// What the latest terms segment of a group was built from: its inputs,
/// and its files' unit keys in its order, so a run whose keys differ by a
/// few files can take the others' terms from it.
const LATEST_MAGIC: [u8; 4] = *b"LMTL";
const LATEST_VERSION: u16 = 1;

/// The name the latest terms segment of one selection is recorded under:
/// its group, and what tells the repository's selections apart (`selection`,
/// such as the extraction options and whether a commit or the working
/// directory is read). Selections of one repository share a group, and one
/// pointer for them all would keep only the last one's.
pub(crate) fn latest_name(group: &str, selection: &str) -> String {
    let selection = lemma_model::content_hash(selection.as_bytes());
    format!("{INPUTS}\0latest\0{group}\0{selection}")
}

/// A record of the segment with `inputs` built from `keys`, in order.
pub(crate) fn encode_latest(inputs: &str, keys: &[String]) -> Vec<u8> {
    // String tables are sorted, so the keys go in sorted, with each
    // position's index into them.
    let mut sorted: Vec<&str> = keys.iter().map(String::as_str).collect();
    sorted.sort_unstable();
    sorted.dedup();
    let positions: Vec<u32> = keys
        .iter()
        .map(|key| {
            let index = sorted.binary_search(&key.as_str()).unwrap_or(0);
            u32::try_from(index).unwrap_or(u32::MAX)
        })
        .collect();
    let mut writer = RecordWriter::new(LATEST_MAGIC, LATEST_VERSION);
    writer.strings(&[inputs]);
    writer.strings(&sorted);
    writer.column(&positions);
    writer.finish()
}

/// The inputs and the keys, in order, of a record [`encode_latest`] wrote.
pub(crate) fn decode_latest(bytes: &[u8]) -> Result<(String, Vec<String>), RecordError> {
    let mut reader = RecordReader::open(bytes, LATEST_MAGIC, LATEST_VERSION)?;
    let inputs = reader.strings()?;
    let sorted = reader.strings()?;
    let positions = reader.column()?;
    reader.finish()?;
    let inputs = inputs
        .first()
        .ok_or(RecordError::OutOfRange { field: "inputs" })?;
    let mut keys = Vec::with_capacity(positions.len());
    for position in positions {
        let key = sorted
            .get(position as usize)
            .ok_or(RecordError::OutOfRange { field: "keys" })?;
        keys.push((*key).to_owned());
    }
    Ok(((*inputs).to_owned(), keys))
}

/// Each file's terms, by position, from a segment of `file_count` files.
pub(crate) fn file_terms(bytes: &[u8], file_count: usize) -> Result<Vec<Vec<String>>, RecordError> {
    let mut reader = RecordReader::open(bytes, MAGIC, VERSION)?;
    let count = reader.u32()? as usize;
    let terms = reader.strings()?;
    let ends = reader.column()?;
    let files = reader.column()?;
    reader.finish()?;
    if count != file_count || ends.len() != terms.len() {
        return Err(RecordError::OutOfRange { field: "files" });
    }
    let mut by_file = vec![Vec::new(); count];
    let mut start = 0usize;
    for (term, end) in terms.iter().zip(&ends) {
        let end = *end as usize;
        let listed = files
            .get(start..end)
            .ok_or(RecordError::OutOfRange { field: "ends" })?;
        for file in listed {
            let terms_of_file = by_file
                .get_mut(*file as usize)
                .ok_or(RecordError::OutOfRange { field: "files" })?;
            terms_of_file.push((*term).to_owned());
        }
        start = end;
    }
    Ok(by_file)
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
    fn a_segment_gives_back_each_files_terms() {
        let first = terms(&["name:pay", "path:/a"]);
        let second = terms(&[]);
        let third = terms(&["path:/a"]);
        let bytes = encode(&[&first, &second, &third]);

        let found = file_terms(&bytes, 3).unwrap();

        assert_eq!(found, vec![first, second, third]);
    }

    #[test]
    fn the_latest_record_keeps_keys_in_their_order() {
        let keys = terms(&["key-b", "key-a", "key-c"]);

        let decoded = decode_latest(&encode_latest("inputs", &keys)).unwrap();

        assert_eq!(decoded, ("inputs".to_owned(), keys));
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
