//! Interaction-state paint primitives: hover, press, focus ring - all
//! folded into a single [`Interaction`] fat component.
//!
//! `Interaction.hover_tint` blends the [`Visuals`] solid fill toward a
//! tint while the entity is [`Hovered`] or [`Focused`]. `press_tint`
//! overrides that blend when the entity is [`Pressed`] - pressed >
//! hovered/focused > idle. `focus_outline` paints a stroked ring around
//! the entity while it has [`Focused`]. Gradient fills are skipped by
//! the tween (no animation path); the gradient renders unchanged.
//!
//! Tween state (`HoverBaseColor`, `HoverTween`, `PressTween`) stays in
//! separate transient components - they're auto-attached / cleared by
//! the systems below, never authored.

use bevy_ecs::prelude::*;
use lumen_core::components::{Fill, Visuals};
use lumen_core::prelude::*;
use lumen_core::render_world::{ExtractedOutline, build_parent_map, paint_order_of};
use lumen_core::time::Instant;

/// Default-construct a `last_step: Instant` for tween components. The
/// systems immediately overwrite this with `Tick.now` on the first step,
/// so the wall-clock initialiser only exists to keep the public
/// `Default` impl total - actual elapsed-time math always runs through
/// the shared [`Tick`] resource.
fn tween_instant_seed() -> Instant {
    Instant::now()
}

/// Author-set interaction-state paint: hover tint + press tint + focus
/// outline, all on one component. Each field is `Option`-shaped so
/// markup can opt into any subset. Absent component = no
/// hover / press / focus visual feedback.
#[derive(Component, Clone, Copy, Debug, Default, PartialEq)]
pub struct Interaction {
    /// Color blended into a solid [`Visuals::fill`] while hovered or
    /// focused. Markup: `hover-bg="#xxx"`.
    pub hover_tint: Option<Color>,
    /// Color blended into a solid fill while pressed. Wins over
    /// `hover_tint`. Markup: `press-bg="#xxx"`.
    pub press_tint: Option<Color>,
    /// Outline ring stroked around the entity while focused. Markup:
    /// `focus-outline="<width>px <#color>"`.
    pub focus_outline: Option<FocusOutlineSpec>,
    /// Outline ring shown only when focus arrived via the keyboard
    /// (CSS `:focus-visible`). Wins over [`Self::focus_outline`] while
    /// the `FocusVisible` marker is present; pointer-driven focus falls
    /// back to `focus_outline` (or nothing).
    pub focus_visible_outline: Option<FocusOutlineSpec>,
    /// Border swapped into [`Visuals::border`] while hovered. CSS:
    /// `:hover { border: ... }` (or the Lumen-native `hover-border`
    /// property). Snaps rather than tweening - same as CSS without a
    /// `transition`.
    pub hover_border: Option<lumen_core::components::Border>,
    /// Border swapped into [`Visuals::border`] while focused. Wins over
    /// [`Self::hover_border`] when both states are active (mirrors the
    /// skin convention that focus chrome is authored after hover).
    pub focus_border: Option<lumen_core::components::Border>,
}

/// Stored on [`Interaction::focus_outline`]. Was a free-standing
/// `FocusOutline` component before the alpha2 collapse.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct FocusOutlineSpec {
    /// Stroke width in logical pixels.
    pub width: f32,
    /// Stroke color.
    pub color: Color,
    /// CSS `outline-offset`: gap between the border box edge and the
    /// ring's inner edge. Default 0.
    pub offset: f32,
}

/// Tracks the *original* background color so we can restore it when the
/// pointer leaves. Auto-attached by [`apply_hover_tint`] the first time
/// the entity becomes [`Hovered`].
#[derive(Component, Clone, Copy, Debug)]
pub struct HoverBaseColor(pub Color);

/// How long it takes for the hover-tint to fully blend in or out, in
/// seconds. Picked to feel responsive but not jittery - comparable to a
/// macOS button highlight, faster than a Material ripple.
pub const HOVER_TWEEN_DURATION: f32 = 0.12;

