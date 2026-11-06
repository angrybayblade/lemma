//! `lm`'s command line.

use std::path::PathBuf;

use clap::{Parser, Subcommand};

/// Work with `.lm` modules: check them, format them, convert a lemma.yaml
/// into one, or serve them to an editor. `lemma check` and `lemma verify`
/// run what they declare.
#[derive(Parser)]
#[command(name = "lm", version)]
pub(crate) struct Cli {
    /// Print results as JSON.
    #[arg(long, global = true)]
    pub json: bool,
    /// Log what `lm` is doing to stderr: `error`, `warn`, `info`, `debug`
    /// or `trace`. Off by default.
    #[arg(short = 'l', long = "logging", global = true, value_name = "LEVEL")]
    pub logging: Option<crate::logging::LogLevel>,
    #[command(subcommand)]
    pub command: Command,
}

#[derive(Subcommand)]
pub(crate) enum Command {
    /// Load a module and everything it imports, and report every problem.
    ///
    /// Exits with 1 if there is an error.
    Check {
        /// The entry module.
        #[arg(default_value = "lemma.lm")]
        file: PathBuf,
    },
    /// Rewrite `.lm` files in the canonical layout.
    ///
    /// Exits with 1 if a file has syntax errors, which leave it unchanged,
    /// or with `--check` if a file is not formatted.
    Fmt {
        /// Files, or directories to search for `.lm` files; the current
        /// directory by default. Hidden directories, `target` and
        /// `node_modules` are not searched.
        paths: Vec<PathBuf>,
        /// List the files that are not formatted instead of rewriting them.
        #[arg(long)]
        check: bool,
    },
    /// Write a lemma.yaml as an `.lm` root module.
    Convert {
        /// The lemma.yaml to convert.
        #[arg(default_value = "lemma.yaml")]
        file: PathBuf,
        /// The module's name, its repository's; the directory's name by
        /// default.
        #[arg(long)]
        module: Option<String>,
        /// Write lemma.lm beside the file instead of printing it.
        #[arg(long)]
        write: bool,
    },
    /// Serve `.lm` modules to an editor over the Language Server Protocol,
    /// on standard input and output.
    Lsp,
}
