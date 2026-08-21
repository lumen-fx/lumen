//! Automatic update check for an installed Lumen toolchain.
//!
//! Someone running `lumenc run` should find out that a newer release exists
//! without going looking for it. This prints one short line on stderr and, on
//! a terminal, offers to install the new release: the shell installer on Unix,
//! the `.msi` on Windows.
//!
//! The rules it follows, in order:
//!
//! * Only the commands a person types get a check: `run`, `check`, `build`,
//!   `bundle`, `new`, `fmt`, `i18n`. The MCP / automation subcommands, `--help`,
//!   `--version`, and anything with `--headless` are silent.
//! * Only an installed copy checks. Discovery starts at the running executable
//!   and looks for `../share/lumen/lumen.receipt`, the receipt `tools/release/install.sh`
//!   and the Windows `.msi` both write. A cargo-built copy in `target/debug`
//!   has no receipt and never reaches the network, and neither does a copy
//!   unpacked from the portable Windows zip, which carries no receipt.
//! * A receipt with a `pinned` line is left alone. Pinning is a decision, not a
//!   mistake to correct. Only `install.sh --version` writes that line; an MSI
//!   install is never pinned.
//! * `LUMEN_NO_UPDATE_CHECK` (any non-empty value) or `CI` in the environment
//!   turns the whole thing off, as does a stderr that is not a terminal.
//! * At most one network request per day, tracked in a small state file under
//!   the user cache directory.
//!
//! The releases page is what says a newer version exists, and [`crate::release`]
//! is what asks it. Everything here is about when to ask and what to print.
//!
//! No step here can fail the command it rides along with: every error is a
//! silent no-op, the request runs on its own thread, and a result that has not
//! arrived shortly after the command finishes is dropped.

use std::io::IsTerminal;
use std::sync::mpsc::{self, Receiver};
use std::time::Duration;

#[cfg(windows)]
use std::path::PathBuf;
#[cfg(any(unix, windows))]
use std::process::Command;

use crate::release::{self, INTERVAL_SECS, current, is_newer};

/// What the notice tells you to run, and what the prompt runs for you.
#[cfg(unix)]
const INSTALL_COMMAND: &str = "curl -fsSL https://lumenfx.dev/install.sh | sh";

/// The Windows installer for the newest release. The URL never changes:
/// `releases/latest/download/<asset>` redirects to whichever release is
/// current.
#[cfg(windows)]
const MSI_URL: &str =
    "https://github.com/lumen-fx/lumen/releases/latest/download/lumen-windows-x86_64.msi";

/// What the notice points you at.
#[cfg(unix)]
fn update_hint() -> String {
    INSTALL_COMMAND.to_string()
}

/// What the notice points you at.
#[cfg(windows)]
fn update_hint() -> String {
    MSI_URL.to_string()
}

/// What the notice points you at.
#[cfg(not(any(unix, windows)))]
fn update_hint() -> String {
    release::latest_url()
}

/// How long the finished command waits for a check still in flight.
const GRACE: Duration = Duration::from_millis(100);

/// Commands a person types, as opposed to ones a tool drives.
const HUMAN_COMMANDS: &[&str] = &["run", "check", "build", "bundle", "new", "fmt", "i18n"];

/// An update check that has been started. Hand it to [`Check::finish`] once the
/// command is done.
pub struct Check(Source);

enum Source {
    /// A newer release the last check already found, replayed without a request.
    Known(String),
    /// A request running on its own thread.
    Pending(Receiver<Option<String>>),
}

/// Decide whether to check for a newer release, and start the request if so.
///
/// `cmd` is the subcommand and `args` the arguments after it. Returns `None`
/// whenever the check does not apply, which is the common case.
pub fn start(cmd: &str, args: &[String]) -> Option<Check> {
    if !HUMAN_COMMANDS.contains(&cmd) {
        return None;
    }
    if args.iter().any(|a| a == "--headless") {
        return None;
    }
    if std::env::var_os("LUMEN_NO_UPDATE_CHECK").is_some_and(|v| !v.is_empty()) {
        return None;
    }
    if std::env::var_os("CI").is_some() {
        return None;
    }
    if !std::io::stderr().is_terminal() {
        return None;
    }
    // Only an installed copy has a release behind it to compare against, and a
    // pinned one asked to stay where it is.
    release::installed_version()?;
    if release::is_pinned() {
        return None;
    }

    let state = release::read_state();
    let now = release::unix_now()?;

    // Inside the daily window: no request, but a newer release the last check
    // already found is still worth repeating.
    if now.saturating_sub(state.checked_at) < INTERVAL_SECS {
        let latest = state.latest?;
        return is_newer(&latest, current()).then_some(Check(Source::Known(latest)));
    }

    // Spend the day's budget before the request goes out, not after it comes
    // back. A short command exits while the request is still in flight and
    // takes the thread with it, so a write that waited for the answer would
    // never land and every invocation would open a connection.
    let previous = state.latest;
    release::write_state(now, previous.as_deref());

    let (tx, rx) = mpsc::channel();
    std::thread::spawn(move || {
        let fetched = release::fetch_latest().ok();
        // Only a successful request has anything to add; a failed one already
        // left the day's timestamp behind.
        if let Some(latest) = fetched.as_deref() {
            release::write_state(release::unix_now().unwrap_or(now), Some(latest));
        }
        let _ = tx.send(fetched);
    });
    Some(Check(Source::Pending(rx)))
}