/// Per-entity timer driving the hover tween. `progress` runs 0 -> 1 toward
/// `HoverTint` while hovered/focused, and 1 -> 0 toward `HoverBaseColor`
/// when both interaction markers are gone. Idempotent across frames so the
/// system can simply step `progress` by delta-time.
#[derive(Component, Clone, Copy, Debug)]
pub struct HoverTween {
    /// Currently in [0, 1].
    pub progress: f32,
    /// Where to push `progress` toward (0 = idle, 1 = active).
    pub target: f32,
    /// Last instant the tween was advanced. Wall-clock - the plugin doesn't
    /// run on a fixed timestep, so we measure real elapsed time.
    pub last_step: Instant,
}

impl Default for HoverTween {
    fn default() -> Self {
        Self {
            progress: 0.0,
            target: 0.0,
            last_step: tween_instant_seed(),
        }
    }
}

/// Plugin: registers [`apply_hover_tint`] then [`apply_press_tint`] in
/// `TickStage::Systems`. Press runs second so it can stamp over whatever
/// hover wrote on the same tick.
pub struct HoverTintPlugin;

impl Plugin for HoverTintPlugin {
    fn build(self, app: &mut App) {
        // `.after(hit_test)` / `.after(dispatch_clicks)`: the Hovered /
        // Pressed markers land via Commands; the explicit edges pull in
        // the sync point so this tick's paint reflects this tick's
        // pointer state. Without them the press/hover visual trailed the
        // marker by one tick - invisible under a streaming mouse, but a
        // real frame of latency for taps and for the capture re-entry
        // re-press (spec section 0 rule 3). No-ops when `lumen-input` isn't
        // installed (the target set is empty).
        app.add_systems(
            TickStage::Systems,
            apply_hover_tint.after(lumen_input::hit_test),
        );
        app.add_systems(
            TickStage::Systems,
            apply_press_tint
                .after(apply_hover_tint)
                .after(lumen_input::dispatch_clicks),
        );
        app.add_systems(TickStage::Systems, apply_state_borders);
        // Render-world side: focused entities also produce an outline.
        app.add_extract_fn(extract_focus_outlines);
    }
}

/// Snapshot of the idle [`Visuals::border`] captured before a
/// hover/focus border swap so the original (possibly `None`) border can
/// be restored when both states end. Auto-attached / removed by
/// [`apply_state_borders`].
#[derive(Component, Clone, Copy, Debug)]
pub struct BaseBorder(pub Option<lumen_core::components::Border>);

/// Swap [`Visuals::border`] per interaction state: focused ->
/// [`Interaction::focus_border`], hovered -> [`Interaction::hover_border`],
/// idle -> the captured [`BaseBorder`]. Focus wins over hover when both
/// are active. Borders snap (no tween), matching CSS `:hover` /
/// `:focus` border rules without a `transition`.
#[allow(clippy::type_complexity)]
pub fn apply_state_borders(
    mut commands: Commands,
    mut active: Query<
        (
            Entity,
            &Interaction,
            &mut Visuals,
            Option<&BaseBorder>,
            Option<&Hovered>,
            Option<&Focused>,
        ),
        Or<(With<Hovered>, With<Focused>)>,
    >,
    mut idle: Query<(Entity, &mut Visuals, &BaseBorder), (Without<Hovered>, Without<Focused>)>,
) {
    for (entity, ix, mut vis, base, hovered, focused) in &mut active {
        let want = match (
            focused.and(ix.focus_border.as_ref()),
            hovered.and(ix.hover_border.as_ref()),
        ) {
            (Some(b), _) => Some(*b),
            (None, Some(b)) => Some(*b),
            (None, None) => continue,
        };
        if base.is_none() {
            commands.entity(entity).insert(BaseBorder(vis.border));
        }
        if vis.border != want {
            vis.border = want;
        }
    }
    for (entity, mut vis, base) in &mut idle {
        if vis.border != base.0 {
            vis.border = base.0;
        }
        commands.entity(entity).remove::<BaseBorder>();
    }
}

