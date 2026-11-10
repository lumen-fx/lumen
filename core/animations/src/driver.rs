//! Running an element's CSS animations: starting and stopping them as its
//! declarations change, sampling them each tick, raising their events, and
//! asking for frames only while one moves on screen.

use std::sync::Arc;

use bevy_ecs::hierarchy::ChildOf;
use bevy_ecs::message::MessageWriter;
use bevy_ecs::prelude::*;
use bevy_ecs::system::SystemParam;
use glam::Vec2;
use lumen_core::components::{Disabled, DropHovered, Style, Transform, Visible};
use lumen_core::input::{
    FocusVisible, Focused, HoverWithin, PointerState, PressWithin, ScrollOffset,
};
use lumen_core::motion::Easing;
use lumen_core::render_world::AnimationsActive;
use lumen_core::style_overrides::{StyleFields, StyleOverrides, StyleOwner, StyleValues};
use lumen_core::tick::{Tick, WakeDeadline};
use lumen_core::time::{Duration, Instant};
use lumen_core::window::{PrimaryWindow, WindowViewport};
use lumen_ir::animation::{
    AnimationDeclsIr, AnimationIr, AnimationPlayStateIr, AnimationTimelineIr,
};
use lumen_primitives::state_style::{StateMarkers, active_states};
use lumen_primitives::transition::{MotionComponents, live_of, write_motion_values};
use lumen_scene::style_sheet::{AnimationDeclarations, ImportantFields, StateAnimations};
use lumen_script::CustomEvent;

use crate::keyframes::{Frame, KeyframeSet, Keyframes};
use crate::timing::{Phase, Timing};

/// The [`StyleOverrides`] layer animations hold their values in, at rank
/// 70: above every normal declaration and state rule, below transitions, as
/// CSS orders them.
pub struct AnimationHolder;

impl StyleOwner for AnimationHolder {
    const RANK: u8 = 70;
}

/// One animation running on an element.
#[derive(Clone, Debug)]
pub struct Running {
    /// The animation as its declarations resolve now.
    pub spec: AnimationIr,
    /// Its timing function.
    pub easing: Easing,
    /// When its timeline started: the start of its delay.
    pub start: Instant,
    /// Where on its timeline it stopped, while paused.
    pub paused_at: Option<f32>,
    /// The phase and iteration its last event was raised for.
    pub reported: Option<(Phase, u64)>,
}

impl Running {
    fn new(spec: AnimationIr, now: Instant) -> Self {
        let paused = spec.play_state == AnimationPlayStateIr::Paused;
        Running {
            easing: Easing::from(&spec.timing),
            spec,
            start: now,
            paused_at: paused.then_some(0.0),
            reported: None,
        }
    }

    /// Its timing.
    pub fn timing(&self) -> Timing {
        Timing {
            duration: self.spec.duration_ms as f32 / 1000.0,
            delay: self.spec.delay_ms as f32 / 1000.0,
            iterations: self.spec.iteration_count,
            direction: self.spec.direction,
            fill: self.spec.fill_mode,
        }
    }

    /// Seconds into its timeline at `now`.
    pub fn local(&self, now: Instant) -> f32 {
        self.paused_at
            .unwrap_or_else(|| now.saturating_duration_since(self.start).as_secs_f32())
    }

    /// Take the declarations' new values, keeping its place on the timeline.
    fn update(&mut self, spec: AnimationIr, now: Instant) {
        let was_paused = self.paused_at.is_some();
        let pause = spec.play_state == AnimationPlayStateIr::Paused;
        match (was_paused, pause) {
            (false, true) => self.paused_at = Some(self.local(now)),
            (true, false) => {
                let at = self.paused_at.take().unwrap_or(0.0);
                self.start = now - Duration::from_secs_f32(at.max(0.0));
            }
            _ => {}
        }
        if spec.timing != self.spec.timing {
            self.easing = Easing::from(&spec.timing);
        }
        self.spec = spec;
    }
}

/// The animations running on an element, in the order its
/// `animation-name` lists them: a later one wins a property both set.
#[derive(Component, Clone, Debug, Default)]
#[require(StyleOverrides)]
pub struct RunningAnimations(pub Vec<Running>);

/// The interaction markers of an element, for the state rules.
type Markers = (
    Has<HoverWithin>,
    Option<&'static Focused>,
    Option<&'static FocusVisible>,
    Has<PressWithin>,
    Has<DropHovered>,
    Has<Disabled>,
);

