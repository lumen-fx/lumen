//! Extensions that ride this backend's event loop.
//!
//! Some capabilities need the platform's window events and the loop itself,
//! not just the messages the backend turns them into: a drag that crosses
//! the app's windows reads every pointer move of the pressed window and
//! moves a window along with it, and on Wayland it shares the compositor
//! connection. Such a capability lives in a crate of its own, implements
//! [`LoopExtension`], and registers it with [`add_loop_extension`] when it
//! installs; the backend calls every registered extension at the same
//! points of the loop and names none of them. An app whose link leaves the
//! capability out runs no extension and carries none of its code.

use std::sync::Arc;
use std::time::Instant;

use bevy_ecs::prelude::Entity;
use lumen_core::app::App;
use winit::event::WindowEvent;
use winit::event_loop::ActiveEventLoop;
use winit::window::{Window, WindowAttributes};

/// What an extension sees of the backend at each call: the app, the event
/// loop, and the platform windows by their window entities.
pub struct LoopCx<'a> {
    /// The app.
    pub app: &'a mut App,
    /// The event loop.
    pub event_loop: &'a ActiveEventLoop,
    windows: &'a dyn Fn(Entity) -> Option<Arc<Window>>,
    open: Vec<Entity>,
    primary: Option<Entity>,
    frame: bool,
    release: bool,
}

impl<'a> LoopCx<'a> {
    pub(crate) fn new(
        app: &'a mut App,
        event_loop: &'a ActiveEventLoop,
        windows: &'a dyn Fn(Entity) -> Option<Arc<Window>>,
        open: Vec<Entity>,
        primary: Option<Entity>,
    ) -> Self {
        Self {
            app,
            event_loop,
            windows,
            open,
            primary,
            frame: false,
            release: false,
        }
    }

    /// The platform window of the window entity `window`, once it is open.
    pub fn window(&self, window: Entity) -> Option<Arc<Window>> {
        (self.windows)(window)
    }

    /// Every window entity with an open platform window.
    pub fn open_windows(&self) -> &[Entity] {
        &self.open
    }

    /// The primary window, once its platform window is open.
    pub fn primary(&self) -> Option<Entity> {
        self.primary
    }

    /// Ask for a tick and a frame: the extension wrote something the app
    /// should see now.
    pub fn want_frame(&mut self) {
        self.frame = true;
    }

    pub(crate) fn wants_frame(&self) -> bool {
        self.frame
    }

    /// The platform ended the pointer's press without telling the window,
    /// as a drag the compositor runs does: the backend releases the mouse
    /// buttons it still holds, where the pointer last was, once the
    /// extension returns.
    pub fn release_pointer(&mut self) {
        self.release = true;
    }

    pub(crate) fn wants_release(&self) -> bool {
        self.release
    }
}

/// A capability riding the event loop. Every method has a default that
/// does nothing, so an extension implements the points it needs. The
/// backend drops its extensions when the loop exits, while the platform
/// connection is still open.
pub trait LoopExtension: 'static {
    /// The loop started, or resumed, and the connection to the platform is
    /// up.
    fn resumed(&mut self, cx: &mut LoopCx<'_>) {
        let _ = cx;
    }

    /// A platform event for the window `window`, before the backend turns
    /// it into the app's messages.
    fn window_event(&mut self, cx: &mut LoopCx<'_>, window: Entity, event: &WindowEvent) {
        let _ = (cx, window, event);
    }

    /// The loop is about to wait, before the backend opens, changes and
    /// closes the platform windows the app asked for.
    fn about_to_wait(&mut self, cx: &mut LoopCx<'_>) {
        let _ = cx;
    }

    /// The attributes the platform window for `window` opens with, which an
    /// extension may change.
    fn before_open(
        &mut self,
        cx: &mut LoopCx<'_>,
        window: Entity,
        attrs: WindowAttributes,
    ) -> WindowAttributes {
        let _ = (cx, window);
        attrs
    }

    /// The platform window for `window` opened.
    fn opened(&mut self, cx: &mut LoopCx<'_>, window: Entity) {
        let _ = (cx, window);
    }

    /// The platform window for `window` closed.
    fn closed(&mut self, window: Entity) {
        let _ = window;
    }

    /// When the loop should look again even with no event, while the
    /// extension waits on something the platform does not wake it for.
    fn wake_at(&self) -> Option<Instant> {
        None
    }
}

/// The extensions an app registered, until the backend takes them when it
/// starts.
#[derive(Default)]
pub struct LoopExtensions(pub Vec<Box<dyn LoopExtension>>);

/// Register `extension` to ride the event loop of this backend, when it
/// runs the app.
pub fn add_loop_extension(app: &mut App, extension: Box<dyn LoopExtension>) {
    if app.world.get_non_send::<LoopExtensions>().is_none() {
        app.world.insert_non_send(LoopExtensions::default());
    }
    if let Some(mut list) = app.world.get_non_send_mut::<LoopExtensions>() {
        list.0.push(extension);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    struct Nothing;
    impl LoopExtension for Nothing {}

    #[test]
    fn extensions_queue_up_for_the_backend() {
        let mut app = App::new();
        add_loop_extension(&mut app, Box::new(Nothing));
        add_loop_extension(&mut app, Box::new(Nothing));
        let list = app.world.get_non_send::<LoopExtensions>().expect("kept");
        assert_eq!(list.0.len(), 2);
        assert!(list.0[0].wake_at().is_none());
        let mut ext = Nothing;
        ext.closed(Entity::PLACEHOLDER);
    }
}
