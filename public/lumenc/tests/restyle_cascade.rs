// Drives the real pipeline (parse -> cascade -> spawn -> layout), which needs
// `RunOptions` / `build_headless_app`; lumenc only exposes those under
// `dev-run`.
#![cfg(feature = "dev-run")]

//! A restyle resolves an element from scratch.
//!
//! A class change, an inline style or a theme flip re-runs the cascade for
//! an element and rebuilds its style components from the result, so a
//! property nothing sets any more goes back to where it started. What a tag
//! is by default comes from the user-agent sheet and what the parser builds
//! a widget out of is recorded as the element's own declarations, so both
//! come back; what a runtime system holds while it runs (a hidden `<if>`, an
//! open popup, a virtualized row, a hover tint, a tween) stays.
//!
//! Headless: no window, no GPU.

use std::time::{Duration, Instant};

use bevy_ecs::entity::Entity;
use glam::Vec2;
use lumen_core::app::App;
use lumen_core::components::{
    Color, Display, Fill, FlexDirection, FlexJustify, Length, LumenClasses, LumenId, Position,
    Style, StyleVersion, Transform, Visuals,
};
use lumen_core::input::{PointerMoved, PointerState};
use lumen_core::property_store::PropertyStore;
use lumen_script::ScriptCommand;
use lumen_script::node_query::{drain_external_dom_commands, push_external_dom_command};
use lumenc::RunOptions;
use lumenc::run::build_headless_app;

/// The external DOM bus is process-global, so the apps that write to it run
/// one at a time.
static SERIAL: std::sync::Mutex<()> = std::sync::Mutex::new(());

struct Harness {
    app: App,
    _guard: std::sync::MutexGuard<'static, ()>,
}

