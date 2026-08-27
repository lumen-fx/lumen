//! GitHub issue lookups behind `lumen_framework_status`.
//!
//! The tool used to count checkboxes in a `TODO.md` that no checkout
//! carries, so it always reported zero. It now counts open issues
//! on the repository the checkout's `origin` git remote points at, fetched
//! through the `gh` CLI - the tool this machine already has authenticated,
//! and the smallest thing that answers the question without this crate
//! inventing its own GitHub credential handling.
//!
//! Both steps report a clear error instead of guessing: no `origin` remote,
//! a remote that isn't on github.com, a missing `gh` binary, and a `gh` call
//! that times out (no network, unreachable GitHub) all come back as a
//! string in [`IssuesReport::error`] rather than an empty issue list that
//! would read as "zero open issues". A repository with more open issues
//! than [`ISSUE_LIMIT`] is likewise distinguishable from an exact count -
//! see [`IssuesReport::truncated`].

use std::ffi::OsString;
use std::time::Duration;

use serde::Deserialize;

/// How `fetch_open_issues` runs `gh`. A field rather than a constant so a
/// test can point `bin` at a binary that does not exist and exercise the
/// "cannot reach it" path without a network connection.
pub(crate) struct GhConfig {
    pub(crate) bin: OsString,
    pub(crate) timeout: Duration,
}

impl Default for GhConfig {
    fn default() -> Self {
        Self {
            bin: OsString::from("gh"),
            timeout: Duration::from_secs(5),
        }
    }
}

/// One open issue, trimmed to what the status summary shows.
#[derive(Debug, Deserialize)]
pub(crate) struct OpenIssue {
    pub(crate) number: u64,
    pub(crate) title: String,
}

/// Cap on how many open issues a single call counts exactly. `fetch_open_issues`
/// asks `gh` for one more than this, so a result of `ISSUE_LIMIT + 1` raw
/// issues tells `summarize_issues` the repository has more than the cap and
/// the count is a floor, not an exact total - see [`IssuesReport::truncated`].
const ISSUE_LIMIT: usize = 200;

/// What `lumen_framework_status` reports about the issue tracker.
///
/// `repo` is `None` only when the checkout's remote couldn't be resolved at
/// all; once a repo is known, a fetch failure still reports it alongside
/// `error` so the summary can say which repository it tried.
pub(crate) struct IssuesReport {
    pub(crate) repo: Option<String>,
    /// `Some(n)` on success: exactly `n` when `truncated` is `false`, or a
    /// floor of `n` (== `ISSUE_LIMIT`) when it's `true` - the repository has
    /// more than `n` open issues, not exactly `n`.
    pub(crate) open_issues: Option<usize>,
    /// `true` when the repository has more open issues than `ISSUE_LIMIT`,
    /// so `open_issues` is a lower bound rather than an exact count.
    pub(crate) truncated: bool,
    pub(crate) first_open: Vec<String>,
    pub(crate) error: Option<String>,
}

/// Resolve the repository, fetch its open issues, and shape the result the
/// `lumen_framework_status` tool returns. Never panics and never blocks
/// longer than `cfg.timeout` plus the (effectively instant) local git call.
pub(crate) async fn framework_issues_report(cfg: &GhConfig) -> IssuesReport {
    let repo = match origin_repo_slug() {
        Ok(repo) => repo,
        Err(error) => {
            return IssuesReport {
                repo: None,
                open_issues: None,
                truncated: false,
                first_open: Vec::new(),
                error: Some(error),
            };
        }
    };
    match fetch_open_issues(&repo, cfg).await {
        Ok(issues) => {
            let (open_issues, truncated, first_open) = summarize_issues(&issues);
            IssuesReport {
                repo: Some(repo),
                open_issues: Some(open_issues),
                truncated,
                first_open,
                error: None,
            }
        }
        Err(error) => IssuesReport {
            repo: Some(repo),
            open_issues: None,
            truncated: false,
            first_open: Vec::new(),
            error: Some(error),
        },
    }
}

/// Turn a raw issue list (up to `ISSUE_LIMIT + 1` entries - see
/// `fetch_open_issues`) into `(count, truncated, first-10 titles)`. A raw
/// list longer than `ISSUE_LIMIT` means the repository has more open issues
/// than the cap, so the count is reported as the cap itself with
/// `truncated: true` rather than as an exact number.
fn summarize_issues(issues: &[OpenIssue]) -> (usize, bool, Vec<String>) {
    let truncated = issues.len() > ISSUE_LIMIT;
    let open_issues = issues.len().min(ISSUE_LIMIT);
    let first_open = issues
        .iter()
        .take(10)
        .map(|i| format!("#{} {}", i.number, i.title))
        .collect();
    (open_issues, truncated, first_open)
}

