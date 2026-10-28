//! Turning winit's mouse, touch and pen input into Lumen pointer messages.
//!
//! winit 0.30 reports the mouse as `CursorMoved` / `MouseInput` and every
//! contact a screen or tablet makes as `Touch`, with a force where the
//! platform measures one. What each desktop gives:
//!
//! - Windows: touch and pen both arrive as `Touch`, with pressure. The pen's
//!   tilt, twist, barrel button and eraser end are read from the system for
//!   the pointer winit names, so a pen reports them all.
//! - macOS: no `Touch` at all; a tablet pen drives the mouse, with no
//!   pressure or tilt.
//! - Linux (X11 and Wayland): touch screens arrive as `Touch`, without
//!   pressure; a tablet pen drives the mouse, with no pressure or tilt.
//!
//! [`PointerTracker`] keeps what the messages need between events: the mouse
//! buttons held, and each contact's pointer id, kind and whether it is the
//! primary pointer.

use glam::Vec2;
use lumen_core::input::{PointerButton, PointerData, PointerKind};
use winit::event::{Force, TouchPhase};

/// A pen's readings for one contact, beyond the position and pressure winit
/// reports.
#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub(crate) struct PenSample {
    /// Tilt toward the right, degrees.
    pub tilt_x: f32,
    /// Tilt toward the user, degrees.
    pub tilt_y: f32,
    /// Rotation about the pen's axis, degrees.
    pub twist: f32,
    /// The eraser end is the one in contact or in range.
    pub eraser: bool,
    /// The barrel button is held.
    pub barrel: bool,
}

/// One pointer message for the window to write.
#[derive(Clone, Copy, Debug, PartialEq)]
pub(crate) enum PointerOutput {
    /// The pointer moved, or a pen in range hovered.
    Moved(Vec2, PointerData),
    /// A contact began.
    Pressed(Vec2, PointerButton, PointerData),
    /// A contact ended.
    Released(Vec2, PointerButton, PointerData),
    /// The system took the contact away.
    Cancelled(Vec2, PointerData),
}

impl PointerOutput {
    /// The pointer the message is about.
    pub fn data(&self) -> PointerData {
        match *self {
            PointerOutput::Moved(_, d)
            | PointerOutput::Pressed(_, _, d)
            | PointerOutput::Released(_, _, d)
            | PointerOutput::Cancelled(_, d) => d,
        }
    }
}

/// A touch or pen contact being tracked.
#[derive(Clone, Copy, Debug)]
struct Contact {
    /// The id winit gives it.
    winit_id: u64,
    /// The pointer id handed to scripts.
    data: PointerData,
    /// The button the contact went down with.
    button: PointerButton,
    /// Touching (a pen can be tracked while it hovers).
    down: bool,
}

/// The pointer state carried between winit events.
#[derive(Debug)]
pub(crate) struct PointerTracker {
    /// Mouse buttons held, as a web `buttons` mask.
    mouse_buttons: u32,
    /// Live touch and pen contacts.
    contacts: Vec<Contact>,
    /// The next pointer id to hand out; the mouse keeps
    /// [`PointerData::MOUSE_ID`].
    next_id: u64,
}

impl Default for PointerTracker {
    fn default() -> Self {
        Self {
            mouse_buttons: 0,
            contacts: Vec::new(),
            next_id: PointerData::MOUSE_ID + 1,
        }
    }
}

/// Pressure from a winit force, `0.0` to `1.0`.
fn pressure_of(force: Force) -> f32 {
    match force {
        Force::Normalized(f) => f as f32,
        Force::Calibrated {
            force,
            max_possible_force,
            ..
        } => {
            if max_possible_force > 0.0 {
                (force / max_possible_force) as f32
            } else {
                0.0
            }
        }
    }
    .clamp(0.0, 1.0)
}

impl PointerTracker {
    /// The mouse as it is now.
    pub fn mouse(&self) -> PointerData {
        PointerData::mouse(self.mouse_buttons)
    }

    /// The mouse buttons held now.
    pub fn held_mouse_buttons(&self) -> Vec<PointerButton> {
        [
            PointerButton::Primary,
            PointerButton::Secondary,
            PointerButton::Middle,
            PointerButton::Back,
            PointerButton::Forward,
        ]
        .into_iter()
        .filter(|b| self.mouse_buttons & b.bit() != 0)
        .collect()
    }

    /// A mouse button went down or up; the mouse as it is after.
    pub fn mouse_button(&mut self, button: PointerButton, pressed: bool) -> PointerData {
        if pressed {
            self.mouse_buttons |= button.bit();
        } else {
            self.mouse_buttons &= !button.bit();
        }
        self.mouse()
    }

