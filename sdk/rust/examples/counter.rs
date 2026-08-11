//! The counter app rebuilt in the ECS-first SDK.
//!
//! No builder, no string-keyed callbacks: two ordinary `bevy_ecs` systems are
//! scheduled into [`TickStage::Systems`]. `bump_counter` folds this tick's
//! clicks into the typed `count` signal; `update_label` reads it back and
//! writes the label's [`TextContent`] directly through a query - a real ECS
//! mutation, no binding indirection.
//!
//! Run with:
//!
//! ```bash
//! cargo run -p lumenui --example counter          # hot-reloads examples/main.lmn live
//! cargo run -p lumenui --example counter --release # markup/CSS embedded, no watcher
//! ```
//!
//! The UI comes from [`lumen_source!`]: in a debug `cargo run` it is read from
//! `examples/main.lmn` / `examples/main.css` on disk and hot-reloaded by the
//! runtime watcher (edit the markup and the window updates live); a
//! `--release` build `include_str!`-embeds the same files.

use lumenui::prelude::*;

fn main() -> lumenui::Result<()> {
    lumenui::App::new()
        .add_plugins(
            LumenDefaultPlugins
                .with_source(lumen_source!("examples/main.lmn", "examples/main.css"))
                .with_title("Lumen counter (ECS SDK)")
                .with_size(960, 720),
        )
        .insert_signal("count", 0i64)
        .add_systems(TickStage::Systems, (bump_counter, update_label).chain())
        .run()
}

/// Add this tick's click count to the `count` signal.
fn bump_counter(mut clicks: MessageReader<ClickEvent>, mut signals: Signals) {
    let hits = clicks.read().count();
    if hits > 0 {
        let total = signals.get_or::<i64>("count", 0) + hits as i64;
        signals.set("count", total);
    }
}

/// Reflect the `count` signal into the counter label's text.
fn update_label(signals: Signals, mut labels: Query<(&LumenId, &mut TextContent)>) {
    let count = signals.get_or::<i64>("count", 0);
    for (id, mut text) in &mut labels {
        if id.0 == "counter-label" {
            let next = format!("Lumen - clicks: {count}");
            if text.0 != next {
                text.0 = next;
            }
        }
    }
}
