//! Which published release this toolchain draws its files from.
//!
//! `CARGO_PKG_VERSION` says what this copy of `lumenc` is, and that is all it
//! says. A branch carries the next version number from the moment a tag is
//! cut, so a download URL or a cache directory built out of that number names
//! a release nobody published. Everything that needs to name one asks here
//! instead, and the answer comes from the releases page.
//!
//! Two things can answer, in this order:
//!
//! * The install receipt. An installed toolchain came from a release, and the
//!   receipt's `version` line records which one. That release stays the answer
//!   for as long as the installation lasts, so the launcher stub and the web
//!   runtime a build downloads are the ones this compiler was published
//!   beside. It is also what holds an `install.sh --version` pin in place: the
//!   pinned version is the version the installer wrote.
//! * The releases page. A copy that was not installed, such as a cargo build
//!   or an unpacked portable zip, has no receipt, so the newest published
//!   release answers. `<repo>/releases/latest` redirects to it, and the last
//!   path segment of that redirect is the tag.
//!
//! When neither answers, the caller gets a message saying which case it hit: a
//! repository with no releases and a releases page that cannot be reached are
//! different problems with different fixes.
//!
//! The request is a HEAD through `curl` (or `wget`), read for the redirect's
//! `location:` header. That avoids the GitHub JSON API, whose anonymous rate
//! limit is per source IP and is shared by everyone behind one NAT. The answer
//! is remembered for a day under the user cache directory, so a machine that
//! resolves twice in a row makes one request, and a machine that is offline
//! today still has the answer the page gave yesterday.

use std::fmt;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::time::{SystemTime, UNIX_EPOCH};

/// The repository releases come from unless `LUMEN_GH_REPO` names another.
const DEFAULT_REPO: &str = "lumen-fx/lumen";

/// How long a resolved answer is reused before the page is asked again.
pub(crate) const INTERVAL_SECS: u64 = 24 * 60 * 60;

/// The repository, as `owner/name`.
pub fn repo() -> String {
    std::env::var("LUMEN_GH_REPO")
        .ok()
        .filter(|v| !v.is_empty())
        .unwrap_or_else(|| DEFAULT_REPO.to_string())
}

/// The repository's web address.
pub fn repo_url() -> String {
    format!("https://github.com/{}", repo())
}

/// The redirect that names the newest published tag.
pub fn latest_url() -> String {
    format!("{}/releases/latest", repo_url())
}

/// Where the files published with release `version` live.
pub fn asset_base(version: &str) -> String {
    format!("{}/releases/download/v{version}", repo_url())
}

/// The version this binary was built as. It says what this copy is, never what
/// is published, so it belongs in messages and in `--version` output and
/// nowhere else.
pub fn current() -> &'static str {
    env!("CARGO_PKG_VERSION")
}

/// The release this toolchain downloads published files from.
///
/// An installed copy answers from its receipt without a request. Anything else
/// asks the releases page, and the answer is kept for a day.
pub fn resolve() -> Result<String, Unresolved> {
    let receipt = receipt_text();
    let now = unix_now().unwrap_or(0);
    let answer = decide(receipt.as_deref(), &read_state(), now, fetch_latest)?;
    if let Answer::Asked(version) = &answer {
        write_state(now, Some(version));
    }
    Ok(answer.into_version())
}

/// Which of the things that can name a release did, so a caller knows whether
/// the answer is worth writing down and a reader can tell the paths apart.
#[derive(Debug, PartialEq, Eq)]
enum Answer {
    /// The receipt named the release this copy was installed from.
    Installed(String),
    /// The answer from an earlier run was still inside its day.
    Remembered(String),
    /// The releases page answered just now.
    Asked(String),
    /// The page could not be reached, so the last answer it gave stands.
    Stale(String),
}

impl Answer {
    fn into_version(self) -> String {
        match self {
            Answer::Installed(v) | Answer::Remembered(v) | Answer::Asked(v) | Answer::Stale(v) => v,
        }
    }
}

