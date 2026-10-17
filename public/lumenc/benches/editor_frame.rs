//! What one frame of a large pixel editor costs, phase by phase, with no
//! window: a canvas drawing N large pixel buffers stacked with blend modes,
//! beside a dense panel UI of about two thousand elements, on the app's real
//! pipeline (markup, CSS, layout, the canvas module, an offscreen renderer).
//!
//! ```text
//! cargo bench -p lumenc --features dev-run --bench editor_frame -- gpu
//! cargo bench -p lumenc --features dev-run --bench editor_frame -- cpu buffers=2,8,32
//! ```
//!
//! Arguments: `gpu` or `cpu` (default both), `buffers=N[,N...]` (default
//! 2,8,32), `side=N` (buffer side, default 4096), `rows=N` (list rows in the
//! side panel, default 300), `frames=N` (frames per phase, default 30).
//!
//! The phases: idle ticks, a repaint of an unchanged frame, a pan, a brush
//! stroke, rewriting the top buffer, a class edit on the root that changes
//! nothing, an edit of the tree's shape, rewriting every buffer, and hiding and freeing
//! one. Each reports the median and the worst frame, split at the instants
//! the app's `FrameTiming` records: update (input, systems, style, layout),
//! extract, encode (the renderer building its draw commands) and submit
//! (handing them to the device, or rasterizing). `gpu` is how long the device
//! then took to finish, measured by reading the frame back and subtracting a
//! readback of an unchanged frame.

use std::path::PathBuf;
use std::time::{Duration, Instant};

use bevy_ecs::prelude::*;
use lumen_canvas::{CanvasSet, CompositeOperation, Context2d, PixelBuffer};
use lumen_core::app::App;
use lumen_core::prelude::{FrameDirty, FrameTiming, TickStage, Viewport};
use lumen_core::render_backend::install_offscreen;
use lumen_render_cpu::CpuRenderer;
use lumen_render_wgpu::{WgpuRenderer, gpu_unavailable_reason};
use lumen_script::ScriptCommand;
use lumen_script::node_query;
use lumen_text::ShaperService;
use lumenc::RunOptions;
use lumenc::run::build_headless_app;

const WIDTH: u32 = 1440;
const HEIGHT: u32 = 900;

/// What the drawing system does this tick.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Action {
    /// Draw nothing.
    Idle,
    /// Redraw every visible buffer at a new offset.
    Pan,
    /// Write a small dab into the top buffer and redraw.
    Stroke,
    /// Write every pixel of every buffer and redraw: what reopening a
    /// document, or rebuilding a flattened layer stack, does.
    Rewrite,
    /// Write every pixel of the top buffer and redraw: a transform of one
    /// whole layer.
    RewriteOne,
}

#[derive(Resource)]
struct Art {
    buffers: Vec<PixelBuffer>,
    visible: Vec<bool>,
    action: Action,
    pan: f64,
    dab: i64,
}

fn draw(mut art: ResMut<Art>) {
    let action = art.action;
    match action {
        Action::Idle => return,
        Action::Pan => art.pan += 3.0,
        Action::Stroke => {
            let top = art.buffers.last().expect("a buffer");
            let at = 1000 + art.dab * 8;
            top.fill_rect(at, 1000, 8, 8, 0xff2040ff).expect("dab");
            art.dab += 1;
        }
        Action::RewriteOne => {
            art.dab += 1;
            let shade = (art.dab as u32 * 40) & 0xff;
            let top = art.buffers.last().expect("a buffer");
            let side = top.width();
            top.fill_rect(0, 0, side, side, 0x30201000 | (shade << 24) | 0xff)
                .expect("fill");
        }
        Action::Rewrite => {
            art.dab += 1;
            let shade = (art.dab as u32 * 40) & 0xff;
            for buffer in &art.buffers {
                let side = buffer.width();
                buffer
                    .fill_rect(0, 0, side, side, 0x10203000 | (shade << 24) | 0xff)
                    .expect("fill");
            }
        }
    }
    let ctx = Context2d::new("art");
    ctx.clear();
    ctx.set_image_smoothing(false);
    let (x, y, w, h) = (20.0 + art.pan, 10.0, 760.0, 760.0);
    let ops = [
        CompositeOperation::SourceOver,
        CompositeOperation::Multiply,
        CompositeOperation::Screen,
        CompositeOperation::Overlay,
    ];
    for (i, buffer) in art.buffers.iter().enumerate() {
        if !art.visible[i] {
            continue;
        }
        ctx.set_global_composite_operation(if i == 0 { ops[0] } else { ops[i % 4] });
        ctx.set_global_alpha(if i == 0 { 1.0 } else { 200.0 / 255.0 });
        ctx.draw_buffer_scaled(buffer, x, y, w, h);
    }
    ctx.set_global_composite_operation(CompositeOperation::SourceOver);
    ctx.set_global_alpha(1.0);
}