/// Extract: for every focused entity whose [`Interaction`] declares a
/// focus outline, spawn an [`ExtractedOutline`] in the render world
/// that draws on top of the matching rect.
pub fn extract_focus_outlines(main: &mut World, render: &mut World) {
    use lumen_core::components::Opacity;
    use lumen_core::render_world::{RenderEntityMap, hidden_entities, parent_scroll_offsets};
    let (parents, mut depth_cache) = build_parent_map(main);
    let scroll = parent_scroll_offsets(main, &parents);
    // A `Visible(false)` on the focused entity or any ancestor suppresses its
    // focus ring along with the rest of the hidden subtree.
    let hidden = hidden_entities(main, &parents);
    type Row<'a> = (
        Entity,
        &'a Transform,
        &'a Interaction,
        Option<&'a Opacity>,
        Option<&'a Visuals>,
        Option<&'a lumen_core::input::FocusVisible>,
    );
    let mut q = main.query_filtered::<Row, With<Focused>>();
    let pairs: Vec<(Entity, ExtractedOutline)> = q
        .iter(main)
        .filter(|(e, ..)| !hidden.contains(e))
        .filter_map(|(e, t, ix, opacity, vis, focus_visible)| {
            // `:focus-visible` ring wins while focus is keyboard-driven;
            // otherwise the always-on `:focus` ring (which may be absent
            // - a skin can style keyboard focus only).
            let ring = match (focus_visible, ix.focus_visible_outline) {
                (Some(_), Some(fv)) => fv,
                _ => ix.focus_outline?,
            };
            let alpha = opacity.copied().unwrap_or_default();
            let off = scroll.get(&e).copied().unwrap_or(glam::Vec2::ZERO);
            // CSS `outline` semantics: the ring paints just OUTSIDE the
            // border box, pushed out further by `outline-offset`, and
            // never affects layout. The emitter strokes centered on the
            // given rect, so grow the rect by offset + half the stroke
            // width per side; the ring then covers exactly
            // [box edge + offset, box edge + offset + width]. Corner
            // rounding follows the box radius concentrically.
            let half = ring.width * 0.5;
            let grow = ring.offset + half;
            let radius = vis.map(|v| v.radius).unwrap_or(0.0);
            Some((
                e,
                ExtractedOutline {
                    origin: t.absolute - off - glam::Vec2::splat(grow),
                    size: t.size + glam::Vec2::splat(grow * 2.0),
                    stroke: alpha.apply(ring.color),
                    width: ring.width,
                    radius: if radius > 0.0 {
                        radius + grow.max(0.0)
                    } else {
                        0.0
                    },
                    order: paint_order_of(e, &parents, &mut depth_cache),
                },
            ))
        })
        .collect();
    // Keyed-upsert against `RenderEntityMap.outline`; reused render entities are validated to drop recycled ids.
    let prior = std::mem::take(&mut render.resource_mut::<RenderEntityMap>().outline);
    let mut next: std::collections::HashMap<Entity, Entity> =
        std::collections::HashMap::with_capacity(pairs.len());
    for (main_e, outline) in pairs {
        let reuse = prior
            .get(&main_e)
            .copied()
            .filter(|&re| render.get_entity(re).is_ok());
        let render_e = match reuse {
            Some(re) => {
                render.entity_mut(re).insert(outline);
                re
            }
            None => render.spawn(outline).id(),
        };
        next.insert(main_e, render_e);
    }
    for (main_e, render_e) in &prior {
        if !next.contains_key(main_e)
            && let Ok(em) = render.get_entity_mut(*render_e)
        {
            em.despawn();
        }
    }
    render.resource_mut::<RenderEntityMap>().outline = next;
}

/// Resolve the duration + easing the hover / press tint tween should
/// use. A CSS `transition: background-color <dur> <easing>` declaration
/// on the entity (its [`TransitionSpecs`]) wins; otherwise the built-in
/// `default_duration` + cubic ease-out.
///
/// [`TransitionSpecs`]: crate::transition::TransitionSpecs
fn bg_tween_params(
    specs: Option<&crate::transition::TransitionSpecs>,
    default_duration: f32,
) -> (f32, crate::transition::Easing) {
    match specs.and_then(|s| s.for_property(crate::transition::TransitionProperty::BackgroundColor))
    {
        Some(spec) => (spec.duration.as_secs_f32().max(f32::EPSILON), spec.easing),
        None => (default_duration, crate::transition::Easing::EaseOut),
    }
}

