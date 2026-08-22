//! Headless proof that a forced color scheme survives a page navigation.
//!
//! A page is an `<if>` gate over the reserved `route.path` signal, and the
//! elements inside it carry the attributes the load-time cascade resolved.
//! Nothing re-resolved them when the gate opened, so a page reached after
//! `set_color_scheme("force-light")` mounted in the scheme the app booted
//! with while the page already on screen had flipped.

use lumen_core::components::{Fill, Visuals};
use lumen_core::nav;
use lumen_ir::artifact::{self, CompiledApp, CompiledPages};
use lumen_ir::css::{
    ColorSchemePreference, Declaration, MediaFeature, MediaQuery, Origin, Rule, Stylesheet,
};
use lumen_ir::layout_ir::{Attributes, Element, IfModeSpec, LayoutIR};
use lumen_runtime::{RunOptions, build_headless_app};
use lumen_script::node_query;

/// The DOM snapshot and the navigation bus are process-global, so the
/// headless apps that use them run one at a time.
static SERIAL: std::sync::Mutex<()> = std::sync::Mutex::new(());

fn tile(id: &str) -> Element {
    Element {
        tag: "tile".to_string(),
        attrs: Attributes {
            id: Some(id.to_string()),
            classes: vec!["cell".to_string()],
            ..Default::default()
        },
        ..Default::default()
    }
}

/// One page gate: `<if signal="route.path" eq="<key>">` around one tile.
fn page(key: &str, id: &str) -> Element {
    Element {
        tag: "if".to_string(),
        attrs: Attributes {
            if_signal: Some(nav::PATH_SIGNAL.to_string()),
            if_eq: Some(key.to_string()),
            if_mode: IfModeSpec::Render,
            ..Default::default()
        },
        children: vec![tile(id)],
        ..Default::default()
    }
}

/// One rule from a selector string and `(property, value)` pairs, optionally
/// inside an `@media` block.
fn rule(
    selector: &str,
    decls: &[(&str, &str)],
    source_order: usize,
    media: Option<MediaQuery>,
) -> Rule {
    Rule {
        selectors: lumen_ir::css::parse_selector_list(selector).expect("selector parses"),
        declarations: decls
            .iter()
            .map(|(name, value)| Declaration {
                name: (*name).to_string(),
                value: (*value).to_string(),
                important: false,
            })
            .collect(),
        origin: Origin::Author,
        source_order,
        media,
        selector: Default::default(),
    }
}

/// Whether a fill matches an `#rrggbb` literal, with the tolerance the
/// byte-to-f32 channel conversion needs.
fn near(a: lumen_core::components::Color, hex: &str) -> bool {
    let byte = |i: usize| {
        u8::from_str_radix(&hex[1 + i * 2..3 + i * 2], 16).expect("#rrggbb literal") as f32 / 255.0
    };
    (a.r - byte(0)).abs() < 0.01 && (a.g - byte(1)).abs() < 0.01 && (a.b - byte(2)).abs() < 0.01
}

struct Harness {
    app: lumen_core::app::App,
    dir: std::path::PathBuf,
    _guard: std::sync::MutexGuard<'static, ()>,
}

