// Boots real apps through `build_headless_app` / `RunOptions`, which
// lumenc only exposes under the `dev-run` feature. Gate the whole file so
// a thin (`--no-default-features`) `--all-targets` build compiles it out
// instead of failing on the missing symbols.
#![cfg(feature = "dev-run")]

//! Golden-image (screenshot regression) suite.
//!
//! Each case builds a small markup+CSS app in a temp dir, runs the full
//! headless pipeline in-process (same plugin stack as `lumenc run`, no
//! window - see [`lumenc::build_headless_app`]), renders through the
//! offscreen wgpu+vello renderer, reads the framebuffer back, and
//! compares against a checked-in PNG under `tests/goldens/`.
//!
//! - Update mode: `LUMEN_GOLDEN_UPDATE=1 cargo test -p lumenc --test golden`
//!   rewrites the goldens instead of asserting. See `tests/goldens/README.md`.
//! - No GPU adapter, only a software one, or a machine whose fonts are not the
//!   baselines' -> every test skips with a message instead of failing. See
//!   [`gpu_blocker`] and [`baseline_fonts`].
//! - Determinism audit: every capture runs twice (two fully independent
//!   app builds); the two frames must agree within [`SELF_TOLERANCE`]
//!   before the golden comparison happens.
//! - All time-based visuals (hover / press tweens, 120 ms) are settled by
//!   ticking across a wall-clock window long enough for every tween to
//!   clamp to its end state. The text caret does not blink (it is painted
//!   whenever the entity is focused), so no freeze hook is needed.

use glam::Vec2;
use image::RgbaImage;
use lumen_core::prelude::{
    App, Color, ColorScheme, Key, KeyPressed, KeyReleased, LumenId, Modifiers, MouseWheel,
    NamedKey, PointerButton, PointerMoved, PointerPressed, PointerState, PropertyStore,
    StyleManager, Transform, Viewport,
};
use lumen_render_wgpu::{WgpuRenderer, WgpuRendererPlugin, gpu_unavailable_reason};
use lumen_text_cosmic::CosmicShaper;
use lumenc::{RunOptions, build_headless_app};
use std::path::PathBuf;
use std::sync::{Mutex, OnceLock};
use std::time::{Duration, Instant};

/// Fixed logical viewport for every case. Small enough to keep goldens
/// light, large enough for a widget plus surrounding context.
const VIEW_W: u32 = 400;
const VIEW_H: u32 = 300;

/// Comparison thresholds. GPU rasterization is not bit-stable across
/// driver / vello versions, so byte-equality is the wrong bar:
///
/// - `max_channel_delta`: per-pixel, per-channel absolute difference that
///   is ignored entirely. 4/255 absorbs sRGB rounding and minor
///   anti-aliasing coverage jitter while staying far below any visible
///   color change.
/// - `max_diff_fraction`: fraction of pixels allowed to exceed the
///   channel delta. 0.1 % of 400x300 = 120 px - enough for AA edges to
///   drift by a sub-pixel after a driver update, small enough that any
///   real layout / color regression (which flips whole widget areas)
///   fails loudly. Caveat: regressions confined to < ~120 px (e.g. the
///   caret alone) can slip under this bar; they are still caught by the
///   focus outline / surrounding state in the same golden.
struct Tolerance {
    max_channel_delta: u8,
    max_diff_fraction: f64,
}

/// Golden comparison: tolerant of cross-driver rasterization drift.
const GOLDEN_TOLERANCE: Tolerance = Tolerance {
    max_channel_delta: 4,
    max_diff_fraction: 0.001,
};

/// Self-consistency (same process, same device, back-to-back builds)
/// must be much tighter - any real nondeterminism should fail here, not
/// get silently absorbed by the golden tolerance.
const SELF_TOLERANCE: Tolerance = Tolerance {
    max_channel_delta: 2,
    max_diff_fraction: 0.0002,
};

/// Settle window after driving input. Longest time-based visual is the
/// 120 ms hover/press tween; 700 ms of wall-clock ticking clamps every
/// tween/transition to its end state with a wide margin.
const SETTLE_AFTER_INPUT_MS: u64 = 700;