fn lerp(a: f32, b: f32, t: f32) -> f32 {
    a + (b - a) * t
}

fn lerp_color(a: Color, b: Color, t: f32) -> Color {
    Color {
        r: lerp(a.r, b.r, t),
        g: lerp(a.g, b.g, t),
        b: lerp(a.b, b.b, t),
        a: lerp(a.a, b.a, t),
    }
}

/// Read the solid fill color out of a `Visuals` mut handle, if it has
/// one. Returns `None` if the entity has no fill or has a gradient
/// (gradients are not animated by hover/press tweens).
fn solid_fill_color(v: &Visuals) -> Option<Color> {
    v.fill.as_ref().and_then(Fill::as_solid)
}

/// Write a new solid color back into `Visuals::fill`. No-ops when the
/// fill is `None` or a gradient.
fn set_solid_fill(v: &mut Visuals, c: Color) {
    if let Some(Fill::Solid(slot)) = v.fill.as_mut() {
        *slot = c;
    }
}

/// Drive [`HoverTween`]'s progress toward `target`, then blend the
/// solid [`Visuals::fill`] between [`HoverBaseColor`] and [`HoverTint`].
///
/// Idle entities (no `Hovered`/`Focused`) target 0; active ones target 1.
/// Once an idle entity finishes draining (progress = 0), the tween and
/// base-color components are removed to keep archetype churn bounded.
#[allow(clippy::type_complexity)]
pub fn apply_hover_tint(
    mut commands: Commands,
    tick: Res<Tick>,
    anim: Res<lumen_core::render_world::AnimationsActive>,
    mut active: Query<
        (
            Entity,
            &Interaction,
            &mut Visuals,
            Option<&HoverBaseColor>,
            Option<&mut HoverTween>,
            Option<&crate::transition::TransitionSpecs>,
        ),
        Or<(With<Hovered>, With<Focused>)>,
    >,
    mut idle: Query<
        (
            Entity,
            &mut Visuals,
            &HoverBaseColor,
            &Interaction,
            &mut HoverTween,
            Option<&crate::transition::TransitionSpecs>,
        ),
        (Without<Hovered>, Without<Focused>),
    >,
) {
    let now = tick.now;
    for (entity, ix, mut vis, base, tween, specs) in &mut active {
        let Some(tint) = ix.hover_tint else {
            continue;
        };
        let Some(current) = solid_fill_color(&vis) else {
            continue;
        };
        // CSS `transition: background-color ...` on the entity overrides
        // the built-in duration + curve for the state tween.
        let (duration, easing) = bg_tween_params(specs, HOVER_TWEEN_DURATION);
        // Capture the resting colour once, before the first tween (see
        // [`crate::baseline::capture_baseline`]).
        let base_color = crate::baseline::capture_baseline(
            &mut commands,
            entity,
            base,
            current,
            |b| b.0,
            HoverBaseColor,
        );
        let progress = match tween {
            Some(mut t) => {
                let dt = now.duration_since(t.last_step).as_secs_f32();
                t.last_step = now;
                t.target = 1.0;
                let step = (dt / duration).min(1.0);
                t.progress = (t.progress + step).min(1.0);
                t.progress
            }
            None => {
                commands.entity(entity).insert(HoverTween {
                    progress: 0.0,
                    target: 1.0,
                    last_step: now,
                });
                0.0
            }
        };
        // Ramp toward the hover tint still in flight - keep the loop awake.
        if progress < 1.0 {
            anim.request();
        }
        let next = lerp_color(base_color, tint, easing.apply(progress));
        if current != next {
            set_solid_fill(&mut vis, next);
        }
    }
    for (entity, mut vis, base, ix, mut tween, specs) in &mut idle {
        let Some(tint) = ix.hover_tint else {
            commands.entity(entity).remove::<HoverBaseColor>();
            commands.entity(entity).remove::<HoverTween>();
            continue;
        };
        let Some(current) = solid_fill_color(&vis) else {
            commands.entity(entity).remove::<HoverBaseColor>();
            commands.entity(entity).remove::<HoverTween>();
            continue;
        };
        let (duration, easing) = bg_tween_params(specs, HOVER_TWEEN_DURATION);
        let dt = now.duration_since(tween.last_step).as_secs_f32();
        tween.last_step = now;
        tween.target = 0.0;
        let step = (dt / duration).min(1.0);
        tween.progress = (tween.progress - step).max(0.0);
        // Hover-out fade still in flight - keep the loop awake.
        if tween.progress > 0.0 {
            anim.request();
        }
        let next = lerp_color(base.0, tint, easing.apply(tween.progress));
        if current != next {
            set_solid_fill(&mut vis, next);
        }
        if tween.progress <= 0.0 {
            // Pin to the exact base color and tear down the tween state so
            // archetype iteration stays cheap for hovers that never fire.
            if current != base.0 {
                set_solid_fill(&mut vis, base.0);
            }
            commands.entity(entity).remove::<HoverBaseColor>();
            commands.entity(entity).remove::<HoverTween>();
        }
    }
}

