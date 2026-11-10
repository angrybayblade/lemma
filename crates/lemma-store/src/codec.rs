//! The node encodings. Integers are little-endian and fixed width, and
//! every length is checked against the bytes left before it is used, so
//! damaged bytes are an error and never a panic.

use std::collections::BTreeSet;

use lemma_model::{Language, Position, Span, SymbolKind};

use crate::{Digest, FileRecord, SourceRecord, StoreError, SymbolRecord};

/// Every symbol kind; a kind is stored as its position here, so entries
/// are only ever added at the end.
const KINDS: [SymbolKind; 10] = [
    SymbolKind::Function,
    SymbolKind::Method,
    SymbolKind::Class,
    SymbolKind::Interface,
    SymbolKind::Struct,
    SymbolKind::Enum,
    SymbolKind::TypeAlias,
    SymbolKind::Constant,
    SymbolKind::Variable,
    SymbolKind::Field,
];

/// What a directory's entry points at.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum EntryKind {
    File,
    Directory,
}

/// A child of a directory.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct Entry {
    pub kind: EntryKind,
    pub digest: Digest,
    pub offset: u64,
}

/// A stored source and the tree it points at.
pub(crate) struct DecodedSource {
    pub record: SourceRecord,
    pub tree: Entry,
}

pub(crate) fn kind_code(kind: SymbolKind) -> u8 {
    let position = KINDS.iter().position(|known| *known == kind);
    u8::try_from(position.unwrap_or(usize::MAX)).unwrap_or(u8::MAX)
}

fn language_code(language: Language) -> u8 {
    let position = Language::ALL.iter().position(|known| *known == language);
    u8::try_from(position.unwrap_or(usize::MAX)).unwrap_or(u8::MAX)
}

#[derive(Default)]
pub(crate) struct Encoder {
    pub bytes: Vec<u8>,
}

impl Encoder {
    pub fn u8(&mut self, value: u8) {
        self.bytes.push(value);
    }

    pub fn u32(&mut self, value: u32) {
        self.bytes.extend_from_slice(&value.to_le_bytes());
    }

    pub fn u64(&mut self, value: u64) {
        self.bytes.extend_from_slice(&value.to_le_bytes());
    }

    pub fn i64(&mut self, value: i64) {
        self.bytes.extend_from_slice(&value.to_le_bytes());
    }

    pub fn digest(&mut self, value: &Digest) {
        self.bytes.extend_from_slice(value);
    }

    pub fn count(&mut self, value: usize) {
        // A length beyond u32 saturates, and the reader then rejects the
        // node as truncated rather than misreading it.
        self.u32(u32::try_from(value).unwrap_or(u32::MAX));
    }

    pub fn text(&mut self, value: &str) {
        self.count(value.len());
        self.bytes.extend_from_slice(value.as_bytes());
    }

    pub fn span(&mut self, span: Span) {
        self.u32(span.start.line);
        self.u32(span.start.column);
        self.u32(span.end.line);
        self.u32(span.end.column);
    }
}

pub(crate) struct Decoder<'a> {
    bytes: &'a [u8],
    at: usize,
}

impl<'a> Decoder<'a> {
    pub fn new(bytes: &'a [u8]) -> Self {
        Self { bytes, at: 0 }
    }

    fn take(&mut self, count: usize) -> Result<&'a [u8], StoreError> {
        let end = self.at.checked_add(count).ok_or(TRUNCATED)?;
        let taken = self.bytes.get(self.at..end).ok_or(TRUNCATED)?;
        self.at = end;
        Ok(taken)
    }

    pub fn u8(&mut self) -> Result<u8, StoreError> {
        Ok(self.take(1)?[0])
    }

    pub fn u32(&mut self) -> Result<u32, StoreError> {
        let taken = self.take(4)?;
        Ok(u32::from_le_bytes([taken[0], taken[1], taken[2], taken[3]]))
    }

    pub fn u64(&mut self) -> Result<u64, StoreError> {
        let low = u64::from(self.u32()?);
        let high = u64::from(self.u32()?);
        Ok(low | (high << 32))
    }

    pub fn i64(&mut self) -> Result<i64, StoreError> {
        Ok(i64::from_le_bytes(self.u64()?.to_le_bytes()))
    }

    pub fn digest(&mut self) -> Result<Digest, StoreError> {
        let mut digest = [0; 32];
        digest.copy_from_slice(self.take(32)?);
        Ok(digest)
    }

    /// A count of rows, each at least `row_bytes` long: checked against
    /// the bytes left, so a damaged count cannot demand a huge allocation.
    pub fn count(&mut self, row_bytes: usize) -> Result<usize, StoreError> {
        let count = usize::try_from(self.u32()?).map_err(|_| TRUNCATED)?;
        let needed = count.checked_mul(row_bytes).ok_or(TRUNCATED)?;
        if needed > self.bytes.len() - self.at {
            return Err(TRUNCATED);
        }
        Ok(count)
    }

    pub fn text(&mut self) -> Result<&'a str, StoreError> {
        let length = self.count(1)?;
        std::str::from_utf8(self.take(length)?).map_err(|_| StoreError::Damaged {
            what: "text is not UTF-8",
        })
    }

    pub fn span(&mut self) -> Result<Span, StoreError> {
        let start = Position {
            line: self.u32()?,
            column: self.u32()?,
        };
        let end = Position {
            line: self.u32()?,
            column: self.u32()?,
        };
        Ok(Span { start, end })
    }
}

