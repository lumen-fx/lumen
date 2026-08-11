//! `<progress>` bar behavior: determinate fill sync + indeterminate
//! sweep animation.
//!
//! The markup parser desugars `<progress>` into a track element (tag
//! `progress`, carrying [`ProgressBar`]) with a single `.progress-fill`
//! child (carrying [`ProgressFill`]). Both are real elements, so all
//! styling - track / fill colors, height, radius, the indeterminate
//! sweep period - is CSS-reachable through the skins:
//!
//! ```css
//! progress       { bg: var(--lumen-track); height: 6; radius: 3; }
//! .progress-fill { bg: var(--lumen-accent); radius: 3; }
//! progress       { progress-duration: var(--lumen-progress-period); }
//! ```
//!
//! - Determinate (`value=` / `bind-value=`): the fill's width tracks
//!   `value / max` as a percent of the track.
//! - Indeterminate (no `value`, no `bind-value`): a 30 %-wide chunk
//!   sweeps back and forth (GTK-style bounce; period from
//!   `progress-duration`, token `--lumen-progress-period`). The sweep
//!   keeps the frame loop awake via [`AnimationsActive`] only while an
//!   indeterminate bar is actually visible.
//! - Not focusable, no interaction - `<progress>` never carries
//!   `TabIndex` and none of these systems consume input.

use bevy_ecs::prelude::*;
use lumen_core::components::{BindValue, Length, Style, Transform, Visible};
use lumen_core::prelude::*;
use lumen_core::property_store::{PropertyKey, PropertyStore, PropertyValue};
use lumen_core::render_world::AnimationsActive;
use std::time::Instant;

/// Runtime fallback for the indeterminate sweep period. The single
/// Rust-side value; skins route `progress-duration:
/// var(--lumen-progress-period)` over it and markup `duration=` wins.
pub const PROGRESS_PERIOD_MS: u32 = 1200;

/// Fallback fraction of the track width the indeterminate chunk
/// occupies, used when the track carries no [`ProgressChunk`] (e.g. no
/// CSS `progress-chunk` authored).
const INDETERMINATE_FILL_FRACTION: f32 = 0.3;

/// Width of the indeterminate chunk as a fraction of the track.
#[derive(Component, Clone, Copy, Debug)]
pub struct ProgressChunk(pub f32);

impl Default for ProgressChunk {
    fn default() -> Self {
        Self(INDETERMINATE_FILL_FRACTION)
    }
}

/// State for one `<progress>` element (the track entity).
#[derive(Component, Clone, Debug)]
pub struct ProgressBar {
    /// `Some(v)` = determinate at `v / max`; `None` = indeterminate.
    /// `bind-value` writes flip an indeterminate bar to determinate.
    pub value: Option<f32>,
    /// Upper bound for `value`. Authored `max=`, default 1.0.
    pub max: f32,
    /// Indeterminate sweep period in milliseconds (one full
    /// left-to-right-to-left bounce).
    pub period_ms: u32,
}

impl Default for ProgressBar {
    fn default() -> Self {
        Self {
            value: None,
            max: 1.0,
            period_ms: PROGRESS_PERIOD_MS,
        }
    }
}

/// Marker on the fill child spawned inside every `<progress>`.
#[derive(Component, Clone, Copy, Debug, Default)]
pub struct ProgressFill;

/// Plugin: registers the binding pull + fill sync systems.
pub struct ProgressPlugin;

impl Plugin for ProgressPlugin {
    fn build(self, app: &mut App) {
        // Ordered after the store drain the host registers (no-op edge
        // when absent) so a same-tick script write is observed, and the
        // sync runs after the pull so the fill reflects this tick's
        // value.
        app.add_systems(
            TickStage::Systems,
            apply_progress_bindings.after(lumen_core::property_store::commit_external_properties),
        );
        app.add_systems(
            TickStage::Systems,
            sync_progress_fill.after(apply_progress_bindings),
        );
    }
}