/// Resolve the `owner/repo` this checkout's `origin` remote points at.
///
/// `git remote get-url origin` walks up from the current directory to the
/// enclosing repository the same way every other git command does, so this
/// needs no directory-walking of its own - unlike the old `TODO.md` search,
/// it can't land on a directory that merely happens to hold a same-named
/// file. A checkout with no `origin` remote, or one that isn't a
/// `github.com` URL, is reported as such rather than guessed at.
pub(crate) fn origin_repo_slug() -> Result<String, String> {
    let output = std::process::Command::new("git")
        .args(["remote", "get-url", "origin"])
        .output()
        .map_err(|e| format!("git not runnable: {e}"))?;
    if !output.status.success() {
        return Err("no 'origin' git remote (not a git checkout, or origin is unset)".into());
    }
    let url = String::from_utf8_lossy(&output.stdout).trim().to_string();
    parse_github_slug(&url)
        .ok_or_else(|| format!("origin remote '{url}' does not point at github.com"))
}

/// Pull `owner/repo` out of an `https://github.com/...`, `git@github.com:...`,
/// or `ssh://git@github.com/...` remote URL.
fn parse_github_slug(url: &str) -> Option<String> {
    let rest = url
        .strip_prefix("git@github.com:")
        .or_else(|| url.strip_prefix("ssh://git@github.com/"))
        .or_else(|| url.strip_prefix("https://github.com/"))
        .or_else(|| url.strip_prefix("http://github.com/"))?;
    let rest = rest.strip_suffix(".git").unwrap_or(rest);
    let (owner, repo) = rest.trim_matches('/').split_once('/')?;
    if owner.is_empty() || repo.is_empty() {
        return None;
    }
    Some(format!("{owner}/{repo}"))
}

/// Fetch open issues for `repo` ("owner/repo"), up to `ISSUE_LIMIT + 1` of
/// them, bounded by `cfg.timeout`. The child is killed on timeout so an
/// unreachable network never leaves a `gh` process running in the
/// background. Asking for one more than the cap is what lets
/// `summarize_issues` tell an exact count from a truncated one.
pub(crate) async fn fetch_open_issues(
    repo: &str,
    cfg: &GhConfig,
) -> Result<Vec<OpenIssue>, String> {
    let request_limit = (ISSUE_LIMIT + 1).to_string();
    let mut command = tokio::process::Command::new(&cfg.bin);
    command
        .arg("issue")
        .arg("list")
        .arg("--repo")
        .arg(repo)
        .arg("--state")
        .arg("open")
        .arg("--limit")
        .arg(&request_limit)
        .arg("--json")
        .arg("number,title");

    let bin_name = cfg.bin.to_string_lossy().into_owned();
    let output = run_with_timeout(command, cfg.timeout, &bin_name).await?;

    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr);
        return Err(format!(
            "'{bin_name}' exited with {}: {}",
            output.status,
            stderr.trim()
        ));
    }

    serde_json::from_slice::<Vec<OpenIssue>>(&output.stdout)
        .map_err(|e| format!("could not parse '{bin_name} issue list' output: {e}"))
}

