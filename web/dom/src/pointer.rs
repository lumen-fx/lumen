//! The pointer, as the browser reports it.
//!
//! A window backend turns the OS pointer into [`PointerPressed`],
//! [`PointerReleased`], [`PointerMoved`], [`MouseWheel`] and [`PointerLeft`],
//! and the hit test marks the entity under it [`Hovered`]. The script event
//! driver reads the messages and aims each one at the hovered entity, which
//! is how a handler bound with `on("pointerdown", ...)` fires. A page has no
//! hit test to run: the browser already knows which element the pointer is
//! over and says so on every event. This module writes the same messages and
//! the same marker from that, so the driver runs unchanged. Pointer Events
//! cover the mouse, a pen and touch alike, so a tap reaches a script as the
//! same `pointerdown` and `pointerup` a click does.
//!
//! What the desktop hit test decides, this decides the same way:
//!
//! - Nothing inside a disabled subtree is hovered. The pointer falls through
//!   to the enabled element around it.
//! - A primary press captures the pointer. Until the release, the hover
//!   marker sits on the pressed entity while the pointer is over it and on
//!   nothing while it is dragged off, so a drag across other elements gives
//!   them no `pointerenter` and no events.
//!
//! The listeners sit on the document, like the keys: a pointer that leaves
//! the app's root is still a pointer the app has to see leave. None of them
//! calls `preventDefault`. The wheel and move listeners are passive, so the
//! page scrolls exactly as it would without Lumen; a script's `wheel` handler
//! observes the scroll and cannot cancel it, which matches the desktop, where
//! only `click` and `submit` have a default action a handler can prevent.
//!
//! A pointer that moves several times between two ticks is one move: only
//! the last position is queued, so a fast mouse costs one message per frame.

use std::cell::RefCell;

use bevy_ecs::hierarchy::ChildOf;
use bevy_ecs::message::MessageWriter;
use bevy_ecs::prelude::*;
use glam::Vec2;
use lumen_core::components::Disabled;
use lumen_core::input::{
    Hovered, MouseWheel, PointerButton, PointerLeft, PointerMoved, PointerPressed, PointerReleased,
    PointerState,
};
use wasm_bindgen::JsCast;
use wasm_bindgen::prelude::*;
use web_sys::{AddEventListenerOptions, Document, Event, MouseEvent, WheelEvent};

use crate::events::{Hit, hit_of, origin_of};
use crate::nodes::NodeTable;

/// One thing the pointer did, waiting for the next tick.
#[derive(Clone, Debug, PartialEq)]
enum PointerInput {
    /// A button went down.
    Down {
        /// The element under the pointer, if it stands for one, and where its
        /// box was.
        hit: Option<Hit>,
        /// Client coordinates.
        position: Vec2,
        /// Which button.
        button: PointerButton,
    },
    /// A button came up.
    Up {
        /// The element under the pointer, if it stands for one, and where its
        /// box was.
        hit: Option<Hit>,
        /// Client coordinates.
        position: Vec2,
        /// Which button.
        button: PointerButton,
    },
    /// The pointer moved.
    Move {
        /// The element under the pointer, if it stands for one, and where its
        /// box was.
        hit: Option<Hit>,
        /// Client coordinates.
        position: Vec2,
    },
    /// The wheel turned, or a touchpad scrolled.
    Wheel {
        /// The element under the pointer, if it stands for one, and where its
        /// box was.
        hit: Option<Hit>,
        /// Client coordinates.
        position: Vec2,
        /// Scroll distance in pixels, positive down and right.
        delta: Vec2,
    },
    /// The pointer left the document, or the browser took the gesture over
    /// (a touch that became a scroll). Either way nothing is under it and no
    /// press is in flight.
    Left,
}

thread_local! {
    /// What the pointer did since the last tick. A thread local for the same
    /// reason as the event queue in `events`: the listeners outlive every
    /// borrow of the world.
    static QUEUE: RefCell<Vec<PointerInput>> = const { RefCell::new(Vec::new()) };
}

/// Queue `input`, folding a move into a move queued just before it.
///
/// Only the latest position of an unbroken run of moves matters to anything
/// that reads it, so a pointer that moves ten times between two frames is
/// queued once.
fn push(queue: &mut Vec<PointerInput>, input: PointerInput) {
    if matches!(input, PointerInput::Move { .. })
        && let Some(last @ PointerInput::Move { .. }) = queue.last_mut()
    {
        *last = input;
        return;
    }
    queue.push(input);
}

