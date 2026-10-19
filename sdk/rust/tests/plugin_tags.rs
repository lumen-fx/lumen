//! A widget plugin added with `add_plugin` registers its tag in time for the
//! markup that uses it.
//!
//! The tag registry is process-wide, so this test binary holds one test and
//! one tag: nothing else here may register it first and hide the order.

// In an app this is the published `lumenui` crate; inside the workspace the
// engine crate it re-exports stands in for it under the same name.
extern crate lumenui_engine as lumenui;

use std::sync::Mutex;

use lumenui::prelude::*;

const TAG: &str = "plugin-order-gauge";

#[derive(Component, Clone, Debug, Default, Widget)]
#[widget(tag = "plugin-order-gauge")]
struct Gauge;

/// The ids of the elements with the plugin's tag in the running app's tree,
/// written by a system.
static SEEN: Mutex<Vec<String>> = Mutex::new(Vec::new());

fn record(elements: Query<(&LumenTag, &LumenId)>) {
    let mut seen = SEEN.lock().unwrap();
    seen.clear();
    seen.extend(
        elements
            .iter()
            .filter(|(tag, _)| &*tag.0 == TAG)
            .map(|(_, id)| id.0.to_string()),
    );
}

#[test]
fn a_plugins_tag_is_registered_before_the_markup_is_parsed() {
    let dir = std::env::temp_dir().join(format!("lumenui-plugin-tags-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(dir.join("src")).unwrap();
    std::fs::write(
        dir.join("src").join("main.lmn"),
        "<root><plugin-order-gauge id=\"fuel\"/></root>",
    )
    .unwrap();
    std::fs::write(dir.join("lumen.toml"), "[mcp]\nport = 0\n").unwrap();

    // No `Gauge::register()` call: adding the plugin is the whole setup. The
    // markup is parsed when the app starts, after its plugins built.
    lumenui::App::from_markup("<root><plugin-order-gauge id=\"fuel\"/></root>")
        .app_dir(&dir)
        .add_plugin(GaugePlugin)
        .add_systems(TickStage::Systems, record)
        .run_headless(2)
        .expect("the markup parses with the plugin's tag");

    assert_eq!(*SEEN.lock().unwrap(), ["fuel"]);
}