/// Whether an element shows: no `Visible(false)` or `display: none` on it
/// or an ancestor. An animation on an element that does not show is
/// cancelled, and starts afresh when it shows again, as CSS does for
/// `display: none`.
#[derive(SystemParam)]
pub struct Shown<'w, 's> {
    nodes: Query<
        'w,
        's,
        (
            Option<&'static Visible>,
            Option<&'static Style>,
            Option<&'static ChildOf>,
        ),
    >,
}

impl Shown<'_, '_> {
    fn shows(&self, mut e: Entity) -> bool {
        loop {
            let Ok((visible, style, parent)) = self.nodes.get(e) else {
                return true;
            };
            if visible.is_some_and(|v| !v.0)
                || style.is_some_and(|s| s.display == lumen_core::components::Display::None)
            {
                return false;
            }
            match parent {
                Some(p) => e = p.parent(),
                None => return true,
            }
        }
    }
}

/// The animations an element's declarations run now: its own, with the
/// state rules that apply laid over them.
fn declared(
    base: Option<&AnimationDeclarations>,
    states: Option<&StateAnimations>,
    markers: StateMarkers,
) -> Vec<AnimationIr> {
    let mut decls: AnimationDeclsIr = base.map(|b| b.0.clone()).unwrap_or_default();
    if let Some(s) = states {
        let s = &s.0;
        for (on, d) in active_states(markers).into_iter().zip([
            &s.hover,
            &s.focus,
            &s.focus_visible,
            &s.active,
            &s.drag_over,
            &s.disabled,
        ]) {
            if on {
                decls.overlay(d);
            }
        }
    }
    decls
        .effective()
        .into_iter()
        .filter(|a| a.timeline != AnimationTimelineIr::None)
        .collect()
}

/// An event about `running`, for its element.
fn event(target: Entity, name: &str, running: &Running, elapsed: f32) -> CustomEvent {
    CustomEvent::new(target, name)
        .with("animation_name", running.spec.name.clone())
        .with("elapsed_time", f64::from(elapsed.max(0.0)))
        .with("pseudo_element", String::new())
}

/// Start, update and cancel each element's animations to match what its
/// declarations say now.
///
/// An animation is matched to the one already running by name (from the end
/// of the list, for a name listed twice): a match keeps its place on its
/// timeline and takes the new longhands; a name that appears starts from the
/// beginning; one that goes away is cancelled. That is the CSS rule a
/// `:hover { animation: ... }` restarts by.
#[allow(clippy::type_complexity)]
pub fn reconcile_animations(
    mut commands: Commands,
    tick: Res<Tick>,
    pointer: Option<Res<PointerState>>,
    shown: Shown,
    mut events: MessageWriter<CustomEvent>,
    anim: Res<AnimationsActive>,
    mut q: Query<
        (
            Entity,
            Option<&AnimationDeclarations>,
            Option<&StateAnimations>,
            Option<&mut RunningAnimations>,
            Markers,
        ),
        Or<(
            With<AnimationDeclarations>,
            With<StateAnimations>,
            With<RunningAnimations>,
        )>,
    >,
) {
    let now = tick.now;
    let primary_down = pointer.is_some_and(|p| p.primary_down);
    for (
        entity,
        base,
        states,
        running,
        (hovered, focused, focus_visible, pressed, drag_over, disabled),
    ) in &mut q
    {
        let markers = StateMarkers {
            hovered,
            focused: focused.is_some(),
            focus_visible: focus_visible.is_some(),
            pressed,
            primary_down,
            drag_over,
            disabled,
        };
        let wanted = if shown.shows(entity) {
            declared(base, states, markers)
        } else {
            Vec::new()
        };
        let Some(mut running) = running else {
            if !wanted.is_empty() {
                let list = wanted.into_iter().map(|a| Running::new(a, now)).collect();
                commands.entity(entity).try_insert(RunningAnimations(list));
            }
            continue;
        };
        let same = running.0.len() == wanted.len()
            && running.0.iter().zip(&wanted).all(|(r, w)| r.spec == *w);
        if same {
            continue;
        }
        let mut old: Vec<Option<Running>> = std::mem::take(&mut running.0)
            .into_iter()
            .map(Some)
            .collect();
        let mut next: Vec<Running> = Vec::with_capacity(wanted.len());
        for spec in wanted.into_iter().rev() {
            let found = old
                .iter_mut()
                .rev()
                .find(|r| r.as_ref().is_some_and(|r| r.spec.name == spec.name))
                .and_then(Option::take);
            next.push(match found {
                Some(mut r) => {
                    r.update(spec, now);
                    r
                }
                None => Running::new(spec, now),
            });
        }
        next.reverse();
        for gone in old.into_iter().flatten() {
            // An animation that had started and had not ended is cancelled.
            if matches!(gone.reported, Some((Phase::Before | Phase::Active, _))) {
                let at = (gone.local(now) - gone.timing().delay).max(0.0);
                events.write(event(entity, "animationcancel", &gone, at));
                anim.request();
            }
        }
        running.0 = next;
    }
}