/// `MouseEvent.button` in Lumen's spelling.
fn button_of(code: i16) -> PointerButton {
    match code {
        0 => PointerButton::Primary,
        1 => PointerButton::Middle,
        2 => PointerButton::Secondary,
        other => PointerButton::Other(u16::try_from(other).unwrap_or(u16::MAX)),
    }
}

/// Pixels per line for a wheel that reports lines, the same factor the
/// window backend normalises to.
const PIXELS_PER_LINE: f64 = 32.0;

/// A wheel event's distance in pixels, whatever unit the browser reported it
/// in.
fn wheel_delta(event: &WheelEvent) -> Vec2 {
    let (x, y) = (event.delta_x(), event.delta_y());
    let (sx, sy) = match event.delta_mode() {
        WheelEvent::DOM_DELTA_LINE => (PIXELS_PER_LINE, PIXELS_PER_LINE),
        WheelEvent::DOM_DELTA_PAGE => {
            let window = web_sys::window();
            let size = |v: Option<Result<JsValue, JsValue>>| {
                v.and_then(Result::ok)
                    .and_then(|v| v.as_f64())
                    .unwrap_or(PIXELS_PER_LINE)
            };
            (
                size(window.as_ref().map(web_sys::Window::inner_width)),
                size(window.as_ref().map(web_sys::Window::inner_height)),
            )
        }
        _ => (1.0, 1.0),
    };
    Vec2::new((x * sx) as f32, (y * sy) as f32)
}

/// Client coordinates of a pointer or wheel event.
fn position_of(event: &MouseEvent) -> Vec2 {
    Vec2::new(event.client_x() as f32, event.client_y() as f32)
}

/// Attach one pointer listener to the document.
fn on(
    document: &Document,
    kind: &str,
    passive: bool,
    handler: impl FnMut(Event) + 'static,
) -> Result<(), JsValue> {
    let handler = Closure::wrap(Box::new(handler) as Box<dyn FnMut(Event)>);
    let options = AddEventListenerOptions::new();
    options.set_passive(passive);
    document.add_event_listener_with_callback_and_add_event_listener_options(
        kind,
        handler.as_ref().unchecked_ref(),
        &options,
    )?;
    // Handed to the browser, which owns the callback from here.
    handler.forget();
    Ok(())
}

/// Start listening on `document` for what the pointer does.
///
/// Called once per document, from the same guard the keys use: the queue is
/// the page's, so a second app binding again would queue every event twice.
pub(crate) fn listen(document: &Document) -> Result<(), JsValue> {
    let queue = |input: PointerInput| QUEUE.with_borrow_mut(|q| push(q, input));
    for (kind, down) in [("pointerdown", true), ("pointerup", false)] {
        on(document, kind, false, move |event: Event| {
            let Some(mouse) = event.dyn_ref::<MouseEvent>() else {
                return;
            };
            let hit = hit_of(&event);
            let position = position_of(mouse);
            let button = button_of(mouse.button());
            queue(if down {
                PointerInput::Down {
                    hit,
                    position,
                    button,
                }
            } else {
                PointerInput::Up {
                    hit,
                    position,
                    button,
                }
            });
        })?;
    }
    on(document, "pointermove", true, move |event: Event| {
        if let Some(mouse) = event.dyn_ref::<MouseEvent>() {
            queue(PointerInput::Move {
                hit: hit_of(&event),
                position: position_of(mouse),
            });
        }
    })?;
    on(document, "wheel", true, move |event: Event| {
        if let Some(wheel) = event.dyn_ref::<WheelEvent>() {
            queue(PointerInput::Wheel {
                hit: hit_of(&event),
                position: position_of(wheel),
                delta: wheel_delta(wheel),
            });
        }
    })?;
    // `pointerout` fires on every crossing from one element to the next, and
    // names the element entered as `relatedTarget`. It is `None` only when
    // no element is entered at all: the pointer left the window, or a touch
    // ended and its pointer went away.
    on(document, "pointerout", true, move |event: Event| {
        if event
            .dyn_ref::<MouseEvent>()
            .is_some_and(|m| m.related_target().is_none())
        {
            queue(PointerInput::Left);
        }
    })?;
    // The browser took the gesture over, most often a touch that turned into
    // a scroll. No `pointerup` follows.
    on(document, "pointercancel", true, move |_: Event| {
        queue(PointerInput::Left);
    })?;
    Ok(())
}

