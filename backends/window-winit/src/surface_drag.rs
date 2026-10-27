//! The window backend's side of a drag that crosses the app's windows (the
//! core's surface-drag seam).
//!
//! On Windows, macOS and X11 the window that was pressed keeps receiving the
//! pointer's moves past its edge until the release (Windows captures the
//! mouse, AppKit sends the drag to the window it began in, X11 keeps the
//! implicit grab). Each move converts to the desktop through that window's
//! inner position; the window under the point is found from every window's
//! rectangle, the most recently focused first; a window that follows the
//! pointer is moved with `set_outer_position` right in the event handler, at
//! input rate. A window asked to follow before it exists opens inactive,
//! under the pointer, so the drag keeps the keyboard.
//!
//! On Wayland no client may learn where the pointer is outside its own
//! surfaces or place its windows, so the drag runs as a `wl_data_device`
//! drag with a MIME type private to this process ([`wayland`]).

use std::collections::HashSet;

use bevy_ecs::prelude::*;
use glam::{IVec2, UVec2, Vec2};
use lumen_core::surface_drag::{
    DesktopSurface, InProcessDrag, SurfaceDragEvent, SurfaceDragRequest,
};
use winit::window::Window;

/// What the backend keeps of a drag across windows.
#[derive(Default)]
pub(crate) struct DragSeam {
    /// The drag, where the backend tracks the pointer itself.
    pub inner: InProcessDrag,
    /// Windows the app asked to follow before they opened: they open
    /// inactive, under the pointer.
    pub open_following: HashSet<Entity>,
    /// The window entities, most recently focused first.
    pub order: Vec<Entity>,
    /// The pointer's last desktop position during a drag, physical pixels.
    pub last_desktop: Option<IVec2>,
    /// The Wayland drag, when the app runs on Wayland and the compositor
    /// offers a data device.
    #[cfg(target_os = "linux")]
    pub wayland: Option<wayland::WaylandDrag>,
}

impl DragSeam {
    /// Whether the drag runs through Wayland's data device rather than the
    /// pointer moves winit reports.
    pub fn on_wayland(&self) -> bool {
        #[cfg(target_os = "linux")]
        {
            self.wayland.is_some()
        }
        #[cfg(not(target_os = "linux"))]
        {
            false
        }
    }

    /// Put `window` first in the stacking order the drag reads.
    pub fn raised(&mut self, window: Entity) {
        self.order.retain(|&w| w != window);
        self.order.insert(0, window);
    }

    /// Forget a closed window.
    pub fn closed(&mut self, window: Entity) {
        self.order.retain(|&w| w != window);
        self.open_following.remove(&window);
    }

    /// Take in a request the app wrote.
    pub fn request(&mut self, request: &SurfaceDragRequest, opened: &dyn Fn(Entity) -> bool) {
        if let SurfaceDragRequest::Follow(Some(w)) = request
            && !opened(*w)
        {
            self.open_following.insert(*w);
        }
        if let SurfaceDragRequest::Begin {
            follow: Some(w), ..
        } = request
            && !opened(*w)
        {
            self.open_following.insert(*w);
        }
        if matches!(request, SurfaceDragRequest::End) {
            self.open_following.clear();
            self.last_desktop = None;
        }
        self.inner.request(request);
    }

    /// The pointer moved to `local` in `window`; `surfaces` lists every
    /// window top-most first. Returns the event to write and where to put
    /// the following window's client area.
    pub fn moved(
        &mut self,
        surfaces: &[DesktopSurface],
        window: Entity,
        local: Vec2,
    ) -> Option<(SurfaceDragEvent, Option<(Entity, IVec2)>)> {
        if let Some(s) = surfaces.iter().find(|s| s.window == window) {
            self.last_desktop = Some(s.to_desktop(local));
        }
        let step = self.inner.moved(surfaces, window, local)?;
        Some((step.event, step.follow_to))
    }

