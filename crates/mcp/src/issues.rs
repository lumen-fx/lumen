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
//! would read as "zero open issues".

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

/// What `lumen_framework_status` reports about the issue tracker.
///
/// `repo` is `None` only when the checkout's remote couldn't be resolved at
/// all; once a repo is known, a fetch failure still reports it alongside
/// `error` so the summary can say which repository it tried.
pub(crate) struct IssuesReport {
    pub(crate) repo: Option<String>,
    pub(crate) open_issues: Option<usize>,
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
                first_open: Vec::new(),
                error: Some(error),
            };
        }
    };
    match fetch_open_issues(&repo, cfg).await {
        Ok(issues) => {
            let first_open = issues
                .iter()
                .take(10)
                .map(|i| format!("#{} {}", i.number, i.title))
                .collect();
            IssuesReport {
                repo: Some(repo),
                open_issues: Some(issues.len()),
                first_open,
                error: None,
            }
        }
        Err(error) => IssuesReport {
            repo: Some(repo),
            open_issues: None,
            first_open: Vec::new(),
            error: Some(error),
        },
    }
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

/// Fetch every open issue for `repo` ("owner/repo"), bounded by
/// `cfg.timeout`. The child is killed on timeout so an unreachable network
/// never leaves a `gh` process running in the background.
pub(crate) async fn fetch_open_issues(
    repo: &str,
    cfg: &GhConfig,
) -> Result<Vec<OpenIssue>, String> {
    let mut command = tokio::process::Command::new(&cfg.bin);
    command.args([
        "issue",
        "list",
        "--repo",
        repo,
        "--state",
        "open",
        "--limit",
        "200",
        "--json",
        "number,title",
    ]);

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
    /// unreachable network) times out well short of the 5 s it would
    /// otherwise take to finish, and the deadline leaves no child behind.
    #[tokio::test]
    async fn a_hung_process_times_out_instead_of_hanging_the_call() {
        let mut command = tokio::process::Command::new("sleep");
        command.arg("5");
        let start = std::time::Instant::now();
        let err = run_with_timeout(command, Duration::from_millis(200), "sleep")
            .await
            .expect_err("the timeout must fire before sleep 5 finishes");
        assert!(err.contains("did not answer within"), "{err}");
        assert!(
            start.elapsed() < Duration::from_secs(2),
            "the call did not respect the timeout"
        );
    }
}
