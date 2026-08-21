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
use std::path::PathBuf;
use std::sync::mpsc::{self, Receiver};
use std::time::Duration;

#[cfg(any(unix, windows))]
use std::process::Command;

use crate::release::{self, INTERVAL_SECS, State, current, is_newer};

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

/// Why an invocation does not check. Each one is a rule from the list at the
/// top of this file.
#[derive(Debug, PartialEq, Eq)]
enum Skip {
    /// A subcommand a tool drives rather than a person types.
    NotHuman,
    /// A `--headless` run, which is automation with a terminal attached.
    Headless,
    /// `LUMEN_NO_UPDATE_CHECK`.
    Disabled,
    /// `CI`.
    Ci,
    /// Nothing would read the notice.
    NotATerminal,
    /// No install receipt, so there is no release behind this copy.
    NotInstalled,
    /// The install asked to stay where it is.
    Pinned,
}

/// What the check reads about the run it rides along with.
struct Facts<'a> {
    cmd: &'a str,
    headless: bool,
    disabled: bool,
    ci: bool,
    terminal: bool,
    /// The install receipt, when this copy was installed from a release.
    receipt: Option<&'a str>,
}

/// Why this invocation does not check, or `None` when it does.
fn skip(facts: &Facts) -> Option<Skip> {
    if !HUMAN_COMMANDS.contains(&facts.cmd) {
        return Some(Skip::NotHuman);
    }
    if facts.headless {
        return Some(Skip::Headless);
    }
    if facts.disabled {
        return Some(Skip::Disabled);
    }
    if facts.ci {
        return Some(Skip::Ci);
    }
    if !facts.terminal {
        return Some(Skip::NotATerminal);
    }
    // A receipt records the release this copy came from, and that is what a
    // newer one is compared against. No receipt, or one too damaged to name a
    // release, leaves nothing to compare.
    let Some(receipt) = facts
        .receipt
        .filter(|r| release::installed_release(r).is_some())
    else {
        return Some(Skip::NotInstalled);
    };
    release::is_pinned_receipt(receipt).then_some(Skip::Pinned)
}

/// Decide whether to check for a newer release, and start the request if so.
///
/// `cmd` is the subcommand and `args` the arguments after it. Returns `None`
/// whenever the check does not apply, which is the common case.
pub fn start(cmd: &str, args: &[String]) -> Option<Check> {
    let receipt = release::receipt_text();
    let facts = Facts {
        cmd,
        headless: args.iter().any(|a| a == "--headless"),
        disabled: std::env::var_os("LUMEN_NO_UPDATE_CHECK").is_some_and(|v| !v.is_empty()),
        ci: std::env::var_os("CI").is_some(),
        terminal: std::io::stderr().is_terminal(),
        receipt: receipt.as_deref(),
    };
    if skip(&facts).is_some() {
        return None;
    }
    let state_file = release::state_path();
    begin(
        state_file,
        release::read_state(),
        release::unix_now()?,
        || release::fetch_latest().ok(),
    )
}

/// Start the check with the day's state in hand, asking `ask` on its own
/// thread when the day's request has not gone out yet.
///
/// The state file, the clock and the request are all arguments, so the rule
/// about one request a day is checkable without any of the three.
fn begin(
    state_file: Option<PathBuf>,
    state: State,
    now: u64,
    ask: impl FnOnce() -> Option<String> + Send + 'static,
) -> Option<Check> {
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
    if let Some(path) = state_file.as_deref() {
        release::write_state_at(path, now, state.latest.as_deref());
    }

    let (tx, rx) = mpsc::channel();
    std::thread::spawn(move || {
        let fetched = ask();
        // Only a successful request has anything to add; a failed one already
        // left the day's timestamp behind.
        if let (Some(path), Some(latest)) = (state_file.as_deref(), fetched.as_deref()) {
            release::write_state_at(path, release::unix_now().unwrap_or(now), Some(latest));
        }
        let _ = tx.send(fetched);
    });
    Some(Check(Source::Pending(rx)))
}

/// The notice for a release that is worth telling someone about, or `None`
/// when it is not newer than what is running.
fn notice(latest: &str, current: &str) -> Option<String> {
    is_newer(latest, current).then(|| {
        format!(
            "lumenc {latest} is available (you have {current}). Update: {}",
            update_hint()
        )
    })
}

impl Check {
    /// Print the notice, if a newer release turned up in time.
    pub fn finish(self) {
        let Some(latest) = self.found() else {
            return;
        };
        let Some(notice) = notice(&latest, current()) else {
            return;
        };
        eprintln!("{notice}");
        offer_update();
    }

    /// The release this check found, waiting a moment for a request still in
    /// flight and giving up on it rather than holding the command open.
    fn found(self) -> Option<String> {
        match self.0 {
            Source::Known(v) => Some(v),
            Source::Pending(rx) => rx.recv_timeout(GRACE).ok().flatten(),
        }
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

    /// A run that checks: a person's command, on a terminal, from an
    /// installed copy that is not pinned.
    fn checking() -> Facts<'static> {
        Facts {
            cmd: "run",
            headless: false,
            disabled: false,
            ci: false,
            terminal: true,
            receipt: Some("version 0.1.0\ntarget linux-x86_64\n"),
        }
    }

    #[test]
    fn an_installed_copy_on_a_terminal_checks() {
        assert_eq!(skip(&checking()), None);
    }

