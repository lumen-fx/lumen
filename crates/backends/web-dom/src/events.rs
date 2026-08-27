//! The browser as the event source.
//!
//! One listener per type sits on the app root and catches everything that
//! bubbles to it. It records which node the event landed on and what it
//! carried, and that is all it does: propagation, handler lookup and the
//! widget behaviour behind a click are Lumen's, unchanged from the desktop.
//! A click becomes a [`ClickEvent`], the same message a window backend
//! produces, and the tab strip, the toggle and the script event driver all
//! read it without knowing where it came from.
//!
//! A listener runs whenever the browser says so, which is never during a
//! tick, so what it records goes on a queue the app drains at the start of
//! the next one.
//!
//! Two things happen synchronously, inside the browser's own dispatch,
//! because by the time a tick drains the queue it is too late to ask for:
//! whether a click on a same-page `<a href>` should keep the browser from
//! navigating (soft mode intercepts it; [`should_soft_navigate`] is the
//! decision), and whether a `dragover` is accepted at all (a target has to
//! call `preventDefault` or the browser refuses the `drop` that would
//! follow).

use std::cell::RefCell;
use std::collections::HashMap;

use bevy_ecs::hierarchy::ChildOf;
use bevy_ecs::message::MessageWriter;
use bevy_ecs::prelude::*;
use lumen_core::components::{DropHovered, DropTarget, SliderValue, TextContent, Toggleable};
use lumen_core::input::{ClickEvent, FocusTracker, Focused, PointerButton};
use lumen_core::property_store::PropertyStore;
use lumen_html::contract::DATA_LM;
use lumen_scene::spawn::IfMarker;
use wasm_bindgen::JsCast;
use wasm_bindgen::prelude::*;
use web_sys::{
    Element, Event, HtmlAnchorElement, HtmlInputElement, HtmlTextAreaElement, MouseEvent,
};

/// One thing the browser told us, waiting for the next tick.
enum PendingEvent {
    /// A click landed on the node at this path.
    Click {
        /// Node path of the element the event landed on.
        path: String,
        /// Client coordinates of the pointer.
        position: (f64, f64),
        /// `MouseEvent.button`.
        button: i16,
    },
    /// A text entry's value changed as the user typed.
    Input {
        /// Node path of the entry.
        path: String,
        /// The value now in the field.
        value: String,
    },
    /// A checkbox or radio was switched by the browser's own control.
    Checked {
        /// Node path of the control.
        path: String,
        /// The state it is now in.
        checked: bool,
    },
    /// Focus moved onto or off the node at this path.
    Focus {
        /// Node path of the element focus moved to, or away from.
        path: String,
        /// True when focus arrived, false when it left.
        gained: bool,
    },
    /// A drag entered the node at this path (or a descendant of it).
    DragEnter {
        /// Node path of the element the event landed on.
        path: String,
    },
    /// A drag left the node at this path (or a descendant of it).
    DragLeave {
        /// Node path of the element the event landed on.
        path: String,
    },
    /// Something was dropped on the node at this path (or a descendant).
    Drop {
        /// Node path of the element the event landed on.
        path: String,
    },
    /// A drag left the document entirely, so every drop target's hover
    /// marker clears regardless of which one it landed on last.
    DragCancelled,
}

thread_local! {
    /// Events the browser has delivered since the last tick.
    ///
    /// A thread local rather than a resource: the listeners are owned by the
    /// browser and outlive every borrow of the world, so they cannot hold
    /// one. The page is single threaded, which is what makes this the whole
    /// of the synchronisation.
    static QUEUE: RefCell<Vec<PendingEvent>> = const { RefCell::new(Vec::new()) };

    /// Node paths of the dialogs the browser has dismissed since the last
    /// tick. Their own queue because what they need from the world is a
    /// different set of things entirely.
    static DISMISSED: RefCell<Vec<String>> = const { RefCell::new(Vec::new()) };
}

/// The node path of the element an event landed on, or of the nearest
/// ancestor that stands for a node.
fn path_of(event: &Event) -> Option<String> {
    let target = event.target()?.dyn_into::<Element>().ok()?;
    let element = target
        .closest(&format!("[{DATA_LM}]"))
        .ok()
        .flatten()
        .unwrap_or(target);
    element.get_attribute(DATA_LM)
}

