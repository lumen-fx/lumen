//! A small editor workspace: a file list, two documents in a tabbed center,
//! and a log. F6 moves between the stacks, Ctrl+Shift+D pops the focused
//! panel out into a window of its own and docks it back, and the layout is
//! kept between runs.
//!
//! It is an example of the generated `lumenui-dock` crate. From a Lumen
//! checkout, write that crate outside the checkout and run it against an
//! engine set (see `tools/release/lumenui-engine.py`):
//!
//! ```bash
//! tools/release/lumenui-engine.py wrappers --out /tmp/lumenui-sdk
//! LUMEN_ENGINE_ARTIFACTS=<set> cargo run --manifest-path /tmp/lumenui-sdk/lumenui-dock/Cargo.toml --example workspace
//! ```

use lumenui::prelude::*;
use lumenui_dock::{Dock, DockPlugin, Persist};

const MARKUP: &str = r#"<root>
  <dock id="workspace" label="Workspace">
    <dock-split axis="row">
      <dock-stack basis="200">
        <dock-panel id="files" title="Files"><label text="main.rs"/></dock-panel>
      </dock-stack>
      <dock-split axis="column">
        <dock-stack center="true">
          <dock-panel id="main-rs" title="main.rs"><textarea/></dock-panel>
          <dock-panel id="lib-rs" title="lib.rs"><textarea/></dock-panel>
        </dock-stack>
        <dock-stack basis="140">
          <dock-panel id="log" title="Log"><label text="ready"/></dock-panel>
        </dock-stack>
      </dock-split>
    </dock-split>
  </dock>
</root>"#;

/// Ctrl+Shift+D: pop the focused panel out, or dock it back.
fn pop_out_on_key(
    dock: Dock,
    mut keys: MessageReader<KeyPressed>,
    focus: Res<lumenui::lumen_core::input::FocusTracker>,
    panels: Query<&lumenui_dock::DockPanel>,
    parents: Query<&ChildOf>,
) {
    for key in keys.read() {
        let m = key.modifiers;
        if !(m.ctrl && m.shift && key.key == Key::from("D")) {
            continue;
        }
        let mut cur = focus.0;
        while let Some(e) = cur {
            if let Ok(panel) = panels.get(e) {
                if dock.window_of(&panel.id) > 0 {
                    let _ = dock.attach(&panel.id);
                } else {
                    let _ = dock.detach(&panel.id);
                }
                break;
            }
            cur = parents.get(e).ok().map(ChildOf::parent);
        }
    }
}

fn main() -> lumenui::Result<()> {
    App::from_markup(MARKUP)
        .window_title("Workspace")
        .window_size(900, 600)
        .add_plugin(DockPlugin {
            persist: Persist::Auto,
            ..DockPlugin::default()
        })
        .add_systems(TickStage::Systems, pop_out_on_key)
        .run()
}
