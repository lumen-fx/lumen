//! Input on a surface, forwarded to its producer.
//!
//! A surface is an element like any other: it is hovered, pressed, focused
//! and locked through the input layer, and its overlay children take the
//! pointer first. What reaches the surface itself goes to the producer, in
//! the order it happened, with pointer positions in texture pixels (the
//! element's box, `object-fit` and the device pixel ratio undone), so a
//! click picks what is under it in the frame.
//!
//! Keyboard rules, after play-in-editor tools and the browser's pointer
//! lock: while the surface has focus, keys go to the producer except Tab
//! (which moves focus) and the menu bar's shortcuts; under a pointer lock,
//! or with `keyboard="all"`, every key goes except Escape, which ends the
//! lock. When the surface loses focus the producer hears it and lets go of
//! every key and button it held.

use crate::api::SurfaceElement;
use crate::paint::frame_rect;
use bevy_ecs::message::MessageReader;
use bevy_ecs::prelude::*;
use lumen_module::lumen_core::components::{ImageFit, LumenAttributes};
use lumen_module::lumen_core::input::{
    Focused, HoverWithin, Hovered, ImeEvent, Key, KeyPressed, KeyReleased, Modifiers, MouseWheel,
    NamedKey, PointerData, PointerKind, PointerLockChanged, PointerLockState, PointerMoved,
    PointerPressed, PointerReleased, WindowFocused,
};
use lumen_module::lumen_core::window::Viewports;
use lumen_module::lumen_primitives::MenuAccelerators;
use lumen_surface_protocol::wire::{InputEvent, PointerInput, modifiers};

/// What the forwarding remembers between ticks.
#[derive(Resource, Default)]
pub struct ForwardState {
    /// The surface a press began on, which keeps the pointer until the
    /// buttons come up, wherever it goes.
    pressed: Option<Entity>,
}

/// A position in the element's frame, in texture pixels, from a window
/// position: undoes where the element is, how the frame fits it, and the
/// device pixel ratio.
fn to_texture(
    element: &SurfaceElement,
    fit: ImageFit,
    scale: f32,
    position: glam::Vec2,
) -> Option<glam::Vec2> {
    let frame = element.shared.frame()?.size;
    let [x, y, w, h] = element.shared.placement()?;
    let [fx, fy, fw, fh] = frame_rect(
        frame,
        scale,
        glam::Vec2::new(x, y),
        glam::Vec2::new(w, h),
        fit,
    );
    if fw <= 0.0 || fh <= 0.0 {
        return None;
    }
    Some(glam::Vec2::new(
        (position.x - fx) * frame.0 as f32 / fw,
        (position.y - fy) * frame.1 as f32 / fh,
    ))
}

/// The ratio from logical motion to texture pixels.
fn motion_scale(element: &SurfaceElement, fit: ImageFit, scale: f32) -> glam::Vec2 {
    let (Some(frame), Some([x, y, w, h])) = (
        element.shared.frame().map(|f| f.size),
        element.shared.placement(),
    ) else {
        return glam::Vec2::splat(scale);
    };
    let [_, _, fw, fh] = frame_rect(
        frame,
        scale,
        glam::Vec2::new(x, y),
        glam::Vec2::new(w, h),
        fit,
    );
    glam::Vec2::new(
        frame.0 as f32 / fw.max(0.001),
        frame.1 as f32 / fh.max(0.001),
    )
}

fn pointer_input(
    at: glam::Vec2,
    motion: glam::Vec2,
    data: &PointerData,
    button: i16,
) -> PointerInput {
    PointerInput {
        x: at.x,
        y: at.y,
        dx: motion.x,
        dy: motion.y,
        buttons: data.buttons,
        pointer_id: u32::try_from(data.id).unwrap_or(u32::MAX),
        pointer_type: match data.kind {
            PointerKind::Mouse => 0,
            PointerKind::Pen => 1,
            PointerKind::Touch => 2,
        },
        button,
        pressure: data.pressure,
        tilt_x: data.tilt_x,
        tilt_y: data.tilt_y,
        twist: data.twist,
    }
}

fn modifier_bits(m: &Modifiers) -> u16 {
    let mut bits = 0;
    if m.shift {
        bits |= modifiers::SHIFT;
    }
    if m.ctrl {
        bits |= modifiers::CTRL;
    }
    if m.alt {
        bits |= modifiers::ALT;
    }
    if m.super_ {
        bits |= modifiers::META;
    }
    bits
}

/// The DOM `key` for a key.
fn key_name(key: &Key) -> String {
    match key {
        Key::Character(s) => s.clone(),
        Key::Named(n) => match n {
            NamedKey::Tab => "Tab",
            NamedKey::Enter => "Enter",
            NamedKey::Escape => "Escape",
            NamedKey::Backspace => "Backspace",
            NamedKey::Space => " ",
            NamedKey::ArrowUp => "ArrowUp",
            NamedKey::ArrowDown => "ArrowDown",
            NamedKey::ArrowLeft => "ArrowLeft",
            NamedKey::ArrowRight => "ArrowRight",
            NamedKey::Home => "Home",
            NamedKey::End => "End",
            NamedKey::Delete => "Delete",
        }
        .to_string(),
    }
}

