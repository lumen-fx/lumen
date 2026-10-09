//! The `window` script namespace's cache. It is process-wide and every app
//! tick may write it, so these run in a binary of their own, one at a time,
//! where no other test's app can tick in between.

use std::sync::Mutex;

use lumen_core::app::App;
use lumen_core::render_world::Viewport;
use lumen_core::window_state::{dpr, set_dpr, set_size, set_title, size, title};

static SERIAL: Mutex<()> = Mutex::new(());

#[test]
fn title_and_size_round_trip() {
    let _guard = SERIAL.lock().unwrap_or_else(|e| e.into_inner());
    set_title("Hello");
    assert_eq!(title(), "Hello");
    set_size(640.0, 480.0);
    assert_eq!(size(), (640.0, 480.0));
    set_dpr(2.0);
    assert_eq!(dpr(), 2.0);
}

/// The getters answer the viewport the app runs at, from the first tick on,
/// and follow it when it changes.
#[test]
fn the_getters_follow_the_viewport() {
    let _guard = SERIAL.lock().unwrap_or_else(|e| e.into_inner());
    let mut app = App::new();
    {
        let mut viewport = app.world.resource_mut::<Viewport>();
        viewport.size = glam::Vec2::new(320.0, 200.0);
        viewport.scale_factor = 3.0;
    }
    app.tick();
    assert_eq!(size(), (320.0, 200.0));
    assert_eq!(dpr(), 3.0);

    app.world.resource_mut::<Viewport>().size = glam::Vec2::new(800.0, 600.0);
    app.tick();
    assert_eq!(size(), (800.0, 600.0));
}
