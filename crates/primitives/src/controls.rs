//! Stateful control behaviors for the `<toggle>` and `<slider>` markup tags.
//!
//! Each control owns a single component holding its current value
//! (`Toggleable.checked`, `SliderValue.value`). These systems map raw
//! [`ClickEvent`] / [`DragStartEvent`] / [`DragMoveEvent`] messages into
//! state mutations on the matching entity. Script-facing notifications
//! ([`ToggleChanged`] / [`SliderChanged`]) fire on every state change so the
//! Rhai host can forward them as `on_toggle(id, checked)` /
//! `on_slider(id, value)`.

use bevy_ecs::message::{Message, MessageReader, MessageWriter, Messages};
use bevy_ecs::prelude::*;
use lumen_core::components::{Fill, Visuals};
use lumen_core::prelude::*;

/// Track fill for an unchecked `<toggle>` when neither markup nor CSS
/// supplied one. Matches the default skin's `#555555`.
pub const TOGGLE_UNCHECKED_BG: Color = Color::rgb(0.33, 0.33, 0.33);

/// Track fill for a checked `<toggle>` when no `:checked { bg: ... }`
/// rule supplied one. Accent teal so checked vs unchecked is visible
/// with zero author CSS.
pub const TOGGLE_CHECKED_BG: Color = Color::rgb(0.20, 0.66, 0.70);

/// Fill for the toggle knob / slider thumb child tiles.
pub const KNOB_FILL: Color = Color::rgb(0.92, 0.92, 0.94);

/// Fallback gap between the toggle knob and the track edge, in logical
/// pixels, used when the entity carries no [`KnobGeometry`] (e.g. no CSS
/// `knob-inset` authored).
pub const KNOB_INSET: f32 = 4.0;

/// Fallback slider thumb diameter in logical pixels, used when the
/// entity carries no [`KnobGeometry`] (e.g. no CSS `thumb-size`
/// authored).
pub const THUMB_SIZE: f32 = 16.0;

/// Knob and thumb geometry, resolved from CSS at spawn. Sits on the
/// `<toggle>` / `<switch>` track entity (consulted via [`KnobGeometry::inset`])
/// and on the `<slider>` track entity (consulted via
/// [`KnobGeometry::thumb_size`]). An entity without this component uses
/// [`KnobGeometry::default`], which reproduces today's hardcoded geometry
/// exactly.
#[derive(Component, Clone, Copy, Debug)]
pub struct KnobGeometry {
    /// Gap between the knob and the track edge, in logical pixels.
    pub inset: f32,
    /// Slider thumb diameter, in logical pixels.
    pub thumb_size: f32,
}

impl Default for KnobGeometry {
    fn default() -> Self {
        Self {
            inset: KNOB_INSET,
            thumb_size: THUMB_SIZE,
        }
    }
}

/// The full track palette of a `<toggle>` or `<switch>`, resolved at
/// spawn from markup / CSS and refreshed by the restyle pass.
/// [`sync_track_fill`] is the only thing that reads it, and
/// [`TrackStyle::fill_for`] is the only place a state maps to a color.
///
/// Both controls share one component because they share one skin family:
/// they differ in shape and in how the knob moves, not in what the track
/// is painted with.
#[derive(Component, Clone, Copy, Debug)]
pub struct TrackStyle {
    /// Track fill while checked (`:checked { bg: ... }` or the default
    /// accent).
    pub checked_bg: Color,
    /// Track fill while unchecked (author `bg` or the default gray).
    pub unchecked_bg: Color,
    /// Track fill while disabled (`:disabled { bg: ... }`), when a rule
    /// supplied one. `None` leaves a disabled track on its
    /// checked/unchecked fill, which the generic disabled-opacity dim
    /// then covers.
    pub disabled_bg: Option<Color>,
}

impl Default for TrackStyle {
    fn default() -> Self {
        Self {
            checked_bg: TOGGLE_CHECKED_BG,
            unchecked_bg: TOGGLE_UNCHECKED_BG,
            disabled_bg: None,
        }
    }
}

impl TrackStyle {
    /// The fill this track paints in the given state.
    ///
    /// Disabled outranks checked: the skins order `:disabled` after
    /// `:checked`, so in a browser the later rule would win, and a
    /// control nobody can operate should not advertise its state as
    /// live.
    pub fn fill_for(&self, checked: bool, disabled: bool) -> Color {
        if disabled && let Some(c) = self.disabled_bg {
            return c;
        }
        if checked {
            self.checked_bg
        } else {
            self.unchecked_bg
        }
    }
}

/// Marker for the knob child entity spawned inside every `<toggle>`.
/// Absolute-positioned; [`sync_toggle_visuals`] slides it left / right
/// by the parent's checked state.
#[derive(Component, Clone, Copy, Debug, Default)]
pub struct ToggleKnob;

/// Marker for the thumb child entity spawned inside every `<slider>`.
/// Absolute-positioned; [`sync_slider_thumb`] places it along the
/// track at `(value - min) / (max - min)`.
#[derive(Component, Clone, Copy, Debug, Default)]
pub struct SliderThumb;

/// Emitted after a [`Toggleable`] flips its `checked` state in response to
/// user input. Carries the entity and the new value.
#[derive(Message, Clone, Copy, Debug)]
pub struct ToggleChanged {
    /// Affected entity.
    pub entity: Entity,
    /// New checked state.
    pub checked: bool,
}

/// Emitted after a [`SliderValue`] is updated by user drag / click. Carries
/// the entity and the new clamped value.
#[derive(Message, Clone, Copy, Debug)]
pub struct SliderChanged {
    /// Affected entity.
    pub entity: Entity,
    /// New value, clamped to `[min, max]`.
    pub value: f32,
}

/// Plugin: registers the toggle + slider behavior systems and their
/// outbound message types.
pub struct ControlsPlugin;

