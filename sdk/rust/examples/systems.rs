//! The Bevy user's aha moment: it's a real ECS - you can touch *everything*.
//!
//! This app carries no signals and no bindings. A single system reads the
//! [`ClickEvent`] stream and mutates the clicked entity's [`Visuals`]
//! component directly, cycling its fill through a palette. The renderer picks
//! the new fill up on the same frame because `Visuals` is the very component it
//! extracts from.
//!
//! Run with:
//!
//! ```bash
//! cargo run -p lumen --example systems
//! ```

use lumen::prelude::*;

/// A little palette the clicked tile cycles through.
const PALETTE: [Color; 5] = [
    Color::rgb(0.86, 0.27, 0.28),
    Color::rgb(0.20, 0.55, 0.92),
    Color::rgb(0.28, 0.79, 0.42),
    Color::rgb(0.93, 0.69, 0.20),
    Color::rgb(0.71, 0.34, 0.85),
];

fn main() -> lumen::Result<()> {
    lumen::App::new()
        .add_plugins(
            LumenDefaultPlugins
                .with_source(lumen_source!("examples/main.lmn", "examples/main.css"))
                .with_title("Lumen systems demo - direct Visuals mutation"),
        )
        .add_systems(TickStage::Systems, recolor_clicked)
        .run()
}

/// On each click, advance the target tile's fill to the next palette entry.
///
/// `next` is a per-system [`Local`], so the palette index survives across ticks
/// without any resource or signal - pure ECS state.
fn recolor_clicked(
    mut clicks: MessageReader<ClickEvent>,
    mut next: Local<usize>,
    mut visuals: Query<&mut Visuals>,
) {
    for click in clicks.read() {
        if let Ok(mut v) = visuals.get_mut(click.entity) {
            v.fill = Some(Fill::Solid(PALETTE[*next % PALETTE.len()]));
            *next += 1;
        }
    }
}
