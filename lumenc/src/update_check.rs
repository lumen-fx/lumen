//! Automatic update check for an installed Lumen toolchain.
//!
//! Someone running `lumenc run` should find out that a newer release exists
//! without going looking for it. This prints one short line on stderr and, on
//! a terminal, offers to run the installer.
//!
//! The rules it follows, in order:
//!
//! * Only the commands a person types get a check: `run`, `check`, `build`,
//!   `bundle`, `new`, `fmt`, `i18n`. The MCP / automation subcommands, `--help`,
//!   `--version`, and anything with `--headless` are silent.
//! * Only an installed copy checks. Discovery starts at the running executable
//!   and looks for `../share/lumen/lumen.receipt`, the receipt `tools/install.sh`
//!   writes. A cargo-built copy in `target/debug` has no receipt and never
//!   reaches the network.
//! * A receipt with a `pinned` line is left alone. Pinning is a decision, not a
//!   mistake to correct.
//! * `LUMEN_NO_UPDATE_CHECK` (any non-empty value) or `CI` in the environment
//!   turns the whole thing off, as does a stderr that is not a terminal.
//! * At most one network request per day, tracked in a small state file under
//!   the user cache directory.
//!
//! The request is a HEAD of the GitHub `releases/latest` URL through `curl`
//! (or `wget`), read for the redirect's `location:` header; the last path
//! segment is the tag. That avoids the GitHub JSON API, whose anonymous rate
//! limit is per source IP and is shared by everyone behind one NAT.
//!
//! No step here can fail the command it rides along with: every error is a
//! silent no-op, the request runs on its own thread, and a result that has not
//! arrived shortly after the command finishes is dropped.

use std::io::IsTerminal;
use std::path::PathBuf;
use std::process::Command;
use std::sync::mpsc::{self, Receiver};
use std::time::{Duration, SystemTime, UNIX_EPOCH};

/// The redirect that names the newest published tag.
const LATEST_URL: &str = "https://github.com/lumen-fx/lumen/releases/latest";

/// What the notice tells you to run, and what the prompt runs for you.
const INSTALL_COMMAND: &str = "curl -fsSL https://lumenfx.dev/install.sh | sh";

/// One network check per day.
const INTERVAL_SECS: u64 = 24 * 60 * 60;

/// How long the finished command waits for a check still in flight.
const GRACE: Duration = Duration::from_millis(100);

/// Commands a person types, as opposed to ones a tool drives.
const HUMAN_COMMANDS: &[&str] = &["run", "check", "build", "bundle", "new", "fmt", "i18n"];

