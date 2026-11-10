//! Segments (spec 17): immutable tables of files and their symbols, built
//! in one sorted pass. A full segment holds every file of a root; a delta
//! segment holds only the files one commit changed, and a newer segment
//! replaces those whole files in older ones. Every offset inside a segment
//! is relative to its start, so a segment can be copied to another file
//! unchanged.
//!
//! A segment is a header of counts and a section table, then the sections:
//! sorted paths, file rows, a sorted string table, symbol rows, each
//! symbol's mentions, a sorted id table, and three lists from a string to
//! the symbols that bear it as a name, as a qualified name, or mention it.

use std::borrow::Cow;
use std::cmp::Ordering;
use std::collections::{HashMap, HashSet};
use std::ops::Range;

use lemma_model::{Language, Span, SymbolKind};

use crate::codec::{self, Decoder, Encoder};
use crate::ids::SymbolId;
use crate::{Digest, FileRecord, FileStamp, StoreError, SymbolRecord};

const PATH_OFFSETS: usize = 0;
const PATH_BYTES: usize = 1;
const FILES: usize = 2;
const STRING_OFFSETS: usize = 3;
const STRING_BYTES: usize = 4;
const SYMBOLS: usize = 5;
const MENTIONS: usize = 6;
const IDS: usize = 7;
const BY_NAME: usize = 8;
const BY_NAME_ROWS: usize = 9;
const BY_QUALIFIED: usize = 10;
const BY_QUALIFIED_ROWS: usize = 11;
const MENTIONED_BY: usize = 12;
const MENTIONED_BY_ROWS: usize = 13;
const SECTIONS: usize = 14;
const HEADER_BYTES: usize = 16 + SECTIONS * 8;

/// Bytes of a file row: blob, language, flags, stamp, first symbol, count.
const FILE_ROW: usize = 32 + 1 + 1 + 32 + 4 + 4;
/// Bytes of a symbol row; see [`Segment::symbol`] for its columns.
const SYMBOL_ROW: usize = 4 + 4 + 1 + 16 + 16 + 4 + 32 + 16 + 4 + 4;
const SYMBOL_FILE_AT: usize = 93;
const SYMBOL_MENTIONS_AT: usize = 97;
const ID_ROW: usize = 16 + 4;

const SYNTAX_ERRORS: u8 = 1;
const REMOVED: u8 = 2;

const DAMAGED: StoreError = StoreError::Damaged {
    what: "a segment is damaged",
};
const TOO_LARGE: StoreError = StoreError::Damaged {
    what: "a segment would be larger than 4 GiB",
};

/// What a segment holds for one path.
pub(crate) enum Entry<'a> {
    /// The file and its metadata.
    Put {
        record: Cow<'a, FileRecord>,
        stamp: FileStamp,
    },
    /// The file is gone.
    Removed,
}

/// A file's row.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct FileRow {
    pub blob: Digest,
    pub language: Language,
    pub has_syntax_errors: bool,
    pub is_removed: bool,
    pub stamp: FileStamp,
    /// Its symbols' positions in the segment.
    pub symbols: Range<usize>,
}

/// A symbol's row, its strings borrowed from the segment.
pub(crate) struct SymbolRow<'s> {
    pub name: &'s str,
    pub qualified_name: &'s str,
    pub kind: SymbolKind,
    pub span: Span,
    pub name_span: Span,
    pub discriminator: u32,
    pub digest: Digest,
    pub id: SymbolId,
    pub file: usize,
    /// Its mentions' positions in the segment.
    pub mentions: Range<usize>,
}

fn small(value: usize) -> Result<u32, StoreError> {
    u32::try_from(value).map_err(|_| TOO_LARGE)
}

/// Columns gathered while files are written, for the tables built after.
#[derive(Default)]
struct Tables {
    sections: [Vec<u8>; SECTIONS],
    symbols: usize,
    names: Vec<u32>,
    qualified: Vec<u32>,
    mentions: Vec<u32>,
    mention_owners: Vec<u32>,
    ids: Vec<(SymbolId, u32)>,
}

