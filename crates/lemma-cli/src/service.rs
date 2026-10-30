//! Running the daemon as a login service, so it is already up when an agent
//! asks its first question.
//!
//! macOS uses a launchd agent and Linux a systemd user unit. Both start the
//! daemon at login and restart it only after a crash, so `lemma daemon
//! stop`, the configured `idle_exit`, and upgrades still stop it normally;
//! the next command starts it again either way.

use std::fs;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};

use anyhow::{Context, bail};

/// launchd label, also the plist's file name.
const LAUNCHD_LABEL: &str = "dev.lemma.daemon";
/// systemd unit name.
const SYSTEMD_UNIT: &str = "lemma.service";

/// The service manager on this platform.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Manager {
    Launchd,
    Systemd,
}

impl Manager {
    fn current() -> anyhow::Result<Self> {
        if cfg!(target_os = "macos") {
            return Ok(Self::Launchd);
        }
        if cfg!(target_os = "linux") {
            return Ok(Self::Systemd);
        }
        bail!("running the daemon as a service is supported on macOS and Linux")
    }

    fn name(self) -> &'static str {
        match self {
            Self::Launchd => "launchd",
            Self::Systemd => "systemd",
        }
    }

    fn file(self) -> anyhow::Result<PathBuf> {
        let home = PathBuf::from(std::env::var_os("HOME").context("HOME is not set")?);
        Ok(match self {
            Self::Launchd => home
                .join("Library/LaunchAgents")
                .join(format!("{LAUNCHD_LABEL}.plist")),
            Self::Systemd => {
                let config = std::env::var_os("XDG_CONFIG_HOME")
                    .map_or_else(|| home.join(".config"), PathBuf::from);
                config.join("systemd/user").join(SYSTEMD_UNIT)
            }
        })
    }
}

/// Installs and starts the service. Returns a line describing what was done.
pub(crate) fn install() -> anyhow::Result<String> {
    let manager = Manager::current()?;
    let executable = std::env::current_exe().context("could not find the lemma executable")?;
    let file = manager.file()?;
    let contents = match manager {
        Manager::Launchd => launchd_plist(&executable),
        Manager::Systemd => systemd_unit(&executable),
    };
    if let Some(parent) = file.parent() {
        fs::create_dir_all(parent)
            .with_context(|| format!("could not create {}", parent.display()))?;
    }
    fs::write(&file, contents).with_context(|| format!("could not write {}", file.display()))?;
    match manager {
        Manager::Launchd => {
            // Replace an earlier copy; failing to remove one that is not
            // loaded is expected.
            let _ = run("launchctl", &["bootout", &launchd_target()?]);
            run(
                "launchctl",
                &["bootstrap", &launchd_domain()?, &file.display().to_string()],
            )?;
        }
        Manager::Systemd => {
            run("systemctl", &["--user", "daemon-reload"])?;
            run("systemctl", &["--user", "enable", "--now", SYSTEMD_UNIT])?;
        }
    }
    Ok(format!(
        "installed the lemma daemon as a {} service ({}); it starts at login",
        manager.name(),
        file.display()
    ))
}

/// Stops and removes the service. Returns a line describing what was done.
pub(crate) fn uninstall() -> anyhow::Result<String> {
    let manager = Manager::current()?;
    let file = manager.file()?;
    if !file.exists() {
        return Ok(format!(
            "no lemma service is installed ({} not found)",
            file.display()
        ));
    }
    match manager {
        Manager::Launchd => {
            let _ = run("launchctl", &["bootout", &launchd_target()?]);
        }
        Manager::Systemd => {
            let _ = run("systemctl", &["--user", "disable", "--now", SYSTEMD_UNIT]);
        }
    }
    fs::remove_file(&file).with_context(|| format!("could not remove {}", file.display()))?;
    if manager == Manager::Systemd {
        let _ = run("systemctl", &["--user", "daemon-reload"]);
    }
    Ok(format!("removed the lemma {} service", manager.name()))
}

/// Whether the service is installed, for status reports.
pub(crate) fn describe() -> String {
    let Ok(manager) = Manager::current() else {
        return "service: not supported on this platform".to_owned();
    };
    match manager.file() {
        Ok(file) if file.exists() => format!(
            "service: installed ({}, {})",
            manager.name(),
            file.display()
        ),
        _ => "service: not installed; `lemma daemon install` starts the daemon at login".to_owned(),
    }
}

/// A launchd agent that runs the daemon at login and restarts it only if
/// it crashes.
fn launchd_plist(executable: &Path) -> String {
    let executable = xml_escape(&executable.display().to_string());
    format!(
        r#"<?xml version="1.0" encoding="UTF-8"?>
<!DOCTYPE plist PUBLIC "-//Apple//DTD PLIST 1.0//EN" "http://www.apple.com/DTDs/PropertyList-1.0.dtd">
<plist version="1.0">
<dict>
    <key>Label</key>
    <string>{LAUNCHD_LABEL}</string>
    <key>ProgramArguments</key>
    <array>
        <string>{executable}</string>
        <string>daemon</string>
    </array>
    <key>RunAtLoad</key>
    <true/>
    <key>KeepAlive</key>
    <dict>
        <key>SuccessfulExit</key>
        <false/>
    </dict>
    <key>ProcessType</key>
    <string>Background</string>
</dict>
</plist>
"#
    )
}

/// A systemd user unit that runs the daemon at login and restarts it only
/// if it crashes.
fn systemd_unit(executable: &Path) -> String {
    format!(
        "[Unit]\n\
         Description=Lemma daemon: warm index and language servers\n\
         \n\
         [Service]\n\
         ExecStart=\"{}\" daemon\n\
         Restart=on-failure\n\
         \n\
         [Install]\n\
         WantedBy=default.target\n",
        executable.display()
    )
}

fn xml_escape(text: &str) -> String {
    text.replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
}

/// `gui/<uid>`, the launchd domain of the logged-in user.
fn launchd_domain() -> anyhow::Result<String> {
    use std::os::unix::fs::MetadataExt;
    let home = std::env::var_os("HOME").context("HOME is not set")?;
    let user = fs::metadata(&home)?.uid();
    Ok(format!("gui/{user}"))
}

fn launchd_target() -> anyhow::Result<String> {
    Ok(format!("{}/{LAUNCHD_LABEL}", launchd_domain()?))
}

fn run(program: &str, args: &[&str]) -> anyhow::Result<()> {
    let output = Command::new(program)
        .args(args)
        .stdin(Stdio::null())
        .output()
        .with_context(|| format!("could not run {program}"))?;
    if !output.status.success() {
        bail!(
            "`{program} {}` failed: {}",
            args.join(" "),
            String::from_utf8_lossy(&output.stderr).trim()
        );
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_launchd_agent_runs_the_daemon_and_restarts_only_after_a_crash() {
        let plist = launchd_plist(Path::new("/opt/lemma & co/bin/lemma"));

        assert!(plist.contains("<string>/opt/lemma &amp; co/bin/lemma</string>"));
        assert!(plist.contains("<string>daemon</string>"));
        assert!(plist.contains("<key>SuccessfulExit</key>\n        <false/>"));
    }

    #[test]
    fn the_systemd_unit_runs_the_daemon_and_restarts_only_after_a_crash() {
        let unit = systemd_unit(Path::new("/home/me/.cargo/bin/lemma"));

        assert!(unit.contains("ExecStart=\"/home/me/.cargo/bin/lemma\" daemon"));
        assert!(unit.contains("Restart=on-failure"));
        assert!(unit.contains("WantedBy=default.target"));
    }
}
