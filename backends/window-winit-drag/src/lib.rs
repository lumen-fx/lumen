//! Drags that cross the app's windows, on the winit window backend: the
//! platform side of the core's surface-drag seam
//! (`lumen_core::surface_drag`).
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
//! surfaces or place its windows, so once the pointer leaves the window it
//! pressed, the drag runs as a `wl_data_device` drag with a MIME type
//! private to this process ([`wayland`]). Until then the window reports the
//! pointer and keeps the keyboard, so Escape cancels the drag even where
//! the compositor withholds keys during a data-device drag (wlroots).
//!
//! It rides the winit backend's event loop as a
//! [`LoopExtension`](lumen_window_winit::extension::LoopExtension), and the
//! `window-winit-drag` capability registers it: an app whose link leaves the
//! capability out carries none of this, and its drags stay inside a window.

use std::collections::{HashMap, HashSet};
use std::time::{Duration, Instant};

use bevy_ecs::message::{MessageCursor, Messages};
use bevy_ecs::prelude::*;
use glam::{IVec2, UVec2, Vec2};
use lumen_capability::{CapabilityEnv, Select};
use lumen_core::app::App;
use lumen_core::surface_drag::{
    DesktopSurface, InProcessDrag, SurfaceDragEvent, SurfaceDragRequest, SurfaceDragSupport,
};
use lumen_window_winit::extension::{LoopCx, LoopExtension, add_loop_extension};
use winit::event::{ElementState, MouseButton, WindowEvent};
use winit::keyboard::{Key, NamedKey};
use winit::window::{Window, WindowAttributes};

/// A link that picks its capabilities carries this one for an app that
/// declares the dock module, the one that drags across windows; one whose
/// sources it cannot read carries it too.
pub const SELECT: Select = Select::OnUse(&["lumen-dock"]);

/// Where a concealed window waits during a drag, off every desktop.
const OFF_DESKTOP: IVec2 = IVec2::new(-30000, -30000);

/// How often the loop looks at a Wayland drag that may be running: winit
/// reads the shared connection and does not wake the loop for the drag's
/// own queue.
const DRAG_POLL: Duration = Duration::from_millis(4);

/// Whether `event` ends the drag, and with it the press that began it.
#[cfg(target_os = "linux")]
fn ends_press(event: &SurfaceDragEvent) -> bool {
    matches!(
        event,
        SurfaceDragEvent::Dropped { .. } | SurfaceDragEvent::Cancelled
    )
}

/// Register the drag with the winit backend. What the capability crate
/// beside this one registers.
pub fn install(app: &mut App, _env: &CapabilityEnv) {
    app.add_message::<SurfaceDragRequest>();
    app.add_message::<SurfaceDragEvent>();
    add_loop_extension(app, Box::<WinitDrag>::default());
}

