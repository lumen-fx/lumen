// Drives the real pipeline (parse -> cascade -> spawn), which needs
// `RunOptions` / `build_headless_app`; lumenc only exposes those under
// `dev-run`.
#![cfg(feature = "dev-run")]

//! A widget's props inside a `<for>` row take the row's values.
//!
//! A `<tooltip>` body and the attributes of a custom widget are written the
//! way a label's text is, so `{row.field}` in them, or several fields in one
//! value, shows the row's record, in a plain list and in a virtualized one,
//! and follows the record when it changes under a kept row.
//!
//! Headless: no window, no GPU.

use bevy_ecs::prelude::*;
use lumen_core::app::App;
use lumen_core::components::TextContent;
use lumen_core::signals::{ArrayItem, ArraySignals};
use lumen_primitives::TooltipSource;

/// One app at a time: the headless apps share process-global buses.
static SERIAL: std::sync::Mutex<()> = std::sync::Mutex::new(());

/// A custom widget with one prop, built from the attributes it is written
/// with.
#[derive(Component, Clone, Debug, Default)]
struct Badge {
    caption: String,
}

impl lumen_widget::Widget for Badge {
    fn name() -> &'static str {
        "Badge"
    }

    fn parser_tag() -> &'static str {
        "row-badge"
    }

    fn spawn(entity: Entity, attrs: &lumen_widget::Attributes, world: &mut World) {
        let caption = attrs.get("caption").unwrap_or_default().to_string();
        world.entity_mut(entity).insert(Badge { caption });
    }
}

struct Harness {
    app: App,
    _guard: std::sync::MutexGuard<'static, ()>,
}

impl Harness {
    fn new(markup: &str) -> Self {
        let guard = SERIAL.lock().unwrap_or_else(|e| e.into_inner());
        lumen_widget::register_widget::<Badge>();
        let dir =
            std::env::temp_dir().join(format!("lumenc_row_props_{}_{}", std::process::id(), {
                static SEQ: std::sync::atomic::AtomicUsize = std::sync::atomic::AtomicUsize::new(0);
                SEQ.fetch_add(1, std::sync::atomic::Ordering::Relaxed)
            }));
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(
            dir.join("lumen.toml"),
            "[window]\nsize = [800, 600]\n\n[mcp]\nport = 0\n",
        )
        .unwrap();
        let mut opts = lumenc::RunOptions::new(&dir)
            .with_parser(lumenc::default_parser())
            .with_markup(markup.to_string());
        opts.hot_reload = false;
        let (app, _window) = lumenc::run::build_headless_app(opts).expect("build_headless_app");
        let _ = std::fs::remove_dir_all(&dir);
        Self { app, _guard: guard }
    }

    fn set_lines(&mut self, lines: &[(&str, &str, &str)]) {
        let items = lines
            .iter()
            .map(|(key, source, text)| {
                let mut item = ArrayItem::new();
                item.insert("key".to_string(), (*key).to_string());
                item.insert("source".to_string(), (*source).to_string());
                item.insert("text".to_string(), (*text).to_string());
                item
            })
            .collect();
        self.app
            .world
            .resource_mut::<ArraySignals>()
            .set("lines", items);
        for _ in 0..6 {
            self.app.tick();
        }
    }

    /// The tooltip body on the element showing `label`.
    fn tooltip_of(&mut self, label: &str) -> String {
        let mut q = self.app.world.query::<(&TextContent, &TooltipSource)>();
        q.iter(&self.app.world)
            .find(|(t, _)| t.0 == label)
            .map(|(_, tip)| tip.text.clone())
            .unwrap_or_else(|| panic!("no tooltip trigger shows `{label}`"))
    }

    fn badges(&mut self) -> Vec<String> {
        let mut q = self.app.world.query::<&Badge>();
        let mut out: Vec<String> = q.iter(&self.app.world).map(|b| b.caption.clone()).collect();
        out.sort();
        out
    }
}

const PLAIN: &str = r#"<root>
  <column>
    <for each="lines" key="key">
      <tooltip text="{row.source}: {row.text}">
        <label text="{row.text}" />
      </tooltip>
    </for>
  </column>
</root>"#;

const VIRTUALIZED: &str = r#"<root>
  <column height="200">
    <for each="lines" key="key" virtualized="true" row-height="18">
      <tooltip text="{row.source}: {row.text}">
        <label text="{row.text}" />
      </tooltip>
    </for>
  </column>
</root>"#;

#[test]
fn a_tooltip_in_a_row_shows_the_row_fields() {
    let mut h = Harness::new(PLAIN);
    h.set_lines(&[("1", "net", "timed out"), ("2", "disk", "full")]);
    assert_eq!(h.tooltip_of("timed out"), "net: timed out");
    assert_eq!(h.tooltip_of("full"), "disk: full");
}

#[test]
fn a_tooltip_in_a_kept_row_follows_its_record() {
    let mut h = Harness::new(PLAIN);
    h.set_lines(&[("1", "net", "timed out")]);
    h.set_lines(&[("1", "dns", "retrying")]);
    assert_eq!(h.tooltip_of("retrying"), "dns: retrying");
}

#[test]
fn a_tooltip_in_a_virtualized_row_shows_the_row_fields() {
    let mut h = Harness::new(VIRTUALIZED);
    h.set_lines(&[("1", "net", "timed out"), ("2", "disk", "full")]);
    assert_eq!(h.tooltip_of("timed out"), "net: timed out");
    h.set_lines(&[("1", "dns", "retrying"), ("2", "disk", "full")]);
    assert_eq!(h.tooltip_of("retrying"), "dns: retrying");
}

#[test]
fn a_custom_widget_prop_in_a_row_takes_the_row_value() {
    let mut h = Harness::new(
        r#"<root>
  <column>
    <for each="lines" key="key">
      <row-badge caption="{row.source}: {row.text}" />
    </for>
  </column>
</root>"#,
    );
    h.set_lines(&[("1", "net", "timed out"), ("2", "disk", "full")]);
    assert_eq!(h.badges(), ["disk: full", "net: timed out"]);
    h.set_lines(&[("1", "dns", "retrying"), ("2", "disk", "full")]);
    assert_eq!(h.badges(), ["disk: full", "dns: retrying"]);
}