/// The `<a href>` a click landed on or inside, whether or not it is one
/// Lumen spawned.
fn anchor_of(event: &Event) -> Option<HtmlAnchorElement> {
    let target = event.target()?.dyn_into::<Element>().ok()?;
    target
        .closest("a[href]")
        .ok()
        .flatten()
        .and_then(|el| el.dyn_into::<HtmlAnchorElement>().ok())
}

/// Everything a click's target anchor says, gathered once so
/// [`should_soft_navigate`] reads names instead of a run of positional
/// bools a call site could quietly transpose.
struct LinkClick {
    /// The element carries [`DATA_LM`] - Lumen spawned it, rather than a
    /// script writing raw markup in on its own.
    is_lumen_anchor: bool,
    /// The link's origin is this page's origin.
    same_origin: bool,
    /// The link's `pathname` or `search` differs from the current
    /// document's. False for a same-page `href="#section"`: such a link is
    /// same-origin, carries [`DATA_LM`] like every anchor Lumen spawns, and
    /// passes every other guard, but intercepting it hands the click to the
    /// in-app router, which has no page named `#section` and falls back to
    /// the entry page. Left alone, the browser's own same-document anchor
    /// handling - the scroll, and no navigation at all - is what "back to
    /// top" and a table of contents need.
    navigates_elsewhere: bool,
    /// `MouseEvent.button` is the primary button (usually left).
    primary_button: bool,
    /// Ctrl, Shift, Meta or Alt was held - the visitor asked for something
    /// other than "follow this link here" (a new tab, a new window).
    modifier_held: bool,
    /// The anchor's `target` names somewhere other than this document.
    opens_elsewhere: bool,
    /// The anchor carries `download`.
    is_download: bool,
}

/// Whether a click on a same-page `<a href>` should be handled by the in-app
/// router instead of the browser's own navigation.
///
/// Every field of `link` is something a hand-written router already has to
/// respect: the visitor opened the link a plain way, it stays on this
/// document, and Lumen is the one that spawned it rather than a script
/// writing markup in on its own. `soft` gates all of it, so a site built
/// with `navigation = "hard"` never intercepts at all.
fn should_soft_navigate(soft: bool, link: LinkClick) -> bool {
    soft && link.is_lumen_anchor
        && link.same_origin
        && link.navigates_elsewhere
        && link.primary_button
        && !link.modifier_held
        && !link.opens_elsewhere
        && !link.is_download
}

/// Walk up from `start` to the nearest entity - itself or an ancestor - that
/// carries [`DropTarget`].
///
/// A `dragenter` or `dragleave` lands on whatever the pointer is directly
/// over, which is as often a drop target's label or icon as the target
/// itself; the stylesheet's `:drag-over` rule is written against the target,
/// so that is the entity the marker belongs on.
fn nearest_drop_target(
    start: Entity,
    targets: &Query<(), With<DropTarget>>,
    parents: &Query<&ChildOf>,
) -> Option<Entity> {
    let mut cur = start;
    loop {
        if targets.contains(cur) {
            return Some(cur);
        }
        cur = parents.get(cur).ok()?.parent();
    }
}

/// Nesting depth of unmatched `dragenter`s over a drop target, keyed by the
/// entity [`DropHovered`] mirrors onto.
///
/// `dragenter` and `dragleave` bubble like `mouseover` / `mouseout`, so
/// crossing from one of a target's children to a sibling fires a
/// `dragleave` immediately followed by a `dragenter`, both resolving to the
/// same target. The four functions below are the state machine that keeps
/// the count straight; [`drain_dom_events`] is the only caller, and reading
/// it as `World`-free plain data is what makes the transitions provable
/// without a browser (or even a `World`) in front of them.
type HoverDepth = HashMap<Entity, u32>;

/// A `dragenter` resolved to `target`: its count goes up by one. Returns
/// `true` exactly when the count crossed 0 -> 1, which is the one transition
/// that needs `DropHovered` inserted.
fn enter_drop_target(depth: &mut HoverDepth, target: Entity) -> bool {
    let count = depth.entry(target).or_insert(0);
    *count += 1;
    *count == 1
}

