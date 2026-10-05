//! Builtins whose subsystem the running engine was linked without.
//!
//! A packaged app can ship an engine relinked for it, carrying only the
//! optional subsystems it uses. A builtin stays callable whether or not its
//! subsystem is linked in: its body queues a command, and with the subsystem
//! gone no system applies that command, so the call does nothing. This module
//! is how such a call is told why, once per builtin.
//!
//! What it knows is what the app's artifact recorded: each left-out subsystem's
//! name and the markers its selection rule scans sources for. Both are opaque
//! strings here; nothing in this crate names a subsystem. The check sits in
//! [`ScriptFn::invoke_into`](crate::ScriptFn::invoke_into), which every host
//! adapter goes through, so it reaches every language the same way.

use std::collections::HashSet;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Mutex, RwLock};

use lumen_core::warn_line;

/// Whether anything was recorded. Read on every builtin call, so the usual
/// answer, nothing, costs one relaxed load.
static ANY: AtomicBool = AtomicBool::new(false);

/// What the engine was linked without: a subsystem's name and its markers.
static UNLINKED: RwLock<Vec<(String, Vec<String>)>> = RwLock::new(Vec::new());

/// The builtins already warned about, so each warns once per process.
static WARNED: Mutex<Option<HashSet<String>>> = Mutex::new(None);

/// Record the subsystems the running engine was linked without, each with the
/// markers that reach it. Replaces what an earlier call recorded; an empty
/// list turns the check off.
pub fn record_unlinked(unlinked: impl IntoIterator<Item = (String, Vec<String>)>) {
    let unlinked: Vec<(String, Vec<String>)> = unlinked.into_iter().collect();
    ANY.store(!unlinked.is_empty(), Ordering::Relaxed);
    *UNLINKED.write().unwrap_or_else(|e| e.into_inner()) = unlinked;
}

/// The subsystem a call to `builtin` needs and the engine lacks, if any.
pub fn unlinked_subsystem(builtin: &str) -> Option<String> {
    if !ANY.load(Ordering::Relaxed) {
        return None;
    }
    UNLINKED
        .read()
        .unwrap_or_else(|e| e.into_inner())
        .iter()
        .find(|(_, markers)| markers.iter().any(|m| reaches(builtin, m)))
        .map(|(name, _)| name.clone())
}

/// Warn, once per builtin, when a call to `builtin` needs a subsystem the
/// engine was linked without.
pub(crate) fn warn_if_unlinked(builtin: &str) {
    let Some(subsystem) = unlinked_subsystem(builtin) else {
        return;
    };
    let mut warned = WARNED.lock().unwrap_or_else(|e| e.into_inner());
    if warned
        .get_or_insert_with(HashSet::new)
        .insert(builtin.to_string())
    {
        warn_line!(
            "lumen: warning: `{builtin}` does nothing here: this package was built without \
             {subsystem}. Name it in lumen.toml [capabilities] to package it in."
        );
    }
}

/// Whether a call to `builtin` contains `marker`, which is the question the
/// source scan asked of the app's text. A marker ending in `(` is a call to a
/// builtin of exactly that name or ending in it; a bare marker is part of
/// every builtin name holding it.
fn reaches(builtin: &str, marker: &str) -> bool {
    match marker.strip_suffix('(') {
        Some(callee) => !callee.is_empty() && builtin.ends_with(callee),
        None => !marker.is_empty() && builtin.contains(marker),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_marker_reaches_the_builtins_a_scan_would_have_counted() {
        assert!(reaches("tray_icon", "tray_icon"));
        assert!(reaches("tray_icon_menu", "tray_icon"));
        assert!(!reaches("notify", "tray_icon"));
        assert!(reaches("fetch", "fetch("));
        assert!(!reaches("fetch_all", "fetch("));
        assert!(!reaches("anything", ""));
        assert!(!reaches("anything", "("));
    }

    /// One test for the whole process-global table, so no other test in this
    /// binary races it.
    #[test]
    fn a_recorded_subsystem_answers_for_its_builtins_until_cleared() {
        assert_eq!(unlinked_subsystem("tray_icon"), None);
        record_unlinked([(
            "os-tray".to_string(),
            vec!["tray_icon".to_string(), "fetch(".to_string()],
        )]);
        assert_eq!(unlinked_subsystem("tray_icon").as_deref(), Some("os-tray"));
        assert_eq!(unlinked_subsystem("fetch").as_deref(), Some("os-tray"));
        assert_eq!(unlinked_subsystem("notify"), None);
        warn_if_unlinked("tray_icon");
        warn_if_unlinked("tray_icon");
        assert_eq!(
            WARNED
                .lock()
                .unwrap()
                .as_ref()
                .map(|w| w.contains("tray_icon")),
            Some(true)
        );
        record_unlinked(Vec::new());
        assert_eq!(unlinked_subsystem("tray_icon"), None);
    }
}
