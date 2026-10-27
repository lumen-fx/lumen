//! Dragging across the app's windows.
//!
//! A drag that starts in one window and may end in another, or outside every
//! window, needs the platform: only the window backend knows where the
//! pointer is once it leaves the window that was pressed, which of the app's
//! windows lies under it, and how to move a window along with it. This
//! module is the seam between whatever runs such a drag (a dock tearing a
//! tab off, an editor dragging an asset onto another window) and the backend
//! that carries it out. Nothing here names who drags or what is dragged.
//!
//! The dragging side writes [`SurfaceDragRequest`]s: it begins a drag in the
//! window the press happened in, asks for a window to follow the pointer
//! (and stop following), and ends it. The backend writes
//! [`SurfaceDragEvent`]s: which of the app's windows the pointer is over and
//! where, where it was dropped, and that the platform cancelled it. A
//! backend that carries the seam inserts [`SurfaceDragSupport`], which also
//! says whether it can make a window follow the pointer; an app with no such
//! resource has no cross-window drag, and an in-window drag reads the
//! ordinary pointer messages instead.
//!
//! Where the backend tracks the pointer itself (the headless stand-in, and a
//! desktop that keeps sending a pressed window the pointer's moves past its
//! edge: Windows, macOS, X11), [`InProcessDrag`] does the bookkeeping: given
//! where each window sits on the desktop, it turns a pointer move into the
//! event to write and the place the following window goes. Content dragged
//! to or from other applications is a different thing and stays with the
//! OS drag and drop capability.

use bevy_ecs::prelude::*;
use glam::{IVec2, UVec2, Vec2};

/// What the side running a drag asks of the window backend.
#[derive(Message, Clone, Copy, Debug, PartialEq)]
pub enum SurfaceDragRequest {
    /// A drag begins in `source`, the window the pointer was pressed in,
    /// while the button is still held. `follow` names a window to move with
    /// the pointer from the start, kept `grab_offset` logical pixels from the
    /// pointer (the point of the window that was grabbed).
    Begin {
        /// The window the press happened in.
        source: Entity,
        /// A window that follows the pointer.
        follow: Option<Entity>,
        /// Where in the following window the pointer holds it, in that
        /// window's logical pixels from its top-left corner.
        grab_offset: Vec2,
    },
    /// Make `Some(window)` follow the pointer from now on, at the begin
    /// request's grab offset, or stop any window following with `None`. A
    /// window that has not opened yet opens under the pointer without taking
    /// focus, so the drag keeps the keyboard.
    Follow(Option<Entity>),
    /// The drag is over; the backend stops reporting it. Written after a
    /// drop or a cancel the dragging side handled, or to abandon a drag
    /// (Escape).
    End,
}

/// What the window backend reports about a drag that crosses windows.
#[derive(Message, Clone, Copy, Debug, PartialEq)]
pub enum SurfaceDragEvent {
    /// The pointer moved. `window` is the app window under it, never the
    /// one following it; `position` is in that window's logical pixels.
    /// With `window` `None` the pointer is over none of the app's windows
    /// and `position` is in the source window's logical pixels, outside
    /// its client area, or the last place it was known where the platform
    /// stops reporting the pointer away from the app (Wayland).
    Over {
        /// The app window under the pointer.
        window: Option<Entity>,
        /// Where, as described above.
        position: Vec2,
    },
    /// The button was released: the same two fields as
    /// [`SurfaceDragEvent::Over`], for where it happened. The drag is over.
    Dropped {
        /// The app window under the pointer.
        window: Option<Entity>,
        /// Where, as for [`SurfaceDragEvent::Over`].
        position: Vec2,
    },
    /// The platform ended the drag without a drop (Escape on a compositor
    /// that handles it, the pointer taken away). The drag is over.
    Cancelled,
}

/// Present when the window backend carries cross-window drags, inserted by
/// the backend.
#[derive(Resource, Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct SurfaceDragSupport {
    /// Whether a window can follow the pointer during a drag. A Wayland
    /// compositor without the toplevel-drag protocol cannot; the dragging
    /// side then opens what it would have dragged where the drop lands.
    pub follow: bool,
}

