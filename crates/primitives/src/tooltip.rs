//! Hover-delay tooltip widget driven by markup `<tooltip text="Save" delay="500">...trigger...</tooltip>`.
//!
//! - The single child is the trigger; the tooltip remains invisible until the trigger has carried [`Hovered`] for `delay` milliseconds (default 500, `--lumen-tooltip-delay` token).
//! - The trigger entity carries [`TooltipSource`]; the first tick of [`Hovered`] records [`HoverStartedAt`].
//! - Once dwell exceeds the configured delay, a [`TooltipPopup`] entity is spawned below-right of the CURSOR (Qt `QToolTip` placement), flipping above/left near viewport edges - see [`cursor_tooltip_origin`].
//! - The popup despawns one tick after [`Hovered`] clears.
//!
//! ## Canonical `#[derive(Widget)]` port
//!
//! Tooltip is the reference port to the new
//! [`lumen_widget_macros::Widget`] derive. `TooltipSource` carries
//! `#[derive(Widget)]` with `tag = "tooltip"`, which emits a
//! [`lumen_widget::Widget`] impl (parser-tag accessor + spawn glue) so
//! a runtime registry can wire `<tooltip text="...">` markup to this
//! component without the previous hand-coded
//! `if tag == "tooltip" { ... }` branch in `lumenc::parser_html`.
//!
//! The four per-tick systems remain hand-written (the `#[widget(prop)]`
//! /`#[widget(state)]` attributes only cover marker-component
//! authoring; system bodies stay author-supplied for v1). The plugin
//! struct is also hand-written (via `#[widget(plugin =
//! "TooltipPlugin")]`) because the four systems need to be installed
//! in a specific tick stage - the canned no-op plugin emitted by the
//! derive can't do that yet.

use bevy_ecs::prelude::*;
use bevy_ecs::query::Added;
use glam::Vec2;
use lumen_core::components::{
    Color, Fill, LumenClasses, Position, Style, TextContent, TextStyle, Transform, Visuals,
};
use lumen_core::input::Hovered;
use lumen_core::prelude::*;
use lumen_core::render_world::Viewport;
use lumen_widget_macros::Widget;
use std::time::Instant;

/// Authored on the trigger via `<tooltip text="..." delay="...">`. The
/// markup parser attaches this component on the SINGLE child the
/// `<tooltip>` wraps - `<tooltip>` itself is a no-op container in the
/// layout tree.
///
/// Carries `#[derive(Widget)]` (see crate docs) so the runtime widget
/// registry can spawn it from a parsed `<tooltip>` tag.
#[derive(Component, Clone, Debug, Widget)]
#[widget(tag = "tooltip", plugin = "TooltipPlugin")]
pub struct TooltipSource {
    /// Tooltip body text. Multi-line strings are honoured (the text
    /// renderer wraps according to the popup's container width).
    #[widget(prop)]
    pub text: String,
    /// Hover dwell before the popup appears. Default 500 ms - the
    /// single Rust-side fallback; the built-in skins route it through
    /// the `--lumen-tooltip-delay` token and the markup `delay=` attr
    /// overrides both (resolution happens in `lumenc`'s cascade).
    #[widget(prop)]
    pub delay_ms: u32,
    /// Gap between the cursor hotspot and the popup's top-left corner,
    /// in logical pixels. Default 12 px (~ Qt's cursor-to-tip offset);
    /// token-reachable via `--lumen-tooltip-offset`, markup `offset=`
    /// wins.
    #[widget(prop)]
    pub offset: f32,
}

impl Default for TooltipSource {
    fn default() -> Self {
        Self {
            text: String::new(),
            delay_ms: 500,
            offset: 12.0,
        }
    }
}

/// Per-entity timestamp of when [`Hovered`] was first inserted.
/// Recorded on `Added<Hovered>` and used to compute dwell time before
/// the tooltip popup is allowed to appear.
#[derive(Component, Clone, Copy, Debug)]
pub struct HoverStartedAt(pub Instant);

/// Marker on the spawned popup entity. Tracks which trigger the
/// popup belongs to so the cleanup pass knows what to despawn when
/// the trigger un-hovers.
#[derive(Component, Clone, Copy, Debug)]
pub struct TooltipPopup {
    /// The trigger entity this popup is paired with.
    pub trigger: Entity,
}

/// Plugin: registers the four tooltip systems and a default `class`
/// rule via the LSP completion tables. No render path of its own -
/// the popup is just an `<overlay>`-shaped entity with a `Visuals`
/// fill + `TextContent`, so all standard styling (CSS `.tooltip`
/// selector, hover-bg, opacity) works.
pub struct TooltipPlugin;

impl Plugin for TooltipPlugin {
    fn build(self, app: &mut App) {
        app.add_systems(
            TickStage::Systems,
            (
                record_hover_started,
                spawn_tooltip_popups,
                apply_tooltip_defaults,
                despawn_tooltip_popups,
            ),
        );
    }
}

