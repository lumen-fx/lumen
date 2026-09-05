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
//! The keys are the exception to both halves of that. They listen on the
//! document, because a key pressed while focus sits outside the app never
//! reaches the root, and they are filtered before they are queued: the same
//! pipeline runs here as on the desktop, so a key the browser's own control
//! has already acted on would be acted on twice. [`browser_handles`] is that
//! table.
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

use std::cell::{Cell, RefCell};
use std::collections::HashMap;

use bevy_ecs::hierarchy::ChildOf;
use bevy_ecs::message::MessageWriter;
use bevy_ecs::prelude::*;
use lumen_core::components::{DropHovered, DropTarget, SliderValue, TextContent, Toggleable};
use lumen_core::input::{
    ClickEvent, FocusTracker, Focused, Key, KeyPressed, KeyReleased, Modifiers, ModifiersState,
    NamedKey, PointerButton,
};
use lumen_core::property_store::PropertyStore;
use lumen_html::contract::DATA_LM;
use lumen_scene::spawn::IfMarker;
use wasm_bindgen::JsCast;
use wasm_bindgen::prelude::*;
use web_sys::{
    Element, Event, EventTarget, HtmlAnchorElement, HtmlElement, HtmlInputElement,
    HtmlTextAreaElement, KeyboardEvent, MouseEvent,
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
    /// A key the browser did not already act on for the element it landed
    /// on. No path: the key bus is global on the desktop too, and what is
    /// routed at an entity is routed off `FocusTracker`, which the focus
    /// events above already maintain.
    Key {
        /// True for `keydown`, false for `keyup`.
        down: bool,
        /// The key, already in Lumen's spelling.
        key: Key,
        /// Modifier state the browser reported on this event.
        modifiers: Modifiers,
        /// `KeyboardEvent.repeat`: the OS repeating a held key.
        repeat: bool,
    },
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

    /// Whether the document already carries the key listeners.
    ///
    /// Everything else listens on an app's own root, so a second app in the
    /// same page gets its own listener for it. The keys listen on the
    /// document, which the page has one of, and they push onto the queue
    /// above, which the page also has one of: binding them twice queues
    /// every keystroke twice.
    static KEYS_BOUND: Cell<bool> = const { Cell::new(false) };
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

/// The `KeyboardEvent.key` name in Lumen's spelling, or nothing for a key
/// that is not one of Lumen's at all.
///
/// The named keys map onto the [`NamedKey`] variants that exist, and
/// everything else becomes a `Key::Character`: a typed grapheme ("a",
/// "e\u{301}") and every named key with no variant of its own ("F1",
/// "PageUp", "Control") alike. That is the convention the window backend's
/// own key mapping follows, so a binding written once matches on both
/// targets.
fn lumen_key(name: &str) -> Option<Key> {
    Some(match name {
        "Tab" => Key::Named(NamedKey::Tab),
        "Enter" => Key::Named(NamedKey::Enter),
        "Escape" => Key::Named(NamedKey::Escape),
        "Backspace" => Key::Named(NamedKey::Backspace),
        // The browser spells the space bar as the character it types.
        // winit spells it `Space`, and the script layer renders that back
        // as "Space", so mapping it here is what makes `event.key` read the
        // same in a page as it does in a window.
        " " => Key::Named(NamedKey::Space),
        "ArrowUp" => Key::Named(NamedKey::ArrowUp),
        "ArrowDown" => Key::Named(NamedKey::ArrowDown),
        "ArrowLeft" => Key::Named(NamedKey::ArrowLeft),
        "ArrowRight" => Key::Named(NamedKey::ArrowRight),
        "Home" => Key::Named(NamedKey::Home),
        "End" => Key::Named(NamedKey::End),
        "Delete" => Key::Named(NamedKey::Delete),
        // A composing keystroke is the IME's, and the desktop routes what
        // it produces through `ImeEvent` rather than as a key.
        "Dead" | "Process" | "Unidentified" => return None,
        other => Key::Character(other.to_string()),
    })
}

/// True for the canonical named-key strings that reach the world as
/// `Key::Character` ("Shift", "F1", "PageUp", ...): multi-char pure-ASCII
/// words, where typed text is a single grapheme - one scalar, or a cluster
/// containing non-ASCII. The same test `lumen-input` applies to the keys it
/// must not type, kept here rather than exported so the dependency
/// direction stays as it is.
fn is_named_key_string(name: &str) -> bool {
    name.chars().count() > 1 && name.chars().all(|c| c.is_ascii_alphanumeric())
}

/// The kind of control a key landed on, which is what the browser's own
/// behaviour for that key depends on.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum TargetKind {
    /// An `<input>` of a text-like type, a `<textarea>`, or a
    /// contenteditable element: the browser is the editor.
    Text {
        /// Enter inserts a newline instead of doing nothing.
        multiline: bool,
    },
    /// `<button>`, `<a href>`, `<summary>`: the browser fires a click of
    /// its own for the activation keys.
    Activatable,
    /// `<input type=checkbox>`.
    Toggle,
    /// `<input type=radio>`, which the browser navigates as a `name` group.
    Radio,
    /// `<input type=range>`, which the browser steps itself.
    Range,
    /// Anything else, where no key does anything until Lumen acts on it.
    Other,
}

