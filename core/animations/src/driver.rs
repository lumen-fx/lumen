//! Running an element's CSS animations: starting and stopping them as its
//! declarations change, sampling them each tick, raising their events, and
//! asking for frames only while one moves on screen.

use std::sync::Arc;

use bevy_ecs::hierarchy::{ChildOf, Children};
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
    AnimationDeclsIr, AnimationIr, AnimationPlayStateIr, AnimationTimelineIr, ScrollAxisIr,
    ScrollerIr,
};
use lumen_primitives::state_style::{StateMarkers, active_states};
use lumen_primitives::transition::{MotionComponents, live_of, write_motion_values};
use lumen_scene::style_sheet::{
    AnimationDeclarations, HoldRemoval, ImportantFields, Leaving, StateAnimations,
};
use lumen_script::CustomEvent;

use crate::keyframes::{Frame, KeyframeSet, Keyframes};
use crate::script::{Control, Request, ScriptAnimations, Snapshot};
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
    /// What a script's own animation (`animations::animate`) adds; `None`
    /// for one a stylesheet declares.
    pub script: Option<ScriptState>,
}

/// The parts of a script's animation that a declared one does not have.
#[derive(Clone, Debug)]
pub struct ScriptState {
    /// The handle the script holds.
    pub id: u64,
    /// The keyframes it was given.
    pub keyframes: Arc<Keyframes>,
    /// How fast its timeline runs; negative runs it backwards.
    pub rate: f32,
    /// Where on its timeline it was at [`Running::start`].
    pub origin: f32,
    /// Its end has been reported.
    pub finished: bool,
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
            script: None,
        }
    }

    /// Re-anchor its timeline at `now`, keeping its place: what a change of
    /// rate or a seek starts from.
    fn anchor(&mut self, now: Instant, at: f32) {
        if let Some(s) = self.script.as_mut() {
            s.origin = at;
        }
        self.start = now;
        if self.paused_at.is_some() {
            self.paused_at = Some(at);
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
        self.paused_at.unwrap_or_else(|| {
            let elapsed = now.saturating_duration_since(self.start).as_secs_f32();
            match &self.script {
                Some(s) => s.origin + elapsed * s.rate,
                None => elapsed,
            }
        })
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
    let mut decls: AnimationDeclsIr = base.map(|b| (*b.0).clone()).unwrap_or_default();
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
            Has<Leaving>,
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
        leaving,
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
                let list: Vec<Running> = wanted.into_iter().map(|a| Running::new(a, now)).collect();
                // An exit that starts as the element starts to leave holds
                // it from this tick on; the sampler lets go when it ends.
                let exit = leaving
                    && list
                        .iter()
                        .any(|r| r.timing().end_time().is_finite() && r.paused_at.is_none());
                if exit {
                    commands
                        .entity(entity)
                        .try_insert((RunningAnimations(list), HoldRemoval));
                } else {
                    commands.entity(entity).try_insert(RunningAnimations(list));
                }
            }
            continue;
        };
        // A script's own animations are not the stylesheet's to start or
        // stop: they stay, after the declared ones, as Web Animations
        // stacks them.
        let declared_now: Vec<&Running> = running.0.iter().filter(|r| r.script.is_none()).collect();
        let same = declared_now.len() == wanted.len()
            && declared_now.iter().zip(&wanted).all(|(r, w)| r.spec == *w);
        if same {
            continue;
        }
        let (scripted, declared_before): (Vec<Running>, Vec<Running>) =
            std::mem::take(&mut running.0)
                .into_iter()
                .partition(|r| r.script.is_some());
        let mut old: Vec<Option<Running>> = declared_before.into_iter().map(Some).collect();
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
        next.extend(scripted);
        running.0 = next;
    }
}

/// What [`Timelines`] reads of each element on the way up the tree.
type TimelineNode = (
    Option<&'static Transform>,
    Option<&'static ScrollOffset>,
    Has<lumen_core::input::Scroll>,
    Option<&'static ChildOf>,
    Option<&'static Children>,
);