impl Check {
    /// Print the notice, if a newer release turned up in time.
    pub fn finish(self) {
        let latest = match self.0 {
            Source::Known(v) => Some(v),
            Source::Pending(rx) => rx.recv_timeout(GRACE).ok().flatten(),
        };
        let Some(latest) = latest else {
            return;
        };
        if !is_newer(&latest, current()) {
            return;
        }
        eprintln!(
            "lumenc {latest} is available (you have {}). Update: {}",
            current(),
            update_hint()
        );
        offer_update();
    }
}

/// On a terminal, offer to run the installer. Anything but `y` does nothing.
#[cfg(unix)]
fn offer_update() {
    use std::io::Write;

    if !std::io::stdin().is_terminal() || !std::io::stderr().is_terminal() {
        return;
    }
    eprint!("Update now? [y/N] ");
    let _ = std::io::stderr().flush();
    let mut line = String::new();
    if std::io::stdin().read_line(&mut line).is_err() {
        return;
    }
    let answer = line.trim().to_ascii_lowercase();
    if answer != "y" && answer != "yes" {
        return;
    }
    let _ = Command::new("sh").arg("-c").arg(INSTALL_COMMAND).status();
}

/// On a terminal, offer to install the newest `.msi`. Anything but `y` does
/// nothing.
///
/// The install cannot run now. Windows Installer has to replace `lumenc.exe`,
/// and this process has it open, so a `/passive` run would hit files-in-use
/// and end at 1603 or 3010. The download happens here and the install happens
/// afterwards, driven by a detached PowerShell that waits for this process id
/// to go away first.
#[cfg(windows)]
fn offer_update() {
    use std::io::Write;
    use std::os::windows::process::CommandExt;

    // Detached and in its own process group, so the waiter outlives both this
    // process and the console it was started from.
    const DETACHED_PROCESS: u32 = 0x0000_0008;
    const CREATE_NEW_PROCESS_GROUP: u32 = 0x0000_0200;

    if !std::io::stdin().is_terminal() || !std::io::stderr().is_terminal() {
        return;
    }
    eprint!("Update now? [y/N] ");
    let _ = std::io::stderr().flush();
    let mut line = String::new();
    if std::io::stdin().read_line(&mut line).is_err() {
        return;
    }
    let answer = line.trim().to_ascii_lowercase();
    if answer != "y" && answer != "yes" {
        return;
    }

    let Some(temp) = std::env::var_os("TEMP").filter(|v| !v.is_empty()) else {
        eprintln!("No TEMP directory to download into. Install {MSI_URL} by hand.");
        return;
    };
    let msi = PathBuf::from(temp).join("lumen-update.msi");
    let msi = msi.display().to_string();

    if !download(MSI_URL, &msi) {
        eprintln!("Could not download {MSI_URL}");
        return;
    }

    // Single quotes are PowerShell's literal string, and a doubled quote is
    // how a literal quote is written inside one.
    let quoted = msi.replace('\'', "''");
    let pid = std::process::id();
    let script = format!(
        "Wait-Process -Id {pid} -Timeout 120 -ErrorAction SilentlyContinue; \
         Start-Process msiexec -ArgumentList '/i','{quoted}','/passive','/norestart' -Wait; \
         Remove-Item '{quoted}' -ErrorAction SilentlyContinue"
    );
    let spawned = Command::new("powershell")
        .args(["-NoProfile", "-NonInteractive", "-Command", &script])
        .creation_flags(DETACHED_PROCESS | CREATE_NEW_PROCESS_GROUP)
        .spawn();
    if spawned.is_err() {
        eprintln!("Could not start the installer. Run {msi} by hand.");
        return;
    }
    eprintln!("The update installs once this command exits. Open a new terminal afterwards.");
}

/// Download `url` to `dest`, and report whether it arrived. A missing
/// `curl.exe` (a spawn error, not a failed request) falls back to PowerShell.
#[cfg(windows)]
fn download(url: &str, dest: &str) -> bool {
    let curl = Command::new("curl.exe")
        .args(["-fL", "--max-time", "300", "-o", dest, url])
        .status();
    match curl {
        Ok(status) => status.success(),
        Err(_) => {
            let quoted = dest.replace('\'', "''");
            Command::new("powershell")
                .args([
                    "-NoProfile",
                    "-NonInteractive",
                    "-Command",
                    &format!("Invoke-WebRequest -Uri '{url}' -OutFile '{quoted}'"),
                ])
                .status()
                .is_ok_and(|status| status.success())
        }
    }
}

/// Everything else gets the notice without a prompt: there is no installer to
/// offer.
#[cfg(not(any(unix, windows)))]
fn offer_update() {}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn only_human_commands_are_listed() {
        for cmd in ["run", "check", "build", "bundle", "new", "fmt", "i18n"] {
            assert!(HUMAN_COMMANDS.contains(&cmd), "{cmd} should be checked");
        }
        for cmd in [
            "snapshot",
            "find",
            "element-at",
            "click",
            "type",
            "key",
            "scroll",
            "lint",
            "diff",
            "screenshot",
            "--help",
            "--version",
        ] {
            assert!(!HUMAN_COMMANDS.contains(&cmd), "{cmd} should stay silent");
        }
    }
}
