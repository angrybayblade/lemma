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
use std::ops::Range;

use lemma_model::{Language, Span, SymbolKind};
use rayon::prelude::*;
use rustc_hash::{FxHashMap, FxHashSet};

use crate::codec::{self, Decoder, Encoder, Slot};
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

/// The columns of a symbol's row that ranking reads.
pub(crate) struct SymbolBrief {
    /// String ids of its name and qualified name.
    pub name: usize,
    pub qualified_name: usize,
    pub kind: SymbolKind,
    /// The line its definition starts on.
    pub line: u32,
    pub file: usize,
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

/// Where a file's rows start in the segment's tables.
#[derive(Clone, Copy)]
struct Bases {
    file: u32,
    symbol: u32,
    mention: u32,
}

impl Bases {
    /// How many symbols and mentions lie between these bases and `later`.
    fn until(self, later: Bases) -> (usize, usize) {
        (
            (later.symbol - self.symbol) as usize,
            (later.mention - self.mention) as usize,
        )
    }
}

type Sids<'a> = FxHashMap<&'a str, u32>;

/// Segments of fewer files, such as a commit's delta, are built on the
/// calling thread: handing them to other threads costs more than it saves.
const PARALLEL_FILES: usize = 256;

/// The columns the reverse lists and the id table are built from: each
/// symbol's name and qualified name, each mention and its symbol, and
/// each symbol's id.
struct Columns {
    names: Vec<u32>,
    qualified: Vec<u32>,
    mentions: Vec<u32>,
    owners: Vec<u32>,
    ids: Vec<(SymbolId, u32)>,
}

/// One file's share of every table it writes to, borrowed from the
/// segment and the columns, so that files fill them in parallel.
struct FileSlots<'a> {
    file_row: &'a mut [u8],
    symbol_rows: &'a mut [u8],
    mention_rows: &'a mut [u8],
    names: &'a mut [u32],
    qualified: &'a mut [u32],
    mentions: &'a mut [u32],
    owners: &'a mut [u32],
    ids: &'a mut [(SymbolId, u32)],
}