impl Plugin for ControlsPlugin {
    fn build(self, app: &mut App) {
        app.world.init_resource::<Messages<ToggleChanged>>();
        app.world.init_resource::<Messages<SliderChanged>>();
        app.add_systems(TickStage::Systems, update_message_buffers);
        // The user-input mutators are ordered before lumen-core's
        // signal push-back systems (`push_toggle_to_signal` /
        // `push_slider_to_signal`, registered by the host when signal
        // bindings are in play). Without the edge the push system can
        // run earlier in the same tick, miss this tick's mutation, and
        // - because the app only ticks on wakes - the bound signal
        // (and every label bound to it) lags one whole user
        // interaction behind. Ordering against a system the host never
        // registers is a no-op, so headless/no-binding apps are
        // unaffected.
        // The click / drag consumers are additionally ordered after
        // their producers (`lumen_input::dispatch_clicks`,
        // `crate::drag::update_drag_on_move`) so a click or drag-move
        // mutates the control on the SAME tick it happened rather than
        // one tick late off the double-buffered bus. Same no-op rule as
        // above when the host never registers the producer.
        app.add_systems(
            TickStage::Systems,
            flip_toggle_on_click
                .after(lumen_input::dispatch_clicks)
                .before(lumen_core::signals::push_toggle_to_signal),
        );
        app.add_systems(
            TickStage::Systems,
            set_slider_on_click
                .after(lumen_input::dispatch_clicks)
                .before(lumen_core::signals::push_slider_to_signal),
        );
        app.add_systems(
            TickStage::Systems,
            set_slider_on_drag
                .after(crate::drag::update_drag_on_move)
                .before(lumen_core::signals::push_slider_to_signal),
        );
        app.add_systems(
            TickStage::Systems,
            move_slider_on_keys.before(lumen_core::signals::push_slider_to_signal),
        );
        app.add_systems(
            TickStage::Systems,
            adjust_slider_on_wheel.before(lumen_core::signals::push_slider_to_signal),
        );
        // Escape-cancels-drag: the pre-drag value snapshot must be taken
        // before this tick's DragMoveEvents mutate the value, and the
        // cancel must run after them so an Escape and a move landing on
        // the same tick resolve to the restored value.
        app.add_systems(
            TickStage::Systems,
            snapshot_slider_drag_origin
                .after(crate::drag::update_drag_on_move)
                .before(set_slider_on_drag),
        );
        // `.before(release_drag_on_unpress)`: the Wave-3 generic Escape
        // press-cancel (`lumen_input::cancel_press_on_escape`, Input
        // stage) strips `Pressed` on the same keystroke, which would let
        // `release_drag_on_unpress` tear the `DragState` down as a
        // *normal* release - committing the dragged value - before this
        // cancel could observe the drag and restore the snapshot. The
        // explicit edge guarantees the cancel wins the race.
        app.add_systems(
            TickStage::Systems,
            cancel_slider_drag_on_escape
                .after(set_slider_on_drag)
                .before(crate::drag::release_drag_on_unpress)
                .before(lumen_core::signals::push_slider_to_signal),
        );
        app.add_systems(TickStage::Systems, clear_slider_drag_origin_on_end);
        app.add_systems(
            TickStage::Systems,
            sync_toggle_visuals.after(flip_toggle_on_click),
        );
        // Track fill for both `<toggle>` and `<switch>` - registered here
        // rather than beside each widget's knob sync because it is driven
        // by the shared `Toggleable` path this plugin owns.
        app.add_systems(
            TickStage::Systems,
            sync_track_fill.after(flip_toggle_on_click),
        );
        app.add_systems(
            TickStage::Systems,
            sync_slider_thumb.after(set_slider_on_drag),
        );
        // `<switch>` shares this control's `Toggleable` click / keyboard /
        // binding path; its visual sync + thumb-slide animation register
        // here (rather than as a boot-time plugin) so every host that
        // installs `ControlsPlugin` gets the switch for free.
        crate::switch::register_switch_systems(app);
        // Reactive wake (see `crate::wake`): a tick that ends with
        // undrained PropertyStore writes must schedule a follow-up tick
        // so if/for reconcilers and binding pulls observe the write
        // within one frame - measured on widget-garden, the `<dialog>`
        // otherwise waited ~550 ms (open) / ~4 s (close) for the next
        // incidental input event. Ordered before the core clear system
        // that empties the queue at end of tick.
        app.add_systems(
            TickStage::A11ySync,
            crate::wake::request_tick_on_property_writes
                .before(lumen_core::property_store::clear_property_store_dirty),
        );
    }
}

fn update_message_buffers(
    mut toggles: ResMut<Messages<ToggleChanged>>,
    mut sliders: ResMut<Messages<SliderChanged>>,
) {
    toggles.update();
    sliders.update();
}

/// Resolve an input target to the control entity that owns it: `start`
/// itself when `contains` matches, else the nearest matching ancestor.
///
/// The `<toggle>` knob / `<slider>` thumb children carry [`Visuals`],
/// so deepest-child-wins hit-testing (`lumen_input::hit_test`) routes
/// press / drag / click events at the *child* entity - the same
/// ancestor walk `lumen_primitives::popup::press_hits_popup` uses lets
/// the click/drag mutators act on the parent control while the children
/// stay hittable for hover / cursor purposes. Without this, grabbing
/// the thumb (or clicking the knob) was a permanent dead zone.
///
/// `pub(crate)`: the tab / dropdown / menu click dispatchers
/// (`crate::tabs`) and the radio dispatcher (`crate::radio`) share the
/// same hit-shadowing fix - any dispatcher matching only the exact
/// clicked entity silently dead-zones once the control grows a
/// hit-testable child (text, knob, dot...).
pub(crate) fn resolve_control(
    start: Entity,
    parents: &Query<&ChildOf>,
    contains: impl Fn(Entity) -> bool,
) -> Option<Entity> {
    let mut cur = Some(start);
    while let Some(e) = cur {
        if contains(e) {
            return Some(e);
        }
        cur = parents.get(e).ok().map(|c| c.parent());
    }
    None
}