/// One of the app's windows on the desktop, as a backend sees it: where its
/// client area is in physical desktop pixels, and the scale it draws at.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct DesktopSurface {
    /// The window entity.
    pub window: Entity,
    /// The client area's top-left corner, physical desktop pixels.
    pub origin: IVec2,
    /// The client area's size, physical pixels.
    pub size: UVec2,
    /// Logical to physical multiplier.
    pub scale: f32,
    /// Whether it shows. A hidden window still places the pointer's moves
    /// reported in its coordinates, but the pointer is never over it.
    pub visible: bool,
}

impl DesktopSurface {
    /// Whether the physical desktop point `p` is inside the client area.
    pub fn contains(&self, p: IVec2) -> bool {
        let end = self.origin + self.size.as_ivec2();
        p.x >= self.origin.x && p.y >= self.origin.y && p.x < end.x && p.y < end.y
    }

    /// The desktop point `p` in this window's logical pixels.
    pub fn to_local(self, p: IVec2) -> Vec2 {
        (p - self.origin).as_vec2() / self.scale.max(0.01)
    }

    /// The point `local` (this window's logical pixels) on the desktop.
    pub fn to_desktop(self, local: Vec2) -> IVec2 {
        self.origin + (local * self.scale).round().as_ivec2()
    }
}

/// The window under the desktop point `p`: the first shown one of
/// `surfaces` that holds it, so the caller lists them top-most first.
/// `skip` is left out (the window following the pointer is always under
/// it).
pub fn surface_at(
    surfaces: &[DesktopSurface],
    p: IVec2,
    skip: Option<Entity>,
) -> Option<&DesktopSurface> {
    surfaces
        .iter()
        .filter(|s| s.visible && Some(s.window) != skip)
        .find(|s| s.contains(p))
}

/// A drag in progress, as [`InProcessDrag`] keeps it.
#[derive(Clone, Copy, Debug, PartialEq)]
struct Active {
    source: Entity,
    follow: Option<Entity>,
    grab_offset: Vec2,
}

/// The bookkeeping of a drag a backend tracks itself: it sees every pointer
/// move of the drag in the coordinates of some window of the app (the source
/// window keeps receiving them past its edge on Windows, macOS and X11), and
/// knows where every window sits on the desktop.
///
/// Feed it the requests ([`Self::request`]), then each move
/// ([`Self::moved`]) and the release ([`Self::released`]); write the events
/// they return and move the following window where [`Self::moved`] says.
#[derive(Clone, Debug, Default)]
pub struct InProcessDrag {
    active: Option<Active>,
}

/// What a pointer move during a drag comes to.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct DragStep {
    /// The event to write.
    pub event: SurfaceDragEvent,
    /// The following window and where its client area's top-left corner
    /// goes, in physical desktop pixels.
    pub follow_to: Option<(Entity, IVec2)>,
}

impl InProcessDrag {
    /// Take in one request.
    pub fn request(&mut self, request: &SurfaceDragRequest) {
        match *request {
            SurfaceDragRequest::Begin {
                source,
                follow,
                grab_offset,
            } => {
                self.active = Some(Active {
                    source,
                    follow,
                    grab_offset,
                });
            }
            SurfaceDragRequest::Follow(window) => {
                if let Some(a) = self.active.as_mut() {
                    a.follow = window;
                }
            }
            SurfaceDragRequest::End => self.active = None,
        }
    }

    /// Whether a drag is in progress.
    pub fn is_active(&self) -> bool {
        self.active.is_some()
    }

    /// The window the drag began in.
    pub fn source(&self) -> Option<Entity> {
        self.active.map(|a| a.source)
    }

    /// The window following the pointer.
    pub fn follow(&self) -> Option<Entity> {
        self.active.and_then(|a| a.follow)
    }

