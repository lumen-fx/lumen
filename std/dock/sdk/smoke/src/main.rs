//! Exercises what a Rust app needs from `lumenui-dock` alone: a dock read
//! from markup, a panel popped out into a window of its own through the
//! `Dock` system parameter, the layout saved as a value, and the panel docked
//! back. Exits non-zero when any of it does not hold.
//!
//! `tools/release/lumenui-engine.py wrappers` gives it a manifest inside the
//! generated `lumenui-dock` crate, and CI builds and runs it there against
//! the engine set.

use std::sync::Mutex;

use lumenui::prelude::*;
use lumenui_dock::{Dock, DockPlugin, DockWindow};

/// What the app saw: whether the panel opened a window, the layout saved
/// while it was out, and whether it docked back.
static SEEN: Mutex<(bool, Option<String>, bool)> = Mutex::new((false, None, false));

#[derive(Resource, Default)]
struct Ticks(u32);

fn drive(dock: Dock, windows: Query<&DockWindow>, mut ticks: ResMut<Ticks>) {
    ticks.0 += 1;
    let mut seen = SEEN.lock().unwrap_or_else(|e| e.into_inner());
    match ticks.0 {
        2 => {
            dock.detach("log").expect("the log panel pops out");
        }
        4 => {
            seen.0 = !windows.is_empty() && dock.window_of("log") > 0;
            seen.1 = Some(dock.save().to_json());
            dock.attach("log").expect("the log panel docks back");
        }
        6 => {
            seen.2 = windows.is_empty() && dock.window_of("log") == 0;
        }
        _ => {}
    }
}

fn main() {
    let dir = std::env::current_dir().expect("a working directory");
    let run = App::from_markup(
        r#"<root>
  <dock id="main">
    <dock-split axis="row">
      <dock-stack center="true"><dock-panel id="code" title="Code"/></dock-stack>
      <dock-stack basis="120"><dock-panel id="log" title="Log"/></dock-stack>
    </dock-split>
  </dock>
</root>"#,
    )
    .app_dir(dir)
    .window_size(320, 200)
    .add_plugin(DockPlugin::default())
    .insert_resource(Ticks::default())
    .add_systems(TickStage::Systems, drive)
    .screenshot(HeadlessOptions {
        dpr: 1.0,
        ticks: Some(8),
        ..HeadlessOptions::default()
    });
    if let Err(e) = run {
        eprintln!("lumenui-dock-smoke: the app did not run: {e}");
        std::process::exit(1);
    }
    let (opened, saved, back) = SEEN.lock().unwrap_or_else(|e| e.into_inner()).clone();
    if !opened {
        eprintln!("lumenui-dock-smoke: the panel did not open a window");
        std::process::exit(1);
    }
    if !saved.is_some_and(|s| s.contains("\"windows\"")) {
        eprintln!("lumenui-dock-smoke: the saved layout has no window");
        std::process::exit(1);
    }
    if !back {
        eprintln!("lumenui-dock-smoke: the panel did not dock back");
        std::process::exit(1);
    }
    println!("lumenui-dock-smoke: ok");
}
