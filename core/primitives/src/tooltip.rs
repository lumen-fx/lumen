//! Hover-delay tooltip widget driven by markup `<tooltip text="Save" delay="500">...trigger...</tooltip>`.
//!
//! - The single child is the trigger; the tooltip remains invisible until the trigger has been hovered for `delay` milliseconds (default 500, `--lumen-tooltip-delay` token).
//! - A trigger is hovered while it carries [`HoverWithin`]: the pointer is over it or over any of its descendants, so a button's tooltip holds while the pointer crosses onto the button's icon.
//! - The trigger entity carries [`TooltipSource`]; the first tick of [`HoverWithin`] records [`HoverStartedAt`].
//! - Once dwell exceeds the configured delay, a [`TooltipPopup`] entity is spawned below-right of the CURSOR (Qt `QToolTip` placement), flipping above/left near viewport edges - see [`cursor_tooltip_origin`].
//! - With tooltip triggers nested inside each other, only the innermost hovered one shows, as with nested `title` attributes in a browser.
//! - The popup despawns one tick after its trigger stops being hovered, or stops being the innermost hovered trigger.
//!
//! ## Canonical `#[derive(Widget)]` port
//!
//! Tooltip is the reference port to the new
//! [`lumen_widget_macros::Widget`] derive. `TooltipSource` carries
//! `#[derive(Widget)]` with `tag = "tooltip"`, which emits a
//! [`lumen_widget::Widget`] impl (parser-tag accessor + spawn glue) so
//! a runtime registry can wire `<tooltip text="...">` markup to this
//! component without the previous hand-coded
//! `if tag == "tooltip" { ... }` branch in `lumenc::parse::html`.
//!
//! The per-tick systems remain hand-written (the `#[widget(prop)]`
//! /`#[widget(state)]` attributes only cover marker-component
//! authoring; system bodies stay author-supplied for v1). The plugin
//! struct is also hand-written (via `#[widget(plugin =
//! "TooltipPlugin")]`) because the systems need to be installed
//! in specific tick stages - the canned no-op plugin emitted by the
//! derive can't do that yet.

use bevy_ecs::prelude::*;
use bevy_ecs::query::Added;
use glam::Vec2;
use lumen_core::components::{
    DomHidden, Edges, LumenClasses, LumenTag, Position, Style, StyleDirty, TextContent, Transform,
};
use lumen_core::input::HoverWithin;
use lumen_core::prelude::*;
use lumen_core::render_world::Viewport;
use lumen_core::style_overrides::{Live, StyleOverrides, StyleOwner, StyleValues};
use lumen_core::tick::WakeDeadline;
use lumen_core::time::{Duration, Instant};
use lumen_widget_macros::Widget;

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

/// When a tooltip trigger started being hovered. Recorded on
/// `Added<HoverWithin>` and used to compute dwell time before the tooltip
/// popup is allowed to appear.
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

/// The popup holds its placement at the pointer over its styles.
impl StyleOwner for TooltipPopup {
    const RANK: u8 = 20;
}

/// Plugin: registers the tooltip systems. No render path of its own -
/// the popup is a plain text element classed `tooltip`, so all standard
/// styling (CSS `.tooltip` selector, opacity, tokens) works.
pub struct TooltipPlugin;

impl Plugin for TooltipPlugin {
    fn build(self, app: &mut App) {
        // The popup spawns ahead of `Systems`, where the restyle runs, so it
        // is styled in the frame it first paints.
        app.add_systems(TickStage::CommandDrain, spawn_tooltip_popups);
        app.add_systems(
            TickStage::Systems,
            (record_hover_started, despawn_tooltip_popups),
        );
    }
}