/// Flip [`Toggleable::checked`] every time the entity - or a child of
/// it, such as the spawned [`ToggleKnob`] - receives a [`ClickEvent`].
/// Fires [`ToggleChanged`] so the script host can echo it to
/// `on_toggle(id, checked)`.
pub fn flip_toggle_on_click(
    mut clicks: MessageReader<ClickEvent>,
    mut q: Query<&mut Toggleable>,
    parents: Query<&ChildOf>,
    mut out: MessageWriter<ToggleChanged>,
) {
    for click in clicks.read() {
        let Some(target) = resolve_control(click.entity, &parents, |e| q.contains(e)) else {
            continue;
        };
        if let Ok(mut t) = q.get_mut(target) {
            t.checked = !t.checked;
            out.write(ToggleChanged {
                entity: target,
                checked: t.checked,
            });
        }
    }
}

/// Clamp `v` into the range described by `lo`/`hi` without panicking on
/// an inverted (`lo > hi`, e.g. a descending `<slider min="100"
/// max="0">`) or NaN range. `f32::clamp` asserts `lo <= hi` and would
/// crash the whole app; this mirrors the guard
/// `popup_nav::scroll_row_into_view` uses. NaN bounds fall through to the
/// `min`/`max` path, which ignores NaN rather than propagating it.
fn clamp_range(v: f32, lo: f32, hi: f32) -> f32 {
    if lo <= hi {
        v.clamp(lo, hi)
    } else {
        v.min(lo).max(hi)
    }
}

/// Map a pointer x (window space) to a `[0, 1]` slider fraction using the
/// SAME reduced range the thumb is drawn over. [`sync_slider_thumb`]
/// places the thumb's left edge across `size.x - thumb_size`, so the
/// thumb centre travels `[thumb_size/2, size.x - thumb_size/2]`. Mapping
/// the pointer through the full track width instead makes a press on the
/// thumb jump the value by up to `thumb_size/2` worth of range (the
/// "volume slider jumps when you grab it" bug); the two only agreed at
/// `frac = 0.5`. Subtracting the half-thumb and dividing by the reduced
/// range keeps a stationary grab value-neutral. `thumb_size` is the
/// entity's resolved [`KnobGeometry::thumb_size`] (or [`THUMB_SIZE`] when
/// the entity carries no component), so this stays in lockstep with
/// wherever [`sync_slider_thumb`] actually draws the thumb.
fn slider_frac(pointer_x: f32, absolute_x: f32, size_x: f32, thumb_size: f32) -> f32 {
    let track = size_x - thumb_size;
    if track <= 0.0 {
        return 0.0;
    }
    (((pointer_x - absolute_x) - thumb_size / 2.0) / track).clamp(0.0, 1.0)
}

/// Click-anywhere-on-track: set [`SliderValue`] based on where in the
/// track's content rect the pointer landed. Clicks landing on the
/// [`SliderThumb`] child resolve to the parent slider (see
/// [`resolve_control`]). Coarse positioning; the drag handler refines
/// from there.
pub fn set_slider_on_click(
    mut clicks: MessageReader<ClickEvent>,
    mut q: Query<(&mut SliderValue, &Transform, Option<&KnobGeometry>)>,
    parents: Query<&ChildOf>,
    mut out: MessageWriter<SliderChanged>,
) {
    for click in clicks.read() {
        let Some(target) = resolve_control(click.entity, &parents, |e| q.contains(e)) else {
            continue;
        };
        if let Ok((mut s, t, geo)) = q.get_mut(target) {
            let thumb_size = geo.copied().unwrap_or_default().thumb_size;
            let frac = slider_frac(click.position.x, t.absolute.x, t.size.x, thumb_size);
            let new = s.min + frac * (s.max - s.min);
            if new != s.value {
                s.value = new;
                out.write(SliderChanged {
                    entity: target,
                    value: new,
                });
            }
        }
    }
}

/// Keyboard control for a focused `<slider>`. Reads the raw
/// [`KeyPressed`] bus directly rather than `FocusedKey` - the same
/// pattern `lumen_primitives::tabs::dispatch_tab_keys` uses for its
/// roving-tabindex nav - since a slider isn't a `TextInput` and doesn't
/// need the extra `dispatch_focused_keys` routing hop.
///
/// - `ArrowLeft` / `ArrowDown`: decrement by one step.
/// - `ArrowRight` / `ArrowUp`: increment by one step.
/// - `PageDown` / `PageUp` (forwarded by the winit backend as
///   `Key::Character("PageDown"/"PageUp")` - there's no dedicated
///   `NamedKey` variant for them yet): move by ten steps.
/// - `Home` / `End`: jump to `min` / `max`.
///
/// Step comes from [`SliderValue::step_size`]: the authored `step`
/// attribute, or `(max - min) / 100` - matching the
/// `<input type=range>` browser default of 100 discrete positions.
pub fn move_slider_on_keys(
    mut keys: MessageReader<KeyPressed>,
    tracker: Res<FocusTracker>,
    mut sliders: Query<&mut SliderValue>,
    mut out: MessageWriter<SliderChanged>,
) {
    let Some(entity) = tracker.0 else {
        keys.read().for_each(drop);
        return;
    };
    let Ok(mut slider) = sliders.get_mut(entity) else {
        keys.read().for_each(drop);
        return;
    };
    let step = slider.step_size();
    for ev in keys.read() {
        let new = match &ev.key {
            Key::Named(NamedKey::ArrowLeft) | Key::Named(NamedKey::ArrowDown) => {
                Some(slider.value - step)
            }
            Key::Named(NamedKey::ArrowRight) | Key::Named(NamedKey::ArrowUp) => {
                Some(slider.value + step)
            }
            Key::Character(s) if s.as_str() == "PageDown" => Some(slider.value - step * 10.0),
            Key::Character(s) if s.as_str() == "PageUp" => Some(slider.value + step * 10.0),
            Key::Named(NamedKey::Home) => Some(slider.min),
            Key::Named(NamedKey::End) => Some(slider.max),
            _ => None,
        };
        let Some(new) = new else { continue };
        let clamped = clamp_range(new, slider.min, slider.max);
        if clamped != slider.value {
            slider.value = clamped;
            out.write(SliderChanged {
                entity,
                value: clamped,
            });
        }
    }
}

