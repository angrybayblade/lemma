//! A root's files as its segments give them (spec 17). The newest segment
//! holding a path decides what is at that path, so a view answers by
//! asking its segments newest first and dropping what a newer one replaced.

use std::collections::BTreeMap;

use crate::ids::{IdPrefix, SymbolId};
use crate::segment::{FileRow, Segment};
use crate::{FileRecord, StoreError};

/// A file's row: its segment's position in the view, and the row's.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct At {
    pub segment: usize,
    pub file: usize,
}

/// A symbol's row: its segment's position in the view, and the row's.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub(crate) struct SymbolAt {
    pub segment: usize,
    pub symbol: usize,
}

/// One root's segments, oldest first.
pub(crate) struct View<'a> {
    segments: Vec<Segment<'a>>,
}

impl<'a> View<'a> {
    pub(crate) fn new(segments: Vec<Segment<'a>>) -> Self {
        Self { segments }
    }

    pub(crate) fn segments(&self) -> &[Segment<'a>] {
        &self.segments
    }

    pub(crate) fn segment(&self, at: usize) -> Result<&Segment<'a>, StoreError> {
        self.segments.get(at).ok_or(StoreError::Damaged {
            what: "a root names a segment it does not have",
        })
    }

    /// The newest row for `path`, which may say the file was removed.
    fn newest(&self, path: &str) -> Result<Option<At>, StoreError> {
        for (segment, rows) in self.segments.iter().enumerate().rev() {
            if let Some(file) = rows.find_path(path)? {
                return Ok(Some(At { segment, file }));
            }
        }
        Ok(None)
    }

    /// Where the root's file at `path` is, if it has one.
    pub(crate) fn locate(&self, path: &str) -> Result<Option<At>, StoreError> {
        let Some(at) = self.newest(path)? else {
            return Ok(None);
        };
        if self.row(at)?.is_removed {
            return Ok(None);
        }
        Ok(Some(at))
    }

    /// Whether no segment newer than `segment` holds `path`.
    fn is_current(&self, segment: usize, path: &str) -> Result<bool, StoreError> {
        for newer in self.segments.iter().skip(segment + 1) {
            if newer.find_path(path)?.is_some() {
                return Ok(false);
            }
        }
        Ok(true)
    }

    /// Whether a symbol's file is still the root's file at its path.
    fn is_current_symbol(&self, at: SymbolAt) -> Result<bool, StoreError> {
        if at.segment + 1 == self.segments.len() {
            return Ok(true);
        }
        let segment = self.segment(at.segment)?;
        let file = segment.symbol_file(at.symbol)?;
        self.is_current(at.segment, segment.path(file)?)
    }

    /// Those of a segment's `symbols` the root still has, sorted, once each.
    fn current(
        &self,
        segment: usize,
        mut symbols: Vec<usize>,
    ) -> Result<Vec<SymbolAt>, StoreError> {
        symbols.sort_unstable();
        symbols.dedup();
        let mut kept = Vec::with_capacity(symbols.len());
        for symbol in symbols {
            let at = SymbolAt { segment, symbol };
            if self.is_current_symbol(at)? {
                kept.push(at);
            }
        }
        Ok(kept)
    }

    /// Every file of the root, sorted by path.
    pub(crate) fn files(&self) -> Result<Vec<At>, StoreError> {
        let Some((first, rest)) = self.segments.split_first() else {
            return Ok(Vec::new());
        };
        let mut pending = overlay(rest)?.into_iter().peekable();
        let mut files = Vec::with_capacity(first.file_count() + pending.len());
        for file in 0..first.file_count() {
            let path = first.path(file)?;
            while let Some((_, at)) = pending.next_if(|(newer, _)| *newer < path) {
                self.keep_present(at, &mut files)?;
            }
            if let Some((_, at)) = pending.next_if(|(newer, _)| *newer == path) {
                self.keep_present(at, &mut files)?;
                continue;
            }
            self.keep_present(At { segment: 0, file }, &mut files)?;
        }
        for (_, at) in pending {
            self.keep_present(at, &mut files)?;
        }
        Ok(files)
    }

    fn keep_present(&self, at: At, files: &mut Vec<At>) -> Result<(), StoreError> {
        if !self.row(at)?.is_removed {
            files.push(at);
        }
        Ok(())
    }

    /// The newest row of every path the deltas hold, removals included.
    pub(crate) fn delta_rows(&self) -> Result<BTreeMap<&str, At>, StoreError> {
        overlay(self.segments.get(1..).unwrap_or_default())
    }

    pub(crate) fn row(&self, at: At) -> Result<FileRow, StoreError> {
        self.segment(at.segment)?.file(at.file)
    }

    pub(crate) fn path(&self, at: At) -> Result<&str, StoreError> {
        self.segment(at.segment)?.path(at.file)
    }

    pub(crate) fn record(&self, at: At) -> Result<FileRecord, StoreError> {
        self.segment(at.segment)?.record(at.file)
    }

    /// The symbols of the root's file at `path`, in source order.
    pub(crate) fn symbols_in(&self, path: &str) -> Result<Vec<SymbolAt>, StoreError> {
        let Some(at) = self.locate(path)? else {
            return Ok(Vec::new());
        };
        Ok(self
            .row(at)?
            .symbols
            .map(|symbol| SymbolAt {
                segment: at.segment,
                symbol,
            })
            .collect())
    }

    /// The symbol with `id`, if the root has it.
    pub(crate) fn find_id(&self, id: SymbolId) -> Result<Option<SymbolAt>, StoreError> {
        for (segment, rows) in self.segments.iter().enumerate().rev() {
            let Some(symbol) = rows.find_id(id)? else {
                continue;
            };
            let at = SymbolAt { segment, symbol };
            if self.is_current_symbol(at)? {
                return Ok(Some(at));
            }
        }
        Ok(None)
    }

    /// The symbols whose ids begin with `prefix`.
    pub(crate) fn ids_with(&self, prefix: IdPrefix) -> Result<Vec<SymbolAt>, StoreError> {
        let mut found = Vec::new();
        for (segment, rows) in self.segments.iter().enumerate() {
            let symbols = rows.ids_between(prefix.lowest, prefix.highest)?;
            found.extend(self.current(segment, symbols)?);
        }
        Ok(found)
    }

    /// The symbols whose name or qualified name is `text`.
    pub(crate) fn exactly_named(&self, text: &str) -> Result<Vec<SymbolAt>, StoreError> {
        let mut found = Vec::new();
        for (segment, rows) in self.segments.iter().enumerate() {
            let Some(sid) = rows.find_string(text)? else {
                continue;
            };
            let mut symbols = rows.named(sid)?;
            symbols.extend(rows.qualified_as(sid)?);
            found.extend(self.current(segment, symbols)?);
        }
        Ok(found)
    }

    /// The symbols whose qualified name holds `text`, ignoring ASCII case.
    pub(crate) fn containing(&self, text: &str) -> Result<Vec<SymbolAt>, StoreError> {
        let mut found = Vec::new();
        for (segment, rows) in self.segments.iter().enumerate() {
            let mut symbols = Vec::new();
            for sid in rows.strings_containing(text)? {
                symbols.extend(rows.qualified_as(sid)?);
            }
            found.extend(self.current(segment, symbols)?);
        }
        Ok(found)
    }

    /// The symbols whose code mentions `name`.
    pub(crate) fn mentioning(&self, name: &str) -> Result<Vec<SymbolAt>, StoreError> {
        let mut found = Vec::new();
        for (segment, rows) in self.segments.iter().enumerate() {
            let Some(sid) = rows.find_string(name)? else {
                continue;
            };
            found.extend(self.current(segment, rows.mentioned_by(sid)?)?);
        }
        Ok(found)
    }
}

/// The newest row of every path in `segments`, which follow the view's
/// first segment; positions count that first one.
fn overlay<'s>(segments: &'s [Segment<'_>]) -> Result<BTreeMap<&'s str, At>, StoreError> {
    let mut rows = BTreeMap::new();
    for (position, segment) in segments.iter().enumerate() {
        for file in 0..segment.file_count() {
            let at = At {
                segment: position + 1,
                file,
            };
            rows.insert(segment.path(file)?, at);
        }
    }
    Ok(rows)
}
