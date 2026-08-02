//! Embedded user-agent stylesheets.
//!
//! [`DEFAULT`] / [`MACOS`] / [`WINDOWS`] / [`LINUX`] are opt-in, activated
//! via `<root skin="<name>">` or `lumen.toml` `[skin] name = "..."`; when
//! neither opts in, none of them is applied.
//!
//! [`UA`] is different: it is not selected by name and applies to every
//! app unconditionally, beneath whichever named skin (if any) and
//! beneath the app's own CSS - see its own doc comment.

/// The always-on user-agent baseline: per-tag sizing floors (button /
/// input / toggle / switch / slider / progress minimums, root / title-bar
/// fill) applied beneath any opt-in skin and beneath app CSS. Folded into
/// the combined cascade by `run::loading::load_ir` for every app, skinned
/// or not - the framework's equivalent of a browser's built-in
/// stylesheet.
pub const UA: &str = include_str!("skins/ua.css");

/// Default (neutral, dark-first) skin CSS for buttons, inputs,
/// toggles, sliders, and tiles.
pub const DEFAULT: &str = include_str!("skins/default.css");
/// macOS-flavoured skin (macOS 14/15-era Aqua): 20px buttons, no
/// hover feedback, pill switch, soft accent focus halo.
pub const MACOS: &str = include_str!("skins/macos.css");
/// Windows 11 / WinUI 3 (Fluent 2) skin: 4px radii, accent primary
/// buttons, elevation bottom edge, keyboard-only focus ring.
pub const WINDOWS: &str = include_str!("skins/windows.css");
/// Linux (libadwaita-leaning neutral) skin: flat fg-alpha fills,
/// bold accent, 12px popovers, 46x26 pill switches.
pub const LINUX: &str = include_str!("skins/linux.css");

/// The list of recognised skin names (excluding `auto`).
pub const NAMES: [&str; 4] = ["default", "macos", "windows", "linux"];

/// Returns the embedded CSS source for `name`, or `None` for an
/// unknown skin. `"auto"` resolves to the current OS per
/// [`resolve_auto`], so forcing any concrete name on any OS works
/// (cross-platform preview).
pub fn lookup(name: &str) -> Option<&'static str> {
    match name {
        "default" => Some(DEFAULT),
        "macos" => Some(MACOS),
        "windows" => Some(WINDOWS),
        "linux" => Some(LINUX),
        "auto" => lookup(resolve_auto()),
        _ => None,
    }
}

/// Maps `std::env::consts::OS` onto a concrete skin name:
/// `"macos"` / `"windows"` / everything else -> `"linux"` (the
/// adwaita-leaning neutral degrades gracefully across desktops).
pub fn resolve_auto() -> &'static str {
    match std::env::consts::OS {
        "macos" => "macos",
        "windows" => "windows",
        _ => "linux",
    }
}