/// While the user drags a slider, continuously update its value based on
/// the cursor x within the track. Reuses [`DragMoveEvent`], so the slider
/// inherits all of lumen-input's per-OS drag-threshold behavior for free.
/// Drags that started on the [`SliderThumb`] child (the common grab)
/// resolve to the parent slider (see [`resolve_control`]).
pub fn set_slider_on_drag(
    mut drags: MessageReader<DragMoveEvent>,
    mut q: Query<(&mut SliderValue, &Transform, Option<&KnobGeometry>)>,
    parents: Query<&ChildOf>,
    mut out: MessageWriter<SliderChanged>,
) {
    for d in drags.read() {
        let Some(target) = resolve_control(d.entity, &parents, |e| q.contains(e)) else {
            continue;
        };
        if let Ok((mut s, t, geo)) = q.get_mut(target) {
            let thumb_size = geo.copied().unwrap_or_default().thumb_size;
            let frac = slider_frac(d.position.x, t.absolute.x, t.size.x, thumb_size);
            let new = s.min + frac * (s.max - s.min);
            if new != s.value {
                s.value = new;
                out.write(SliderChanged {
                    entity: target,
                    value: new,
                });
            }
        }
    }
}

/// One wheel line-notch in logical pixels. Must match the window
/// backend's `LineDelta` normalization (`LINE_PX` in
/// `lumen-window-winit`), so one physical wheel detent maps to exactly
/// one slider step; pixel-precise trackpad deltas scale proportionally.
pub const WHEEL_NOTCH_PX: f32 = 32.0;

/// Wheel over a hovered `<slider>` (or its thumb child) nudges the value
/// by [`SliderValue::step_size`] per notch - wheel-up increases, matching
/// Qt's `QSlider::wheelEvent`. The wheel is *consumed* by the slider:
/// `lumen_primitives::scroll::accumulate_wheel` stands down whenever a
/// slider sits on the hovered entity's ancestor chain, so adjusting a
/// slider never also scrolls an ancestor scroll container.
pub fn adjust_slider_on_wheel(
    mut wheels: MessageReader<MouseWheel>,
    hovered: Query<Entity, With<Hovered>>,
    parents: Query<&ChildOf>,
    mut sliders: Query<&mut SliderValue>,
    mut out: MessageWriter<SliderChanged>,
) {
    let mut total = 0.0_f32;
    for ev in wheels.read() {
        total += ev.delta.y;
    }
    if total == 0.0 {
        return;
    }
    let Some(target) = hovered
        .iter()
        .next()
        .and_then(|h| resolve_control(h, &parents, |e| sliders.contains(e)))
    else {
        return;
    };
    let Ok(mut s) = sliders.get_mut(target) else {
        return;
    };
    let new = clamp_range(
        s.value + (total / WHEEL_NOTCH_PX) * s.step_size(),
        s.min,
        s.max,
    );
    if new != s.value {
        s.value = new;
        out.write(SliderChanged {
            entity: target,
            value: new,
        });
    }
}

/// Pre-drag value snapshot for the Escape-cancel path. Inserted on the
/// *slider* entity at [`DragStartEvent`] (whether the grab landed on the
/// slider or its thumb child), consumed by
/// [`cancel_slider_drag_on_escape`], and cleared on normal
/// [`DragEndEvent`] release.
#[derive(Component, Clone, Copy, Debug)]
pub struct SliderDragOrigin(pub f32);

/// On [`DragStartEvent`] targeting a slider (or its thumb child), stash
/// the current value so Escape can restore it. Ordered before
/// [`set_slider_on_drag`] so the snapshot always precedes this tick's
/// moves.
pub fn snapshot_slider_drag_origin(
    mut commands: Commands,
    mut starts: MessageReader<DragStartEvent>,
    sliders: Query<&SliderValue>,
    parents: Query<&ChildOf>,
) {
    for ev in starts.read() {
        let Some(target) = resolve_control(ev.entity, &parents, |e| sliders.contains(e)) else {
            continue;
        };
        if let Ok(s) = sliders.get(target) {
            commands.entity(target).insert(SliderDragOrigin(s.value));
        }
    }
}

/// A drag that ends normally (pointer release) commits the dragged
/// value: drop the snapshot so a later Escape doesn't roll back a
/// finished interaction.
pub fn clear_slider_drag_origin_on_end(
    mut commands: Commands,
    mut ends: MessageReader<DragEndEvent>,
    sliders: Query<(), With<SliderDragOrigin>>,
    parents: Query<&ChildOf>,
) {
    for ev in ends.read() {
        if let Some(target) = resolve_control(ev.entity, &parents, |e| sliders.contains(e)) {
            commands.entity(target).remove::<SliderDragOrigin>();
        }
    }
}

