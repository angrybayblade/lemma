//! `lm`'s command line.

use std::path::PathBuf;

use clap::{Parser, Subcommand};

/// Work with `.lm` modules: check them, format them, or serve them to an editor. `lemma check` and `lemma verify`
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
        #[arg(default_value = "mod.lm")]
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
    /// Serve `.lm` modules to an editor over the Language Server Protocol,
    /// on standard input and output.
    Lsp,
    /// Add a declaration to a module from a template.
    New {
        #[command(subcommand)]
        what: New,
    },
}

/// What `lm new` adds.
#[derive(Subcommand)]
pub(crate) enum New {
    /// A behavior for one of the module's operations: a checker that runs
    /// it, the checker's script (Python, TypeScript with Bun, or a stub for
    /// Go) under `tools/`, and a behavior demanding its evidence. With
    /// `--through`, the script replaces the call that link makes.
    Behavior {
        /// The behavior's name, such as `passes_on_retriable`.
        name: String,
        /// The operation to run, as the module names it: `orders.place_order`.
        #[arg(long)]
        handler: String,
        /// A link the operation's code calls through, as a graph of the
        /// module writes it: `"orders.charge -> api.createCharge"`.
        #[arg(long)]
        through: Option<String>,
        /// The module to add it to.
        #[arg(long, default_value = "mod.lm")]
        module: PathBuf,
    },
}

#[cfg(test)]
mod tests {
    use clap::CommandFactory;

    use super::Cli;

    /// The command reference, whose `lm` section must name every command
    /// and option.
    const REFERENCE: &str = include_str!("../../../docs/commands.md");

    /// Each subcommand under `command` (named `path`) and each long option
    /// of it and below that the reference does not mention.
    fn unmentioned(command: &clap::Command, path: &str, missing: &mut Vec<String>) {
        for argument in command.get_arguments() {
            let Some(long) = argument.get_long().filter(|_| !argument.is_hide_set()) else {
                continue;
            };
            let option = format!("--{long}");
            if !["--help", "--version"].contains(&option.as_str()) && !REFERENCE.contains(&option) {
                missing.push(format!("{path} {option}"));
            }
        }
        for subcommand in command.get_subcommands() {
            if subcommand.is_hide_set() || subcommand.get_name() == "help" {
                continue;
            }
            let named = format!("{path} {}", subcommand.get_name());
            if !REFERENCE.contains(&named) {
                missing.push(named.clone());
            }
            unmentioned(subcommand, &named, missing);
        }
    }

    #[test]
    fn every_command_and_option_is_in_the_command_reference() {
        let mut missing = Vec::new();

        unmentioned(&Cli::command(), "lm", &mut missing);

        assert_eq!(
            missing,
            Vec::<String>::new(),
            "missing from docs/commands.md"
        );
    }
}