/// The version this binary was built as.
fn current() -> &'static str {
    env!("CARGO_PKG_VERSION")
}

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
    let receipt = std::fs::read_to_string(receipt_path()?).ok()?;
    if receipt_is_pinned(&receipt) {
        return None;
    }

    let state_file = state_path();
    let state = state_file
        .as_ref()
        .and_then(|p| std::fs::read_to_string(p).ok())
        .map(|text| parse_state(&text))
        .unwrap_or_default();
    let now = unix_now()?;

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
    if let Some(path) = state_file.as_deref() {
        write_state(path, now, previous.as_deref());
    }

    let (tx, rx) = mpsc::channel();
    std::thread::spawn(move || {
        let fetched = fetch_latest();
        // Only a successful request has anything to add; a failed one already
        // left the day's timestamp behind.
        if let (Some(path), Some(latest)) = (state_file.as_deref(), fetched.as_deref()) {
            write_state(path, unix_now().unwrap_or(now), Some(latest));
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
            "lumenc {latest} is available (you have {}). Update: {INSTALL_COMMAND}",
            current()
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

/// Windows gets the notice without the prompt: there is no `sh` to run the
/// one-liner with.
#[cfg(not(unix))]
fn offer_update() {}

// --- receipt -----------------------------------------------------------------

/// `<prefix>/share/lumen/lumen.receipt`, derived from the running executable at
/// `<prefix>/bin/lumenc`. Symlinks are resolved first so a `~/.local/bin/lumenc`
/// symlink into the install prefix still finds the receipt.
fn receipt_path() -> Option<PathBuf> {
    let exe = std::env::current_exe().ok()?;
    let exe = std::fs::canonicalize(&exe).unwrap_or(exe);
    let prefix = exe.parent()?.parent()?;
    Some(prefix.join("share").join("lumen").join("lumen.receipt"))
}

/// Whether a receipt records a pinned install (`--version` on the installer).
fn receipt_is_pinned(receipt: &str) -> bool {
    receipt
        .lines()
        .any(|line| line.split_whitespace().next() == Some("pinned"))
}

// --- state file ---------------------------------------------------------------

/// When the last request went out, and the newest release it saw.
#[derive(Default, PartialEq, Eq, Debug)]
struct State {
    checked_at: u64,
    latest: Option<String>,
}

/// `$XDG_CACHE_HOME/lumen`, or `~/.cache/lumen`.
#[cfg(not(windows))]
fn state_dir() -> Option<PathBuf> {
    if let Some(cache) = std::env::var_os("XDG_CACHE_HOME").filter(|v| !v.is_empty()) {
        return Some(PathBuf::from(cache).join("lumen"));
    }
    std::env::var_os("HOME")
        .filter(|v| !v.is_empty())
        .map(|home| PathBuf::from(home).join(".cache").join("lumen"))
}

/// `%LOCALAPPDATA%\lumen`.
#[cfg(windows)]
fn state_dir() -> Option<PathBuf> {
    std::env::var_os("LOCALAPPDATA")
        .filter(|v| !v.is_empty())
        .map(|local| PathBuf::from(local).join("lumen"))
}

fn state_path() -> Option<PathBuf> {
    Some(state_dir()?.join("update-check"))
}

/// Reads the two-key state file. Unknown keys and malformed lines are ignored,
/// so a file from a future version still parses as far as it can.
fn parse_state(text: &str) -> State {
    let mut state = State::default();
    for line in text.lines() {
        let mut parts = line.split_whitespace();
        match (parts.next(), parts.next()) {
            (Some("checked"), Some(v)) => state.checked_at = v.parse().unwrap_or(0),
            (Some("latest"), Some(v)) => state.latest = Some(v.to_string()),
            _ => {}
        }
    }
    state
}

fn format_state(checked_at: u64, latest: Option<&str>) -> String {
    let mut out = format!("checked {checked_at}\n");
    if let Some(latest) = latest {
        out.push_str(&format!("latest {latest}\n"));
    }
    out
}

fn write_state(path: &std::path::Path, checked_at: u64, latest: Option<&str>) {
    if let Some(dir) = path.parent() {
        let _ = std::fs::create_dir_all(dir);
    }
    let _ = std::fs::write(path, format_state(checked_at, latest));
}

fn unix_now() -> Option<u64> {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .ok()
        .map(|d| d.as_secs())
}

// --- the request --------------------------------------------------------------

/// The newest published version, or `None` for any failure at all.
fn fetch_latest() -> Option<String> {
    version_from_headers(&head_request()?)
}

/// The published version a redirect points at. A repository with no releases
/// redirects to the releases index instead of a tag, which lands here as a
/// segment that is not a version and yields `None`.
fn version_from_headers(headers: &str) -> Option<String> {
    let tag = parse_location_tag(headers)?;
    let version = tag.strip_prefix('v').unwrap_or(&tag);
    parse_version(version).map(|_| version.to_string())
}

/// HEAD the releases URL and hand back whatever the tool printed. curl writes
/// headers to stdout, wget to stderr, so both streams are returned joined.
/// A missing curl (spawn error, not a failed request) falls back to wget.
fn head_request() -> Option<String> {
    let curl = Command::new("curl")
        .args(["-fsSI", "--max-time", "4", LATEST_URL])
        .output();
    let out = match curl {
        Ok(out) => out,
        Err(_) => Command::new("wget")
            .args([
                "--quiet",
                "--server-response",
                "--max-redirect=0",
                "--timeout=4",
                "--tries=1",
                "-O",
                "-",
                LATEST_URL,
            ])
            .output()
            .ok()?,
    };
    let mut text = String::from_utf8_lossy(&out.stdout).into_owned();
    text.push('\n');
    text.push_str(&String::from_utf8_lossy(&out.stderr));
    Some(text)
}

/// The last path segment of the last `location:` header, which for
/// `releases/latest` is the newest tag.
fn parse_location_tag(headers: &str) -> Option<String> {
    let mut found = None;
    for line in headers.lines() {
        let Some((name, value)) = line.trim().split_once(':') else {
            continue;
        };
        if !name.trim().eq_ignore_ascii_case("location") {
            continue;
        }
        // wget appends a "[following]" marker after the URL; take the URL only.
        let Some(url) = value.split_whitespace().next() else {
            continue;
        };
        let url = url.trim_end_matches('/');
        if let Some(segment) = url.rsplit('/').next()
            && !segment.is_empty()
        {
            found = Some(segment.to_string());
        }
    }
    found
}

// --- versions -----------------------------------------------------------------

/// `X.Y.Z`, with an optional leading `v` and any `-pre` / `+build` suffix cut.
/// Missing components read as zero; anything non-numeric fails to parse.
fn parse_version(s: &str) -> Option<(u64, u64, u64)> {
    let s = s.trim();
    let s = s
        .strip_prefix('v')
        .or_else(|| s.strip_prefix('V'))
        .unwrap_or(s);
    let core = s.split(['-', '+']).next()?;
    let mut parts = core.split('.');
    let major = parts.next()?.parse().ok()?;
    let minor = parts.next().unwrap_or("0").parse().ok()?;
    let patch = parts.next().unwrap_or("0").parse().ok()?;
    if parts.next().is_some() {
        return None;
    }
    Some((major, minor, patch))
}

/// Whether `latest` is a strictly newer release than `current`. A version
/// neither side can parse is not newer, which keeps the notice quiet rather
/// than wrong.
fn is_newer(latest: &str, current: &str) -> bool {
    match (parse_version(latest), parse_version(current)) {
        (Some(l), Some(c)) => l > c,
        _ => false,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn location_header_yields_the_tag() {
        let headers = "HTTP/2 302 \r\n\
                       server: GitHub.com\r\n\
                       location: https://github.com/lumen-fx/lumen/releases/tag/v0.2.0\r\n\
                       content-length: 0\r\n";
        assert_eq!(parse_location_tag(headers).as_deref(), Some("v0.2.0"));
    }

    #[test]
    fn location_header_is_matched_case_insensitively_and_untrimmed() {
        let headers = "  Location: https://github.com/lumen-fx/lumen/releases/tag/v1.10.3\n";
        assert_eq!(parse_location_tag(headers).as_deref(), Some("v1.10.3"));
    }

    #[test]
    fn wget_following_marker_is_ignored() {
        let headers = "  HTTP/1.1 302 Found\n  \
                       Location: https://github.com/lumen-fx/lumen/releases/tag/v0.3.1 [following]\n";
        assert_eq!(parse_location_tag(headers).as_deref(), Some("v0.3.1"));
    }

    #[test]
    fn trailing_slash_and_last_header_win() {
        let headers = "location: https://example.invalid/releases/tag/v0.1.0/\n\
                       location: https://example.invalid/releases/tag/v0.4.0\n";
        assert_eq!(parse_location_tag(headers).as_deref(), Some("v0.4.0"));
    }

    #[test]
    fn headers_without_a_location_yield_nothing() {
        assert_eq!(parse_location_tag(""), None);
        assert_eq!(
            parse_location_tag("HTTP/2 200\r\ncontent-length: 0\r\n"),
            None
        );
        assert_eq!(
            parse_location_tag("relocation: https://example.invalid/x"),
            None
        );
    }

    /// Trimmed from a real `curl -fsSI` against a GitHub `releases/latest`
    /// URL, tag line and all.
    #[test]
    fn a_real_github_redirect_yields_a_version() {
        let headers = "HTTP/2 302 \r\n\
                       date: Sun, 09 Aug 2026 15:08:10 GMT\r\n\
                       content-type: text/html; charset=utf-8\r\n\
                       location: https://github.com/rust-lang/rust/releases/tag/1.97.1\r\n\
                       cache-control: no-cache\r\n\
                       content-length: 0\r\n";
        assert_eq!(version_from_headers(headers).as_deref(), Some("1.97.1"));
    }

    #[test]
    fn a_repository_with_no_releases_yields_nothing() {
        // GitHub sends `releases/latest` to the index when there is no tag.
        let headers = "HTTP/2 302 \r\n\
                       location: https://github.com/lumen-fx/lumen/releases\r\n";
        assert_eq!(version_from_headers(headers), None);
    }

    #[test]
    fn versions_parse_with_and_without_the_v() {
        assert_eq!(parse_version("0.1.0"), Some((0, 1, 0)));
        assert_eq!(parse_version("v2.11.4"), Some((2, 11, 4)));
        assert_eq!(parse_version("1.2"), Some((1, 2, 0)));
        assert_eq!(parse_version("3"), Some((3, 0, 0)));
        assert_eq!(parse_version("0.2.0-rc.1"), Some((0, 2, 0)));
        assert_eq!(parse_version("0.2.0+build7"), Some((0, 2, 0)));
    }

    #[test]
    fn nonsense_versions_do_not_parse() {
        assert_eq!(parse_version(""), None);
        assert_eq!(parse_version("latest"), None);
        assert_eq!(parse_version("releases"), None);
        assert_eq!(parse_version("1.2.3.4"), None);
        assert_eq!(parse_version("1.x.3"), None);
    }

    #[test]
    fn newer_compares_numerically_not_lexically() {
        assert!(is_newer("0.10.0", "0.9.0"));
        assert!(is_newer("1.0.0", "0.99.99"));
        assert!(is_newer("0.1.1", "0.1.0"));
        assert!(!is_newer("0.1.0", "0.1.0"));
        assert!(!is_newer("0.1.0", "0.2.0"));
        assert!(!is_newer("v0.9.0", "0.10.0"));
    }

    #[test]
    fn an_unparseable_version_is_never_newer() {
        assert!(!is_newer("latest", "0.1.0"));
        assert!(!is_newer("0.2.0", "not-a-version"));
    }

    #[test]
    fn only_a_pinned_line_pins() {
        let plain = "version 0.1.0\ntarget linux-x86_64\nfile bin/lumenc\n";
        let pinned = "version 0.1.0\ntarget linux-x86_64\npinned 0.1.0\nfile bin/lumenc\n";
        assert!(!receipt_is_pinned(plain));
        assert!(receipt_is_pinned(pinned));
        // A path that merely contains the word is not a pin.
        assert!(!receipt_is_pinned("file bin/pinned\n"));
    }

    #[test]
    fn state_round_trips() {
        let text = format_state(1_754_697_600, Some("0.2.0"));
        assert_eq!(
            parse_state(&text),
            State {
                checked_at: 1_754_697_600,
                latest: Some("0.2.0".to_string()),
            }
        );
        let no_version = format_state(42, None);
        assert_eq!(
            parse_state(&no_version),
            State {
                checked_at: 42,
                latest: None,
            }
        );
    }

    #[test]
    fn a_damaged_state_file_reads_as_never_checked() {
        assert_eq!(
            parse_state("garbage\nchecked notanumber\n"),
            State::default()
        );
        assert_eq!(parse_state(""), State::default());
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
