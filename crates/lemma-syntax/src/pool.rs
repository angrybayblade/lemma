//! Extractors shared by the threads of a parallel read.

use std::sync::{Mutex, PoisonError};

use crate::{Extractor, SyntaxError};

/// One extractor per worker thread, created the first time that thread has
/// to parse. An extractor compiles a language's query when it first reads
/// that language (milliseconds each), so a read whose files are all cached
/// should not pay for it, and a parallel read must not pay for it once per
/// piece of work: rayon splits a large read
/// into far more pieces than it has threads. Parsers cannot be shared
/// between threads, so each thread has its own slot.
pub struct ExtractorPool {
    slots: Vec<Mutex<Option<Extractor>>>,
}

impl ExtractorPool {
    /// An empty pool sized for the current rayon thread pool.
    pub fn new() -> Self {
        // One slot per pool thread, plus one for a caller outside the pool.
        let slots = (0..=rayon::current_num_threads())
            .map(|_| Mutex::new(None))
            .collect();
        Self { slots }
    }

    /// Runs `work` with this thread's extractor, creating it if needed.
    pub fn with<T>(&self, work: impl FnOnce(&mut Extractor) -> T) -> Result<T, SyntaxError> {
        let index = rayon::current_thread_index().map_or(0, |thread| thread + 1);
        let Some(slot) = self.slots.get(index) else {
            return Ok(work(&mut Extractor::new()?));
        };
        // Each slot is only used by its own thread, so a poisoned lock just
        // means earlier work on this thread panicked; the extractor itself
        // is still usable.
        let mut slot = slot.lock().unwrap_or_else(PoisonError::into_inner);
        let extractor = match slot.take() {
            Some(extractor) => extractor,
            None => Extractor::new()?,
        };
        Ok(work(slot.insert(extractor)))
    }
}

impl Default for ExtractorPool {
    fn default() -> Self {
        Self::new()
    }
}
