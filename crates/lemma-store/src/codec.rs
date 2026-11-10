//! The primitive encodings. Integers are little-endian and fixed width, and
//! every length is checked against the bytes left before it is used, so
//! damaged bytes are an error and never a panic.

use lemma_model::{Language, Position, Span, SymbolKind};

use crate::{Digest, StoreError};

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

pub(crate) fn kind_code(kind: SymbolKind) -> u8 {
    let position = KINDS.iter().position(|known| *known == kind);
    u8::try_from(position.unwrap_or(usize::MAX)).unwrap_or(u8::MAX)
}

pub(crate) fn kind_of(code: u8) -> Result<SymbolKind, StoreError> {
    KINDS
        .get(usize::from(code))
        .copied()
        .ok_or(StoreError::Damaged {
            what: "unknown symbol kind",
        })
}

pub(crate) fn language_code(language: Language) -> u8 {
    let position = Language::ALL.iter().position(|known| *known == language);
    u8::try_from(position.unwrap_or(usize::MAX)).unwrap_or(u8::MAX)
}

pub(crate) fn language_of(code: u8) -> Result<Language, StoreError> {
    Language::ALL
        .get(usize::from(code))
        .copied()
        .ok_or(StoreError::Damaged {
            what: "unknown language",
        })
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

    pub fn digest(&mut self, value: &Digest) {
        self.bytes.extend_from_slice(value);
    }

    pub fn count(&mut self, value: usize) {
        // A length beyond u32 saturates, and the reader then rejects the
        // bytes as truncated rather than misreading them.
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

/// Writes values one after another into a row of a fixed size, in place.
/// Rows are sized from the same constants their writers follow; a value
/// that would not fit is dropped rather than panicking, and the tests that
/// read rows back catch it.
pub(crate) struct Slot<'a> {
    out: &'a mut [u8],
    at: usize,
}

impl<'a> Slot<'a> {
    pub fn new(out: &'a mut [u8]) -> Self {
        Self { out, at: 0 }
    }

    pub fn bytes(&mut self, value: &[u8]) {
        let end = self.at + value.len();
        if let Some(target) = self.out.get_mut(self.at..end) {
            target.copy_from_slice(value);
        }
        self.at = end;
    }

    pub fn u8(&mut self, value: u8) {
        self.bytes(&[value]);
    }

    pub fn u32(&mut self, value: u32) {
        self.bytes(&value.to_le_bytes());
    }

    pub fn u64(&mut self, value: u64) {
        self.bytes(&value.to_le_bytes());
    }

    pub fn i64(&mut self, value: i64) {
        self.bytes(&value.to_le_bytes());
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

    pub fn bytes16(&mut self) -> Result<[u8; 16], StoreError> {
        let mut bytes = [0; 16];
        bytes.copy_from_slice(self.take(16)?);
        Ok(bytes)
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
    what: "stored bytes are truncated",
};