/// About two thousand elements: a menu bar, a tool column, a layer panel,
/// a long list, a property grid and a status bar around the canvas.
fn markup(rows: usize) -> String {
    let mut m = String::from("<root id=\"app\">\n<row id=\"menubar\">");
    for i in 0..16 {
        m += &format!("<button class=\"menu\" text=\"Menu {i}\"/>");
    }
    m += "<spacer/><label text=\"untitled.pixl\"/></row>\n<row id=\"options\">";
    for i in 0..12 {
        m += &format!(
            "<row class=\"option\"><label text=\"Opt {i}\"/><slider min=\"0\" max=\"100\" value=\"50\"/></row>"
        );
    }
    m += "</row>\n<row id=\"workspace\"><column id=\"tools\">";
    for i in 0..24 {
        m += &format!("<button class=\"tool\" text=\"T{i}\"/>");
    }
    m +=
        "</column><column id=\"stage\"><canvas id=\"art\" width=\"1100\" height=\"800\"/></column>";
    m += "<column id=\"panels\"><column class=\"panel\">";
    for c in 0..8 {
        m += &format!(
            "<row class=\"channel\"><label text=\"C{c}\"/><slider min=\"0\" max=\"255\" value=\"9\"/><label text=\"9\"/></row>"
        );
    }
    m += "</column><column class=\"panel\"><row class=\"swatches\">";
    for s in 0..64 {
        m += &format!("<tile class=\"swatch s{}\"/>", s % 8);
    }
    m += "</row></column><scroll id=\"layers\" class=\"list\">";
    for l in 0..32 {
        m += &format!(
            "<row class=\"layer\"><checkbox checked=\"true\"/><tile class=\"thumb\"/><label class=\"name\" text=\"Layer {l}\"/><label text=\"100%\"/></row>"
        );
    }
    m += "</scroll><scroll id=\"history\" class=\"list\">";
    for r in 0..rows {
        m += &format!(
            "<row class=\"entry\"><label class=\"name\" text=\"Step {r}\"/><spacer/><button class=\"small\" text=\"Go\"/></row>"
        );
    }
    m += "</scroll></column></row>\n<row id=\"status\">";
    for i in 0..10 {
        m += &format!("<label text=\"status {i}\"/>");
    }
    m += "</row>\n</root>\n";
    m
}