    /// Where a window opening to follow the pointer goes: its client
    /// area's corner, `grab` logical pixels up and left of the pointer.
    pub fn open_at(&self, grab: Vec2, scale: f32) -> Option<IVec2> {
        self.last_desktop
            .map(|p| p - (grab * scale).round().as_ivec2())
    }
}

/// `window` on the desktop: its client area, scale and whether it shows.
pub(crate) fn desktop_surface(
    entity: Entity,
    window: &Window,
    visible: bool,
) -> Option<DesktopSurface> {
    let origin = window.inner_position().ok()?;
    let size = window.inner_size();
    Some(DesktopSurface {
        window: entity,
        origin: IVec2::new(origin.x, origin.y),
        size: UVec2::new(size.width, size.height),
        scale: window.scale_factor() as f32,
        visible,
    })
}

/// Move `window` so its client area's top-left corner is at `client`
/// (physical desktop pixels).
pub(crate) fn place_client_area(window: &Window, client: IVec2) {
    let offset = match (window.inner_position(), window.outer_position()) {
        (Ok(inner), Ok(outer)) => IVec2::new(inner.x - outer.x, inner.y - outer.y),
        _ => IVec2::ZERO,
    };
    let to = client - offset;
    window.set_outer_position(winit::dpi::PhysicalPosition::new(to.x, to.y));
}

/// Order `surfaces` top-most first by `order` (most recently focused
/// first); a window never focused goes after, newest first.
pub(crate) fn stack(mut surfaces: Vec<DesktopSurface>, order: &[Entity]) -> Vec<DesktopSurface> {
    surfaces.sort_by(|a, b| {
        let rank = |e: Entity| order.iter().position(|&o| o == e).unwrap_or(usize::MAX);
        rank(a.window)
            .cmp(&rank(b.window))
            .then(b.window.cmp(&a.window))
    });
    surfaces
}

#[cfg(target_os = "linux")]
pub(crate) mod wayland {
    //! The drag on Wayland: a `wl_data_device` drag on winit's own
    //! connection, with a MIME type private to the process, so the app's
    //! own surfaces report where the pointer is and nothing outside the app
    //! accepts the drop.
    //!
    //! The module binds a seat and a pointer of its own beside winit's, for
    //! one reason: the serial of the press that starts the drag, which
    //! winit does not hand out. It starts the data-device drag when the app
    //! begins one (the button is still held, so the press serial is valid);
    //! from then on winit sees the pointer leave and the module reports the
    //! drag from the data device: `enter` and `motion` on the app's
    //! surfaces become `Over`, `leave` becomes `Over` with no window, and a
    //! `drop` becomes `Dropped`. A compositor that offers
    //! `xdg_toplevel_drag_v1` moves a following window itself; one that does
    //! not cannot make a window follow (`SurfaceDragSupport::follow` is
    //! `false`).
    //!
    //! Compositors differ in the details (measured on Hyprland 0.56): the
    //! `enter` position is not where the pointer is, so the first `motion`
    //! is used instead; a release over the desktop arrives as a `drop` on
    //! the last surface entered, outside its bounds, which is read as a drop
    //! on no window; Escape cancels the drag in the compositor and then
    //! reaches the app as a key. A `cancelled` source with no drop is
    //! therefore a cancel when an Escape press follows it promptly, and a
    //! drop on no window otherwise.

    use std::time::{Duration, Instant};

    use bevy_ecs::prelude::Entity;
    use glam::Vec2;
    use lumen_core::surface_drag::{SurfaceDragEvent, SurfaceDragRequest};
    use wayland_backend::client::{Backend, ObjectId};
    use wayland_client::protocol::{
        wl_data_device, wl_data_device_manager, wl_data_offer, wl_data_source, wl_pointer,
        wl_registry, wl_seat, wl_surface,
    };
    use wayland_client::{Connection, Dispatch, EventQueue, Proxy, QueueHandle, WEnum};
    use wayland_protocols::xdg::shell::client::xdg_toplevel;
    use wayland_protocols::xdg::toplevel_drag::v1::client::{
        xdg_toplevel_drag_manager_v1, xdg_toplevel_drag_v1,
    };