/// What [`OnScreen`] reads of each element on the way up the tree.
type ScreenNode = (
    Option<&'static Transform>,
    Option<&'static ScrollOffset>,
    Option<&'static lumen_core::input::Scroll>,
    Has<lumen_core::transform::ElementTransform>,
    Option<&'static ChildOf>,
);

/// Whether an element is somewhere a person can see it: inside the primary
/// window and inside every scroll container around it. An animation on an
/// element that is not keeps its timeline but asks for no frames.
#[derive(SystemParam)]
pub struct OnScreen<'w, 's> {
    viewport: Query<'w, 's, &'static WindowViewport, With<PrimaryWindow>>,
    nodes: Query<'w, 's, ScreenNode>,
}

impl OnScreen<'_, '_> {
    fn on_screen(&self, e: Entity) -> bool {
        let Ok((Some(t), _, _, moved, parent)) = self.nodes.get(e) else {
            return true;
        };
        if moved {
            return true;
        }
        // The ancestors, nearest first, with what each contributes: its own
        // scroll offset, and its box when it clips (a scroll container).
        let mut chain: Vec<(Vec2, Option<(Vec2, Vec2)>)> = Vec::new();
        let mut up = parent.map(ChildOf::parent);
        while let Some(a) = up {
            let Ok((ta, offset, scroller, moved, parent)) = self.nodes.get(a) else {
                break;
            };
            if moved {
                // A transform above moves everything below where the boxes
                // do not say; count it as showing.
                return true;
            }
            let clip = ta
                .filter(|_| scroller.is_some())
                .map(|ta| (ta.absolute, ta.size));
            chain.push((offset.map_or(Vec2::ZERO, |o| o.0), clip));
            up = parent.map(ChildOf::parent);
        }
        // A box shows moved up by the scroll of every ancestor above it.
        let mut above = Vec2::ZERO;
        let mut clips: Vec<(Vec2, Vec2)> = Vec::new();
        for (offset, clip) in chain.iter().rev() {
            if let Some((o, s)) = clip {
                clips.push((*o - above, *s));
            }
            above += *offset;
        }
        let origin = t.absolute - above;
        let overlaps = |(o, s): (Vec2, Vec2)| {
            origin.x < o.x + s.x
                && origin.y < o.y + s.y
                && origin.x + t.size.x > o.x
                && origin.y + t.size.y > o.y
        };
        if let Ok(vp) = self.viewport.single()
            && !overlaps((Vec2::ZERO, vp.size))
        {
            return false;
        }
        clips.into_iter().all(overlaps)
    }
}

/// What an animation needs next to stay in step.
#[derive(Clone, Copy, Debug, PartialEq)]
enum Next {
    /// A frame on every vsync: its values move continuously.
    Frame,
    /// A tick at this instant: a delay ends, a step jumps, an iteration
    /// turns.
    At(Instant),
    /// Nothing: paused, or ended.
    Never,
}

impl Next {
    fn earliest(self, other: Next) -> Next {
        match (self, other) {
            (Next::Frame, _) | (_, Next::Frame) => Next::Frame,
            (Next::At(a), Next::At(b)) => Next::At(a.min(b)),
            (Next::At(a), Next::Never) | (Next::Never, Next::At(a)) => Next::At(a),
            (Next::Never, Next::Never) => Next::Never,
        }
    }
}

/// The frames of `kf` that set `field`, with the element's own value
/// standing in at `0` and `1` where no frame sets it there, as CSS fills in
/// a missing `from` or `to`.
fn frames_for(kf: &Keyframes, field: StyleFields) -> Vec<&Frame> {
    kf.frames
        .iter()
        .filter(|f| f.patch.fields().intersects(field))
        .collect()
}