/// A `dragleave` resolved to `target`: its count goes down by one, floored
/// at zero. Returns `true` exactly when the count crossed 1 -> 0, which is
/// the one transition that needs `DropHovered` removed. A `dragleave` for a
/// target this map never saw a matching `dragenter` for changes nothing.
fn leave_drop_target(depth: &mut HoverDepth, target: Entity) -> bool {
    let Some(count) = depth.get_mut(&target) else {
        return false;
    };
    *count = count.saturating_sub(1);
    if *count == 0 {
        depth.remove(&target);
        true
    } else {
        false
    }
}

/// A drop on `target`, or the drag it was part of being cancelled: the
/// gesture is over regardless of how many unmatched enters were on the
/// books, so its count clears outright and `DropHovered` always comes off.
fn clear_drop_target(depth: &mut HoverDepth, target: Entity) {
    depth.remove(&target);
}

/// The drag left the document entirely, which is not an event any one
/// target's count can see: every tracked target's count clears, and
/// `DropHovered` comes off every one of them.
fn clear_every_drop_target(depth: &mut HoverDepth) -> Vec<Entity> {
    depth.drain().map(|(entity, _)| entity).collect()
}

#[cfg(test)]
mod drag_hover_tests {
    use bevy_ecs::system::RunSystemOnce;

    use super::{
        ChildOf, DropTarget, Entity, HoverDepth, Query, With, World, clear_drop_target,
        clear_every_drop_target, enter_drop_target, leave_drop_target, nearest_drop_target,
    };

    /// Resolve `start` against a freshly built `World`, the same way
    /// [`drain_dom_events`] would with real queries.
    fn walk(world: &mut World, start: Entity) -> Option<Entity> {
        world
            .run_system_once(
                move |targets: Query<(), With<DropTarget>>, parents: Query<&ChildOf>| {
                    nearest_drop_target(start, &targets, &parents)
                },
            )
            .unwrap()
    }

    #[test]
    fn a_drop_target_resolves_to_itself() {
        let mut world = World::new();
        let target = world.spawn(DropTarget).id();
        assert_eq!(walk(&mut world, target), Some(target));
    }

    #[test]
    fn a_descendant_resolves_to_its_nearest_drop_target_ancestor() {
        let mut world = World::new();
        let target = world.spawn(DropTarget).id();
        let child = world.spawn(ChildOf(target)).id();
        let grandchild = world.spawn(ChildOf(child)).id();
        assert_eq!(walk(&mut world, grandchild), Some(target));
    }

    #[test]
    fn nothing_with_no_drop_target_ancestor_resolves_at_all() {
        let mut world = World::new();
        let parent = world.spawn_empty().id();
        let child = world.spawn(ChildOf(parent)).id();
        assert_eq!(walk(&mut world, child), None);
    }

    /// A lone `Entity` id, for tests that only care about depth-counting and
    /// never resolve one through a `World`.
    fn entity(index: u32) -> Entity {
        Entity::from_raw_u32(index).unwrap()
    }

    #[test]
    fn a_single_enter_marks_and_a_single_leave_clears() {
        let mut depth = HoverDepth::new();
        let target = entity(0);
        assert!(enter_drop_target(&mut depth, target), "0 -> 1 marks");
        assert!(leave_drop_target(&mut depth, target), "1 -> 0 clears");
        assert!(depth.is_empty(), "nothing left to track");
    }

    #[test]
    fn a_second_enter_does_not_re_mark() {
        let mut depth = HoverDepth::new();
        let target = entity(0);
        assert!(enter_drop_target(&mut depth, target));
        assert!(
            !enter_drop_target(&mut depth, target),
            "already marked; 1 -> 2 is not a transition"
        );
    }

    /// The bug the depth counter exists to prevent: crossing from one child
    /// of a target to a sibling fires `dragleave` then `dragenter` for the
    /// same target. Binary insert/remove would clear the marker for a tick;
    /// counted, the leave only drops to 1 and never signals a clear.
    #[test]
    fn a_leave_immediately_followed_by_an_enter_never_clears() {
        let mut depth = HoverDepth::new();
        let target = entity(0);
        enter_drop_target(&mut depth, target); // first child's enter: 0 -> 1
        enter_drop_target(&mut depth, target); // second child's enter: 1 -> 2
        assert!(
            !leave_drop_target(&mut depth, target),
            "first child's leave: 2 -> 1, still hovered"
        );
        assert_eq!(depth.get(&target), Some(&1));
    }