    /// How long after a `cancelled` with no drop an Escape press may still
    /// arrive and make it a cancel.
    const ESCAPE_GRACE: Duration = Duration::from_millis(60);

    /// One of the app's windows as the Wayland drag sees it.
    #[derive(Clone, Debug)]
    pub struct Surface {
        /// The window entity.
        pub window: Entity,
        /// Its `wl_surface`, from the raw window handle.
        pub surface: *mut std::ffi::c_void,
        /// Its `xdg_toplevel`, from winit.
        pub toplevel: Option<*mut std::ffi::c_void>,
        /// Its size in logical pixels (surface coordinates).
        pub size: Vec2,
    }

    #[derive(Default)]
    struct State {
        seat: Option<wl_seat::WlSeat>,
        ddm: Option<wl_data_device_manager::WlDataDeviceManager>,
        tdm: Option<xdg_toplevel_drag_manager_v1::XdgToplevelDragManagerV1>,
        pointer: Option<wl_pointer::WlPointer>,
        device: Option<wl_data_device::WlDataDevice>,
        /// The serial of the last press our pointer saw, while held.
        press_serial: Option<u32>,
        /// The surfaces, by object id.
        surfaces: Vec<(ObjectId, Entity, Vec2)>,
        source: Option<wl_data_source::WlDataSource>,
        offer: Option<wl_data_offer::WlDataOffer>,
        /// The app surface the drag is over, and where.
        over: Option<Entity>,
        position: Vec2,
        /// Entered but no motion yet: the enter position is not trusted.
        fresh: bool,
        dropped: bool,
        cancelled_at: Option<Instant>,
        events: Vec<SurfaceDragEvent>,
        mime: String,
    }

    impl State {
        fn window_of(&self, surface: &wl_surface::WlSurface) -> Option<Entity> {
            let id = surface.id();
            self.surfaces
                .iter()
                .find(|(s, _, _)| *s == id)
                .map(|(_, w, _)| *w)
        }

        fn size_of(&self, window: Entity) -> Option<Vec2> {
            self.surfaces
                .iter()
                .find(|(_, w, _)| *w == window)
                .map(|(_, _, s)| *s)
        }

        /// The window the pointer is over: the surface entered, while the
        /// position lies inside it. Some compositors keep reporting the
        /// last surface entered while the pointer is over the desktop.
        fn under(&self) -> Option<Entity> {
            self.over.filter(|w| {
                self.size_of(*w).is_some_and(|size| {
                    self.position.x >= 0.0
                        && self.position.y >= 0.0
                        && self.position.x < size.x
                        && self.position.y < size.y
                })
            })
        }
    }

    /// The Wayland drag.
    pub struct WaylandDrag {
        conn: Connection,
        queue: EventQueue<State>,
        state: State,
        drag: Option<xdg_toplevel_drag_v1::XdgToplevelDragV1>,
        grab: Vec2,
        escape: bool,
        ended_by_app: bool,
    }

    impl WaylandDrag {
        /// Bind the drag's objects on the connection `display` (winit's
        /// `wl_display`). `None` when the compositor offers no data device.
        ///
        /// # Safety
        ///
        /// `display` is the live `wl_display` of the event loop, which
        /// outlives this value (the backend drops it before the loop ends).
        pub unsafe fn new(display: *mut std::ffi::c_void) -> Option<Self> {
            // SAFETY: the caller guarantees `display` is a live wl_display.
            let backend = unsafe { Backend::from_foreign_display(display.cast()) };
            let conn = Connection::from_backend(backend);
            let mut queue = conn.new_event_queue();
            let qh = queue.handle();
            conn.display().get_registry(&qh, ());
            let mut state = State {
                mime: format!(
                    "application/x-lumen-surface-drag;token={:x}{:08x}",
                    std::process::id(),
                    std::time::SystemTime::now()
                        .duration_since(std::time::UNIX_EPOCH)
                        .map_or(0, |d| d.subsec_nanos())
                ),
                ..State::default()
            };
            queue.roundtrip(&mut state).ok()?;
            let seat = state.seat.clone()?;
            let ddm = state.ddm.clone()?;
            state.pointer = Some(seat.get_pointer(&qh, ()));
            state.device = Some(ddm.get_data_device(&seat, &qh, ()));
            let _ = conn.flush();
            Some(Self {
                conn,
                queue,
                state,
                drag: None,
                grab: Vec2::ZERO,
                escape: false,
                ended_by_app: false,
            })
        }