/// Escape while a slider drag is in flight cancels it (Qt drag-cancel
/// contract): the value snaps back to its [`SliderDragOrigin`] and the
/// drag machinery is torn down - [`crate::drag::DragState`] *and*
/// `Pressed` are removed, so the drag doesn't resume while the button
/// stays held and the eventual pointer release emits no [`ClickEvent`]
/// (no release commit). Fires [`SliderChanged`] with the restored value
/// so bound signals / scripts roll back too.
pub fn cancel_slider_drag_on_escape(
    mut commands: Commands,
    mut keys: MessageReader<KeyPressed>,
    holders: Query<Entity, With<crate::drag::DragState>>,
    mut sliders: Query<&mut SliderValue>,
    origins: Query<&SliderDragOrigin>,
    parents: Query<&ChildOf>,
    mut out: MessageWriter<SliderChanged>,
) {
    let escape = keys
        .read()
        .any(|k| matches!(k.key, Key::Named(NamedKey::Escape)));
    if !escape {
        return;
    }
    for holder in &holders {
        let Some(target) = resolve_control(holder, &parents, |e| origins.contains(e)) else {
            continue;
        };
        let Ok(origin) = origins.get(target) else {
            continue;
        };
        if let Ok(mut s) = sliders.get_mut(target)
            && s.value != origin.0
        {
            s.value = origin.0;
            out.write(SliderChanged {
                entity: target,
                value: origin.0,
            });
        }
        commands.entity(target).remove::<SliderDragOrigin>();
        commands
            .entity(holder)
            .remove::<(crate::drag::DragState, Pressed)>();
    }
}

/// Paint every `<toggle>` / `<switch>` track with the fill its current
/// state calls for. This is the single owner of a track's
/// [`Visuals::fill`]; the per-widget sync systems only move the knob.
///
/// It runs every tick rather than on `Changed<Toggleable>`, because the
/// state is not the only thing that repaints a track: the restyle pass
/// re-applies the resting `bg` on a theme or media flip, and a gate on
/// state change would leave that repaint standing and the control stuck
/// looking off. Writes happen only when the fill differs, so an idle
/// frame still triggers no change detection.
///
/// A track whose fill the hover / press tint FSM currently owns keeps
/// its tint; the captured [`crate::hover::HoverBaseColor`] /
/// [`crate::hover::PressBaseColor`] snapshot is rebased instead, so the
/// tint releases onto the new state color rather than the stale one.
/// A restyle tween aimed at the resting fill is retired for the same
/// reason: state, not the cascade, decides what a track shows.
#[allow(clippy::type_complexity)]
pub fn sync_track_fill(
    mut commands: Commands,
    mut tracks: Query<(
        Entity,
        &Toggleable,
        &TrackStyle,
        Has<lumen_core::components::Disabled>,
        &mut Visuals,
        Option<&mut crate::hover::HoverBaseColor>,
        Option<&mut crate::hover::PressBaseColor>,
    )>,
) {
    for (entity, t, style, disabled, mut vis, hover_base, press_base) in &mut tracks {
        let target = style.fill_for(t.checked, disabled);
        let tinted = hover_base.is_some() || press_base.is_some();
        if let Some(mut base) = hover_base {
            base.0 = target;
        }
        if let Some(mut base) = press_base {
            base.0 = target;
        }
        if tinted {
            continue;
        }
        if vis.fill.as_ref().and_then(Fill::as_solid) != Some(target) {
            vis.fill = Some(Fill::Solid(target));
            commands
                .entity(entity)
                .remove::<crate::transition::BackgroundTransition>();
        }
    }
}

/// Slide the [`ToggleKnob`] child of every `<toggle>` to the track end
/// its [`Toggleable::checked`] state calls for, and size it from the
/// laid-out track. The track fill itself belongs to
/// [`sync_track_fill`].
#[allow(clippy::type_complexity)]
pub fn sync_toggle_visuals(
    mut commands: Commands,
    parents: Query<(&Toggleable, &Transform, Option<&KnobGeometry>)>,
    mut knobs: Query<
        (Entity, &ChildOf, &mut Style, &mut Visuals),
        (With<ToggleKnob>, Without<Toggleable>),
    >,
) {
    for (knob_e, child_of, mut style, mut vis) in &mut knobs {
        let Ok((t, tr, geo)) = parents.get(child_of.parent()) else {
            continue;
        };
        if tr.size.x <= 0.0 || tr.size.y <= 0.0 {
            continue;
        }
        let inset = geo.copied().unwrap_or_default().inset;
        let knob = (tr.size.y - 2.0 * inset).max(2.0);
        let left = if t.checked {
            (tr.size.x - knob - inset).max(inset)
        } else {
            inset
        };
        let radius = knob / 2.0;
        if vis.radius != radius {
            vis.radius = radius;
        }
        let size = Length::Px(knob);
        if style.width != size
            || style.height != size
            || style.inset.left != left
            || style.inset.top != inset
        {
            style.width = size;
            style.height = size;
            style.inset.left = left;
            style.inset.top = inset;
            commands.entity(knob_e).insert(DirtyLayout);
        }
    }
}

/// Place the [`SliderThumb`] child along its track at
/// `(value - min) / (max - min)`, vertically centred. Runs every tick
/// (the position depends on the track's laid-out size, not just
/// [`SliderValue`]); writes only when the target inset actually moved
/// so change detection and relayout stay quiet on idle frames.
pub fn sync_slider_thumb(
    mut commands: Commands,
    parents: Query<(&SliderValue, &Transform, Option<&KnobGeometry>)>,
    mut thumbs: Query<(Entity, &ChildOf, &mut Style), With<SliderThumb>>,
) {
    for (thumb_e, child_of, mut style) in &mut thumbs {
        let Ok((s, tr, geo)) = parents.get(child_of.parent()) else {
            continue;
        };
        if tr.size.x <= 0.0 {
            continue;
        }
        let thumb_size = geo.copied().unwrap_or_default().thumb_size;
        let denom = s.max - s.min;
        let frac = if denom.abs() <= f32::EPSILON {
            0.0
        } else {
            ((s.value - s.min) / denom).clamp(0.0, 1.0)
        };
        let left = frac * (tr.size.x - thumb_size).max(0.0);
        let top = ((tr.size.y - thumb_size) / 2.0).max(0.0);
        if (style.inset.left - left).abs() > 0.25 || (style.inset.top - top).abs() > 0.25 {
            style.inset.left = left;
            style.inset.top = top;
            commands.entity(thumb_e).insert(DirtyLayout);
        }
    }
}