/// The entity the desktop hit test would mark hovered for a pointer over
/// `entity`: `entity` itself, unless it sits in a disabled subtree, in which
/// case the enabled element around that subtree.
fn hover_target(
    entity: Entity,
    parents: &Query<&ChildOf>,
    disabled: &Query<(), With<Disabled>>,
) -> Option<Entity> {
    let mut target = Some(entity);
    let mut current = entity;
    loop {
        let parent = parents.get(current).ok().map(ChildOf::parent);
        if disabled.contains(current) {
            target = parent;
        }
        match parent {
            Some(parent) => current = parent,
            None => return target,
        }
    }
}

/// What a batch of pointer input has done to the pointer so far.
#[derive(Default)]
pub struct PointerTracking {
    /// The entity a primary press is holding the pointer on, until its
    /// release.
    captured: Option<Entity>,
}

/// Turn what the pointer did into the messages and the hover marker the
/// app's own systems read.
///
/// The script event driver aims every pointer message of a tick at the one
/// entity hovered when it runs, so a tick carries messages for one hover
/// target only. Input that would need a different one (a touch lifting off
/// and then leaving, a press on one element straight after a move over
/// another) stays queued for the next tick, a frame later, rather than
/// reaching the wrong element.
#[allow(clippy::too_many_arguments)] // ECS system: each arg is a query/param
pub fn drain_pointer_events(
    table: NonSend<NodeTable>,
    mut commands: Commands,
    mut state: ResMut<PointerState>,
    mut writers: PointerWriters,
    hovered: Query<Entity, With<Hovered>>,
    parents: Query<&ChildOf>,
    disabled: Query<(), With<Disabled>>,
    mut tracking: Local<PointerTracking>,
) {
    let pending = QUEUE.with_borrow_mut(std::mem::take);
    if pending.is_empty() {
        return;
    }
    // The hover target the messages written so far were aimed at.
    let mut aimed: Option<Option<Entity>> = None;
    let mut hover = hovered.iter().next();
    let mut rest = pending.into_iter();
    while let Some(input) = rest.next() {
        let hit = match &input {
            PointerInput::Down { hit, .. }
            | PointerInput::Up { hit, .. }
            | PointerInput::Move { hit, .. }
            | PointerInput::Wheel { hit, .. } => hit.clone(),
            PointerInput::Left => None,
        };
        let landed = hit.as_ref().and_then(|hit| table.entity_at(&hit.path));
        let target = landed.and_then(|e| hover_target(e, &parents, &disabled));
        // Held by a press: the pressed entity or nothing.
        let target = match tracking.captured {
            Some(captured) => target.filter(|t| *t == captured),
            None => target,
        };
        if aimed.is_some_and(|a| a != target) {
            // Leave this one and everything after it for the next tick.
            let mut left: Vec<PointerInput> = std::iter::once(input).chain(rest).collect();
            QUEUE.with_borrow_mut(|q| {
                left.append(q);
                *q = left;
            });
            break;
        }
        hover = target;
        // Where the pointer is on the target's box. The listener measured the
        // box the event landed on; a disabled one hands the pointer to the
        // element around it, whose box is measured here instead.
        let local = |position: Vec2| match (&hit, target) {
            (Some(hit), Some(target)) if landed == Some(target) => Some(position - hit.origin),
            (_, Some(target)) => table.element(target).map(|el| position - origin_of(el)),
            (_, None) => None,
        };
        match input {
            PointerInput::Down {
                position, button, ..
            } => {
                aimed = Some(target);
                state.position = Some(position);
                if button == PointerButton::Primary {
                    state.primary_down = true;
                    tracking.captured = target;
                }
                writers.presses.write(PointerPressed {
                    position,
                    button,
                    local: local(position),
                });
            }
            PointerInput::Up {
                position, button, ..
            } => {
                aimed = Some(target);
                state.position = Some(position);
                if button == PointerButton::Primary {
                    state.primary_down = false;
                    tracking.captured = None;
                }
                writers.releases.write(PointerReleased {
                    position,
                    button,
                    local: local(position),
                });
            }
            PointerInput::Move { position, .. } => {
                aimed = Some(target);
                state.position = Some(position);
                writers.moves.write(PointerMoved {
                    position,
                    local: local(position),
                });
            }
            PointerInput::Wheel {
                position, delta, ..
            } => {
                aimed = Some(target);
                state.position = Some(position);
                writers.wheels.write(MouseWheel {
                    delta,
                    position,
                    local: local(position),
                });
            }
            PointerInput::Left => {
                state.position = None;
                state.primary_down = false;
                tracking.captured = None;
                writers.left.write(PointerLeft);
            }
        }
    }
    // One entity carries the marker, and only when it changed does the world
    // hear about it: `pointerenter` and `pointerleave` fire off the change.
    for entity in &hovered {
        if Some(entity) != hover {
            commands.entity(entity).try_remove::<Hovered>();
        }
    }
    if let Some(entity) = hover
        && !hovered.contains(entity)
    {
        commands.entity(entity).try_insert(Hovered);
    }
}

