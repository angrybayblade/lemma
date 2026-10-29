//! Watching what a server is doing, to tell when it is ready to answer.
//!
//! LSP has no "analysis finished" signal. Some servers answer requests before
//! they have loaded the workspace and return empty results that look like
//! real answers. These signals, taken together, are the best available hint.

use std::collections::HashMap;
use std::time::{Duration, Instant};

use serde_json::Value;

/// How long to wait for a configured ready signal before falling back to
/// the quiet-period heuristic.
const READY_SIGNAL_GRACE: Duration = Duration::from_secs(10);

/// Why a wait for readiness ended.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Readiness {
    /// The server logged a message configured as its ready signal, such as
    /// pyright's "Found 1234 source files".
    Announced,
    /// The server published diagnostics for the document, so it has analyzed
    /// it in its real project.
    DiagnosticsPublished,
    /// The server sent nothing for the quiet period and reported no work in
    /// progress.
    Quiet,
    /// Neither happened before the limit. Answers may be incomplete.
    TimedOut,
}

/// Recent server activity, updated by the reader thread.
#[derive(Debug)]
pub(crate) struct Activity {
    last_message_at: Instant,
    /// Titles of work-done progress reports that have begun but not ended,
    /// by token.
    active_progress: HashMap<String, String>,
    /// When diagnostics were last published, by document URI.
    diagnostics_published_at: HashMap<String, Instant>,
    /// Patterns for log messages, or titles of finished progress reports,
    /// that mean the server is ready. When set, they replace the diagnostics
    /// and quiet-period heuristics.
    ready_messages: Vec<String>,
    /// Whether a ready message has been seen.
    has_announced_ready: bool,
}

impl Activity {
    pub fn new(now: Instant, ready_messages: Vec<String>) -> Self {
        Self {
            last_message_at: now,
            active_progress: HashMap::new(),
            diagnostics_published_at: HashMap::new(),
            ready_messages,
            has_announced_ready: false,
        }
    }

    /// Records any message from the server, noting progress and diagnostics.
    pub fn record(&mut self, method: Option<&str>, params: &Value, now: Instant) {
        self.last_message_at = now;
        match method {
            Some("$/progress") => self.record_progress(params),
            Some("textDocument/publishDiagnostics") => {
                if let Some(uri) = params.get("uri").and_then(Value::as_str) {
                    self.diagnostics_published_at.insert(uri.to_owned(), now);
                }
            }
            Some("window/logMessage") => self.record_log(params),
            _ => {}
        }
    }

    fn record_log(&mut self, params: &Value) {
        let Some(message) = params.get("message").and_then(Value::as_str) else {
            return;
        };
        self.has_announced_ready |= self.is_ready_message(message);
    }

    fn is_ready_message(&self, text: &str) -> bool {
        self.ready_messages
            .iter()
            .any(|pattern| matches_pattern(pattern, text))
    }

    /// Treats `now` as activity, so a quiet period cannot end before a wait
    /// has started.
    pub fn touch(&mut self, now: Instant) {
        self.last_message_at = self.last_message_at.max(now);
    }

    /// Whether the server looks ready to answer about `uri`, opened at
    /// `opened_at`. `None` means keep waiting.
    pub fn readiness(
        &self,
        uri: &str,
        opened_at: Instant,
        quiet_period: Duration,
        now: Instant,
    ) -> Option<Readiness> {
        if !self.ready_messages.is_empty() {
            return self.announced_readiness(opened_at, quiet_period, now);
        }
        let published_since_open = self
            .diagnostics_published_at
            .get(uri)
            .is_some_and(|published_at| *published_at >= opened_at);
        if published_since_open {
            return Some(Readiness::DiagnosticsPublished);
        }
        let is_quiet = now.duration_since(self.last_message_at) >= quiet_period;
        if is_quiet && self.active_progress.is_empty() {
            return Some(Readiness::Quiet);
        }
        None
    }