/// A segment holding `files`, which are sorted by path with no path twice.
pub(crate) fn build(files: &[(&str, Entry<'_>)]) -> Result<Vec<u8>, StoreError> {
    build_after(files, 0)
}

/// A segment holding `files`, after `prefix` zeroed bytes left for the
/// caller. Every table's size is known before anything is written, so the
/// segment is laid out at once and each file fills its own share of each
/// table, in parallel, in place. The buffer is allocated zeroed at its
/// final size, which the system hands over as untouched pages, so the
/// threads filling it touch its pages first rather than one thread zeroing
/// them all beforehand.
pub(crate) fn build_after(
    files: &[(&str, Entry<'_>)],
    prefix: usize,
) -> Result<Vec<u8>, StoreError> {
    let strings = string_table(files);
    let sids: Sids<'_> = strings
        .iter()
        .enumerate()
        .map(|(sid, text)| Ok((*text, small(sid)?)))
        .collect::<Result<_, StoreError>>()?;
    let layout = layout(files)?;
    let (symbols, mentions) = (layout.end.symbol as usize, layout.end.mention as usize);
    let counts = [files.len(), symbols, strings.len(), mentions];
    let lengths = section_lengths(files, &strings, counts)?;
    let mut out = vec![0; prefix + HEADER_BYTES + lengths.iter().sum::<usize>()];
    let (header, body) = out[prefix..].split_at_mut(HEADER_BYTES);
    write_header(header, counts, &lengths)?;
    let mut sections = split_sections(body, &lengths);
    fill_sections(files, &strings, &sids, &layout, &mut sections)?;
    Ok(out)
}

/// Where each file's rows start, and where the rows after the last end.
struct Layout {
    bases: Vec<Bases>,
    end: Bases,
}

/// Fills a laid-out segment's sections: paths and strings beside the
/// files' rows and columns, then the lists built from the columns.
fn fill_sections(
    files: &[(&str, Entry<'_>)],
    strings: &[&str],
    sids: &Sids<'_>,
    layout: &Layout,
    sections: &mut [&mut [u8]],
) -> Result<(), StoreError> {
    let (symbols, mentions) = (layout.end.symbol as usize, layout.end.mention as usize);
    let [
        path_offsets,
        path_bytes,
        file_rows,
        string_offsets,
        string_bytes,
        symbol_rows,
        mention_rows,
        lists @ ..,
    ] = sections
    else {
        return Err(DAMAGED);
    };
    let mut columns = Columns {
        names: vec![0; symbols],
        qualified: vec![0; symbols],
        mentions: vec![0; mentions],
        owners: vec![0; mentions],
        ids: vec![(SymbolId::from(0), 0); symbols],
    };
    let slots = FileSlots {
        file_row: file_rows,
        symbol_rows,
        mention_rows,
        names: &mut columns.names,
        qualified: &mut columns.qualified,
        mentions: &mut columns.mentions,
        owners: &mut columns.owners,
        ids: &mut columns.ids,
    };
    let mut texts = || {
        write_texts(
            files.iter().map(|(path, _)| *path),
            path_offsets,
            path_bytes,
        )?;
        write_texts(strings.iter().copied(), string_offsets, string_bytes)
    };
    let fill = || fill_files(files, &layout.bases, layout.end, sids, slots);
    let is_small = files.len() < PARALLEL_FILES;
    let (written, filled) = if is_small {
        (texts(), fill())
    } else {
        rayon::join(texts, fill)
    };
    written?;
    filled?;
    write_lists(lists, &mut columns, strings.len(), is_small);
    Ok(())
}

/// Below this many files, a share of the files is filled on one thread.
const FILL_GRAIN: usize = 512;

/// Fills `files`, whose shares of the tables are `slots`, halving them and
/// filling the halves at once until a half is small. `end` is where the
/// rows after the last file would start.
fn fill_files(
    files: &[(&str, Entry<'_>)],
    bases: &[Bases],
    end: Bases,
    sids: &Sids<'_>,
    slots: FileSlots<'_>,
) -> Result<(), StoreError> {
    let (Some(first), Some(_)) = (bases.first().copied(), files.first()) else {
        return Ok(());
    };
    if files.len() <= FILL_GRAIN {
        let mut rest = slots;
        for (at, ((path, entry), base)) in files.iter().zip(bases).enumerate() {
            let next = bases.get(at + 1).copied().unwrap_or(end);
            let (taken, left) = cut(rest, 1, base.until(next));
            fill_file(path, entry, *base, sids, taken)?;
            rest = left;
        }
        return Ok(());
    }
    let middle = files.len() / 2;
    let split = bases[middle];
    let (left_slots, right_slots) = cut(slots, middle, first.until(split));
    let (left, right) = rayon::join(
        || fill_files(&files[..middle], &bases[..middle], split, sids, left_slots),
        || fill_files(&files[middle..], &bases[middle..], end, sids, right_slots),
    );
    left.and(right)
}

/// Each section's length, from the counts and the paths' and strings' text.
fn section_lengths(
    files: &[(&str, Entry<'_>)],
    strings: &[&str],
    [files_count, symbols, strings_count, mentions]: [usize; 4],
) -> Result<[usize; SECTIONS], StoreError> {
    let table = |count: usize| (count + 1) * 4;
    let lengths = [
        table(files_count),
        files.iter().map(|(path, _)| path.len()).sum(),
        files_count * FILE_ROW,
        table(strings_count),
        strings.iter().map(|text| text.len()).sum(),
        symbols * SYMBOL_ROW,
        mentions * 4,
        symbols * ID_ROW,
        table(strings_count),
        symbols * 4,
        table(strings_count),
        symbols * 4,
        table(strings_count),
        mentions * 4,
    ];
    small(HEADER_BYTES + lengths.iter().sum::<usize>())?;
    Ok(lengths)
}

fn write_header(
    header: &mut [u8],
    counts: [usize; 4],
    lengths: &[usize; SECTIONS],
) -> Result<(), StoreError> {
    let mut encoder = Encoder::default();
    for count in counts {
        encoder.u32(small(count)?);
    }
    let mut at = HEADER_BYTES;
    for length in lengths {
        encoder.u32(small(at)?);
        encoder.u32(small(*length)?);
        at += length;
    }
    header.copy_from_slice(&encoder.bytes);
    Ok(())
}

fn split_sections<'a>(mut body: &'a mut [u8], lengths: &[usize; SECTIONS]) -> Vec<&'a mut [u8]> {
    let mut sections = Vec::with_capacity(SECTIONS);
    for length in lengths {
        let (section, rest) = std::mem::take(&mut body).split_at_mut(*length);
        sections.push(section);
        body = rest;
    }
    sections
}

/// Where each file's symbols and mentions start, and where they end.
fn layout(files: &[(&str, Entry<'_>)]) -> Result<Layout, StoreError> {
    let mut bases = Vec::with_capacity(files.len());
    let (mut symbol, mut mention) = (0usize, 0usize);
    for (file, (_, entry)) in files.iter().enumerate() {
        bases.push(Bases {
            file: small(file)?,
            symbol: small(symbol)?,
            mention: small(mention)?,
        });
        if let Entry::Put { record, .. } = entry {
            symbol += record.symbols.len();
            mention += record
                .symbols
                .iter()
                .map(|symbol| symbol.mentions.len())
                .sum::<usize>();
        }
    }
    let end = Bases {
        file: small(files.len())?,
        symbol: small(symbol)?,
        mention: small(mention)?,
    };
    Ok(Layout { bases, end })
}

/// The share of `slots` of the first `files` files, which hold `symbols`
/// and `mentions` between them, and what is left.
fn cut(
    slots: FileSlots<'_>,
    files: usize,
    (symbols, mentions): (usize, usize),
) -> (FileSlots<'_>, FileSlots<'_>) {
    let FileSlots {
        file_row,
        symbol_rows,
        mention_rows,
        names,
        qualified,
        mentions: mention_sids,
        owners,
        ids,
    } = slots;
    let file_row = file_row.split_at_mut(files * FILE_ROW);
    let symbol_rows = symbol_rows.split_at_mut(symbols * SYMBOL_ROW);
    let mention_rows = mention_rows.split_at_mut(mentions * 4);
    let names = names.split_at_mut(symbols);
    let qualified = qualified.split_at_mut(symbols);
    let mention_sids = mention_sids.split_at_mut(mentions);
    let owners = owners.split_at_mut(mentions);
    let ids = ids.split_at_mut(symbols);
    let taken = FileSlots {
        file_row: file_row.0,
        symbol_rows: symbol_rows.0,
        mention_rows: mention_rows.0,
        names: names.0,
        qualified: qualified.0,
        mentions: mention_sids.0,
        owners: owners.0,
        ids: ids.0,
    };
    let left = FileSlots {
        file_row: file_row.1,
        symbol_rows: symbol_rows.1,
        mention_rows: mention_rows.1,
        names: names.1,
        qualified: qualified.1,
        mentions: mention_sids.1,
        owners: owners.1,
        ids: ids.1,
    };
    (taken, left)
}

/// Every name, qualified name and mention in `files`, sorted, once each.
fn string_table<'a>(files: &'a [(&str, Entry<'_>)]) -> Vec<&'a str> {
    if files.len() < PARALLEL_FILES {
        let mut strings: Vec<&str> = files
            .iter()
            .filter_map(|(_, entry)| match entry {
                Entry::Put { record, .. } => Some(record),
                Entry::Removed => None,
            })
            .flat_map(|record| &record.symbols)
            .flat_map(|symbol| {
                [symbol.name.as_str(), symbol.qualified_name.as_str()]
                    .into_iter()
                    .chain(symbol.mentions.iter().map(String::as_str))
            })
            .collect();
        strings.sort_unstable();
        strings.dedup();
        return strings;
    }
    // Each piece of work gathers its strings into a set of its own, which
    // are merged after; few large pieces mean few sets to merge.
    let grain = files.len() / (rayon::current_num_threads() * 2) + 1;
    let seen = files
        .par_iter()
        .with_min_len(grain)
        .fold(
            FxHashSet::default,
            |mut seen: FxHashSet<&'a str>, (_, entry)| {
                if let Entry::Put { record, .. } = entry {
                    for symbol in &record.symbols {
                        seen.insert(&symbol.name);
                        seen.insert(&symbol.qualified_name);
                        seen.extend(symbol.mentions.iter().map(String::as_str));
                    }
                }
                seen
            },
        )
        .reduce(FxHashSet::default, |left, right| {
            let (mut larger, smaller) = if left.len() < right.len() {
                (right, left)
            } else {
                (left, right)
            };
            larger.extend(smaller);
            larger
        });
    let mut strings: Vec<&str> = seen.into_iter().collect();
    strings.par_sort_unstable();
    strings
}

/// Writes texts and the offsets that delimit them into a pair of sections.
fn write_texts<'t>(
    texts: impl Iterator<Item = &'t str>,
    offsets: &mut [u8],
    bytes: &mut [u8],
) -> Result<(), StoreError> {
    let mut length = 0;
    let mut at = 0;
    for text in texts {
        offsets[at..at + 4].copy_from_slice(&small(length)?.to_le_bytes());
        bytes[length..length + text.len()].copy_from_slice(text.as_bytes());
        length += text.len();
        at += 4;
    }
    offsets[at..at + 4].copy_from_slice(&small(length)?.to_le_bytes());
    Ok(())
}

/// Fills one file's row, its symbols' rows and its share of the columns.
fn fill_file(
    path: &str,
    entry: &Entry<'_>,
    bases: Bases,
    sids: &Sids<'_>,
    slots: FileSlots<'_>,
) -> Result<(), StoreError> {
    let FileSlots {
        file_row,
        symbol_rows,
        mention_rows,
        names,
        qualified,
        mentions,
        owners,
        ids,
    } = slots;
    let mut row = Slot::new(file_row);
    let Entry::Put { record, stamp } = entry else {
        row.bytes(&[0; 32]);
        row.u8(0);
        row.u8(REMOVED);
        write_stamp(&mut row, FileStamp::default());
        row.u32(bases.symbol);
        row.u32(0);
        return Ok(());
    };
    row.bytes(&record.blob);
    row.u8(codec::language_code(record.language));
    row.u8(if record.has_syntax_errors {
        SYNTAX_ERRORS
    } else {
        0
    });
    write_stamp(&mut row, *stamp);
    row.u32(bases.symbol);
    row.u32(small(record.symbols.len())?);
    let mut mention = 0;
    for (position, symbol) in record.symbols.iter().enumerate() {
        let ordinal = bases.symbol + small(position)?;
        let first_mention = bases.mention + small(mention)?;
        let symbol_row = &mut symbol_rows[position * SYMBOL_ROW..(position + 1) * SYMBOL_ROW];
        let (name, qualified_name, id) =
            fill_symbol(path, symbol, (bases.file, first_mention), sids, symbol_row)?;
        names[position] = name;
        qualified[position] = qualified_name;
        ids[position] = (id, ordinal);
        for text in &symbol.mentions {
            let sid = sids.get(text.as_str()).copied().ok_or(DAMAGED)?;
            mention_rows[mention * 4..mention * 4 + 4].copy_from_slice(&sid.to_le_bytes());
            mentions[mention] = sid;
            owners[mention] = ordinal;
            mention += 1;
        }
    }
    Ok(())
}

fn write_stamp(row: &mut Slot<'_>, stamp: FileStamp) {
    row.u64(stamp.size_bytes);
    row.i64(stamp.modified_ns);
    row.i64(stamp.changed_ns);
    row.u64(stamp.inode);
}

/// Writes one symbol's row and returns its name, qualified name and id.
fn fill_symbol(
    path: &str,
    symbol: &SymbolRecord,
    (file, first_mention): (u32, u32),
    sids: &Sids<'_>,
    out: &mut [u8],
) -> Result<(u32, u32, SymbolId), StoreError> {
    let sid = |text: &str| sids.get(text).copied().ok_or(DAMAGED);
    let id = SymbolId::of(
        path,
        &symbol.qualified_name,
        symbol.kind,
        symbol.discriminator,
    );
    let name = sid(&symbol.name)?;
    let qualified = sid(&symbol.qualified_name)?;
    let mut row = Slot::new(out);
    row.u32(name);
    row.u32(qualified);
    row.u8(codec::kind_code(symbol.kind));
    row.span(symbol.span);
    row.span(symbol.name_span);
    row.u32(symbol.discriminator);
    row.bytes(&symbol.digest);
    row.bytes(&id.to_bytes());
    row.u32(file);
    row.u32(first_mention);
    Ok((name, qualified, id))
}

/// Writes the id table and the three lists from a string to its symbols.
fn write_lists(lists: &mut [&mut [u8]], columns: &mut Columns, strings: usize, is_small: bool) {
    let [
        ids,
        by_name,
        by_name_rows,
        by_qualified,
        by_qualified_rows,
        mentioned_by,
        mentioned_by_rows,
    ] = lists
    else {
        return;
    };
    let Columns {
        names,
        qualified,
        mentions,
        owners,
        ids: id_column,
    } = columns;
    if is_small {
        id_column.sort_unstable();
    } else {
        id_column.par_sort_unstable();
    }
    let owners: &[u32] = owners;
    let jobs: Vec<Box<dyn FnOnce() + Send + '_>> = vec![
        Box::new(|| write_id_table(id_column, ids)),
        Box::new(|| reverse(strings, names, ordinal, (by_name, by_name_rows))),
        Box::new(|| {
            reverse(
                strings,
                qualified,
                ordinal,
                (by_qualified, by_qualified_rows),
            );
        }),
        Box::new(|| {
            reverse(
                strings,
                mentions,
                |at| owners[at],
                (mentioned_by, mentioned_by_rows),
            );
        }),
    ];
    if is_small {
        for job in jobs {
            job();
        }
    } else {
        jobs.into_par_iter().for_each(|job| job());
    }
}

/// A symbol's position as stored; `bases` checked that every one fits.
fn ordinal(symbol: usize) -> u32 {
    u32::try_from(symbol).unwrap_or(u32::MAX)
}

/// The id table from ids already sorted.
fn write_id_table(ids: &[(SymbolId, u32)], out: &mut [u8]) {
    for ((id, symbol), row) in ids.iter().zip(out.as_chunks_mut::<ID_ROW>().0) {
        row[..16].copy_from_slice(&id.to_bytes());
        row[16..].copy_from_slice(&symbol.to_le_bytes());
    }
}

/// A list from each string to its owners, in owner order: offsets into
/// the rows, then the rows. `keys[i]` is the string of the `i`th entry,
/// whose owner is `owner(i)`.
fn reverse(
    strings: usize,
    keys: &[u32],
    owner: impl Fn(usize) -> u32,
    (offsets, rows): (&mut [u8], &mut [u8]),
) {
    let mut starts = vec![0u32; strings + 1];
    for key in keys {
        starts[*key as usize + 1] += 1;
    }
    for at in 1..starts.len() {
        starts[at] += starts[at - 1];
    }
    for (start, slot) in starts.iter().zip(offsets.as_chunks_mut::<4>().0) {
        slot.copy_from_slice(&start.to_le_bytes());
    }
    for (entry, key) in keys.iter().enumerate() {
        let slot = &mut starts[*key as usize];
        let at = *slot as usize * 4;
        rows[at..at + 4].copy_from_slice(&owner(entry).to_le_bytes());
        *slot += 1;
    }
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

/// Whether `text` holds `needle`, ignoring ASCII case: candidates are
/// found by the needle's first byte in either case, then compared.
fn holds_ignoring_case(text: &[u8], needle: &[u8]) -> bool {
    let Some((first, rest)) = needle.split_first() else {
        return true;
    };
    if text.len() < needle.len() {
        return false;
    }
    let last_start = text.len() - needle.len();
    let candidates = memchr::memchr2_iter(
        first.to_ascii_lowercase(),
        first.to_ascii_uppercase(),
        &text[..=last_start],
    );
    for at in candidates {
        if text[at + 1..at + needle.len()].eq_ignore_ascii_case(rest) {
            return true;
        }
    }
    false
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

    /// The ids of strings holding `needle`, ignoring ASCII case, sorted.
    pub(crate) fn strings_containing(&self, needle: &str) -> Result<Vec<usize>, StoreError> {
        let needle = needle.as_bytes();
        (0..self.strings)
            .into_par_iter()
            .with_min_len(4096)
            .filter_map(|sid| match self.string_bytes(sid) {
                Ok(text) => holds_ignoring_case(text, needle).then_some(Ok(sid)),
                Err(error) => Some(Err(error)),
            })
            .collect()
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

    /// What ranking a symbol needs, read from its row alone.
    pub(crate) fn symbol_brief(&self, symbol: usize) -> Result<SymbolBrief, StoreError> {
        let row = self.symbol_row(symbol)?;
        let name = u32_at(row, 0)? as usize;
        let qualified_name = u32_at(row, 4)? as usize;
        let kind = codec::kind_of(*row.get(8).ok_or(DAMAGED)?)?;
        let line = u32_at(row, 9)?;
        let file = u32_at(row, SYMBOL_FILE_AT)? as usize;
        if name >= self.strings || qualified_name >= self.strings || file >= self.files {
            return Err(DAMAGED);
        }
        Ok(SymbolBrief {
            name,
            qualified_name,
            kind,
            line,
            file,
        })
    }

    /// The length in bytes of string `sid`.
    pub(crate) fn string_length(&self, sid: usize) -> Result<usize, StoreError> {
        let bytes = self.section(STRING_BYTES);
        Ok(self.range_in(STRING_OFFSETS, sid, bytes.len())?.len())
    }

    /// A file's language, without decoding the rest of its row.
    pub(crate) fn file_language(&self, file: usize) -> Result<Language, StoreError> {
        let at = file.checked_mul(FILE_ROW).ok_or(DAMAGED)? + 32;
        codec::language_of(*self.section(FILES).get(at).ok_or(DAMAGED)?)
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

    #[test]
    fn a_segment_built_in_parallel_reads_back_every_file() {
        let records: Vec<(String, FileRecord)> = (0..3000)
            .map(|n| {
                let name = format!("f{n}");
                let qualified_name = format!("m{}.f{n}", n % 7);
                let group = format!("g{}", n % 13);
                let symbols = vec![symbol(&name, &qualified_name, &["shared", &group])];
                (format!("src/file{n:05}.py"), record(symbols))
            })
            .collect();
        let files: Vec<(&str, Entry<'_>)> = records
            .iter()
            .map(|(path, record)| {
                let entry = Entry::Put {
                    record: Cow::Borrowed(record),
                    stamp: FileStamp::default(),
                };
                (path.as_str(), entry)
            })
            .collect();

        let bytes = build(&files).unwrap();

        let segment = Segment::parse(Cow::Borrowed(&bytes)).unwrap();
        for (file, (_, record)) in records.iter().enumerate() {
            assert_eq!(&segment.record(file).unwrap(), record);
        }
        let shared = segment.find_string("shared").unwrap().unwrap();
        assert_eq!(segment.mentioned_by(shared).unwrap().len(), 3000);
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