const TRUNCATED: StoreError = StoreError::Damaged {
    what: "node is truncated",
};

const SID_RANGE: StoreError = StoreError::Damaged {
    what: "string id is out of range",
};

/// The digest a file node is filed under.
pub(crate) fn file_digest(record: &FileRecord) -> Digest {
    *blake3::hash(&encode_file(record)).as_bytes()
}

/// A file node's payload. It holds no offsets, so its digest is the same
/// in any store.
pub(crate) fn encode_file(record: &FileRecord) -> Vec<u8> {
    let mut strings = BTreeSet::new();
    for symbol in &record.symbols {
        strings.insert(symbol.name.as_str());
        strings.insert(symbol.qualified_name.as_str());
        strings.extend(symbol.mentions.iter().map(String::as_str));
    }
    let strings: Vec<&str> = strings.into_iter().collect();
    let sid = |text: &str| strings.binary_search(&text).unwrap_or(usize::MAX);

    let mut encoder = Encoder::default();
    encoder.digest(&record.blob);
    encoder.u8(language_code(record.language));
    encoder.u8(u8::from(record.has_syntax_errors));
    encoder.count(strings.len());
    for text in &strings {
        encoder.text(text);
    }
    encoder.count(record.symbols.len());
    for symbol in &record.symbols {
        encode_symbol(&mut encoder, symbol, &sid);
    }
    encoder.bytes
}

fn encode_symbol(encoder: &mut Encoder, symbol: &SymbolRecord, sid: &dyn Fn(&str) -> usize) {
    encoder.count(sid(&symbol.name));
    encoder.count(sid(&symbol.qualified_name));
    encoder.u8(kind_code(symbol.kind));
    encoder.span(symbol.span);
    encoder.span(symbol.name_span);
    encoder.u32(symbol.discriminator);
    encoder.digest(&symbol.digest);
    encoder.count(symbol.mentions.len());
    for mention in &symbol.mentions {
        encoder.count(sid(mention));
    }
}

/// Bytes of a symbol's fixed fields, the least a row can take.
const SYMBOL_ROW_BYTES: usize = 4 + 4 + 1 + 32 + 4 + 32 + 4;

pub(crate) fn decode_file(payload: &[u8]) -> Result<FileRecord, StoreError> {
    let mut decoder = Decoder::new(payload);
    let blob = decoder.digest()?;
    let language = *Language::ALL
        .get(usize::from(decoder.u8()?))
        .ok_or(StoreError::Damaged {
            what: "unknown language",
        })?;
    let has_syntax_errors = decoder.u8()? != 0;
    let count = decoder.count(4)?;
    let mut strings = Vec::with_capacity(count);
    for _ in 0..count {
        strings.push(decoder.text()?);
    }
    let count = decoder.count(SYMBOL_ROW_BYTES)?;
    let mut symbols = Vec::with_capacity(count);
    for _ in 0..count {
        symbols.push(decode_symbol(&mut decoder, &strings)?);
    }
    Ok(FileRecord {
        blob,
        language,
        has_syntax_errors,
        symbols,
    })
}

/// A file's blob digest alone, without decoding the rest.
pub(crate) fn decode_blob(payload: &[u8]) -> Result<Digest, StoreError> {
    Decoder::new(payload).digest()
}