/// Press tween duration - faster than hover, matches the snappy "click
/// pulse" of native buttons.
pub const PRESS_TWEEN_DURATION: f32 = 0.06;

/// Per-entity press tween (mirror of [`HoverTween`] for the press state).
#[derive(Component, Clone, Copy, Debug)]
pub struct PressTween {
    /// Currently in [0, 1].
    pub progress: f32,
    /// Last instant the tween was advanced.
    pub last_step: Instant,
}

/// Snapshot of the solid fill captured at press-start so the entity can
/// be restored when the press releases on an entity that was never
/// [`Hovered`] (e.g. keyboard-down on a focused button, or pointer left
/// the bounds while pressed).
///
/// Distinct from [`HoverBaseColor`] because a press can fire without a
/// hover (keyboard activation), and a hover can end without a press
/// being involved. The two captures don't need to agree - whichever
/// snapshot is freshest at restoration time wins (hover takes priority
/// because hover paints below press in the layered model).
#[derive(Component, Clone, Copy, Debug)]
pub struct PressBaseColor(pub Color);

/// Override the solid [`Visuals::fill`] with [`PressTint`] while the
/// entity is *visually* pressed. Tweens over [`PRESS_TWEEN_DURATION`]
/// using ease-out.
///
/// Visually pressed = `Pressed` AND (currently over OR keyboard press):
/// during a pointer press, `Pressed` doubles as the capture marker and
/// survives the pointer dragging off the widget (spec section 0 rule 3), but
/// the pressed *visual* must un-press while off and re-press on
/// re-entry. "Currently over" is the [`Hovered`] marker (`hit_test`
/// confines it to the captured entity mid-press); keyboard presses
/// (Space FSM) have the primary button up and present pressed
/// regardless of hover.
///
/// Runs after [`apply_hover_tint`] so it stamps on top - pressed beats
/// hovered/focused. Restores by reading [`HoverBaseColor`] if present
/// (hover paint manages the base color) or the current solid fill
/// snapshot otherwise. Gradient-filled tiles are skipped (no animation
/// path).
#[allow(clippy::type_complexity)]
pub fn apply_press_tint(
    mut commands: Commands,
    tick: Res<Tick>,
    anim: Res<lumen_core::render_world::AnimationsActive>,
    pointer: Option<Res<PointerState>>,
    mut tinted: Query<
        (
            Entity,
            &Interaction,
            &mut Visuals,
            Option<&HoverBaseColor>,
            Option<&PressBaseColor>,
            Option<&mut PressTween>,
            Option<&crate::transition::TransitionSpecs>,
            Has<Pressed>,
            Has<Hovered>,
        ),
        Or<(With<Pressed>, With<PressTween>)>,
    >,
) {
    let now = tick.now;
    let primary_down = pointer.map(|p| p.primary_down).unwrap_or(false);
    for (entity, ix, mut vis, hover_base, press_base, tween, specs, is_pressed, is_hovered) in
        &mut tinted
    {
        let engaged = is_pressed && (is_hovered || !primary_down);
        let (duration, easing) = bg_tween_params(specs, PRESS_TWEEN_DURATION);
        if engaged {
            let Some(tint) = ix.press_tint else {
                continue;
            };
            let Some(current) = solid_fill_color(&vis) else {
                continue;
            };
            // Capture the press origin once, at first activation. We prefer
            // the hover-base when present (a press inside an active hover
            // should release back to the hover-tinted color, not to the
            // unhovered idle), otherwise we cache whatever solid fill the
            // entity carries at the moment of press - including for the
            // keyboard-activated, never-hovered case.
            let base_color = match (hover_base, press_base) {
                (Some(h), _) => h.0,
                (None, Some(p)) => p.0,
                (None, None) => {
                    commands.entity(entity).insert(PressBaseColor(current));
                    current
                }
            };
            let progress = match tween {
                Some(mut t) => {
                    let dt = now.duration_since(t.last_step).as_secs_f32();
                    t.last_step = now;
                    let step = (dt / duration).min(1.0);
                    t.progress = (t.progress + step).min(1.0);
                    t.progress
                }
                None => {
                    // Instant press tint: seed the tween fully-pressed so
                    // the FIRST frame after activation renders the full
                    // press tint rather than the base color. Press feedback
                    // must read as immediate - a 60 ms ramp-up from base
                    // means a quick tap often shows no tint at all. The
                    // disengaged branch below animates the fade back down
                    // from 1.0 -> 0.0. Re-entering the captured widget after
                    // a drag-off takes this same instant-on path.
                    commands.entity(entity).insert(PressTween {
                        progress: 1.0,
                        last_step: now,
                    });
                    1.0
                }
            };
            // Still ramping toward the pressed tint (only relevant if some
            // other code seeds progress < 1) - keep the loop awake so the
            // ramp completes without an unrelated event.
            if progress < 1.0 {
                anim.request();
            }
            let next = lerp_color(base_color, tint, easing.apply(progress));
            if current != next {
                set_solid_fill(&mut vis, next);
            }
        } else {
            // Not visually pressed: released, or captured-but-off. Fade
            // the tint back out. Entities that never tinted (pressed with
            // no tween yet - e.g. gained `Pressed` while dragged off)
            // have nothing to fade.
            let Some(mut tween) = tween else {
                continue;
            };
            let dt = now.duration_since(tween.last_step).as_secs_f32();
            tween.last_step = now;
            let step = (dt / duration).min(1.0);
            tween.progress = (tween.progress - step).max(0.0);
            // Release fade still in flight - keep the loop awake so the
            // fade reaches 0 without waiting for an unrelated OS event.
            if tween.progress > 0.0 {
                anim.request();
            }
            // Determine the press origin to blend FROM. Hover-base wins
            // when hover is currently driving the entity (a press released
            // while still hovered should snap back to the hover-tinted
            // color, not the raw idle fill); otherwise the press-base
            // snapshot captured at press-start is authoritative.
            let base_color = match (hover_base, press_base) {
                (Some(h), _) => Some(h.0),
                (None, Some(p)) => Some(p.0),
                (None, None) => None,
            };
            // Re-render the press blend at the new (decayed) progress so
            // long as we still have both the base and the tint. This is
            // the mirror of the engaged path's `lerp(base, tint,
            // progress)`.
            if let (Some(base_color), Some(tint), Some(current)) =
                (base_color, ix.press_tint, solid_fill_color(&vis))
            {
                let next = lerp_color(base_color, tint, easing.apply(tween.progress));
                if current != next {
                    set_solid_fill(&mut vis, next);
                }
            }
            if tween.progress <= 0.0 {
                // Final pin: restore exactly to the captured baseline so
                // float-blend rounding doesn't leave the entity painted at
                // a near-but-not-equal color forever.
                if let Some(c) = base_color
                    && let Some(current) = solid_fill_color(&vis)
                    && current != c
                {
                    set_solid_fill(&mut vis, c);
                }
                commands.entity(entity).remove::<PressTween>();
                // Drop the press-base snapshot only when hover isn't still
                // managing the fill - if it is, hover-released will clean
                // up its own `HoverBaseColor` and the press snapshot can be
                // discarded alongside. A captured-but-off press keeps the
                // snapshot only via the re-press path re-capturing it.
                if hover_base.is_none() {
                    commands.entity(entity).remove::<PressBaseColor>();
                }
            }
        }
    }
}