    #[test]
    fn a_leave_for_an_untracked_target_changes_nothing() {
        let mut depth = HoverDepth::new();
        assert!(!leave_drop_target(&mut depth, entity(0)));
        assert!(depth.is_empty());
    }

    #[test]
    fn a_drop_clears_however_many_unmatched_enters_were_on_the_books() {
        let mut depth = HoverDepth::new();
        let target = entity(0);
        enter_drop_target(&mut depth, target);
        enter_drop_target(&mut depth, target);
        clear_drop_target(&mut depth, target);
        assert!(depth.is_empty());
    }

    #[test]
    fn cancelling_clears_every_tracked_target() {
        let mut depth = HoverDepth::new();
        let a = entity(0);
        let b = entity(1);
        enter_drop_target(&mut depth, a);
        enter_drop_target(&mut depth, b);
        let cleared = clear_every_drop_target(&mut depth);
        assert_eq!(cleared.len(), 2);
        assert!(cleared.contains(&a) && cleared.contains(&b));
        assert!(depth.is_empty());
    }
}

/// The text an element carries as a form value, for the entries that have
/// one.
fn value_of(event: &Event) -> Option<String> {
    let target = event.target()?;
    if let Some(input) = target.dyn_ref::<HtmlInputElement>() {
        return Some(input.value());
    }
    target.dyn_ref::<HtmlTextAreaElement>().map(|a| a.value())
}

/// Whether the control an event landed on is checked, for the ones that can
/// be.
fn checked_of(event: &Event) -> Option<bool> {
    let input = event.target()?.dyn_into::<HtmlInputElement>().ok()?;
    matches!(input.type_().as_str(), "checkbox" | "radio").then(|| input.checked())
}

/// Push `event` onto the queue.
fn queue(event: PendingEvent) {
    QUEUE.with_borrow_mut(|queue| queue.push(event));
}

/// Attach a listener that outlives this call.
fn on(root: &Element, kind: &str, handler: Closure<dyn FnMut(Event)>) -> Result<(), JsValue> {
    root.add_event_listener_with_callback(kind, handler.as_ref().unchecked_ref())?;
    // Handed to the browser, which owns the callback from here.
    handler.forget();
    Ok(())
}

/// Attach a listener that also sees events which do not bubble.
///
/// The capture phase runs from the document down to the target whatever the
/// event is, so one listener on the root catches an event fired at a
/// descendant that would never travel back up. A dialog's dismissal is one
/// of those.
fn on_capture(
    root: &Element,
    kind: &str,
    handler: Closure<dyn FnMut(Event)>,
) -> Result<(), JsValue> {
    root.add_event_listener_with_callback_and_bool(kind, handler.as_ref().unchecked_ref(), true)?;
    handler.forget();
    Ok(())
}