/// Settle window after app build (first layout, style reapply, seeds).
const SETTLE_AFTER_BUILD_MS: u64 = 250;

// --- GPU probe / paths ------------------------------------------------------

/// Probe the adapter once per test binary; `Some(reason)` means no pixel work
/// here. The baselines are hardware renders, and Direct3D's WARP rasterizer
/// faults the process partway through offscreen rendering.
fn gpu_blocker() -> Option<&'static str> {
    static PROBE: OnceLock<Option<String>> = OnceLock::new();
    PROBE.get_or_init(gpu_unavailable_reason).as_deref()
}

/// Whether this machine can be compared against the checked-in baselines.
///
/// The baselines carry one machine's font set. Text is shaped with whatever the
/// system resolves for the default sans-serif, so a machine that resolves a
/// different face draws different glyphs and every case containing text lands
/// far outside [`GOLDEN_TOLERANCE`] while the shape-only cases still pass. That
/// is what CI runners do: Linux and macOS runners disagree with the baselines by
/// nearly the same amount on the same cases even though their renderers share
/// nothing, which points at the font rather than the rasterizer. Until the
/// harness pins its own font file, these stay a local guard and skip on CI.
fn baseline_fonts() -> bool {
    std::env::var_os("CI").is_none()
}

fn goldens_dir() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("tests")
        .join("goldens")
}

/// Scratch directory for actual/diff PNGs on mismatch.
fn failure_dir() -> PathBuf {
    let base = std::env::var_os("CARGO_TARGET_DIR")
        .map(PathBuf::from)
        .unwrap_or_else(std::env::temp_dir);
    base.join("lumen-golden-failures")
}

fn update_mode() -> bool {
    std::env::var("LUMEN_GOLDEN_UPDATE").is_ok_and(|v| v == "1")
}

/// Captures run serialized: each builds its own wgpu device + font
/// system, and the settle loops measure wall-clock time - parallel test
/// threads would add contention without adding coverage.
fn capture_lock() -> &'static Mutex<()> {
    static LOCK: OnceLock<Mutex<()>> = OnceLock::new();
    LOCK.get_or_init(|| Mutex::new(()))
}

// --- Harness ----------------------------------------------------------------

/// Build the full lumenc plugin stack headless, add the offscreen wgpu
/// renderer, drive the case, and read back the framebuffer.
fn capture_once(
    name: &str,
    run: u32,
    markup: &str,
    css: &str,
    drive: &dyn Fn(&mut App),
) -> RgbaImage {
    // Temp app dir: only `lumen.toml` lives on disk (markup/CSS are passed
    // in-memory). `[mcp] port = 0` keeps the introspection server from
    // binding a TCP port per test.
    let dir =
        std::env::temp_dir().join(format!("lumen-golden-{name}-{}-r{run}", std::process::id()));
    std::fs::create_dir_all(&dir).expect("create temp app dir");
    std::fs::write(
        dir.join("lumen.toml"),
        "[mcp]\nport = 0\n\n[script]\nengine = \"rhai\"\n",
    )
    .expect("write lumen.toml");

    let mut opts = RunOptions::new(&dir).with_markup(markup).with_css(css);
    opts.hot_reload = false;
    opts.size = (VIEW_W, VIEW_H);

    let (mut app, _window) = build_headless_app(opts).expect("build headless app");
    app.add_plugin(WgpuRendererPlugin::new(VIEW_W, VIEW_H).with_text_shaper(CosmicShaper::new()));

    // Pin every environment-dependent knob before the first tick:
    // fixed 400x300 logical viewport at dpr 1 in BOTH worlds, and a
    // forced color scheme so the OS light/dark preference can't leak
    // into `@media (prefers-color-scheme)` rules.
    let clear = Color::rgb(0.07, 0.07, 0.09);
    for vp in [
        &mut *app.world.resource_mut::<Viewport>(),
        &mut *app.render_world.resource_mut::<Viewport>(),
    ] {
        vp.size = Vec2::new(VIEW_W as f32, VIEW_H as f32);
        vp.scale_factor = 1.0;
        vp.clear = clear;
    }
    app.world
        .resource_mut::<StyleManager>()
        .set_scheme(ColorScheme::ForceDark);

    settle(&mut app, SETTLE_AFTER_BUILD_MS);
    drive(&mut app);
    settle(&mut app, SETTLE_AFTER_INPUT_MS);

    let pixels = {
        let renderer = app
            .render_world
            .get_non_send::<WgpuRenderer>()
            .expect("offscreen renderer present");
        renderer.read_rgba8().expect("framebuffer readback")
    };
    drop(app);
    let _ = std::fs::remove_dir_all(&dir);

    RgbaImage::from_raw(VIEW_W, VIEW_H, pixels).expect("raw pixel buffer size")
}

