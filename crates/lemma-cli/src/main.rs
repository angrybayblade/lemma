//! The `lemma` command-line tool.
//!
//! Commands run in a background daemon that keeps the index open and
//! language servers warm, so the command itself only forwards its arguments.
//! With `--no-daemon`, `LEMMA_NO_DAEMON=1`, on platforms without Unix
//! sockets, or if no daemon can be started, the command runs in-process.

mod addresses;
mod anchors;
mod check;
mod cli;
#[cfg(unix)]
mod config;
#[cfg(unix)]
mod daemon;
mod duration;
mod engine;
mod mcp;
mod output;
mod selection;
#[cfg(unix)]
mod service;
mod verify;

use std::io::Write;
use std::process::ExitCode;

use clap::Parser;

#[cfg(unix)]
use crate::cli::DaemonAction;
use crate::cli::{Cli, Command};
use crate::engine::{Engine, Invocation};

fn main() -> ExitCode {
    let cli = Cli::parse();
    if let Command::Daemon(args) = &cli.command {
        return run_daemon(args);
    }
    if let Command::Verify(args) = &cli.command {
        return verify::run(args, cli.json);
    }
    if let Command::Check(args) = &cli.command {
        let lsp = check::LanguageServers {
            asks_daemon: !cli.no_daemon && !daemon_disabled_by_environment(),
            providers_file: cli.providers.clone(),
        };
        return check::run(args, &lsp, cli.json);
    }
    if matches!(cli.command, Command::Mcp) {
        return run_mcp(!cli.no_daemon && !daemon_disabled_by_environment());
    }
    let Ok(cwd) = std::env::current_dir() else {
        eprintln!("error: could not read the current directory");
        return ExitCode::FAILURE;
    };
    // Tracing through a workspace runs inference over every repository,
    // which runs in its own process, as `lemma check` does.
    let traces_workspace = matches!(&cli.command, Command::Graph(args) if args.workspace.is_some());
    if !cli.no_daemon && !daemon_disabled_by_environment() && !traces_workspace {
        if let Some(code) = forward(cwd.clone()) {
            return code;
        }
    }
    run_in_process(cli, cwd)
}

fn run_mcp(uses_daemon: bool) -> ExitCode {
    match mcp::serve(uses_daemon) {
        Ok(()) => ExitCode::SUCCESS,
        Err(error) => {
            eprintln!("error: {error:#}");
            ExitCode::FAILURE
        }
    }
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
    let result = match args.action {
        // A daemon started on demand would otherwise keep the socket.
        Some(DaemonAction::Install) => daemon::stop()
            .and_then(|_| service::install())
            .map(|done| println!("{done}")),
        Some(DaemonAction::Uninstall) => service::uninstall().map(|done| println!("{done}")),
        Some(DaemonAction::Stop) => daemon::stop().map(|was_running| {
            if !was_running {
                eprintln!("no lemma daemon is running");
            }
        }),
        Some(DaemonAction::Status) => daemon::status().map(|status| {
            println!("{}", service::describe());
            match status {
                Some(text) => print!("{text}"),
                None => println!("no lemma daemon is running"),
            }
        }),
        None => daemon::serve(args.idle_secs.map(std::time::Duration::from_secs)),
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