/// Pull `bind-value` signal writes into [`ProgressBar::value`]. Same
/// dirty-gated shape as `lumen_core::signals::apply_value_bindings`
/// (which targets sliders - progress deliberately does not carry a
/// `SliderValue`, or it would inherit wheel / click / drag mutation).
pub fn apply_progress_bindings(
    store: Res<PropertyStore>,
    mut q: Query<(
        &BindValue,
        &mut ProgressBar,
        Option<&mut lumen_core::components::A11yValue>,
    )>,
    new_binds: Query<(), Added<BindValue>>,
) {
    if store.dirty_peek().is_empty() && new_binds.is_empty() {
        return;
    }
    for (bind, mut bar, a11y) in &mut q {
        let key = PropertyKey::Global(std::sync::Arc::<str>::from(bind.0.as_str()));
        let Some(pv) = store.get(&key) else {
            continue;
        };
        let parsed: Option<f32> = match pv {
            PropertyValue::F64(n) => Some(*n as f32),
            PropertyValue::I64(n) => Some(*n as f32),
            PropertyValue::Bool(b) => Some(if *b { 1.0 } else { 0.0 }),
            PropertyValue::Str(s) => s.as_ref().parse::<f32>().ok(),
            _ => None,
        };
        let Some(parsed) = parsed else {
            continue;
        };
        let clamped = parsed.clamp(0.0, bar.max.max(0.0));
        if bar.value != Some(clamped) {
            bar.value = Some(clamped);
        }
        // Mirror into the accessibility value so the published reading
        // follows `bind-value` writes. Guarded so an unchanged value
        // does not trip the a11y change detection.
        if let Some(mut v) = a11y {
            let (now, max) = (f64::from(clamped), f64::from(bar.max));
            if v.now != now || v.max != max {
                v.now = now;
                v.max = max;
            }
        }
    }
}

/// Triangle wave in `[0, 1]` for phase `t` in `[0, 1]`: `0 -> 1` over
/// the first half, `1 -> 0` over the second (GTK-style bounce).
fn bounce(t: f32) -> f32 {
    let t = t.fract();
    if t < 0.5 { t * 2.0 } else { 2.0 - t * 2.0 }
}