/// The values `kf` shows at directed progress `p` for the fields in
/// `fields`, over `under`, and when that output next changes if it only
/// changes in steps.
#[allow(clippy::too_many_arguments)]
fn sample_keyframes(
    kf: &Keyframes,
    fields: StyleFields,
    p: f32,
    reversed: bool,
    easing: &Easing,
    under: &StyleValues,
    size: Vec2,
) -> (StyleValues, Option<Vec<f32>>) {
    let mut out = StyleValues::default();
    // Directed progress at which a stepped segment next jumps, for each
    // field; `None` once one field moves continuously.
    let mut jumps: Option<Vec<f32>> = Some(Vec::new());
    for field in fields.iter() {
        let declared = frames_for(kf, field);
        let edge = |offset: f32| Frame {
            offset,
            patch: Default::default(),
            timing: None,
        };
        let start = edge(0.0);
        let end = edge(1.0);
        let mut frames: Vec<&Frame> = Vec::with_capacity(declared.len() + 2);
        if declared.first().is_none_or(|f| f.offset > 0.0) {
            frames.push(&start);
        }
        frames.extend(declared);
        if frames.last().is_none_or(|f| f.offset < 1.0) {
            frames.push(&end);
        }
        let i = frames
            .windows(2)
            .position(|w| p <= w[1].offset)
            .unwrap_or(frames.len() - 2);
        let (a, b) = (frames[i], frames[i + 1]);
        let span = b.offset - a.offset;
        let local = if span > 0.0 {
            ((p - a.offset) / span).clamp(0.0, 1.0)
        } else {
            1.0
        };
        let curve = a.timing.as_ref().unwrap_or(easing);
        let t = curve.apply(local);
        let from = a.patch.values(field, under);
        let to = b.patch.values(field, under);
        out.overlay(&from.interpolate(&to, t, size));
        // When does this field's output change next?
        match (curve, jumps.as_mut()) {
            (Easing::Steps(n, _), Some(list)) => {
                let n = (*n).max(1);
                let k = (local * n as f32).floor();
                let edge = if reversed {
                    // Running backwards through the segment: the jump below.
                    let at = if (local * n as f32).fract() == 0.0 {
                        k - 1.0
                    } else {
                        k
                    };
                    a.offset + (at.max(0.0) / n as f32) * span
                } else {
                    a.offset + ((k + 1.0).min(n as f32) / n as f32) * span
                };
                list.push(edge);
            }
            _ => jumps = None,
        }
    }
    (out, jumps)
}