/// What the backend keeps of a drag across windows.
#[derive(Default)]
pub struct DragSeam {
    /// The drag, where the backend tracks the pointer itself.
    pub inner: InProcessDrag,
    /// Windows the app asked to follow before they opened: they open
    /// inactive, under the pointer.
    pub open_following: HashSet<Entity>,
    /// Windows opening to follow the pointer, until they have opened.
    pub opening: HashSet<Entity>,
    /// A concealed following window moved off the desktop, and where its
    /// client area was.
    pub parked: Option<(Entity, IVec2)>,
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
        self.opening.remove(&window);
        if self.parked.is_some_and(|(w, _)| w == window) {
            self.parked = None;
        }
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
pub fn desktop_surface(entity: Entity, window: &Window, visible: bool) -> Option<DesktopSurface> {
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
pub fn place_client_area(window: &Window, client: IVec2) {
    let offset = match (window.inner_position(), window.outer_position()) {
        (Ok(inner), Ok(outer)) => IVec2::new(inner.x - outer.x, inner.y - outer.y),
        _ => IVec2::ZERO,
    };
    let to = client - offset;
    window.set_outer_position(winit::dpi::PhysicalPosition::new(to.x, to.y));
}

/// Order `surfaces` top-most first by `order` (most recently focused
/// first); a window never focused goes after, newest first.
pub fn stack(mut surfaces: Vec<DesktopSurface>, order: &[Entity]) -> Vec<DesktopSurface> {
    surfaces.sort_by(|a, b| {
        let rank = |e: Entity| order.iter().position(|&o| o == e).unwrap_or(usize::MAX);
        rank(a.window)
            .cmp(&rank(b.window))
            .then(b.window.cmp(&a.window))
    });
    surfaces
}

/// The drag across windows, as the winit backend's loop extension.
#[derive(Default)]
pub struct WinitDrag {
    /// The drag's bookkeeping.
    pub seam: DragSeam,
    /// The app's requests read so far.
    requests: MessageCursor<SurfaceDragRequest>,
    /// Where the pointer last was in each window, logical pixels.
    last: HashMap<Entity, Vec2>,
}

impl WinitDrag {
    /// Every open window on the desktop, the top-most first.
    fn desktop_surfaces(&self, cx: &LoopCx<'_>) -> Vec<DesktopSurface> {
        let list = cx
            .open_windows()
            .iter()
            .filter_map(|&e| {
                let window = cx.window(e)?;
                let visible = cx
                    .app
                    .world
                    .get::<lumen_core::window::Window>(e)
                    .is_none_or(|w| w.visible);
                desktop_surface(e, &window, visible)
            })
            .collect();
        stack(list, &self.seam.order)
    }

    /// The pointer moved to `p` in `window` during a drag tracked here.
    fn moved(&mut self, cx: &mut LoopCx<'_>, window: Entity, p: Vec2) {
        #[cfg(target_os = "linux")]
        if self.seam.wayland.is_some() {
            self.moved_on_wayland(cx, window, p);
            return;
        }
        if !self.seam.inner.is_active() {
            return;
        }
        let surfaces = self.desktop_surfaces(cx);
        let Some((event, follow)) = self.seam.moved(&surfaces, window, p) else {
            return;
        };
        cx.app.world.write_message(event);
        cx.want_frame();
        if let Some((f, to)) = follow
            && let Some(w) = cx.window(f)
        {
            place_client_area(&w, to);
        }
    }

    /// A drag on Wayland still in the window it began in: the window
    /// reports the pointer until it leaves, when the data-device drag
    /// starts.
    #[cfg(target_os = "linux")]
    fn moved_on_wayland(&mut self, cx: &mut LoopCx<'_>, window: Entity, p: Vec2) {
        let surfaces = Self::wayland_surfaces(cx);
        let Some(w) = self.seam.wayland.as_mut() else {
            return;
        };
        if w.waiting() != Some(window) {
            return;
        }
        let size = surfaces
            .iter()
            .find(|s| s.window == window)
            .map_or(Vec2::ZERO, |s| s.size);
        if wayland::within(p, size) {
            cx.app.world.write_message(SurfaceDragEvent::Over {
                window: Some(window),
                position: p,
            });
            cx.want_frame();
        } else {
            w.set_surfaces(&surfaces);
            w.start(&surfaces);
            // Off the window: the compositor reports the pointer again
            // once it is over one of the app's windows.
            cx.app.world.write_message(SurfaceDragEvent::Over {
                window: None,
                position: p,
            });
            cx.want_frame();
        }
    }

    /// The button came up at `p` in `window` during a drag tracked here.
    fn released(&mut self, cx: &mut LoopCx<'_>, window: Entity, p: Vec2) {
        #[cfg(target_os = "linux")]
        if let Some(w) = self.seam.wayland.as_mut() {
            // A drag that never left its window drops where it is.
            if w.waiting() == Some(window) {
                w.stop_waiting();
                cx.app.world.write_message(SurfaceDragEvent::Dropped {
                    window: Some(window),
                    position: p,
                });
                cx.want_frame();
            }
            return;
        }
        if !self.seam.inner.is_active() {
            return;
        }
        let surfaces = self.desktop_surfaces(cx);
        if let Some(event) = self.seam.inner.released(&surfaces, window, p) {
            cx.app.world.write_message(event);
            cx.want_frame();
        }
    }

    /// The app's drag requests since the last look.
    fn take_requests(&mut self, cx: &mut LoopCx<'_>) -> Vec<SurfaceDragRequest> {
        let Some(messages) = cx.app.world.get_resource::<Messages<SurfaceDragRequest>>() else {
            return Vec::new();
        };
        let requests: Vec<SurfaceDragRequest> = self.requests.read(messages).copied().collect();
        let opened: HashSet<Entity> = cx.open_windows().iter().copied().collect();
        for r in &requests {
            let follow = self.seam.inner.follow();
            match *r {
                SurfaceDragRequest::Conceal(c) => self.conceal(cx, follow, c),
                SurfaceDragRequest::End => self.conceal(cx, follow, false),
                _ => {}
            }
            self.seam.request(r, &|e| opened.contains(&e));
        }
        requests
    }

    /// Get the following window out of the way, or bring it back. Hiding the
    /// window that holds the pointer grab ends the grab on X11 and Windows,
    /// so off Wayland the window is moved off the desktop instead and put
    /// back where it was; Wayland keeps the drag on the data device and the
    /// window hides.
    fn conceal(&mut self, cx: &mut LoopCx<'_>, follow: Option<Entity>, concealed: bool) {
        if self.seam.on_wayland() {
            if let Some(w) = follow.and_then(|f| cx.window(f)) {
                w.set_visible(!concealed);
            }
            return;
        }
        match (concealed, self.seam.parked) {
            (true, None) => {
                let Some((f, w)) = follow.and_then(|f| cx.window(f).map(|w| (f, w))) else {
                    return;
                };
                let Ok(at) = w.inner_position() else { return };
                self.seam.parked = Some((f, IVec2::new(at.x, at.y)));
                place_client_area(&w, OFF_DESKTOP);
            }
            (false, Some((f, at))) => {
                self.seam.parked = None;
                if let Some(w) = cx.window(f) {
                    place_client_area(&w, at);
                }
            }
            _ => {}
        }
    }

    /// The app's windows as the Wayland drag sees them.
    #[cfg(target_os = "linux")]
    fn wayland_surfaces(cx: &LoopCx<'_>) -> Vec<wayland::Surface> {
        use raw_window_handle::{HasWindowHandle, RawWindowHandle};
        use winit::platform::wayland::WindowExtWayland;
        cx.open_windows()
            .iter()
            .filter_map(|&e| {
                let window = cx.window(e)?;
                let RawWindowHandle::Wayland(h) = window.window_handle().ok()?.as_raw() else {
                    return None;
                };
                let size = cx
                    .app
                    .world
                    .get::<lumen_core::window::WindowViewport>(e)
                    .map_or(Vec2::ZERO, |v| v.size);
                Some(wayland::Surface {
                    window: e,
                    surface: h.surface.as_ptr(),
                    toplevel: window.xdg_toplevel().map(|t| t.as_ptr()),
                    size,
                })
            })
            .collect()
    }
}

impl LoopExtension for WinitDrag {
    fn resumed(&mut self, cx: &mut LoopCx<'_>) {
        // On Wayland the drag binds its objects on winit's connection and
        // asks the compositor whether a window can follow; elsewhere the
        // pressed window keeps the pointer and a window always can.
        #[cfg(target_os = "linux")]
        {
            use raw_window_handle::{HasDisplayHandle, RawDisplayHandle};
            if let Ok(handle) = cx.event_loop.display_handle()
                && let RawDisplayHandle::Wayland(h) = handle.as_raw()
            {
                // SAFETY: the display is the event loop's own, alive as long
                // as the loop; the backend drops its loop extensions when the
                // loop exits, before the display closes.
                self.seam.wayland = unsafe { wayland::WaylandDrag::new(h.display.as_ptr()) };
                match &self.seam.wayland {
                    Some(w) => {
                        let follow = w.can_follow();
                        cx.app.world.insert_resource(SurfaceDragSupport { follow });
                    }
                    None => {
                        cx.app.world.remove_resource::<SurfaceDragSupport>();
                    }
                }
                return;
            }
        }
        cx.app
            .world
            .insert_resource(SurfaceDragSupport { follow: true });
    }

    fn window_event(&mut self, cx: &mut LoopCx<'_>, window: Entity, event: &WindowEvent) {
        match event {
            WindowEvent::CursorMoved { position, .. } => {
                let scale = cx.window(window).map_or(1.0, |w| w.scale_factor());
                let logical = position.to_logical::<f32>(scale);
                let p = Vec2::new(logical.x, logical.y);
                self.last.insert(window, p);
                self.moved(cx, window, p);
            }
            WindowEvent::MouseInput {
                state: ElementState::Released,
                button: MouseButton::Left,
                ..
            } => {
                let p = self.last.get(&window).copied().unwrap_or_default();
                self.released(cx, window, p);
            }
            WindowEvent::KeyboardInput { event, .. }
                if event.state == ElementState::Pressed
                    && event.logical_key == Key::Named(NamedKey::Escape) =>
            {
                #[cfg(target_os = "linux")]
                if let Some(w) = self.seam.wayland.as_mut() {
                    w.escape_pressed();
                }
            }
            WindowEvent::Focused(true) => self.seam.raised(window),
            _ => {}
        }
    }

    fn about_to_wait(&mut self, cx: &mut LoopCx<'_>) {
        // The Wayland drag's queue first: a drag the app begins needs the
        // serial of the press that came in with this batch.
        #[cfg(target_os = "linux")]
        if let Some(active) = self.seam.wayland.as_ref().map(|w| w.active()) {
            let surfaces = Self::wayland_surfaces(cx);
            let w = self.seam.wayland.as_mut().expect("checked");
            if active {
                w.set_surfaces(&surfaces);
            }
            let events = w.dispatch();
            if !events.is_empty() {
                // The compositor ran the drag and kept the button's release
                // from the window; the drag's end is that release.
                if events.iter().any(ends_press) {
                    cx.release_pointer();
                }
                for e in events {
                    cx.app.world.write_message(e);
                }
                cx.want_frame();
            }
        }
        let requests = self.take_requests(cx);
        #[cfg(target_os = "linux")]
        if !requests.is_empty() && self.seam.wayland.is_some() {
            let surfaces = Self::wayland_surfaces(cx);
            if let Some(w) = self.seam.wayland.as_mut() {
                w.set_surfaces(&surfaces);
                for r in &requests {
                    w.request(r, &surfaces);
                }
            }
        }
        let _ = requests;
    }

    fn before_open(
        &mut self,
        cx: &mut LoopCx<'_>,
        window: Entity,
        attrs: WindowAttributes,
    ) -> WindowAttributes {
        // A window opening to follow the pointer opens under it and leaves
        // the keyboard with the drag.
        if !self.seam.open_following.remove(&window) {
            return attrs;
        }
        self.seam.opening.insert(window);
        let attrs = attrs.with_active(false);
        let scale = cx
            .primary()
            .and_then(|p| cx.window(p))
            .map_or(1.0, |w| w.scale_factor() as f32);
        match self
            .seam
            .inner
            .grab_offset()
            .and_then(|g| self.seam.open_at(g, scale))
        {
            Some(at) => attrs.with_position(winit::dpi::PhysicalPosition::new(at.x, at.y)),
            None => attrs,
        }
    }

    fn opened(&mut self, cx: &mut LoopCx<'_>, window: Entity) {
        let following = self.seam.opening.remove(&window);
        #[cfg(target_os = "linux")]
        if following && self.seam.wayland.is_some() {
            let surfaces = Self::wayland_surfaces(cx);
            if let Some(w) = self.seam.wayland.as_mut() {
                w.set_surfaces(&surfaces);
                w.request(&SurfaceDragRequest::Follow(Some(window)), &surfaces);
            }
        }
        let _ = (cx, following);
    }

    fn closed(&mut self, window: Entity) {
        self.seam.closed(window);
        self.last.remove(&window);
    }

    fn wake_at(&self) -> Option<Instant> {
        #[cfg(target_os = "linux")]
        if self
            .seam
            .wayland
            .as_ref()
            .is_some_and(|w| w.wants_wakeups())
        {
            return Some(Instant::now() + DRAG_POLL);
        }
        None
    }
}

#[cfg(target_os = "linux")]
pub mod wayland {
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

    use std::time::Instant;

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

    use super::Duration;

    /// How long after a `cancelled` with no drop an Escape press may still
    /// arrive and make it a cancel.
    const ESCAPE_GRACE: Duration = Duration::from_millis(60);

    /// The MIME type the drag offers: private to this process, so no other
    /// application takes the drop.
    pub fn mime_type(pid: u32, nonce: u32) -> String {
        format!("application/x-lumen-surface-drag;token={pid:x}{nonce:08x}")
    }

    /// What a drag the compositor cancelled with no drop comes to, `since`
    /// the cancel: nothing to report when the app ended it itself; a cancel
    /// when the user cancelled it (an Escape press reached the app, or the
    /// compositor cancelled it over one of the app's windows); a drop on no
    /// window once the grace for that Escape has passed; and `None` while it
    /// may still come.
    pub fn after_cancel(
        ended_by_app: bool,
        cancelled: bool,
        since: Duration,
        position: Vec2,
    ) -> Option<Vec<SurfaceDragEvent>> {
        if ended_by_app {
            Some(Vec::new())
        } else if cancelled {
            Some(vec![SurfaceDragEvent::Cancelled])
        } else if since >= ESCAPE_GRACE {
            Some(vec![SurfaceDragEvent::Dropped {
                window: None,
                position,
            }])
        } else {
            None
        }
    }

    /// Whether `position` lies inside a surface of `size`, in its own
    /// coordinates.
    pub fn within(position: Vec2, size: Vec2) -> bool {
        position.x >= 0.0 && position.y >= 0.0 && position.x < size.x && position.y < size.y
    }

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
        /// The compositor cancelled the drag over one of the app's windows,
        /// where a release would have been a drop: the compositor's own
        /// Escape (KWin's), not a release over the desktop.
        cancelled_inside: bool,
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
                self.size_of(*w)
                    .is_some_and(|size| within(self.position, size))
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
        /// A drag the app began whose data-device drag waits for the
        /// pointer to leave the window it began in: the source window and
        /// the window to follow.
        waiting: Option<(Entity, Option<Entity>)>,
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
                mime: mime_type(
                    std::process::id(),
                    std::time::SystemTime::now()
                        .duration_since(std::time::UNIX_EPOCH)
                        .map_or(0, |d| d.subsec_nanos()),
                ),
                ..State::default()
            };
            queue.roundtrip(&mut state).ok()?;
            let seat = state.seat.clone()?;
            let ddm = state.ddm.clone()?;
            state.device = Some(ddm.get_data_device(&seat, &qh, ()));
            // The seat's capabilities, and with them our pointer, arrive in
            // reply to the bind.
            queue.roundtrip(&mut state).ok()?;
            let _ = conn.flush();
            Some(Self {
                conn,
                queue,
                state,
                drag: None,
                grab: Vec2::ZERO,
                escape: false,
                ended_by_app: false,
                waiting: None,
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
            match *request {
                SurfaceDragRequest::Begin {
                    source,
                    follow,
                    grab_offset,
                } => {
                    self.end();
                    self.grab = grab_offset;
                    self.escape = false;
                    self.ended_by_app = false;
                    // Within its window a drag needs no data device: the
                    // window keeps the pointer and the keyboard (a wlroots
                    // compositor gives neither to anyone during one, so
                    // Escape could not cancel it). It starts one when the
                    // pointer leaves, or at once when a window follows it.
                    self.waiting = Some((source, follow));
                    if follow.is_some() {
                        self.start(surfaces);
                    }
                }
                SurfaceDragRequest::Follow(Some(w)) => {
                    if let Some((_, follow)) = &mut self.waiting {
                        *follow = Some(w);
                        self.start(surfaces);
                    } else {
                        self.attach(w, surfaces);
                    }
                }
                // The protocol has no detach; the window stays with the
                // drag until it ends. Concealing is the window's own
                // visibility, which the backend sets.
                SurfaceDragRequest::Follow(None) | SurfaceDragRequest::Conceal(_) => {}
                SurfaceDragRequest::End => {
                    self.ended_by_app = true;
                    self.end();
                }
            }
            let _ = self.conn.flush();
        }

        /// The window a drag began in while it waits for the pointer to
        /// leave it.
        pub fn waiting(&self) -> Option<Entity> {
            self.waiting.map(|(source, _)| source)
        }

        /// The waiting drag ended within its window.
        pub fn stop_waiting(&mut self) {
            self.waiting = None;
        }

        /// Start the data-device drag a waiting drag needs now.
        pub fn start(&mut self, surfaces: &[Surface]) {
            let qh = self.queue.handle();
            let Some((source, follow)) = self.waiting.take() else {
                return;
            };
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
                ObjectId::from_ptr(wl_surface::WlSurface::interface(), origin.surface.cast())
            }) else {
                return;
            };
            let Ok(origin) = wl_surface::WlSurface::from_id(&self.conn, id) else {
                return;
            };
            let (Some(ddm), Some(device)) = (self.state.ddm.clone(), self.state.device.clone())
            else {
                return;
            };
            let src = ddm.create_data_source(&qh, ());
            src.offer(self.state.mime.clone());
            src.set_actions(wl_data_device_manager::DndAction::Move);
            self.drag = self
                .state
                .tdm
                .as_ref()
                .map(|m| m.get_xdg_toplevel_drag(&src, &qh, ()));
            device.start_drag(Some(&src), &origin, None, serial);
            self.state.source = Some(src);
            self.state.dropped = false;
            self.state.cancelled_at = None;
            self.state.cancelled_inside = false;
            self.state.over = None;
            if let Some(f) = follow {
                self.attach(f, surfaces);
            }
            let _ = self.conn.flush();
        }