/// Keep every `.progress-fill` child in step with its parent
/// [`ProgressBar`].
///
/// - Determinate: `width = value / max` percent, pinned left.
/// - Indeterminate: fixed 30 % width, `inset.left` bouncing across the
///   remaining track width on [`ProgressBar::period_ms`]; requests an
///   [`AnimationsActive`] follow-up tick while any such bar is visible
///   so the sweep animates without input events. Hidden bars (closed
///   tab, hidden dialog) stop requesting ticks - quiescence holds.
#[allow(clippy::type_complexity, clippy::too_many_arguments)]
pub fn sync_progress_fill(
    mut commands: Commands,
    bars: Query<(&ProgressBar, &Transform, Option<&ProgressChunk>)>,
    mut fills: Query<(Entity, &ChildOf, &mut Style), With<ProgressFill>>,
    parents: Query<&ChildOf>,
    visibles: Query<&Visible>,
    // `Without<ProgressFill>` keeps this read disjoint from the `&mut
    // Style` above (B0001); the hidden walk only ever inspects the
    // track and its ancestors, never a fill child.
    styles: Query<&Style, Without<ProgressFill>>,
    anim: Option<Res<AnimationsActive>>,
    mut epoch: Local<Option<Instant>>,
) {
    let epoch = *epoch.get_or_insert_with(Instant::now);
    let now = Instant::now();
    let mut any_indeterminate_visible = false;
    for (fill_e, child_of, mut style) in &mut fills {
        let parent = child_of.parent();
        let Ok((bar, tr, chunk)) = bars.get(parent) else {
            continue;
        };
        match bar.value {
            Some(v) => {
                let denom = bar.max.max(f32::EPSILON);
                let pct = (v / denom).clamp(0.0, 1.0) * 100.0;
                let target = Length::Percent(pct);
                if style.width != target || style.inset.left != 0.0 {
                    style.width = target;
                    style.inset.left = 0.0;
                    commands.entity(fill_e).insert(DirtyLayout);
                }
            }
            None => {
                // Shared section 17.4 walk over a `Without<ProgressFill>`-filtered
                // Style query (inferred `F`) so it coexists with the
                // fill-mutating `styles` view in this same system.
                if hidden_via_ancestors(parent, &parents, &visibles, &styles) {
                    continue;
                }
                any_indeterminate_visible = true;
                if tr.size.x <= 0.0 {
                    continue;
                }
                let period = bar.period_ms.max(1) as f32 / 1000.0;
                let phase = (now - epoch).as_secs_f32() / period;
                let fraction = chunk.copied().unwrap_or_default().0;
                let fill_w = tr.size.x * fraction;
                let left = bounce(phase) * (tr.size.x - fill_w).max(0.0);
                let target_w = Length::Percent(fraction * 100.0);
                if style.width != target_w || (style.inset.left - left).abs() > 0.25 {
                    style.width = target_w;
                    style.inset.left = left;
                    commands.entity(fill_e).insert(DirtyLayout);
                }
            }
        }
    }
    if any_indeterminate_visible && let Some(anim) = anim {
        anim.request();
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use bevy_ecs::system::RunSystemOnce;
    use glam::Vec2;

    fn spawn_bar(world: &mut World, bar: ProgressBar, width: f32) -> (Entity, Entity) {
        let track = world
            .spawn((bar, Transform::new(Vec2::ZERO, Vec2::new(width, 6.0))))
            .id();
        let fill = world
            .spawn((
                ProgressFill,
                Style {
                    position: lumen_core::components::Position::Absolute,
                    ..Default::default()
                },
                ChildOf(track),
            ))
            .id();
        (track, fill)
    }

    #[test]
    fn determinate_fill_width_tracks_value_over_max() {
        let mut world = World::new();
        let (_, fill) = spawn_bar(
            &mut world,
            ProgressBar {
                value: Some(30.0),
                max: 100.0,
                period_ms: PROGRESS_PERIOD_MS,
            },
            200.0,
        );
        world.run_system_once(sync_progress_fill).unwrap();
        let style = world.get::<Style>(fill).unwrap();
        let Length::Percent(p) = style.width else {
            panic!("fill width must be percent, got {:?}", style.width);
        };
        assert!((p - 30.0).abs() < 0.01, "expected ~30%, got {p}");
        assert_eq!(style.inset.left, 0.0);
    }

    #[test]
    fn determinate_value_clamps_to_max() {
        let mut world = World::new();
        let (track, fill) = spawn_bar(&mut world, ProgressBar::default(), 200.0);
        world.get_mut::<ProgressBar>(track).unwrap().value = Some(250.0);
        world.get_mut::<ProgressBar>(track).unwrap().max = 100.0;
        world.run_system_once(sync_progress_fill).unwrap();
        let style = world.get::<Style>(fill).unwrap();
        assert_eq!(
            style.width,
            Length::Percent(100.0),
            "value > max pins at 100%"
        );
    }

    #[test]
    fn indeterminate_bar_animates_and_requests_ticks() {
        let mut world = World::new();
        world.insert_resource(AnimationsActive::default());
        // No `ProgressChunk` on the track - the no-CSS-authored path must
        // reproduce today's hardcoded 30% chunk exactly.
        let (_, fill) = spawn_bar(&mut world, ProgressBar::default(), 200.0);
        world.run_system_once(sync_progress_fill).unwrap();
        let style = world.get::<Style>(fill).unwrap();
        let Length::Percent(p) = style.width else {
            panic!("fill width must be percent, got {:?}", style.width);
        };
        assert!(
            (p - 30.0).abs() < 0.01,
            "indeterminate chunk is ~30%, got {p}"
        );
        assert!(
            world.resource::<AnimationsActive>().get(),
            "visible indeterminate bar keeps the frame loop awake"
        );
    }

    /// A CSS-supplied [`ProgressChunk`] on the track changes the
    /// indeterminate sweep's chunk width.
    #[test]
    fn custom_progress_chunk_changes_the_fill_width() {
        let mut world = World::new();
        world.insert_resource(AnimationsActive::default());
        let (track, fill) = spawn_bar(&mut world, ProgressBar::default(), 200.0);
        world.entity_mut(track).insert(ProgressChunk(0.6));
        world.run_system_once(sync_progress_fill).unwrap();
        let style = world.get::<Style>(fill).unwrap();
        let Length::Percent(p) = style.width else {
            panic!("fill width must be percent, got {:?}", style.width);
        };
        assert!(
            (p - 60.0).abs() < 0.01,
            "a CSS-supplied progress-chunk of 0.6 renders as ~60%, got {p}"
        );
    }

    #[test]
    fn hidden_indeterminate_bar_stays_quiescent() {
        let mut world = World::new();
        world.insert_resource(AnimationsActive::default());
        let (track, _) = spawn_bar(&mut world, ProgressBar::default(), 200.0);
        world.entity_mut(track).insert(Visible(false));
        world.run_system_once(sync_progress_fill).unwrap();
        assert!(
            !world.resource::<AnimationsActive>().get(),
            "hidden bar must not request ticks"
        );
    }

    #[test]
    fn binding_write_flips_indeterminate_to_determinate() {
        let mut world = World::new();
        let mut store = PropertyStore::default();
        store.set_global_str("pct", "0.4");
        world.insert_resource(store);
        let (track, _) = spawn_bar(&mut world, ProgressBar::default(), 200.0);
        world.entity_mut(track).insert(BindValue("pct".into()));
        world.run_system_once(apply_progress_bindings).unwrap();
        let bar = world.get::<ProgressBar>(track).unwrap();
        assert_eq!(bar.value, Some(0.4));
    }

    #[test]
    fn bounce_sweeps_out_and_back() {
        assert_eq!(bounce(0.0), 0.0);
        assert_eq!(bounce(0.25), 0.5);
        assert_eq!(bounce(0.5), 1.0);
        assert_eq!(bounce(0.75), 0.5);
        assert!(bounce(0.999) < 0.01);
    }
}