/// Tick across `ms` of wall-clock time so `Instant`-based tweens
/// (hover/press 120 ms, CSS transitions) clamp to their end state.
fn settle(app: &mut App, ms: u64) {
    let deadline = Instant::now() + Duration::from_millis(ms);
    while Instant::now() < deadline {
        app.tick();
        std::thread::sleep(Duration::from_millis(8));
    }
    // A couple of trailing ticks so state written on the last timed tick
    // (marker components, signal mirrors) reaches extract + render.
    app.tick();
    app.tick();
}

fn tick(app: &mut App) {
    app.tick();
}

// --- Input drivers (same messages the winit backend / MCP simulate emit) ----

/// Center of the entity carrying `LumenId(id)`, post-layout.
fn find_center(app: &mut App, id: &str) -> Vec2 {
    let mut q = app.world.query::<(&LumenId, &Transform)>();
    for (lid, tf) in q.iter(&app.world) {
        if lid.0 == id {
            return tf.absolute + tf.size * 0.5;
        }
    }
    panic!("no entity with id {id:?} (or layout has not run)");
}

/// `hit_test` reads the [`PointerState`] resource (not the message ring),
/// so synthetic moves must mirror the winit backend / MCP simulate and
/// update both. See `lumen-mcp`'s `drain_simulate_queue` for the same
/// dual-write.
fn set_pointer(app: &mut App, pos: Vec2) {
    app.world.resource_mut::<PointerState>().position = Some(pos);
}

fn pointer_move(app: &mut App, pos: Vec2) {
    set_pointer(app, pos);
    app.world.write_message(PointerMoved { position: pos });
    tick(app);
}

/// Press without release - drives the `:active` / `Pressed` state.
fn pointer_press(app: &mut App, pos: Vec2) {
    set_pointer(app, pos);
    app.world.resource_mut::<PointerState>().primary_down = true;
    app.world.write_message(PointerMoved { position: pos });
    app.world.write_message(PointerPressed {
        position: pos,
        button: PointerButton::Primary,
    });
    tick(app);
}

fn key_tap(app: &mut App, key: Key) {
    app.world.write_message(KeyPressed {
        key: key.clone(),
        modifiers: Modifiers::default(),
        repeat: false,
    });
    app.world.write_message(KeyReleased {
        key,
        modifiers: Modifiers::default(),
    });
    tick(app);
}

fn press_tab(app: &mut App) {
    key_tap(app, Key::Named(NamedKey::Tab));
}

fn type_str(app: &mut App, text: &str) {
    for ch in text.chars() {
        key_tap(app, Key::Character(ch.to_string()));
    }
}

fn wheel(app: &mut App, pos: Vec2, delta: Vec2) {
    set_pointer(app, pos);
    app.world.write_message(MouseWheel {
        delta,
        position: pos,
    });
    tick(app);
}

/// Write a global string signal (what `<dialog open=...>`, `<if signal=...>`,
/// and dropdown open-state read).
fn set_signal(app: &mut App, name: &str, value: &str) {
    app.world
        .resource_mut::<PropertyStore>()
        .set_global_str(name, value);
    tick(app);
}