/// Stamp `HoverStartedAt` on any entity that just gained `Hovered`.
fn record_hover_started(mut commands: Commands, new_hovers: Query<Entity, Added<Hovered>>) {
    for e in &new_hovers {
        commands.entity(e).insert(HoverStartedAt(Instant::now()));
    }
}

/// Conservative pre-layout estimate of the tooltip body size used by
/// the edge-flip heuristic. Real text shaping happens during the
/// layout pass *after* this spawn, so we approximate from the source
/// string length. Each line uses ~7 px / char average at the default
/// 13 px font and 18 px line height - close enough to choose left-vs-
/// right and top-vs-bottom anchoring; a 1-tick mis-flip on the very
/// first display of a borderline tooltip is acceptable.
fn estimated_tooltip_size(text: &str) -> Vec2 {
    let lines = text.lines().collect::<Vec<_>>();
    let max_chars = lines.iter().map(|l| l.chars().count()).max().unwrap_or(0);
    let row_count = lines.len().max(1) as f32;
    Vec2::new(
        (max_chars as f32 * 7.0 + 16.0).max(48.0),
        row_count * 18.0 + 16.0,
    )
}

/// Cursor-relative tooltip origin (Qt `QToolTip` placement): the popup
/// sits below-right of the cursor hotspot, `offset` px away on each
/// axis. When the preferred spot would overflow the viewport, each axis
/// flips independently to the cursor's other side (above / left),
/// saturating at 0 so the popup never leaves the window.
pub fn cursor_tooltip_origin(cursor: Vec2, popup: Vec2, viewport: Vec2, offset: f32) -> Vec2 {
    let x = if cursor.x + offset + popup.x <= viewport.x {
        cursor.x + offset
    } else {
        (cursor.x - offset - popup.x).max(0.0)
    };
    let y = if cursor.y + offset + popup.y <= viewport.y {
        cursor.y + offset
    } else {
        (cursor.y - offset - popup.y).max(0.0)
    };
    Vec2::new(x, y)
}

/// Spawn a popup for any hovered `<tooltip>` trigger whose dwell
/// exceeds the configured delay and whose popup doesn't already
/// exist.
///
/// Placement is CURSOR-relative (Qt style): below-right of the pointer
/// hotspot by [`TooltipSource::offset`] px, flipping above / left of
/// the cursor near the viewport's bottom / right edges - see
/// [`cursor_tooltip_origin`]. Falls back to the trigger's top-left
/// when the backend reports no pointer position (keyboard-only /
/// synthetic hover).
fn spawn_tooltip_popups(
    mut commands: Commands,
    triggers: Query<(Entity, &TooltipSource, &Transform, &HoverStartedAt), With<Hovered>>,
    existing: Query<&TooltipPopup>,
    viewport: Option<Res<Viewport>>,
    pointer: Option<Res<lumen_core::input::PointerState>>,
) {
    let already: std::collections::HashSet<Entity> = existing.iter().map(|p| p.trigger).collect();
    let now = Instant::now();
    let viewport_size = viewport
        .map(|v| v.size)
        .unwrap_or(Vec2::new(f32::INFINITY, f32::INFINITY));
    let cursor = pointer.as_ref().and_then(|p| p.position);
    for (trigger, src, t, started) in &triggers {
        if already.contains(&trigger) {
            continue;
        }
        if now.duration_since(started.0).as_millis() < u128::from(src.delay_ms) {
            continue;
        }
        let est = estimated_tooltip_size(&src.text);
        let anchor = cursor.unwrap_or(t.absolute);
        let origin = cursor_tooltip_origin(anchor, est, viewport_size, src.offset);
        // Default Visuals / TextStyle are not spawned inline: they
        // would beat any author CSS `.tooltip { background: ... }`
        // rule (inline-attribute styling has the highest cascade
        // priority). `apply_tooltip_defaults` fills them in only when
        // CSS-or-other-systems haven't already attached a Visuals
        // component - preserving the unstyled-tooltip default look
        // while letting `.tooltip` author rules win when present.
        commands.spawn((
            TooltipPopup { trigger },
            // Top-layer paint band: the tooltip stacks with other popups
            // by open order (later-opened on top) instead of the orphan
            // fallback band.
            lumen_core::render_world::OverlayLayer,
            LumenClasses(vec!["tooltip".into()]),
            Style {
                width: lumen_core::components::Length::Auto,
                height: lumen_core::components::Length::Auto,
                position: Position::Absolute,
                inset: lumen_core::components::Edges {
                    left: origin.x,
                    right: f32::INFINITY,
                    top: origin.y,
                    bottom: f32::INFINITY,
                    // W5.5: logical-edge overrides default to None.
                    ..Default::default()
                },
                padding: lumen_core::components::Edges::all(8.0),
                ..Default::default()
            },
            TextContent(src.text.clone()),
        ));
    }
}