    /// Each rule on its own, so a change to one of them cannot hide behind
    /// another.
    #[test]
    fn every_rule_turns_the_check_off_by_itself() {
        let cases: [(&str, Facts, Skip); 6] = [
            (
                "an automation subcommand",
                Facts {
                    cmd: "snapshot",
                    ..checking()
                },
                Skip::NotHuman,
            ),
            (
                "a headless run",
                Facts {
                    headless: true,
                    ..checking()
                },
                Skip::Headless,
            ),
            (
                "LUMEN_NO_UPDATE_CHECK",
                Facts {
                    disabled: true,
                    ..checking()
                },
                Skip::Disabled,
            ),
            (
                "CI",
                Facts {
                    ci: true,
                    ..checking()
                },
                Skip::Ci,
            ),
            (
                "output that nobody reads",
                Facts {
                    terminal: false,
                    ..checking()
                },
                Skip::NotATerminal,
            ),
            (
                "a pinned install",
                Facts {
                    receipt: Some("version 0.1.0\npinned 0.1.0\n"),
                    ..checking()
                },
                Skip::Pinned,
            ),
        ];
        for (what, facts, want) in cases {
            assert_eq!(skip(&facts), Some(want), "{what}");
        }
    }

    /// A cargo build and the portable zip carry no receipt, and a copy with no
    /// release behind it has nothing to compare against.
    #[test]
    fn a_copy_that_was_not_installed_never_checks() {
        assert_eq!(
            skip(&Facts {
                receipt: None,
                ..checking()
            }),
            Some(Skip::NotInstalled)
        );
    }

    fn state_file(name: &str) -> PathBuf {
        std::env::temp_dir()
            .join(format!("lumen-check-{}-{name}", std::process::id()))
            .join("update-check")
    }

    /// Inside the day the request does not go out, and a newer release the
    /// last one found is repeated from the state file instead.
    #[test]
    fn a_second_run_the_same_day_makes_no_request() {
        let asked = std::sync::Arc::new(std::sync::atomic::AtomicUsize::new(0));
        let counter = asked.clone();
        let check = begin(
            None,
            State {
                checked_at: 1_000_000,
                latest: Some("99.0.0".to_string()),
            },
            1_000_000 + INTERVAL_SECS - 1,
            move || {
                counter.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
                Some("99.0.0".to_string())
            },
        );
        assert_eq!(check.and_then(Check::found).as_deref(), Some("99.0.0"));
        assert_eq!(asked.load(std::sync::atomic::Ordering::SeqCst), 0);
    }

    /// A release that is not newer than what is running is not a notice, so
    /// there is nothing to hand back.
    #[test]
    fn a_release_that_is_not_newer_says_nothing() {
        let check = begin(
            None,
            State {
                checked_at: 1_000_000,
                latest: Some("0.0.1".to_string()),
            },
            1_000_000,
            || None,
        );
        assert!(check.is_none());
        assert_eq!(notice("0.0.1", current()), None);
        assert_eq!(notice(current(), current()), None);
    }

    /// Once the day is up the request goes out, its answer comes back through
    /// the check, and the state file carries it into the next run.
    #[test]
    fn the_day_after_asks_and_writes_the_answer_down() {
        let path = state_file("asks");
        let _ = std::fs::remove_dir_all(path.parent().expect("has a parent"));

        let check = begin(
            Some(path.clone()),
            State::default(),
            INTERVAL_SECS * 30,
            || Some("99.0.0".to_string()),
        );
        assert_eq!(check.and_then(Check::found).as_deref(), Some("99.0.0"));

        let written = release::read_state_at(&path);
        assert_eq!(written.latest.as_deref(), Some("99.0.0"));

        let _ = std::fs::remove_dir_all(path.parent().expect("has a parent"));
    }

    /// A request that fails is a silent no-op, and the day's budget is still
    /// spent so the next command does not open another connection.
    #[test]
    fn a_failed_request_stays_quiet_and_still_spends_the_day() {
        let path = state_file("fails");
        let _ = std::fs::remove_dir_all(path.parent().expect("has a parent"));

        let now = INTERVAL_SECS * 30;
        let check = begin(Some(path.clone()), State::default(), now, || None);
        assert_eq!(check.and_then(Check::found), None);

        let written = release::read_state_at(&path);
        assert_eq!(written.checked_at, now);
        assert_eq!(written.latest, None);

        let _ = std::fs::remove_dir_all(path.parent().expect("has a parent"));
    }

    /// A copy built from source has no receipt, so no command it runs reaches
    /// the network, whatever the terminal and the environment say.
    #[test]
    fn a_build_from_source_starts_no_check() {
        for cmd in ["run", "build", "snapshot"] {
            assert!(start(cmd, &[]).is_none(), "{cmd}");
        }
    }

    /// A release older than the running copy is not news. This is what keeps a
    /// build carrying a version that has not been tagged yet quiet, rather
    /// than telling someone to downgrade.
    #[test]
    fn a_release_older_than_this_build_prints_nothing() {
        assert_eq!(notice("0.0.1", "0.0.4"), None);
        // Reaching the end without printing or prompting is the whole point.
        Check(Source::Known("0.0.1".to_string())).finish();
    }

    /// The notice names both versions and how to install the new one.
    #[test]
    fn the_notice_names_the_new_release_and_how_to_get_it() {
        let notice = notice("99.0.0", "0.0.1").expect("99.0.0 is newer");
        assert!(notice.contains("99.0.0"), "{notice}");
        assert!(notice.contains("0.0.1"), "{notice}");
        assert!(notice.contains(&update_hint()), "{notice}");
    }

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
