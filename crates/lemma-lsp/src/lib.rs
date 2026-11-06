//! A small Language Server Protocol client that talks to servers over stdio.
//!
//! It owns the parts every server shares: framing, request ids, the
//! initialize handshake, timeouts and cancellation, answering the server's own
//! requests, and shutting the process down. What to ask and how to interpret
//! the answers belongs to provider adapters.

mod activity;
mod client;
mod encoding;
mod message;
mod process;
mod transport;

use std::path::PathBuf;
use std::time::Duration;

pub use activity::Readiness;
pub use client::{LaunchConfig, LspClient, ServerDetails};
pub use encoding::{PositionEncoding, line_text};
pub use transport::{read_message, write_message};

/// Resident memory, in bytes, of each server process and the processes it
/// started, by process id. Servers that are not running are left out.
pub fn memory_bytes(process_ids: &[u32]) -> std::collections::HashMap<u32, u64> {
    process::tree_memory_bytes(process_ids)
}
pub use lsp_types;

/// Why talking to a language server failed.
#[derive(Debug, thiserror::Error)]
pub enum LspError {
    /// The server executable could not be started.
    #[error("could not start `{command}`")]
    Spawn {
        /// The command that was run.
        command: String,
        /// Why the operating system refused.
        #[source]
        source: std::io::Error,
    },
    /// The workspace root cannot be turned into a `file://` URI.
    #[error("workspace root {} cannot be expressed as a file URI", path.display())]
    InvalidRoot {
        /// The offending path.
        path: PathBuf,
    },
    /// A thread for reading server output could not be started.
    #[error("could not start the language server reader thread")]
    Thread(#[source] std::io::Error),
    /// Writing to the server failed, usually because it exited.
    #[error("could not write to the language server")]
    Write(#[source] std::io::Error),
    /// The server exited or closed its output before answering.
    #[error("the language server closed the connection")]
    Closed,
    /// The server exited while starting, saying why on its error output,
    /// such as an interpreter its script names that is not installed.
    #[error("the language server exited while starting: {said}")]
    Exited {
        /// The end of what it wrote to its error output.
        said: String,
    },
    /// The server did not answer in time. The request has been cancelled.
    #[error("{method} timed out after {} seconds", timeout.as_secs())]
    Timeout {
        /// The request method.
        method: &'static str,
        /// How long the client waited.
        timeout: Duration,
    },
    /// The server answered with an error.
    #[error("{method} failed: {message} (code {code})")]
    Server {
        /// The request method.
        method: &'static str,
        /// The JSON-RPC error code.
        code: i64,
        /// The server's explanation.
        message: String,
    },
    /// Request parameters could not be turned into JSON.
    #[error("{method} parameters could not be encoded")]
    InvalidRequest {
        /// The request method.
        method: &'static str,
        /// What could not be encoded.
        #[source]
        source: serde_json::Error,
    },
    /// The server's answer did not have the expected shape.
    #[error("{method} returned a response that could not be read")]
    InvalidResponse {
        /// The request method.
        method: &'static str,
        /// What did not match.
        #[source]
        source: serde_json::Error,
    },
}