        /// Whether a window can follow the pointer: the compositor offers
        /// `xdg_toplevel_drag_v1`.
        pub fn can_follow(&self) -> bool {
            self.state.tdm.is_some()
        }

        /// Whether a drag is running.
        pub fn active(&self) -> bool {
            self.state.source.is_some()
        }

        /// The app's windows, as they are now.
        pub fn set_surfaces(&mut self, surfaces: &[Surface]) {
            self.state.surfaces = surfaces
                .iter()
                .filter_map(|s| {
                    // SAFETY: the pointer is the live wl_surface of a window
                    // the backend owns.
                    let id = unsafe {
                        ObjectId::from_ptr(wl_surface::WlSurface::interface(), s.surface.cast())
                    }
                    .ok()?;
                    Some((id, s.window, s.size))
                })
                .collect();
        }

        /// An Escape press reached the app.
        pub fn escape_pressed(&mut self) {
            self.escape = true;
        }

        fn toplevel(&self, s: &Surface) -> Option<xdg_toplevel::XdgToplevel> {
            let ptr = s.toplevel?;
            // SAFETY: winit hands out the live xdg_toplevel of a window the
            // backend owns.
            let id =
                unsafe { ObjectId::from_ptr(xdg_toplevel::XdgToplevel::interface(), ptr.cast()) }
                    .ok()?;
            xdg_toplevel::XdgToplevel::from_id(&self.conn, id).ok()
        }

        fn attach(&mut self, follow: Entity, surfaces: &[Surface]) {
            let (Some(drag), Some(s)) = (
                self.drag.as_ref(),
                surfaces.iter().find(|s| s.window == follow),
            ) else {
                return;
            };
            if let Some(top) = self.toplevel(s) {
                drag.attach(&top, self.grab.x as i32, self.grab.y as i32);
            }
        }

        /// Take in a request the app wrote.
        pub fn request(&mut self, request: &SurfaceDragRequest, surfaces: &[Surface]) {
            let qh = self.queue.handle();
            match *request {
                SurfaceDragRequest::Begin {
                    source,
                    follow,
                    grab_offset,
                } => {
                    self.end();
                    let (Some(serial), Some(origin)) = (
                        self.state.press_serial,
                        surfaces.iter().find(|s| s.window == source),
                    ) else {
                        lumen_core::warn_line!(
                            "lumen-window-winit: a drag across windows needs the press that began it; none is held"
                        );
                        self.state.events.push(SurfaceDragEvent::Cancelled);
                        return;
                    };
                    // SAFETY: as in `set_surfaces`.
                    let Ok(id) = (unsafe {
                        ObjectId::from_ptr(
                            wl_surface::WlSurface::interface(),
                            origin.surface.cast(),
                        )
                    }) else {
                        return;
                    };
                    let Ok(origin) = wl_surface::WlSurface::from_id(&self.conn, id) else {
                        return;
                    };
                    let (Some(ddm), Some(device)) =
                        (self.state.ddm.clone(), self.state.device.clone())
                    else {
                        return;
                    };
                    let src = ddm.create_data_source(&qh, ());
                    src.offer(self.state.mime.clone());
                    src.set_actions(wl_data_device_manager::DndAction::Move);
                    self.grab = grab_offset;
                    self.drag = self
                        .state
                        .tdm
                        .as_ref()
                        .map(|m| m.get_xdg_toplevel_drag(&src, &qh, ()));
                    device.start_drag(Some(&src), &origin, None, serial);
                    self.state.source = Some(src);
                    self.state.dropped = false;
                    self.state.cancelled_at = None;
                    self.state.over = None;
                    self.escape = false;
                    self.ended_by_app = false;
                    if let Some(f) = follow {
                        self.attach(f, surfaces);
                    }
                }
                SurfaceDragRequest::Follow(Some(w)) => self.attach(w, surfaces),
                // The protocol has no detach; the window stays with the
                // drag until it ends.
                SurfaceDragRequest::Follow(None) => {}
                SurfaceDragRequest::End => {
                    self.ended_by_app = true;
                    self.end();
                }
            }
            let _ = self.conn.flush();
        }

