//! The byte layout Lemma's records and segments share (spec 12): a magic
//! and major version, then fields written in a fixed order as
//! little-endian `u32`s, length-prefixed byte runs, columns of `u32`s and
//! sorted string tables, closed by a BLAKE3 digest of everything before it.
//!
//! A reader checks the digest, magic and version before handing out any
//! field, and every length against the bytes left before reading it, so a
//! damaged or foreign object is an error, never a panic or a wrong answer.
//! What the fields mean is the writer's business: this module only knows
//! their shapes.

/// Bytes of the digest closing every object.
const DIGEST_BYTES: usize = 32;

/// Bytes of the header: magic, then major version.
const HEADER_BYTES: usize = 6;

/// Why an object could not be read.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum RecordError {
    /// The bytes end before a field they promise.
    #[error("object is truncated")]
    Truncated,
    /// The digest does not match the bytes: damaged or partly written.
    #[error("object digest does not match its contents")]
    DigestMismatch,
    /// The object is of another kind.
    #[error("object is not a {expected:?} object")]
    WrongKind {
        /// The magic the reader expected.
        expected: [u8; 4],
    },
    /// The object was written in a layout this build does not read.
    #[error("object version {found} is not supported (expected {expected})")]
    UnsupportedVersion {
        /// The version written.
        found: u16,
        /// The version this build reads.
        expected: u16,
    },
    /// A string is not UTF-8.
    #[error("object holds a string that is not UTF-8")]
    NotUtf8,
    /// A string table is not strictly sorted, so lookups in it would fail.
    #[error("object string table is not sorted")]
    UnsortedStrings,
    /// A value is outside what its field allows, such as an index past
    /// the end of the table it points into.
    #[error("object field {field} is out of range")]
    OutOfRange {
        /// The field, as the reader names it.
        field: &'static str,
    },
    /// Bytes are left after the last field.
    #[error("object has trailing bytes")]
    TrailingBytes,
}

/// Writes one object, field by field.
pub struct RecordWriter {
    bytes: Vec<u8>,
}

impl RecordWriter {
    /// Starts an object of the kind `magic` names, in layout `major`.
    pub fn new(magic: [u8; 4], major: u16) -> Self {
        let mut bytes = Vec::new();
        bytes.extend_from_slice(&magic);
        bytes.extend_from_slice(&major.to_le_bytes());
        Self { bytes }
    }

    /// Writes one integer.
    pub fn u32(&mut self, value: u32) {
        self.bytes.extend_from_slice(&value.to_le_bytes());
    }

    /// Writes a run of bytes, prefixed by its length.
    pub fn bytes(&mut self, value: &[u8]) {
        self.u32(length(value.len()));
        self.bytes.extend_from_slice(value);
    }

    /// Writes a column of integers, prefixed by its length.
    pub fn column(&mut self, values: &[u32]) {
        self.u32(length(values.len()));
        self.bytes.reserve(values.len() * 4);
        for value in values {
            self.bytes.extend_from_slice(&value.to_le_bytes());
        }
    }

    /// Writes a table of strings, which the caller has sorted and
    /// deduplicated: their count, where each ends, then their bytes.
    pub fn strings<S: AsRef<str>>(&mut self, sorted: &[S]) {
        self.u32(length(sorted.len()));
        let mut end = 0usize;
        for text in sorted {
            end += text.as_ref().len();
            self.u32(length(end));
        }
        for text in sorted {
            self.bytes.extend_from_slice(text.as_ref().as_bytes());
        }
    }

    /// The finished object, closed by its digest.
    pub fn finish(mut self) -> Vec<u8> {
        let digest = blake3::hash(&self.bytes);
        self.bytes.extend_from_slice(digest.as_bytes());
        self.bytes
    }
}

/// A length as written. Objects stay far below 4 GiB, so a longer one is a
/// bug in the writer; it saturates, and the reader then rejects the object
/// as truncated rather than misreading it.
fn length(value: usize) -> u32 {
    u32::try_from(value).unwrap_or(u32::MAX)
}

/// A sorted table of strings in one buffer, as read by
/// [`RecordReader::string_table`].
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct StringTable {
    text: Box<str>,
    /// Where each string ends in `text`; checked to fall on character
    /// boundaries and never to decrease.
    ends: Vec<u32>,
}

