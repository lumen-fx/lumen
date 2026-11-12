// Drives the real pipeline (parse -> cascade -> spawn -> layout), which needs
// `RunOptions` / `build_headless_app`; lumenc only exposes those under
// `dev-run`.
#![cfg(feature = "dev-run")]

//! Where a virtualized `<for>` row lays out, on the tick it mounts and after.
//!
//! A row sits at `position: absolute` in the slot its index gives it, so it
//! never counts toward the content size of anything above it, and it follows
//! its list wherever a relayout moves the list.
//!
//! Headless: no window, no GPU.

use std::sync::{Arc, Mutex};

use bevy_ecs::entity::Entity;
use glam::Vec2;
use lumen_core::app::App;
use lumen_core::components::{LumenClasses, LumenId, TextContent, Transform};
use lumen_core::input::ScrollOffset;
use lumen_core::node::NodeHandle;
use lumen_core::property_store::PropertyStore;
use lumen_core::signals::{ArrayItem, ArraySignals};
use lumen_core::window::primary_viewport_mut;
use lumen_script::ScriptValue;

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

    /// An app read from `files` under `src/` (its `main.lmn`, stylesheet and
    /// scripts), the way `lumenc run` reads one.
    fn from_files(files: &[(&str, &str)]) -> Self {
        let guard = SERIAL.lock().unwrap_or_else(|e| e.into_inner());
        let dir =
            std::env::temp_dir().join(format!("lumenc_virt_files_{}_{}", std::process::id(), {
                static SEQ: std::sync::atomic::AtomicUsize = std::sync::atomic::AtomicUsize::new(0);
                SEQ.fetch_add(1, std::sync::atomic::Ordering::Relaxed)
            }));
        let src = dir.join("src");
        std::fs::create_dir_all(&src).unwrap();
        for (name, text) in files {
            std::fs::write(src.join(name), text).unwrap();
        }
        std::fs::write(
            dir.join("lumen.toml"),
            "[window]\nsize = [800, 600]\n\n[mcp]\nport = 0\n",
        )
        .unwrap();
        let mut opts = lumenc::RunOptions::new(&dir).with_parser(lumenc::default_parser());
        opts.hot_reload = false;
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

    fn set_assets(&mut self, n: usize) {
        let items: Vec<ArrayItem> = (0..n)
            .map(|i| {
                let mut item = ArrayItem::new();
                item.insert("id".to_string(), i.to_string());
                item.insert("name".to_string(), format!("asset {i}"));
                item
            })
            .collect();
        self.app
            .world
            .resource_mut::<ArraySignals>()
            .set("assets", items);
    }

    /// The cell showing asset `index`.
    fn cell(&mut self, index: usize) -> Entity {
        let name = format!("asset {index}");
        let mut q = self.app.world.query::<(Entity, &TextContent)>();
        let label = q
            .iter(&self.app.world)
            .find(|(_, t)| t.0 == name)
            .map(|(e, _)| e)
            .unwrap_or_else(|| panic!("asset {index} is not mounted"));
        self.app
            .world
            .get::<bevy_ecs::hierarchy::ChildOf>(label)
            .expect("a label inside its cell")
            .parent()
    }

    fn signal(&self, name: &str) -> Option<String> {
        self.app
            .world
            .resource::<PropertyStore>()
            .get_global_str(name)
            .map(|v| v.to_string())
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

/// #578: after a resize moves the list, its rows follow it on the same
/// frame, even when a row mounts in that frame too.
#[test]
fn rows_follow_their_list_through_a_resize() {
    let markup = r##"<root>
  <column width="100%" height="100%">
    <column id="top" class="grow" />
    <scroll id="console" height="200">
      <for each="lines" virtualized="true" row-height="18">
        <row class="line"><label text="{row.text}" /></row>
      </for>
    </scroll>
  </column>
</root>"##;
    let mut h = Harness::new(markup, ".grow { flex-grow: 1; }");
    let mut lines: Vec<String> = (0..5).map(|i| format!("line {i}")).collect();
    h.set_lines(&lines);
    h.ticks(4);
    let list = h.by_id("console");
    assert_eq!(
        h.transform(list).absolute.y,
        400.0,
        "the list sits at the bottom"
    );

    for (size, scale) in [
        (Vec2::new(800.0, 900.0), 1.0),
        (Vec2::new(1200.0, 1400.0), 1.5),
    ] {
        {
            let mut vp = primary_viewport_mut(&mut h.app.world);
            vp.size = size;
            vp.scale_factor = scale;
        }
        // A line arrives in the frame the window resizes.
        lines.push(format!("line {}", lines.len()));
        h.set_lines(&lines);
        h.ticks(1);
        let at = h.transform(list).absolute;
        assert_eq!(
            at.y,
            size.y - 200.0,
            "the list follows the window to {size:?}"
        );
        let rows = h.by_class("line");
        assert_eq!(rows.len(), lines.len(), "every line mounts");
        for row in rows {
            let t = h.transform(row);
            assert!(
                t.absolute.y >= at.y && t.absolute.y < at.y + 200.0,
                "row at {:?} sits inside the list at {at:?} after resizing to {size:?}",
                t.absolute
            );
        }
    }
}

const GRID: &str = r##"<root>
  <scroll id="browser" height="600">
    <for id="assets" each="assets" key="id" virtualized="true" row-height="92" cell-width="84">
      <column class="asset"><label text="{row.name}" /></column>
    </for>
  </scroll>
</root>"##;

const GRID_CSS: &str = "#browser { width: 100%; } #assets { gap: 8px; }";

/// Cells per grid row of a block `width` wide: 84 px cells, 8 px gaps.
fn columns(width: f32) -> usize {
    (((width + 8.0) / 92.0).floor() as usize).max(1)
}

/// The detail of every `rangechange` a native handler on `node` heard.
fn listen(node: Entity) -> Arc<Mutex<Vec<(i64, i64)>>> {
    let heard = Arc::new(Mutex::new(Vec::new()));
    let sink = heard.clone();
    lumen_script::event::register_native_binding(
        NodeHandle::new(node).pack(),
        "rangechange".to_string(),
        false,
        Arc::new(move || {
            let detail = lumen_script::event::event_detail();
            let index = |key: &str| match detail.get(key) {
                Some(ScriptValue::I64(i)) => *i,
                other => panic!("`{key}` is an integer, not {other:?}"),
            };
            sink.lock().unwrap().push((index("first"), index("last")));
        }),
    );
    heard
}

/// #581: a 5000-item grid mounts the cells its window shows, holds the
/// scroll extent of every grid row, and tells a native handler (an SDK app
/// with no script) which items it mounted.
#[test]
fn a_grid_mounts_only_the_cells_its_window_shows() {
    let mut h = Harness::new(GRID, GRID_CSS);
    let block = h.by_id("assets");
    let heard = listen(block);
    h.set_assets(5000);
    h.ticks(4);

    let laid_out = h.transform(block);
    let cols = columns(laid_out.size.x);
    assert!(
        cols > 1,
        "an {} px block holds several cells",
        laid_out.size.x
    );
    // 600 px of 100 px grid rows, plus max(2, ceil(8 / cols)) below.
    let buffer = 8usize.div_ceil(cols).max(2);
    let mounted = (6 + buffer) * cols;
    assert_eq!(h.by_class("asset").len(), mounted, "only the window mounts");

    let rows = 5000usize.div_ceil(cols) as f32;
    assert_eq!(
        laid_out.size.y,
        rows * 92.0 + (rows - 1.0) * 8.0,
        "the block is as tall as every grid row and the gaps between them"
    );
    // Item `cols + 1` opens the second grid row, one cell in.
    let cell = h.cell(cols + 1);
    let at = h.transform(cell);
    assert_eq!(at.absolute - laid_out.absolute, Vec2::new(92.0, 100.0));
    assert_eq!(at.size, Vec2::new(84.0, 92.0));
    assert_eq!(
        heard.lock().unwrap().last().copied(),
        Some((0, mounted as i64)),
        "the handler hears what mounted"
    );

    // Scroll to grid row 10: rows 10..16 show, and the buffer goes both ways.
    let browser = h.by_id("browser");
    h.app.world.get_mut::<ScrollOffset>(browser).unwrap().0.y = 1000.0;
    h.ticks(2);
    let first = (10 - buffer) * cols;
    let last = (16 + buffer) * cols;
    assert_eq!(h.by_class("asset").len(), last - first);
    assert_eq!(
        heard.lock().unwrap().last().copied(),
        Some((first as i64, last as i64))
    );
}

/// #581: a window resize changes how many cells a grid row holds, and the
/// cells the block keeps move to their new places.
#[test]
fn a_resize_moves_a_grid_to_its_new_column_count() {
    let mut h = Harness::new(GRID, GRID_CSS);
    let block = h.by_id("assets");
    h.set_assets(5000);
    h.ticks(4);
    let wide = columns(h.transform(block).size.x);
    let kept = h.cell(5);

    {
        let mut vp = primary_viewport_mut(&mut h.app.world);
        vp.size = Vec2::new(400.0, 600.0);
    }
    h.ticks(3);
    let laid_out = h.transform(block);
    let cols = columns(laid_out.size.x);
    assert!(cols < wide, "{cols} columns at 400 px, {wide} at 800");
    assert_eq!(h.cell(5), kept, "item 5 keeps its cell");
    let at = h.transform(kept).absolute - laid_out.absolute;
    assert_eq!(
        at,
        Vec2::new((5 % cols) as f32 * 92.0, (5 / cols) as f32 * 100.0),
        "item 5 sits at column {} of grid row {}",
        5 % cols,
        5 / cols
    );
    let buffer = 8usize.div_ceil(cols).max(2);
    assert_eq!(h.by_class("asset").len(), (6 + buffer) * cols);
}

const RANGE_CANDELA: &str = r#"import "lumen.cdl";
fn on_range(ev: int) {
    let detail = lumen::event_detail(ev) as {string: any};
    let first = detail["first"] as int;
    let last = detail["last"] as int;
    lumen::signal_set("candela_range", string(first) + ":" + string(last));
}
fn on_ready() {
    lumen::event_on(lumen::node_get_by_id("assets"), "rangechange", "on_range");
}
fn main() {}
"#;

const RANGE_RHAI: &str = r#"fn on_ready() {
    get_by_id("assets").on("rangechange", |e| {
        let d = e.detail();
        signal("rhai_range", "").set(`${d.first}:${d.last}`);
    });
}
"#;

const RANGE_LUA: &str = r#"function on_ready()
    get_by_id("assets"):on("rangechange", function(e)
        local d = e:detail()
        signal("lua_range", ""):set(d.first .. ":" .. d.last)
    end)
end
"#;

/// #581: `rangechange` reaches a handler in every script host, with the
/// mounted range in its detail.
#[test]
fn rangechange_reaches_every_script_host() {
    let markup = GRID.replace(
        "</root>",
        "  <script src=\"main.cdl\"/>\n  <script src=\"side.rhai\"/>\n  <script src=\"side.lua\"/>\n</root>",
    );
    let mut h = Harness::from_files(&[
        ("main.lmn", &markup),
        ("main.css", GRID_CSS),
        ("main.cdl", RANGE_CANDELA),
        ("side.rhai", RANGE_RHAI),
        ("side.lua", RANGE_LUA),
    ]);
    let block = h.by_id("assets");
    h.set_assets(5000);
    h.ticks(4);
    let cols = columns(h.transform(block).size.x);
    let mounted = (6 + 8usize.div_ceil(cols).max(2)) * cols;
    let want = format!("0:{mounted}");
    for host in ["candela_range", "rhai_range", "lua_range"] {
        assert_eq!(h.signal(host).as_deref(), Some(want.as_str()), "{host}");
    }
}