/// The whole decision, with everything it reads handed to it: the install
/// receipt, what an earlier run wrote down, the time, and a way to ask the
/// releases page.
///
/// Separating this from the reading is what lets the rules be checked without
/// a network, a receipt on disk, or a clock.
fn decide(
    receipt: Option<&str>,
    state: &State,
    now: u64,
    ask: impl FnOnce() -> Result<String, Unresolved>,
) -> Result<Answer, Unresolved> {
    // An installed copy came from a release, and its own release is the one
    // whose files match the compiler sitting beside them. A pinned install
    // holds here, because the pinned version is what the installer recorded.
    if let Some(installed) = receipt.and_then(installed_release) {
        return Ok(Answer::Installed(installed));
    }
    if now.saturating_sub(state.checked_at) < INTERVAL_SECS
        && let Some(remembered) = state.latest.clone()
    {
        return Ok(Answer::Remembered(remembered));
    }
    match ask() {
        Ok(latest) => Ok(Answer::Asked(latest)),
        // Yesterday's answer came from the page too, so an unreachable page
        // falls back to it rather than to a number this binary made up. A
        // repository that has dropped its releases is a different answer and
        // replaces the remembered one.
        Err(Unresolved::Unreachable) => state
            .latest
            .clone()
            .map(Answer::Stale)
            .ok_or(Unresolved::Unreachable),
        Err(other) => Err(other),
    }
}

/// Why the releases page could not name a release.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Unresolved {
    /// The page was reached and the repository has published nothing.
    NoReleases,
    /// The request did not get through.
    Unreachable,
}

impl fmt::Display for Unresolved {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Unresolved::NoReleases => write!(f, "{} has published no releases yet", repo()),
            Unresolved::Unreachable => write!(f, "{} could not be reached", latest_url()),
        }
    }
}

impl std::error::Error for Unresolved {}

/// The newest published release, asked for now.
pub(crate) fn fetch_latest() -> Result<String, Unresolved> {
    let headers = head_request().ok_or(Unresolved::Unreachable)?;
    version_from_headers(&headers)
}

/// The published version a redirect points at.
fn version_from_headers(headers: &str) -> Result<String, Unresolved> {
    // No redirect means the request never reached the releases page.
    let tag = parse_location_tag(headers).ok_or(Unresolved::Unreachable)?;
    let version = tag.strip_prefix('v').unwrap_or(&tag);
    // A repository with no releases redirects to the releases index instead of
    // to a tag, which arrives here as a last segment that is not a version.
    parse_version(version)
        .map(|_| version.to_string())
        .ok_or(Unresolved::NoReleases)
}