// --- Comparison -------------------------------------------------------------

struct DiffStats {
    differing: usize,
    total: usize,
    max_delta: u8,
}

impl DiffStats {
    fn fraction(&self) -> f64 {
        self.differing as f64 / self.total as f64
    }
    fn within(&self, tol: &Tolerance) -> bool {
        self.fraction() <= tol.max_diff_fraction
    }
}

/// Count pixels whose max per-channel delta exceeds `tol.max_channel_delta`,
/// and build a heatmap (red intensity = delta) for failure triage.
fn diff_images(a: &RgbaImage, b: &RgbaImage, tol: &Tolerance) -> (DiffStats, RgbaImage) {
    assert_eq!(a.dimensions(), b.dimensions(), "image dimensions differ");
    let (w, h) = a.dimensions();
    let mut heat = RgbaImage::new(w, h);
    let mut differing = 0usize;
    let mut max_delta = 0u8;
    for (pa, (pb, ph)) in a.pixels().zip(b.pixels().zip(heat.pixels_mut())) {
        let delta =
            pa.0.iter()
                .zip(pb.0.iter())
                .map(|(&x, &y)| x.abs_diff(y))
                .max()
                .unwrap_or(0);
        max_delta = max_delta.max(delta);
        if delta > tol.max_channel_delta {
            differing += 1;
            ph.0 = [255, 255u8.saturating_sub(delta.saturating_mul(4)), 0, 255];
        } else {
            // Dimmed grayscale of the golden for spatial context.
            let g = pa.0[0] / 4;
            ph.0 = [g, g, g, 255];
        }
    }
    (
        DiffStats {
            differing,
            total: (w * h) as usize,
            max_delta,
        },
        heat,
    )
}

// --- Case runner ------------------------------------------------------------

fn run_case(name: &str, markup: &str, css: &str, drive: &dyn Fn(&mut App)) {
    if let Some(why) = gpu_blocker() {
        eprintln!("golden[{name}]: SKIP - {why}");
        return;
    }
    if !baseline_fonts() {
        eprintln!(
            "golden[{name}]: SKIP - CI runner; its default sans-serif is not the \
             one the baselines were captured with"
        );
        return;
    }
    let _guard = capture_lock().lock().unwrap_or_else(|p| p.into_inner());

    // Determinism audit: two fully independent builds must agree.
    let first = capture_once(name, 0, markup, css, drive);
    let second = capture_once(name, 1, markup, css, drive);
    let (self_stats, _) = diff_images(&first, &second, &SELF_TOLERANCE);
    assert!(
        self_stats.within(&SELF_TOLERANCE),
        "golden[{name}]: NONDETERMINISTIC capture - two in-process runs differ by \
         {} px ({:.4}%), max channel delta {}. Fix the case (unfinished animation, \
         wall-clock leak) before comparing to a golden.",
        self_stats.differing,
        self_stats.fraction() * 100.0,
        self_stats.max_delta,
    );

    let golden_path = goldens_dir().join(format!("{name}.png"));
    if update_mode() {
        std::fs::create_dir_all(goldens_dir()).expect("create goldens dir");
        first.save(&golden_path).expect("write golden");
        eprintln!("golden[{name}]: UPDATED {}", golden_path.display());
        return;
    }

    let golden = image::open(&golden_path)
        .unwrap_or_else(|e| {
            panic!(
                "golden[{name}]: cannot read {} ({e}). Capture baselines with \
                 LUMEN_GOLDEN_UPDATE=1 cargo test -p lumenc --test golden",
                golden_path.display()
            )
        })
        .to_rgba8();

    let (stats, heat) = diff_images(&golden, &first, &GOLDEN_TOLERANCE);
    if !stats.within(&GOLDEN_TOLERANCE) {
        let out = failure_dir().join(name);
        std::fs::create_dir_all(&out).expect("create failure dir");
        let actual_path = out.join("actual.png");
        let diff_path = out.join("diff.png");
        first.save(&actual_path).expect("write actual");
        heat.save(&diff_path).expect("write diff heatmap");
        panic!(
            "golden[{name}]: MISMATCH - {} px differ ({:.4}% > {:.4}%), max channel \
             delta {}.\n  expected: {}\n  actual:   {}\n  diff:     {}\n  (intentional \
             change? re-baseline with LUMEN_GOLDEN_UPDATE=1)",
            stats.differing,
            stats.fraction() * 100.0,
            GOLDEN_TOLERANCE.max_diff_fraction * 100.0,
            stats.max_delta,
            golden_path.display(),
            actual_path.display(),
            diff_path.display(),
        );
    }
}