fn string_at(strings: &[&str], decoder: &mut Decoder<'_>) -> Result<String, StoreError> {
    let sid = usize::try_from(decoder.u32()?).map_err(|_| SID_RANGE)?;
    strings
        .get(sid)
        .map(|text| (*text).to_owned())
        .ok_or(SID_RANGE)
}

fn decode_symbol(decoder: &mut Decoder<'_>, strings: &[&str]) -> Result<SymbolRecord, StoreError> {
    let name = string_at(strings, decoder)?;
    let qualified_name = string_at(strings, decoder)?;
    let kind = *KINDS
        .get(usize::from(decoder.u8()?))
        .ok_or(StoreError::Damaged {
            what: "unknown symbol kind",
        })?;
    let span = decoder.span()?;
    let name_span = decoder.span()?;
    let discriminator = decoder.u32()?;
    let digest = decoder.digest()?;
    let count = decoder.count(4)?;
    let mut mentions = Vec::with_capacity(count);
    for _ in 0..count {
        mentions.push(string_at(strings, decoder)?);
    }
    Ok(SymbolRecord {
        name,
        qualified_name,
        kind,
        span,
        name_span,
        discriminator,
        digest,
        mentions,
    })
}

const FILE_ENTRY: u8 = 0;
const DIRECTORY_ENTRY: u8 = 1;

fn entry_code(kind: EntryKind) -> u8 {
    match kind {
        EntryKind::File => FILE_ENTRY,
        EntryKind::Directory => DIRECTORY_ENTRY,
    }
}

/// A directory's payload, entries sorted by name.
pub(crate) fn encode_directory<'a>(
    entries: impl ExactSizeIterator<Item = (&'a String, &'a Entry)>,
) -> Vec<u8> {
    let mut encoder = Encoder::default();
    encoder.count(entries.len());
    for (name, entry) in entries {
        encoder.text(name);
        encoder.u8(entry_code(entry.kind));
        encoder.digest(&entry.digest);
        encoder.u64(entry.offset);
    }
    encoder.bytes
}

/// A directory's digest: its names, kinds and children's digests. Offsets
/// are left out, so the same content has the same digest in any store.
pub(crate) fn directory_digest<'a>(
    entries: impl Iterator<Item = (&'a String, &'a Entry)>,
) -> Digest {
    let mut hasher = blake3::Hasher::new();
    for (name, entry) in entries {
        hasher.update(&(name.len() as u64).to_le_bytes());
        hasher.update(name.as_bytes());
        hasher.update(&[entry_code(entry.kind)]);
        hasher.update(&entry.digest);
    }
    *hasher.finalize().as_bytes()
}

/// Bytes of a directory row beyond its name.
const ENTRY_ROW_BYTES: usize = 4 + 1 + 32 + 8;

pub(crate) fn decode_directory(payload: &[u8]) -> Result<Vec<(String, Entry)>, StoreError> {
    let mut decoder = Decoder::new(payload);
    let count = decoder.count(ENTRY_ROW_BYTES)?;
    let mut entries = Vec::with_capacity(count);
    for _ in 0..count {
        let name = decoder.text()?.to_owned();
        let kind = match decoder.u8()? {
            FILE_ENTRY => EntryKind::File,
            DIRECTORY_ENTRY => EntryKind::Directory,
            _ => {
                return Err(StoreError::Damaged {
                    what: "unknown directory entry kind",
                });
            }
        };
        let digest = decoder.digest()?;
        let offset = decoder.u64()?;
        entries.push((
            name,
            Entry {
                kind,
                digest,
                offset,
            },
        ));
    }
    Ok(entries)
}

pub(crate) fn encode_source(record: &SourceRecord, tree: &Entry) -> Vec<u8> {
    let mut encoder = Encoder::default();
    encoder.text(&record.repository);
    encoder.text(&record.revision);
    encoder.digest(&record.profile);
    encoder.digest(&tree.digest);
    encoder.u64(tree.offset);
    encoder.bytes
}

pub(crate) fn decode_source(payload: &[u8]) -> Result<DecodedSource, StoreError> {
    let mut decoder = Decoder::new(payload);
    let repository = decoder.text()?.to_owned();
    let revision = decoder.text()?.to_owned();
    let profile = decoder.digest()?;
    let digest = decoder.digest()?;
    let offset = decoder.u64()?;
    Ok(DecodedSource {
        record: SourceRecord {
            repository,
            revision,
            profile,
        },
        tree: Entry {
            kind: EntryKind::Directory,
            digest,
            offset,
        },
    })
}