#[cfg(test)]
mod press_tint_tests {
    //! Pressed-but-off visual (spec section 0 rule 3): the press tint shows only
    //! while `Pressed` AND (`Hovered` OR pointer-up). Dragging off the
    //! captured widget fades the tint out; re-entering snaps it back on;
    //! keyboard presses (pointer up, no hover) tint too.
    use super::*;
    use bevy_ecs::schedule::Schedule;
    use lumen_core::input::PointerState;
    use lumen_core::render_world::AnimationsActive;

    const IDLE: Color = Color {
        r: 0.2,
        g: 0.2,
        b: 0.2,
        a: 1.0,
    };
    const TINT: Color = Color {
        r: 0.9,
        g: 0.1,
        b: 0.1,
        a: 1.0,
    };

    fn world_with_button(primary_down: bool) -> (World, Entity) {
        let mut world = World::new();
        world.insert_resource(Tick::default());
        world.init_resource::<AnimationsActive>();
        world.insert_resource(PointerState {
            position: None,
            primary_down,
        });
        let e = world
            .spawn((
                Interaction {
                    hover_tint: None,
                    press_tint: Some(TINT),
                    ..Default::default()
                },
                Visuals {
                    fill: Some(Fill::Solid(IDLE)),
                    ..Default::default()
                },
            ))
            .id();
        (world, e)
    }

