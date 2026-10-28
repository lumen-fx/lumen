// Drives the real pipeline (parse -> cascade -> spawn -> layout), which needs
// `RunOptions` / `build_headless_app`; lumenc only exposes those under
// `dev-run`.
#![cfg(feature = "dev-run")]

//! Where a virtualized `<for>` row lays out, on the tick it mounts and after.
//!
//! A row sits at `position: absolute` in the slot its index gives it, so it
//! never counts toward the content size of anything above it.
//!
//! Headless: no window, no GPU.

use bevy_ecs::entity::Entity;
use lumen_core::app::App;
use lumen_core::components::{LumenClasses, LumenId, TextContent, Transform};
use lumen_core::signals::{ArrayItem, ArraySignals};

/// One app at a time: the headless apps share process-global buses.
static SERIAL: std::sync::Mutex<()> = std::sync::Mutex::new(());

struct Harness {
    app: App,
    _guard: std::sync::MutexGuard<'static, ()>,
}

impl Harness {
    fn new(markup: &str, css: &str) -> Self {
        let guard = SERIAL.lock().unwrap_or_else(|e| e.into_inner());
        let dir =
            std::env::temp_dir().join(format!("lumenc_virt_rows_{}_{}", std::process::id(), {
                static SEQ: std::sync::atomic::AtomicUsize = std::sync::atomic::AtomicUsize::new(0);
                SEQ.fetch_add(1, std::sync::atomic::Ordering::Relaxed)
            }));
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(
            dir.join("lumen.toml"),
            "[window]\nsize = [800, 600]\n\n[mcp]\nport = 0\n",
        )
        .unwrap();
        let opts = lumenc::RunOptions::new(&dir)
            .with_parser(lumenc::default_parser())
            .with_markup(markup.to_string())
            .with_css(css.to_string());
        let (app, _window) = lumenc::run::build_headless_app(opts).expect("build_headless_app");
        let _ = std::fs::remove_dir_all(&dir);
        let mut h = Self { app, _guard: guard };
        h.ticks(6);
        h
    }

    fn ticks(&mut self, n: usize) {
        for _ in 0..n {
            self.app.tick();
        }
    }

    fn by_id(&mut self, id: &str) -> Entity {
        let mut q = self.app.world.query::<(Entity, &LumenId)>();
        q.iter(&self.app.world)
            .find(|(_, l)| l.0 == id)
            .map(|(e, _)| e)
            .unwrap_or_else(|| panic!("no element with id `{id}`"))
    }

    fn by_class(&mut self, class: &str) -> Vec<Entity> {
        let mut q = self.app.world.query::<(Entity, &LumenClasses)>();
        q.iter(&self.app.world)
            .filter(|(_, c)| c.0.iter().any(|x| x.as_ref() == class))
            .map(|(e, _)| e)
            .collect()
    }

    fn transform(&self, e: Entity) -> Transform {
        *self.app.world.get::<Transform>(e).expect("laid out")
    }

    fn set_text(&mut self, id: &str, text: &str) {
        let e = self.by_id(id);
        self.app.world.get_mut::<TextContent>(e).expect("a text").0 = text.to_string();
    }

    fn set_lines(&mut self, lines: &[String]) {
        let items: Vec<ArrayItem> = lines
            .iter()
            .map(|text| {
                let mut item = ArrayItem::new();
                item.insert("text".to_string(), text.clone());
                item
            })
            .collect();
        self.app
            .world
            .resource_mut::<ArraySignals>()
            .set("lines", items);
    }
}

const CONSOLE: &str = r##"<root>
  <split id="split" width="800" height="400">
    <column id="left" class="side"><label id="status" text="0 lines" /></column>
    <column id="pane" class="center">
      <scroll id="console" height="300">
        <for each="lines" virtualized="true" row-height="18">
          <row class="line"><label text="{row.text}" /></row>
        </for>
      </scroll>
    </column>
    <column id="right" class="side"><label text="inspector" /></column>
  </split>
</root>"##;

// Every pane sizes from its content and shares what is left, so the center
// pane's width follows its content's.
const CONSOLE_CSS: &str = ".side, .center { flex-basis: auto; flex-grow: 1; }";

/// #585: the tick a row mounts lays the elements above the list out exactly
/// as the tick after it does, and as a tick that mounts nothing.
#[test]
fn a_mounting_row_does_not_widen_its_ancestors() {
    let mut h = Harness::new(CONSOLE, CONSOLE_CSS);
    let pane = h.by_id("pane");
    let mut lines: Vec<String> = (0..3).map(|i| format!("line {i}")).collect();
    h.set_lines(&lines);
    h.ticks(4);
    let settled = h.transform(pane).size;
    h.ticks(1);
    assert_eq!(
        h.transform(pane).size,
        settled,
        "a quiet tick moves nothing"
    );

    for n in 3..8 {
        // A line far wider than the pane, so a row laid out in flow would
        // push the pane wider.
        lines.push(format!("{n} {}", "wide console output ".repeat(20)));
        h.set_lines(&lines);
        // Something outside the pane changes in the same tick, as a line
        // counter does, so the tick lays out from the document root too.
        h.set_text("status", &format!("{} lines", lines.len()));
        h.ticks(1);
        let mount = h.transform(pane).size;
        h.ticks(1);
        let next = h.transform(pane).size;
        assert_eq!(mount, next, "line {n}: the mount tick matches the next");
        assert_eq!(mount, settled, "line {n}: the pane keeps its width");
    }
}
