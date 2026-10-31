//! The rendered headless run from the SDK `App`: an app paints its frame
//! with no window, reads it back to screenshot or golden-test itself, and
//! takes that mode from `run` when `lumenc run --headless` started it. One app
//! at a time: an app publishes process-global state.

// In an app this is the published `lumenui` crate; inside the workspace the
// engine crate it re-exports stands in for it under the same name.
extern crate lumenui_engine as lumenui;

use std::path::PathBuf;
use std::sync::Mutex;

use lumenui::lumen_runtime::app_kind::HeadlessHandoff;
use lumenui::prelude::*;

static APP_ISOLATION: Mutex<()> = Mutex::new(());

fn isolate() -> std::sync::MutexGuard<'static, ()> {
    APP_ISOLATION
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
}

/// An app whose whole viewport is one known colour.
fn fixture(tag: &str) -> PathBuf {
    let dir = std::env::temp_dir().join(format!(
        "lumenui-headless-rendered-{}-{tag}",
        std::process::id()
    ));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(dir.join("src")).unwrap();
    std::fs::write(dir.join("src").join("main.lmn"), "<root id=\"r\"/>").unwrap();
    std::fs::write(
        dir.join("src").join("main.css"),
        "#r { width: 100%; height: 100%; background: #ff0000; }",
    )
    .unwrap();
    std::fs::write(dir.join("lumen.toml"), "[mcp]\nport = 0\n").unwrap();
    dir
}

#[test]
fn a_screenshot_is_the_frame_the_app_painted() {
    let _guard = isolate();
    let dir = fixture("screenshot");
    let frame = App::new(CompiledUi::compile(&dir).expect("the UI compiles"))
        .window_size(64, 48)
        .screenshot(HeadlessOptions {
            dpr: 2.0,
            ticks: Some(2),
            ..HeadlessOptions::default()
        })
        .expect("the app paints a frame");
    assert_eq!((frame.width(), frame.height()), (128, 96));
    assert_eq!(frame.pixel(64, 48), Some([255, 0, 0, 255]));

    // The golden-test round trip: what was saved compares equal.
    let png = dir.join("shot.png");
    frame.save_png(&png).unwrap();
    assert_eq!(lumenui::Frame::open_png(&png).unwrap(), frame);
    let _ = std::fs::remove_dir_all(&dir);
}

/// An image decodes on a worker thread, so a run bounded to its first tick
/// ends before the decode lands. `wait_for_loads` holds the closing frame
/// until it has, and the screenshot shows the image.
#[test]
fn a_screenshot_can_wait_for_its_images() {
    let _guard = isolate();
    let dir = fixture("loads");
    let (w, h) = (1024u32, 1024u32);
    let blue: Vec<u8> = [0u8, 0, 255, 255].repeat((w * h) as usize);
    lumenui::Frame::from_rgba8(w, h, blue)
        .unwrap()
        .save_png(dir.join("blue.png"))
        .unwrap();
    std::fs::write(
        dir.join("src").join("main.lmn"),
        "<root id=\"r\"><image src=\"blue.png\" width=\"100%\" height=\"100%\"/></root>",
    )
    .unwrap();
    let frame = App::new(CompiledUi::compile(&dir).expect("the UI compiles"))
        .window_size(64, 48)
        .screenshot(HeadlessOptions {
            dpr: 1.0,
            ticks: Some(1),
            wait_for_loads: true,
            ..HeadlessOptions::default()
        })
        .expect("the app paints a frame");
    assert_eq!(frame.pixel(32, 24), Some([0, 0, 255, 255]));
    let _ = std::fs::remove_dir_all(&dir);
}

/// The wait follows whatever reports a load still pending, not the images
/// alone: a run bounded to two ticks keeps ticking until a system stops
/// raising [`LoadsPending`], and without `wait_for_loads` it stops at two.
#[test]
fn a_bounded_run_holds_its_last_frame_for_pending_loads() {
    use std::sync::atomic::{AtomicU32, Ordering};
    static TICKS: AtomicU32 = AtomicU32::new(0);
    fn loading_for_ten_ticks(loads: Res<LoadsPending>) {
        if TICKS.fetch_add(1, Ordering::Relaxed) + 1 < 10 {
            loads.request();
        }
    }

    let _guard = isolate();
    let dir = fixture("pending");
    for (wait, ran) in [(false, 2), (true, 10)] {
        TICKS.store(0, Ordering::Relaxed);
        App::new(CompiledUi::compile(&dir).expect("the UI compiles"))
            .window_size(16, 16)
            .add_systems(TickStage::Systems, loading_for_ten_ticks)
            .screenshot(HeadlessOptions {
                ticks: Some(2),
                wait_for_loads: wait,
                ..HeadlessOptions::default()
            })
            .expect("the app paints a frame");
        assert!(
            TICKS.load(Ordering::Relaxed) >= ran,
            "wait_for_loads: {wait}: ran {} ticks",
            TICKS.load(Ordering::Relaxed)
        );
        if !wait {
            assert!(TICKS.load(Ordering::Relaxed) < 10, "it did not wait");
        }
    }
    let _ = std::fs::remove_dir_all(&dir);
}

/// `lumenc run --headless` sets the handoff variables on the app's process,
/// and `run` takes the bounded headless path instead of opening a window.
#[test]
fn run_takes_the_headless_handoff() {
    let _guard = isolate();
    let dir = fixture("handoff");
    let env = HeadlessHandoff {
        ticks: Some(2),
        dpr: None,
        size: Some((32, 32)),
    }
    .to_env();
    // SAFETY: the isolation lock keeps every app in this binary, and so
    // every reader of these variables, off other threads while they change.
    for (k, v) in &env {
        unsafe { std::env::set_var(k, v) };
    }
    let result = App::new(CompiledUi::compile(&dir).expect("the UI compiles")).run();
    for (k, _) in &env {
        unsafe { std::env::remove_var(k) };
    }
    result.expect("a handed-off run is bounded and headless");
    let _ = std::fs::remove_dir_all(&dir);
}
