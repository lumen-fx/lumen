//! File-based-pages navigation primitive - the ONE surface every embedding
//! reaches through.
//!
//! Navigation in Lumen is not a per-language script builtin: it is a command
//! carried on the shared external-signal bus. A script host (Rhai now, candela
//! later), the Rust SDK, a C-ABI plugin, and the future Python / C# SDKs all
//! reach navigation by writing the reserved [`REQUEST_SIGNAL`] cell through
//! [`request`] (which routes through
//! [`crate::signals::push_external_signal`] -> [`crate::property_store::PropertyStore`]).
//! The runtime's `apply_navigation` system is the single resolver: it reads
//! the request cell, resolves the target path against the registered pages
//! (longest existing-file prefix - the framework never pattern-matches
//! `:id` segments), and writes the reserved [`PATH_SIGNAL`] / [`SEGMENT_SIGNAL`]
//! cells that `<if>` page gates, `bind-*`, and derivations react to.
//!
//! This mirrors real-HTML navigation semantics (an `<a href>` click and a
//! programmatic `history.pushState` both end at one URL that the view reacts
//! to) and Next.js / SvelteKit file-based routing (a page == a file), while
//! staying candela-neutral: nothing here is Rhai-specific.
//!
//! ## Wire format
//!
//! The request cell carries a single opaque string so a repeated identical
//! op (two `back()`s in a row) still edge-triggers: `"<seq>\u{1f}<kind>\u{1f}<arg>"`
//! where `seq` is a process-monotonic nonce, `kind` is one of `{nav, back, forward}`,
//! and `arg` is the target path for `nav` (empty otherwise). Producers build
//! it with [`encode_request`]; the resolver parses it with [`parse_request`].

use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Mutex, OnceLock};

/// Reserved global signal the navigation resolver reads. Producers write it
/// via [`request`]; it is not meant to be bound in markup.
pub const REQUEST_SIGNAL: &str = "route.request";

/// Reserved global signal holding the active page key (the resolved
/// `.lmn` filename stem). `<if eq="settings">` page gates compare against it;
/// `bind-text="route.path"` and derivations may read it.
pub const PATH_SIGNAL: &str = "route.path";

/// Reserved global signal holding the leftover path after the matched page
/// prefix (e.g. navigating `/user/7` when only `user.lmn` exists leaves
/// `/7` here). The framework never parses this into typed params - the
/// page's own code does.
pub const SEGMENT_SIGNAL: &str = "route.segment";

/// A navigation operation. Host-neutral: every surface produces one of these.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum NavOp {
    /// Navigate to a target path (`"settings"`, `"/user/7"`, `"/"`).
    Navigate(String),
    /// Step one entry back in the in-memory history stack.
    Back,
    /// Step one entry forward in the in-memory history stack.
    Forward,
}

/// ASCII unit separator; will not appear in a page path or op token.
const SEP: char = '\u{1f}';

static SEQ: AtomicU64 = AtomicU64::new(1);

fn next_seq() -> u64 {
    SEQ.fetch_add(1, Ordering::Relaxed)
}

/// Encode `op` into the reserved-request wire string with a fresh nonce so an
/// immediately-repeated op still registers as a change.
pub fn encode_request(op: &NavOp) -> String {
    let seq = next_seq();
    match op {
        NavOp::Navigate(path) => format!("{seq}{SEP}nav{SEP}{path}"),
        NavOp::Back => format!("{seq}{SEP}back{SEP}"),
        NavOp::Forward => format!("{seq}{SEP}forward{SEP}"),
    }
}

/// Parse a reserved-request wire string back into `(seq, op)`. Returns `None`
/// for an unrecognised / malformed value.
pub fn parse_request(raw: &str) -> Option<(u64, NavOp)> {
    let mut parts = raw.splitn(3, SEP);
    let seq: u64 = parts.next()?.parse().ok()?;
    let kind = parts.next()?;
    let arg = parts.next().unwrap_or("");
    let op = match kind {
        "nav" => NavOp::Navigate(arg.to_string()),
        "back" => NavOp::Back,
        "forward" => NavOp::Forward,
        _ => return None,
    };
    Some((seq, op))
}

/// Request a navigation from ANY thread / ANY surface. Writes the reserved
/// request cell through the external-signal bus; the runtime's
/// `apply_navigation` system resolves it on the next tick.
///
/// Returns `false` only when the external bus has been torn down.
pub fn request(op: NavOp) -> bool {
    crate::signals::push_external_signal(REQUEST_SIGNAL, encode_request(&op))
}

/// Convenience: navigate to `path` (equivalent to `request(NavOp::Navigate(..))`).
pub fn navigate(path: impl Into<String>) -> bool {
    request(NavOp::Navigate(path.into()))
}

/// Convenience: step back in history.
pub fn back() -> bool {
    request(NavOp::Back)
}

/// Convenience: step forward in history.
pub fn forward() -> bool {
    request(NavOp::Forward)
}