impl StringTable {
    /// How many strings there are.
    pub fn len(&self) -> usize {
        self.ends.len()
    }

    /// Whether there are none.
    pub fn is_empty(&self) -> bool {
        self.ends.is_empty()
    }

    /// The string at `index`, or `None` past the end.
    pub fn get(&self, index: usize) -> Option<&str> {
        let end = *self.ends.get(index)? as usize;
        let start = match index {
            0 => 0,
            _ => self.ends[index - 1] as usize,
        };
        self.text.get(start..end)
    }
}

/// Reads one object's fields in the order they were written.
pub struct RecordReader<'a> {
    body: &'a [u8],
    at: usize,
}

impl<'a> RecordReader<'a> {
    /// Opens `bytes` as an object of the kind `magic` names in layout
    /// `major`, after checking its digest.
    pub fn open(bytes: &'a [u8], magic: [u8; 4], major: u16) -> Result<Self, RecordError> {
        let Some(split) = bytes.len().checked_sub(DIGEST_BYTES) else {
            return Err(RecordError::Truncated);
        };
        let (body, digest) = bytes.split_at(split);
        if blake3::hash(body).as_bytes() != digest {
            return Err(RecordError::DigestMismatch);
        }
        if body.len() < HEADER_BYTES {
            return Err(RecordError::Truncated);
        }
        if body[..4] != magic {
            return Err(RecordError::WrongKind { expected: magic });
        }
        let found = u16::from_le_bytes([body[4], body[5]]);
        if found != major {
            return Err(RecordError::UnsupportedVersion {
                found,
                expected: major,
            });
        }
        Ok(Self {
            body,
            at: HEADER_BYTES,
        })
    }

