//! Bloom filters over attribute keys (spec 17). Keys are BLAKE3 digests,
//! already uniform, so a filter reads its probe positions from the key's
//! own bytes instead of hashing again. Ten bits a key and seven probes
//! wrongly let through about one key in a hundred.

use crate::Digest;

const PROBES: usize = 7;
const BITS_PER_KEY: usize = 10;

/// The filter's words for `keys`.
pub(crate) fn build<'a>(keys: impl ExactSizeIterator<Item = &'a Digest>) -> Vec<u64> {
    let mut words = vec![0u64; words_for(keys.len())];
    let bits = words.len() * 64;
    for key in keys {
        for bit in probes(key, bits) {
            words[bit / 64] |= 1 << (bit % 64);
        }
    }
    words
}

/// Whether `key` may be among the filter's keys; `word` reads one of its
/// `count` words.
pub(crate) fn may_contain<E>(
    key: &Digest,
    count: usize,
    mut word: impl FnMut(usize) -> Result<u64, E>,
) -> Result<bool, E> {
    if count == 0 {
        return Ok(true);
    }
    for bit in probes(key, count * 64) {
        if word(bit / 64)? & (1 << (bit % 64)) == 0 {
            return Ok(false);
        }
    }
    Ok(true)
}

fn words_for(keys: usize) -> usize {
    (keys * BITS_PER_KEY).div_ceil(64).max(1)
}

fn probes(key: &Digest, bits: usize) -> impl Iterator<Item = usize> + '_ {
    key.as_chunks::<4>().0[..PROBES]
        .iter()
        .map(move |bytes| usize::try_from(u32::from_le_bytes(*bytes)).unwrap_or_default() % bits)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn key(number: u32) -> Digest {
        *blake3::hash(&number.to_le_bytes()).as_bytes()
    }

    #[test]
    fn every_key_built_in_is_let_through() {
        let keys: Vec<Digest> = (0..1_000).map(key).collect();
        let words = build(keys.iter());

        for key in &keys {
            let found = may_contain(key, words.len(), |at| Ok::<_, ()>(words[at]));
            assert_eq!(found, Ok(true));
        }
    }

    #[test]
    fn most_other_keys_are_ruled_out() {
        let keys: Vec<Digest> = (0..1_000).map(key).collect();
        let words = build(keys.iter());

        let passed = (1_000..11_000)
            .filter(|number| {
                may_contain(&key(*number), words.len(), |at| Ok::<_, ()>(words[at])) == Ok(true)
            })
            .count();

        assert!(passed < 300, "{passed} of 10000 other keys passed");
    }
}
