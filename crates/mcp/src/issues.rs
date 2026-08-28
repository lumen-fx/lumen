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
//!
//! The only parts of this module that need a live process are the spawn
//! itself and the local `git remote` read; everything else - argument
//! construction, remote-URL parsing, the truncation boundary, and shaping
//! a fetch outcome into an [`IssuesReport`] - is a small pure function so
//! it is covered without a network connection. See the `tests` module.

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
///
/// This function itself is thin glue - resolve, then fetch, then shape -
/// over `origin_repo_slug`, `fetch_open_issues`, and `report_from_fetch_result`,
/// each of which is tested on its own.
pub(crate) async fn framework_issues_report(cfg: &GhConfig) -> IssuesReport {
    let repo = match origin_repo_slug() {
        Ok(repo) => repo,
        Err(error) => return report_from_repo_error(error),
    };
    let fetch = fetch_open_issues(&repo, cfg).await;
    report_from_fetch_result(repo, fetch)
}

/// Shape the "couldn't even resolve a repository" outcome. Pure.
fn report_from_repo_error(error: String) -> IssuesReport {
    IssuesReport {
        repo: None,
        open_issues: None,
        truncated: false,
        first_open: Vec::new(),
        error: Some(error),
    }
}

/// Shape a `fetch_open_issues` outcome for a known `repo` into the report
/// the tool returns. Pure - every branch (fetch failed, fetch succeeded
/// under the cap, fetch succeeded over the cap) is reachable with a
/// synthetic `Result` and no process.
fn report_from_fetch_result(repo: String, fetch: Result<Vec<OpenIssue>, String>) -> IssuesReport {
    match fetch {
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
///
/// This is a local, synchronous read of `.git/config` - no network - so it
/// runs unconditionally in tests rather than needing a fake.
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

/// The `gh issue list` argument vector for `repo`, requesting one more than
/// [`ISSUE_LIMIT`] so `summarize_issues` can tell an exact count from a
/// truncated one. Pure, so the exact flags are covered without spawning
/// anything.
fn gh_issue_list_args(repo: &str) -> Vec<String> {
    vec![
        "issue".to_string(),
        "list".to_string(),
        "--repo".to_string(),
        repo.to_string(),
        "--state".to_string(),
        "open".to_string(),
        "--limit".to_string(),
        (ISSUE_LIMIT + 1).to_string(),
        "--json".to_string(),
        "number,title".to_string(),
    ]
}

/// Turn `gh issue list --json number,title`'s stdout into issues, naming
/// `bin_name` in the error so it reads the same whether `gh` or a test
/// double produced the bytes. Pure.
fn parse_issue_list(bin_name: &str, stdout: &[u8]) -> Result<Vec<OpenIssue>, String> {
    serde_json::from_slice::<Vec<OpenIssue>>(stdout)
        .map_err(|e| format!("could not parse '{bin_name} issue list' output: {e}"))
}

/// Fetch open issues for `repo` ("owner/repo"), up to `ISSUE_LIMIT + 1` of
/// them, bounded by `cfg.timeout`. The child is killed on timeout so an
/// unreachable network never leaves a `gh` process running in the
/// background.
pub(crate) async fn fetch_open_issues(
    repo: &str,
    cfg: &GhConfig,
) -> Result<Vec<OpenIssue>, String> {
    let mut command = tokio::process::Command::new(&cfg.bin);
    command.args(gh_issue_list_args(repo));

    let bin_name = cfg.bin.to_string_lossy().into_owned();
    let (_pid, result) = spawn_with_timeout(command, cfg.timeout, &bin_name).await;
    let output = result?;

    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr);
        return Err(format!(
            "'{bin_name}' exited with {}: {}",
            output.status,
            stderr.trim()
        ));
    }

    parse_issue_list(&bin_name, &output.stdout)
}