/// A segment holding `files`, which are sorted by path with no path twice.
pub(crate) fn build(files: &[(&str, Entry<'_>)]) -> Result<Vec<u8>, StoreError> {
    let strings = string_table(files);
    let sids: HashMap<&str, u32> = strings
        .iter()
        .enumerate()
        .map(|(sid, text)| Ok((*text, small(sid)?)))
        .collect::<Result<_, StoreError>>()?;
    let mut tables = Tables::default();
    write_paths(files, &mut tables.sections)?;
    for (file, (path, entry)) in files.iter().enumerate() {
        write_file((small(file)?, path), entry, &sids, &mut tables)?;
    }
    write_strings(&strings, &mut tables.sections)?;
    tables.ids.sort_unstable();
    for (id, symbol) in &tables.ids {
        tables.sections[IDS].extend_from_slice(&id.to_bytes());
        tables.sections[IDS].extend_from_slice(&symbol.to_le_bytes());
    }
    let owners: Vec<u32> = (0..small(tables.symbols)?).collect();
    let lists = [
        (BY_NAME, BY_NAME_ROWS, &tables.names, &owners),
        (BY_QUALIFIED, BY_QUALIFIED_ROWS, &tables.qualified, &owners),
        (
            MENTIONED_BY,
            MENTIONED_BY_ROWS,
            &tables.mentions,
            &tables.mention_owners,
        ),
    ];
    for (offsets, rows, keys, owners) in lists {
        let (starts, owned) = reverse(strings.len(), keys, owners);
        tables.sections[offsets] = starts;
        tables.sections[rows] = owned;
    }
    let counts = [
        files.len(),
        tables.symbols,
        strings.len(),
        tables.mentions.len(),
    ];
    assemble(counts, &tables.sections)
}

/// Every name, qualified name and mention in `files`, sorted, once each.
fn string_table<'a>(files: &'a [(&str, Entry<'_>)]) -> Vec<&'a str> {
    let mut seen: HashSet<&str> = HashSet::new();
    for (_, entry) in files {
        let Entry::Put { record, .. } = entry else {
            continue;
        };
        for symbol in &record.symbols {
            seen.insert(&symbol.name);
            seen.insert(&symbol.qualified_name);
            seen.extend(symbol.mentions.iter().map(String::as_str));
        }
    }
    let mut strings: Vec<&str> = seen.into_iter().collect();
    strings.sort_unstable();
    strings
}

fn write_paths(files: &[(&str, Entry<'_>)], sections: &mut [Vec<u8>]) -> Result<(), StoreError> {
    let mut length = 0;
    for (path, _) in files {
        sections[PATH_OFFSETS].extend_from_slice(&small(length)?.to_le_bytes());
        sections[PATH_BYTES].extend_from_slice(path.as_bytes());
        length += path.len();
    }
    sections[PATH_OFFSETS].extend_from_slice(&small(length)?.to_le_bytes());
    Ok(())
}

fn write_strings(strings: &[&str], sections: &mut [Vec<u8>]) -> Result<(), StoreError> {
    let mut length = 0;
    for text in strings {
        sections[STRING_OFFSETS].extend_from_slice(&small(length)?.to_le_bytes());
        sections[STRING_BYTES].extend_from_slice(text.as_bytes());
        length += text.len();
    }
    sections[STRING_OFFSETS].extend_from_slice(&small(length)?.to_le_bytes());
    Ok(())
}

/// Writes one file's row, and its symbols' rows; `file` is its position
/// and path.
fn write_file(
    (file, path): (u32, &str),
    entry: &Entry<'_>,
    sids: &HashMap<&str, u32>,
    tables: &mut Tables,
) -> Result<(), StoreError> {
    let mut row = Encoder::default();
    let Entry::Put { record, stamp } = entry else {
        row.digest(&[0; 32]);
        row.u8(0);
        row.u8(REMOVED);
        write_stamp(&mut row, FileStamp::default());
        row.u32(small(tables.symbols)?);
        row.u32(0);
        tables.sections[FILES].extend_from_slice(&row.bytes);
        return Ok(());
    };
    row.digest(&record.blob);
    row.u8(codec::language_code(record.language));
    row.u8(if record.has_syntax_errors {
        SYNTAX_ERRORS
    } else {
        0
    });
    write_stamp(&mut row, *stamp);
    row.u32(small(tables.symbols)?);
    row.u32(small(record.symbols.len())?);
    tables.sections[FILES].extend_from_slice(&row.bytes);
    for symbol in &record.symbols {
        write_symbol(path, file, symbol, sids, tables)?;
    }
    Ok(())
}

fn write_stamp(row: &mut Encoder, stamp: FileStamp) {
    row.u64(stamp.size_bytes);
    row.i64(stamp.modified_ns);
    row.i64(stamp.changed_ns);
    row.u64(stamp.inode);
}

fn write_symbol(
    path: &str,
    file: u32,
    symbol: &SymbolRecord,
    sids: &HashMap<&str, u32>,
    tables: &mut Tables,
) -> Result<(), StoreError> {
    let sid = |text: &str| sids.get(text).copied().ok_or(DAMAGED);
    let ordinal = small(tables.symbols)?;
    let id = SymbolId::of(
        path,
        &symbol.qualified_name,
        symbol.kind,
        symbol.discriminator,
    );
    let mut row = Encoder::default();
    row.u32(sid(&symbol.name)?);
    row.u32(sid(&symbol.qualified_name)?);
    row.u8(codec::kind_code(symbol.kind));
    row.span(symbol.span);
    row.span(symbol.name_span);
    row.u32(symbol.discriminator);
    row.digest(&symbol.digest);
    row.bytes.extend_from_slice(&id.to_bytes());
    row.u32(file);
    row.u32(small(tables.mentions.len())?);
    tables.sections[SYMBOLS].extend_from_slice(&row.bytes);
    tables.names.push(sid(&symbol.name)?);
    tables.qualified.push(sid(&symbol.qualified_name)?);
    for mention in &symbol.mentions {
        let mention = sid(mention)?;
        tables.sections[MENTIONS].extend_from_slice(&mention.to_le_bytes());
        tables.mentions.push(mention);
        tables.mention_owners.push(ordinal);
    }
    tables.ids.push((id, ordinal));
    tables.symbols += 1;
    Ok(())
}

/// A list from each string to its owners, in owner order: offsets into the
/// rows, then the rows. `keys[i]` is the string of `owners[i]`.
fn reverse(strings: usize, keys: &[u32], owners: &[u32]) -> (Vec<u8>, Vec<u8>) {
    let mut starts = vec![0u32; strings + 1];
    for key in keys {
        starts[*key as usize + 1] += 1;
    }
    for at in 1..starts.len() {
        starts[at] += starts[at - 1];
    }
    let mut next = starts.clone();
    let mut rows = vec![0u32; keys.len()];
    for (key, owner) in keys.iter().zip(owners) {
        let slot = &mut next[*key as usize];
        rows[*slot as usize] = *owner;
        *slot += 1;
    }
    let encode = |values: &[u32]| {
        values
            .iter()
            .flat_map(|value| value.to_le_bytes())
            .collect()
    };
    (encode(&starts), encode(&rows))
}

fn assemble(counts: [usize; 4], sections: &[Vec<u8>; SECTIONS]) -> Result<Vec<u8>, StoreError> {
    let total = HEADER_BYTES + sections.iter().map(Vec::len).sum::<usize>();
    small(total)?;
    let mut encoder = Encoder {
        bytes: Vec::with_capacity(total),
    };
    for count in counts {
        encoder.u32(small(count)?);
    }
    let mut at = HEADER_BYTES;
    for section in sections {
        encoder.u32(small(at)?);
        encoder.u32(small(section.len())?);
        at += section.len();
    }
    for section in sections {
        encoder.bytes.extend_from_slice(section);
    }
    Ok(encoder.bytes)
}

/// A segment read in place.
pub(crate) struct Segment<'a> {
    bytes: Cow<'a, [u8]>,
    files: usize,
    symbols: usize,
    strings: usize,
    mentions: usize,
    sections: [Range<usize>; SECTIONS],
}

fn u32_at(bytes: &[u8], at: usize) -> Result<u32, StoreError> {
    let slice = at
        .checked_add(4)
        .and_then(|end| bytes.get(at..end))
        .ok_or(DAMAGED)?;
    Ok(u32::from_le_bytes([slice[0], slice[1], slice[2], slice[3]]))
}

fn utf8(bytes: &[u8]) -> Result<&str, StoreError> {
    std::str::from_utf8(bytes).map_err(|_| StoreError::Damaged {
        what: "segment text is not UTF-8",
    })
}

/// The position `compare` finds equal among `count` sorted rows.
fn search(
    count: usize,
    mut compare: impl FnMut(usize) -> Result<Ordering, StoreError>,
) -> Result<Option<usize>, StoreError> {
    let (mut low, mut high) = (0, count);
    while low < high {
        let middle = low + (high - low) / 2;
        match compare(middle)? {
            Ordering::Equal => return Ok(Some(middle)),
            Ordering::Less => low = middle + 1,
            Ordering::Greater => high = middle,
        }
    }
    Ok(None)
}

impl<'a> Segment<'a> {
    /// Reads a segment's header, checking every section against the bytes.
    pub(crate) fn parse(bytes: Cow<'a, [u8]>) -> Result<Self, StoreError> {
        let mut decoder = Decoder::new(&bytes);
        let mut counts = [0usize; 4];
        for count in &mut counts {
            *count = decoder.u32()? as usize;
        }
        let mut sections: [Range<usize>; SECTIONS] = std::array::from_fn(|_| 0..0);
        for section in &mut sections {
            let start = decoder.u32()? as usize;
            let end = start.checked_add(decoder.u32()? as usize).ok_or(DAMAGED)?;
            if start < HEADER_BYTES || end > bytes.len() {
                return Err(DAMAGED);
            }
            *section = start..end;
        }
        let [files, symbols, strings, mentions] = counts;
        let segment = Self {
            bytes,
            files,
            symbols,
            strings,
            mentions,
            sections,
        };
        let expected = segment.expected_lengths().ok_or(DAMAGED)?;
        let lengths = segment.sections.iter().map(ExactSizeIterator::len);
        if !lengths.eq(expected) {
            return Err(DAMAGED);
        }
        Ok(segment)
    }

    fn expected_lengths(&self) -> Option<[usize; SECTIONS]> {
        let paths = self.files.checked_add(1)?.checked_mul(4)?;
        let strings = self.strings.checked_add(1)?.checked_mul(4)?;
        let per_symbol = self.symbols.checked_mul(4)?;
        let per_mention = self.mentions.checked_mul(4)?;
        Some([
            paths,
            self.sections[PATH_BYTES].len(),
            self.files.checked_mul(FILE_ROW)?,
            strings,
            self.sections[STRING_BYTES].len(),
            self.symbols.checked_mul(SYMBOL_ROW)?,
            per_mention,
            self.symbols.checked_mul(ID_ROW)?,
            strings,
            per_symbol,
            strings,
            per_symbol,
            strings,
            per_mention,
        ])
    }

    fn section(&self, index: usize) -> &[u8] {
        // Every range was checked against the bytes by `parse`.
        self.bytes.get(self.sections[index].clone()).unwrap_or(&[])
    }

    /// The range a table of offsets gives its `index`th item, checked to
    /// lie within `limit`.
    fn range_in(
        &self,
        offsets: usize,
        index: usize,
        limit: usize,
    ) -> Result<Range<usize>, StoreError> {
        let table = self.section(offsets);
        let start = u32_at(table, index * 4)? as usize;
        let end = u32_at(table, index * 4 + 4)? as usize;
        if start > end || end > limit {
            return Err(DAMAGED);
        }
        Ok(start..end)
    }

    pub(crate) fn file_count(&self) -> usize {
        self.files
    }

    fn path_bytes(&self, file: usize) -> Result<&[u8], StoreError> {
        let bytes = self.section(PATH_BYTES);
        let range = self.range_in(PATH_OFFSETS, file, bytes.len())?;
        Ok(&bytes[range])
    }

    pub(crate) fn path(&self, file: usize) -> Result<&str, StoreError> {
        utf8(self.path_bytes(file)?)
    }

    /// The position of `path`'s row, if the segment has one.
    pub(crate) fn find_path(&self, path: &str) -> Result<Option<usize>, StoreError> {
        search(self.files, |file| {
            Ok(self.path_bytes(file)?.cmp(path.as_bytes()))
        })
    }

    pub(crate) fn file(&self, file: usize) -> Result<FileRow, StoreError> {
        let start = file.checked_mul(FILE_ROW).ok_or(DAMAGED)?;
        let row = self
            .section(FILES)
            .get(start..start + FILE_ROW)
            .ok_or(DAMAGED)?;
        let mut decoder = Decoder::new(row);
        let blob = decoder.digest()?;
        let language = codec::language_of(decoder.u8()?)?;
        let flags = decoder.u8()?;
        let stamp = FileStamp {
            size_bytes: decoder.u64()?,
            modified_ns: decoder.i64()?,
            changed_ns: decoder.i64()?,
            inode: decoder.u64()?,
        };
        let first = decoder.u32()? as usize;
        let end = first
            .checked_add(decoder.u32()? as usize)
            .filter(|end| *end <= self.symbols)
            .ok_or(DAMAGED)?;
        Ok(FileRow {
            blob,
            language,
            has_syntax_errors: flags & SYNTAX_ERRORS != 0,
            is_removed: flags & REMOVED != 0,
            stamp,
            symbols: first..end,
        })
    }

    fn string_bytes(&self, sid: usize) -> Result<&[u8], StoreError> {
        let bytes = self.section(STRING_BYTES);
        let range = self.range_in(STRING_OFFSETS, sid, bytes.len())?;
        Ok(&bytes[range])
    }

    pub(crate) fn string(&self, sid: usize) -> Result<&str, StoreError> {
        if sid >= self.strings {
            return Err(DAMAGED);
        }
        utf8(self.string_bytes(sid)?)
    }

    /// The string id of `text`, if the segment holds it.
    pub(crate) fn find_string(&self, text: &str) -> Result<Option<usize>, StoreError> {
        search(self.strings, |sid| {
            Ok(self.string_bytes(sid)?.cmp(text.as_bytes()))
        })
    }

    /// The ids of strings holding `needle`, ignoring ASCII case.
    pub(crate) fn strings_containing(&self, needle: &str) -> Result<Vec<usize>, StoreError> {
        let needle = needle.as_bytes();
        let mut found = Vec::new();
        for sid in 0..self.strings {
            let text = self.string_bytes(sid)?;
            let holds = needle.is_empty()
                || text
                    .windows(needle.len())
                    .any(|window| window.eq_ignore_ascii_case(needle));
            if holds {
                found.push(sid);
            }
        }
        Ok(found)
    }

    fn symbol_row(&self, symbol: usize) -> Result<&[u8], StoreError> {
        let start = symbol.checked_mul(SYMBOL_ROW).ok_or(DAMAGED)?;
        self.section(SYMBOLS)
            .get(start..start + SYMBOL_ROW)
            .ok_or(DAMAGED)
    }

    /// A symbol's row: name and qualified name as string ids, kind, span,
    /// name span, discriminator, digest, id, file and first mention.
    pub(crate) fn symbol(&self, symbol: usize) -> Result<SymbolRow<'_>, StoreError> {
        let mut decoder = Decoder::new(self.symbol_row(symbol)?);
        let name = self.string(decoder.u32()? as usize)?;
        let qualified_name = self.string(decoder.u32()? as usize)?;
        let kind = codec::kind_of(decoder.u8()?)?;
        let span = decoder.span()?;
        let name_span = decoder.span()?;
        let discriminator = decoder.u32()?;
        let digest = decoder.digest()?;
        let id = SymbolId::from_bytes(decoder.bytes16()?);
        let file = decoder.u32()? as usize;
        let first = decoder.u32()? as usize;
        let end = if symbol + 1 < self.symbols {
            u32_at(self.symbol_row(symbol + 1)?, SYMBOL_MENTIONS_AT)? as usize
        } else {
            self.mentions
        };
        if file >= self.files || first > end || end > self.mentions {
            return Err(DAMAGED);
        }
        Ok(SymbolRow {
            name,
            qualified_name,
            kind,
            span,
            name_span,
            discriminator,
            digest,
            id,
            file,
            mentions: first..end,
        })
    }

    /// The file a symbol belongs to, without decoding the rest of it.
    pub(crate) fn symbol_file(&self, symbol: usize) -> Result<usize, StoreError> {
        Ok(u32_at(self.symbol_row(symbol)?, SYMBOL_FILE_AT)? as usize)
    }

    /// The string ids a symbol mentions, from its row's range.
    pub(crate) fn mentions(&self, range: Range<usize>) -> Result<Vec<usize>, StoreError> {
        let bytes = self.section(MENTIONS);
        range
            .map(|at| u32_at(bytes, at * 4).map(|sid| sid as usize))
            .collect()
    }

    /// The position of the symbol with `id`, if the segment has it.
    pub(crate) fn find_id(&self, id: SymbolId) -> Result<Option<usize>, StoreError> {
        let rows = self.section(IDS);
        let wanted = id.to_bytes();
        let found = search(self.symbols, |row| {
            let start = row * ID_ROW;
            let stored = rows.get(start..start + 16).ok_or(DAMAGED)?;
            Ok(stored.cmp(wanted.as_slice()))
        })?;
        let Some(row) = found else {
            return Ok(None);
        };
        Ok(Some(u32_at(rows, row * ID_ROW + 16)? as usize))
    }

    /// The symbols whose ids lie between `lowest` and `highest`, inclusive.
    pub(crate) fn ids_between(
        &self,
        lowest: [u8; 16],
        highest: [u8; 16],
    ) -> Result<Vec<usize>, StoreError> {
        let rows = self.section(IDS);
        let id_at = |row: usize| -> Result<&[u8], StoreError> {
            let start = row * ID_ROW;
            rows.get(start..start + 16).ok_or(DAMAGED)
        };
        let (mut low, mut high) = (0, self.symbols);
        while low < high {
            let middle = low + (high - low) / 2;
            if id_at(middle)? < lowest.as_slice() {
                low = middle + 1;
            } else {
                high = middle;
            }
        }
        let mut found = Vec::new();
        let mut row = low;
        while row < self.symbols && id_at(row)? <= highest.as_slice() {
            found.push(u32_at(rows, row * ID_ROW + 16)? as usize);
            row += 1;
        }
        Ok(found)
    }

    fn listed(
        &self,
        (offsets, rows): (usize, usize),
        sid: usize,
    ) -> Result<Vec<usize>, StoreError> {
        if sid >= self.strings {
            return Err(DAMAGED);
        }
        let rows = self.section(rows);
        let range = self.range_in(offsets, sid, rows.len() / 4)?;
        range
            .map(|at| u32_at(rows, at * 4).map(|symbol| symbol as usize))
            .collect()
    }

    /// The symbols whose name is string `sid`.
    pub(crate) fn named(&self, sid: usize) -> Result<Vec<usize>, StoreError> {
        self.listed((BY_NAME, BY_NAME_ROWS), sid)
    }

    /// The symbols whose qualified name is string `sid`.
    pub(crate) fn qualified_as(&self, sid: usize) -> Result<Vec<usize>, StoreError> {
        self.listed((BY_QUALIFIED, BY_QUALIFIED_ROWS), sid)
    }

    /// The symbols whose code mentions string `sid`.
    pub(crate) fn mentioned_by(&self, sid: usize) -> Result<Vec<usize>, StoreError> {
        self.listed((MENTIONED_BY, MENTIONED_BY_ROWS), sid)
    }

    /// A file's whole record.
    pub(crate) fn record(&self, file: usize) -> Result<FileRecord, StoreError> {
        let row = self.file(file)?;
        let symbols = row
            .symbols
            .clone()
            .map(|symbol| self.symbol_record(symbol))
            .collect::<Result<_, _>>()?;
        Ok(FileRecord {
            blob: row.blob,
            language: row.language,
            has_syntax_errors: row.has_syntax_errors,
            symbols,
        })
    }

    fn symbol_record(&self, symbol: usize) -> Result<SymbolRecord, StoreError> {
        let row = self.symbol(symbol)?;
        let mentions = self
            .mentions(row.mentions.clone())?
            .into_iter()
            .map(|sid| self.string(sid).map(str::to_owned))
            .collect::<Result<_, _>>()?;
        Ok(SymbolRecord {
            name: row.name.to_owned(),
            qualified_name: row.qualified_name.to_owned(),
            kind: row.kind,
            span: row.span,
            name_span: row.name_span,
            discriminator: row.discriminator,
            digest: row.digest,
            mentions,
        })
    }
}

#[cfg(test)]
mod tests {
    use lemma_model::Position;

    use super::*;

    fn symbol(name: &str, qualified_name: &str, mentions: &[&str]) -> SymbolRecord {
        let span = Span {
            start: Position { line: 1, column: 0 },
            end: Position { line: 2, column: 4 },
        };
        SymbolRecord {
            name: name.to_owned(),
            qualified_name: qualified_name.to_owned(),
            kind: SymbolKind::Function,
            span,
            name_span: span,
            discriminator: 0,
            digest: *blake3::hash(qualified_name.as_bytes()).as_bytes(),
            mentions: mentions.iter().map(|text| (*text).to_owned()).collect(),
        }
    }

    fn record(symbols: Vec<SymbolRecord>) -> FileRecord {
        FileRecord {
            blob: [3; 32],
            language: Language::Python,
            has_syntax_errors: false,
            symbols,
        }
    }

    fn sample() -> Vec<u8> {
        let first = record(vec![
            symbol("Store", "Store", &["save"]),
            symbol("save", "Store.save", &["row"]),
        ]);
        let second = record(vec![symbol("post", "post", &["save", "store"])]);
        let stamp = FileStamp {
            size_bytes: 9,
            ..FileStamp::default()
        };
        let files = [
            (
                "api.py",
                Entry::Put {
                    record: Cow::Owned(second),
                    stamp,
                },
            ),
            ("gone.py", Entry::Removed),
            (
                "store.py",
                Entry::Put {
                    record: Cow::Owned(first),
                    stamp,
                },
            ),
        ];
        build(&files).unwrap()
    }

    #[test]
    fn a_built_segment_reads_back_its_files_and_symbols() {
        let bytes = sample();
        let segment = Segment::parse(Cow::Borrowed(&bytes)).unwrap();

        assert_eq!(segment.file_count(), 3);
        assert_eq!(segment.find_path("store.py").unwrap(), Some(2));
        assert_eq!(segment.find_path("none.py").unwrap(), None);
        assert!(segment.file(1).unwrap().is_removed);
        let store = segment.record(2).unwrap();
        assert_eq!(store.symbols[1].qualified_name, "Store.save");
        assert_eq!(store.symbols[1].mentions, ["row"]);
        assert_eq!(segment.file(0).unwrap().stamp.size_bytes, 9);
    }

    #[test]
    fn reverse_lists_find_symbols_by_name_and_by_mention() {
        let bytes = sample();
        let segment = Segment::parse(Cow::Borrowed(&bytes)).unwrap();
        let qualified = |symbols: Vec<usize>| -> Vec<String> {
            symbols
                .into_iter()
                .map(|symbol| segment.symbol(symbol).unwrap().qualified_name.to_owned())
                .collect()
        };

        let save = segment.find_string("save").unwrap().unwrap();

        assert_eq!(qualified(segment.named(save).unwrap()), ["Store.save"]);
        assert_eq!(
            qualified(segment.mentioned_by(save).unwrap()),
            ["post", "Store"]
        );
        let id = SymbolId::of("api.py", "post", SymbolKind::Function, 0);
        assert_eq!(segment.find_id(id).unwrap(), Some(0));
        assert_eq!(segment.symbol(0).unwrap().id, id);
    }

    /// Reads every file of a damaged segment, if it parses at all.
    fn read_everything(bytes: Vec<u8>) {
        let Ok(segment) = Segment::parse(Cow::Owned(bytes)) else {
            return;
        };
        for file in 0..segment.file_count() {
            let _ = segment.record(file);
        }
    }

    #[test]
    fn damaged_headers_are_errors_not_panics() {
        let bytes = sample();
        for at in 0..HEADER_BYTES {
            let mut damaged = bytes.clone();
            damaged[at] ^= 0xa5;
            read_everything(damaged);
        }
    }
}