/// Start listening on `root` for the events that drive an app.
///
/// `soft_navigation` is `[web] navigation = "soft"` from the site's
/// manifest: whether a click on a same-page `<a href>` this listener finds
/// eligible (see [`should_soft_navigate`]) keeps the browser from loading
/// the next document at all, leaving the in-app router (already installed
/// whenever the site has more than one page) to swap it in place.
pub(crate) fn listen(root: &Element, soft_navigation: bool) -> Result<(), JsValue> {
    let location = web_sys::window().map(|w| w.location());
    let origin = location
        .as_ref()
        .and_then(|l| l.origin().ok())
        .unwrap_or_default();
    // Captured once: nothing in this build moves the document to a new
    // address (soft navigation swaps the page in place, and hard navigation
    // leaves the page entirely), so the address a `dragover` and a click both
    // compare against never changes under this listener's watch.
    let pathname = location
        .as_ref()
        .and_then(|l| l.pathname().ok())
        .unwrap_or_default();
    let search = location.and_then(|l| l.search().ok()).unwrap_or_default();
    on(
        root,
        "click",
        Closure::wrap(Box::new(move |event: Event| {
            let mouse = event.dyn_ref::<MouseEvent>();
            if soft_navigation && let Some(anchor) = anchor_of(&event) {
                let modifier_held = mouse
                    .is_some_and(|m| m.ctrl_key() || m.shift_key() || m.meta_key() || m.alt_key());
                let primary_button = mouse.is_none_or(|m| m.button() == 0);
                let opens_elsewhere = !matches!(anchor.target().as_str(), "" | "_self");
                let navigates_elsewhere =
                    anchor.pathname() != pathname || anchor.search() != search;
                if should_soft_navigate(
                    soft_navigation,
                    LinkClick {
                        is_lumen_anchor: anchor.has_attribute(DATA_LM),
                        same_origin: anchor.origin() == origin,
                        navigates_elsewhere,
                        primary_button,
                        modifier_held,
                        opens_elsewhere,
                        is_download: anchor.has_attribute("download"),
                    },
                ) {
                    event.prevent_default();
                }
            }
            let Some(path) = path_of(&event) else {
                return;
            };
            queue(PendingEvent::Click {
                path,
                position: mouse.map_or((0.0, 0.0), |m| {
                    (f64::from(m.client_x()), f64::from(m.client_y()))
                }),
                button: mouse.map_or(0, MouseEvent::button),
            });
        }) as Box<dyn FnMut(Event)>),
    )?;
    // `dragover` fires continuously while a drag hovers, and the browser
    // refuses `drop` on any element whose `dragover` did not prevent the
    // default action - accepting a drop is opt-in the same way it is for a
    // hand-written page, so every drop target says so unconditionally.
    on(
        root,
        "dragover",
        Closure::wrap(Box::new(move |event: Event| {
            event.prevent_default();
        }) as Box<dyn FnMut(Event)>),
    )?;
    on(
        root,
        "dragenter",
        Closure::wrap(Box::new(move |event: Event| {
            if let Some(path) = path_of(&event) {
                queue(PendingEvent::DragEnter { path });
            }
        }) as Box<dyn FnMut(Event)>),
    )?;
    on(
        root,
        "dragleave",
        Closure::wrap(Box::new(move |event: Event| {
            // `dragleave`'s `relatedTarget` is the element the pointer is
            // entering, the same as `mouseout`'s; it is `None` only when
            // there is no such element, which is what leaving the document
            // altogether looks like - dragging out of the browser window
            // being the common way an OS file drag gets cancelled instead of
            // dropped. Nothing else says so, so this is the fallback that
            // clears every drop target's marker rather than leaving one lit
            // because the element-level `dragleave` it depends on never
            // fires on this path.
            let left_the_document = event
                .dyn_ref::<MouseEvent>()
                .is_none_or(|m| m.related_target().is_none());
            if left_the_document {
                queue(PendingEvent::DragCancelled);
            } else if let Some(path) = path_of(&event) {
                queue(PendingEvent::DragLeave { path });
            }
        }) as Box<dyn FnMut(Event)>),
    )?;
    on(
        root,
        "drop",
        Closure::wrap(Box::new(move |event: Event| {
            // Left unaccepted, the browser's own default action for a drop
            // is to open the dropped file - preventing it keeps the page
            // where it was even for a target this build has no `DropTarget`
            // for.
            event.prevent_default();
            if let Some(path) = path_of(&event) {
                queue(PendingEvent::Drop { path });
            }
        }) as Box<dyn FnMut(Event)>),
    )?;
    on(
        root,
        "input",
        Closure::wrap(Box::new(move |event: Event| {
            let Some(path) = path_of(&event) else {
                return;
            };
            if let Some(checked) = checked_of(&event) {
                queue(PendingEvent::Checked { path, checked });
            } else if let Some(value) = value_of(&event) {
                queue(PendingEvent::Input { path, value });
            }
        }) as Box<dyn FnMut(Event)>),
    )?;
    // Escape on a showing dialog is the browser's to handle, and `cancel` is
    // how it says so. Letting it run closes the element; what the world still
    // has to learn is that the signal behind it is now false.
    on_capture(
        root,
        "cancel",
        Closure::wrap(Box::new(move |event: Event| {
            if let Some(path) = path_of(&event) {
                DISMISSED.with_borrow_mut(|queue| queue.push(path));
            }
        }) as Box<dyn FnMut(Event)>),
    )?;
    for (kind, gained) in [("focusin", true), ("focusout", false)] {
        on(
            root,
            kind,
            Closure::wrap(Box::new(move |event: Event| {
                if let Some(path) = path_of(&event) {
                    queue(PendingEvent::Focus { path, gained });
                }
            }) as Box<dyn FnMut(Event)>),
        )?;
    }
    Ok(())
}