    /// A winit `Touch` event at `position` (logical pixels). `pen` carries
    /// the pen readings when the contact is a pen; `None` is a finger.
    pub fn touch(
        &mut self,
        phase: TouchPhase,
        winit_id: u64,
        position: Vec2,
        force: Option<Force>,
        pen: Option<PenSample>,
    ) -> Option<PointerOutput> {
        let index = self.contacts.iter().position(|c| c.winit_id == winit_id);
        let index = match (index, phase) {
            (Some(i), _) => i,
            // A contact starts, or a pen comes into range hovering.
            (None, TouchPhase::Started) | (None, TouchPhase::Moved) if pen.is_some() => {
                self.add(winit_id, PointerKind::Pen)
            }
            (None, TouchPhase::Started) => self.add(winit_id, PointerKind::Touch),
            // A finger winit never said began.
            (None, _) => return None,
        };
        let contact = &mut self.contacts[index];
        if let Some(p) = pen {
            contact.data.tilt_x = p.tilt_x;
            contact.data.tilt_y = p.tilt_y;
            contact.data.twist = p.twist;
        }
        let output = match phase {
            TouchPhase::Started => {
                contact.button = if pen.is_some_and(|p| p.eraser) {
                    PointerButton::Eraser
                } else {
                    PointerButton::Primary
                };
                contact.down = true;
                contact.data.buttons = contact.button.bit()
                    | if pen.is_some_and(|p| p.barrel) {
                        PointerButton::Secondary.bit()
                    } else {
                        0
                    };
                contact.data.pressure = force
                    .map(pressure_of)
                    .unwrap_or_else(|| PointerData::default_pressure(contact.data.buttons));
                PointerOutput::Pressed(position, contact.button, contact.data)
            }
            TouchPhase::Moved => {
                if let Some(p) = pen {
                    let barrel = if p.barrel {
                        PointerButton::Secondary.bit()
                    } else {
                        0
                    };
                    let tip = if contact.down {
                        contact.button.bit()
                    } else {
                        0
                    };
                    contact.data.buttons = tip | barrel;
                }
                contact.data.pressure = match force {
                    Some(f) if contact.down => pressure_of(f),
                    _ => PointerData::default_pressure(contact.data.buttons),
                };
                PointerOutput::Moved(position, contact.data)
            }
            TouchPhase::Ended | TouchPhase::Cancelled => {
                contact.down = false;
                contact.data.buttons = 0;
                contact.data.pressure = 0.0;
                let data = contact.data;
                let button = contact.button;
                let pen_stays = data.kind == PointerKind::Pen && phase == TouchPhase::Ended;
                if !pen_stays {
                    self.contacts.remove(index);
                }
                return Some(if phase == TouchPhase::Ended {
                    PointerOutput::Released(position, button, data)
                } else {
                    PointerOutput::Cancelled(position, data)
                });
            }
        };
        Some(output)
    }

    /// Start tracking a contact; the first finger down while no other is,
    /// and every pen, is primary.
    fn add(&mut self, winit_id: u64, kind: PointerKind) -> usize {
        let is_primary = kind == PointerKind::Pen
            || !self
                .contacts
                .iter()
                .any(|c| c.data.kind == PointerKind::Touch && c.data.is_primary);
        let id = self.next_id;
        self.next_id += 1;
        self.contacts.push(Contact {
            winit_id,
            data: PointerData {
                id,
                kind,
                is_primary,
                ..PointerData::mouse(0)
            },
            button: PointerButton::Primary,
            down: false,
        });
        self.contacts.len() - 1
    }
}

/// The pen readings for the pointer winit names `winit_id`, when that
/// pointer is a pen. winit forwards each `WM_POINTER` message while the
/// window procedure is still handling it, so the system answers for the
/// sample being reported.
#[cfg(target_os = "windows")]
pub(crate) fn pen_sample(winit_id: u64) -> Option<PenSample> {
    use windows_sys::Win32::UI::Input::Pointer::{
        GetPointerPenInfo, GetPointerType, POINTER_PEN_INFO,
    };
    use windows_sys::Win32::UI::WindowsAndMessaging::{
        PEN_FLAG_BARREL, PEN_FLAG_ERASER, PEN_FLAG_INVERTED, PEN_MASK_ROTATION, PEN_MASK_TILT_X,
        PEN_MASK_TILT_Y, PT_PEN,
    };

    let id = u32::try_from(winit_id).ok()?;
    let mut kind = 0;
    // SAFETY: `kind` is a valid out pointer for the call's duration.
    if unsafe { GetPointerType(id, &mut kind) } == 0 || kind != PT_PEN {
        return None;
    }
    // SAFETY: POINTER_PEN_INFO is plain data; all-zero is a valid value.
    let mut info: POINTER_PEN_INFO = unsafe { std::mem::zeroed() };
    // SAFETY: `info` is a valid out pointer for the call's duration.
    if unsafe { GetPointerPenInfo(id, &mut info) } == 0 {
        return Some(PenSample::default());
    }
    let has = |mask: u32| info.penMask & mask != 0;
    Some(PenSample {
        tilt_x: if has(PEN_MASK_TILT_X) {
            info.tiltX as f32
        } else {
            0.0
        },
        tilt_y: if has(PEN_MASK_TILT_Y) {
            info.tiltY as f32
        } else {
            0.0
        },
        twist: if has(PEN_MASK_ROTATION) {
            info.rotation as f32
        } else {
            0.0
        },
        eraser: info.penFlags & (PEN_FLAG_ERASER | PEN_FLAG_INVERTED) != 0,
        barrel: info.penFlags & PEN_FLAG_BARREL != 0,
    })
}