        fn end(&mut self) {
            if let Some(d) = self.drag.take() {
                d.destroy();
            }
            if let Some(src) = self.state.source.take() {
                src.destroy();
            }
            self.state.cancelled_at = None;
        }

        /// Read what the compositor sent and return the events to write.
        pub fn dispatch(&mut self) -> Vec<SurfaceDragEvent> {
            if let Some(guard) = self.conn.prepare_read() {
                let _ = guard.read();
            }
            if let Err(e) = self.queue.dispatch_pending(&mut self.state) {
                lumen_core::warn_line!("lumen-window-winit: the Wayland drag stopped: {e}");
            }
            // A cancel with no drop: Escape, or a release away from the app.
            if let Some(at) = self.state.cancelled_at {
                if self.ended_by_app {
                    self.state.cancelled_at = None;
                } else if self.escape {
                    self.state.events.push(SurfaceDragEvent::Cancelled);
                    self.state.cancelled_at = None;
                } else if at.elapsed() >= ESCAPE_GRACE {
                    self.state.events.push(SurfaceDragEvent::Dropped {
                        window: None,
                        position: self.state.position,
                    });
                    self.state.cancelled_at = None;
                }
                if self.state.cancelled_at.is_none() {
                    self.end();
                }
            }
            let _ = self.conn.flush();
            std::mem::take(&mut self.state.events)
        }

        /// Whether the backend should wake soon to read the drag: one is
        /// running. A press needs no wake-up: winit wakes the loop for it,
        /// and the backend reads this queue before taking the app's
        /// requests, so the press serial is known when a drag begins.
        pub fn wants_wakeups(&self) -> bool {
            self.active()
        }
    }

    impl Dispatch<wl_registry::WlRegistry, ()> for State {
        fn event(
            s: &mut Self,
            r: &wl_registry::WlRegistry,
            e: wl_registry::Event,
            _: &(),
            _: &Connection,
            qh: &QueueHandle<Self>,
        ) {
            if let wl_registry::Event::Global {
                name,
                interface,
                version,
            } = e
            {
                match interface.as_str() {
                    "wl_seat" if s.seat.is_none() => {
                        s.seat = Some(r.bind(name, version.min(5), qh, ()));
                    }
                    "wl_data_device_manager" => {
                        s.ddm = Some(r.bind(name, version.min(3), qh, ()));
                    }
                    "xdg_toplevel_drag_manager_v1" => {
                        s.tdm = Some(r.bind(name, 1, qh, ()));
                    }
                    _ => {}
                }
            }
        }
    }

    impl Dispatch<wl_seat::WlSeat, ()> for State {
        fn event(
            _: &mut Self,
            _: &wl_seat::WlSeat,
            _: wl_seat::Event,
            _: &(),
            _: &Connection,
            _: &QueueHandle<Self>,
        ) {
        }
    }

    impl Dispatch<wl_data_device_manager::WlDataDeviceManager, ()> for State {
        fn event(
            _: &mut Self,
            _: &wl_data_device_manager::WlDataDeviceManager,
            _: wl_data_device_manager::Event,
            _: &(),
            _: &Connection,
            _: &QueueHandle<Self>,
        ) {
        }
    }

