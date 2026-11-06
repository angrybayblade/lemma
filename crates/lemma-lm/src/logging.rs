//! What `lm` is doing, as log lines on stderr when `-l/--logging` asks for
//! them; off by default, so stdout (results, JSON, the language server's
//! protocol) is never touched.

use std::io::Write;
use std::time::Instant;

use log::{LevelFilter, Log, Metadata, Record};

/// How much to log, from `-l/--logging`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, clap::ValueEnum)]
pub(crate) enum LogLevel {
    /// Failures only.
    Error,
    /// Failures, and what was worked around.
    Warn,
    /// Each step: modules loaded, repositories read, files formatted.
    Info,
    /// More detail on each step.
    Debug,
    /// Everything, including each message to and from an editor.
    Trace,
}

impl LogLevel {
    fn filter(self) -> LevelFilter {
        match self {
            Self::Error => LevelFilter::Error,
            Self::Warn => LevelFilter::Warn,
            Self::Info => LevelFilter::Info,
            Self::Debug => LevelFilter::Debug,
            Self::Trace => LevelFilter::Trace,
        }
    }
}

/// Writes each record to stderr with the time since `lm` started.
struct Stderr {
    started: Instant,
}

impl Log for Stderr {
    fn enabled(&self, metadata: &Metadata<'_>) -> bool {
        metadata.level() <= log::max_level()
    }

    fn log(&self, record: &Record<'_>) {
        if !self.enabled(record.metadata()) {
            return;
        }
        let seconds = self.started.elapsed().as_secs_f64();
        // A log line that cannot be written is dropped; stderr is gone.
        let _ = writeln!(
            std::io::stderr().lock(),
            "{seconds:>7.3}s {:<5} {}: {}",
            record.level(),
            record.target(),
            record.args()
        );
    }

    fn flush(&self) {}
}

/// Logs at `level` to stderr, or nothing without one.
pub(crate) fn init(level: Option<LogLevel>) {
    let Some(level) = level else {
        return;
    };
    // The logger lives as long as `lm` does.
    let logger: &'static Stderr = Box::leak(Box::new(Stderr {
        started: Instant::now(),
    }));
    if log::set_logger(logger).is_ok() {
        log::set_max_level(level.filter());
    }
}
