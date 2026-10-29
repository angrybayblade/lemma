//! The `lemma` command-line tool.
//!
//! Commands run in a background daemon that keeps the index open and
//! language servers warm, so the command itself only forwards its arguments.
//! With `--no-daemon`, `LEMMA_NO_DAEMON=1`, on platforms without Unix
//! sockets, or if no daemon can be started, the command runs in-process.

mod cli;
#[cfg(unix)]
mod config;
#[cfg(unix)]
mod daemon;
mod engine;
mod output;

use std::io::Write;
use std::process::ExitCode;

use clap::Parser;

use crate::cli::{Cli, Command};
use crate::engine::{Engine, Invocation};

fn main() -> ExitCode {
    let cli = Cli::parse();
    if let Command::Daemon(args) = &cli.command {
        return run_daemon(args);
    }
    let Ok(cwd) = std::env::current_dir() else {
        eprintln!("error: could not read the current directory");
        return ExitCode::FAILURE;
    };
    if !cli.no_daemon && !daemon_disabled_by_environment() {
        if let Some(code) = forward(cwd.clone()) {
            return code;
        }
    }
    run_in_process(cli, cwd)
}

fn daemon_disabled_by_environment() -> bool {
    std::env::var_os("LEMMA_NO_DAEMON").is_some_and(|value| value != "0")
}

fn run_in_process(cli: Cli, cwd: std::path::PathBuf) -> ExitCode {
    let invocation = Invocation {
        cwd,
        path_env: None,
    };
    let mut engine = Engine::one_shot();
    let mut stdout = std::io::stdout().lock();
    let result = engine.execute(cli, &invocation, &mut stdout);
    engine.shutdown();
    match result {
        Ok(code) => ExitCode::from(code),
        Err(error) => {
            eprintln!("error: {error:#}");
            ExitCode::FAILURE
        }
    }
}

#[cfg(unix)]
fn forward(cwd: std::path::PathBuf) -> Option<ExitCode> {
    let args = std::env::args().skip(1).collect();
    let finished = daemon::forward(args, cwd)?;
    // Output that cannot be written (a closed pipe) is not worth reporting.
    let _ = std::io::stdout().write_all(finished.stdout.as_bytes());
    let _ = std::io::stderr().write_all(finished.stderr.as_bytes());
    Some(ExitCode::from(finished.exit_code))
}

#[cfg(not(unix))]
fn forward(_cwd: std::path::PathBuf) -> Option<ExitCode> {
    None
}

#[cfg(unix)]
fn run_daemon(args: &cli::DaemonArgs) -> ExitCode {
    let result = if args.stop {
        daemon::stop().map(|was_running| {
            if !was_running {
                eprintln!("no lemma daemon is running");
            }
        })
    } else if args.status {
        daemon::status().map(|status| match status {
            Some(text) => print!("{text}"),
            None => eprintln!("no lemma daemon is running"),
        })
    } else {
        daemon::serve(args.idle_secs.map(std::time::Duration::from_secs))
    };
    match result {
        Ok(()) => ExitCode::SUCCESS,
        Err(error) => {
            eprintln!("error: {error:#}");
            ExitCode::FAILURE
        }
    }
}

#[cfg(not(unix))]
fn run_daemon(_args: &cli::DaemonArgs) -> ExitCode {
    eprintln!("error: the lemma daemon needs Unix sockets");
    ExitCode::FAILURE
}