/// Sample every running animation, hold the result, raise the events, and
/// ask for what comes next: a frame while one moves on screen, a timed wake
/// for a delay, a step or an animation moving off screen, nothing when
/// paused or done.
#[allow(clippy::type_complexity, clippy::too_many_arguments)]
pub fn sample_animations(
    mut commands: Commands,
    tick: Res<Tick>,
    anim: Res<AnimationsActive>,
    wake: Res<WakeDeadline>,
    set: Res<KeyframeSet>,
    screen: OnScreen,
    mut events: MessageWriter<CustomEvent>,
    mut q: Query<(
        Entity,
        &mut RunningAnimations,
        &mut StyleOverrides,
        Option<&Transform>,
        Option<&ImportantFields>,
        Option<&lumen_primitives::Interaction>,
        MotionComponents,
    )>,
) {
    let now = tick.now;
    for (entity, mut running, mut overrides, layout, important, interaction, mut written) in &mut q
    {
        if running.0.is_empty() {
            let out = overrides.release::<AnimationHolder>();
            write_motion_values(&mut commands, entity, &mut written, &out);
            commands.entity(entity).try_remove::<RunningAnimations>();
            continue;
        }
        let size = layout.map_or(Vec2::ZERO, |t| t.size);
        let important = important.map_or(StyleFields::empty(), |i| i.0);
        let all: StyleFields = running
            .0
            .iter()
            .filter_map(|r| set.get(&r.spec.name))
            .fold(StyleFields::empty(), |f, k| f | k.fields)
            - important;
        // What shows under the animations, and what each animation lays its
        // missing frames over: what the ones before it in the list left.
        let mut under = overrides.below::<AnimationHolder>(all, &live_of(&written));
        if all.contains(StyleFields::OUTLINE_COLOR) && under.outline_color == Some(None) {
            under.outline_color = Some(
                interaction
                    .and_then(|ix| ix.focus_visible_outline.or(ix.focus_outline))
                    .map(|o| o.color),
            );
        }
        let mut held = StyleValues::default();
        let mut next = Next::Never;
        let mut raised = false;
        for r in running.0.iter_mut() {
            let Some(kf) = set.get(&r.spec.name).map(Arc::clone) else {
                // A name no block answers to runs nothing and raises
                // nothing, as in a browser.
                continue;
            };
            let timing = r.timing();
            let local = r.local(now);
            let s = timing.sample(local);
            // Events, for the phase and iteration this tick reached.
            let was = r.reported;
            let reached = (s.phase, s.iteration);
            if was != Some(reached) {
                let started = matches!(was, Some((Phase::Active | Phase::After, _)));
                match s.phase {
                    Phase::Before => {}
                    Phase::Active => {
                        if !started {
                            let at = (-timing.delay).clamp(0.0, timing.active_duration());
                            events.write(event(entity, "animationstart", r, at));
                            raised = true;
                        } else if was.is_some_and(|(_, i)| i != s.iteration) {
                            let at = s.iteration as f32 * timing.duration;
                            events.write(event(entity, "animationiteration", r, at));
                            raised = true;
                        }
                    }
                    Phase::After => {
                        if !started {
                            let at = (-timing.delay).clamp(0.0, timing.active_duration());
                            events.write(event(entity, "animationstart", r, at));
                        }
                        if !matches!(was, Some((Phase::After, _))) {
                            events.write(event(
                                entity,
                                "animationend",
                                r,
                                timing.active_duration(),
                            ));
                            raised = true;
                        }
                    }
                }
                r.reported = Some(reached);
            }
            // What comes next on its own timeline.
            let at = |secs: f32| r.start + Duration::from_secs_f32(secs.max(0.0));
            let own = if r.paused_at.is_some() {
                Next::Never
            } else {
                match s.phase {
                    Phase::Before => Next::At(at(timing.delay)),
                    Phase::After => Next::Never,
                    Phase::Active => Next::Frame,
                }
            };
            let fields = kf.fields - important;
            let Some(p) = s.progress else {
                next = next.earliest(own);
                continue;
            };
            let (values, jumps) =
                sample_keyframes(&kf, fields, p, s.reversed, &r.easing, &under, size);
            held.overlay(&values);
            under.overlay(&values);
            let own = match (own, jumps) {
                (Next::Frame, Some(jumps)) if timing.duration > 0.0 => {
                    // Only steps: wake at the nearest jump, or at the end of
                    // the iteration if that comes first.
                    let iter_start = timing.iteration_start(s.iteration);
                    let raw = |directed: f32| if s.reversed { 1.0 - directed } else { directed };
                    let soonest = jumps
                        .into_iter()
                        .map(raw)
                        .chain([1.0])
                        .filter(|x| *x * timing.duration > local - iter_start)
                        .fold(1.0f32, f32::min);
                    Next::At(at(iter_start + soonest * timing.duration) + Duration::from_millis(1))
                }
                (own, _) => own,
            };
            next = next.earliest(own);
        }
        let held = held.restricted(all);
        let out = if held.fields().is_empty() {
            overrides.release::<AnimationHolder>()
        } else {
            overrides.hold::<AnimationHolder>(held, &live_of(&written))
        };
        write_motion_values(&mut commands, entity, &mut written, &out);
        if raised {
            // The events are delivered on the next tick; make sure there is
            // one.
            anim.request();
        }
        match next {
            Next::Frame if screen.on_screen(entity) => anim.request(),
            Next::Frame => {
                // Off screen: no frames, but wake for the next event so a
                // handler hears it on time.
                if let Some(when) = next_event_time(&running.0, now) {
                    wake.request(when);
                }
            }
            Next::At(when) => wake.request(when),
            Next::Never => {}
        }
    }
}

/// The soonest instant one of `running` turns an iteration or ends.
fn next_event_time(running: &[Running], now: Instant) -> Option<Instant> {
    running
        .iter()
        .filter(|r| r.paused_at.is_none())
        .filter_map(|r| {
            let timing = r.timing();
            let local = r.local(now);
            let s = timing.sample(local);
            if s.phase != Phase::Active || timing.duration <= 0.0 {
                return None;
            }
            let turn = timing
                .iteration_start(s.iteration + 1)
                .min(timing.end_time());
            Some(r.start + Duration::from_secs_f32(turn.max(0.0)))
        })
        .min()
}

/// Rebuild the keyframe set when the stylesheet or the media it is read
/// against changes.
pub fn sync_keyframes(
    mut set: ResMut<KeyframeSet>,
    sheet: Option<Res<lumen_scene::style_sheet::RuntimeStylesheet>>,
    media: Option<Res<lumen_scene::style_sheet::LastMediaContext>>,
) {
    let Some(sheet) = sheet else {
        return;
    };
    if !sheet.is_changed() && !media.as_ref().is_some_and(|m| m.is_changed()) && !set.is_added() {
        return;
    }
    let m = media.map(|m| m.0);
    set.rebuild(&sheet.0.at_rules, m, || sheet.0.root_vars());
}
