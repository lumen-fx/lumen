//! Opt-in profiler installation invoked from `lumenc run --profile <kind>`.
//!
//! Compiled in only when lumenc is built with the `profiling` cargo feature
//! (which also enables `bevy_ecs/trace` so per-system spans exist at all);
//! `--profile tracy` additionally needs `profiling-tracy`. On a default
//! build every mode errors at startup with a rebuild hint - the default
//! binary carries neither span instrumentation nor a subscriber stack.
//!
//! Installs a `tracing` subscriber that captures `bevy_ecs/trace` spans and exports them in one of three formats.
//!
//! - `chrome`: writes `lumen-trace.json` in the cwd in Chrome trace format; open in `chrome://tracing` or Perfetto.
//! - `tracy`: connects to a running `tracy-profiler` GUI over TCP.
//! - `stderr`: line-per-span formatter writing to stderr.

#[cfg(feature = "profiling")]
use std::fs::File;
#[cfg(feature = "profiling")]
use std::io::BufWriter;
#[cfg(feature = "profiling")]
use std::path::PathBuf;
#[cfg(feature = "profiling")]
use tracing_subscriber::EnvFilter;
#[cfg(feature = "profiling")]
use tracing_subscriber::layer::SubscriberExt;
#[cfg(feature = "profiling")]
use tracing_subscriber::util::SubscriberInitExt;

/// Default `tracing` filter applied when `RUST_LOG` is unset.
/// Enables `trace`-level capture for `bevy_ecs`, `lumen`, and `lumenc`; override at runtime via `RUST_LOG`.
#[cfg(feature = "profiling")]
const DEFAULT_PROFILE_FILTER: &str = "bevy_ecs=trace,lumen=trace,lumenc=trace";

#[cfg(feature = "profiling")]
fn profile_filter() -> EnvFilter {
    EnvFilter::try_from_default_env().unwrap_or_else(|_| EnvFilter::new(DEFAULT_PROFILE_FILTER))
}

/// Profiler output mode chosen via `--profile <kind>` on the CLI.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ProfileMode {
    /// Chrome JSON trace exporter - writes `lumen-trace.json`.
    Chrome,
    /// Live Tracy connection - launch `tracy-profiler` to view.
    Tracy,
    /// stderr line dump via `tracing-subscriber`'s default formatter.
    Stderr,
}

impl TryFrom<&str> for ProfileMode {
    type Error = String;
    fn try_from(s: &str) -> Result<Self, Self::Error> {
        match s {
            "chrome" => Ok(Self::Chrome),
            "tracy" => Ok(Self::Tracy),
            "stderr" => Ok(Self::Stderr),
            other => Err(format!(
                "unknown profile mode '{other}' (supported: chrome, tracy, stderr)"
            )),
        }
    }
}

/// Lifetime guard returned by [`install`]. Drop flushes and closes the underlying writer.
#[cfg(feature = "profiling")]
pub struct ProfileGuard {
    chrome: Option<tracing_chrome::FlushGuard>,
    chrome_path: Option<PathBuf>,
}

/// Stub guard for builds without the `profiling` feature - [`install`]
/// always errors before one is constructed, but the type must exist for
/// the CLI's `Option<ProfileGuard>` binding to compile.
#[cfg(not(feature = "profiling"))]
pub struct ProfileGuard {}

#[cfg(feature = "profiling")]
impl Drop for ProfileGuard {
    fn drop(&mut self) {
        // Drop the chrome flush guard first so the writer flushes and closes the file.
        // Then rewrite chrome event names via `rewrite_chrome_names` so spans show the system fn path instead of the generic `system` placeholder.
        self.chrome.take();
        if let Some(path) = self.chrome_path.take()
            && let Err(e) = rewrite_chrome_names(&path)
        {
            eprintln!(
                "lumenc: failed to rewrite chrome trace names ({}): {e}",
                path.display()
            );
        }
    }
}

/// Streams `path` line-by-line, lifting `args.name` into the top-level chrome `name` field via [`lift_args_name`] and rewriting the file in place.
#[cfg(feature = "profiling")]
fn rewrite_chrome_names(path: &std::path::Path) -> std::io::Result<()> {
    use std::io::{BufRead, BufReader, Write};
    let tmp = path.with_extension("json.tmp");
    let reader = BufReader::new(File::open(path)?);
    let mut writer = BufWriter::new(File::create(&tmp)?);
    for line in reader.lines() {
        let mut line = line?;
        if let Some(rewritten) = lift_args_name(&line) {
            line = rewritten;
        }
        writer.write_all(line.as_bytes())?;
        writer.write_all(b"\n")?;
    }
    writer.flush()?;
    drop(writer);
    std::fs::rename(&tmp, path)?;
    Ok(())
}

/// Parses one chrome JSON event line and returns a rewritten line that copies `args.name` into the top-level `name` field.
/// Returns `None` for the array bracket lines and events whose top-level `name` is not one of the generic placeholders.
#[cfg(feature = "profiling")]
fn lift_args_name(line: &str) -> Option<String> {
    let trimmed = line.trim_end_matches(',').trim();
    if trimmed.is_empty() || matches!(trimmed, "[" | "]") {
        return None;
    }
    let trailing_comma = line.trim_end().ends_with(',');
    let mut v: serde_json::Value = serde_json::from_str(trimmed).ok()?;
    let obj = v.as_object_mut()?;
    // Skip events whose top-level `name` is not a generic `tracing-chrome` placeholder.
    let current_name = obj.get("name").and_then(|n| n.as_str())?;
    if !matches!(current_name, "system" | "schedule" | "system_commands") {
        return None;
    }
    let args_name = obj
        .get("args")
        .and_then(|a| a.get("name"))
        .and_then(|n| n.as_str())?
        .trim_matches('"')
        .to_string();
    if args_name.is_empty() {
        return None;
    }
    // Keep the `system_commands` (deferred command apply) span visually
    // distinct from the system's own run. Renaming BOTH to the bare fn
    // path made every system appear to run twice per tick in trace
    // viewers - that artifact was mis-diagnosed as a double registration
    // at least once.
    let lifted = if current_name == "system_commands" {
        format!("{args_name} (commands)")
    } else {
        args_name
    };
    obj.insert("name".into(), serde_json::Value::String(lifted));
    let mut s = serde_json::to_string(&v).ok()?;
    if trailing_comma {
        s.push(',');
    }
    Some(s)
}

