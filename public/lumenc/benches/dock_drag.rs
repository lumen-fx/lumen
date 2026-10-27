//! What a 30-panel docked workspace costs at rest and while the user drags,
//! with no window: the dock module loaded from `lumen.toml`, six stacks of
//! five panels each holding a few controls, laid out and painted offscreen
//! on the app's real pipeline.
//!
//! ```text
//! cargo bench -p lumenc --features dev-run --bench dock_drag
//! cargo bench -p lumenc --features dev-run --bench dock_drag -- gpu frames=120
//! ```
//!
//! Arguments: `cpu` or `gpu` (default `cpu`), `frames=N` (frames per phase,
//! default 60).
//!
//! The phases: idle ticks; a forced repaint of an unchanged frame; the
//! pointer moved across the dock with no button held; a tab
//! dragged across every stack, one pointer move a frame, the ghost and the
//! region preview following it; the drop that moves the panel; a float
//! dragged by its title bar; and a crowded strip scrolled by the wheel. Each
//! reports the median and the worst frame in milliseconds, split into the
//! update (input, systems, style, layout) and the rest (extract and paint).
//! A frame at 60 Hz has 16.7 ms.

use std::path::PathBuf;
use std::time::{Duration, Instant};

use bevy_ecs::prelude::*;
use glam::Vec2;
use lumen_core::app::App;
use lumen_core::input::{
    MouseWheel, PointerButton, PointerData, PointerMoved, PointerPressed, PointerReleased,
    PointerState,
};
use lumen_core::prelude::{FrameDirty, FrameTiming};
use lumen_core::render_backend::install_offscreen;
use lumen_render_cpu::CpuRenderer;
use lumen_render_wgpu::WgpuRenderer;
use lumen_text::ShaperService;
use lumenc::RunOptions;
use lumenc::run::build_headless_app;

const WIDTH: u32 = 1400;
const HEIGHT: u32 = 900;

