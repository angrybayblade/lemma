//! A read-only memory map of a file, so a reader can use a store's bytes
//! without copying them (spec 17). Everything past [`Mapped::open`] is safe
//! code working on a byte slice.

use std::fs::File;
use std::io;

/// The bytes of a file as they were when it was opened.
#[derive(Debug)]
pub struct Mapped {
    map: memmap2::Mmap,
}

impl Mapped {
    /// Maps all of `file`. Fails on an empty file, which cannot be mapped.
    pub fn open(file: &File) -> io::Result<Self> {
        // SAFETY: the map is read-only and is only given files Lemma treats
        // as append-only: bytes already written never change, and compaction
        // replaces a file by renaming, which leaves this mapping's file
        // intact. A process outside Lemma truncating the file would end this
        // process with a bus error; that is accepted, and is why positional
        // reads remain available.
        let map = unsafe { memmap2::Mmap::map(file)? };
        Ok(Self { map })
    }

    /// The file's bytes.
    pub fn bytes(&self) -> &[u8] {
        &self.map
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Write;

    #[test]
    fn a_mapped_file_holds_its_bytes() {
        let mut file = tempfile::tempfile().unwrap();
        file.write_all(b"lemma").unwrap();

        let mapped = Mapped::open(&file).unwrap();

        assert_eq!(mapped.bytes(), b"lemma");
    }
}