/// The message writers [`drain_pointer_events`] fills, grouped so the system
/// stays under the parameter limit.
#[derive(bevy_ecs::system::SystemParam)]
pub struct PointerWriters<'w> {
    presses: MessageWriter<'w, PointerPressed>,
    releases: MessageWriter<'w, PointerReleased>,
    moves: MessageWriter<'w, PointerMoved>,
    wheels: MessageWriter<'w, MouseWheel>,
    left: MessageWriter<'w, PointerLeft>,
}

#[cfg(test)]
mod tests {
    use super::{PointerButton, PointerInput, button_of, hover_target, push};
    use bevy_ecs::hierarchy::ChildOf;
    use bevy_ecs::prelude::*;
    use bevy_ecs::system::SystemState;
    use glam::Vec2;
    use lumen_core::components::Disabled;

    /// The two queries [`hover_target`] walks.
    type Walk = (
        Query<'static, 'static, &'static ChildOf>,
        Query<'static, 'static, (), With<Disabled>>,
    );

    /// Where the hover lands for a pointer over `entity` in `world`.
    fn hovered_for(world: &mut World, entity: Entity) -> Option<Entity> {
        let mut state: SystemState<Walk> = SystemState::new(world);
        let (parents, disabled) = state.get(world).expect("the queries are valid");
        hover_target(entity, &parents, &disabled)
    }

    #[test]
    fn an_enabled_element_is_hovered_itself() {
        let mut world = World::new();
        let root = world.spawn_empty().id();
        let pad = world.spawn(ChildOf(root)).id();
        assert_eq!(hovered_for(&mut world, pad), Some(pad));
    }

    /// The desktop hit test skips a disabled subtree whole, so the pointer
    /// lands on the enabled element around it, however deep inside it the
    /// pointer is.
    #[test]
    fn a_disabled_subtree_hands_the_hover_to_the_element_around_it() {
        let mut world = World::new();
        let root = world.spawn_empty().id();
        let panel = world.spawn(ChildOf(root)).id();
        let button = world.spawn((Disabled, ChildOf(panel))).id();
        let caption = world.spawn(ChildOf(button)).id();
        assert_eq!(hovered_for(&mut world, button), Some(panel));
        assert_eq!(hovered_for(&mut world, caption), Some(panel));

        world.entity_mut(panel).insert(Disabled);
        assert_eq!(
            hovered_for(&mut world, caption),
            Some(root),
            "the outermost disabled ancestor decides"
        );
    }

    fn moved(x: f32) -> PointerInput {
        PointerInput::Move {
            hit: Some(crate::events::Hit {
                path: "0".to_string(),
                origin: Vec2::ZERO,
            }),
            position: Vec2::new(x, 0.0),
        }
    }

    #[test]
    fn a_run_of_moves_is_queued_as_its_last_position() {
        let mut queue = Vec::new();
        for x in [1.0, 2.0, 3.0] {
            push(&mut queue, moved(x));
        }
        assert_eq!(queue, vec![moved(3.0)]);
    }

    /// A move either side of a press is a different position the press
    /// happened between, so both are kept.
    #[test]
    fn a_press_breaks_the_run() {
        let mut queue = Vec::new();
        let down = PointerInput::Down {
            hit: None,
            position: Vec2::ZERO,
            button: PointerButton::Primary,
        };
        push(&mut queue, moved(1.0));
        push(&mut queue, down.clone());
        push(&mut queue, moved(2.0));
        push(&mut queue, moved(3.0));
        assert_eq!(queue, vec![moved(1.0), down, moved(3.0)]);
    }

    #[test]
    fn buttons_map_onto_the_window_backend_s_names() {
        assert_eq!(button_of(0), PointerButton::Primary);
        assert_eq!(button_of(1), PointerButton::Middle);
        assert_eq!(button_of(2), PointerButton::Secondary);
        assert_eq!(button_of(3), PointerButton::Other(3));
    }
}