/// Six stacks of five panels: two columns of two on each side of a center
/// stack, the sixth crowded with long titles so its strip overflows.
fn markup() -> String {
    let panel = |i: usize, title: &str| {
        format!(
            r#"<dock-panel id="p{i}" title="{title}"><column padding="8" gap="4"><label text="Panel {i}"/><input text="field {i}"/><button text="Action {i}"/></column></dock-panel>"#
        )
    };
    let stack = |from: usize, attrs: &str| {
        let mut s = format!("<dock-stack {attrs}>");
        for i in from..from + 5 {
            s += &panel(i, &format!("Panel {i}"));
        }
        s + "</dock-stack>"
    };
    let mut crowded = String::from(r#"<dock-stack basis="220">"#);
    for i in 26..=30 {
        crowded += &panel(i, &format!("A longer title {i}"));
    }
    crowded += "</dock-stack>";
    format!(
        r#"<root><button id="bar" text="toolbar"/><dock id="bench" label="Bench"><dock-split axis="row">
<dock-split axis="column" basis="300">{}{}</dock-split>
{}
<dock-split axis="column" basis="300">{}{}</dock-split>
{crowded}
</dock-split></dock></root>"#,
        stack(1, ""),
        stack(6, ""),
        stack(11, r#"center="true""#),
        stack(16, ""),
        stack(21, ""),
    )
}

fn app_dir() -> PathBuf {
    let dir = std::env::temp_dir().join(format!("lumen-dock-drag-bench-{}", std::process::id()));
    std::fs::create_dir_all(&dir).expect("scratch dir");
    std::fs::write(
        dir.join("lumen.toml"),
        format!(
            "[window]\nsize = [{WIDTH}, {HEIGHT}]\n\n[mcp]\nport = 0\n\n\
             [dependencies]\nlumen-dock = {{ bundled = true }}\n"
        ),
    )
    .expect("lumen.toml");
    dir
}

/// One frame, in milliseconds: the whole tick and its update part.
#[derive(Clone, Copy, Default)]
struct Sample {
    total: f64,
    update: f64,
}

fn ms(d: Duration) -> f64 {
    d.as_secs_f64() * 1000.0
}

fn frame(app: &mut App) -> Sample {
    let t0 = Instant::now();
    app.tick();
    let total = ms(t0.elapsed());
    app.world.resource_mut::<FrameDirty>().dirty = false;
    let t = *app.world.resource::<FrameTiming>();
    Sample {
        total,
        update: ms(t.update_end.saturating_duration_since(t.start)),
    }
}

fn report(name: &str, samples: &[Sample]) {
    let pick = |f: fn(&Sample) -> f64| {
        let mut v: Vec<f64> = samples.iter().map(f).collect();
        v.sort_by(f64::total_cmp);
        (v[v.len() / 2], *v.last().unwrap_or(&0.0))
    };
    let (tm, tw) = pick(|s| s.total);
    let (um, uw) = pick(|s| s.update);
    println!(
        "  {name:<20} total {tm:>6.2}/{tw:<7.2} update {um:>6.2}/{uw:<7.2} ({} frames)",
        samples.len()
    );
}

/// Write the pointer the way a window backend reports it.
fn pointer(app: &mut App, at: Vec2, press: Option<bool>) {
    {
        let mut state = app.world.resource_mut::<PointerState>();
        state.position = Some(at);
        if let Some(down) = press {
            state.primary_down = down;
        }
    }
    let held = u32::from(app.world.resource::<PointerState>().primary_down);
    app.world.write_message(PointerMoved {
        position: at,
        local: None,
        pointer: PointerData::mouse(held).stamped(),
    });
    match press {
        Some(true) => {
            app.world.write_message(PointerPressed {
                position: at,
                button: PointerButton::Primary,
                local: None,
                pointer: PointerData::mouse(1).stamped(),
            });
        }
        Some(false) => {
            app.world.write_message(PointerReleased {
                position: at,
                button: PointerButton::Primary,
                local: None,
                pointer: PointerData::mouse(0).stamped(),
            });
        }
        None => {}
    }
}

/// The middle of the strip button showing `panel`.
fn tab(app: &mut App, panel: &str) -> Vec2 {
    let mut q = app.world.query::<(
        &lumen_primitives::TabStripButton,
        &lumen_core::components::Transform,
    )>();
    let found: Vec<(Entity, Vec2)> = q
        .iter(&app.world)
        .map(|(b, t)| (b.tab, t.absolute + t.size / 2.0))
        .collect();
    found
        .into_iter()
        .find(|(t, _)| {
            app.world
                .get::<lumen_dock::DockTab>(*t)
                .is_some_and(|t| t.panel == panel)
        })
        .map(|(_, at)| at)
        .unwrap_or_else(|| panic!("no tab for {panel}"))
}

fn main() {
    let mut backend = "cpu";
    let mut frames = 60usize;
    for arg in std::env::args().skip(1) {
        match arg.as_str() {
            "cpu" | "gpu" => backend = if arg == "gpu" { "gpu" } else { "cpu" },
            a if a.starts_with("frames=") => {
                frames = a["frames=".len()..].parse().expect("frames=N");
            }
            "--bench" => {}
            other => eprintln!("dock_drag: ignoring {other}"),
        }
    }
    let dir = app_dir();
    let opts = RunOptions::new(&dir)
        .with_parser(lumenc::default_parser())
        .with_markup(markup());
    let (mut app, mut window) = build_headless_app(opts).expect("the app builds");
    let primary = lumen_core::window::primary_entity(&app.world).expect("a primary window");
    if backend == "gpu" {
        install_offscreen(
            &mut app,
            WgpuRenderer::new_offscreen(primary, WIDTH, HEIGHT).expect("a GPU"),
        );
    } else {
        install_offscreen(&mut app, CpuRenderer::new_offscreen(primary, WIDTH, HEIGHT));
    }
    if let Some(shaper) = window.text_shaper.take() {
        app.render_world
            .insert_non_send(ShaperService::from(shaper));
    }
    {
        let mut vp = lumen_core::window::primary_viewport_mut(&mut app.world);
        vp.size = Vec2::new(WIDTH as f32, HEIGHT as f32);
        vp.scale_factor = 1.0;
    }
    for _ in 0..6 {
        frame(&mut app);
    }
    let elements = app
        .world
        .query_filtered::<Entity, With<lumen_core::components::Transform>>()
        .iter(&app.world)
        .count();
    println!("{backend}, 30 panels in 6 stacks, {elements} laid-out elements, {WIDTH}x{HEIGHT}");

    let run = |app: &mut App, force: bool| -> Vec<Sample> {
        (0..frames)
            .map(|_| {
                if force {
                    app.world.resource_mut::<FrameDirty>().dirty = true;
                }
                frame(app)
            })
            .collect()
    };
    let idle = run(&mut app, false);
    report("idle", &idle);
    let repaint = run(&mut app, true);
    report("repaint, same", &repaint);

    // The pointer moved across the dock with no button held, for what a
    // move costs without a drag.
    let mut moves = Vec::with_capacity(frames);
    for i in 0..frames {
        let t = i as f32 / frames.max(2) as f32;
        let x = 20.0 + t * (WIDTH as f32 - 40.0);
        let y = 120.0 + (t * 9.0).sin().abs() * (HEIGHT as f32 - 240.0);
        pointer(&mut app, Vec2::new(x, y), None);
        moves.push(frame(&mut app));
    }
    report("pointer move", &moves);

    // A tab dragged across the whole dock and back, one move a frame.
    let from = tab(&mut app, "p3");
    pointer(&mut app, from, Some(true));
    frame(&mut app);
    pointer(&mut app, from + Vec2::new(8.0, 0.0), None);
    frame(&mut app);
    let mut hover = Vec::with_capacity(frames);
    for i in 0..frames {
        let t = i as f32 / frames.max(2) as f32;
        let x = 20.0 + t * (WIDTH as f32 - 40.0);
        let y = 120.0 + (t * 9.0).sin().abs() * (HEIGHT as f32 - 240.0);
        pointer(&mut app, Vec2::new(x, y), None);
        hover.push(frame(&mut app));
    }
    report("drag hover", &hover);
    // The drop: onto the middle of the center stack.
    let center = tab(&mut app, "p11") + Vec2::new(0.0, 200.0);
    pointer(&mut app, center, None);
    frame(&mut app);
    pointer(&mut app, center, Some(false));
    let drop = vec![frame(&mut app), frame(&mut app)];
    report("drop (2 frames)", &drop);

    // A float dragged by its title bar.
    {
        use bevy_ecs::system::RunSystemOnce;
        app.world
            .run_system_once(|d: lumen_dock::Dock| {
                d.float(
                    "p7",
                    lumen_dock::model::Rect {
                        x: 400.0,
                        y: 200.0,
                        w: 360.0,
                        h: 260.0,
                    },
                )
                .expect("p7 floats");
            })
            .expect("the dock is loaded");
    }
    for _ in 0..4 {
        frame(&mut app);
    }
    let grip = tab(&mut app, "p7") + Vec2::new(200.0, 0.0);
    pointer(&mut app, grip, Some(true));
    frame(&mut app);
    pointer(&mut app, grip + Vec2::new(8.0, 0.0), None);
    frame(&mut app);
    let mut moving = Vec::with_capacity(frames);
    for i in 0..frames {
        let d = Vec2::new(
            (i as f32 * 0.3).cos() * 200.0,
            (i as f32 * 0.2).sin() * 120.0,
        );
        pointer(&mut app, grip + d, None);
        moving.push(frame(&mut app));
    }
    report("float move", &moving);
    pointer(&mut app, grip, Some(false));
    frame(&mut app);

    // The crowded strip scrolled by the wheel.
    let strip = tab(&mut app, "p27");
    let mut wheel = Vec::with_capacity(frames);
    for i in 0..frames {
        let delta = if (i / 10) % 2 == 0 { 40.0 } else { -40.0 };
        pointer(&mut app, strip, None);
        app.world.write_message(MouseWheel {
            delta: Vec2::new(0.0, delta),
            position: strip,
            local: None,
            time_stamp: Instant::now(),
        });
        wheel.push(frame(&mut app));
    }
    report("strip wheel", &wheel);
    let _ = std::fs::remove_dir_all(&dir);
}
