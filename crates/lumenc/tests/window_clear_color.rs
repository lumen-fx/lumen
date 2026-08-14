// Needs `RunOptions` / `build_headless_app`, which only exist under
// `dev-run`.
#![cfg(feature = "dev-run")]

//! The window's GPU clear color - what a user sees before the root element
//! itself paints - used to be three different hardcoded values across
//! `lumen-window-winit`, `lumen-runtime`, and the code that wired one into
//! the other. It now resolves from the `--lumen-window-bg` custom property
//! of the fully-combined stylesheet (Palette, then UA, then skin, then app)
//! when any layer defines it, falling back to
//! `lumen_core::window::DEFAULT_CLEAR` otherwise. See
//! `lumen_runtime::run::app_build::build_app`.

use lumen_core::window::{DEFAULT_CLEAR, WindowOptions};
use lumenc::RunOptions;
use lumenc::run::build_headless_app;

fn build(markup: &str, css: &str, lumen_toml: &str) -> WindowOptions {
    // A per-call counter, not a timestamp: these tests run concurrently in
    // one process, and SystemTime's granularity (about a microsecond on
    // macOS) let two of them mint the same directory and read each other's
    // lumen.toml.
    static SEQ: std::sync::atomic::AtomicUsize = std::sync::atomic::AtomicUsize::new(0);
    let dir = std::env::temp_dir().join(format!(
        "lumenc_clear_color_{}_{}",
        std::process::id(),
        SEQ.fetch_add(1, std::sync::atomic::Ordering::Relaxed)
    ));
    std::fs::create_dir_all(&dir).unwrap();
    std::fs::write(dir.join("lumen.toml"), lumen_toml).unwrap();
    let opts = RunOptions::new(&dir)
        .with_parser(lumenc::default_parser())
        .with_markup(markup.to_string())
        .with_css(css.to_string());
    let (_app, window) = build_headless_app(opts).expect("build_headless_app");
    let _ = std::fs::remove_dir_all(&dir);
    window.options
}

fn approx(c: lumen_core::components::Color, want: (f32, f32, f32)) -> bool {
    (c.r - want.0).abs() < 0.01 && (c.g - want.1).abs() < 0.01 && (c.b - want.2).abs() < 0.01
}

/// No skin, no `main.css` `:root` - nothing defines `--lumen-window-bg`, so
/// the clear color must fall back to the constant, unchanged from what
/// every real launch path (`RunOptions::new`) has always rendered.
#[test]
fn falls_back_to_the_constant_with_no_token_defined() {
    let window = build("<root/>", "", "[mcp]\nport = 0\n");
    assert!(
        approx(
            window.clear,
            (DEFAULT_CLEAR.r, DEFAULT_CLEAR.g, DEFAULT_CLEAR.b,)
        ),
        "expected the DEFAULT_CLEAR fallback, got {:?}",
        window.clear
    );
}

/// `linux.css`'s light `--lumen-window-bg` (`#fafafb`) resolves once the
/// skin is active, with no app CSS involved at all - proof the clear color
/// is genuinely CSS-reachable, not just wired to the Rust constant.
#[test]
fn resolves_from_an_active_skin() {
    let window = build("<root/>", "", "[mcp]\nport = 0\n[skin]\nname = \"linux\"\n");
    assert!(
        approx(
            window.clear,
            (
                0xfa as f32 / 255.0,
                0xfa as f32 / 255.0,
                0xfb as f32 / 255.0
            )
        ),
        "expected linux.css's light --lumen-window-bg (#fafafb), got {:?}",
        window.clear
    );
}

/// The app's own `:root` overrides the active skin's `--lumen-window-bg`,
/// matching ordinary author-beats-user-agent cascade precedence.
#[test]
fn app_root_overrides_the_skins_token() {
    let window = build(
        "<root/>",
        ":root { --lumen-window-bg: #112233; }",
        "[mcp]\nport = 0\n[skin]\nname = \"linux\"\n",
    );
    assert!(
        approx(
            window.clear,
            (
                0x11 as f32 / 255.0,
                0x22 as f32 / 255.0,
                0x33 as f32 / 255.0
            )
        ),
        "app :root override did not win, got {:?}",
        window.clear
    );
}