const CSS: &str = r#"
#app { background: #1e1f22; color: #dddddd; }
#menubar, #options, #status { height: 28; align: center; gap: 4; padding: 0 8; background: #2b2d31; }
.option { align: center; gap: 4; }
.option slider { width: 60; }
#workspace { grow: 1; min-height: 0; }
#tools { width: 44; gap: 2; padding: 4; background: #2b2d31; }
.tool { height: 28; }
#stage { grow: 1; min-width: 0; overflow: hidden; background: #111111; }
#stage canvas { position: absolute; inset: 0; }
#panels { width: 300; gap: 2; background: #2b2d31; }
.panel { padding: 6; gap: 4; border-bottom: 1 solid #444444; }
.channel { align: center; gap: 4; }
.channel slider { grow: 1; min-width: 0; }
.swatches { flex-wrap: wrap; gap: 2; }
.swatch { width: 14; height: 14; border-radius: 2; background: #cc3333; }
.swatch.s1 { background: #33cc33; } .swatch.s2 { background: #3333cc; } .swatch.s3 { background: #cccc33; }
.list { grow: 1; min-height: 0; }
.layer, .entry { height: 24; align: center; gap: 6; padding: 0 6; }
.layer:hover, .entry:hover { background: #35373c; }
.thumb { width: 20; height: 20; background: #555555; }
.name { grow: 1; }
"#;

fn app_dir(backend: &str) -> PathBuf {
    let dir = std::env::temp_dir().join(format!(
        "lumen-editor-frame-{backend}-{}",
        std::process::id()
    ));
    std::fs::create_dir_all(&dir).expect("scratch dir");
    std::fs::write(
        dir.join("lumen.toml"),
        format!(
            "[window]\nsize = [{WIDTH}, {HEIGHT}]\n\n[render]\nbackend = \"{backend}\"\n\n\
             [mcp]\nport = 0\n\n[dependencies]\nlumen-canvas = {{ bundled = true }}\n"
        ),
    )
    .expect("lumen.toml");
    dir
}

/// The renderer the frame lands in, kept reachable for readback.
#[derive(Clone, Copy, PartialEq, Eq)]
enum Backend {
    Gpu,
    Cpu,
}

fn read_back(app: &App) {
    if let Some(gpu) = app.render_world.get_non_send::<WgpuRenderer>() {
        std::hint::black_box(gpu.read_rgba8().expect("readback"));
    }
}

/// One frame's phases, in milliseconds.
#[derive(Clone, Copy, Default)]
struct Sample {
    total: f64,
    update: f64,
    extract: f64,
    encode: f64,
    submit: f64,
    gpu: f64,
}

fn ms(d: Duration) -> f64 {
    d.as_secs_f64() * 1000.0
}

/// One tick, then what a run loop does once the frame is up: the dirty flag
/// is consumed.
fn tick(app: &mut App) {
    app.tick();
    app.world.resource_mut::<FrameDirty>().dirty = false;
}

/// Tick once and time it. `sync` waits for the device by reading back.
fn frame(app: &mut App, readback: f64) -> Sample {
    let t0 = Instant::now();
    tick(app);
    let ticked = Instant::now();
    read_back(app);
    let synced = Instant::now();
    let t = *app.world.resource::<FrameTiming>();
    let since =
        |a: Instant, b: Option<Instant>| b.map_or(0.0, |b| ms(b.saturating_duration_since(a)));
    let extract_end = t.extract_end.unwrap_or(t.update_end);
    let encoded = t.encoded.unwrap_or(extract_end);
    Sample {
        total: ms(ticked - t0) + (ms(synced - ticked) - readback).max(0.0),
        update: ms(t.update_end.saturating_duration_since(t.start)),
        extract: since(t.update_end, t.extract_end),
        encode: since(extract_end, t.encoded),
        submit: since(encoded, t.submitted),
        gpu: (ms(synced - ticked) - readback).max(0.0),
    }
}

/// One column of the report: its label and the phase it reads.
type Column = (&'static str, fn(&Sample) -> f64);

fn report(name: &str, samples: &[Sample]) {
    let pick = |f: fn(&Sample) -> f64| {
        let mut v: Vec<f64> = samples.iter().map(f).collect();
        v.sort_by(f64::total_cmp);
        (v[v.len() / 2], *v.last().unwrap_or(&0.0))
    };
    let cols: [Column; 6] = [
        ("total", |s| s.total),
        ("update", |s| s.update),
        ("extract", |s| s.extract),
        ("encode", |s| s.encode),
        ("submit", |s| s.submit),
        ("gpu", |s| s.gpu),
    ];
    let mut line = format!("  {name:<16}");
    for (label, f) in cols {
        let (median, worst) = pick(f);
        line += &format!(" {label} {median:>6.2}/{worst:<7.2}");
    }
    println!("{line}");
}

fn run(backend: Backend, buffers: usize, side: u32, rows: usize, frames: usize) {
    lumen_canvas::store::reset();
    let name = if backend == Backend::Gpu {
        "gpu"
    } else {
        "cpu"
    };
    let dir = app_dir(name);
    let opts = RunOptions::new(&dir)
        .with_parser(lumenc::default_parser())
        .with_markup(markup(rows))
        .with_css(CSS);
    let (mut app, mut window) = build_headless_app(opts).expect("the app builds");
    match backend {
        Backend::Gpu => install_offscreen(
            &mut app,
            WgpuRenderer::new_offscreen(WIDTH, HEIGHT).expect("a GPU"),
        ),
        Backend::Cpu => install_offscreen(&mut app, CpuRenderer::new_offscreen(WIDTH, HEIGHT)),
    }
    if let Some(shaper) = window.text_shaper.take() {
        app.render_world
            .insert_non_send(ShaperService::from(shaper));
    }
    for world in [&mut app.world, &mut app.render_world] {
        let mut vp = world.resource_mut::<Viewport>();
        vp.size = glam::Vec2::new(WIDTH as f32, HEIGHT as f32);
        vp.scale_factor = 1.0;
    }

    let t = Instant::now();
    let mut list = Vec::with_capacity(buffers);
    for i in 0..buffers {
        let buffer = PixelBuffer::new(side, side).expect("within the caps");
        for ty in 0..side.div_ceil(64) {
            for tx in 0..side.div_ceil(64) {
                let alpha = if (tx + ty + i as u32).is_multiple_of(5) {
                    0
                } else {
                    255
                };
                let rgba = u32::from_be_bytes([
                    (tx * 4 + i as u32 * 7) as u8,
                    (ty * 4) as u8,
                    (i * 8) as u8,
                    alpha,
                ]);
                buffer
                    .fill_rect(i64::from(tx * 64), i64::from(ty * 64), 64, 64, rgba)
                    .expect("fill");
            }
        }
        list.push(buffer);
    }
    let filled = t.elapsed();
    app.world.insert_resource(Art {
        visible: vec![true; buffers],
        buffers: list,
        action: Action::Pan,
        pan: 0.0,
        dab: 0,
    });
    app.add_systems(TickStage::Systems, draw.before(CanvasSet::Encode));

    // The first frames adopt the canvas and upload every buffer.
    let t = Instant::now();
    let mut first = Sample::default();
    for i in 0..3 {
        let s = frame(&mut app, 0.0);
        if i == 1 {
            first = s;
        }
    }
    let warm = t.elapsed();
    let elements = app
        .world
        .query_filtered::<Entity, With<lumen_core::components::Transform>>()
        .iter(&app.world)
        .count();
    println!(
        "{name}, {buffers} buffers of {side}x{side}, {elements} laid-out elements \
         (filled in {:.0} ms, first frames {:.0} ms)",
        ms(filled),
        ms(warm)
    );
    report("first frame", &[first]);

    // A readback of an unchanged frame: what `gpu` subtracts.
    app.world.resource_mut::<Art>().action = Action::Idle;
    for _ in 0..3 {
        tick(&mut app);
    }
    let mut rb = Vec::new();
    for _ in 0..10 {
        let t = Instant::now();
        read_back(&app);
        rb.push(ms(t.elapsed()));
    }
    rb.sort_by(f64::total_cmp);
    let readback = rb[rb.len() / 2];

    let phase = |app: &mut App, name: &str, action: Action, force: bool| {
        app.world.resource_mut::<Art>().action = action;
        let mut samples = Vec::with_capacity(frames);
        for _ in 0..frames {
            if force {
                app.world.resource_mut::<FrameDirty>().dirty = true;
            }
            samples.push(frame(app, readback));
        }
        report(name, &samples);
    };
    phase(&mut app, "idle", Action::Idle, false);
    phase(&mut app, "repaint, same", Action::Idle, true);
    phase(&mut app, "pan", Action::Pan, false);
    phase(&mut app, "stroke", Action::Stroke, false);
    phase(&mut app, "rewrite one", Action::RewriteOne, false);

    // Edits through the DOM, one a tick, with nothing else moving.
    let dom_phase =
        |app: &mut App, name: &str, target: u64, edit: &dyn Fn(u64) -> ScriptCommand| {
            app.world.resource_mut::<Art>().action = Action::Idle;
            let mut samples = Vec::with_capacity(frames);
            for _ in 0..frames {
                node_query::push_external_dom_command(edit(target));
                samples.push(frame(app, readback));
            }
            report(name, &samples);
        };
    let root = node_query::run_document().expect("the document root");
    let status = node_query::run_get_by_id("status").expect("#status");
    dom_phase(&mut app, "class, no-op", root, &|node| {
        ScriptCommand::ClassRemove {
            node,
            class: "absent".into(),
        }
    });
    let flip = std::cell::Cell::new(false);
    dom_phase(&mut app, "tree edit", status, &|node| {
        flip.set(!flip.get());
        ScriptCommand::SetInnerMarkup {
            node,
            markup: if flip.get() {
                "<label text=\"saved\"/>".into()
            } else {
                "<label text=\"saved\"/><label text=\"1 layer\"/>".into()
            },
        }
    });
    app.world.resource_mut::<Art>().action = Action::Rewrite;
    let rewrite = frame(&mut app, readback);
    app.world.resource_mut::<Art>().action = Action::Pan;
    let after_rewrite = frame(&mut app, readback);
    report("rewrite all", &[rewrite]);
    report("frame after", &[after_rewrite]);

    // Hide a middle buffer: the frame after it stops being drawn.
    {
        let mut art = app.world.resource_mut::<Art>();
        let middle = art.visible.len() / 2;
        art.visible[middle] = false;
        art.action = Action::Pan;
    }
    let hide = frame(&mut app, readback);
    let after = frame(&mut app, readback);
    report("hide a buffer", &[hide]);
    report("frame after", &[after]);
    // Drop it altogether.
    {
        let mut art = app.world.resource_mut::<Art>();
        let middle = art.visible.len() / 2;
        art.buffers.remove(middle);
        art.visible.remove(middle);
    }
    let drop = frame(&mut app, readback);
    report("free a buffer", &[drop]);
    println!("  (readback of an unchanged frame: {readback:.2} ms)");
    drop_app(app);
    let _ = std::fs::remove_dir_all(&dir);
}

fn drop_app(app: App) {
    let t = Instant::now();
    drop(app);
    println!("  (teardown {:.0} ms)", ms(t.elapsed()));
}

fn main() {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let arg = |name: &str| {
        args.iter()
            .find_map(|a| a.strip_prefix(name)?.strip_prefix('='))
            .map(str::to_string)
    };
    let mut backends = Vec::new();
    if args.iter().any(|a| a == "gpu") {
        backends.push(Backend::Gpu);
    }
    if args.iter().any(|a| a == "cpu") {
        backends.push(Backend::Cpu);
    }
    if backends.is_empty() {
        backends = vec![Backend::Gpu, Backend::Cpu];
    }
    if let Some(why) = gpu_unavailable_reason() {
        eprintln!("GPU skipped: {why}");
        backends.retain(|b| *b != Backend::Gpu);
    }
    let counts: Vec<usize> = arg("buffers")
        .map(|s| s.split(',').filter_map(|n| n.parse().ok()).collect())
        .unwrap_or_else(|| vec![2, 8, 32]);
    let side = arg("side").and_then(|s| s.parse().ok()).unwrap_or(4096);
    let rows = arg("rows").and_then(|s| s.parse().ok()).unwrap_or(300);
    let frames = arg("frames").and_then(|s| s.parse().ok()).unwrap_or(30);
    // `LUMEN_BENCH_TRACE=1` on a build with the `profiling` feature writes
    // every system's spans to `lumen-trace.json`.
    let _trace = std::env::var_os("LUMEN_BENCH_TRACE").and_then(|_| {
        lumenc::profile::install(lumenc::profile::ProfileMode::Chrome)
            .map_err(|e| eprintln!("no trace: {e}"))
            .ok()
    });
    println!("columns: median/worst ms per frame");
    for backend in backends {
        for &n in &counts {
            run(backend, n, side, rows, frames);
        }
    }
}
