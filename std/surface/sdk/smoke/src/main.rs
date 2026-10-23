//! Exercises what a Rust app needs from `lumenui-surface` alone: a surface
//! element, its endpoint string where the platform has a transport (Linux
//! and macOS), and the state it reports. The app renders on the CPU, which
//! cannot show shared GPU frames, so the surface must settle as unsupported
//! and say so through its class. Exits non-zero when any of it does not
//! hold.
//!
//! `tools/release/lumenui-engine.py wrappers` gives it a manifest inside the
//! generated `lumenui-surface` crate, and CI builds and runs it there against
//! the engine set.

use std::sync::Mutex;

use lumenui::prelude::*;
use lumenui_surface::{SurfacePlugin, SurfaceStateKind, Surfaces};

/// What the app saw, read after it ran.
static SEEN: Mutex<Option<(Option<String>, SurfaceStateKind, bool)>> = Mutex::new(None);

fn observe(surfaces: Res<Surfaces>, classes: Query<(&LumenId, &LumenClasses)>) {
    let Some(state) = surfaces.state("scene") else {
        return;
    };
    let endpoint = surfaces.endpoint("scene");
    let class = classes
        .iter()
        .any(|(id, classes)| id.0 == "scene" && classes.has("surface-unsupported"));
    *SEEN.lock().unwrap_or_else(|e| e.into_inner()) = Some((endpoint, state, class));
}

fn main() {
    let dir = std::env::current_dir().expect("a working directory");
    let run = App::from_markup(
        r#"<root><surface id="scene" style="width: 64px; height: 64px;"/></root>"#,
    )
    .app_dir(dir)
    .window_size(96, 96)
    .add_plugin(SurfacePlugin::default())
    .add_systems(TickStage::Systems, observe)
    .screenshot(HeadlessOptions {
        dpr: 1.0,
        ticks: Some(6),
        ..HeadlessOptions::default()
    });
    if let Err(e) = run {
        eprintln!("lumenui-surface-smoke: the app did not run: {e}");
        std::process::exit(1);
    }
    let seen = SEEN.lock().unwrap_or_else(|e| e.into_inner()).take();
    let Some((endpoint, state, class)) = seen else {
        eprintln!("lumenui-surface-smoke: the surface never registered");
        std::process::exit(1);
    };
    println!("lumenui-surface-smoke: {state:?}, endpoint {endpoint:?}");
    let well_formed = endpoint
        .as_deref()
        .is_some_and(|e| e.starts_with("v1;") && e.contains(";channel="));
    if cfg!(unix) != well_formed {
        eprintln!(
            "lumenui-surface-smoke: the endpoint is malformed, or present without a transport"
        );
        std::process::exit(1);
    }
    if state != SurfaceStateKind::Unsupported || !class {
        eprintln!("lumenui-surface-smoke: expected an unsupported surface with its class");
        std::process::exit(1);
    }
    println!("lumenui-surface-smoke: ok");
}
