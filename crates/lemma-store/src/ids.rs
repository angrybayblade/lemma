//! Identities that do not depend on where something is stored (spec 17):
//! a symbol's id, and the digest of a root's files.

use std::fmt;

use lemma_model::SymbolKind;

use crate::Digest;
use crate::codec;

/// A symbol's id: the first 16 bytes of a BLAKE3 digest of its path,
/// qualified name, kind and discriminator. The same symbol has the same id
/// in any store and any build, so nothing is stored to keep it across edits.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct SymbolId([u8; 16]);

impl SymbolId {
    /// The id of the symbol with this identity.
    pub fn of(path: &str, qualified_name: &str, kind: SymbolKind, discriminator: u32) -> Self {
        let mut hasher = blake3::Hasher::new();
        hasher.update(b"symbol\0");
        for part in [path.as_bytes(), qualified_name.as_bytes()] {
            hasher.update(&(part.len() as u64).to_le_bytes());
            hasher.update(part);
        }
        hasher.update(&[codec::kind_code(kind)]);
        hasher.update(&discriminator.to_le_bytes());
        let mut id = [0; 16];
        id.copy_from_slice(&hasher.finalize().as_bytes()[..16]);
        Self(id)
    }

    /// An id from its bytes, as a segment stores them.
    pub fn from_bytes(bytes: [u8; 16]) -> Self {
        Self(bytes)
    }

    /// The id's bytes.
    pub fn to_bytes(self) -> [u8; 16] {
        self.0
    }

    /// The id as people are shown it: its first 16 hexadecimal digits,
    /// enough that two symbols in one repository sharing them is unheard
    /// of. A shorter prefix still finds the symbol while it is unique.
    pub fn short(self) -> String {
        let mut text = self.to_string();
        text.truncate(SHORT_DIGITS);
        text
    }
}

/// How many hexadecimal digits [`SymbolId::short`] shows.
const SHORT_DIGITS: usize = 16;

/// The fewest hexadecimal digits a prefix may have.
const PREFIX_DIGITS: usize = 6;

/// The ids that begin with some hexadecimal digits, as a range.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct IdPrefix {
    pub(crate) lowest: [u8; 16],
    pub(crate) highest: [u8; 16],
}

impl IdPrefix {
    /// The prefix `text` writes, if it is 6 to 32 hexadecimal digits.
    pub fn parse(text: &str) -> Option<Self> {
        if !(PREFIX_DIGITS..=32).contains(&text.len()) {
            return None;
        }
        let mut lowest = [0u8; 16];
        let mut highest = [0xffu8; 16];
        for (position, digit) in text.chars().enumerate() {
            let value = u8::try_from(digit.to_digit(16)?).ok()?;
            let byte = position / 2;
            if position % 2 == 0 {
                lowest[byte] = value << 4;
                highest[byte] = (value << 4) | 0x0f;
            } else {
                lowest[byte] |= value;
                highest[byte] = (highest[byte] & 0xf0) | value;
            }
        }
        Some(Self { lowest, highest })
    }

    /// Whether `id` begins with the prefix.
    pub fn holds(&self, id: SymbolId) -> bool {
        self.lowest <= id.0 && id.0 <= self.highest
    }
}

/// Ids from numbers, for tests and fixtures that need a few distinct ones.
impl From<u128> for SymbolId {
    fn from(value: u128) -> Self {
        Self(value.to_be_bytes())
    }
}

impl fmt::Display for SymbolId {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        for byte in self.0 {
            write!(formatter, "{byte:02x}")?;
        }
        Ok(())
    }
}

/// A root's digest as four 64-bit lanes: each file adds a digest of its
/// path and blob, wrapping. Addition does not care about order, so a commit
/// updates a root's digest from the files it changed alone, and the same
/// files give the same digest whichever segments hold them.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub(crate) struct Lanes([u64; 4]);

impl Lanes {
    pub(crate) fn from_digest(digest: &Digest) -> Self {
        let mut lanes = [0; 4];
        for (lane, bytes) in lanes.iter_mut().zip(digest.as_chunks::<8>().0) {
            *lane = u64::from_le_bytes(*bytes);
        }
        Self(lanes)
    }

    pub(crate) fn to_digest(self) -> Digest {
        let mut digest = [0; 32];
        for (bytes, lane) in digest.as_chunks_mut::<8>().0.iter_mut().zip(self.0) {
            *bytes = lane.to_le_bytes();
        }
        digest
    }

    /// Counts a file in.
    pub(crate) fn add(&mut self, path: &str, blob: &Digest) {
        for (lane, term) in self.0.iter_mut().zip(file_term(path, blob)) {
            *lane = lane.wrapping_add(term);
        }
    }

    /// Counts a file out.
    pub(crate) fn subtract(&mut self, path: &str, blob: &Digest) {
        for (lane, term) in self.0.iter_mut().zip(file_term(path, blob)) {
            *lane = lane.wrapping_sub(term);
        }
    }
}

fn file_term(path: &str, blob: &Digest) -> [u64; 4] {
    let mut hasher = blake3::Hasher::new();
    hasher.update(&(path.len() as u64).to_le_bytes());
    hasher.update(path.as_bytes());
    hasher.update(blob);
    Lanes::from_digest(hasher.finalize().as_bytes()).0
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn an_id_depends_on_every_part_of_the_identity() {
        let base = SymbolId::of("a.py", "f", SymbolKind::Function, 0);

        assert_eq!(base, SymbolId::of("a.py", "f", SymbolKind::Function, 0));
        assert_ne!(base, SymbolId::of("b.py", "f", SymbolKind::Function, 0));
        assert_ne!(base, SymbolId::of("a.py", "g", SymbolKind::Function, 0));
        assert_ne!(base, SymbolId::of("a.py", "f", SymbolKind::Method, 0));
        assert_ne!(base, SymbolId::of("a.py", "f", SymbolKind::Function, 1));
        assert_ne!(base, SymbolId::of("a.p", "yf", SymbolKind::Function, 0));
    }

    #[test]
    fn a_prefix_holds_the_ids_it_begins() {
        let id = SymbolId::of("a.py", "f", SymbolKind::Function, 0);
        let text = id.to_string();

        for digits in [6, 7, 16, 32] {
            let prefix = IdPrefix::parse(&text[..digits]).unwrap();
            assert!(prefix.holds(id), "{digits} digits");
        }
        let other = SymbolId::of("b.py", "f", SymbolKind::Function, 0);
        assert!(!IdPrefix::parse(&text[..16]).unwrap().holds(other));
        assert_eq!(IdPrefix::parse("12345"), None);
        assert_eq!(IdPrefix::parse("12345z"), None);
        assert_eq!(id.short().len(), 16);
    }

    #[test]
    fn lanes_do_not_depend_on_order_and_undo_exactly() {
        let mut forward = Lanes::default();
        forward.add("a", &[1; 32]);
        forward.add("b", &[2; 32]);
        let mut backward = Lanes::default();
        backward.add("b", &[2; 32]);
        backward.add("a", &[1; 32]);

        assert_eq!(forward, backward);
        forward.subtract("b", &[2; 32]);
        let mut only_a = Lanes::default();
        only_a.add("a", &[1; 32]);
        assert_eq!(forward, only_a);
        assert_eq!(Lanes::from_digest(&only_a.to_digest()), only_a);
    }
}
