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
use lumen_core::components::{TextContent, Toggleable};
use lumen_core::input::{ClickEvent, FocusTracker, Focused, PointerButton};
use lumen_html::contract::DATA_LM;
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