#[cfg(all(test, feature = "profiling"))]
mod lift_tests {
    use super::lift_args_name;

    #[test]
    fn system_span_takes_bare_fn_path() {
        let line = r#"{"name":"system","args":{"name":"my_crate::my_system"}},"#;
        let out = lift_args_name(line).unwrap();
        assert!(out.contains(r#""name":"my_crate::my_system""#));
        assert!(out.ends_with(','));
    }

    /// The command-apply span must stay distinguishable from the system's
    /// own run - naming both identically made every system look like it
    /// ran twice per tick in trace viewers.
    #[test]
    fn system_commands_span_keeps_commands_suffix() {
        let line = r#"{"name":"system_commands","args":{"name":"my_crate::my_system"}}"#;
        let out = lift_args_name(line).unwrap();
        assert!(out.contains(r#""name":"my_crate::my_system (commands)""#));
    }

    #[test]
    fn non_placeholder_names_pass_through() {
        let line = r#"{"name":"already_named","args":{"name":"x"}}"#;
        assert!(lift_args_name(line).is_none());
    }
}

/// Installs a global `tracing` subscriber wired to `mode` and returns a [`ProfileGuard`] that flushes on drop.
///
/// Errors on builds without the `profiling` feature (all modes) or without
/// `profiling-tracy` (`--profile tracy`) - spans don't exist in those
/// builds, so silently installing a subscriber would record nothing.
#[cfg(not(feature = "profiling"))]
pub fn install(_mode: ProfileMode) -> Result<ProfileGuard, String> {
    Err(
        "this lumenc was built without profiler support; rebuild with \
         `cargo build -p lumenc --features profiling` (chrome / stderr) or \
         `--features profiling-tracy` (adds tracy)"
            .into(),
    )
}

/// Installs a global `tracing` subscriber wired to `mode` and returns a [`ProfileGuard`] that flushes on drop.
///
/// Errors on builds without the `profiling` feature (all modes) or without
/// `profiling-tracy` (`--profile tracy`) - spans don't exist in those
/// builds, so silently installing a subscriber would record nothing.
#[cfg(feature = "profiling")]
pub fn install(mode: ProfileMode) -> Result<ProfileGuard, String> {
    match mode {
        ProfileMode::Chrome => {
            let path = PathBuf::from("lumen-trace.json");
            let file = File::create(&path).map_err(|e| format!("open {}: {e}", path.display()))?;
            let writer = BufWriter::new(file);
            let (chrome_layer, guard) = tracing_chrome::ChromeLayerBuilder::new()
                .writer(writer)
                .include_args(true)
                .build();
            tracing_subscriber::registry()
                .with(profile_filter())
                .with(chrome_layer)
                .try_init()
                .map_err(|e| format!("install chrome subscriber: {e}"))?;
            eprintln!(
                "lumenc: chrome profiler writing to {} - open in chrome://tracing or https://ui.perfetto.dev",
                path.display()
            );
            Ok(ProfileGuard {
                chrome: Some(guard),
                chrome_path: Some(path),
            })
        }
        #[cfg(feature = "profiling-tracy")]
        ProfileMode::Tracy => {
            // `manual-lifetime` + `delayed-init` on the tracy-client dep
            // mean nothing tracy-related ran before this point (no ctor,
            // no calibration sleep, no worker threads). The layer's
            // construction starts the client here, at explicit opt-in.
            tracing_subscriber::registry()
                .with(profile_filter())
                .with(tracing_tracy::TracyLayer::default())
                .try_init()
                .map_err(|e| format!("install tracy subscriber: {e}"))?;
            eprintln!("lumenc: tracy profiler active - launch `tracy-profiler` to connect");
            Ok(ProfileGuard {
                chrome: None,
                chrome_path: None,
            })
        }
        #[cfg(not(feature = "profiling-tracy"))]
        ProfileMode::Tracy => Err("this lumenc was built without tracy support; rebuild with \
             `cargo build -p lumenc --features profiling-tracy`"
            .into()),
        ProfileMode::Stderr => {
            tracing_subscriber::registry()
                .with(profile_filter())
                .with(
                    tracing_subscriber::fmt::layer()
                        .with_target(false)
                        // Emit one line per span CLOSE with the measured
                        // busy/idle times - without this the fmt layer
                        // only prints events, and bevy_ecs's per-system
                        // instrumentation is all spans, so `--profile
                        // stderr` printed nothing.
                        .with_span_events(tracing_subscriber::fmt::format::FmtSpan::CLOSE)
                        .with_writer(std::io::stderr),
                )
                .try_init()
                .map_err(|e| format!("install stderr subscriber: {e}"))?;
            eprintln!("lumenc: stderr profiler active - one line per system span");
            Ok(ProfileGuard {
                chrome: None,
                chrome_path: None,
            })
        }
    }
}
