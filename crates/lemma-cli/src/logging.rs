//! What Lemma is doing, as log lines on stderr, when `-l/--logging` asks
//! for them; off by default. Every crate logs through the `log` facade, and
//! this is its one implementation, so stdout (text, JSON, the MCP protocol)
//! is never touched.
//!
//! A command the daemon runs logs in the daemon's process: its lines are
//! captured for the length of the command and sent back with its output,
//! marked as the daemon's, so the caller sees them as if it had run the
//! command itself.

use std::fmt::Write as _;
use std::sync::{Mutex, MutexGuard, PoisonError};
use std::time::Instant;

use log::{LevelFilter, Log, Metadata, Record};

/// How much to log, from `-l/--logging`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, clap::ValueEnum)]
pub(crate) enum LogLevel {
    /// Failures only.
    Error,
    /// Failures, and what Lemma worked around (a provider skipped, an
    /// adapter that could not read the code).
    Warn,
    /// Each step and the choices made in it: in this process or the
    /// daemon, repositories and commits read, caches hit, adapters run,
    /// engines and providers used, with timings.
    Info,
    /// Per-repository and per-cache detail, and commands run.
    Debug,
    /// Everything, including each request to a language server.
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

/// The process's logger: prints to stderr, or, while a daemon runs a
/// caller's command, keeps the lines for that caller.
pub(crate) struct Logger {
    state: Mutex<State>,
}

struct State {
    /// What times are measured from: the process start, or the start of
    /// the command being captured.
    started: Instant,
    /// Lines kept for a caller, while a command is captured.
    captured: Option<Vec<String>>,
}

/// Installs the logger for this process, logging at `level` (nothing when
/// `None`). The `log` facade holds one logger per process, so this is the
/// one place Lemma keeps a global; it is returned so the daemon can
/// capture a command's lines with it.
pub(crate) fn init(level: Option<LogLevel>) -> &'static Logger {
    let logger: &'static Logger = Box::leak(Box::new(Logger::new()));
    // Only fails if a logger is already set, which `main` never does twice.
    let _ = log::set_logger(logger);
    log::set_max_level(level.map_or(LevelFilter::Off, LogLevel::filter));
    logger
}

impl Logger {
    /// A logger printing to stderr, timed from now; [`init`] installs one.
    pub(crate) fn new() -> Self {
        Self {
            state: Mutex::new(State {
                started: Instant::now(),
                captured: None,
            }),
        }
    }

    /// Starts keeping the lines a caller's command logs at `level`, timed
    /// from now, until [`Logger::finish_capture`].
    pub(crate) fn start_capture(&self, level: Option<LogLevel>) {
        let mut state = self.lock();
        state.started = Instant::now();
        state.captured = Some(Vec::new());
        log::set_max_level(level.map_or(LevelFilter::Off, LogLevel::filter));
    }

    /// The lines kept since [`Logger::start_capture`], each ending in a
    /// newline; logging goes back to `level` for the process itself.
    pub(crate) fn finish_capture(&self, level: Option<LogLevel>) -> String {
        let lines = self.lock().captured.take().unwrap_or_default();
        log::set_max_level(level.map_or(LevelFilter::Off, LogLevel::filter));
        let mut text = String::new();
        for line in lines {
            let _ = writeln!(text, "{line}");
        }
        text
    }

    fn lock(&self) -> MutexGuard<'_, State> {
        self.state.lock().unwrap_or_else(PoisonError::into_inner)
    }
}

impl Log for Logger {
    fn enabled(&self, metadata: &Metadata<'_>) -> bool {
        // Lemma's own crates only: libraries it uses log too, at a detail
        // that says nothing about what Lemma decided.
        metadata.level() <= log::max_level() && metadata.target().starts_with("lemma")
    }

    fn log(&self, record: &Record<'_>) {
        if !self.enabled(record.metadata()) {
            return;
        }
        let mut state = self.lock();
        let elapsed = state.started.elapsed().as_secs_f64();
        let is_captured = state.captured.is_some();
        let line = format_line(
            elapsed,
            record.level(),
            record.target(),
            is_captured,
            &record.args().to_string(),
        );
        match &mut state.captured {
            Some(lines) => lines.push(line),
            None => eprintln!("{line}"),
        }
    }

    fn flush(&self) {}
}

/// One log line: seconds since the start, the level, where it came from
/// (`graph::check` for `lemma_graph::check`, `cli::check` for the binary's
/// `lemma::check`), marked `daemon` when the daemon logged it for a caller,
/// then the message.
fn format_line(
    elapsed_secs: f64,
    level: log::Level,
    target: &str,
    is_daemon: bool,
    message: &str,
) -> String {
    let source = source_name(target);
    let marker = if is_daemon { "daemon " } else { "" };
    format!("{elapsed_secs:>8.3}s {level:<5} {marker}{source}: {message}")
}

/// Where a line came from, without the `lemma` every target starts with.
fn source_name(target: &str) -> String {
    if let Some(library) = target.strip_prefix("lemma_") {
        return library.to_owned();
    }
    match target.strip_prefix("lemma::") {
        Some(module) => format!("cli::{module}"),
        None if target == "lemma" => "cli".to_owned(),
        None => target.to_owned(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_line_says_when_where_and_whether_the_daemon_logged_it() {
        let line = format_line(
            1.5,
            log::Level::Info,
            "lemma_graph::check",
            true,
            "read 6 repositories",
        );

        assert_eq!(
            line,
            "   1.500s INFO  daemon graph::check: read 6 repositories"
        );
    }

    fn record(level: log::Level, message: &str, logger: &Logger) {
        logger.log(
            &Record::builder()
                .level(level)
                .target("lemma_graph::check")
                .args(format_args!("{message}"))
                .build(),
        );
    }

    #[test]
    fn a_captured_command_keeps_its_lines_at_its_level() {
        let logger = Logger::new();

        logger.start_capture(Some(LogLevel::Info));
        record(log::Level::Info, "kept", &logger);
        record(log::Level::Debug, "too detailed", &logger);
        let captured = logger.finish_capture(None);

        assert_eq!(captured.lines().count(), 1);
        assert!(captured.ends_with("INFO  daemon graph::check: kept\n"));
    }

    #[test]
    fn a_line_from_this_process_has_no_marker() {
        let line = format_line(0.0, log::Level::Warn, "lemma", false, "no daemon");

        assert_eq!(line, "   0.000s WARN  cli: no daemon");
    }
}