/// The DOM `button` number of a pointer button.
fn button_number(button: lumen_module::lumen_core::input::PointerButton) -> i16 {
    use lumen_module::lumen_core::input::PointerButton as B;
    match button {
        B::Primary => 0,
        B::Middle => 1,
        B::Secondary => 2,
        B::Back => 3,
        B::Forward => 4,
        B::Eraser => 5,
        B::Other(n) => i16::try_from(n).unwrap_or(i16::MAX),
    }
}

/// The messages forwarding reads, grouped so the system stays readable.
#[derive(bevy_ecs::system::SystemParam)]
pub struct InputReaders<'w, 's> {
    moves: MessageReader<'w, 's, PointerMoved>,
    presses: MessageReader<'w, 's, PointerPressed>,
    releases: MessageReader<'w, 's, PointerReleased>,
    wheels: MessageReader<'w, 's, MouseWheel>,
    keys: MessageReader<'w, 's, KeyPressed>,
    key_ups: MessageReader<'w, 's, KeyReleased>,
    ime: Option<MessageReader<'w, 's, ImeEvent>>,
    window_focus: Option<MessageReader<'w, 's, WindowFocused>>,
    locks: Option<MessageReader<'w, 's, PointerLockChanged>>,
}

/// What forwarding reads off each surface element.
type ElementData = (
    Entity,
    &'static SurfaceElement,
    Option<&'static ImageFit>,
    Option<&'static LumenAttributes>,
    Has<Hovered>,
    Has<Focused>,
);

/// The element state forwarding reads.
#[derive(bevy_ecs::system::SystemParam)]
pub struct Targets<'w, 's> {
    elements: Query<'w, 's, ElementData>,
    gained_focus: Query<'w, 's, Entity, (Added<Focused>, With<SurfaceElement>)>,
    lost_focus: RemovedComponents<'w, 's, Focused>,
    left: RemovedComponents<'w, 's, HoverWithin>,
    lock: Option<Res<'w, PointerLockState>>,
    accelerators: Option<Res<'w, MenuAccelerators>>,
    viewports: Viewports<'w, 's>,
}