    impl Dispatch<xdg_toplevel_drag_manager_v1::XdgToplevelDragManagerV1, ()> for State {
        fn event(
            _: &mut Self,
            _: &xdg_toplevel_drag_manager_v1::XdgToplevelDragManagerV1,
            _: xdg_toplevel_drag_manager_v1::Event,
            _: &(),
            _: &Connection,
            _: &QueueHandle<Self>,
        ) {
        }
    }

    impl Dispatch<xdg_toplevel_drag_v1::XdgToplevelDragV1, ()> for State {
        fn event(
            _: &mut Self,
            _: &xdg_toplevel_drag_v1::XdgToplevelDragV1,
            _: xdg_toplevel_drag_v1::Event,
            _: &(),
            _: &Connection,
            _: &QueueHandle<Self>,
        ) {
        }
    }

    impl Dispatch<xdg_toplevel::XdgToplevel, ()> for State {
        fn event(
            _: &mut Self,
            _: &xdg_toplevel::XdgToplevel,
            _: xdg_toplevel::Event,
            _: &(),
            _: &Connection,
            _: &QueueHandle<Self>,
        ) {
        }
    }

    impl Dispatch<wl_surface::WlSurface, ()> for State {
        fn event(
            _: &mut Self,
            _: &wl_surface::WlSurface,
            _: wl_surface::Event,
            _: &(),
            _: &Connection,
            _: &QueueHandle<Self>,
        ) {
        }
    }

    impl Dispatch<wl_pointer::WlPointer, ()> for State {
        fn event(
            s: &mut Self,
            _: &wl_pointer::WlPointer,
            e: wl_pointer::Event,
            _: &(),
            _: &Connection,
            _: &QueueHandle<Self>,
        ) {
            if let wl_pointer::Event::Button { serial, state, .. } = e {
                s.press_serial = match state {
                    WEnum::Value(wl_pointer::ButtonState::Pressed) => Some(serial),
                    _ => None,
                };
            }
        }
    }

    impl Dispatch<wl_data_source::WlDataSource, ()> for State {
        fn event(
            s: &mut Self,
            src: &wl_data_source::WlDataSource,
            e: wl_data_source::Event,
            _: &(),
            _: &Connection,
            _: &QueueHandle<Self>,
        ) {
            if s.source.as_ref() != Some(src) {
                return;
            }
            match e {
                wl_data_source::Event::Cancelled | wl_data_source::Event::DndFinished => {
                    if !s.dropped {
                        s.cancelled_at = Some(Instant::now());
                    } else {
                        s.source = None;
                    }
                }
                // Nothing outside the app takes the private type, so no
                // data is ever asked for.
                wl_data_source::Event::Send { fd, .. } => drop(fd),
                _ => {}
            }
        }
    }

    impl Dispatch<wl_data_offer::WlDataOffer, ()> for State {
        fn event(
            _: &mut Self,
            _: &wl_data_offer::WlDataOffer,
            _: wl_data_offer::Event,
            _: &(),
            _: &Connection,
            _: &QueueHandle<Self>,
        ) {
        }
    }