/// Where a scroll-driven animation is on its timeline: the scroll position
/// of a container (`scroll()`), or an element's way through its
/// container's view (`view()`).
#[derive(SystemParam)]
pub struct Timelines<'w, 's> {
    nodes: Query<'w, 's, TimelineNode>,
}

impl Timelines<'_, '_> {
    /// The scroll container `scroller` names for `e`.
    fn scroller(&self, e: Entity, scroller: ScrollerIr) -> Option<Entity> {
        let is_scroller = |x: Entity| self.nodes.get(x).is_ok_and(|n| n.2);
        let parent = |x: Entity| {
            self.nodes
                .get(x)
                .ok()
                .and_then(|n| n.3.map(ChildOf::parent))
        };
        match scroller {
            ScrollerIr::SelfElement => is_scroller(e).then_some(e),
            ScrollerIr::Nearest => {
                let mut up = parent(e);
                while let Some(a) = up {
                    if is_scroller(a) {
                        return Some(a);
                    }
                    up = parent(a);
                }
                None
            }
            ScrollerIr::Root => {
                let mut found = None;
                let mut up = Some(e);
                while let Some(a) = up {
                    if is_scroller(a) {
                        found = Some(a);
                    }
                    up = parent(a);
                }
                found
            }
        }
    }

    /// How far `scroller` can scroll along each axis: its content's extent
    /// past its own box.
    fn range(&self, scroller: Entity) -> Option<Vec2> {
        let (Some(own), _, _, _, Some(kids)) = self.nodes.get(scroller).ok()? else {
            return None;
        };
        let mut extent = Vec2::ZERO;
        for k in kids.iter() {
            if let Ok((Some(t), ..)) = self.nodes.get(k) {
                extent = extent.max(t.absolute - own.absolute + t.size);
            }
        }
        Some((extent - own.size).max(Vec2::ZERO))
    }

