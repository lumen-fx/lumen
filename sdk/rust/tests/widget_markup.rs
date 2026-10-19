//! A derived widget written as a tag in markup becomes that widget: the
//! element the tag spawns carries the widget's component, with its props read
//! off the attributes the element was written with.
//!
//! The tag registry is process-wide, so this test binary holds one test and
//! one tag.

// In an app this is the published `lumenui` crate; inside the workspace the
// engine crate it re-exports stands in for it under the same name.
extern crate lumenui_engine as lumenui;

use std::sync::Mutex;

use lumenui::prelude::*;

#[derive(Component, Clone, Debug, Default, Widget)]
#[widget(tag = "markup-gauge")]
struct Gauge {
    #[widget(prop)]
    label: String,
    #[widget(prop)]
    level: u32,
    #[widget(state)]
    lit: bool,
}

/// `(id, label, level, lit)` of every element carrying a `Gauge`, written by a
/// system in the running app.
static SEEN: Mutex<Vec<(String, String, u32, bool)>> = Mutex::new(Vec::new());

fn record(gauges: Query<(&Gauge, &LumenId, &LumenTag)>) {
    let mut seen = SEEN.lock().unwrap();
    seen.clear();
    seen.extend(gauges.iter().map(|(gauge, id, tag)| {
        assert_eq!(&*tag.0, "markup-gauge");
        (
            id.0.to_string(),
            gauge.label.clone(),
            gauge.level,
            gauge.lit,
        )
    }));
    seen.sort();
}

#[test]
fn a_derived_widget_tag_in_markup_carries_the_component_and_its_props() {
    let dir = std::env::temp_dir().join(format!("lumenui-widget-markup-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(dir.join("src")).unwrap();
    std::fs::write(
        dir.join("src").join("main.lmn"),
        "<root>\
           <markup-gauge id=\"fuel\" label=\"fuel\" level=\"7\" lit=\"true\"/>\
           <column><markup-gauge id=\"bare\"/></column>\
         </root>",
    )
    .unwrap();
    std::fs::write(dir.join("lumen.toml"), "[mcp]\nport = 0\n").unwrap();

    let markup = std::fs::read_to_string(dir.join("src").join("main.lmn")).unwrap();
    lumenui::App::from_markup(markup)
        .app_dir(&dir)
        .add_plugin(GaugePlugin)
        .add_systems(TickStage::Systems, record)
        .run_headless(2)
        .expect("the app runs");
    let _ = std::fs::remove_dir_all(&dir);

    // `lit` is state: the attribute of that name does not reach it. The
    // element written without attributes still becomes a gauge, at defaults.
    assert_eq!(
        *SEEN.lock().unwrap(),
        [
            ("bare".to_string(), String::new(), 0, false),
            ("fuel".to_string(), "fuel".to_string(), 7, false),
        ]
    );
}
