//! Whether a command may clone a `git(...)` repository it does not have, and
//! asking at a terminal when the setting says to.

use std::io::{BufRead, IsTerminal, Write};
use std::path::{Path, PathBuf};

use lemma_graph::compose::composition;
use lemma_graph::repos::{Cloning, UnavailableRepository, cache_directory, clone_remote};

use crate::cli::{Cli, Command};
use crate::config::{ClonePolicy, clone_policy};

/// Whether a command may clone, given `--no-clone` and the setting for a
/// description in `directory`. Asking happens before this, in the client,
/// so by now `ask` means yes.
pub(crate) fn permitted(no_clone: bool, directory: &Path) -> anyhow::Result<Cloning> {
    if no_clone || clone_policy(directory)? == ClonePolicy::Never {
        return Ok(Cloning::Forbidden);
    }
    Ok(Cloning::Allowed)
}

/// Asks about repositories `lemma index` or `lemma check` would clone, when
/// the setting says to and there is someone to ask. Whether the answer was
/// no. It runs in the client because the daemon has no terminal.
pub(crate) fn ask_before_cloning(cli: &mut Cli) -> bool {
    let Some((entry, directory)) = entry_of(&cli.command) else {
        return false;
    };
    let is_interactive = std::io::stdin().is_terminal() && std::io::stderr().is_terminal();
    let asks = matches!(clone_policy(&directory), Ok(ClonePolicy::Ask));
    if !is_interactive || !asks {
        return false;
    }
    let is_declined = !confirm_and_clone(&entry);
    if is_declined {
        decline(&mut cli.command);
    }
    is_declined
}

/// The description a command reads and its directory, for the commands
/// that may clone. `None` if it forbids cloning, or there is no description.
fn entry_of(command: &Command) -> Option<(PathBuf, PathBuf)> {
    match command {
        Command::Index { path, no_clone, .. } if !no_clone => {
            let (root, description) = crate::selection::locate(path).ok()?;
            Some((description?, root))
        }
        Command::Check(args) if !args.no_clone => {
            let entry = args.manifest.clone();
            let directory = entry.parent().map_or_else(PathBuf::new, Path::to_path_buf);
            Some((entry, directory))
        }
        _ => None,
    }
}

fn decline(command: &mut Command) {
    match command {
        Command::Index { no_clone, .. } => *no_clone = true,
        Command::Check(args) => args.no_clone = true,
        _ => {}
    }
}

/// Clones what `entry` needs if the user agrees, repeating because a cloned
/// repository can name others. Whether every answer was yes. A description
/// that does not load is left for the command to report.
fn confirm_and_clone(entry: &Path) -> bool {
    let cache = cache_directory();
    loop {
        let Ok(found) = composition(entry, &cache, Cloning::Forbidden) else {
            return true;
        };
        if found.unavailable.is_empty() {
            return true;
        }
        if !answer_is_yes(&found.unavailable) {
            return false;
        }
        for missing in &found.unavailable {
            // A failed clone is reported by the command, which tries again.
            if clone_remote(&missing.url, missing.reference.as_deref(), &cache).is_err() {
                return true;
            }
        }
    }
}

fn answer_is_yes(missing: &[UnavailableRepository]) -> bool {
    let mut stderr = std::io::stderr().lock();
    for repository in missing {
        let _ = writeln!(
            stderr,
            "{} is not cloned ({})",
            repository.name, repository.url
        );
    }
    let _ = write!(stderr, "Clone now? [Y/n] ");
    let _ = stderr.flush();
    let mut answer = String::new();
    if std::io::stdin().lock().read_line(&mut answer).is_err() {
        return false;
    }
    is_yes(&answer)
}

/// Whether a typed answer agrees. Enter alone does, since cloning is the
/// default. A closed terminal, which reads as no text at all, does not.
fn is_yes(answer: &str) -> bool {
    if answer.is_empty() {
        return false;
    }
    matches!(answer.trim().to_lowercase().as_str(), "" | "y" | "yes")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn enter_and_yes_agree_to_clone() {
        assert!(is_yes("\n"));
        assert!(is_yes("Y\n"));
        assert!(is_yes("yes\n"));
    }

    #[test]
    fn anything_else_declines_to_clone() {
        assert!(!is_yes("n\n"));
        assert!(!is_yes("later\n"));
    }

    #[test]
    fn a_closed_terminal_declines_to_clone() {
        assert!(!is_yes(""));
    }

    #[test]
    fn no_clone_forbids_cloning_whatever_the_setting() {
        let directory = tempfile::tempdir().unwrap();

        let cloning = permitted(true, directory.path()).unwrap();

        assert_eq!(cloning, Cloning::Forbidden);
    }

    #[test]
    fn a_local_setting_of_never_forbids_cloning() {
        let directory = tempfile::tempdir().unwrap();
        std::fs::create_dir(directory.path().join(".lemma")).unwrap();
        std::fs::write(
            directory.path().join(".lemma/config.toml"),
            "[repositories]\nclone = \"never\"\n",
        )
        .unwrap();

        let cloning = permitted(false, directory.path()).unwrap();

        assert_eq!(cloning, Cloning::Forbidden);
    }
}
