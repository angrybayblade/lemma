//! Watching what a server is doing, to tell when it is ready to answer.
//!
//! LSP has no "analysis finished" signal. Some servers answer requests before
//! they have loaded the workspace and return empty results that look like
//! real answers. These signals, taken together, are the best available hint.

use std::collections::{HashMap, HashSet};
use std::time::{Duration, Instant};

use serde_json::Value;

/// Why a wait for readiness ended.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Readiness {
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
    /// Work-done progress tokens that have begun but not ended.
    active_progress: HashSet<String>,
    /// When diagnostics were last published, by document URI.
    diagnostics_published_at: HashMap<String, Instant>,
}

impl Activity {
    pub fn new(now: Instant) -> Self {
        Self {
            last_message_at: now,
            active_progress: HashSet::new(),
            diagnostics_published_at: HashMap::new(),
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
            _ => {}
        }
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
                self.active_progress.insert(token);
            }
            Some("end") => {
                self.active_progress.remove(&token);
            }
            _ => {}
        }
    }
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
        let mut activity = Activity::new(start);

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
        let mut activity = Activity::new(start);
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
        let activity = Activity::new(start);

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
        let mut activity = Activity::new(start);
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
        let mut activity = Activity::new(start);

        activity.touch(later(start, 1_000));

        assert_eq!(
            activity.readiness(URI, start, QUIET, later(start, 1_200)),
            None
        );
    }
}