/// Which of those the element an event landed on is.
fn target_kind(event: &Event) -> TargetKind {
    let Some(element) = event.target().and_then(|t| t.dyn_into::<Element>().ok()) else {
        return TargetKind::Other;
    };
    if let Some(input) = element.dyn_ref::<HtmlInputElement>() {
        return match input.type_().as_str() {
            "checkbox" => TargetKind::Toggle,
            "radio" => TargetKind::Radio,
            "range" => TargetKind::Range,
            "button" | "submit" | "reset" | "image" => TargetKind::Activatable,
            // Every other type is a field with a caret in it, down to the
            // ones that carry a picker beside it.
            _ => TargetKind::Text { multiline: false },
        };
    }
    if element.dyn_ref::<HtmlTextAreaElement>().is_some() {
        return TargetKind::Text { multiline: true };
    }
    if element
        .dyn_ref::<HtmlElement>()
        .is_some_and(HtmlElement::is_content_editable)
    {
        return TargetKind::Text { multiline: true };
    }
    match element.tag_name().to_ascii_lowercase().as_str() {
        "button" | "summary" => TargetKind::Activatable,
        "a" if element.has_attribute("href") => TargetKind::Activatable,
        _ => TargetKind::Other,
    }
}

/// Whether the browser has already done, for this element, what Lumen's own
/// systems would do for this key.
///
/// A page runs the desktop input pipeline unchanged, so a key this answers
/// `true` for and is forwarded anyway edits a buffer the browser just
/// edited, clicks a button it just clicked, or steps a range it just
/// stepped. Such a key is dropped and reaches neither `KeyPressed` nor
/// `KeyReleased`. Everything else is forwarded, which is what an app-bound
/// shortcut, an Escape handler and the arrows between tabs need.
fn browser_handles(key: &Key, modifiers: Modifiers, target: TargetKind) -> bool {
    // Focus movement is the browser's, whatever the key landed on;
    // `cycle_focus_on_tab` would move Lumen's own `Focused` somewhere else
    // without telling the page. Nothing is lost: `dispatch_focused_keys`
    // never forwards Tab to a script on the desktop either.
    if matches!(key, Key::Named(NamedKey::Tab)) {
        return true;
    }
    match target {
        TargetKind::Text { multiline } => match key {
            // The newline the browser already inserted. On a single-line
            // field Enter mutates nothing, and forwarding it is what makes
            // `activate_focused_on_enter` raise the commit behind a script's
            // `change` and `submit`.
            Key::Named(NamedKey::Enter) => multiline,
            // Everything `type_into_focused` would write into a buffer the
            // browser's own editor owns.
            Key::Named(
                NamedKey::Space
                | NamedKey::Backspace
                | NamedKey::Delete
                | NamedKey::ArrowUp
                | NamedKey::ArrowDown
                | NamedKey::ArrowLeft
                | NamedKey::ArrowRight
                | NamedKey::Home
                | NamedKey::End,
            ) => true,
            // The six editing chords, and no others: Ctrl+K in a field is
            // an app's shortcut and reaches it.
            Key::Character(name) if modifiers.ctrl || modifiers.super_ => matches!(
                name.to_ascii_lowercase().as_str(),
                "a" | "c" | "x" | "v" | "z" | "y"
            ),
            Key::Character(name) => !is_named_key_string(name),
            // Escape, and the function keys the arm above lets through.
            Key::Named(_) => false,
        },
        // The click the browser fires for itself.
        TargetKind::Activatable => matches!(key, Key::Named(NamedKey::Enter | NamedKey::Space)),
        // The toggle, and the click the browser raises behind it.
        TargetKind::Toggle => matches!(key, Key::Named(NamedKey::Space)),
        // The same, plus the browser's own navigation inside the group the
        // emitter wrote as `name`.
        TargetKind::Radio => matches!(
            key,
            Key::Named(
                NamedKey::Space
                    | NamedKey::ArrowUp
                    | NamedKey::ArrowDown
                    | NamedKey::ArrowLeft
                    | NamedKey::ArrowRight
            )
        ),
        // The step `move_slider_on_keys` would take a second time.
        TargetKind::Range => match key {
            Key::Named(
                NamedKey::ArrowUp
                | NamedKey::ArrowDown
                | NamedKey::ArrowLeft
                | NamedKey::ArrowRight
                | NamedKey::Home
                | NamedKey::End,
            ) => true,
            Key::Character(name) => matches!(name.as_str(), "PageUp" | "PageDown"),
            _ => false,
        },
        TargetKind::Other => false,
    }
}