/// Turn everything the browser delivered into the messages and components
/// the app's own systems read.
#[allow(clippy::too_many_arguments)] // ECS system: each arg is a query/param
pub fn drain_dom_events(
    table: NonSend<crate::nodes::NodeTable>,
    mut commands: Commands,
    mut clicks: MessageWriter<ClickEvent>,
    mut texts: Query<&mut TextContent>,
    mut toggles: Query<&mut Toggleable>,
    mut sliders: Query<&mut SliderValue>,
    focus: Option<ResMut<FocusTracker>>,
    drop_targets: Query<(), With<DropTarget>>,
    parents: Query<&ChildOf>,
    mut hover_depth: Local<HoverDepth>,
) {
    let pending = QUEUE.with_borrow_mut(std::mem::take);
    if pending.is_empty() {
        return;
    }
    let mut focus = focus;
    for event in pending {
        match event {
            PendingEvent::Click {
                path,
                position,
                button,
            } => {
                let Some(entity) = table.entity_at(&path) else {
                    continue;
                };
                clicks.write(ClickEvent {
                    entity,
                    position: glam::Vec2::new(position.0 as f32, position.1 as f32),
                    button: match button {
                        1 => PointerButton::Middle,
                        2 => PointerButton::Secondary,
                        0 => PointerButton::Primary,
                        other => PointerButton::Other(other.unsigned_abs()),
                    },
                });
            }
            PendingEvent::Input { path, value } => {
                let Some(entity) = table.entity_at(&path) else {
                    continue;
                };
                // A field being edited is a field with focus, whether or not
                // the focus event that says so has arrived. Without the
                // marker the signal-to-text binding treats the edit as stale
                // and writes the old value back over it.
                commands.entity(entity).insert(Focused);
                if let Some(tracker) = focus.as_mut() {
                    tracker.0 = Some(entity);
                }
                // A range input reports its position as text and the world
                // keeps it as a number, which is what the binding behind it
                // publishes. The browser has already clamped it to the
                // bounds the markup gave the control.
                if let Ok(mut slider) = sliders.get_mut(entity) {
                    if let Ok(moved) = value.parse::<f32>()
                        && slider.value != moved
                    {
                        slider.value = moved;
                    }
                    continue;
                }
                if let Ok(mut text) = texts.get_mut(entity)
                    && text.0 != value
                {
                    text.0 = value;
                }
            }
            PendingEvent::Checked { path, checked } => {
                let Some(entity) = table.entity_at(&path) else {
                    continue;
                };
                if let Ok(mut toggle) = toggles.get_mut(entity)
                    && toggle.checked != checked
                {
                    toggle.checked = checked;
                }
            }
            // The browser owns focus; the world mirrors it, so a binding
            // does not overwrite the field the user is typing in.
            PendingEvent::Focus { path, gained } => {
                let Some(entity) = table.entity_at(&path) else {
                    continue;
                };
                if gained {
                    commands.entity(entity).insert(Focused);
                    if let Some(tracker) = focus.as_mut() {
                        tracker.0 = Some(entity);
                    }
                } else {
                    commands.entity(entity).remove::<Focused>();
                    if let Some(tracker) = focus.as_mut()
                        && tracker.0 == Some(entity)
                    {
                        tracker.0 = None;
                    }
                }
            }
            // `DropHovered` is what `project_control_state` mirrors onto
            // `data-lm-drag-over`, so a target under a drag reads the same
            // way it does on the desktop, where the pointer-gesture pipeline
            // (`lumen-os-dnd::track_drop_hover`) inserts the same marker.
            // The state machine itself - what a resolved entity and the
            // current counts do to `hover_depth` - is the plain-data
            // functions above; this arm is only resolving the path to an
            // entity (which needs `table`, the one thing here a native test
            // cannot construct) and turning their answer into a `Commands`
            // write.
            PendingEvent::DragEnter { path } => {
                let Some(entity) = table.entity_at(&path) else {
                    continue;
                };
                if let Some(target) = nearest_drop_target(entity, &drop_targets, &parents)
                    && enter_drop_target(&mut hover_depth, target)
                {
                    commands.entity(target).insert(DropHovered);
                }
            }
            PendingEvent::DragLeave { path } => {
                let Some(entity) = table.entity_at(&path) else {
                    continue;
                };
                if let Some(target) = nearest_drop_target(entity, &drop_targets, &parents)
                    && leave_drop_target(&mut hover_depth, target)
                {
                    commands.entity(target).remove::<DropHovered>();
                }
            }
            // A drop ends the gesture outright, so the marker clears however
            // many unmatched enters this target still has on the books.
            PendingEvent::Drop { path } => {
                let Some(entity) = table.entity_at(&path) else {
                    continue;
                };
                if let Some(target) = nearest_drop_target(entity, &drop_targets, &parents) {
                    clear_drop_target(&mut hover_depth, target);
                    commands.entity(target).remove::<DropHovered>();
                }
            }
            // The drag left the document, which is not an event any one
            // target's count can see: clear every target this build still
            // has one for.
            PendingEvent::DragCancelled => {
                for target in clear_every_drop_target(&mut hover_depth) {
                    commands.entity(target).remove::<DropHovered>();
                }
            }
        }
    }
}

