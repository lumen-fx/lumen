//! Design-token / named-color [`Palette`] resource.
//!
//! Mirrors the libadwaita "named colors" table
//! (<https://gnome.pages.gitlab.gnome.org/libadwaita/doc/main/named-colors.html>),
//! exposing each role as an `@named` lookup that CSS / scripts can
//! resolve through [`Palette::lookup`]. The `Palette` itself is a
//! bevy_ecs [`Resource`] so apps can swap roles at runtime
//! (`palette.colors.insert("accent_color".into(), Color::rgb(...))`) and
//! style invalidation observes a single [`Changed<Palette>`] tick.
//!
//! Two constants ship by default: [`Palette::adwaita_light`] and
//! [`Palette::adwaita_dark`]. Values come from
//! `defaults-light.css` / `defaults-dark.css` in the libadwaita sources.
//!
//! See `docs/audits/theming.md` section 4 (Design-token / named-color layer)
//! for the rewrite spec.

use std::collections::HashMap;
use std::sync::Arc;

use bevy_ecs::resource::Resource;

use crate::components::Color;

/// Named-color palette modeled on libadwaita's `@accent_color`,
/// `@window_bg_color`, ... bus. Keys are stable Adwaita role names
/// stored as `Arc<str>` so repeated lookups share one allocation.
///
/// Swap the entire palette by replacing the resource (e.g. on
/// theme flip), or override individual roles with [`Self::with`] /
/// `palette.colors.insert(...)` to let users tweak just `accent_color`
/// without restating the whole table.
#[derive(Resource, Clone, Debug, Default, PartialEq)]
pub struct Palette {
    /// Role-name -> [`Color`] map. Names follow libadwaita's
    /// `@accent_color`, `@window_bg_color`, ... convention (no leading
    /// `@`; the parser strips it before lookup).
    pub colors: HashMap<Arc<str>, Color>,
}

impl Palette {
    /// Empty palette. Useful as a base before [`Self::with`] chains.
    pub fn new() -> Self {
        Self {
            colors: HashMap::new(),
        }
    }

    /// Default light palette from libadwaita `defaults-light.css`.
    ///
    /// Values mirror libadwaita 1.5 defaults; alpha-bearing roles
    /// (`@shade_color`, `@scrollbar_outline_color`, `@borders`) use
    /// the `alpha(...)` channel libadwaita ships, baked at table-init
    /// time. The Adwaita `@card_bg_color` is `alpha(@window_fg, 0.05)`;
    /// here it is pre-resolved against the light `@window_fg_color`.
    pub fn adwaita_light() -> Self {
        let mut p = Self::new();
        // Accent (Adwaita "blue 3" #3584e4).
        p = p
            .with("accent_color", (53, 132, 228))
            .with("accent_bg_color", (53, 132, 228))
            .with("accent_fg_color", "#ffffff")
            // Destructive (Adwaita "red 3" #e01b24).
            .with("destructive_color", (192, 28, 40))
            .with("destructive_bg_color", (224, 27, 36))
            .with("destructive_fg_color", "#ffffff")
            // Success (Adwaita "green 4" #2ec27e -> label color 1d8348).
            .with("success_color", (29, 153, 84))
            .with("success_bg_color", (46, 194, 126))
            .with("success_fg_color", "#ffffff")
            // Warning (Adwaita "yellow 5" #e5a50a -> label 905400).
            .with("warning_color", (144, 84, 0))
            .with("warning_bg_color", (229, 165, 10))
            .with("warning_fg_color", Color::rgba(0.0, 0.0, 0.0, 0.8))
            // Error (Adwaita "red 4" #c01c28).
            .with("error_color", (192, 28, 40))
            .with("error_bg_color", (224, 27, 36))
            .with("error_fg_color", "#ffffff")
            // Window background / foreground.
            .with("window_bg_color", "#fafafb")
            .with("window_fg_color", Color::rgba(0.0, 0.0, 0.0, 0.8))
            // View (text-bearing surfaces - entries, list rows).
            .with("view_bg_color", "#ffffff")
            .with("view_fg_color", Color::rgba(0.0, 0.0, 0.0, 0.8))
            // Header bar.
            .with("headerbar_bg_color", "#ebebed")
            .with("headerbar_fg_color", Color::rgba(0.0, 0.0, 0.0, 0.8))
            // Card (Adwaita: alpha(@window_fg_color, 0.05) on light).
            .with("card_bg_color", Color::rgba(0.0, 0.0, 0.0, 0.05))
            // Sidebar / popover surfaces.
            .with("sidebar_bg_color", "#ebebed")
            .with("popover_bg_color", "#ffffff")
            // Shade overlay (alpha(black, 0.07) on light per Adwaita).
            .with("shade_color", Color::rgba(0.0, 0.0, 0.0, 0.07))
            // Scrollbar outline (alpha(white, 0.5) on light).
            .with("scrollbar_outline_color", Color::rgba(1.0, 1.0, 1.0, 0.5))
            // Border (alpha(@window_fg_color, 0.15) on light).
            .with("borders", Color::rgba(0.0, 0.0, 0.0, 0.15));
        p
    }