/// The modifier state the browser reported on a key event.
fn modifiers_of(event: &KeyboardEvent) -> Modifiers {
    Modifiers {
        shift: event.shift_key(),
        ctrl: event.ctrl_key(),
        alt: event.alt_key(),
        super_: event.meta_key(),
    }
}

/// Push `event` onto the queue.
fn queue(event: PendingEvent) {
    QUEUE.with_borrow_mut(|queue| queue.push(event));
}

/// Attach a listener that outlives this call.
///
/// The target is the app root for everything that lands on an element, and
/// the document for the keys, which land wherever focus happens to be.
fn on(target: &EventTarget, kind: &str, handler: Closure<dyn FnMut(Event)>) -> Result<(), JsValue> {
    target.add_event_listener_with_callback(kind, handler.as_ref().unchecked_ref())?;
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
    target: &EventTarget,
    kind: &str,
    handler: Closure<dyn FnMut(Event)>,
) -> Result<(), JsValue> {
    target.add_event_listener_with_callback_and_bool(
        kind,
        handler.as_ref().unchecked_ref(),
        true,
    )?;
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
    // Keys listen on the document rather than on the app root. A key
    // pressed while focus sits on `<body>` - which is the root's own
    // ancestor, and where focus is on a freshly loaded page - targets
    // `<body>` and never passes the root at all, so a listener there would
    // miss every app-wide shortcut. The desktop's key bus is global for the
    // same reason, and what it routes at an entity it routes through
    // `FocusTracker`.
    //
    // Nothing below calls `prevent_default`. Whether an app has a handler
    // bound for a key is only knowable a tick later, when the browser will
    // no longer take the answer, so the page keeps its own behaviour for
    // every key that is forwarded: Space and PageDown still scroll it.
    if let Some(document) = root.owner_document().filter(|_| !KEYS_BOUND.get()) {
        KEYS_BOUND.set(true);
        for (kind, down) in [("keydown", true), ("keyup", false)] {
            on(
                &document,
                kind,
                Closure::wrap(Box::new(move |event: Event| {
                    let Some(event) = event.dyn_ref::<KeyboardEvent>() else {
                        return;
                    };
                    // A keystroke the IME is still composing belongs to the
                    // composition, which reaches the world as the `input`
                    // event carrying the text it settles on.
                    if event.is_composing() {
                        return;
                    }
                    let Some(key) = lumen_key(&event.key()) else {
                        return;
                    };
                    let modifiers = modifiers_of(event);
                    if browser_handles(&key, modifiers, target_kind(event)) {
                        return;
                    }
                    queue(PendingEvent::Key {
                        down,
                        key,
                        modifiers,
                        repeat: event.repeat(),
                    });
                }) as Box<dyn FnMut(Event)>),
            )?;
        }
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
    mut presses: MessageWriter<KeyPressed>,
    mut releases: MessageWriter<KeyReleased>,
    mut modifiers_state: ResMut<ModifiersState>,
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
            // The browser reports the modifier state on the event itself,
            // so there is no separate modifiers event to mirror: the
            // resource `type_into_focused` and `activate_focused_on_enter`
            // read is refreshed from each key as it lands.
            PendingEvent::Key {
                down,
                key,
                modifiers,
                repeat,
            } => {
                modifiers_state.0 = modifiers;
                if down {
                    presses.write(KeyPressed {
                        key,
                        modifiers,
                        repeat,
                    });
                } else {
                    releases.write(KeyReleased { key, modifiers });
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
mod key_tests {
    use super::{Key, Modifiers, NamedKey, TargetKind, browser_handles, lumen_key};

    /// No modifier held, which is how a key arrives unless a test says
    /// otherwise.
    fn plain() -> Modifiers {
        Modifiers::default()
    }

    /// Ctrl held, the chord an app's shortcut and the browser's own editing
    /// commands share.
    fn ctrl() -> Modifiers {
        Modifiers {
            ctrl: true,
            ..Modifiers::default()
        }
    }

    #[test]
    fn a_named_key_maps_onto_the_variant_of_the_same_name() {
        assert_eq!(lumen_key("Enter"), Some(Key::Named(NamedKey::Enter)));
        assert_eq!(
            lumen_key("ArrowRight"),
            Some(Key::Named(NamedKey::ArrowRight))
        );
        assert_eq!(lumen_key("Escape"), Some(Key::Named(NamedKey::Escape)));
    }

    /// The browser spells the space bar as the character it types; every
    /// consumer on both targets reads it as `Space`.
    #[test]
    fn the_space_bar_arrives_as_the_named_key() {
        assert_eq!(lumen_key(" "), Some(Key::Named(NamedKey::Space)));
    }

    #[test]
    fn a_typed_grapheme_is_a_character() {
        assert_eq!(lumen_key("a"), Some(Key::Character("a".to_string())));
        assert_eq!(
            lumen_key("e\u{301}"),
            Some(Key::Character("e\u{301}".to_string()))
        );
    }

    /// The same convention the window backend uses for a named key with no
    /// `NamedKey` variant, so one binding matches on both targets.
    #[test]
    fn a_named_key_without_a_variant_keeps_its_w3c_name() {
        assert_eq!(lumen_key("F1"), Some(Key::Character("F1".to_string())));
        assert_eq!(
            lumen_key("PageUp"),
            Some(Key::Character("PageUp".to_string()))
        );
        assert_eq!(
            lumen_key("Control"),
            Some(Key::Character("Control".to_string()))
        );
    }

    #[test]
    fn a_composition_key_is_not_a_lumen_key_at_all() {
        assert_eq!(lumen_key("Dead"), None);
        assert_eq!(lumen_key("Process"), None);
        assert_eq!(lumen_key("Unidentified"), None);
    }

    #[test]
    fn tab_is_the_browser_s_on_every_element() {
        for target in [
            TargetKind::Other,
            TargetKind::Text { multiline: false },
            TargetKind::Activatable,
        ] {
            assert!(browser_handles(&Key::Named(NamedKey::Tab), plain(), target));
        }
    }

    /// The doubled-character case: the browser edited the field, the
    /// `input` listener carried the result back, and `type_into_focused`
    /// would write the same edit a second time.
    #[test]
    fn a_field_s_own_editing_keys_stay_with_the_field() {
        let field = TargetKind::Text { multiline: false };
        for key in [
            Key::Character("a".to_string()),
            Key::Named(NamedKey::Space),
            Key::Named(NamedKey::Backspace),
            Key::Named(NamedKey::Delete),
            Key::Named(NamedKey::ArrowLeft),
            Key::Named(NamedKey::Home),
            Key::Named(NamedKey::End),
        ] {
            assert!(browser_handles(&key, plain(), field), "{key:?}");
        }
        for chord in ["a", "c", "x", "v", "z", "y", "Z"] {
            assert!(
                browser_handles(&Key::Character(chord.to_string()), ctrl(), field),
                "Ctrl+{chord}"
            );
        }
    }

    /// What an app binds inside a field: a shortcut, an Escape handler, and
    /// the commit `activate_focused_on_enter` raises for a single-line
    /// entry.
    #[test]
    fn a_field_still_lets_an_app_s_own_keys_through() {
        let field = TargetKind::Text { multiline: false };
        assert!(!browser_handles(
            &Key::Character("k".to_string()),
            ctrl(),
            field
        ));
        assert!(!browser_handles(
            &Key::Named(NamedKey::Escape),
            plain(),
            field
        ));
        assert!(!browser_handles(
            &Key::Character("F1".to_string()),
            plain(),
            field
        ));
        assert!(!browser_handles(
            &Key::Named(NamedKey::Enter),
            plain(),
            field
        ));
    }

    /// A textarea is the one field where Enter is an edit.
    #[test]
    fn enter_in_a_multiline_field_is_the_newline_the_browser_inserted() {
        assert!(browser_handles(
            &Key::Named(NamedKey::Enter),
            plain(),
            TargetKind::Text { multiline: true }
        ));
    }

    /// Two clicks per keystroke is what forwarding these would give: the
    /// browser fires its own click on a `<button>`, and
    /// `activate_focused_on_enter` synthesizes another.
    #[test]
    fn the_activation_keys_stay_with_the_control_that_fires_a_click() {
        for key in [Key::Named(NamedKey::Enter), Key::Named(NamedKey::Space)] {
            assert!(browser_handles(&key, plain(), TargetKind::Activatable));
        }
        assert!(browser_handles(
            &Key::Named(NamedKey::Space),
            plain(),
            TargetKind::Toggle
        ));
        assert!(!browser_handles(
            &Key::Named(NamedKey::Escape),
            plain(),
            TargetKind::Activatable
        ));
    }

    /// The browser owns arrow navigation inside a `name` group, so
    /// `radio_group_keys` would move the selection a second time.
    #[test]
    fn a_radio_group_s_arrows_stay_with_the_browser() {
        assert!(browser_handles(
            &Key::Named(NamedKey::ArrowDown),
            plain(),
            TargetKind::Radio
        ));
    }

    /// Two steps per press is what forwarding these would give:
    /// `move_slider_on_keys` steps a value the browser already stepped.
    #[test]
    fn a_range_s_stepping_keys_stay_with_the_browser() {
        for key in [
            Key::Named(NamedKey::ArrowRight),
            Key::Named(NamedKey::Home),
            Key::Character("PageUp".to_string()),
        ] {
            assert!(browser_handles(&key, plain(), TargetKind::Range), "{key:?}");
        }
        assert!(!browser_handles(
            &Key::Named(NamedKey::Enter),
            plain(),
            TargetKind::Range
        ));
    }

    /// Everything outside a native control reaches the app, which is what
    /// the arrows between tabs and an Escape that closes a panel are.
    #[test]
    fn nothing_is_withheld_from_an_element_the_browser_does_nothing_for() {
        for key in [
            Key::Named(NamedKey::ArrowRight),
            Key::Named(NamedKey::Escape),
            Key::Named(NamedKey::Enter),
            Key::Named(NamedKey::Space),
            Key::Character("k".to_string()),
        ] {
            assert!(
                !browser_handles(&key, plain(), TargetKind::Other),
                "{key:?}"
            );
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