#[cfg(test)]
mod slider_key_tests {
    //! `move_slider_on_keys` - arrow/Home/End/PageUp/PageDown control of a
    //! focused `<slider>`. Driven directly via `run_system_once` against a
    //! bare `World` (no full `App`/schedule needed since the system only
    //! touches `FocusTracker`, `KeyPressed`, and `SliderValue`).
    use super::*;
    use bevy_ecs::system::RunSystemOnce;

    fn setup(min: f32, max: f32, value: f32) -> (World, Entity) {
        let mut world = World::new();
        world.init_resource::<Messages<KeyPressed>>();
        world.init_resource::<Messages<SliderChanged>>();
        let e = world
            .spawn(SliderValue {
                value,
                min,
                max,
                step: None,
            })
            .id();
        world.insert_resource(FocusTracker(Some(e)));
        (world, e)
    }

    fn press(world: &mut World, key: Key) {
        world
            .resource_mut::<Messages<KeyPressed>>()
            .write(KeyPressed {
                key,
                modifiers: Modifiers::default(),
                repeat: false,
            });
    }

    /// `run_system_once` builds a fresh system (and thus a fresh
    /// `MessageReader` cursor) on every call, so - unlike a real ticked
    /// `App` - a message written before one call is still visible to
    /// the *next* call's brand-new reader unless the buffer is cleared
    /// in between. Tests that press more than one key in sequence must
    /// clear after each `run_system_once` to avoid re-processing stale
    /// key presses.
    fn run_and_clear(world: &mut World) {
        world.run_system_once(move_slider_on_keys).unwrap();
        world.resource_mut::<Messages<KeyPressed>>().clear();
    }

    fn value_of(world: &World, e: Entity) -> f32 {
        world.get::<SliderValue>(e).unwrap().value
    }

    #[test]
    fn arrow_right_increments_by_one_step() {
        let (mut world, e) = setup(0.0, 100.0, 50.0);
        press(&mut world, Key::Named(NamedKey::ArrowRight));
        run_and_clear(&mut world);
        assert_eq!(value_of(&world, e), 51.0); // step = (100-0)/100 = 1.0
    }

    #[test]
    fn arrow_left_decrements_by_one_step() {
        let (mut world, e) = setup(0.0, 100.0, 50.0);
        press(&mut world, Key::Named(NamedKey::ArrowLeft));
        run_and_clear(&mut world);
        assert_eq!(value_of(&world, e), 49.0);
    }

    #[test]
    fn arrow_up_and_down_mirror_left_right() {
        let (mut world, e) = setup(0.0, 100.0, 50.0);
        press(&mut world, Key::Named(NamedKey::ArrowUp));
        run_and_clear(&mut world);
        assert_eq!(value_of(&world, e), 51.0);
    }

    #[test]
    fn home_and_end_jump_to_bounds() {
        let (mut world, e) = setup(0.0, 100.0, 50.0);
        press(&mut world, Key::Named(NamedKey::End));
        run_and_clear(&mut world);
        assert_eq!(value_of(&world, e), 100.0);

        press(&mut world, Key::Named(NamedKey::Home));
        run_and_clear(&mut world);
        assert_eq!(value_of(&world, e), 0.0);
    }

    #[test]
    fn page_up_and_down_move_by_ten_steps() {
        let (mut world, e) = setup(0.0, 100.0, 50.0);
        press(&mut world, Key::Character("PageUp".into()));
        run_and_clear(&mut world);
        assert_eq!(value_of(&world, e), 60.0);

        press(&mut world, Key::Character("PageDown".into()));
        run_and_clear(&mut world);
        assert_eq!(value_of(&world, e), 50.0);
    }

    #[test]
    fn arrow_right_clamps_at_max() {
        let (mut world, e) = setup(0.0, 100.0, 99.6);
        press(&mut world, Key::Named(NamedKey::ArrowRight));
        run_and_clear(&mut world);
        assert_eq!(value_of(&world, e), 100.0);
    }

    #[test]
    fn authored_step_attribute_overrides_default_step() {
        let (mut world, e) = setup(0.0, 100.0, 50.0);
        world.get_mut::<SliderValue>(e).unwrap().step = Some(5.0);
        press(&mut world, Key::Named(NamedKey::ArrowRight));
        run_and_clear(&mut world);
        assert_eq!(
            value_of(&world, e),
            55.0,
            "step=\"5\" wins over (max-min)/100"
        );
    }

    #[test]
    fn unfocused_slider_ignores_keys() {
        let (mut world, e) = setup(0.0, 100.0, 50.0);
        world.insert_resource(FocusTracker(None));
        press(&mut world, Key::Named(NamedKey::ArrowRight));
        run_and_clear(&mut world);
        assert_eq!(value_of(&world, e), 50.0);
    }

    /// A legit descending slider (`<slider min="100" max="0">`) driven by
    /// Arrow/Home/End keys must not panic - `f32::clamp` asserts
    /// `min <= max` - and the value must stay inside `[0, 100]`.
    #[test]
    fn inverted_range_keys_do_not_panic_and_clamp() {
        let (mut world, e) = setup(100.0, 0.0, 50.0);
        for key in [
            Key::Named(NamedKey::ArrowRight),
            Key::Named(NamedKey::ArrowLeft),
            Key::Named(NamedKey::End),
            Key::Named(NamedKey::Home),
            Key::Character("PageUp".into()),
            Key::Character("PageDown".into()),
        ] {
            press(&mut world, key);
            run_and_clear(&mut world);
            let v = value_of(&world, e);
            assert!(
                v.is_finite() && (0.0..=100.0).contains(&v),
                "inverted-range value escaped [0,100]: {v}"
            );
        }
        // Home / End on a descending slider jump to the authored bounds.
        press(&mut world, Key::Named(NamedKey::End));
        run_and_clear(&mut world);
        assert_eq!(value_of(&world, e), 0.0, "End jumps to authored max (0)");
        press(&mut world, Key::Named(NamedKey::Home));
        run_and_clear(&mut world);
        assert_eq!(
            value_of(&world, e),
            100.0,
            "Home jumps to authored min (100)"
        );
    }

