//! Zed extension for Lemma's `.lm` language.
//!
//! Highlighting, brackets, indentation and the outline come from the queries
//! in `languages/lemma`. Zed starts a language server only through extension
//! code, so this crate's one job is to tell Zed how to run `lemma lm lsp`.

use zed_extension_api::{self as zed, LanguageServerId, Result};

/// The extension. It keeps no state: the command depends only on the
/// worktree it is asked about.
struct LemmaExtension;

impl zed::Extension for LemmaExtension {
    fn new() -> Self {
        LemmaExtension
    }

    /// Runs the `lemma` found on the worktree's `PATH` as `lemma lm lsp`, with
    /// the worktree's shell environment, so a `lemma` installed by a version
    /// manager or a project's shell setup is found the way a terminal finds
    /// it.
    fn language_server_command(
        &mut self,
        _language_server_id: &LanguageServerId,
        worktree: &zed::Worktree,
    ) -> Result<zed::Command> {
        let Some(lemma) = worktree.which("lemma") else {
            return Err(
                "`lemma` is not on PATH; install it to get diagnostics, hover and go-to-definition"
                    .to_owned(),
            );
        };
        Ok(zed::Command {
            command: lemma,
            args: vec!["lm".to_owned(), "lsp".to_owned()],
            env: worktree.shell_env(),
        })
    }
}

// The macro defines the entry point Zed calls, exported by name as
// `init-extension`. A private module keeps that undocumented function out of
// the crate's public items without changing the export.
mod entry {
    use super::{LemmaExtension, zed};

    zed::register_extension!(LemmaExtension);
}
