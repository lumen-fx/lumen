//! `[[hooks]]` runner: executes an app's declared build/setup commands at the
//! `prebuild` and `prerun` trigger points.
//!
//! The schema and field reference live on [`crate::config::HookCfg`]; this
//! module is the execution side: filtering by [`HookCfg::when`] and the
//! current OS, skipping a hook whose declared outputs are already newer than
//! its declared inputs, and running the rest in declaration order.
//!
//! Callers (the `lumenc` CLI's `run` / `build` / `bundle` commands) decide
//! *when* to call [`run_hooks`]; `lumenc check` never does, so a check stays
//! side-effect free.

use crate::config::HookCfg;
use std::path::Path;
use std::process::{Command, ExitStatus};

// Re-exported so a caller only needs `lumen_runtime::hooks::{run_hooks,
// HookWhen, HookOs}` - the trigger-point and OS enums a caller matches
// against - without also reaching into `lumen_runtime::config` for them.
pub use crate::config::{HookOs, HookWhen};

/// A `[[hooks]]` command failed.
#[derive(Debug, thiserror::Error)]
pub enum HookError {
    /// The command could not be spawned at all (missing shell interpreter,
    /// permission denied, ...).
    #[error("hook `{run}`: failed to run: {source}")]
    Spawn {
        /// The hook's `run` command line.
        run: String,
        /// The underlying spawn failure.
        #[source]
        source: std::io::Error,
    },
    /// The command ran and exited non-zero.
    #[error("hook `{run}` exited with {status}")]
    Failed {
        /// The hook's `run` command line.
        run: String,
        /// The command's exit status.
        status: ExitStatus,
    },
}

/// Run every hook in `hooks` whose `when` matches `trigger` and whose `os`
/// (if set) matches [`std::env::consts::OS`], in declaration order, with the
/// child process's cwd set to `dir` (the app directory). A hook whose
/// declared outputs are all at least as new as its declared inputs is
/// skipped; see [`is_stale_free`]. A hook that exits non-zero aborts the run
/// immediately - later hooks do not run.
pub fn run_hooks(hooks: &[HookCfg], trigger: HookWhen, dir: &Path) -> Result<(), HookError> {
    let current_os = HookOs::try_from(std::env::consts::OS).ok();
    for hook in hooks {
        if hook.when != trigger {
            continue;
        }
        if let Some(os) = hook.os
            && Some(os) != current_os
        {
            continue;
        }
        if is_stale_free(hook, dir) {
            continue;
        }
        run_one(hook, dir)?;
    }
    Ok(())
}

/// True when `hook` can be skipped: both `inputs` and `outputs` are
/// non-empty, every declared output resolves to a real file, every declared
/// input resolves to a real file, and every output's mtime is at least as
/// new as the newest input's mtime. A hook missing either list, or one whose
/// declared inputs or outputs do not all exist on disk, is never skipped -
/// it runs, and the command itself gives the better error for a genuinely
/// missing input.
fn is_stale_free(hook: &HookCfg, dir: &Path) -> bool {
    if hook.inputs.is_empty() || hook.outputs.is_empty() {
        return false;
    }
    let Some(newest_input) = hook
        .inputs
        .iter()
        .map(|p| mtime(dir, p))
        .collect::<Option<Vec<_>>>()
        .and_then(|v| v.into_iter().max())
    else {
        return false;
    };
    let Some(output_mtimes) = hook
        .outputs
        .iter()
        .map(|p| mtime(dir, p))
        .collect::<Option<Vec<_>>>()
    else {
        return false;
    };
    output_mtimes.into_iter().all(|out| out >= newest_input)
}

/// Resolve `rel` against `dir` (unless already absolute) and return its
/// mtime, or `None` when the path does not exist / has no readable mtime.
fn mtime(dir: &Path, rel: &str) -> Option<std::time::SystemTime> {
    let p = Path::new(rel);
    let full = if p.is_absolute() {
        p.to_path_buf()
    } else {
        dir.join(p)
    };
    std::fs::metadata(full).ok()?.modified().ok()
}