    /// NaN bounds must not panic (they arrive from a malformed
    /// `min`/`max` attribute or a mid-layout degenerate range).
    #[test]
    fn nan_range_keys_do_not_panic() {
        let (mut world, e) = setup(f32::NAN, 100.0, 50.0);
        press(&mut world, Key::Named(NamedKey::End));
        run_and_clear(&mut world);
        // End targets `max` (100), which is finite; the clamp guard must
        // not propagate the NaN `min` bound.
        assert!(value_of(&world, e).is_finite());
    }

    /// Wheel over a descending slider must not panic and must clamp into
    /// `[0, 100]`.
    #[test]
    fn inverted_range_wheel_does_not_panic_and_clamps() {
        let mut world = World::new();
        world.init_resource::<Messages<MouseWheel>>();
        world.init_resource::<Messages<SliderChanged>>();
        let e = world
            .spawn((
                SliderValue {
                    value: 50.0,
                    min: 100.0,
                    max: 0.0,
                    step: None,
                },
                Hovered,
            ))
            .id();
        world
            .resource_mut::<Messages<MouseWheel>>()
            .write(MouseWheel {
                delta: glam::Vec2::new(0.0, WHEEL_NOTCH_PX * 100.0),
                position: glam::Vec2::ZERO,
            });
        world.run_system_once(adjust_slider_on_wheel).unwrap();
        let v = value_of(&world, e);
        assert!(
            v.is_finite() && (0.0..=100.0).contains(&v),
            "inverted-range wheel value escaped [0,100]: {v}"
        );
    }
}

#[cfg(test)]
mod track_tests {
    //! What a track paints and where its knob sits.
    //!
    //! [`TrackStyle`] routing: each state paints the fill it names, and a
    //! repaint from outside (a restyle re-applying the resting `bg`) does
    //! not leave a track showing the wrong state.
    //!
    //! [`KnobGeometry`] plumbing: an entity with no component reproduces
    //! today's hardcoded [`KNOB_INSET`] / [`THUMB_SIZE`] geometry exactly;
    //! a CSS-supplied component (as the spawn agent would insert from
    //! `knob-inset` / `thumb-size`) moves the knob / thumb instead.
    use super::*;
    use bevy_ecs::system::RunSystemOnce;
    use glam::Vec2;

    #[test]
    fn slider_frac_uses_the_given_thumb_size() {
        // Track 200 wide; default 16px thumb vs. a wider 40px thumb should
        // map the same pointer x to different fractions.
        let default = slider_frac(100.0, 0.0, 200.0, THUMB_SIZE);
        let wider = slider_frac(100.0, 0.0, 200.0, 40.0);
        assert!(
            (default - 0.5).abs() < 1e-6,
            "pointer at the track centre with the default thumb -> frac 0.5"
        );
        assert!(
            (wider - 0.5).abs() < 1e-6,
            "pointer at the track centre is still frac 0.5 regardless of \
             thumb size (the reduced range stays centred)"
        );
        // Off-centre: the wider thumb's reduced range is smaller, so the
        // same pointer x maps to a different fraction.
        let default_off = slider_frac(60.0, 0.0, 200.0, THUMB_SIZE);
        let wider_off = slider_frac(60.0, 0.0, 200.0, 40.0);
        assert!(
            (default_off - wider_off).abs() > 1e-3,
            "a different thumb_size must change the mapping off-centre"
        );
    }

    fn spawn_toggle(
        world: &mut World,
        checked: bool,
        geo: Option<KnobGeometry>,
    ) -> (Entity, Entity) {
        let mut track = world.spawn((
            Toggleable { checked },
            Transform::new(Vec2::ZERO, Vec2::new(48.0, 24.0)),
        ));
        if let Some(geo) = geo {
            track.insert(geo);
        }
        let track = track.id();
        let knob = world
            .spawn((
                ToggleKnob,
                Style::default(),
                Visuals::default(),
                ChildOf(track),
            ))
            .id();
        (track, knob)
    }

    fn knob_inset_of(world: &World, knob: Entity) -> f32 {
        world.get::<Style>(knob).unwrap().inset.left
    }

    /// A track carrying the whole palette, plus the `Visuals` the fill
    /// sync writes into. `disabled` inserts the marker the cascade's
    /// `:disabled` rule keys off.
    fn spawn_track(world: &mut World, checked: bool, disabled: bool) -> Entity {
        let mut e = world.spawn((
            Toggleable { checked },
            TrackStyle {
                checked_bg: Color::rgb(0.0, 1.0, 0.0),
                unchecked_bg: Color::rgb(0.5, 0.5, 0.5),
                disabled_bg: Some(Color::rgb(0.0, 0.0, 1.0)),
            },
            Visuals::default(),
        ));
        if disabled {
            e.insert(lumen_core::components::Disabled);
        }
        e.id()
    }

    fn fill_of(world: &World, e: Entity) -> Option<Color> {
        world
            .get::<Visuals>(e)
            .and_then(|v| v.fill.as_ref().and_then(Fill::as_solid))
    }