/// Forward this tick's input on every surface to its producer.
pub fn forward_input(
    mut readers: InputReaders,
    mut targets: Targets,
    mut state: ResMut<ForwardState>,
) {
    let locked = targets.lock.as_ref().and_then(|l| l.owner);
    let hovered = targets
        .elements
        .iter()
        .find(|(.., hovered, _)| *hovered)
        .map(|(e, ..)| e);
    let focused = targets
        .elements
        .iter()
        .find(|(.., focused)| *focused)
        .map(|(e, ..)| e);

    // Focus first, so a press that focuses a surface arrives after it hears
    // it has focus.
    let gained: Vec<Entity> = targets.gained_focus.iter().collect();
    let lost: Vec<Entity> = targets.lost_focus.read().collect();
    let window_blur = readers
        .window_focus
        .as_mut()
        .is_some_and(|w| w.read().any(|f| !f.focused));
    for (entity, element, ..) in &targets.elements {
        if lost.contains(&entity) || (window_blur && focused == Some(entity)) {
            element.shared.send_input(InputEvent::Focus(false));
        }
        if gained.contains(&entity) {
            element.shared.send_input(InputEvent::Focus(true));
        }
    }
    if let Some(locks) = readers.locks.as_mut() {
        for change in locks.read() {
            for (entity, element, ..) in &targets.elements {
                if change.previous == Some(entity) {
                    element.shared.send_input(InputEvent::Lock(false));
                }
                if change.owner == Some(entity) {
                    element.shared.send_input(InputEvent::Lock(true));
                }
            }
        }
    }

    // The surface the primary pointer's events belong to: the one holding
    // the lock, else the one a press began on, else the one under it (not
    // one of its overlay children: those take the pointer first).
    let pointer_target = locked.or(state.pressed).or(hovered);
    let element_of = |entity: Entity| {
        targets
            .elements
            .get(entity)
            .ok()
            .map(|(_, element, fit, attrs, ..)| {
                (element, fit.copied().unwrap_or(ImageFit::Contain), attrs)
            })
    };
    let scale_of = |entity: Entity| targets.viewports.of(entity).map_or(1.0, |v| v.scale_factor);

    // Pointer input, in the order it happened.
    enum Step {
        Move(PointerMoved),
        Down(PointerPressed),
        Up(PointerReleased),
    }
    let mut steps: Vec<(u64, Step)> = Vec::new();
    steps.extend(
        readers
            .moves
            .read()
            .map(|m| (m.pointer.order, Step::Move(*m))),
    );
    steps.extend(
        readers
            .presses
            .read()
            .map(|p| (p.pointer.order, Step::Down(*p))),
    );
    steps.extend(
        readers
            .releases
            .read()
            .map(|r| (r.pointer.order, Step::Up(*r))),
    );
    steps.sort_by_key(|(order, _)| *order);
    for (_, step) in steps {
        let data = match &step {
            Step::Move(m) => m.pointer,
            Step::Down(p) => p.pointer,
            Step::Up(r) => r.pointer,
        };
        if !data.is_primary {
            continue;
        }
        let target = match &step {
            Step::Down(_) => locked.or(hovered),
            _ => pointer_target,
        };
        let Some(entity) = target else {
            continue;
        };
        let Some((element, fit, _)) = element_of(entity) else {
            continue;
        };
        let scale = scale_of(entity);
        let position = match &step {
            Step::Move(m) => m.position,
            Step::Down(p) => p.position,
            Step::Up(r) => r.position,
        };
        let Some(at) = to_texture(element, fit, scale, position) else {
            continue;
        };
        let motion = data.movement * motion_scale(element, fit, scale);
        let event = match step {
            Step::Move(_) => InputEvent::PointerMove(pointer_input(at, motion, &data, -1)),
            Step::Down(p) => {
                state.pressed = Some(entity);
                InputEvent::PointerDown(pointer_input(
                    at,
                    glam::Vec2::ZERO,
                    &data,
                    button_number(p.button),
                ))
            }
            Step::Up(r) => {
                if data.buttons == 0 {
                    state.pressed = None;
                }
                InputEvent::PointerUp(pointer_input(
                    at,
                    glam::Vec2::ZERO,
                    &data,
                    button_number(r.button),
                ))
            }
        };
        element.shared.send_input(event);
    }
    for wheel in readers.wheels.read() {
        let Some(entity) = locked.or(hovered) else {
            continue;
        };
        let Some((element, fit, _)) = element_of(entity) else {
            continue;
        };
        let Some(at) = to_texture(element, fit, scale_of(entity), wheel.position) else {
            continue;
        };
        element.shared.send_input(InputEvent::Wheel {
            x: at.x,
            y: at.y,
            dx: wheel.delta.x,
            dy: wheel.delta.y,
            unit: 0,
        });
    }
    for entity in targets.left.read() {
        if state.pressed == Some(entity) || locked == Some(entity) {
            continue;
        }
        if let Some((element, ..)) = element_of(entity) {
            element.shared.send_input(InputEvent::Leave);
        }
    }

    // Keys go to the focused surface, or the locked one.
    let Some(entity) = locked.or(focused) else {
        readers.keys.read().for_each(drop);
        readers.key_ups.read().for_each(drop);
        if let Some(ime) = readers.ime.as_mut() {
            ime.read().for_each(drop);
        }
        return;
    };
    let Some((element, _, attrs)) = element_of(entity) else {
        return;
    };
    let takes_all = locked == Some(entity) || attrs.and_then(|a| a.get("keyboard")) == Some("all");
    let accelerators = targets.accelerators.as_deref();
    let forwards = |key: &Key, mods: &Modifiers| -> bool {
        match key {
            // Escape ends a lock and is not the producer's; Tab moves focus.
            Key::Named(NamedKey::Escape) => locked != Some(entity),
            Key::Named(NamedKey::Tab) => takes_all,
            _ => takes_all || !accelerators.is_some_and(|a| a.matches(key, mods)),
        }
    };
    for key in readers.keys.read() {
        if !forwards(&key.key, &key.modifiers) {
            continue;
        }
        element.shared.send_input(InputEvent::Key {
            code: key.code.number(),
            pressed: true,
            repeat: key.repeat,
            modifiers: modifier_bits(&key.modifiers),
            key: key_name(&key.key),
        });
        // A printable key is text too, for a producer's own text fields.
        if let Key::Character(text) = &key.key
            && text.chars().count() == 1
            && !text.chars().any(char::is_control)
            && !key.modifiers.ctrl
            && !key.modifiers.super_
        {
            element.shared.send_input(InputEvent::Text(text.clone()));
        }
    }
    for key in readers.key_ups.read() {
        if !forwards(&key.key, &key.modifiers) {
            continue;
        }
        element.shared.send_input(InputEvent::Key {
            code: key.code.number(),
            pressed: false,
            repeat: false,
            modifiers: modifier_bits(&key.modifiers),
            key: key_name(&key.key),
        });
    }
    if let Some(ime) = readers.ime.as_mut() {
        for event in ime.read() {
            if let ImeEvent::Commit(text) = event {
                element.shared.send_input(InputEvent::Text(text.clone()));
            }
        }
    }
}