/// Run `command` to completion, killing it if `timeout` elapses first so an
/// unreachable network never leaves a background process behind.
async fn run_with_timeout(
    mut command: tokio::process::Command,
    timeout: Duration,
    bin_name: &str,
) -> Result<std::process::Output, String> {
    command.kill_on_drop(true);
    match tokio::time::timeout(timeout, command.output()).await {
        Ok(Ok(output)) => Ok(output),
        Ok(Err(e)) => Err(format!("'{bin_name}' did not run: {e}")),
        Err(_) => Err(format!(
            "'{bin_name}' did not answer within {timeout:?} (offline, or GitHub is unreachable)"
        )),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_https_remote() {
        assert_eq!(
            parse_github_slug("https://github.com/lumen-fx/lumen"),
            Some("lumen-fx/lumen".to_string())
        );
    }

    #[test]
    fn parses_https_remote_with_dot_git_suffix() {
        assert_eq!(
            parse_github_slug("https://github.com/lumen-fx/lumen.git"),
            Some("lumen-fx/lumen".to_string())
        );
    }

    #[test]
    fn parses_ssh_shorthand_remote() {
        assert_eq!(
            parse_github_slug("git@github.com:lumen-fx/lumen.git"),
            Some("lumen-fx/lumen".to_string())
        );
    }

    #[test]
    fn parses_ssh_url_remote() {
        assert_eq!(
            parse_github_slug("ssh://git@github.com/lumen-fx/lumen.git"),
            Some("lumen-fx/lumen".to_string())
        );
    }

    #[test]
    fn rejects_a_non_github_remote() {
        assert_eq!(
            parse_github_slug("https://git.fizzwizzledazzle.dev/LumenUI/lumen.git"),
            None
        );
    }

    /// The offline path that matters most: no network, no `gh`, no
    /// credentials. Pointing `bin` at a binary that cannot possibly exist
    /// exercises the same "cannot reach it" branch a disconnected machine
    /// hits, deterministically and without a network call.
    #[tokio::test]
    async fn missing_gh_binary_reports_a_clear_error() {
        let cfg = GhConfig {
            bin: OsString::from("lumen-mcp-test-nonexistent-gh-binary"),
            timeout: Duration::from_secs(2),
        };
        let err = fetch_open_issues("lumen-fx/lumen", &cfg)
            .await
            .expect_err("a nonexistent binary must not succeed");
        assert!(err.contains("did not run"), "unexpected message: {err}");
    }

    /// A process that never answers (standing in for `gh` stuck on an
    /// unreachable network) times out well short of the time it would
    /// otherwise take to finish, and the timed-out child is killed rather
    /// than left running in the background.
    ///
    /// The child is a shell that sleeps, then touches a marker file; the
    /// timeout is set to fire during the sleep, well before the touch. If
    /// `kill_on_drop` failed to kill the process, an orphaned shell would
    /// keep running past the timeout and eventually create the marker
    /// anyway - so waiting past the shell's own timeline and finding no
    /// marker is what proves the process is gone, not merely that this
    /// call returned quickly.
    #[tokio::test]
    async fn a_timed_out_process_is_killed_not_left_running() {
        let marker = std::env::temp_dir().join(format!(
            "lumen-mcp-issues-test-kill-marker-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap_or_default()
                .as_nanos()
        ));
        let _ = std::fs::remove_file(&marker);

        // The marker path is passed as `$0` rather than interpolated into
        // the script text, so nothing about it needs shell quoting.
        let mut command = tokio::process::Command::new("sh");
        command
            .arg("-c")
            .arg("sleep 0.4 && touch \"$0\"")
            .arg(&marker);

        let start = std::time::Instant::now();
        let err = run_with_timeout(command, Duration::from_millis(50), "sh")
            .await
            .expect_err("the timeout must fire well before the 0.4s sleep completes");
        assert!(err.contains("did not answer within"), "{err}");
        assert!(
            start.elapsed() < Duration::from_secs(1),
            "the call did not respect the timeout"
        );

        // Give an unkilled shell far more time than its own 0.4s timeline
        // needs to create the marker, then confirm it never did.
        tokio::time::sleep(Duration::from_millis(900)).await;
        assert!(
            !marker.exists(),
            "the process kept running past its timeout - it was not killed"
        );
        let _ = std::fs::remove_file(&marker);
    }

    #[test]
    fn exactly_the_limit_is_reported_as_an_exact_count() {
        let issues: Vec<OpenIssue> = (0..ISSUE_LIMIT as u64)
            .map(|n| OpenIssue {
                number: n,
                title: format!("issue {n}"),
            })
            .collect();
        let (count, truncated, first_open) = summarize_issues(&issues);
        assert_eq!(count, ISSUE_LIMIT);
        assert!(!truncated);
        assert_eq!(first_open.len(), 10);
    }

    #[test]
    fn one_more_than_the_limit_is_reported_as_truncated() {
        let issues: Vec<OpenIssue> = (0..(ISSUE_LIMIT as u64 + 1))
            .map(|n| OpenIssue {
                number: n,
                title: format!("issue {n}"),
            })
            .collect();
        let (count, truncated, _) = summarize_issues(&issues);
        // The count is reported as the cap, not the raw (over-fetched)
        // length - `open_issues` is a floor, never a number nothing can
        // confirm.
        assert_eq!(count, ISSUE_LIMIT);
        assert!(truncated);
    }
}