/// No-op driver for pure-markup cases.
fn no_drive(_: &mut App) {}

// --- Cases ------------------------------------------------------------------

/// Button state matrix: keyboard-focused (first in tab order), idle,
/// pointer-hovered, and disabled - all in one frame. `:active` needs the
/// same (single) pointer, so the pressed state is its own case below.
#[test]
fn golden_button_states() {
    run_case(
        "button_states",
        r##"<root skin="default" bg="#11141b">
  <column padding="16" gap="10">
    <button id="b-focus" width="180px" text="Focused" />
    <button id="b-idle" width="180px" text="Idle" />
    <button id="b-hover" width="180px" text="Hovered" />
    <button id="b-disabled" width="180px" disabled="true" text="Disabled" />
  </column>
</root>"##,
        "",
        &|app| {
            press_tab(app); // focus lands on b-focus (first focusable)
            let hover = find_center(app, "b-hover");
            pointer_move(app, hover);
        },
    );
}

/// Pointer held down on a button: `:active` fill + press tint.
#[test]
fn golden_button_pressed() {
    run_case(
        "button_pressed",
        r##"<root skin="default" bg="#11141b">
  <column padding="16">
    <button id="b" width="180px" text="Pressed" />
  </column>
</root>"##,
        "",
        &|app| {
            let center = find_center(app, "b");
            pointer_press(app, center);
        },
    );
}

/// Toggle track + knob: unchecked, checked (accent track, knob at far
/// end), and disabled.
#[test]
fn golden_toggle_states() {
    run_case(
        "toggle_states",
        r##"<root skin="default" bg="#11141b">
  <column padding="16" gap="14">
    <toggle id="t-off" width="64px" height="36px" />
    <toggle id="t-on" width="64px" height="36px" checked="true" />
    <toggle id="t-disabled" width="64px" height="36px" disabled="true" />
  </column>
</root>"##,
        "",
        &no_drive,
    );
}

/// Switch track + thumb: off (thumb parked left, gray track), on (accent
/// track, thumb slid to the far end), and disabled. The 140 ms thumb slide
/// is fully settled by the harness's 700 ms tick window, so the frame is
/// time-stable.
#[test]
fn golden_switch_states() {
    run_case(
        "switch_states",
        r##"<root skin="default" bg="#11141b">
  <column padding="16" gap="14">
    <switch id="sw-off" width="52px" height="28px" />
    <switch id="sw-on" width="52px" height="28px" checked="true" />
    <switch id="sw-disabled" width="52px" height="28px" disabled="true" />
  </column>
</root>"##,
        "",
        &no_drive,
    );
}

/// Slider thumb at 0 / 50 / 100 along a 240px track.
#[test]
fn golden_slider_values() {
    run_case(
        "slider_values",
        r##"<root skin="default" bg="#11141b">
  <column padding="16" gap="20">
    <slider id="s0" width="240px" min="0" max="100" value="0" />
    <slider id="s50" width="240px" min="0" max="100" value="50" />
    <slider id="s100" width="240px" min="0" max="100" value="100" />
  </column>
</root>"##,
        "",
        &no_drive,
    );
}

