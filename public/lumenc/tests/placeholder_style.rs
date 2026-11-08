// Drives the real pipeline (parse -> cascade -> spawn -> layout -> render
// extract), which needs `RunOptions` / `build_headless_app`; lumenc only
// exposes those under `dev-run`.
#![cfg(feature = "dev-run")]

//! How an empty field draws its placeholder.
//!
//! The hint is dimmed by default, from the theme's `--lumen-placeholder`
//! token or the user-agent gray, and a `::placeholder` rule restyles it
//! without touching the field's own text. What the renderer is handed is the
//! answer, so these tests read the render extract. Headless: no window, no
//! GPU.

use bevy_ecs::prelude::*;
use lumen_core::app::App;
use lumen_core::components::{LumenClasses, LumenId, StyleDirty, TextContent};
use lumen_core::render_world::{ExtractedText, RenderEntityMap, extract_text};
use lumenc::RunOptions;
use lumenc::run::build_headless_app;

/// One app at a time: the headless apps share process-global buses.
static SERIAL: std::sync::Mutex<()> = std::sync::Mutex::new(());

struct Harness {
    app: App,
    _guard: std::sync::MutexGuard<'static, ()>,
}

impl Harness {
    fn new(markup: &str, css: &str) -> Self {
        let guard = SERIAL.lock().unwrap_or_else(|e| e.into_inner());
        let dir =
            std::env::temp_dir().join(format!("lumenc_placeholder_{}_{}", std::process::id(), {
                static SEQ: std::sync::atomic::AtomicUsize = std::sync::atomic::AtomicUsize::new(0);
                SEQ.fetch_add(1, std::sync::atomic::Ordering::Relaxed)
            }));
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(dir.join("lumen.toml"), "[mcp]\nport = 0\n").unwrap();
        let mut opts = RunOptions::new(&dir)
            .with_parser(lumenc::default_parser())
            .with_markup(markup.to_string())
            .with_css(css.to_string());
        opts.hot_reload = false;
        let (app, _window) = build_headless_app(opts).expect("build_headless_app");
        let _ = std::fs::remove_dir_all(&dir);
        let mut h = Self { app, _guard: guard };
        h.ticks(4);
        h
    }

    fn ticks(&mut self, n: usize) {
        for _ in 0..n {
            self.app.tick();
        }
    }

    fn by_id(&mut self, id: &str) -> Entity {
        let mut q = self.app.world.query::<(Entity, &LumenId)>();
        q.iter(&self.app.world)
            .find(|(_, name)| name.0 == id)
            .map(|(e, _)| e)
            .unwrap_or_else(|| panic!("no element with id `{id}`"))
    }

    /// What the render extract hands the renderer for the element `id`.
    fn drawn(&mut self, id: &str) -> ExtractedText {
        let entity = self.by_id(id);
        let mut render = World::new();
        render.init_resource::<RenderEntityMap>();
        extract_text(&mut self.app.world, &mut render);
        let drawn = *render
            .resource::<RenderEntityMap>()
            .text
            .get(&entity)
            .unwrap_or_else(|| panic!("`{id}` extracted no text"));
        render
            .get::<ExtractedText>(drawn)
            .expect("extracted text")
            .clone()
    }

    fn rgba(&mut self, id: &str) -> [u8; 4] {
        self.drawn(id).fill.to_rgba8()
    }
}

#[test]
fn a_skinless_hint_draws_in_the_user_agent_gray() {
    let mut h = Harness::new(
        r##"<root><input id="f" placeholder="Search" text-color="#ffffff" /></root>"##,
        "",
    );
    let drawn = h.drawn("f");
    assert_eq!(drawn.text, "Search");
    assert_eq!(drawn.fill.to_rgba8(), [0xa9, 0xa9, 0xa9, 0xff]);
}

#[test]
fn a_skinned_hint_takes_the_theme_token() {
    let mut h = Harness::new(
        r#"<root skin="default"><input id="f" placeholder="Search" /></root>"#,
        ":root { --lumen-placeholder: #123456; }",
    );
    assert_eq!(h.rgba("f"), [0x12, 0x34, 0x56, 0xff]);
}

#[test]
fn typed_text_keeps_the_field_color() {
    let mut h = Harness::new(
        r#"<root><input id="f" placeholder="Search" /></root>"#,
        "input { color: #ff0000; }",
    );
    let field = h.by_id("f");
    h.app.world.get_mut::<TextContent>(field).unwrap().0 = "query".to_string();
    let drawn = h.drawn("f");
    assert_eq!(drawn.text, "query");
    assert_eq!(drawn.fill.to_rgba8(), [0xff, 0x00, 0x00, 0xff]);
}

#[test]
fn a_placeholder_rule_restyles_the_hint() {
    let mut h = Harness::new(
        r#"<root><input id="f" class="search" placeholder="Search" /></root>"#,
        "input { color: #ff0000; font-size: 16; } \
         .search::placeholder { color: #00ff00; opacity: 0.5; font-size: 12; font-weight: 700; }",
    );
    let drawn = h.drawn("f");
    let [r, g, b, a] = drawn.fill.to_rgba8();
    assert_eq!([r, g, b], [0x00, 0xff, 0x00]);
    assert!((126..=129).contains(&a), "half opacity, got alpha {a}");
    assert_eq!(drawn.size_px, 12.0);
    assert_eq!(drawn.weight, 700);
}

#[test]
fn a_class_change_restyles_the_hint() {
    let mut h = Harness::new(
        r#"<root><input id="f" placeholder="Search" /></root>"#,
        ".loud::placeholder { color: #00ff00; }",
    );
    assert_eq!(h.rgba("f"), [0xa9, 0xa9, 0xa9, 0xff]);
    let field = h.by_id("f");
    h.app
        .world
        .entity_mut(field)
        .insert((LumenClasses::from(vec!["loud".to_string()]), StyleDirty));
    h.ticks(2);
    assert_eq!(h.rgba("f"), [0x00, 0xff, 0x00, 0xff]);
}

#[test]
fn a_skinned_hint_follows_the_skins_muted_text() {
    let mut h = Harness::new(
        r#"<root skin="default"><input id="f" placeholder="Search" /></root>"#,
        "",
    );
    // `--lumen-text-muted` in the default skin, light and dark.
    let muted = [[0xa9, 0xb8, 0xc9, 0xff], [0xbc, 0xcb, 0xdb, 0xff]];
    let got = h.rgba("f");
    assert!(muted.contains(&got), "got {got:?}");
}