    /// Default dark palette from libadwaita `defaults-dark.css`.
    pub fn adwaita_dark() -> Self {
        let mut p = Self::new();
        p = p
            .with("accent_color", (120, 174, 237))
            .with("accent_bg_color", (53, 132, 228))
            .with("accent_fg_color", "#ffffff")
            // Destructive (Adwaita dark label red).
            .with("destructive_color", (255, 122, 128))
            .with("destructive_bg_color", (192, 28, 40))
            .with("destructive_fg_color", "#ffffff")
            // Success (Adwaita dark label green #8ff0a4).
            .with("success_color", (143, 240, 164))
            .with("success_bg_color", (38, 162, 105))
            .with("success_fg_color", "#ffffff")
            // Warning (Adwaita dark label yellow #f8e45c).
            .with("warning_color", (248, 228, 92))
            .with("warning_bg_color", (205, 147, 9))
            .with("warning_fg_color", Color::rgba(0.0, 0.0, 0.0, 0.8))
            // Error (Adwaita dark label red).
            .with("error_color", (255, 122, 128))
            .with("error_bg_color", (192, 28, 40))
            .with("error_fg_color", "#ffffff")
            // Window background / foreground (Adwaita 1.5 dark base).
            .with("window_bg_color", "#222226")
            .with("window_fg_color", "#ffffff")
            // View surfaces.
            .with("view_bg_color", "#1d1d20")
            .with("view_fg_color", "#ffffff")
            // Header bar.
            .with("headerbar_bg_color", "#2e2e32")
            .with("headerbar_fg_color", "#ffffff")
            // Card (Adwaita: alpha(white, 0.08) on dark).
            .with("card_bg_color", Color::rgba(1.0, 1.0, 1.0, 0.08))
            // Sidebar / popover.
            .with("sidebar_bg_color", "#2e2e32")
            .with("popover_bg_color", "#36363a")
            // Shade overlay (alpha(black, 0.36) on dark).
            .with("shade_color", Color::rgba(0.0, 0.0, 0.0, 0.36))
            // Scrollbar outline (alpha(black, 0.5) on dark).
            .with("scrollbar_outline_color", Color::rgba(0.0, 0.0, 0.0, 0.5))
            // Border (alpha(white, 0.15) on dark).
            .with("borders", Color::rgba(1.0, 1.0, 1.0, 0.15));
        p
    }

    /// Register or override a named color and return `self` so calls
    /// can chain. Names should match libadwaita's
    /// `@accent_color` / `@window_bg_color` / ... convention.
    ///
    /// Accepts anything `Into<Color>` - hex literal (`"#rrggbb"`), RGB
    /// tuple `(u8, u8, u8)`, or an explicit [`Color`].
    pub fn with(mut self, name: impl Into<Arc<str>>, color: impl Into<Color>) -> Self {
        self.colors.insert(name.into(), color.into());
        self
    }

    /// Look up a named color. `name` matches with or without the
    /// leading `@` so callers can pass `"accent_color"` (from a parser
    /// that already stripped the sigil) or `"@accent_color"` (raw token)
    /// interchangeably.
    pub fn lookup(&self, name: &str) -> Option<Color> {
        let key = name.strip_prefix('@').unwrap_or(name);
        self.colors.get(key).copied()
    }
}

// -- Color conversions (project memory: `From`/`Into` over `convert_x_to_y`) --

impl From<(u8, u8, u8)> for Color {
    fn from((r, g, b): (u8, u8, u8)) -> Self {
        Self::rgb(r as f32 / 255.0, g as f32 / 255.0, b as f32 / 255.0)
    }
}