    /// Where the pointer is, given `local` in the logical pixels of the
    /// window `window` reported it in: the desktop point, the app window
    /// under it, and the position the events carry.
    fn locate(
        &self,
        surfaces: &[DesktopSurface],
        window: Entity,
        local: Vec2,
    ) -> Option<(IVec2, Option<Entity>, Vec2)> {
        let a = self.active?;
        let from = surfaces.iter().find(|s| s.window == window)?;
        let p = from.to_desktop(local);
        match surface_at(surfaces, p, a.follow) {
            Some(under) => Some((p, Some(under.window), under.to_local(p))),
            None => {
                let source = surfaces
                    .iter()
                    .find(|s| s.window == a.source)
                    .unwrap_or(from);
                Some((p, None, source.to_local(p)))
            }
        }
    }

    /// The pointer moved to `local` in `window`'s logical pixels. `None`
    /// when no drag is in progress or `window` is not among `surfaces`.
    pub fn moved(
        &self,
        surfaces: &[DesktopSurface],
        window: Entity,
        local: Vec2,
    ) -> Option<DragStep> {
        let (p, under, position) = self.locate(surfaces, window, local)?;
        let a = self.active?;
        let follow_to = a.follow.map(|f| {
            let scale = surfaces
                .iter()
                .find(|s| s.window == f)
                .map_or(1.0, |s| s.scale);
            (f, p - (a.grab_offset * scale).round().as_ivec2())
        });
        Some(DragStep {
            event: SurfaceDragEvent::Over {
                window: under,
                position,
            },
            follow_to,
        })
    }

    /// The button was released at `local` in `window`'s logical pixels.
    /// Ends the drag.
    pub fn released(
        &mut self,
        surfaces: &[DesktopSurface],
        window: Entity,
        local: Vec2,
    ) -> Option<SurfaceDragEvent> {
        let located = self.locate(surfaces, window, local);
        self.active.take()?;
        let (_, under, position) = located?;
        Some(SurfaceDragEvent::Dropped {
            window: under,
            position,
        })
    }