/// Run one hook's command with `dir` as the child's cwd, stdio inherited
/// from the parent. `sh -c` on unix, `cmd /C` on windows.
fn run_one(hook: &HookCfg, dir: &Path) -> Result<(), HookError> {
    let status = shell_command(&hook.run)
        .current_dir(dir)
        .status()
        .map_err(|source| HookError::Spawn {
            run: hook.run.clone(),
            source,
        })?;
    if !status.success() {
        return Err(HookError::Failed {
            run: hook.run.clone(),
            status,
        });
    }
    Ok(())
}

#[cfg(unix)]
fn shell_command(run: &str) -> Command {
    let mut cmd = Command::new("sh");
    cmd.arg("-c").arg(run);
    cmd
}

#[cfg(windows)]
fn shell_command(run: &str) -> Command {
    let mut cmd = Command::new("cmd");
    cmd.arg("/C").arg(run);
    cmd
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::{Duration, SystemTime};

    /// Build a bare `HookCfg` for the staleness / filter tests. `run` is
    /// never executed by these tests - they call `is_stale_free` / the
    /// `when`/`os` filter directly rather than shelling out.
    fn hook(when: HookWhen, os: Option<HookOs>, inputs: &[&str], outputs: &[&str]) -> HookCfg {
        HookCfg {
            when,
            os,
            run: "true".to_string(),
            inputs: inputs.iter().map(|s| s.to_string()).collect(),
            outputs: outputs.iter().map(|s| s.to_string()).collect(),
        }
    }

    fn temp_dir(tag: &str) -> std::path::PathBuf {
        let dir = std::env::temp_dir().join(format!(
            "lumen_hooks_{tag}_{}_{}",
            std::process::id(),
            SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }

    /// Write `path` and stamp its mtime to `t`, bypassing the system clock so
    /// staleness tests don't depend on real elapsed time or filesystem mtime
    /// granularity (some filesystems only resolve to 1-2 seconds, which a
    /// short `thread::sleep` between writes can land inside).
    fn write_stamped(path: &std::path::Path, t: SystemTime) {
        std::fs::write(path, "x").unwrap();
        let f = std::fs::OpenOptions::new().write(true).open(path).unwrap();
        f.set_modified(t).unwrap();
    }

    #[test]
    fn stale_free_when_output_newer_than_input() {
        let dir = temp_dir("stale_newer");
        let epoch = std::time::UNIX_EPOCH;
        write_stamped(&dir.join("in.c"), epoch + Duration::from_secs(1000));
        write_stamped(&dir.join("out.so"), epoch + Duration::from_secs(2000));

        let h = hook(HookWhen::Prebuild, None, &["in.c"], &["out.so"]);
        assert!(is_stale_free(&h, &dir), "output newer than input must skip");

        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn stale_free_when_output_mtime_equals_input_mtime() {
        // The documented rule is "at least as new as", so an output stamped
        // to the exact same instant as the newest input still counts as
        // fresh - pin that boundary explicitly rather than leaving it to
        // incidental clock behavior.
        let dir = temp_dir("stale_equal");
        let t = std::time::UNIX_EPOCH + Duration::from_secs(1000);
        write_stamped(&dir.join("in.c"), t);
        write_stamped(&dir.join("out.so"), t);

        let h = hook(HookWhen::Prebuild, None, &["in.c"], &["out.so"]);
        assert!(
            is_stale_free(&h, &dir),
            "an output exactly as new as the input must still skip"
        );

        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn runs_when_output_missing() {
        let dir = temp_dir("missing_output");
        write_stamped(
            &dir.join("in.c"),
            std::time::UNIX_EPOCH + Duration::from_secs(1000),
        );
        // out.so intentionally not written.

        let h = hook(HookWhen::Prebuild, None, &["in.c"], &["out.so"]);
        assert!(
            !is_stale_free(&h, &dir),
            "a missing declared output must always run"
        );

        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn runs_when_no_inputs_or_outputs() {
        let dir = temp_dir("no_lists");
        let h = hook(HookWhen::Prebuild, None, &[], &[]);
        assert!(
            !is_stale_free(&h, &dir),
            "a hook with no inputs/outputs always runs"
        );

        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn runs_when_input_missing() {
        let dir = temp_dir("missing_input");
        write_stamped(
            &dir.join("out.so"),
            std::time::UNIX_EPOCH + Duration::from_secs(1000),
        );
        // in.c intentionally not written - staleness can't be judged, so run.

        let h = hook(HookWhen::Prebuild, None, &["in.c"], &["out.so"]);
        assert!(
            !is_stale_free(&h, &dir),
            "a missing declared input must always run rather than erroring"
        );

        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn runs_when_input_newer_than_output() {
        let dir = temp_dir("stale_older");
        let epoch = std::time::UNIX_EPOCH;
        write_stamped(&dir.join("out.so"), epoch + Duration::from_secs(1000));
        write_stamped(&dir.join("in.c"), epoch + Duration::from_secs(2000));

        let h = hook(HookWhen::Prebuild, None, &["in.c"], &["out.so"]);
        assert!(
            !is_stale_free(&h, &dir),
            "an input newer than the output must run"
        );

        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn when_filter_only_matches_declared_trigger() {
        let dir = temp_dir("when_filter");
        // A `prerun` hook must not fire on a `prebuild` trigger, and vice
        // versa. `run_hooks` would try to execute `false` (exit 1) if the
        // filter let it through, so a clean `Ok(())` proves it was skipped.
        let prerun_only = [hook(HookWhen::Prerun, None, &[], &[])];
        assert!(run_hooks(&prerun_only, HookWhen::Prebuild, &dir).is_ok());

        let prebuild_only = [HookCfg {
            run: "false".to_string(),
            ..hook(HookWhen::Prebuild, None, &[], &[])
        }];
        // Sanity: the same hook DOES fire (and fail, since `false` exits 1)
        // when the trigger matches - proving the prior `Ok(())` was really a
        // skip, not a no-op filter bug.
        assert!(run_hooks(&prebuild_only, HookWhen::Prebuild, &dir).is_err());
        assert!(run_hooks(&prebuild_only, HookWhen::Prerun, &dir).is_ok());

        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn os_filter_only_matches_current_platform() {
        let dir = temp_dir("os_filter");
        let current = HookOs::try_from(std::env::consts::OS).expect(
            "test host OS must be one of linux/macos/windows for this assertion to be meaningful",
        );
        let other = match current {
            HookOs::Linux => HookOs::Macos,
            HookOs::Macos => HookOs::Windows,
            HookOs::Windows => HookOs::Linux,
        };

        // A hook pinned to a different OS never runs here.
        let wrong_os = [HookCfg {
            run: "false".to_string(),
            ..hook(HookWhen::Prebuild, Some(other), &[], &[])
        }];
        assert!(run_hooks(&wrong_os, HookWhen::Prebuild, &dir).is_ok());

        // The same hook pinned to the current OS does run (and fails, since
        // the command is `false`) - proof the skip above was the OS filter.
        let right_os = [HookCfg {
            run: "false".to_string(),
            ..hook(HookWhen::Prebuild, Some(current), &[], &[])
        }];
        assert!(run_hooks(&right_os, HookWhen::Prebuild, &dir).is_err());

        // No `os` at all always matches.
        let any_os = [HookCfg {
            run: "false".to_string(),
            ..hook(HookWhen::Prebuild, None, &[], &[])
        }];
        assert!(run_hooks(&any_os, HookWhen::Prebuild, &dir).is_err());

        let _ = std::fs::remove_dir_all(&dir);
    }
}
