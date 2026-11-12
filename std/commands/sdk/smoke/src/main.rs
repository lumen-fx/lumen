//! Exercises what a Rust app needs from `lumenui-commands` alone: a command
//! read from markup, one registered through the `Commands` system parameter,
//! both run, and the `command` event heard by a handler and as `CommandRun`
//! messages. Exits non-zero when any of it does not hold.
//!
//! `tools/release/lumenui-engine.py wrappers` gives it a manifest inside the
//! generated `lumenui-commands` crate, and CI builds and runs it there against
//! the engine set.

use std::sync::Mutex;

use lumenui::prelude::*;
use lumenui_commands::{CommandRun, CommandsPlugin, Persist};

/// What the app saw: the ids the `command` handler heard, and the ids the
/// `CommandRun` messages carried.
static SEEN: Mutex<(Vec<String>, Vec<String>)> = Mutex::new((Vec::new(), Vec::new()));

#[derive(Resource, Default)]
struct Ticks(u32);

fn drive(
    commands: lumenui_commands::Commands,
    mut runs: MessageReader<CommandRun>,
    mut ticks: ResMut<Ticks>,
) {
    ticks.0 += 1;
    match ticks.0 {
        2 => {
            commands.run("hello");
        }
        3 => {
            let _ = commands.register("late", "Late", "F8");
            commands.run("late");
        }
        _ => {}
    }
    let mut seen = SEEN.lock().unwrap_or_else(|e| e.into_inner());
    seen.1.extend(runs.read().map(|r| r.id.clone()));
}

fn main() {
    let dir = std::env::current_dir().expect("a working directory");
    let run = App::from_markup(
        r#"<root>
  <commands>
    <command id="hello" label="Hello" keys="F4"/>
  </commands>
  <label text="commands"/>
</root>"#,
    )
    .app_dir(dir)
    .window_size(320, 200)
    .add_plugin(CommandsPlugin {
        persist: Persist::None,
    })
    .insert_resource(Ticks::default())
    .add_systems(TickStage::Systems, drive)
    .on_any(EventKind::Custom("command"), |_, ev| {
        let id = ev
            .detail()
            .and_then(|d| d.get("id"))
            .map(|v| v.stringify())
            .unwrap_or_default();
        SEEN.lock().unwrap_or_else(|e| e.into_inner()).0.push(id);
    })
    .screenshot(HeadlessOptions {
        dpr: 1.0,
        ticks: Some(8),
        ..HeadlessOptions::default()
    });
    if let Err(e) = run {
        eprintln!("lumenui-commands-smoke: the app did not run: {e}");
        std::process::exit(1);
    }
    let (heard, ran) = SEEN.lock().unwrap_or_else(|e| e.into_inner()).clone();
    let want = vec!["hello".to_string(), "late".to_string()];
    if heard != want {
        eprintln!("lumenui-commands-smoke: the handler heard {heard:?}, wanted {want:?}");
        std::process::exit(1);
    }
    if ran != want {
        eprintln!("lumenui-commands-smoke: CommandRun carried {ran:?}, wanted {want:?}");
        std::process::exit(1);
    }
    println!("lumenui-commands-smoke: ok");
}