// -- current-page mirror -----------------------------------------------------
//
// The resolver publishes the resolved active page key here so a no-arg
// `page()` read is answerable from any surface (Rhai `page()`, the Rust SDK,
// the C-ABI `lumen_current_page`) without threading the running `App`'s world
// across the boundary. Updated once per resolved navigation; lags the
// PropertyStore cell by at most one tick.

static CURRENT: OnceLock<Mutex<String>> = OnceLock::new();

fn current_cell() -> &'static Mutex<String> {
    CURRENT.get_or_init(|| Mutex::new(String::new()))
}

/// Publish the resolved active page key. Called by the runtime resolver.
pub fn set_current(page: &str) {
    if let Ok(mut g) = current_cell().lock() {
        *g = page.to_string();
    }
}

/// Read the current active page key. Empty before the first page mounts.
pub fn current() -> String {
    current_cell().lock().map(|g| g.clone()).unwrap_or_default()
}

// -- page-path resolution (longest existing-file prefix) ---------------------

/// The page `path` names and the part of the path that page answers for, or
/// `None` when no page answers for it.
///
/// Algorithm - the framework does not pattern-match segments:
/// 1. Strip a leading `/`. An empty path names `entry` (the home page).
/// 2. Try the full path as a page key; if absent, walk up one segment at a
///    time to the longest existing prefix (`/user/7` -> `user` when only
///    `user.lmn` exists).
/// 3. The leftover tail after the matched prefix becomes the `segment`
///    (`/7`), for the page's own code to parse.
///
/// Use this where an address that names nothing has an answer of its own,
/// such as a server sending a 404. Somewhere that has to show a page either
/// way, such as a window following a link, wants [`resolve_path`].
pub fn match_path(path: &str, keys: &[String], entry: &str) -> Option<(String, String)> {
    let norm = normalize_path(path);
    if norm.is_empty() {
        return Some((entry.to_string(), String::new()));
    }
    let segs: Vec<&str> = norm.split('/').filter(|s| !s.is_empty()).collect();
    for i in (1..=segs.len()).rev() {
        let candidate = segs[..i].join("/");
        if keys.iter().any(|k| k == &candidate) {
            let leftover = segs[i..].join("/");
            let segment = if leftover.is_empty() {
                String::new()
            } else {
                format!("/{leftover}")
            };
            return Some((candidate, segment));
        }
    }
    None
}

/// Resolve a requested `path` against the set of known page `keys` (each a
/// `.lmn` filename stem), returning `(page_key, segment)`.
///
/// [`match_path`] does the matching. What this adds is the fallback: a path
/// no page answers for lands on `entry` with the whole requested path as the
/// segment, so an app that wants to say "no such thing" can render that
/// itself.
pub fn resolve_path(path: &str, keys: &[String], entry: &str) -> (String, String) {
    match_path(path, keys, entry)
        .unwrap_or_else(|| (entry.to_string(), format!("/{}", normalize_path(path))))
}

/// A request path with the leading and trailing slashes off, which is the
/// shape a page key is compared against.
fn normalize_path(path: &str) -> &str {
    path.trim_start_matches('/').trim_end_matches('/')
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn round_trip_ops() {
        for op in [
            NavOp::Navigate("settings".into()),
            NavOp::Navigate("/user/7".into()),
            NavOp::Back,
            NavOp::Forward,
        ] {
            let (_, parsed) = parse_request(&encode_request(&op)).unwrap();
            assert_eq!(parsed, op);
        }
    }

    #[test]
    fn nonce_makes_repeats_distinct() {
        let a = encode_request(&NavOp::Back);
        let b = encode_request(&NavOp::Back);
        assert_ne!(a, b, "repeated op must differ so it edge-triggers");
    }

    #[test]
    fn resolves_exact_and_prefix_and_root() {
        let keys = vec![
            "index".to_string(),
            "settings".to_string(),
            "user".to_string(),
        ];
        assert_eq!(
            resolve_path("/", &keys, "index"),
            ("index".into(), "".into())
        );
        assert_eq!(
            resolve_path("settings", &keys, "index"),
            ("settings".into(), "".into())
        );
        assert_eq!(
            resolve_path("/user/7", &keys, "index"),
            ("user".into(), "/7".into())
        );
        assert_eq!(
            resolve_path("/user/7/edit", &keys, "index"),
            ("user".into(), "/7/edit".into())
        );
        // Whole-path miss -> entry + full path as segment.
        assert_eq!(
            resolve_path("/nope", &keys, "index"),
            ("index".into(), "/nope".into())
        );
    }

    #[test]
    fn an_address_no_page_answers_for_matches_nothing() {
        let keys = vec!["index".to_string(), "user".to_string()];
        // The root and a deep path a page does answer for both match, which
        // is what keeps `/user/7` a page rather than a miss.
        assert_eq!(
            match_path("/", &keys, "index"),
            Some(("index".into(), "".into()))
        );
        assert_eq!(
            match_path("/user/7", &keys, "index"),
            Some(("user".into(), "/7".into()))
        );
        assert_eq!(match_path("/nope", &keys, "index"), None);
        assert_eq!(match_path("/nope/deeper", &keys, "index"), None);
    }
}