/// Fill in default `Visuals` (dark panel + rounded corners) and
/// `TextStyle` (near-white body) on any tooltip popup that doesn't
/// already carry them - covers the no-CSS case while leaving an
/// author-defined `.tooltip { background: ...; color: ...; }` rule
/// authoritative when one applies.
///
/// Runs after `spawn_tooltip_popups`. Idempotent: once a popup has a
/// `Visuals` or `TextStyle` component (whether from this system or
/// elsewhere), this pass leaves it alone.
fn apply_tooltip_defaults(
    mut commands: Commands,
    needs_visuals: Query<Entity, (With<TooltipPopup>, Without<Visuals>)>,
    needs_text_style: Query<Entity, (With<TooltipPopup>, Without<TextStyle>)>,
) {
    for e in &needs_visuals {
        commands.entity(e).insert(Visuals {
            fill: Some(Fill::Solid(Color::rgba(0.10, 0.12, 0.16, 0.95))),
            radius: 6.0,
            ..Default::default()
        });
    }
    for e in &needs_text_style {
        commands.entity(e).insert(TextStyle {
            color: Color::rgb(0.94, 0.95, 0.97),
            size_px: 13.0,
            ..Default::default()
        });
    }
}

/// Drop popups whose trigger is no longer hovered (or no longer
/// exists).
fn despawn_tooltip_popups(
    mut commands: Commands,
    popups: Query<(Entity, &TooltipPopup)>,
    hovered: Query<(), With<Hovered>>,
) {
    for (popup, info) in &popups {
        if hovered.get(info.trigger).is_err() {
            commands.entity(popup).despawn();
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use lumen_widget::{Attributes, Widget as _};

    #[test]
    fn derive_widget_tag_matches_markup() {
        assert_eq!(TooltipSource::parser_tag(), "tooltip");
    }

    #[test]
    fn derive_widget_name_matches_type() {
        assert_eq!(TooltipSource::name(), "TooltipSource");
    }

    #[test]
    fn widget_spawn_inserts_component_with_attrs() {
        let mut app = App::new();
        let parent = app.world.spawn_empty().id();
        let attrs: Attributes = [("text", "Save"), ("delay_ms", "750")].into();
        let e = TooltipSource::spawn(parent, &attrs, &mut app.world);
        let src = app
            .world
            .entity(e)
            .get::<TooltipSource>()
            .expect("TooltipSource present");
        assert_eq!(src.text, "Save");
        assert_eq!(src.delay_ms, 750);
    }

    #[test]
    fn widget_spawn_keeps_defaults_on_missing_attrs() {
        let mut app = App::new();
        let parent = app.world.spawn_empty().id();
        let attrs = Attributes::new();
        let e = TooltipSource::spawn(parent, &attrs, &mut app.world);
        let src = app.world.entity(e).get::<TooltipSource>().unwrap();
        assert_eq!(src.text, "");
        assert_eq!(src.delay_ms, 500);
        assert_eq!(src.offset, 12.0);
    }

    #[test]
    fn cursor_origin_prefers_below_right() {
        let o = cursor_tooltip_origin(
            Vec2::new(100.0, 100.0),
            Vec2::new(80.0, 30.0),
            Vec2::new(800.0, 600.0),
            12.0,
        );
        assert_eq!(o, Vec2::new(112.0, 112.0), "cursor + offset on both axes");
    }

    #[test]
    fn cursor_origin_flips_left_near_right_edge() {
        let o = cursor_tooltip_origin(
            Vec2::new(780.0, 100.0),
            Vec2::new(80.0, 30.0),
            Vec2::new(800.0, 600.0),
            12.0,
        );
        assert_eq!(
            o.x,
            780.0 - 12.0 - 80.0,
            "popup right edge at cursor - offset"
        );
        assert_eq!(o.y, 112.0, "y axis unaffected");
    }

    #[test]
    fn cursor_origin_flips_above_near_bottom_edge() {
        let o = cursor_tooltip_origin(
            Vec2::new(100.0, 590.0),
            Vec2::new(80.0, 30.0),
            Vec2::new(800.0, 600.0),
            12.0,
        );
        assert_eq!(
            o.y,
            590.0 - 12.0 - 30.0,
            "popup bottom edge at cursor - offset"
        );
        assert_eq!(o.x, 112.0);
    }

    #[test]
    fn cursor_origin_saturates_at_zero() {
        // Cursor near the corner with a popup bigger than the room on
        // both flip sides: never negative.
        let o = cursor_tooltip_origin(
            Vec2::new(5.0, 5.0),
            Vec2::new(2000.0, 2000.0),
            Vec2::new(800.0, 600.0),
            12.0,
        );
        assert_eq!(o, Vec2::ZERO);
    }
}