    #[test]
    fn track_paints_the_fill_its_state_names() {
        let mut world = World::new();
        let off = spawn_track(&mut world, false, false);
        let on = spawn_track(&mut world, true, false);
        let disabled = spawn_track(&mut world, false, true);
        // Disabled outranks checked, so a disabled-and-checked track still
        // reads as unavailable rather than on.
        let both = spawn_track(&mut world, true, true);
        world.run_system_once(sync_track_fill).unwrap();
        assert_eq!(fill_of(&world, off), Some(Color::rgb(0.5, 0.5, 0.5)));
        assert_eq!(fill_of(&world, on), Some(Color::rgb(0.0, 1.0, 0.0)));
        assert_eq!(fill_of(&world, disabled), Some(Color::rgb(0.0, 0.0, 1.0)));
        assert_eq!(fill_of(&world, both), Some(Color::rgb(0.0, 0.0, 1.0)));
    }

    #[test]
    fn track_with_no_disabled_rule_keeps_its_state_fill() {
        let mut world = World::new();
        let e = world
            .spawn((
                Toggleable { checked: true },
                TrackStyle {
                    checked_bg: Color::rgb(0.0, 1.0, 0.0),
                    unchecked_bg: Color::rgb(0.5, 0.5, 0.5),
                    disabled_bg: None,
                },
                Visuals::default(),
                lumen_core::components::Disabled,
            ))
            .id();
        world.run_system_once(sync_track_fill).unwrap();
        assert_eq!(
            fill_of(&world, e),
            Some(Color::rgb(0.0, 1.0, 0.0)),
            "with no `:disabled {{ bg }}` rule the generic opacity dim is \
             the disabled affordance, so the state fill stands"
        );
    }

    #[test]
    fn track_repaints_after_a_restyle_overwrites_its_fill() {
        // The restyle pass re-applies the resting `bg` on a theme or media
        // flip. Gating the repaint on `Changed<Toggleable>` left a checked
        // track stuck on the unchecked color from then on.
        let mut world = World::new();
        let on = spawn_track(&mut world, true, false);
        world.run_system_once(sync_track_fill).unwrap();
        world.get_mut::<Visuals>(on).unwrap().fill = Some(Fill::Solid(Color::rgb(0.5, 0.5, 0.5)));
        world.run_system_once(sync_track_fill).unwrap();
        assert_eq!(fill_of(&world, on), Some(Color::rgb(0.0, 1.0, 0.0)));
    }

    #[test]
    fn tinted_track_rebases_instead_of_stomping_the_tint() {
        let mut world = World::new();
        let on = spawn_track(&mut world, true, false);
        let tint = Color::rgb(1.0, 0.0, 0.0);
        world
            .entity_mut(on)
            .insert(crate::hover::HoverBaseColor(Color::rgb(0.5, 0.5, 0.5)));
        world.get_mut::<Visuals>(on).unwrap().fill = Some(Fill::Solid(tint));
        world.run_system_once(sync_track_fill).unwrap();
        assert_eq!(
            fill_of(&world, on),
            Some(tint),
            "the hover tween owns the fill mid-tint"
        );
        assert_eq!(
            world.get::<crate::hover::HoverBaseColor>(on).unwrap().0,
            Color::rgb(0.0, 1.0, 0.0),
            "the tint must release onto the current state color"
        );
    }

    #[test]
    fn toggle_knob_with_no_geometry_uses_the_default_inset() {
        let mut world = World::new();
        let (_, knob) = spawn_toggle(&mut world, false, None);
        world.run_system_once(sync_toggle_visuals).unwrap();
        assert_eq!(
            knob_inset_of(&world, knob),
            KNOB_INSET,
            "no KnobGeometry -> today's hardcoded inset"
        );
    }

    #[test]
    fn toggle_knob_with_custom_geometry_uses_the_authored_inset() {
        let mut world = World::new();
        let custom = KnobGeometry {
            inset: 10.0,
            thumb_size: THUMB_SIZE,
        };
        let (_, knob) = spawn_toggle(&mut world, false, Some(custom));
        world.run_system_once(sync_toggle_visuals).unwrap();
        assert_eq!(
            knob_inset_of(&world, knob),
            10.0,
            "a CSS-supplied inset moves the knob off the default position"
        );
    }

    fn spawn_slider_track(
        world: &mut World,
        value: f32,
        geo: Option<KnobGeometry>,
    ) -> (Entity, Entity) {
        let mut track = world.spawn((
            SliderValue {
                value,
                min: 0.0,
                max: 100.0,
                step: None,
            },
            Transform::new(Vec2::ZERO, Vec2::new(200.0, 24.0)),
        ));
        if let Some(geo) = geo {
            track.insert(geo);
        }
        let track = track.id();
        let thumb = world
            .spawn((SliderThumb, Style::default(), ChildOf(track)))
            .id();
        (track, thumb)
    }

    fn thumb_left_of(world: &World, thumb: Entity) -> f32 {
        world.get::<Style>(thumb).unwrap().inset.left
    }

    #[test]
    fn slider_thumb_with_no_geometry_uses_the_default_size() {
        let mut world = World::new();
        let (_, thumb) = spawn_slider_track(&mut world, 100.0, None);
        world.run_system_once(sync_slider_thumb).unwrap();
        // value = max -> thumb pinned at the right edge: 200 - THUMB_SIZE.
        assert_eq!(thumb_left_of(&world, thumb), 200.0 - THUMB_SIZE);
    }

    #[test]
    fn slider_thumb_with_custom_geometry_uses_the_authored_size() {
        let mut world = World::new();
        let custom = KnobGeometry {
            inset: KNOB_INSET,
            thumb_size: 40.0,
        };
        let (_, thumb) = spawn_slider_track(&mut world, 100.0, Some(custom));
        world.run_system_once(sync_slider_thumb).unwrap();
        // value = max -> thumb pinned at the right edge: 200 - 40.
        assert_eq!(
            thumb_left_of(&world, thumb),
            160.0,
            "a CSS-supplied thumb_size changes the thumb's rest position"
        );
    }
}