        fn end(&mut self) {
            self.waiting = None;
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
                let outcome = after_cancel(
                    self.ended_by_app,
                    self.escape || self.state.cancelled_inside,
                    at.elapsed(),
                    self.state.position,
                );
                if let Some(outcome) = outcome {
                    self.state.events.extend(outcome);
                    self.state.cancelled_at = None;
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

    /// Whether a seat's capabilities include a pointer. Asking a seat with
    /// none for its pointer is a protocol error that ends the connection,
    /// which this one shares with the event loop.
    pub(crate) fn has_pointer(caps: WEnum<wl_seat::Capability>) -> bool {
        matches!(caps, WEnum::Value(c) if c.contains(wl_seat::Capability::Pointer))
    }

    impl Dispatch<wl_seat::WlSeat, ()> for State {
        fn event(
            s: &mut Self,
            seat: &wl_seat::WlSeat,
            e: wl_seat::Event,
            _: &(),
            _: &Connection,
            qh: &QueueHandle<Self>,
        ) {
            if let wl_seat::Event::Capabilities { capabilities } = e {
                let has = has_pointer(capabilities);
                if has && s.pointer.is_none() {
                    s.pointer = Some(seat.get_pointer(qh, ()));
                } else if !has && let Some(pointer) = s.pointer.take() {
                    if pointer.version() >= 3 {
                        pointer.release();
                    }
                    s.press_serial = None;
                }
            }
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
                        s.cancelled_inside = s.under().is_some();
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

    #[cfg(target_os = "linux")]
    #[test]
    fn a_cancelled_wayland_drag_is_told_apart_from_a_drop_on_nothing() {
        use wayland::{after_cancel, mime_type, within};
        let at = Vec2::new(5.0, 6.0);
        assert_eq!(
            after_cancel(true, true, Duration::ZERO, at),
            Some(Vec::new())
        );
        assert_eq!(
            after_cancel(false, true, Duration::ZERO, at),
            Some(vec![SurfaceDragEvent::Cancelled])
        );
        assert_eq!(
            after_cancel(false, false, Duration::from_millis(10), at),
            None
        );
        assert_eq!(
            after_cancel(false, false, Duration::from_millis(100), at),
            Some(vec![SurfaceDragEvent::Dropped {
                window: None,
                position: at
            }])
        );
        assert_eq!(
            mime_type(0x1f, 0xab),
            "application/x-lumen-surface-drag;token=1f000000ab"
        );
        let size = Vec2::new(10.0, 10.0);
        assert!(within(Vec2::new(0.0, 9.9), size));
        assert!(!within(Vec2::new(10.0, 1.0), size));
        assert!(!within(Vec2::new(-1.0, 1.0), size));
    }

    #[cfg(target_os = "linux")]
    #[test]
    fn a_drop_or_a_cancel_ends_the_press() {
        assert!(ends_press(&SurfaceDragEvent::Cancelled));
        assert!(ends_press(&SurfaceDragEvent::Dropped {
            window: None,
            position: Vec2::ZERO
        }));
        assert!(!ends_press(&SurfaceDragEvent::Over {
            window: None,
            position: Vec2::ZERO
        }));
    }

    #[cfg(target_os = "linux")]
    #[test]
    fn only_a_seat_with_a_pointer_is_asked_for_one() {
        use wayland::has_pointer;
        use wayland_client::WEnum;
        use wayland_client::protocol::wl_seat::Capability;
        assert!(has_pointer(WEnum::Value(Capability::Pointer)));
        assert!(has_pointer(WEnum::Value(
            Capability::Pointer | Capability::Keyboard
        )));
        assert!(!has_pointer(WEnum::Value(Capability::Keyboard)));
        assert!(!has_pointer(WEnum::Value(Capability::empty())));
        assert!(!has_pointer(WEnum::Unknown(0x80)));
    }

    #[test]
    fn the_capability_registers_the_drag_messages() {
        let mut app = App::new();
        install(
            &mut app,
            &CapabilityEnv::new(".", toml::Table::new(), String::new(), false),
        );
        assert!(
            app.world
                .contains_resource::<Messages<SurfaceDragRequest>>()
        );
        assert!(app.world.contains_resource::<Messages<SurfaceDragEvent>>());
        let list = app
            .world
            .get_non_send::<lumen_window_winit::extension::LoopExtensions>()
            .expect("the extension waits for the backend");
        assert_eq!(list.0.len(), 1);
        assert!(list.0[0].wake_at().is_none());
        assert_eq!(SELECT, Select::OnUse(&["lumen-dock"]));
    }

    #[test]
    fn a_window_opening_to_follow_is_tracked_until_it_opens() {
        let mut world = World::new();
        let a = world.spawn_empty().id();
        let mut seam = DragSeam::default();
        seam.open_following.insert(a);
        seam.opening.insert(a);
        seam.closed(a);
        assert!(seam.opening.is_empty() && seam.open_following.is_empty());
    }

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