/// Text input trio: placeholder (unfocused), signal-bound value
/// (unfocused), and focused with typed text + caret + focus outline.
/// The caret does not blink - it paints whenever focused - so the frame
/// is time-stable.
#[test]
fn golden_text_input() {
    run_case(
        "text_input",
        r##"<root skin="default" bg="#11141b">
  <column padding="16" gap="12">
    <input id="i-ph" width="260px" placeholder="Placeholder text" />
    <input id="i-val" width="260px" bind-text="v" />
    <input id="i-focus" width="260px" placeholder="Focus me" />
  </column>
</root>"##,
        "",
        &|app| {
            set_signal(app, "v", "Hello world");
            press_tab(app);
            press_tab(app);
            press_tab(app); // third focusable = i-focus
            type_str(app, "Hi");
        },
    );
}

/// Dropdown closed: header button shows the placeholder, content below
/// is unobscured.
#[test]
fn golden_dropdown_closed() {
    run_case("dropdown_closed", DROPDOWN_MARKUP, "", &no_drive);
}

/// Dropdown open: options panel overlays the content band below the
/// header (paint-order + popup regression).
#[test]
fn golden_dropdown_open() {
    run_case("dropdown_open", DROPDOWN_MARKUP, "", &|app| {
        set_signal(app, "__dropdown_open:choice", "true");
    });
}

// The placeholder ends in a horizontal ellipsis glyph. The checked-in goldens
// were captured with it, so it is spelled as an escape rather than `...`.
const DROPDOWN_MARKUP: &str = concat!(
    r##"<root skin="default" bg="#11141b">
  <column padding="16" gap="8">
    <column width="240px">
      <dropdown bind-value="choice" placeholder="Select"##,
    "\u{2026}",
    r##"">
        <option value="a" label="Alpha" />
        <option value="b" label="Beta" />
        <option value="c" label="Gamma" />
      </dropdown>
    </column>
    <label text="Content below the dropdown" text-color="#a9b8c9" />
    <tile width="368px" height="80px" bg="#26466d" radius="8" />
  </column>
</root>"##
);

/// Tabs: strip with the first tab selected (accent fill) and its body
/// visible.
#[test]
fn golden_tabs() {
    run_case(
        "tabs",
        r##"<root skin="default" bg="#11141b">
  <column padding="16">
    <tabs bind-value="tb">
      <tab name="one" label="First">
        <column padding="12">
          <tile width="140px" height="60px" bg="#9ece6a" radius="8" />
        </column>
      </tab>
      <tab name="two" label="Second">
        <column padding="12">
          <tile width="140px" height="60px" bg="#f7768e" radius="8" />
        </column>
      </tab>
    </tabs>
  </column>
</root>"##,
        "",
        &no_drive,
    );
}

/// Modal dialog open: backdrop dims the underlying content, surface card
/// centered on top.
#[test]
fn golden_dialog_open() {
    run_case(
        "dialog_open",
        r##"<root skin="default" bg="#11141b">
  <column padding="16" gap="8">
    <label text="Underlying content" text-color="#a9b8c9" />
    <tile width="368px" height="200px" bg="#26466d" radius="8" />
  </column>
  <dialog open="dlg">
    <column class="dialog-surface" width="280px" gap="10">
      <label text="Modal dialog" text-color="#ffffff" />
      <button width="120px" text="OK" />
    </column>
  </dialog>
</root>"##,
        "",
        &|app| {
            set_signal(app, "dlg", "1");
        },
    );
}

/// Scroll container after a wheel scroll: content offset + clipped at
/// the container bounds. `inertia="0"` applies the delta immediately -
/// no fling animation to race the capture.
#[test]
fn golden_scroll_offset() {
    run_case(
        "scroll_offset",
        r##"<root skin="default" bg="#11141b">
  <column padding="16">
    <scroll id="sc" width="200px" height="160px" sensitivity="1" inertia="0">
      <column>
        <tile width="180px" height="40px" bg="#7aa2f7" />
        <tile width="180px" height="40px" bg="#bb9af7" />
        <tile width="180px" height="40px" bg="#9ece6a" />
        <tile width="180px" height="40px" bg="#e0af68" />
        <tile width="180px" height="40px" bg="#f7768e" />
        <tile width="180px" height="40px" bg="#33c7ce" />
      </column>
    </scroll>
  </column>
</root>"##,
        "",
        &|app| {
            let center = find_center(app, "sc");
            wheel(app, center, Vec2::new(0.0, -60.0));
        },
    );
}