/// Follow the dialogs the browser dismissed back into the world.
///
/// The element is already closed by the time this runs. What is left is the
/// signal named in `open="..."`, which is what stops the reconciler showing
/// the dialog again and what a script watching the close reads. It is the
/// same write a Cancel button makes, so the dialog's own lifecycle resolves
/// the close as a rejection either way.
pub fn drain_dismissed_dialogs(
    table: NonSend<crate::nodes::NodeTable>,
    branches: Query<&IfMarker>,
    mut store: ResMut<PropertyStore>,
) {
    for path in DISMISSED.with_borrow_mut(std::mem::take) {
        let Some(entity) = table.entity_at(&path) else {
            continue;
        };
        if let Ok(branch) = branches.get(entity) {
            store.set_global_str(branch.signal_name.as_str(), "");
        }
    }
}

#[cfg(test)]
mod tests {
    use super::{LinkClick, should_soft_navigate};

    /// Every guard on, which is a plain left-click on a Lumen anchor to
    /// another page of the same site under `navigation = "soft"`.
    fn eligible() -> LinkClick {
        LinkClick {
            is_lumen_anchor: true,
            same_origin: true,
            navigates_elsewhere: true,
            primary_button: true,
            modifier_held: false,
            opens_elsewhere: false,
            is_download: false,
        }
    }

    #[test]
    fn an_eligible_click_is_intercepted() {
        assert!(should_soft_navigate(true, eligible()));
    }

    #[test]
    fn hard_navigation_never_intercepts() {
        assert!(!should_soft_navigate(false, eligible()));
    }

    #[test]
    fn a_click_outside_a_lumen_anchor_is_left_alone() {
        assert!(!should_soft_navigate(
            true,
            LinkClick {
                is_lumen_anchor: false,
                ..eligible()
            }
        ));
    }

    #[test]
    fn a_cross_origin_link_is_left_alone() {
        assert!(!should_soft_navigate(
            true,
            LinkClick {
                same_origin: false,
                ..eligible()
            }
        ));
    }

    /// The bug #213 review caught: `href="#section"` is same-origin, carries
    /// `data-lm`, and passes every other guard, but its `pathname` and
    /// `search` are the current document's - intercepting it hands the click
    /// to the in-app router, which has no page named `#section` and falls
    /// back to the entry page. `navigates_elsewhere: false` is what a
    /// same-document fragment link looks like, and it alone must be enough
    /// to leave the click alone so the browser's own anchor scroll runs.
    #[test]
    fn a_same_document_fragment_link_is_left_alone() {
        assert!(!should_soft_navigate(
            true,
            LinkClick {
                navigates_elsewhere: false,
                ..eligible()
            }
        ));
    }

    #[test]
    fn a_non_primary_button_is_left_alone() {
        assert!(!should_soft_navigate(
            true,
            LinkClick {
                primary_button: false,
                ..eligible()
            }
        ));
    }

    #[test]
    fn a_modifier_click_is_left_alone() {
        assert!(!should_soft_navigate(
            true,
            LinkClick {
                modifier_held: true,
                ..eligible()
            }
        ));
    }

    #[test]
    fn a_link_that_opens_elsewhere_is_left_alone() {
        assert!(!should_soft_navigate(
            true,
            LinkClick {
                opens_elsewhere: true,
                ..eligible()
            }
        ));
    }

    #[test]
    fn a_download_link_is_left_alone() {
        assert!(!should_soft_navigate(
            true,
            LinkClick {
                is_download: true,
                ..eligible()
            }
        ));
    }
}