    fn run(world: &mut World) {
        let mut s = Schedule::default();
        s.add_systems(apply_press_tint);
        s.run(world);
        // Second pass so command-inserted tween/base state is applied
        // and the blend lands.
        let mut s2 = Schedule::default();
        s2.add_systems(apply_press_tint);
        s2.run(world);
    }

    fn fill(world: &World, e: Entity) -> Color {
        solid_fill_color(world.get::<Visuals>(e).unwrap()).unwrap()
    }

    #[test]
    fn pressed_and_hovered_tints_dragged_off_untints_reenter_retints() {
        let (mut world, e) = world_with_button(true);
        world.entity_mut(e).insert((Pressed, Hovered));
        run(&mut world);
        assert_eq!(fill(&world, e), TINT, "pressed+over shows the press tint");

        // Drag off: capture retained (Pressed stays), hover gone -> the
        // visual must fade back toward idle. Advance time far enough to
        // finish the 60 ms fade in one step.
        world.entity_mut(e).remove::<Hovered>();
        world.resource_mut::<Tick>().now = Instant::now() + std::time::Duration::from_secs(1);
        run(&mut world);
        assert_eq!(
            fill(&world, e),
            IDLE,
            "captured-but-off shows the un-pressed visual"
        );
        assert!(world.get::<Pressed>(e).is_some(), "capture retained");

        // Re-enter: instant-on press tint again.
        world.entity_mut(e).insert(Hovered);
        run(&mut world);
        assert_eq!(fill(&world, e), TINT, "re-entering re-presses");
    }

    #[test]
    fn keyboard_press_tints_without_hover() {
        let (mut world, e) = world_with_button(false);
        world.entity_mut(e).insert(Pressed);
        run(&mut world);
        assert_eq!(
            fill(&world, e),
            TINT,
            "Space-FSM press (pointer up, no hover) still shows the tint"
        );
    }
}

#[cfg(test)]
mod state_border_tests {
    use super::*;
    use lumen_core::components::{Border, Edges};

    fn border(w: f32) -> Border {
        Border {
            widths: Edges::all(w),
            color: Color::rgb(1.0, 0.0, 0.0),
            side_colors: None,
        }
    }

    fn run(world: &mut bevy_ecs::world::World) {
        let mut schedule = bevy_ecs::schedule::Schedule::default();
        schedule.add_systems(apply_state_borders);
        schedule.run(world);
    }