impl Harness {
    fn new(markup: &str, css: &str) -> Self {
        let guard = SERIAL.lock().unwrap_or_else(|e| e.into_inner());
        let _ = drain_external_dom_commands();
        let dir = std::env::temp_dir().join(format!("lumenc_restyle_{}_{}", std::process::id(), {
            static SEQ: std::sync::atomic::AtomicUsize = std::sync::atomic::AtomicUsize::new(0);
            SEQ.fetch_add(1, std::sync::atomic::Ordering::Relaxed)
        }));
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(
            dir.join("lumen.toml"),
            "[window]\nsize = [800, 600]\n\n[mcp]\nport = 0\n",
        )
        .unwrap();
        let opts = RunOptions::new(&dir)
            .with_parser(lumenc::default_parser())
            .with_markup(markup.to_string())
            .with_css(css.to_string());
        let (app, _window) = build_headless_app(opts).expect("build_headless_app");
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

    /// Tick across a wall-clock window, so time-based state settles.
    fn settle(&mut self, ms: u64) {
        let deadline = Instant::now() + Duration::from_millis(ms);
        while Instant::now() < deadline {
            self.app.tick();
            std::thread::sleep(Duration::from_millis(8));
        }
        self.ticks(2);
    }

    /// Restyle the whole document, as a theme flip does.
    fn restyle_all(&mut self) {
        StyleVersion::bump(&mut self.app.world);
        self.ticks(3);
    }

    fn set_classes(&mut self, id: &str, classes: &str) {
        push_external_dom_command(ScriptCommand::SetClasses {
            target_id: id.to_string(),
            classes: classes.to_string(),
        });
        self.ticks(3);
    }

    fn set_signal(&mut self, name: &str, value: &str) {
        self.app
            .world
            .resource_mut::<PropertyStore>()
            .set_global_str(name, value);
        self.ticks(4);
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

    fn style(&self, e: Entity) -> Style {
        self.app.world.get::<Style>(e).expect("a style").clone()
    }

    fn fill(&self, e: Entity) -> Option<Color> {
        self.app
            .world
            .get::<Visuals>(e)
            .and_then(|v| v.fill.as_ref())
            .and_then(Fill::as_solid)
    }

    fn hover(&mut self, e: Entity) {
        let t = *self.app.world.get::<Transform>(e).expect("laid out");
        let pos = t.absolute + t.size * 0.5;
        self.pointer(pos);
    }

    fn pointer(&mut self, pos: Vec2) {
        self.app.world.resource_mut::<PointerState>().position = Some(pos);
        self.app.world.write_message(PointerMoved {
            position: pos,
            local: None,
            pointer: Default::default(),
        });
        self.app.tick();
    }
}

fn near(c: Option<Color>, r: f32, g: f32, b: f32) -> bool {
    c.is_some_and(|c| (c.r - r).abs() < 0.01 && (c.g - g).abs() < 0.01 && (c.b - b).abs() < 0.01)
}

/// #488: removing a class takes with it every property only that class set.
/// The restyle used to copy over only what the new resolution set, so the
/// row kept the removed class's `justify` and background.
#[test]
fn removing_a_class_drops_what_only_it_set() {
    let mut h = Harness::new(
        r##"<root><row id="r" class="row j"><label text="hi"/></row></root>"##,
        ".row { height: 40; } .j { justify: end; bg: #ff0000; }",
    );
    let row = h.by_id("r");
    assert_eq!(h.style(row).justify, FlexJustify::End);
    assert!(near(h.fill(row), 1.0, 0.0, 0.0));

    h.set_classes("r", "row");
    let style = h.style(row);
    assert_eq!(
        style.justify,
        Style::default().justify,
        "the removed class's justify is gone"
    );
    assert_eq!(h.fill(row), None, "and so is its background");
    assert_eq!(
        style.height,
        Length::Px(40.0),
        "the class left keeps its height"
    );
}

/// A tag's direction is a user-agent rule, so a restyle rebuilds it, and a
/// rule can change it.
#[test]
fn a_column_keeps_its_direction_through_a_restyle() {
    let mut h = Harness::new(
        r##"<root><column id="c" class="a"><label text="x"/></column><column id="s" class="side"/></root>"##,
        ".side { flex-direction: row; }",
    );
    let c = h.by_id("c");
    let s = h.by_id("s");
    assert_eq!(h.style(c).flex_direction, FlexDirection::Column);
    h.set_classes("c", "b");
    h.restyle_all();
    assert_eq!(h.style(c).flex_direction, FlexDirection::Column);
    assert_eq!(
        h.style(s).flex_direction,
        FlexDirection::Row,
        "a rule outranks the tag's direction"
    );
}

/// The panel a `<dropdown>` expands into is placed by declarations the
/// parser records as the panel's own, so a restyle puts it back.
#[test]
fn a_dropdown_panel_keeps_its_geometry_through_a_restyle() {
    let mut h = Harness::new(
        r##"<root><row><dropdown bind-value="fruit" width="200">
             <option value="a" label="Apple"/><option value="b" label="Banana"/>
           </dropdown></row></root>"##,
        "",
    );
    // The panel's body mounts the first time it opens.
    h.set_signal("__dropdown_open:fruit", "true");
    h.set_signal("__dropdown_open:fruit", "false");
    let panel = h.by_class("dropdown-panel")[0];
    h.restyle_all();
    let after = h.style(panel);
    assert_eq!(after.position, Position::Absolute);
    assert_eq!(
        (after.inset.top, after.inset.left, after.inset.bottom),
        (40.0, 0.0, -1.0)
    );
    assert_eq!(after.flex_direction, FlexDirection::Column);
}

/// A progress fill's width is the bar's to drive, held over the fill's own
/// styles, so a restyle leaves it where the value put it.
#[test]
fn a_progress_fill_keeps_its_width_through_a_restyle() {
    let mut h = Harness::new(
        r##"<root><progress id="p" value="0.25" width="200"/></root>"##,
        "",
    );
    let fill = h.by_class("progress-fill")[0];
    assert_eq!(h.style(fill).width, Length::Percent(25.0));
    h.restyle_all();
    let style = h.style(fill);
    assert_eq!(style.width, Length::Percent(25.0), "the bar's width stands");
    assert_eq!(
        style.position,
        Position::Absolute,
        "the fill's own placement"
    );
}

/// A hidden `<if mode="hide">` holds `display: none`; a restyle keeps it
/// hidden, and showing it lands on the display its styles give it then.
#[test]
fn a_hidden_if_stays_hidden_through_a_restyle() {
    let mut h = Harness::new(
        r##"<root><if id="g" signal="open" mode="hide" class="gate"><label text="body"/></if></root>"##,
        ".gate.grid { display: grid; }",
    );
    let gate = h.by_id("g");
    h.set_signal("open", "1");
    h.set_signal("open", "");
    assert_eq!(h.style(gate).display, Display::None);
    h.set_classes("g", "gate grid");
    h.restyle_all();
    assert_eq!(h.style(gate).display, Display::None, "still hidden");
    h.set_signal("open", "1");
    assert_eq!(
        h.style(gate).display,
        Display::Grid,
        "shown with the display its styles give it now"
    );
}

/// A virtualized row sits in the slot its index gives it, held over the
/// row's own styles, so a restyle leaves it there.
#[test]
fn virtual_rows_keep_their_slots_through_a_restyle() {
    let mut h = Harness::new(
        r##"<root><scroll height="200">
             <for each="items" virtualized="true" row-height="20">
               <row class="item"><label text="{row.name}"/></row>
             </for>
           </scroll></root>"##,
        ".item { position: relative; }",
    );
    let items: Vec<lumen_core::signals::ArrayItem> = (0..40)
        .map(|i| {
            let mut item = lumen_core::signals::ArrayItem::new();
            item.insert("name".to_string(), format!("n{i}"));
            item
        })
        .collect();
    h.app
        .world
        .resource_mut::<lumen_core::signals::ArraySignals>()
        .set("items", items);
    h.ticks(6);
    let tops = |h: &mut Harness| -> Vec<f32> {
        let rows = h.by_class("item");
        let mut tops: Vec<f32> = rows.into_iter().map(|e| h.style(e).inset.top).collect();
        tops.sort_by(f32::total_cmp);
        tops
    };
    let before = tops(&mut h);
    assert!(!before.is_empty(), "rows mounted");
    assert_eq!(before[1], 20.0, "the second row sits at its slot");
    h.restyle_all();
    assert_eq!(tops(&mut h), before, "every row keeps its slot");
    let row = h.by_class("item")[0];
    assert_eq!(h.style(row).position, Position::Absolute);
}

/// An open popup holds the placement the edge-flip gave it; a restyle keeps
/// it, and closing lets it fall back to where its styles place it.
#[test]
fn an_open_popup_keeps_its_position_through_a_restyle() {
    let mut h = Harness::new(
        r##"<root><row><dropdown bind-value="fruit" width="200">
             <option value="a" label="Apple"/><option value="b" label="Banana"/>
           </dropdown></row></root>"##,
        "",
    );
    h.set_signal("__dropdown_open:fruit", "true");
    let panel = h.by_class("dropdown-panel")[0];
    let open = h.style(panel);
    assert!(
        open.inset.bottom.is_nan(),
        "the open panel sizes to its rows"
    );
    h.restyle_all();
    let after = h.style(panel);
    assert_eq!(after.inset.top, open.inset.top);
    assert!(after.inset.bottom.is_nan(), "the placement stands");
    h.set_signal("__dropdown_open:fruit", "false");
    let closed = h.style(panel);
    assert_eq!(closed.inset.top, 40.0);
    assert_eq!(closed.inset.bottom, -1.0);
}

/// A hover tint is held over the element's fill, so a class change while
/// the pointer is over the element keeps the tint, and leaving fades to the
/// class's fill rather than the one the element had when the pointer came.
#[test]
fn a_hover_tint_survives_a_class_change() {
    let mut h = Harness::new(
        r##"<root><button id="b" text="Go" width="120" height="40"/></root>"##,
        "#b { bg: #000000; hover-bg: #ffffff; } #b.on { bg: #0000ff; }",
    );
    let b = h.by_id("b");
    h.hover(b);
    h.settle(300);
    assert!(near(h.fill(b), 1.0, 1.0, 1.0), "hovered: the tint");
    h.set_classes("b", "on");
    assert!(near(h.fill(b), 1.0, 1.0, 1.0), "the tint survives");
    h.pointer(Vec2::new(700.0, 500.0));
    h.settle(300);
    assert!(
        near(h.fill(b), 0.0, 0.0, 1.0),
        "leaving fades to the new resting fill"
    );
}

/// A transition in flight heads for whatever the cascade resolves now: a
/// class change mid-tween retargets it from where it is.
#[test]
fn a_transition_in_flight_retargets_to_the_new_value() {
    let mut h = Harness::new(
        r##"<root><tile id="t" class="a" width="40" height="40"/></root>"##,
        ".a { bg: #ff0000; transition: bg 5000ms linear; } .b { bg: #0000ff; }",
    );
    let t = h.by_id("t");
    h.set_classes("t", "a b");
    let tween = h
        .app
        .world
        .get::<lumen_primitives::BackgroundTransition>(t)
        .expect("the change tweens")
        .0;
    assert!(near(Some(tween.to), 0.0, 0.0, 1.0));
    // Part of the way there.
    h.settle(300);
    assert!(!near(h.fill(t), 1.0, 0.0, 0.0), "the fill has moved");
    h.set_classes("t", "a");
    let tween = h
        .app
        .world
        .get::<lumen_primitives::BackgroundTransition>(t)
        .expect("still tweening")
        .0;
    assert!(
        near(Some(tween.to), 1.0, 0.0, 0.0),
        "the tween now heads back to the class's colour"
    );
}
