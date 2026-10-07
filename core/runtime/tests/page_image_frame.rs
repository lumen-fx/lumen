//! An image the previous page already loaded paints in the first frame of
//! the page navigation mounts.
//!
//! A page is an `<if>` gate over the reserved `route.path` signal. Two pages
//! showing the same `<image>` used to flash the window blank on every link
//! click: the new page's image element reached the painter one frame before
//! the cached bitmap was attached to it, so the first frame after the swap
//! had the old page gone and nothing in its place.

use lumen_assets::LoadedImage;
use lumen_core::nav;
use lumen_core::render_world::ExtractedImage;
use lumen_ir::artifact::{self, CompiledApp, CompiledPages};
use lumen_ir::layout_ir::{Attributes, Element, IfModeSpec, LayoutIR};
use lumen_runtime::{RunOptions, build_headless_app};

/// A 1x1 red PNG. Small enough to inline, real enough for the decoder.
const RED_DOT_PNG: &[u8] = &[
    0x89, 0x50, 0x4e, 0x47, 0x0d, 0x0a, 0x1a, 0x0a, 0x00, 0x00, 0x00, 0x0d, 0x49, 0x48, 0x44, 0x52,
    0x00, 0x00, 0x00, 0x01, 0x00, 0x00, 0x00, 0x01, 0x08, 0x02, 0x00, 0x00, 0x00, 0x90, 0x77, 0x53,
    0xde, 0x00, 0x00, 0x00, 0x0c, 0x49, 0x44, 0x41, 0x54, 0x78, 0x9c, 0x63, 0xf8, 0xcf, 0xc0, 0x00,
    0x00, 0x03, 0x01, 0x01, 0x00, 0xc9, 0xfe, 0x92, 0xef, 0x00, 0x00, 0x00, 0x00, 0x49, 0x45, 0x4e,
    0x44, 0xae, 0x42, 0x60, 0x82,
];

/// The navigation bus and the DOM snapshot are process-global, so the
/// headless apps that use them run one at a time.
static SERIAL: std::sync::Mutex<()> = std::sync::Mutex::new(());

/// One page gate: `<if signal="route.path" eq="<key>">` around an `<image>`.
fn page(key: &str, src: &str) -> Element {
    let image = Element {
        tag: "image".to_string(),
        attrs: Attributes {
            id: Some(format!("{key}-image")),
            src: Some(src.to_string()),
            ..Default::default()
        },
        ..Default::default()
    };
    Element {
        tag: "if".to_string(),
        attrs: Attributes {
            if_signal: Some(nav::PATH_SIGNAL.to_string()),
            if_eq: Some(key.to_string()),
            if_mode: IfModeSpec::Render,
            ..Default::default()
        },
        children: vec![image],
        ..Default::default()
    }
}

/// How many images the render world holds for the current frame.
fn painted_images(app: &mut lumen_core::app::App) -> usize {
    let mut q = app.render_world.query::<&ExtractedImage>();
    q.iter(&app.render_world).count()
}

#[test]
fn a_cached_image_paints_in_the_first_frame_of_a_new_page() {
    let _guard = SERIAL.lock().unwrap_or_else(|e| e.into_inner());
    let dir = std::env::temp_dir().join(format!("lumen_page_image_{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    std::fs::write(dir.join("lumen.toml"), "[mcp]\nport = 0\n").unwrap();
    let png = dir.join("panorama.png");
    std::fs::write(&png, RED_DOT_PNG).unwrap();
    let src = png.to_string_lossy().into_owned();

    let ir = LayoutIR {
        root: Element {
            tag: "root".to_string(),
            children: vec![page("index", &src), page("other", &src)],
            ..Default::default()
        },
        ..Default::default()
    };
    let bytes = artifact::serialize(&CompiledApp {
        ir,
        pages: Some(CompiledPages {
            entry: "index".to_string(),
            keys: vec!["index".to_string(), "other".to_string()],
        }),
        ..Default::default()
    })
    .unwrap();
    let mut opts = RunOptions::new(&dir).with_artifact_bytes(bytes);
    opts.bounded = true;
    let (mut app, _window) = build_headless_app(opts).expect("build headless app");

    // Let the entry page's decode land and reach the painter.
    let mut loaded = false;
    for _ in 0..200 {
        app.tick();
        let mut q = app.world.query::<&LoadedImage>();
        if q.iter(&app.world).next().is_some() && painted_images(&mut app) == 1 {
            loaded = true;
            break;
        }
        std::thread::sleep(std::time::Duration::from_millis(5));
    }
    assert!(loaded, "the entry page's image never painted");

    // One tick: the swap and the frame it presents.
    nav::navigate("other");
    app.tick();
    assert_eq!(
        painted_images(&mut app),
        1,
        "the first frame after the page swap must paint the cached image"
    );

    // And back, the same way.
    nav::navigate("index");
    app.tick();
    assert_eq!(
        painted_images(&mut app),
        1,
        "navigating back paints the cached image in its first frame too"
    );

    drop(app);
    let _ = std::fs::remove_dir_all(&dir);
}
