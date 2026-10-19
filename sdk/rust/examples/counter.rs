//! A counter, written the way a script would be: one click handler and a
//! DOM write.
//!
//! Every tile click lands in `on_any_click` (the tiles have no handler of
//! their own), which counts it in the `count` signal and writes the label's
//! text through its DOM handle.
//!
//! Run with:
//!
//! ```bash
//! cargo run -p lumenui-engine --example counter
//! ```
//!
//! An app compiles its UI in its build script and starts from
//! `lumenui::app!()`. An example has no build script of its own, so this one
//! compiles `examples/` (its `lumen.toml` and `src/`) when it starts.

// In an app this is the published `lumenui` crate; inside the workspace the
// engine crate it re-exports stands in for it under the same name.
extern crate lumenui_engine as lumenui;

use lumenui::prelude::*;

fn main() -> lumenui::Result<()> {
    let ui = CompiledUi::compile(concat!(env!("CARGO_MANIFEST_DIR"), "/examples"))?;
    App::new(ui)
        .signal("count", 0i64)
        .on_any_click(|ui, _| {
            ui.update("count", |n: i64| n + 1);
            let count: i64 = ui.get("count");
            if let Some(label) = ui.get_by_id("counter-label") {
                label.set_text(&format!("Lumen - clicks: {count}"));
            }
        })
        .run()
}