/// Spawn `command`, wait for it (or `timeout`, whichever comes first), and
/// report the child's pid alongside the result.
///
/// The pid is `None` only when the process never spawned at all (the
/// "did not run" branch); production callers discard it, and the
/// kill-on-timeout test uses it to confirm the OS process is gone, not
/// merely that this function returned.
///
/// Captures stdout/stderr the same way `Command::output()` does (it is
/// implemented in terms of `spawn()` + `wait_with_output()`, which is what
/// this function does directly so it can read the pid in between) so
/// `fetch_open_issues` still gets `gh`'s JSON on `output.stdout`.
async fn spawn_with_timeout(
    mut command: tokio::process::Command,
    timeout: Duration,
    bin_name: &str,
) -> (Option<u32>, Result<std::process::Output, String>) {
    command
        .kill_on_drop(true)
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped());
    let child = match command.spawn() {
        Ok(child) => child,
        Err(e) => return (None, Err(format!("'{bin_name}' did not run: {e}"))),
    };
    let pid = child.id();
    let result = match tokio::time::timeout(timeout, child.wait_with_output()).await {
        Ok(Ok(output)) => Ok(output),
        Ok(Err(e)) => Err(format!("'{bin_name}' did not run: {e}")),
        Err(_) => Err(format!(
            "'{bin_name}' did not answer within {timeout:?} (offline, or GitHub is unreachable)"
        )),
    };
    (pid, result)
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

    #[test]
    fn gh_args_request_one_more_than_the_limit_with_only_the_needed_fields() {
        assert_eq!(
            gh_issue_list_args("lumen-fx/lumen"),
            vec![
                "issue",
                "list",
                "--repo",
                "lumen-fx/lumen",
                "--state",
                "open",
                "--limit",
                "201",
                "--json",
                "number,title",
            ]
        );
    }

    #[test]
    fn valid_stdout_parses_into_issues() {
        let issues = parse_issue_list("gh", br#"[{"number":1,"title":"a"}]"#).unwrap();
        assert_eq!(issues.len(), 1);
        assert_eq!(issues[0].number, 1);
        assert_eq!(issues[0].title, "a");
    }

    #[test]
    fn unparseable_stdout_is_reported_with_the_binary_name() {
        let err = parse_issue_list("gh", b"not json").expect_err("garbage must not parse");
        assert!(
            err.contains("could not parse 'gh issue list' output"),
            "{err}"
        );
    }

    #[test]
    fn repo_error_report_has_no_repo_and_no_count() {
        let report = report_from_repo_error("no origin".to_string());
        assert_eq!(report.repo, None);
        assert_eq!(report.open_issues, None);
        assert!(!report.truncated);
        assert!(report.first_open.is_empty());
        assert_eq!(report.error.as_deref(), Some("no origin"));
    }

    #[test]
    fn fetch_error_report_keeps_the_repo_but_no_count() {
        let report =
            report_from_fetch_result("lumen-fx/lumen".to_string(), Err("boom".to_string()));
        assert_eq!(report.repo.as_deref(), Some("lumen-fx/lumen"));
        assert_eq!(report.open_issues, None);
        assert!(!report.truncated);
        assert_eq!(report.error.as_deref(), Some("boom"));
    }

    #[test]
    fn fetch_success_report_carries_the_count_and_first_titles() {
        let issues = vec![
            OpenIssue {
                number: 1,
                title: "a".into(),
            },
            OpenIssue {
                number: 2,
                title: "b".into(),
            },
        ];
        let report = report_from_fetch_result("lumen-fx/lumen".to_string(), Ok(issues));
        assert_eq!(report.repo.as_deref(), Some("lumen-fx/lumen"));
        assert_eq!(report.open_issues, Some(2));
        assert!(!report.truncated);
        assert_eq!(
            report.first_open,
            vec!["#1 a".to_string(), "#2 b".to_string()]
        );
        assert!(report.error.is_none());
    }

    #[test]
    fn fetch_success_report_marks_truncation() {
        let issues: Vec<OpenIssue> = (0..(ISSUE_LIMIT as u64 + 1))
            .map(|n| OpenIssue {
                number: n,
                title: format!("issue {n}"),
            })
            .collect();
        let report = report_from_fetch_result("lumen-fx/lumen".to_string(), Ok(issues));
        assert_eq!(report.open_issues, Some(ISSUE_LIMIT));
        assert!(report.truncated);
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

    /// Local and synchronous: `git remote get-url origin` only reads
    /// `.git/config`, so this resolves (or fails) the same way in CI as on
    /// a dev machine, without any network. Forks and mirrors carry
    /// different remotes, so this asserts the shape of a resolved slug
    /// rather than a specific owner/repo; the non-GitHub-remote error path
    /// is covered on fixed input by `rejects_a_non_github_remote`.
    #[test]
    fn origin_repo_slug_resolves_locally_without_a_network_call() {
        match origin_repo_slug() {
            Ok(slug) => {
                let parts: Vec<&str> = slug.split('/').collect();
                assert_eq!(parts.len(), 2, "expected 'owner/repo', got {slug}");
                assert!(!parts[0].is_empty() && !parts[1].is_empty());
            }
            Err(e) => assert!(!e.is_empty()),
        }
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

    /// `framework_issues_report` is thin glue over pieces already tested on
    /// their own; this drives it end to end once with a `gh` binary that
    /// cannot possibly succeed, so it lands in whichever error branch this
    /// checkout's own `origin` remote leads to - deterministic either way,
    /// and no network call either way.
    #[tokio::test]
    async fn framework_issues_report_always_carries_an_error_when_gh_cannot_run() {
        let cfg = GhConfig {
            bin: OsString::from("lumen-mcp-test-nonexistent-gh-binary"),
            timeout: Duration::from_secs(2),
        };
        let report = framework_issues_report(&cfg).await;
        assert!(report.error.is_some());
        assert!(report.open_issues.is_none());
    }

    /// `git` is already a hard dependency of `origin_repo_slug`, so it is
    /// guaranteed present wherever this suite runs, needs no network, and
    /// exits fast either way - which makes it a reliable stand-in for `gh`
    /// on both sides of `fetch_open_issues`'s success/failure split: `git
    /// --version` exits 0 for the "fast success" case, and `git`
    /// interpreting `gh`'s fixed arguments as an unknown subcommand exits
    /// non-zero for the "process ran but failed" case. Neither needs a
    /// mock, since both are real, deterministic exits of a real process.
    #[tokio::test]
    async fn a_fast_process_returns_ok_before_the_timeout() {
        let mut command = tokio::process::Command::new("git");
        command.arg("--version");
        let (pid, result) = spawn_with_timeout(command, Duration::from_secs(5), "git").await;
        assert!(pid.is_some(), "a spawned process must report a pid");
        let output = result.expect("git --version should succeed quickly");
        assert!(output.status.success());
    }

    #[tokio::test]
    async fn a_nonzero_exit_is_reported_with_the_binary_name() {
        let cfg = GhConfig {
            bin: OsString::from("git"),
            timeout: Duration::from_secs(5),
        };
        // `fetch_open_issues` always appends the fixed `gh issue list ...`
        // arguments regardless of `cfg.bin`; git rejects `issue` as an
        // unknown subcommand immediately, no network involved.
        let err = fetch_open_issues("lumen-fx/lumen", &cfg)
            .await
            .expect_err("git does not understand gh's arguments");
        assert!(err.contains("exited with"), "{err}");
    }

    /// A process that never answers (standing in for `gh` stuck on an
    /// unreachable network) times out well short of the time it would
    /// otherwise take to finish, and the timed-out child is killed rather
    /// than left running in the background - checked directly against the
    /// OS process table, not inferred from how quickly this call returned.
    ///
    /// The long-lived child is a real, standalone executable on every
    /// platform the workspace targets rather than a shell script: `sleep`
    /// on Unix, `ping` (which idles about a second between each echo) on
    /// Windows. Neither needs a shell, so this runs - and proves the same
    /// property - on every platform in the test matrix, not just the ones
    /// with a POSIX shell.
    #[tokio::test]
    async fn a_timed_out_process_is_killed_not_left_running() {
        let command = spawn_long_lived_command();
        let start = std::time::Instant::now();
        let (pid, result) =
            spawn_with_timeout(command, Duration::from_millis(200), "sleeper").await;
        let err = result.expect_err("the timeout must fire well before the sleeper finishes");
        assert!(err.contains("did not answer within"), "{err}");
        assert!(
            start.elapsed() < Duration::from_secs(2),
            "the call did not respect the timeout"
        );

        let pid = pid.expect("the sleeper must have spawned to be worth killing");
        // The long-lived child's own timeline is several seconds; this
        // check runs well inside that window, so if `kill_on_drop` had not
        // terminated it, it would still show up as alive here. Give the
        // async reaper a brief moment to finish the kill.
        tokio::time::sleep(Duration::from_millis(300)).await;
        assert!(
            !process_is_alive(pid),
            "pid {pid} is still running after its timeout fired - it was not killed"
        );
    }

    /// A process that keeps running for several seconds, with no shell
    /// involved: `sleep` and `ping` are both real, standalone executables.
    fn spawn_long_lived_command() -> tokio::process::Command {
        if cfg!(windows) {
            let mut c = tokio::process::Command::new("ping");
            c.args(["-n", "6", "127.0.0.1"]);
            c
        } else {
            let mut c = tokio::process::Command::new("sleep");
            c.arg("5");
            c
        }
    }

    /// Portable "is this OS process still running" probe, used only by the
    /// kill-on-timeout test above. Neither the tool nor production code
    /// ever needs to ask this about a process it did not itself just wait
    /// on - this exists purely to verify `kill_on_drop`'s effect from
    /// outside the process that spawned the child.
    fn process_is_alive(pid: u32) -> bool {
        if cfg!(windows) {
            std::process::Command::new("tasklist")
                .args(["/FI", &format!("PID eq {pid}"), "/NH"])
                .output()
                .map(|out| String::from_utf8_lossy(&out.stdout).contains(&pid.to_string()))
                .unwrap_or(false)
        } else {
            std::process::Command::new("kill")
                .args(["-0", &pid.to_string()])
                .stdout(std::process::Stdio::null())
                .stderr(std::process::Stdio::null())
                .status()
                .map(|status| status.success())
                .unwrap_or(false)
        }
    }
}