impl From<(u8, u8, u8, u8)> for Color {
    fn from((r, g, b, a): (u8, u8, u8, u8)) -> Self {
        Self::rgba(
            r as f32 / 255.0,
            g as f32 / 255.0,
            b as f32 / 255.0,
            a as f32 / 255.0,
        )
    }
}

impl From<&'static str> for Color {
    /// Parses `#rgb`, `#rgba`, `#rrggbb`, or `#rrggbbaa` literals.
    /// Falls back to [`Color::default`] on any parse error - this
    /// impl is intended for the Adwaita defaults table where every
    /// literal is known-good at compile time.
    fn from(hex: &'static str) -> Self {
        parse_hex(hex).unwrap_or_default()
    }
}

fn parse_hex(s: &str) -> Option<Color> {
    let h = s.strip_prefix('#').unwrap_or(s);
    let bytes: Vec<u8> = match h.len() {
        3 => h
            .chars()
            .map(|c| u8::from_str_radix(&format!("{c}{c}"), 16).ok())
            .collect::<Option<Vec<_>>>()?,
        4 => h
            .chars()
            .map(|c| u8::from_str_radix(&format!("{c}{c}"), 16).ok())
            .collect::<Option<Vec<_>>>()?,
        6 | 8 => (0..h.len())
            .step_by(2)
            .map(|i| u8::from_str_radix(&h[i..i + 2], 16).ok())
            .collect::<Option<Vec<_>>>()?,
        _ => return None,
    };
    Some(match *bytes.as_slice() {
        [r, g, b] => (r, g, b).into(),
        [r, g, b, a] => (r, g, b, a).into(),
        _ => return None,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn adwaita_light_carries_core_roles() {
        let p = Palette::adwaita_light();
        // Every named color the rewrite spec lists must be populated.
        for role in [
            "accent_color",
            "accent_bg_color",
            "accent_fg_color",
            "destructive_color",
            "destructive_bg_color",
            "destructive_fg_color",
            "success_color",
            "warning_color",
            "error_color",
            "window_bg_color",
            "window_fg_color",
            "view_bg_color",
            "view_fg_color",
            "headerbar_bg_color",
            "card_bg_color",
            "sidebar_bg_color",
            "popover_bg_color",
            "shade_color",
            "scrollbar_outline_color",
            "borders",
        ] {
            assert!(
                p.lookup(role).is_some(),
                "light palette missing role {role}"
            );
        }
    }

    #[test]
    fn adwaita_dark_carries_core_roles() {
        let p = Palette::adwaita_dark();
        for role in [
            "accent_color",
            "window_bg_color",
            "view_bg_color",
            "headerbar_bg_color",
            "card_bg_color",
            "borders",
        ] {
            assert!(p.lookup(role).is_some(), "dark palette missing role {role}");
        }
    }

    #[test]
    fn lookup_accepts_at_prefix() {
        let p = Palette::adwaita_light();
        assert_eq!(p.lookup("accent_color"), p.lookup("@accent_color"));
    }

    #[test]
    fn with_overrides_existing_role() {
        let p = Palette::adwaita_light().with("accent_color", "#ff00ff");
        assert_eq!(p.lookup("accent_color"), Some(Color::rgb(1.0, 0.0, 1.0)));
    }

    #[test]
    fn light_and_dark_window_bg_differ() {
        let l = Palette::adwaita_light().lookup("window_bg_color").unwrap();
        let d = Palette::adwaita_dark().lookup("window_bg_color").unwrap();
        assert_ne!(l, d, "light and dark window backgrounds collapsed");
    }

    #[test]
    fn from_rgb_tuple_round_trips() {
        let c: Color = (255, 0, 128).into();
        assert!((c.r - 1.0).abs() < 1e-3);
        assert!(c.g.abs() < 1e-3);
        assert!((c.b - 128.0 / 255.0).abs() < 1e-3);
        assert!((c.a - 1.0).abs() < 1e-3);
    }

    #[test]
    fn from_hex_six_digit() {
        let c: Color = "#ff8000".into();
        assert!((c.r - 1.0).abs() < 1e-3);
        assert!((c.g - 128.0 / 255.0).abs() < 1e-3);
        assert!(c.b.abs() < 1e-3);
    }

    #[test]
    fn from_hex_eight_digit_carries_alpha() {
        let c: Color = "#80808080".into();
        assert!((c.a - 128.0 / 255.0).abs() < 1e-3);
    }
}