    /// Hover swaps the border in; un-hover restores the captured base
    /// (including `None`); focus wins over hover when both are active.
    #[test]
    fn hover_focus_border_swap_and_restore() {
        let mut world = bevy_ecs::world::World::new();
        let e = world
            .spawn((
                Interaction {
                    hover_border: Some(border(1.0)),
                    focus_border: Some(border(2.0)),
                    ..Default::default()
                },
                Visuals::default(),
            ))
            .id();

        // Idle -> no swap.
        run(&mut world);
        assert_eq!(world.get::<Visuals>(e).unwrap().border, None);

        // Hover -> hover border, base captured.
        world.entity_mut(e).insert(Hovered);
        run(&mut world);
        assert_eq!(
            world.get::<Visuals>(e).unwrap().border,
            Some(border(1.0)),
            "hover border swapped in"
        );
        assert!(world.get::<BaseBorder>(e).is_some());

        // Hover + focus -> focus border wins.
        world.entity_mut(e).insert(lumen_core::input::Focused);
        run(&mut world);
        assert_eq!(world.get::<Visuals>(e).unwrap().border, Some(border(2.0)));

        // Both end -> base (None) restored, capture removed.
        world.entity_mut(e).remove::<Hovered>();
        world.entity_mut(e).remove::<lumen_core::input::Focused>();
        run(&mut world);
        assert_eq!(world.get::<Visuals>(e).unwrap().border, None);
        assert!(world.get::<BaseBorder>(e).is_none());
    }
}

#[cfg(test)]
mod focus_outline_tests {
    //! Focus-ring extraction: `outline-offset` grows the ring rect, and
    //! the `:focus-visible` ring wins over the always-on `:focus` ring
    //! only while the `FocusVisible` marker is present.
    use super::*;
    use lumen_core::input::FocusVisible;
    use lumen_core::render_world::RenderEntityMap;

    fn spec(width: f32, offset: f32, r: f32) -> FocusOutlineSpec {
        FocusOutlineSpec {
            width,
            color: Color::rgb(r, 0.0, 0.0),
            offset,
        }
    }

    fn outlines(render: &mut bevy_ecs::world::World) -> Vec<ExtractedOutline> {
        let mut q = render.query::<&ExtractedOutline>();
        q.iter(render).copied().collect()
    }

    #[test]
    fn outline_offset_grows_ring_and_focus_visible_ring_wins_on_keyboard_focus() {
        let mut main = bevy_ecs::world::World::new();
        let mut render = bevy_ecs::world::World::new();
        render.insert_resource(RenderEntityMap::default());

        let e = main
            .spawn((
                Transform::new(glam::Vec2::new(10.0, 10.0), glam::Vec2::new(100.0, 40.0)),
                Interaction {
                    focus_outline: Some(spec(2.0, 0.0, 0.25)),
                    focus_visible_outline: Some(spec(4.0, 3.0, 0.75)),
                    ..Default::default()
                },
                Focused,
            ))
            .id();

        // Pointer focus (no FocusVisible): the always-on :focus ring.
        extract_focus_outlines(&mut main, &mut render);
        let rings = outlines(&mut render);
        assert_eq!(rings.len(), 1);
        assert_eq!(rings[0].width, 2.0, "pointer focus paints the :focus ring");
        // offset 0 -> grown by half the width per side.
        assert_eq!(rings[0].origin, glam::Vec2::new(9.0, 9.0));
        assert_eq!(rings[0].size, glam::Vec2::new(102.0, 42.0));

        // Keyboard focus: the :focus-visible ring wins, offset 3 pushes
        // the rect out by offset + half width = 5 per side.
        main.entity_mut(e).insert(FocusVisible);
        extract_focus_outlines(&mut main, &mut render);
        let rings = outlines(&mut render);
        assert_eq!(rings.len(), 1);
        assert_eq!(rings[0].width, 4.0, "keyboard focus paints :focus-visible");
        assert_eq!(rings[0].origin, glam::Vec2::new(5.0, 5.0));
        assert_eq!(rings[0].size, glam::Vec2::new(110.0, 50.0));
    }
}