    /// Progress along `timeline` for `e`, `0` at its start and `1` at its
    /// end; outside that range while a `view()` subject is out of view.
    /// `None` while the timeline is inactive: no container, or one that
    /// cannot scroll.
    fn progress(&self, e: Entity, timeline: AnimationTimelineIr) -> Option<f32> {
        let pick = |v: Vec2, axis: ScrollAxisIr| match axis {
            ScrollAxisIr::Block | ScrollAxisIr::Y => v.y,
            ScrollAxisIr::Inline | ScrollAxisIr::X => v.x,
        };
        match timeline {
            AnimationTimelineIr::Scroll(scroller, axis) => {
                let s = self.scroller(e, scroller)?;
                let range = pick(self.range(s)?, axis);
                if range <= 0.0 {
                    return None;
                }
                let offset = self.nodes.get(s).ok()?.1.map_or(Vec2::ZERO, |o| o.0);
                Some((pick(offset, axis) / range).clamp(0.0, 1.0))
            }
            AnimationTimelineIr::View(axis) => {
                let s = self.scroller(e, ScrollerIr::Nearest)?;
                let (Some(port), offset, ..) = self.nodes.get(s).ok()? else {
                    return None;
                };
                let (Some(subject), ..) = self.nodes.get(e).ok()? else {
                    return None;
                };
                let offset = offset.map_or(Vec2::ZERO, |o| o.0);
                let at = pick(subject.absolute - port.absolute - offset, axis);
                let (view, size) = (pick(port.size, axis), pick(subject.size, axis));
                let span = view + size;
                (span > 0.0).then(|| (view - at) / span)
            }
            AnimationTimelineIr::Auto | AnimationTimelineIr::None => None,
        }
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
    (screen, timelines, bus): (OnScreen, Timelines, Option<Res<ScriptAnimations>>),
    mut events: MessageWriter<CustomEvent>,
    mut q: Query<(
        Entity,
        &mut RunningAnimations,
        &mut StyleOverrides,
        Option<&Transform>,
        Option<&ImportantFields>,
        Option<&lumen_primitives::Interaction>,
        (Option<&Leaving>, Has<HoldRemoval>),
        MotionComponents,
    )>,
) {
    let now = tick.now;
    for (
        entity,
        mut running,
        mut overrides,
        layout,
        important,
        interaction,
        (leaving, held_in_tree),
        mut written,
    ) in &mut q
    {
        // An element leaving the tree stays while an animation its leaving
        // started still runs: its exit.
        let exiting = leaving.is_some_and(|l| {
            running.0.iter().any(|r| {
                let timing = r.timing();
                r.start >= l.at
                    && timing.end_time().is_finite()
                    && r.paused_at.is_none()
                    && timing.sample(r.local(now)).phase != Phase::After
            })
        });
        match (exiting, held_in_tree) {
            (true, false) => {
                commands.entity(entity).try_insert(HoldRemoval);
            }
            (false, true) => {
                commands.entity(entity).try_remove::<HoldRemoval>();
                // The tick that removes it.
                anim.request();
            }
            _ => {}
        }
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
            .filter_map(|r| match &r.script {
                Some(s) => Some(&s.keyframes),
                None => set.get(&r.spec.name),
            })
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
            let kf = match &r.script {
                Some(s) => Arc::clone(&s.keyframes),
                None => match set.get(&r.spec.name) {
                    Some(k) => Arc::clone(k),
                    // A name no block answers to runs nothing and raises
                    // nothing, as in a browser.
                    None => continue,
                },
            };
            // A scroll-driven animation's place is where the scroll is: its
            // whole active interval spans the timeline, and it asks for no
            // frames of its own, since only a scroll moves it.
            let scroll_driven = !matches!(r.spec.timeline, AnimationTimelineIr::Auto);
            let (timing, local) = if scroll_driven {
                let Some(p) = timelines.progress(entity, r.spec.timeline) else {
                    continue;
                };
                let mut timing = r.timing();
                timing.delay = 0.0;
                if timing.duration <= 0.0 {
                    timing.duration = 1.0;
                }
                let local = p * timing.active_duration();
                (timing, local)
            } else {
                (r.timing(), r.local(now))
            };
            let s = timing.sample(local);
            // A script's animation reports its end, and what the script can
            // read back about it.
            if let Some(state) = r.script.as_mut() {
                let ended = if state.rate < 0.0 {
                    local <= 0.0
                } else {
                    s.phase == Phase::After
                };
                if ended && !state.finished && r.paused_at.is_none() {
                    state.finished = true;
                    events.write(
                        CustomEvent::new(entity, "animationend")
                            .with("animation_name", String::new())
                            .with("animation", state.id as i64)
                            .with("elapsed_time", f64::from(local.max(0.0)))
                            .with("pseudo_element", String::new()),
                    );
                    raised = true;
                }
                if let Some(bus) = bus.as_ref() {
                    bus.0.record(
                        state.id,
                        Snapshot {
                            current_time_ms: f64::from(local) * 1000.0,
                            playback_rate: f64::from(state.rate),
                            play_state: if r.paused_at.is_some() {
                                "paused"
                            } else if state.finished {
                                "finished"
                            } else {
                                "running"
                            },
                        },
                    );
                }
            }
            // Events, for the phase and iteration this tick reached.
            let was = r.reported;
            let reached = (s.phase, s.iteration);
            if r.script.is_none() && was != Some(reached) {
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
            let finished = r.script.as_ref().is_some_and(|st| st.finished);
            let own = if r.paused_at.is_some() || scroll_driven || finished {
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

/// The keyframes a script handed `animations::animate`, read the way a
/// `@keyframes` body is: each declaration through the cascade's own parser,
/// missing offsets spread evenly as Web Animations spreads them.
pub fn script_keyframes(frames: &[crate::script::ScriptFrame]) -> Keyframes {
    let n = frames.len();
    let mut offsets: Vec<Option<f32>> = frames.iter().map(|f| f.offset).collect();
    if n == 1 {
        offsets[0] = offsets[0].or(Some(1.0));
    } else {
        offsets[0] = offsets[0].or(Some(0.0));
        offsets[n - 1] = offsets[n - 1].or(Some(1.0));
    }
    let mut i = 0;
    while i < n {
        if offsets[i].is_some() {
            i += 1;
            continue;
        }
        let a = i - 1;
        let mut b = i;
        while offsets[b].is_none() {
            b += 1;
        }
        let (from, to) = (offsets[a].unwrap_or(0.0), offsets[b].unwrap_or(1.0));
        for (k, slot) in offsets.iter_mut().enumerate().take(b).skip(i) {
            *slot = Some(from + (to - from) * (k - a) as f32 / (b - a) as f32);
        }
        i = b;
    }
    let mut out: Vec<Frame> = Vec::with_capacity(n);
    for (frame, offset) in frames.iter().zip(offsets) {
        let mut patch = crate::keyframes::FramePatch::default();
        for (name, value) in &frame.declarations {
            if let Some(p) = crate::keyframes::declaration_patch(&css_name(name), value) {
                patch.merge(&p);
            }
        }
        out.push(Frame {
            offset: offset.unwrap_or(1.0).clamp(0.0, 1.0),
            patch,
            timing: frame
                .easing
                .as_deref()
                .and_then(|e| lumen_ir::css::parse_easing("animate", "easing", e).ok())
                .map(|e| Easing::from(&e)),
        });
    }
    out.sort_by(|a, b| a.offset.total_cmp(&b.offset));
    let fields = out
        .iter()
        .fold(StyleFields::empty(), |f, fr| f | fr.patch.fields());
    Keyframes {
        frames: out,
        fields,
    }
}

/// A keyframe key as CSS spells it: `backgroundColor`, the way a browser
/// script writes it, is `background-color`.
fn css_name(key: &str) -> String {
    let mut out = String::with_capacity(key.len() + 4);
    for c in key.chars() {
        if c.is_ascii_uppercase() {
            out.push('-');
            out.push(c.to_ascii_lowercase());
        } else {
            out.push(c);
        }
    }
    out
}

/// The declared form of a script's timing, so the timing model reads both
/// kinds of animation alike.
fn script_spec(t: &crate::script::ScriptTiming) -> AnimationIr {
    use lumen_ir::animation::{AnimationCompositionIr, AnimationDirectionIr, AnimationFillModeIr};
    let timing = lumen_ir::css::parse_easing("animate", "easing", &t.easing)
        .unwrap_or(lumen_ir::layout_ir::EasingIr::Linear);
    // A spring with no duration runs for as long as it takes to settle.
    let duration_ms = match timing.settle_ms() {
        Some(ms) if t.duration_ms <= 0.0 => ms,
        _ => t.duration_ms.round().clamp(0.0, u32::MAX as f64) as u32,
    };
    AnimationIr {
        name: String::new(),
        duration_ms,
        timing,
        delay_ms: t.delay_ms.round() as i32,
        iteration_count: t.iterations as f32,
        direction: match t.direction.as_str() {
            "reverse" => AnimationDirectionIr::Reverse,
            "alternate" => AnimationDirectionIr::Alternate,
            "alternate-reverse" => AnimationDirectionIr::AlternateReverse,
            _ => AnimationDirectionIr::Normal,
        },
        fill_mode: match t.fill.as_str() {
            "forwards" => AnimationFillModeIr::Forwards,
            "backwards" => AnimationFillModeIr::Backwards,
            "both" => AnimationFillModeIr::Both,
            _ => AnimationFillModeIr::None,
        },
        play_state: AnimationPlayStateIr::Running,
        composition: AnimationCompositionIr::Replace,
        timeline: AnimationTimelineIr::Auto,
    }
}

/// Apply what scripts asked of their animations since the last tick:
/// start the new ones on their elements, and play, pause, seek, reverse,
/// finish or cancel the ones they hold.
pub fn apply_script_requests(
    mut commands: Commands,
    (tick, anim): (Res<Tick>, Res<AnimationsActive>),
    bus: Option<Res<ScriptAnimations>>,
    entities: &bevy_ecs::entity::Entities,
    mut events: MessageWriter<CustomEvent>,
    mut owners: Local<std::collections::HashMap<u64, Entity>>,
    mut running: Query<&mut RunningAnimations>,
) {
    let Some(bus) = bus else {
        return;
    };
    let requests = bus.0.take();
    if requests.is_empty() {
        return;
    }
    let now = tick.now;
    // New animations for an element this tick, gathered so several land in
    // one insert.
    let mut fresh: std::collections::HashMap<Entity, Vec<Running>> = Default::default();
    for request in requests {
        match request {
            Request::Animate {
                id,
                node,
                frames,
                timing,
            } => {
                let Some(entity) =
                    lumen_core::node::NodeHandle::unpack(node).and_then(|h| h.validate(entities))
                else {
                    continue;
                };
                let spec = script_spec(&timing);
                let mut r = Running::new(spec, now);
                r.script = Some(ScriptState {
                    id,
                    keyframes: Arc::new(script_keyframes(&frames)),
                    rate: 1.0,
                    origin: 0.0,
                    finished: false,
                });
                owners.insert(id, entity);
                match running.get_mut(entity) {
                    Ok(mut list) => list.0.push(r),
                    Err(_) => fresh.entry(entity).or_default().push(r),
                }
                anim.request();
            }
            Request::Control { id, control } => {
                let Some(&entity) = owners.get(&id) else {
                    continue;
                };
                // One started this tick is still in `fresh`, so a script
                // may start an animation and control it in one go.
                let mut in_world = running.get_mut(entity).ok();
                let list: &mut Vec<Running> = match in_world.as_mut() {
                    Some(list) => &mut list.0,
                    None => match fresh.get_mut(&entity) {
                        Some(list) => list,
                        None => continue,
                    },
                };
                let Some(at) = list
                    .iter()
                    .position(|r| r.script.as_ref().is_some_and(|s| s.id == id))
                else {
                    continue;
                };
                if control == Control::Cancel {
                    let r = list.remove(at);
                    owners.remove(&id);
                    bus.0.record(
                        id,
                        Snapshot {
                            current_time_ms: 0.0,
                            playback_rate: f64::from(r.script.as_ref().map_or(1.0, |s| s.rate)),
                            play_state: "idle",
                        },
                    );
                    events.write(
                        CustomEvent::new(entity, "animationcancel")
                            .with("animation_name", String::new())
                            .with("animation", id as i64)
                            .with("elapsed_time", f64::from(r.local(now).max(0.0)))
                            .with("pseudo_element", String::new()),
                    );
                    anim.request();
                    continue;
                }
                let r = &mut list[at];
                let end = r.timing().end_time();
                let local = r.local(now);
                let rate = r.script.as_ref().map_or(1.0, |s| s.rate);
                match control {
                    Control::Pause => r.paused_at = Some(local),
                    Control::Play => {
                        let finished = r.script.as_ref().is_some_and(|s| s.finished);
                        let from = match (finished, rate < 0.0) {
                            (true, false) => 0.0,
                            (true, true) => end,
                            (false, _) => local,
                        };
                        r.paused_at = None;
                        r.anchor(now, from);
                        if let Some(s) = r.script.as_mut() {
                            s.finished = false;
                        }
                    }
                    Control::Reverse => {
                        let from = local.clamp(0.0, end.min(f32::MAX));
                        r.anchor(now, from);
                        if let Some(s) = r.script.as_mut() {
                            s.rate = -s.rate;
                            s.finished = false;
                        }
                    }
                    Control::Finish => {
                        let to = if rate < 0.0 { 0.0 } else { end };
                        if to.is_finite() {
                            r.paused_at = None;
                            r.anchor(now, to);
                        }
                    }
                    Control::Seek(ms) => {
                        r.anchor(now, (ms / 1000.0) as f32);
                        if let Some(s) = r.script.as_mut() {
                            s.finished = false;
                        }
                    }
                    Control::Rate(new_rate) => {
                        r.anchor(now, local);
                        if let Some(s) = r.script.as_mut() {
                            s.rate = new_rate as f32;
                        }
                    }
                    Control::Cancel => {}
                }
                anim.request();
            }
        }
    }
    for (entity, list) in fresh {
        commands.entity(entity).try_insert(RunningAnimations(list));
    }
}
