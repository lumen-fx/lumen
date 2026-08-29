//! The caps, and what a call over one answers.
//!
//! A buffer is memory a script asks for by the megapixel, so every call that
//! allocates or copies is bounded. What matters here is that a refusal is a
//! value the script can branch on rather than a crash, and that the bound is
//! the one the app configured.

use lumen_canvas::store::{self, Caps};
use lumen_canvas::{CanvasPlugin, MAX_BUFFER_COUNT_CAP, MIN_BUFFER_PIXEL_CAP};

/// The store is process-global, so these run one at a time.
static SERIAL: std::sync::Mutex<()> = std::sync::Mutex::new(());

/// A store with the given caps and nothing in it.
fn with_caps(caps: Caps) -> std::sync::MutexGuard<'static, ()> {
    let guard = SERIAL.lock().unwrap_or_else(|e| e.into_inner());
    store::reset();
    store::store().caps = caps;
    guard
}

#[test]
fn a_buffer_over_the_pixel_cap_is_refused() {
    let _guard = with_caps(Caps {
        buffer_pixels: 4096,
        ..Caps::default()
    });
    let mut store = store::store();
    assert!(store.new_buffer(64, 64).is_ok(), "64x64 is 4096 pixels");
    let err = store.new_buffer(65, 64).expect_err("over the cap");
    assert!(err.contains("over the"), "{err}");
    assert!(err.contains("4096"), "the message names the cap: {err}");
}

#[test]
fn a_buffer_with_no_pixels_is_refused() {
    let _guard = with_caps(Caps::default());
    let mut store = store::store();
    let err = store.new_buffer(0, 10).expect_err("no pixels");
    assert!(err.contains("no pixels"), "{err}");
}

#[test]
fn the_buffer_count_is_capped() {
    let _guard = with_caps(Caps {
        buffer_count: 2,
        ..Caps::default()
    });
    let mut store = store::store();
    let first = store.new_buffer(2, 2).expect("first");
    store.new_buffer(2, 2).expect("second");
    let err = store.new_buffer(2, 2).expect_err("third");
    assert!(err.contains("cap"), "{err}");

    // Freeing one makes room again, which is what the message told the
    // script to do.
    store.buffers.remove(&first);
    assert!(store.new_buffer(2, 2).is_ok());
}

#[test]
fn a_freed_handle_reads_like_one_that_never_existed() {
    let _guard = with_caps(Caps::default());
    let mut store = store::store();
    let handle = store.new_buffer(4, 4).expect("buffer");
    store.buffers.remove(&handle);
    assert!(!store.buffers.contains_key(&handle));
    // Handle 0 is never issued, so a script holding a stale zero and one
    // holding a stale handle get the same answer.
    assert!(!store.buffers.contains_key(&0));
}

#[test]
fn handles_are_never_reused() {
    let _guard = with_caps(Caps::default());
    let mut store = store::store();
    let first = store.new_buffer(1, 1).expect("first");
    store.buffers.remove(&first);
    let second = store.new_buffer(1, 1).expect("second");
    assert_ne!(
        first, second,
        "a reused handle would silently hand one script another's pixels"
    );
}

#[test]
fn a_configured_cap_outside_the_range_is_clamped() {
    let plugin = CanvasPlugin::with_caps(0, 0, u64::MAX);
    let caps = plugin.caps();
    assert_eq!(caps.buffer_pixels, MIN_BUFFER_PIXEL_CAP);
    assert_eq!(caps.buffer_count, MAX_BUFFER_COUNT_CAP);
    assert_eq!(caps.region, lumen_canvas::MIN_REGION_CAP);
}

#[test]
fn the_defaults_are_what_an_app_gets_without_config() {
    let caps = CanvasPlugin::default().caps();
    assert_eq!(caps.region, lumen_canvas::DEFAULT_REGION_CAP);
    assert_eq!(caps.buffer_pixels, lumen_canvas::DEFAULT_BUFFER_PIXEL_CAP);
    assert_eq!(caps.buffer_count, lumen_canvas::DEFAULT_BUFFER_COUNT_CAP);
}