/// Stamp `HoverStartedAt` on any tooltip trigger that just started being
/// hovered, and drop it from one that stopped, so a trigger hovered again
/// later waits out its delay afresh instead of showing on a stale stamp.
///
/// The trigger also asks the loop to wake when its delay runs out: with
/// the pointer resting on the trigger nothing else may tick, and the popup
/// only spawns on a tick.
fn record_hover_started(
    mut commands: Commands,
    new_hovers: Query<(Entity, &TooltipSource), Added<HoverWithin>>,
    mut left: RemovedComponents<HoverWithin>,
    stamped: Query<(), (With<HoverStartedAt>, Without<HoverWithin>)>,
    wake: Option<Res<WakeDeadline>>,
) {
    for e in left.read() {
        if stamped.contains(e) {
            commands.entity(e).try_remove::<HoverStartedAt>();
        }
    }
    let now = Instant::now();
    for (e, src) in &new_hovers {
        commands.entity(e).insert(HoverStartedAt(now));
        if let Some(wake) = wake.as_deref() {
            wake.request(now + Duration::from_millis(u64::from(src.delay_ms)));
        }
    }
}

/// The innermost hovered tooltip trigger: of the triggers on the hover
/// chain, the one deepest in the tree. Only it shows a tooltip.
fn innermost_trigger(
    hovered: impl Iterator<Item = Entity>,
    parents: &Query<&ChildOf>,
) -> Option<Entity> {
    hovered.max_by_key(|&e| (parents.iter_ancestors(e).count(), e))
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

/// Spawn a popup for the innermost hovered `<tooltip>` trigger once its
/// dwell exceeds the configured delay and its popup doesn't already
/// exist.
///
/// Placement is CURSOR-relative (Qt style): below-right of the pointer
/// hotspot by [`TooltipSource::offset`] px, flipping above / left of
/// the cursor near the viewport's bottom / right edges - see
/// [`cursor_tooltip_origin`]. Falls back to the trigger's top-left
/// when the backend reports no pointer position (keyboard-only /
/// synthetic hover).
///
/// The popup is a `<tooltip class="tooltip">` element under the root of
/// the trigger's tree, so the stylesheet styles it like any element and
/// its `var()` tokens resolve against the root. It is [`DomHidden`]:
/// scripts and structural selectors never see it. Its `position:
/// absolute` and the inset that puts it at the pointer are held in
/// [`StyleOverrides`], which a restyle keeps; everything else, its look
/// included, is the cascade's.
#[allow(clippy::too_many_arguments)]
fn spawn_tooltip_popups(
    mut commands: Commands,
    triggers: Query<
        (Entity, &TooltipSource, &Transform, Option<&HoverStartedAt>),
        With<HoverWithin>,
    >,
    existing: Query<&TooltipPopup>,
    parents: Query<&ChildOf>,
    transforms: Query<&Transform>,
    viewport: Option<Res<Viewport>>,
    pointer: Option<Res<lumen_core::input::PointerState>>,
    wake: Option<Res<WakeDeadline>>,
) {
    let already: std::collections::HashSet<Entity> = existing.iter().map(|p| p.trigger).collect();
    let now = Instant::now();
    let viewport_size = viewport
        .map(|v| v.size)
        .unwrap_or(Vec2::new(f32::INFINITY, f32::INFINITY));
    let cursor = pointer.as_ref().and_then(|p| p.position);
    let innermost = innermost_trigger(triggers.iter().map(|(e, ..)| e), &parents);
    // The innermost trigger is picked before its dwell stamp lands, so an
    // outer trigger never shows for the tick in between.
    if let Some((trigger, src, t, Some(started))) = innermost.and_then(|e| triggers.get(e).ok()) {
        if already.contains(&trigger) {
            return;
        }
        let due = started.0 + Duration::from_millis(u64::from(src.delay_ms));
        if now < due {
            // Still dwelling: keep the loop's wake-up booked for the moment
            // the delay ends, since the deadline is cleared every tick.
            if let Some(wake) = wake.as_deref() {
                wake.request(due);
            }
            return;
        }
        let est = estimated_tooltip_size(&src.text);
        let anchor = cursor.unwrap_or(t.absolute);
        let origin = cursor_tooltip_origin(anchor, est, viewport_size, src.offset);
        let root = parents.iter_ancestors(trigger).last();
        // The inset resolves against the root, the popup's containing
        // block; a trigger with no tree above it leaves the popup a root of
        // its own, whose inset resolves against the window.
        let local = origin
            - root
                .and_then(|r| transforms.get(r).ok())
                .map_or(Vec2::ZERO, |r| r.absolute);
        let placement = StyleValues {
            position: Some(Position::Absolute),
            // Pinned by its top-left corner; `right` / `bottom` stay `auto`
            // so the popup keeps its content size.
            inset: Some(Edges {
                left: local.x,
                top: local.y,
                ..Edges::auto()
            }),
            ..Default::default()
        };
        let mut style = Style::default();
        let mut overrides = StyleOverrides::default();
        let live = Live {
            style: Some(&style),
            ..Live::default()
        };
        let placed = overrides.hold::<TooltipPopup>(placement, &live);
        placed.write_style(&mut &mut style);
        let mut popup = commands.spawn((
            TooltipPopup { trigger },
            // Top-layer paint band: the tooltip stacks with other popups
            // by open order (later-opened on top) and escapes the clips of
            // the tree it sits in.
            lumen_core::render_world::OverlayLayer,
            LumenTag("tooltip".into()),
            LumenClasses(vec!["tooltip".into()]),
            DomHidden,
            style,
            overrides,
            StyleDirty,
            TextContent(src.text.clone()),
        ));
        if let Some(root) = root {
            popup.insert(ChildOf(root));
        }
    }
}

/// Drop popups whose trigger is no longer hovered (or no longer
/// exists), or is no longer the innermost hovered trigger.
fn despawn_tooltip_popups(
    mut commands: Commands,
    popups: Query<(Entity, &TooltipPopup)>,
    hovered: Query<Entity, (With<TooltipSource>, With<HoverWithin>)>,
    parents: Query<&ChildOf>,
) {
    if popups.is_empty() {
        return;
    }
    let innermost = innermost_trigger(hovered.iter(), &parents);
    for (popup, info) in &popups {
        if innermost != Some(info.trigger) {
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
        let e = app.world.spawn_empty().id();
        let attrs: Attributes = [("text", "Save"), ("delay_ms", "750")].into();
        TooltipSource::spawn(e, &attrs, &mut app.world);
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
        let e = app.world.spawn_empty().id();
        let attrs = Attributes::new();
        TooltipSource::spawn(e, &attrs, &mut app.world);
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

    /// A trigger the pointer leaves loses its dwell stamp, so hovering it
    /// again waits out the delay from the new hover.
    #[test]
    fn leaving_a_trigger_drops_its_dwell_stamp() {
        let mut app = App::new();
        app.add_plugin(TooltipPlugin);
        let e = app
            .world
            .spawn((
                TooltipSource {
                    text: "Save".into(),
                    ..Default::default()
                },
                Transform::default(),
                HoverWithin,
            ))
            .id();
        app.tick();
        assert!(app.world.get::<HoverStartedAt>(e).is_some());
        app.world.entity_mut(e).remove::<HoverWithin>();
        app.tick();
        assert!(app.world.get::<HoverStartedAt>(e).is_none());
    }

    /// A trigger resting under the pointer books a wake-up for when its delay
    /// ends, on the first hovered tick and on every tick until the popup
    /// spawns, so an idle loop still shows the tooltip.
    #[test]
    fn a_dwelling_trigger_wakes_the_loop_when_its_delay_ends() {
        let mut app = App::new();
        app.add_plugin(TooltipPlugin);
        let before = Instant::now();
        app.world.spawn((
            TooltipSource {
                text: "Save".into(),
                delay_ms: 10_000,
                ..Default::default()
            },
            Transform::default(),
            HoverWithin,
        ));
        for _ in 0..2 {
            app.tick();
            let at = lumen_core::tick::wake_deadline(&app.world).expect("a wake-up is booked");
            assert!(at >= before + Duration::from_millis(10_000));
            assert!(at <= Instant::now() + Duration::from_millis(10_000));
        }
    }
}