/// No platform outside Windows tells a pen from a finger through winit.
#[cfg(not(target_os = "windows"))]
pub(crate) fn pen_sample(_winit_id: u64) -> Option<PenSample> {
    None
}

#[cfg(test)]
mod tests {
    use super::*;

    const AT: Vec2 = Vec2::new(10.0, 20.0);

    #[test]
    fn the_mouse_reports_the_buttons_it_holds() {
        let mut t = PointerTracker::default();
        let d = t.mouse_button(PointerButton::Primary, true);
        assert_eq!((d.id, d.kind, d.buttons), (1, PointerKind::Mouse, 1));
        assert_eq!(d.pressure, 0.5, "no sensor: 0.5 while a button is held");
        let d = t.mouse_button(PointerButton::Secondary, true);
        assert_eq!(d.buttons, 3);
        assert_eq!(
            t.held_mouse_buttons(),
            vec![PointerButton::Primary, PointerButton::Secondary]
        );
        let d = t.mouse_button(PointerButton::Primary, false);
        assert_eq!(d.buttons, 2);
        let d = t.mouse_button(PointerButton::Secondary, false);
        assert_eq!((d.buttons, d.pressure), (0, 0.0));
        assert!(t.held_mouse_buttons().is_empty());
    }

    #[test]
    fn the_first_finger_is_primary_and_the_second_is_not() {
        let mut t = PointerTracker::default();
        let a = t
            .touch(
                TouchPhase::Started,
                7,
                AT,
                Some(Force::Normalized(0.25)),
                None,
            )
            .unwrap();
        let b = t.touch(TouchPhase::Started, 8, AT, None, None).unwrap();
        let (a, b) = (a.data(), b.data());
        assert_eq!(a.kind, PointerKind::Touch);
        assert!(a.is_primary && !b.is_primary);
        assert_ne!(a.id, b.id);
        assert_ne!(a.id, PointerData::MOUSE_ID);
        assert_eq!(a.pressure, 0.25);
        assert_eq!(b.pressure, 0.5, "no force: the web default");
        assert_eq!(a.buttons, 1);

        // The ids hold for the contact's life; a lift releases with no
        // buttons, and the next finger alone is primary again.
        let moved = t.touch(TouchPhase::Moved, 7, AT, None, None).unwrap();
        assert_eq!(moved.data().id, a.id);
        let up = t.touch(TouchPhase::Ended, 7, AT, None, None).unwrap();
        assert!(
            matches!(up, PointerOutput::Released(_, PointerButton::Primary, d) if d.buttons == 0)
        );
        t.touch(TouchPhase::Cancelled, 8, AT, None, None).unwrap();
        let c = t.touch(TouchPhase::Started, 9, AT, None, None).unwrap();
        assert!(c.data().is_primary);
        assert!(
            t.touch(TouchPhase::Moved, 42, AT, None, None).is_none(),
            "a finger winit never began is dropped"
        );
    }

    #[test]
    fn a_pen_reports_tilt_twist_and_its_eraser() {
        let mut t = PointerTracker::default();
        let hover = PenSample {
            tilt_x: 30.0,
            tilt_y: -10.0,
            twist: 90.0,
            ..PenSample::default()
        };
        // In range, hovering: a move with no buttons.
        let m = t
            .touch(
                TouchPhase::Moved,
                3,
                AT,
                Some(Force::Normalized(0.0)),
                Some(hover),
            )
            .unwrap();
        assert!(matches!(m, PointerOutput::Moved(..)));
        let d = m.data();
        assert_eq!((d.kind, d.buttons, d.pressure), (PointerKind::Pen, 0, 0.0));
        assert_eq!((d.tilt_x, d.tilt_y, d.twist), (30.0, -10.0, 90.0));
        assert!(d.is_primary);

        // The eraser end touches: button 5, buttons 32.
        let eraser = PenSample {
            eraser: true,
            ..hover
        };
        let down = t
            .touch(
                TouchPhase::Started,
                3,
                AT,
                Some(Force::Normalized(0.75)),
                Some(eraser),
            )
            .unwrap();
        let PointerOutput::Pressed(_, button, d) = down else {
            panic!("a press")
        };
        assert_eq!(button, PointerButton::Eraser);
        assert_eq!((button.code(), d.buttons, d.pressure), (5, 32, 0.75));
        assert_eq!(d.id, m.data().id, "one pen, one id");

        // The pen lifts but stays in range, keeping its id.
        t.touch(TouchPhase::Ended, 3, AT, None, Some(hover))
            .unwrap();
        let again = t
            .touch(TouchPhase::Moved, 3, AT, None, Some(hover))
            .unwrap();
        assert_eq!(again.data().id, d.id);
    }
}