    /// Readiness for a server with a configured ready signal. If the signal
    /// never comes, perhaps because a newer server words it differently, a
    /// long quiet spell eventually counts instead of waiting for the timeout.
    fn announced_readiness(
        &self,
        opened_at: Instant,
        quiet_period: Duration,
        now: Instant,
    ) -> Option<Readiness> {
        if self.has_announced_ready {
            return Some(Readiness::Announced);
        }
        let is_past_grace = now.duration_since(opened_at) >= READY_SIGNAL_GRACE;
        let is_quiet = now.duration_since(self.last_message_at) >= quiet_period;
        (is_past_grace && is_quiet && self.active_progress.is_empty()).then_some(Readiness::Quiet)
    }

    /// How long until a quiet period could end, for sleeping between checks.
    pub fn time_until_quiet(&self, quiet_period: Duration, now: Instant) -> Duration {
        quiet_period.saturating_sub(now.duration_since(self.last_message_at))
    }

    fn record_progress(&mut self, params: &Value) {
        let Some(token) = params.get("token").map(token_key) else {
            return;
        };
        match params.pointer("/value/kind").and_then(Value::as_str) {
            Some("begin") => {
                let title = params.pointer("/value/title").and_then(Value::as_str);
                self.active_progress
                    .insert(token, title.unwrap_or_default().to_owned());
            }
            Some("end") => {
                let title = self.active_progress.remove(&token).unwrap_or_default();
                self.has_announced_ready |= self.is_ready_message(&title);
            }
            _ => {}
        }
    }
}

/// Whether `text` matches `pattern`, where `*` stands for any run of
/// characters and everything else is literal.
fn matches_pattern(pattern: &str, text: &str) -> bool {
    let mut parts = pattern.split('*');
    let Some(first) = parts.next() else {
        return false;
    };
    let Some(mut rest) = text.strip_prefix(first) else {
        return false;
    };
    let mut parts = parts.peekable();
    while let Some(part) = parts.next() {
        if parts.peek().is_none() {
            return rest.ends_with(part);
        }
        let Some(found_at) = rest.find(part) else {
            return false;
        };
        rest = &rest[found_at + part.len()..];
    }
    rest.is_empty()
}