/// HEAD the releases URL and hand back whatever the tool printed. curl writes
/// headers to stdout, wget to stderr, so both streams are returned joined.
/// A missing curl (spawn error, not a failed request) falls back to wget.
fn head_request() -> Option<String> {
    let url = latest_url();
    let curl = Command::new("curl")
        .args(["-fsSI", "--max-time", "4", &url])
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
                &url,
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

// --- the install receipt ------------------------------------------------------

/// The receipt for the running executable, if this copy was installed.
pub(crate) fn receipt_text() -> Option<String> {
    std::fs::read_to_string(receipt_path()?).ok()
}

/// The receipt for the running executable. Symlinks are resolved first so a
/// `~/.local/bin/lumenc` symlink into the install prefix still finds it.
fn receipt_path() -> Option<PathBuf> {
    let exe = std::env::current_exe().ok()?;
    let exe = std::fs::canonicalize(&exe).unwrap_or(exe);
    receipt_path_from(&exe)
}

/// `<prefix>/share/lumen/lumen.receipt` for an executable at
/// `<prefix>/bin/lumenc`. Path arithmetic only; the caller resolves symlinks.
fn receipt_path_from(exe: &Path) -> Option<PathBuf> {
    let prefix = exe.parent()?.parent()?;
    Some(prefix.join("share").join("lumen").join("lumen.receipt"))
}

/// The release a receipt records. A receipt whose `version` line is damaged
/// reads as no receipt at all, so the releases page answers instead of a
/// malformed URL being built.
pub(crate) fn installed_release(receipt: &str) -> Option<String> {
    let value = receipt_field(receipt, "version")?;
    let value = value.strip_prefix('v').unwrap_or(&value).to_string();
    parse_version(&value).map(|_| value)
}

/// Whether a receipt records a pinned install. Pinning is a decision, so a
/// pinned install is never told about newer releases.
pub(crate) fn is_pinned_receipt(receipt: &str) -> bool {
    receipt_field(receipt, "pinned").is_some()
}

/// The value on the receipt line starting with `key`. A receipt line is a key,
/// a space, and a value.
fn receipt_field(receipt: &str, key: &str) -> Option<String> {
    for line in receipt.lines() {
        let mut parts = line.split_whitespace();
        if parts.next() == Some(key)
            && let Some(value) = parts.next()
        {
            return Some(value.to_string());
        }
    }
    None
}

// --- the remembered answer ----------------------------------------------------

/// When the releases page was last asked, and the newest release it named.
#[derive(Default, PartialEq, Eq, Debug)]
pub(crate) struct State {
    pub(crate) checked_at: u64,
    pub(crate) latest: Option<String>,
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

pub(crate) fn state_path() -> Option<PathBuf> {
    Some(state_dir()?.join("update-check"))
}

/// What the state file holds, or an empty state when there is none.
pub(crate) fn read_state() -> State {
    state_path().map(|p| read_state_at(&p)).unwrap_or_default()
}

/// What the state file at `path` holds. A file that is not there, or cannot be
/// read, is an empty state: the answer is asked for again rather than guessed.
pub(crate) fn read_state_at(path: &Path) -> State {
    std::fs::read_to_string(path)
        .map(|text| parse_state(&text))
        .unwrap_or_default()
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

/// Records the answer. A failure to write costs one extra request later, so it
/// is not worth reporting.
pub(crate) fn write_state(checked_at: u64, latest: Option<&str>) {
    if let Some(path) = state_path() {
        write_state_at(&path, checked_at, latest);
    }
}

/// Records the answer in the file at `path`, creating the directory it sits in.
pub(crate) fn write_state_at(path: &Path, checked_at: u64, latest: Option<&str>) {
    if let Some(dir) = path.parent() {
        let _ = std::fs::create_dir_all(dir);
    }
    let _ = std::fs::write(path, format_state(checked_at, latest));
}

pub(crate) fn unix_now() -> Option<u64> {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .ok()
        .map(|d| d.as_secs())
}

// --- versions -----------------------------------------------------------------

/// `X.Y.Z`, with an optional leading `v` and any `-pre` / `+build` suffix cut.
/// Missing components read as zero; anything non-numeric fails to parse.
pub(crate) fn parse_version(s: &str) -> Option<(u64, u64, u64)> {
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
/// neither side can parse is not newer, which keeps a notice quiet rather than
/// wrong.
pub(crate) fn is_newer(latest: &str, current: &str) -> bool {
    match (parse_version(latest), parse_version(current)) {
        (Some(l), Some(c)) => l > c,
        _ => false,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// An installed copy, as the installer writes it.
    const RECEIPT: &str = "version 0.1.0\ntarget linux-x86_64\nfile bin/lumenc\n";

    /// The same, pinned by `install.sh --version`.
    const PINNED: &str = "version 0.1.0\ntarget linux-x86_64\npinned 0.1.0\nfile bin/lumenc\n";

    /// A page that answers with `latest`, and a count of how often it was
    /// asked, so a test can prove a path made no request at all.
    fn page(
        latest: Result<String, Unresolved>,
        asked: &std::cell::Cell<u32>,
    ) -> impl FnOnce() -> Result<String, Unresolved> {
        move || {
            asked.set(asked.get() + 1);
            latest
        }
    }

    fn remembered(checked_at: u64, latest: &str) -> State {
        State {
            checked_at,
            latest: Some(latest.to_string()),
        }
    }

    /// An installed toolchain uses the release it was installed from, and
    /// makes no request to find that out.
    #[test]
    fn an_installed_copy_answers_from_its_receipt() {
        let asked = std::cell::Cell::new(0);
        let answer = decide(
            Some(RECEIPT),
            &State::default(),
            1_000_000,
            page(Ok("9.9.9".to_string()), &asked),
        );
        assert_eq!(answer, Ok(Answer::Installed("0.1.0".to_string())));
        assert_eq!(asked.get(), 0, "the receipt already answered");
    }

    /// A pin holds because the installer wrote the pinned version as the
    /// installed one: the newest release does not enter into it.
    #[test]
    fn a_pinned_copy_stays_on_the_version_it_was_pinned_to() {
        let asked = std::cell::Cell::new(0);
        let answer = decide(
            Some(PINNED),
            &remembered(1_000_000, "9.9.9"),
            1_000_000,
            page(Ok("9.9.9".to_string()), &asked),
        );
        assert_eq!(answer.map(Answer::into_version), Ok("0.1.0".to_string()));
        assert_eq!(asked.get(), 0);
        assert!(is_pinned_receipt(PINNED));
        assert!(!is_pinned_receipt(RECEIPT));
    }

    /// A receipt that does not say which release it came from cannot answer,
    /// so the page does. A damaged line must not become part of a URL.
    #[test]
    fn a_damaged_receipt_falls_through_to_the_page() {
        let asked = std::cell::Cell::new(0);
        for receipt in [
            "target linux-x86_64\n",
            "version not-a-number\n",
            "version\n",
        ] {
            assert_eq!(installed_release(receipt), None, "{receipt:?}");
        }
        let answer = decide(
            Some("version wobble\n"),
            &State::default(),
            1_000_000,
            page(Ok("0.3.0".to_string()), &asked),
        );
        assert_eq!(answer, Ok(Answer::Asked("0.3.0".to_string())));
        assert_eq!(asked.get(), 1);
    }

    /// A receipt records the bare version, but a `v` prefix is tolerated
    /// rather than turned into `vv0.1.0` further down.
    #[test]
    fn a_receipt_version_reads_with_or_without_the_v() {
        assert_eq!(
            installed_release("version v0.1.0\n").as_deref(),
            Some("0.1.0")
        );
        assert_eq!(installed_release(RECEIPT).as_deref(), Some("0.1.0"));
    }

    /// A copy that was not installed asks the page, and the answer is one the
    /// caller should write down.
    #[test]
    fn a_source_build_asks_the_page() {
        let asked = std::cell::Cell::new(0);
        let answer = decide(
            None,
            &State::default(),
            1_000_000,
            page(Ok("0.0.3".to_string()), &asked),
        );
        assert_eq!(answer, Ok(Answer::Asked("0.0.3".to_string())));
        assert_eq!(asked.get(), 1);
    }

    /// Inside the day, the answer from the last request is reused, so a run of
    /// builds makes one request between them.
    #[test]
    fn an_answer_inside_its_day_is_reused() {
        let asked = std::cell::Cell::new(0);
        let answer = decide(
            None,
            &remembered(1_000_000, "0.0.3"),
            1_000_000 + INTERVAL_SECS - 1,
            page(Ok("0.0.4".to_string()), &asked),
        );
        assert_eq!(answer, Ok(Answer::Remembered("0.0.3".to_string())));
        assert_eq!(asked.get(), 0);
    }

    /// Once the day is up the page is asked again, and its answer wins over
    /// the remembered one.
    #[test]
    fn an_answer_older_than_a_day_is_asked_again() {
        let asked = std::cell::Cell::new(0);
        let answer = decide(
            None,
            &remembered(1_000_000, "0.0.3"),
            1_000_000 + INTERVAL_SECS,
            page(Ok("0.0.4".to_string()), &asked),
        );
        assert_eq!(answer, Ok(Answer::Asked("0.0.4".to_string())));
        assert_eq!(asked.get(), 1);
    }

    /// Offline with an answer already on disk: the last answer the page gave
    /// stands, because it is the page's and not this binary's own version.
    #[test]
    fn an_unreachable_page_falls_back_to_the_last_answer() {
        let asked = std::cell::Cell::new(0);
        let answer = decide(
            None,
            &remembered(0, "0.0.3"),
            INTERVAL_SECS * 30,
            page(Err(Unresolved::Unreachable), &asked),
        );
        assert_eq!(answer, Ok(Answer::Stale("0.0.3".to_string())));
        assert_eq!(asked.get(), 1);
    }

    /// Offline with nothing on disk: no answer, and the message says the page
    /// could not be reached rather than naming a release.
    #[test]
    fn an_unreachable_page_with_nothing_remembered_fails() {
        let asked = std::cell::Cell::new(0);
        let answer = decide(
            None,
            &State::default(),
            1_000_000,
            page(Err(Unresolved::Unreachable), &asked),
        );
        assert_eq!(answer, Err(Unresolved::Unreachable));
    }

    /// A repository that has published nothing replaces a remembered answer
    /// rather than falling back to it: the page was reached, and this is what
    /// it said.
    #[test]
    fn a_repository_with_no_releases_answers_nothing() {
        let asked = std::cell::Cell::new(0);
        let answer = decide(
            None,
            &remembered(0, "0.0.3"),
            INTERVAL_SECS * 30,
            page(Err(Unresolved::NoReleases), &asked),
        );
        assert_eq!(answer, Err(Unresolved::NoReleases));
    }

    /// A copy built by cargo is not an installed one: the search walks up from
    /// the running executable, and a target directory holds no receipt. This
    /// is what keeps a development build from resolving to the release whose
    /// number it happens to carry.
    #[test]
    fn a_build_from_source_has_no_receipt() {
        assert_eq!(receipt_text(), None);
    }

    /// The remembered answer belongs to the user, not to a project, so it sits
    /// under the user cache directory rather than beside a build.
    #[test]
    fn the_remembered_answer_lives_under_the_user_cache() {
        let Some(path) = state_path() else {
            return;
        };
        assert!(path.ends_with("update-check"), "{}", path.display());
        assert!(
            path.parent().is_some_and(|p| p.ends_with("lumen")),
            "{}",
            path.display()
        );
        // And that path is the file the answer is read back from.
        assert_eq!(read_state(), read_state_at(&path));
    }

    /// The state file is the seam between one run and the next, so it has to
    /// survive the round trip on disk and read as empty when it is not there.
    #[test]
    fn the_remembered_answer_survives_a_round_trip() {
        let dir = std::env::temp_dir().join(format!("lumen-state-{}", std::process::id()));
        let path = dir.join("update-check");
        let _ = std::fs::remove_dir_all(&dir);

        assert_eq!(read_state_at(&path), State::default());
        write_state_at(&path, 1_754_697_600, Some("0.2.0"));
        assert_eq!(read_state_at(&path), remembered(1_754_697_600, "0.2.0"));
        write_state_at(&path, 42, None);
        assert_eq!(
            read_state_at(&path),
            State {
                checked_at: 42,
                latest: None
            }
        );

        let _ = std::fs::remove_dir_all(&dir);
    }

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
        // A header with nothing after the colon names no tag either.
        assert_eq!(parse_location_tag("location:   \r\n"), None);
    }

    /// Trimmed from a `curl -fsSI` against a GitHub `releases/latest` URL, tag
    /// line and all.
    #[test]
    fn a_github_redirect_yields_a_version() {
        let headers = "HTTP/2 302 \r\n\
                       date: Sun, 09 Aug 2026 15:08:10 GMT\r\n\
                       content-type: text/html; charset=utf-8\r\n\
                       location: https://github.com/rust-lang/rust/releases/tag/1.97.1\r\n\
                       cache-control: no-cache\r\n\
                       content-length: 0\r\n";
        assert_eq!(version_from_headers(headers).as_deref(), Ok("1.97.1"));
    }

    #[test]
    fn a_repository_with_no_releases_says_so() {
        // GitHub sends `releases/latest` to the index when there is no tag.
        let headers = "HTTP/2 302 \r\n\
                       location: https://github.com/lumen-fx/lumen/releases\r\n";
        assert_eq!(version_from_headers(headers), Err(Unresolved::NoReleases));
    }

    #[test]
    fn a_request_that_did_not_get_through_says_so() {
        assert_eq!(version_from_headers(""), Err(Unresolved::Unreachable));
    }

    /// The two cases read differently, because the fix for each is different.
    #[test]
    fn the_two_failures_name_the_repository_or_the_page() {
        let empty = Unresolved::NoReleases.to_string();
        assert!(empty.contains(&repo()), "{empty}");
        let unreachable = Unresolved::Unreachable.to_string();
        assert!(unreachable.contains("releases/latest"), "{unreachable}");
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

    /// A receipt line is a key and a value, so a path that merely contains the
    /// word `pinned` is not a pin.
    #[test]
    fn only_a_pinned_line_pins() {
        assert!(!is_pinned_receipt("file bin/pinned\n"));
        assert_eq!(
            receipt_field(RECEIPT, "target").as_deref(),
            Some("linux-x86_64")
        );
        assert_eq!(receipt_field(RECEIPT, "installer"), None);
    }

    /// The MSI installs into `%LOCALAPPDATA%\Programs\Lumen`, two levels
    /// above the receipt and one above the executable. Built out of `join`
    /// rather than a literal path so it reads the same on any host.
    #[test]
    fn the_receipt_sits_beside_the_install_prefix() {
        let prefix = PathBuf::from("C:")
            .join("Users")
            .join("dev")
            .join("AppData")
            .join("Local")
            .join("Programs")
            .join("Lumen");
        let exe = prefix.join("bin").join("lumenc.exe");
        let want = prefix.join("share").join("lumen").join("lumen.receipt");
        assert_eq!(receipt_path_from(&exe), Some(want));

        let prefix = PathBuf::from("home").join("dev").join(".lumen");
        let exe = prefix.join("bin").join("lumenc");
        let want = prefix.join("share").join("lumen").join("lumen.receipt");
        assert_eq!(receipt_path_from(&exe), Some(want));
    }

    #[test]
    fn an_executable_with_nothing_above_it_has_no_receipt() {
        assert_eq!(receipt_path_from(Path::new("lumenc")), None);
    }

    #[test]
    fn a_damaged_state_file_reads_as_never_checked() {
        assert_eq!(
            parse_state("garbage\nchecked notanumber\n"),
            State::default()
        );
        assert_eq!(parse_state(""), State::default());
    }

    /// The repository is one setting for every lookup: the page the newest
    /// release is read from and the release files are downloaded from are the
    /// same repository.
    #[test]
    fn the_urls_all_name_the_same_repository() {
        let repo = repo();
        assert!(latest_url().starts_with(&format!("https://github.com/{repo}/")));
        assert_eq!(
            asset_base("1.2.3"),
            format!("https://github.com/{repo}/releases/download/v1.2.3")
        );
    }
}