impl Harness {
    /// A two-page app whose `.cell` is red by default and green under
    /// `prefers-color-scheme: light`, with `script` baked in as its Rhai
    /// source.
    fn two_pages(script: &str) -> Self {
        let guard = SERIAL.lock().unwrap_or_else(|e| e.into_inner());
        let dir =
            std::env::temp_dir().join(format!("lumen_scheme_nav_{}_{}", std::process::id(), {
                static SEQ: std::sync::atomic::AtomicUsize = std::sync::atomic::AtomicUsize::new(0);
                SEQ.fetch_add(1, std::sync::atomic::Ordering::Relaxed)
            }));
        std::fs::create_dir_all(&dir).unwrap();
        // Pin the engine: the baked source below is Rhai, and the default is
        // candela. Port 0 keeps parallel test binaries off a shared socket.
        std::fs::write(
            dir.join("lumen.toml"),
            "[mcp]\nport = 0\n\n[script]\nengine = \"rhai\"\n",
        )
        .unwrap();

        let light = MediaQuery {
            features: vec![MediaFeature::PrefersColorScheme(
                ColorSchemePreference::Light,
            )],
        };
        let mut ir = LayoutIR {
            root: Element {
                tag: "root".to_string(),
                attrs: Attributes {
                    id: Some("app".to_string()),
                    ..Default::default()
                },
                children: vec![page("index", "p1"), page("other", "p2")],
                ..Default::default()
            },
            script_source: script.to_string(),
            combined_stylesheet: Some(Stylesheet {
                rules: vec![
                    rule(".cell", &[("bg", "#ff0000")], 0, None),
                    rule(".cell", &[("bg", "#00ff00")], 1, Some(light)),
                ],
            }),
            ..Default::default()
        };
        // Cascade once up front, the way `lumenc build` does: an artifact
        // carries attributes the stylesheet already resolved.
        let sheet = ir.combined_stylesheet.clone().expect("stylesheet");
        lumen_ir::css::apply_css(&mut ir, &sheet).expect("cascade");

        let bytes = artifact::serialize(&CompiledApp {
            ir,
            script_source: script.to_string(),
            pages: Some(CompiledPages {
                entry: "index".to_string(),
                keys: vec!["index".to_string(), "other".to_string()],
            }),
            ..Default::default()
        })
        .unwrap();
        let mut opts = RunOptions::new(&dir).with_artifact_bytes(bytes);
        opts.bounded = true;
        let (app, _window) = build_headless_app(opts).expect("build headless app");
        let mut h = Harness {
            app,
            dir,
            _guard: guard,
        };
        h.settle();
        h
    }

    fn settle(&mut self) {
        for _ in 0..6 {
            self.app.tick();
        }
    }

    /// The solid fill currently on the element with that id.
    fn fill(&self, id: &str) -> lumen_core::components::Color {
        let handle = node_query::run_get_by_id(id).expect("id is in the DOM index");
        let entity = lumen_core::node::NodeHandle::unpack(handle)
            .expect("live handle")
            .entity;
        self.app
            .world
            .get::<Visuals>(entity)
            .and_then(|v| v.fill.as_ref())
            .and_then(Fill::as_solid)
            .expect("cascaded fill")
    }
}

impl Drop for Harness {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.dir);
    }
}

/// The page the app boots on follows a forced scheme, and so does the one
/// navigation mounts afterwards.
#[test]
fn a_forced_scheme_survives_navigation() {
    let mut h = Harness::two_pages(r#"fn on_start() { set_color_scheme("force-light"); }"#);
    assert!(
        near(h.fill("p1"), "#00ff00"),
        "the entry page follows the forced scheme"
    );

    nav::navigate("other");
    h.settle();
    assert!(
        near(h.fill("p2"), "#00ff00"),
        "a page mounted after the scheme was forced follows it too"
    );
}

/// Forcing the scheme after the first navigation still reaches a page
/// mounted later, so the fix is not a boot-time coincidence.
#[test]
fn a_scheme_forced_mid_run_reaches_a_later_page() {
    let mut h = Harness::two_pages(r#"fn on_start() { set_color_scheme("force-dark"); }"#);
    assert!(
        near(h.fill("p1"), "#ff0000"),
        "under a forced dark scheme the light media rule does not apply"
    );

    // What `set_color_scheme("force-light")` lands on, reached directly so
    // the flip happens mid-run rather than in `on_start`.
    h.app
        .world
        .resource_mut::<lumen_core::components::StyleManager>()
        .set_scheme(lumen_core::components::ColorScheme::ForceLight);
    h.settle();
    assert!(near(h.fill("p1"), "#00ff00"), "the live page flips");

    nav::navigate("other");
    h.settle();
    assert!(
        near(h.fill("p2"), "#00ff00"),
        "the page mounted after the flip follows it"
    );
}