    /// The platform took the pointer away. Ends the drag.
    pub fn cancelled(&mut self) -> Option<SurfaceDragEvent> {
        self.active.take().map(|_| SurfaceDragEvent::Cancelled)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn surfaces(world: &mut World) -> (Vec<DesktopSurface>, Entity, Entity, Entity) {
        let a = world.spawn_empty().id();
        let b = world.spawn_empty().id();
        let f = world.spawn_empty().id();
        let list = vec![
            // Top-most first: the following window, then b over a.
            DesktopSurface {
                window: f,
                origin: IVec2::new(150, 150),
                size: UVec2::new(100, 100),
                scale: 2.0,
                visible: true,
            },
            DesktopSurface {
                window: b,
                origin: IVec2::new(900, 100),
                size: UVec2::new(400, 300),
                scale: 1.0,
                visible: true,
            },
            DesktopSurface {
                window: a,
                origin: IVec2::new(100, 100),
                size: UVec2::new(1600, 1200),
                scale: 2.0,
                visible: true,
            },
        ];
        (list, a, b, f)
    }

    #[test]
    fn a_surface_converts_between_desktop_and_logical_pixels() {
        let mut world = World::new();
        let (list, a, _, _) = surfaces(&mut world);
        let s = list.iter().find(|s| s.window == a).unwrap();
        assert!(s.contains(IVec2::new(100, 100)));
        assert!(!s.contains(IVec2::new(1700, 100)));
        assert_eq!(s.to_local(IVec2::new(300, 500)), Vec2::new(100.0, 200.0));
        assert_eq!(s.to_desktop(Vec2::new(100.0, 200.0)), IVec2::new(300, 500));
        assert!(surface_at(&list, IVec2::new(-5, -5), None).is_none());
        let mut hidden = list.clone();
        hidden[2].visible = false;
        assert!(surface_at(&hidden, IVec2::new(300, 500), None).is_none());
        let mut drag = InProcessDrag::default();
        drag.request(&SurfaceDragRequest::Begin {
            source: a,
            follow: None,
            grab_offset: Vec2::ZERO,
        });
        assert_eq!(
            drag.moved(&hidden, a, Vec2::new(100.0, 200.0))
                .map(|s| s.event),
            Some(SurfaceDragEvent::Over {
                window: None,
                position: Vec2::new(100.0, 200.0)
            }),
            "a hidden window still places a move reported in it"
        );
    }

    #[test]
    fn moves_report_the_window_under_the_pointer_and_carry_the_follower() {
        let mut world = World::new();
        let (list, a, b, f) = surfaces(&mut world);
        let mut drag = InProcessDrag::default();
        assert!(drag.moved(&list, a, Vec2::ZERO).is_none(), "no drag yet");
        drag.request(&SurfaceDragRequest::Begin {
            source: a,
            follow: None,
            grab_offset: Vec2::new(10.0, 5.0),
        });
        assert!(drag.is_active());
        assert_eq!(drag.source(), Some(a));
        // Over a, in a's own coordinates.
        let step = drag.moved(&list, a, Vec2::new(300.0, 300.0)).unwrap();
        assert_eq!(
            step.event,
            SurfaceDragEvent::Over {
                window: Some(a),
                position: Vec2::new(300.0, 300.0)
            }
        );
        assert_eq!(step.follow_to, None);
        // Before it follows, the window that will is a window like any.
        let step = drag.moved(&list, a, Vec2::new(50.0, 50.0)).unwrap();
        assert_eq!(
            step.event,
            SurfaceDragEvent::Over {
                window: Some(f),
                position: Vec2::new(25.0, 25.0)
            }
        );
        // Past a's edge and over b, which lies on top: b's coordinates.
        let step = drag.moved(&list, a, Vec2::new(410.0, 10.0)).unwrap();
        assert_eq!(
            step.event,
            SurfaceDragEvent::Over {
                window: Some(b),
                position: Vec2::new(20.0, 20.0)
            }
        );
        // Off every window: a's coordinates, outside it.
        let step = drag.moved(&list, a, Vec2::new(-30.0, 10.0)).unwrap();
        assert_eq!(
            step.event,
            SurfaceDragEvent::Over {
                window: None,
                position: Vec2::new(-30.0, 10.0)
            }
        );
        // A follower is never the window under the pointer, and its corner
        // trails the pointer by the grab offset at its own scale.
        drag.request(&SurfaceDragRequest::Follow(Some(f)));
        assert_eq!(drag.follow(), Some(f));
        let step = drag.moved(&list, a, Vec2::new(50.0, 50.0)).unwrap();
        assert_eq!(
            step.event,
            SurfaceDragEvent::Over {
                window: Some(a),
                position: Vec2::new(50.0, 50.0)
            }
        );
        assert_eq!(step.follow_to, Some((f, IVec2::new(180, 190))));
        // A move reported in another window converts through that window.
        let step = drag.moved(&list, b, Vec2::new(20.0, 20.0)).unwrap();
        assert_eq!(
            step.event,
            SurfaceDragEvent::Over {
                window: Some(b),
                position: Vec2::new(20.0, 20.0)
            }
        );
        assert!(
            drag.moved(&list, world.spawn_empty().id(), Vec2::ZERO)
                .is_none()
        );
        drag.request(&SurfaceDragRequest::Follow(None));
        assert_eq!(drag.follow(), None);
        // The release drops where it happens and ends the drag.
        assert_eq!(
            drag.released(&list, a, Vec2::new(410.0, 10.0)),
            Some(SurfaceDragEvent::Dropped {
                window: Some(b),
                position: Vec2::new(20.0, 20.0)
            })
        );
        assert!(!drag.is_active());
        assert_eq!(drag.released(&list, a, Vec2::ZERO), None);
        assert_eq!(drag.cancelled(), None);
        drag.request(&SurfaceDragRequest::Follow(Some(f)));
        assert_eq!(drag.follow(), None, "no drag to follow");
    }

    #[test]
    fn a_cancel_or_an_end_stops_the_drag() {
        let mut world = World::new();
        let (_, a, _, _) = surfaces(&mut world);
        let mut drag = InProcessDrag::default();
        let begin = SurfaceDragRequest::Begin {
            source: a,
            follow: None,
            grab_offset: Vec2::ZERO,
        };
        drag.request(&begin);
        assert_eq!(drag.cancelled(), Some(SurfaceDragEvent::Cancelled));
        assert!(!drag.is_active());
        drag.request(&begin);
        drag.request(&SurfaceDragRequest::End);
        assert!(!drag.is_active());
    }
}