    fn take(&mut self, count: usize) -> Result<&'a [u8], RecordError> {
        let end = self.at.checked_add(count).ok_or(RecordError::Truncated)?;
        let taken = self.body.get(self.at..end).ok_or(RecordError::Truncated)?;
        self.at = end;
        Ok(taken)
    }

    /// Reads one integer.
    pub fn u32(&mut self) -> Result<u32, RecordError> {
        let taken = self.take(4)?;
        Ok(u32::from_le_bytes([taken[0], taken[1], taken[2], taken[3]]))
    }

    /// Reads a count or offset, as a `usize`.
    fn count(&mut self) -> Result<usize, RecordError> {
        usize::try_from(self.u32()?).map_err(|_| RecordError::Truncated)
    }

    /// Reads a run of bytes written by [`RecordWriter::bytes`].
    pub fn bytes(&mut self) -> Result<&'a [u8], RecordError> {
        let count = self.count()?;
        self.take(count)
    }

    /// Reads a column written by [`RecordWriter::column`].
    pub fn column(&mut self) -> Result<Vec<u32>, RecordError> {
        let count = self.count()?;
        let byte_count = count.checked_mul(4).ok_or(RecordError::Truncated)?;
        let taken = self.take(byte_count)?;
        Ok(taken
            .chunks_exact(4)
            .map(|word| u32::from_le_bytes([word[0], word[1], word[2], word[3]]))
            .collect())
    }

    /// Reads a table written by [`RecordWriter::strings`], checking it is
    /// UTF-8 and strictly sorted.
    pub fn strings(&mut self) -> Result<Vec<&'a str>, RecordError> {
        let count = self.count()?;
        let mut ends = Vec::with_capacity(count.min(self.body.len() / 4));
        for _ in 0..count {
            ends.push(self.count()?);
        }
        let total = ends.last().copied().unwrap_or(0);
        let text = std::str::from_utf8(self.take(total)?).map_err(|_| RecordError::NotUtf8)?;
        let mut strings = Vec::with_capacity(count);
        let mut start = 0;
        for end in ends {
            let item = text.get(start..end).ok_or(RecordError::NotUtf8)?;
            if strings
                .last()
                .is_some_and(|previous: &&str| *previous >= item)
            {
                return Err(RecordError::UnsortedStrings);
            }
            strings.push(item);
            start = end;
        }
        Ok(strings)
    }

    /// Reads a table written by [`RecordWriter::strings`] into one owned
    /// buffer, checked as [`RecordReader::strings`] checks it: for a reader
    /// that keeps the table after the object's bytes are gone, at two
    /// allocations rather than one per string.
    pub fn string_table(&mut self) -> Result<StringTable, RecordError> {
        let count = self.count()?;
        let mut ends = Vec::with_capacity(count.min(self.body.len() / 4));
        for _ in 0..count {
            ends.push(self.u32()?);
        }
        let total = ends.last().map_or(0, |end| *end as usize);
        let text = std::str::from_utf8(self.take(total)?).map_err(|_| RecordError::NotUtf8)?;
        let mut start = 0;
        let mut previous: Option<&str> = None;
        for end in &ends {
            let item = text.get(start..*end as usize).ok_or(RecordError::NotUtf8)?;
            if previous.is_some_and(|previous| previous >= item) {
                return Err(RecordError::UnsortedStrings);
            }
            previous = Some(item);
            start = *end as usize;
        }
        Ok(StringTable {
            text: text.into(),
            ends,
        })
    }

    /// Checks every byte was read.
    pub fn finish(self) -> Result<(), RecordError> {
        if self.at != self.body.len() {
            return Err(RecordError::TrailingBytes);
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const MAGIC: [u8; 4] = *b"TEST";

    fn sample() -> Vec<u8> {
        let mut writer = RecordWriter::new(MAGIC, 1);
        writer.u32(7);
        writer.bytes(b"raw");
        writer.column(&[1, 2, 3]);
        writer.strings(&["alpha", "beta"]);
        writer.finish()
    }

    #[test]
    fn fields_read_back_in_the_order_written() {
        let bytes = sample();

        let mut reader = RecordReader::open(&bytes, MAGIC, 1).unwrap();

        assert_eq!(reader.u32().unwrap(), 7);
        assert_eq!(reader.bytes().unwrap(), b"raw");
        assert_eq!(reader.column().unwrap(), [1, 2, 3]);
        assert_eq!(reader.strings().unwrap(), ["alpha", "beta"]);
        assert_eq!(reader.finish(), Ok(()));
    }

    #[test]
    fn an_owned_string_table_holds_the_strings_written() {
        let mut writer = RecordWriter::new(MAGIC, 1);
        writer.strings(&["", "alpha", "beta"]);
        let bytes = writer.finish();

        let table = RecordReader::open(&bytes, MAGIC, 1)
            .unwrap()
            .string_table()
            .unwrap();

        assert_eq!(table.len(), 3);
        assert_eq!(
            (table.get(0), table.get(2), table.get(3)),
            (Some(""), Some("beta"), None)
        );
    }

    #[test]
    fn a_changed_byte_fails_the_digest() {
        let mut bytes = sample();
        bytes[8] ^= 1;

        let opened = RecordReader::open(&bytes, MAGIC, 1);

        assert_eq!(opened.err(), Some(RecordError::DigestMismatch));
    }

    #[test]
    fn another_kind_or_version_is_rejected() {
        let bytes = sample();

        let kind = RecordReader::open(&bytes, *b"ELSE", 1).err();
        let version = RecordReader::open(&bytes, MAGIC, 2).err();

        assert_eq!(kind, Some(RecordError::WrongKind { expected: *b"ELSE" }));
        assert_eq!(
            version,
            Some(RecordError::UnsupportedVersion {
                found: 1,
                expected: 2
            })
        );
    }

    #[test]
    fn a_length_past_the_end_is_truncation_not_a_panic() {
        let mut writer = RecordWriter::new(MAGIC, 1);
        writer.u32(1_000_000);
        let bytes = writer.finish();

        let mut reader = RecordReader::open(&bytes, MAGIC, 1).unwrap();

        assert_eq!(reader.column().err(), Some(RecordError::Truncated));
    }

    #[test]
    fn an_unsorted_string_table_is_rejected() {
        let mut writer = RecordWriter::new(MAGIC, 1);
        writer.strings(&["b", "a"]);
        let bytes = writer.finish();

        let mut reader = RecordReader::open(&bytes, MAGIC, 1).unwrap();

        assert_eq!(reader.strings().err(), Some(RecordError::UnsortedStrings));
    }
}