/// Progress tokens may be strings or numbers; both become map keys.
fn token_key(token: &Value) -> String {
    token
        .as_str()
        .map_or_else(|| token.to_string(), str::to_owned)
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    const QUIET: Duration = Duration::from_millis(500);
    const URI: &str = "file:///repo/a.py";

    fn later(start: Instant, millis: u64) -> Instant {
        start + Duration::from_millis(millis)
    }

    #[test]
    fn diagnostics_after_opening_mean_ready() {
        let start = Instant::now();
        let mut activity = Activity::new(start, Vec::new());

        activity.record(
            Some("textDocument/publishDiagnostics"),
            &json!({ "uri": URI, "diagnostics": [] }),
            later(start, 100),
        );

        assert_eq!(
            activity.readiness(URI, start, QUIET, later(start, 100)),
            Some(Readiness::DiagnosticsPublished)
        );
    }

    #[test]
    fn diagnostics_from_before_opening_do_not_count() {
        let start = Instant::now();
        let mut activity = Activity::new(start, Vec::new());
        activity.record(
            Some("textDocument/publishDiagnostics"),
            &json!({ "uri": URI }),
            start,
        );

        let opened_at = later(start, 50);
        assert_eq!(
            activity.readiness(URI, opened_at, QUIET, later(start, 100)),
            None
        );
    }

    #[test]
    fn silence_without_progress_means_ready() {
        let start = Instant::now();
        let activity = Activity::new(start, Vec::new());

        assert_eq!(
            activity.readiness(URI, start, QUIET, later(start, 499)),
            None
        );
        assert_eq!(
            activity.readiness(URI, start, QUIET, later(start, 500)),
            Some(Readiness::Quiet)
        );
    }

    #[test]
    fn work_in_progress_holds_off_quiet_readiness() {
        let start = Instant::now();
        let mut activity = Activity::new(start, Vec::new());
        let begin = json!({ "token": 7, "value": { "kind": "begin", "title": "Indexing" } });
        let end = json!({ "token": 7, "value": { "kind": "end" } });

        activity.record(Some("$/progress"), &begin, start);
        assert_eq!(
            activity.readiness(URI, start, QUIET, later(start, 2_000)),
            None
        );

        activity.record(Some("$/progress"), &end, later(start, 2_000));
        assert_eq!(
            activity.readiness(URI, start, QUIET, later(start, 2_500)),
            Some(Readiness::Quiet)
        );
    }

    #[test]
    fn touching_restarts_the_quiet_period() {
        let start = Instant::now();
        let mut activity = Activity::new(start, Vec::new());

        activity.touch(later(start, 1_000));

        assert_eq!(
            activity.readiness(URI, start, QUIET, later(start, 1_200)),
            None
        );
    }

    #[test]
    fn patterns_match_literal_text_around_wildcards() {
        assert!(matches_pattern(
            "Found * source file*",
            "Found 79506 source files"
        ));
        assert!(matches_pattern(
            "Found * source file*",
            "Found 1 source file"
        ));
        assert!(matches_pattern(
            "No source files found*",
            "No source files found."
        ));
        assert!(!matches_pattern(
            "Found * source file*",
            "Searching for source files"
        ));
        assert!(!matches_pattern(
            "Found * source file*",
            "Found path /usr/lib"
        ));
        assert!(matches_pattern("exact", "exact"));
        assert!(!matches_pattern("exact", "exactly"));
    }

    #[test]
    fn a_ready_message_replaces_the_heuristics() {
        let start = Instant::now();
        let ready_messages = vec!["Found * source file*".to_owned()];
        let mut activity = Activity::new(start, ready_messages);
        activity.record(
            Some("textDocument/publishDiagnostics"),
            &json!({ "uri": URI, "diagnostics": [] }),
            later(start, 100),
        );

        assert_eq!(
            activity.readiness(URI, start, QUIET, later(start, 900)),
            None
        );

        activity.record(
            Some("window/logMessage"),
            &json!({ "type": 3, "message": "Found 12 source files" }),
            later(start, 1_000),
        );

        assert_eq!(
            activity.readiness(URI, start, QUIET, later(start, 1_000)),
            Some(Readiness::Announced)
        );
    }

    #[test]
    fn a_finished_progress_report_can_be_the_ready_signal() {
        let start = Instant::now();
        let ready_messages = vec!["Initializing JS/TS language features*".to_owned()];
        let mut activity = Activity::new(start, ready_messages);
        let begin = json!({
            "token": "t1",
            "value": { "kind": "begin", "title": "Initializing JS/TS language features…" }
        });
        activity.record(Some("$/progress"), &begin, later(start, 700));

        assert_eq!(
            activity.readiness(URI, start, QUIET, later(start, 1_400)),
            None
        );

        let end = json!({ "token": "t1", "value": { "kind": "end" } });
        activity.record(Some("$/progress"), &end, later(start, 2_000));

        assert_eq!(
            activity.readiness(URI, start, QUIET, later(start, 2_000)),
            Some(Readiness::Announced)
        );
    }

    #[test]
    fn a_missing_ready_signal_falls_back_to_quiet_after_the_grace_period() {
        let start = Instant::now();
        let activity = Activity::new(start, vec!["never logged".to_owned()]);

        assert_eq!(
            activity.readiness(URI, start, QUIET, later(start, 5_000)),
            None
        );
        assert_eq!(
            activity.readiness(URI, start, QUIET, later(start, 10_000)),
            Some(Readiness::Quiet)
        );
    }
}
