//! `lm`: the command for `.lm` modules. It checks them, formats them and
//! serves them to editors. Running
//! what modules declare (checks, verification, behaviors) is `lemma`'s.
//!
//! The `lm` binary is built by `lemma-cli`, beside `lemma`, and calls
//! [`run`]. Every command runs in its process; none needs Lemma's daemon or
//! index.

mod check;
mod cli;
mod format;
mod logging;
mod new;
mod scaffold;

use std::process::ExitCode;

use clap::Parser;
use lemma_graph::repos::cache_directory;

use crate::cli::{Cli, Command, New};

/// Runs `lm` with the process's arguments; its exit code.
pub fn run() -> ExitCode {
    let cli = Cli::parse();
    logging::init(cli.logging);
    log::info!("lm {}", env!("CARGO_PKG_VERSION"));
    let outcome = match &cli.command {
        Command::Check { file } => check::run(file, cli.json),
        Command::Fmt { paths, check } => format::run(paths, *check),
        Command::Lsp => serve().map(|()| ExitCode::SUCCESS),
        Command::New {
            what:
                New::Behavior {
                    name,
                    handler,
                    through,
                    module,
                },
        } => new::run(&new::Request {
            name,
            handler,
            through: through.as_deref(),
            module,
        }),
    };
    match outcome {
        Ok(code) => code,
        Err(error) => {
            eprintln!("error: {error:#}");
            ExitCode::FAILURE
        }
    }
}

/// Serves an editor on standard input and output until it exits.
fn serve() -> anyhow::Result<()> {
    let input = std::io::BufReader::new(std::io::stdin());
    let stdout = std::io::stdout();
    lemma_lang_server::serve(input, &mut stdout.lock(), &cache_directory())?;
    Ok(())
}