    impl Dispatch<wl_data_device::WlDataDevice, ()> for State {
        fn event(
            s: &mut Self,
            _: &wl_data_device::WlDataDevice,
            e: wl_data_device::Event,
            _: &(),
            _: &Connection,
            _: &QueueHandle<Self>,
        ) {
            if s.source.is_none() {
                // Another application's drag: os-dnd's business.
                if let wl_data_device::Event::Enter { id: Some(o), .. } = e {
                    o.destroy();
                }
                return;
            }
            match e {
                wl_data_device::Event::Enter {
                    serial,
                    surface,
                    x,
                    y,
                    id,
                } => {
                    s.over = s.window_of(&surface);
                    s.position = Vec2::new(x as f32, y as f32);
                    s.fresh = true;
                    if let Some(o) = id {
                        o.accept(serial, Some(s.mime.clone()));
                        if o.version() >= 3 {
                            o.set_actions(
                                wl_data_device_manager::DndAction::Move,
                                wl_data_device_manager::DndAction::Move,
                            );
                        }
                        s.offer = Some(o);
                    }
                }
                wl_data_device::Event::Motion { x, y, .. } => {
                    s.position = Vec2::new(x as f32, y as f32);
                    s.fresh = false;
                    s.events.push(SurfaceDragEvent::Over {
                        window: s.under(),
                        position: s.position,
                    });
                }
                wl_data_device::Event::Leave => {
                    s.over = None;
                    if let Some(o) = s.offer.take() {
                        o.destroy();
                    }
                    s.events.push(SurfaceDragEvent::Over {
                        window: None,
                        position: s.position,
                    });
                }
                wl_data_device::Event::Drop => {
                    // A drop outside the surface's bounds is a drop on the
                    // desktop a compositor reports on the last surface.
                    let window = s.under().filter(|_| !s.fresh);
                    s.events.push(SurfaceDragEvent::Dropped {
                        window,
                        position: s.position,
                    });
                    s.dropped = true;
                    if let Some(o) = s.offer.take() {
                        o.finish();
                        o.destroy();
                    }
                }
                _ => {}
            }
        }

        wayland_client::event_created_child!(State, wl_data_device::WlDataDevice, [
            wl_data_device::EVT_DATA_OFFER_OPCODE => (wl_data_offer::WlDataOffer, ()),
        ]);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_most_recently_focused_window_is_on_top() {
        let mut world = World::new();
        let a = world.spawn_empty().id();
        let b = world.spawn_empty().id();
        let c = world.spawn_empty().id();
        let surface = |w: Entity| DesktopSurface {
            window: w,
            origin: IVec2::ZERO,
            size: UVec2::new(10, 10),
            scale: 1.0,
            visible: true,
        };
        let mut seam = DragSeam::default();
        seam.raised(a);
        seam.raised(b);
        let order: Vec<Entity> = stack(vec![surface(a), surface(b), surface(c)], &seam.order)
            .into_iter()
            .map(|s| s.window)
            .collect();
        assert_eq!(order, vec![b, a, c]);
        seam.closed(b);
        assert_eq!(seam.order, vec![a]);
    }

    #[test]
    fn a_window_asked_to_follow_before_it_opens_opens_under_the_pointer() {
        let mut world = World::new();
        let a = world.spawn_empty().id();
        let f = world.spawn_empty().id();
        let mut seam = DragSeam::default();
        assert!(!seam.on_wayland() || cfg!(target_os = "linux"));
        seam.request(
            &SurfaceDragRequest::Begin {
                source: a,
                follow: None,
                grab_offset: Vec2::new(10.0, 5.0),
            },
            &|_| true,
        );
        let surfaces = [DesktopSurface {
            window: a,
            origin: IVec2::new(100, 100),
            size: UVec2::new(200, 200),
            scale: 2.0,
            visible: true,
        }];
        let (event, follow) = seam.moved(&surfaces, a, Vec2::new(10.0, 10.0)).unwrap();
        assert_eq!(
            event,
            SurfaceDragEvent::Over {
                window: Some(a),
                position: Vec2::new(10.0, 10.0)
            }
        );
        assert_eq!(follow, None);
        seam.request(&SurfaceDragRequest::Follow(Some(f)), &|w| w != f);
        assert!(seam.open_following.contains(&f));
        assert_eq!(
            seam.open_at(Vec2::new(10.0, 5.0), 2.0),
            Some(IVec2::new(100, 110))
        );
        seam.request(&SurfaceDragRequest::End, &|_| true);
        assert!(seam.open_following.is_empty());
        assert_eq!(seam.open_at(Vec2::ZERO, 1.0), None);
        // Begin with a follower that has not opened marks it too.
        seam.request(
            &SurfaceDragRequest::Begin {
                source: a,
                follow: Some(f),
                grab_offset: Vec2::ZERO,
            },
            &|w| w != f,
        );
        assert!(seam.open_following.contains(&f));
        seam.closed(f);
        assert!(seam.open_following.is_empty());
    }
}