/// Single-line truncation: a fixed-width label whose text cannot fit
/// gets the ellipsis pass; the short control line stays intact.
///
/// NOTE: at baseline time the long line renders UNCLIPPED past its
/// 180px box (no truncation reaches the draw path). The golden captures
/// current behavior - when ellipsis lands, this golden shifts and must
/// be re-baselined deliberately.
#[test]
fn golden_text_ellipsis() {
    run_case(
        "text_ellipsis",
        r##"<root skin="default" bg="#11141b">
  <column padding="16" gap="10">
    <label width="180px" text="This is a very long single line that must truncate" text-color="#ffffff" />
    <label text="Short line" text-color="#ffffff" />
  </column>
</root>"##,
        "",
        &no_drive,
    );
}

/// Corner radius + drop shadow tile row (sharp, rounded, pill+shadow).
#[test]
fn golden_decor_tiles() {
    run_case(
        "decor_tiles",
        r##"<root skin="default" bg="#11141b">
  <row padding="24" gap="20">
    <tile width="90px" height="90px" bg="#7aa2f7" />
    <tile width="90px" height="90px" bg="#bb9af7" radius="14" />
    <tile width="90px" height="90px" bg="#9ece6a" radius="45" shadow="0 8 20 #000000aa" />
  </row>
</root>"##,
        "",
        &no_drive,
    );
}

/// Gradient fills: linear, radial, conic.
///
/// NOTE: at baseline time the conic tile renders as a solid first-stop
/// fill and `linear-gradient(90deg, ...)` paints top->bottom rather than
/// left->right; the golden captures current behavior.
#[test]
fn golden_gradient_tiles() {
    run_case(
        "gradient_tiles",
        r##"<root skin="default" bg="#11141b">
  <row padding="24" gap="16">
    <tile width="100px" height="100px" bg="linear-gradient(90deg, #bb9af7, #f7768e)" radius="8" />
    <tile width="100px" height="100px" bg="radial-gradient(#7aa2f7, #1a1b26)" radius="8" />
    <tile width="100px" height="100px" bg="conic-gradient(from 0deg, #9ece6a, #e0af68, #9ece6a)" radius="8" />
  </row>
</root>"##,
        "",
        &no_drive,
    );
}

/// Overlay stacking: an `<overlay>` child paints above earlier siblings
/// (document-order z regression - Lumen has no z-index property).
#[test]
fn golden_overlay_stacking() {
    run_case(
        "overlay_stacking",
        r##"<root skin="default" bg="#11141b">
  <column padding="16" gap="8">
    <tile width="220px" height="110px" bg="#9ece6a" radius="8" />
    <tile width="220px" height="110px" bg="#e0af68" radius="8" />
  </column>
  <overlay>
    <column padding="60">
      <tile width="180px" height="90px" bg="#f7768eee" radius="12" />
    </column>
  </overlay>
</root>"##,
        "",
        &no_drive,
    );
}

/// Nested group opacity: 0.6 outer x 0.5 inner over an opaque leaf.
///
/// NOTE: at baseline time opacity applies to each element's own fill
/// (the two container fills blend correctly) but does not composite
/// down onto the opaque leaf, which renders solid white; the golden
/// captures current behavior.
#[test]
fn golden_opacity_nesting() {
    run_case(
        "opacity_nesting",
        r##"<root skin="default" bg="#11141b">
  <column padding="16">
    <column class="o-outer" width="240px" padding="24" radius="10">
      <column class="o-inner" width="160px" padding="24" radius="10">
        <tile width="80px" height="40px" bg="#ffffff" />
      </column>
    </column>
  </column>
</root>"##,
        r#".o-outer { bg: #7aa2f7; opacity: 0.6; }
.o-inner { bg: #f7768e; opacity: 0.5; }"#,
        &no_drive,
    );
}
