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

use std::cell::RefCell;

use bevy_ecs::message::MessageWriter;
use bevy_ecs::prelude::*;
use lumen_core::components::{SliderValue, TextContent, Toggleable};
use lumen_core::input::{ClickEvent, FocusTracker, Focused, PointerButton};
use lumen_core::property_store::PropertyStore;
use lumen_html::contract::DATA_LM;
use lumen_scene::spawn::IfMarker;
use wasm_bindgen::JsCast;
use wasm_bindgen::prelude::*;
use web_sys::{Element, Event, HtmlInputElement, HtmlTextAreaElement, MouseEvent};

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
pub(crate) fn listen(root: &Element) -> Result<(), JsValue> {
    on(
        root,
        "click",
        Closure::wrap(Box::new(move |event: Event| {
            let Some(path) = path_of(&event) else {
                return;
            };
            let mouse = event.dyn_ref::<MouseEvent>();
            queue(PendingEvent::Click {
                path,
                position: mouse.map_or((0.0, 0.0), |m| {
                    (f64::from(m.client_x()), f64::from(m.client_y()))
                }),
                button: mouse.map_or(0, MouseEvent::button),
            });
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
pub fn drain_dom_events(
    table: NonSend<crate::nodes::NodeTable>,
    mut commands: Commands,
    mut clicks: MessageWriter<ClickEvent>,
    mut texts: Query<&mut TextContent>,
    mut toggles: Query<&mut Toggleable>,
    mut sliders: Query<&mut SliderValue>,
    focus: Option<ResMut<FocusTracker>>,
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
