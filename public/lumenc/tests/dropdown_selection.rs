// Drives the real pipeline (parse -> spawn -> signal bindings), which needs
// `RunOptions` / `build_headless_app`; lumenc only exposes those under
// `dev-run`.
#![cfg(feature = "dev-run")]

//! What a closed `<dropdown>` says it has selected.
//!
//! The bound signal holds an `<option>`'s `value` - that is what a script
//! reads and what a click writes - while the header reads its `label`.
//! Nothing in the parser can settle that: the value only exists at
//! runtime, so these tests boot the app and read the header's live text.
//! Headless: no window, no GPU.

use lumen_core::app::App;
use lumen_core::components::{LumenClasses, TextContent};
use lumen_core::input::Focused;
use lumen_core::prelude::Entity;
use lumen_core::property_store::PropertyStore;
use lumenc::RunOptions;
use lumenc::run::build_headless_app;

fn build(markup: &str) -> App {
    let dir =
        std::env::temp_dir().join(format!("lumenc_dropdown_sel_{}_{}", std::process::id(), {
            static SEQ: std::sync::atomic::AtomicUsize = std::sync::atomic::AtomicUsize::new(0);
            SEQ.fetch_add(1, std::sync::atomic::Ordering::Relaxed)
        }));
    std::fs::create_dir_all(&dir).unwrap();
    std::fs::write(dir.join("lumen.toml"), "[mcp]\nport = 0\n").unwrap();
    let opts = RunOptions::new(&dir)
        .with_parser(lumenc::default_parser())
        .with_markup(markup.to_string());
    let (mut app, _window) = build_headless_app(opts).expect("build_headless_app");
    for _ in 0..4 {
        app.tick();
    }
    let _ = std::fs::remove_dir_all(&dir);
    app
}

/// The text on the synthesized header button, which carries the
/// `dropdown-button` class.
fn header_text(app: &mut App) -> String {
    let mut q = app.world.query::<(&LumenClasses, &TextContent)>();
    q.iter(&app.world)
        .find(|(classes, _)| classes.0.iter().any(|c| c.as_ref() == "dropdown-button"))
        .map(|(_, text)| text.0.clone())
        .expect("a dropdown header")
}

/// Puts keyboard focus on the header, which is what closing the popup
/// does once a selection commits.
fn focus_header(app: &mut App) {
    let mut q = app.world.query::<(Entity, &LumenClasses)>();
    let header = q
        .iter(&app.world)
        .find(|(_, classes)| classes.0.iter().any(|c| c.as_ref() == "dropdown-button"))
        .map(|(entity, _)| entity)
        .expect("a dropdown header");
    app.world.entity_mut(header).insert(Focused);
}

fn select(app: &mut App, signal: &str, value: &str) {
    app.world
        .resource_mut::<PropertyStore>()
        .set_global_str(signal, value);
    for _ in 0..2 {
        app.tick();
    }
}

const FRUIT: &str = r##"<root>
  <dropdown bind-value="fruit">
    <option value="a" label="Apple"/>
    <option value="b" label="Banana"/>
  </dropdown>
</root>"##;

/// The first option seeds the signal, so a dropdown nobody has touched
/// already has a selection - and it reads as the label, not as the value
/// the signal holds.
#[test]
fn the_closed_header_reads_the_selected_option_label() {
    let mut app = build(FRUIT);
    assert_eq!(header_text(&mut app), "Apple");
}

/// A script writing the value signal moves the header onto that option's
/// label.
#[test]
fn writing_the_value_signal_moves_the_header_to_that_label() {
    let mut app = build(FRUIT);
    select(&mut app, "fruit", "b");
    assert_eq!(header_text(&mut app), "Banana");
}

/// The signal keeps holding the value, which is what a script reads and
/// what the rest of the app matches on.
#[test]
fn the_signal_still_holds_the_value() {
    let mut app = build(FRUIT);
    select(&mut app, "fruit", "b");
    assert_eq!(
        app.world
            .resource::<PropertyStore>()
            .get_global_str("fruit")
            .as_deref(),
        Some("b")
    );
}

/// An `<option>` with no `label` shows its value, which is the documented
/// fallback.
#[test]
fn an_option_with_no_label_reads_as_its_value() {
    let mut app = build(
        r##"<root>
  <dropdown bind-value="size">
    <option value="small"/>
    <option value="large"/>
  </dropdown>
</root>"##,
    );
    assert_eq!(header_text(&mut app), "small");
}

/// Selecting hands focus back to the header, and the header still tracks
/// the signal from there: the second selection moves the closed face too.
#[test]
fn a_focused_header_keeps_following_the_value_signal() {
    let mut app = build(FRUIT);
    focus_header(&mut app);
    select(&mut app, "fruit", "b");
    assert_eq!(header_text(&mut app), "Banana");
    select(&mut app, "fruit", "a");
    assert_eq!(header_text(&mut app), "Apple");
}

/// A value no option declares is shown as it stands, which is what leaves
/// a placeholder in place until something selects an option.
#[test]
fn a_value_no_option_declares_is_shown_as_it_stands() {
    let mut app = build(FRUIT);
    select(&mut app, "fruit", "kiwi");
    assert_eq!(header_text(&mut app), "kiwi");
}

/// Press and release the primary button at `p`, the way a window backend
/// reports a click, so the hit test decides what was clicked.
fn pointer_click(app: &mut App, p: glam::Vec2) {
    use bevy_ecs::message::Messages;
    use lumen_core::input::{
        PointerButton, PointerMoved, PointerPressed, PointerReleased, PointerState,
    };
    app.world.resource_mut::<PointerState>().position = Some(p);
    app.world
        .resource_mut::<Messages<PointerMoved>>()
        .write(PointerMoved {
            position: p,
            local: None,
        });
    app.tick();
    app.world.resource_mut::<PointerState>().primary_down = true;
    app.world
        .resource_mut::<Messages<PointerPressed>>()
        .write(PointerPressed {
            position: p,
            button: PointerButton::Primary,
            local: None,
        });
    app.tick();
    app.world.resource_mut::<PointerState>().primary_down = false;
    app.world
        .resource_mut::<Messages<PointerReleased>>()
        .write(PointerReleased {
            position: p,
            button: PointerButton::Primary,
            local: None,
        });
    for _ in 0..3 {
        app.tick();
    }
}

/// The centre of the first entity carrying `class` whose text is `text`
/// (any text when `text` is empty).
fn centre_of(app: &mut App, class: &str, text: &str) -> glam::Vec2 {
    use lumen_core::components::Transform;
    let mut q = app
        .world
        .query::<(&LumenClasses, Option<&TextContent>, &Transform)>();
    q.iter(&app.world)
        .find(|(classes, label, _)| {
            classes.0.iter().any(|c| c.as_ref() == class)
                && (text.is_empty() || label.is_some_and(|l| l.0 == text))
        })
        .map(|(_, _, t)| t.absolute + t.size * 0.5)
        .unwrap_or_else(|| panic!("no .{class} {text:?}"))
}

/// An option row with no background is still what a click on it hits.
/// The hit test once considered only elements that paint, scroll or take
/// focus, so with no `bg` on `.dropdown-option` (a bare app has none) the
/// click went through the panel to whatever lay under it.
#[test]
fn clicking_an_option_without_a_background_selects_it() {
    let mut app = build(
        r##"<root padding="20" gap="10">
  <dropdown width="160px" bind-value="theme">
    <option value="dark" label="Dark"/>
    <option value="light" label="Light"/>
  </dropdown>
  <button width="200px" height="80px" text="under"/>
</root>"##,
    );
    let header = centre_of(&mut app, "dropdown-button", "");
    pointer_click(&mut app, header);
    // The panel is measured, then placed below the header; read the row's
    // position once it has landed.
    for _ in 0..6 {
        app.tick();
    }
    let light = centre_of(&mut app, "dropdown-option", "Light");
    pointer_click(&mut app, light);
    assert_eq!(
        app.world
            .resource::<PropertyStore>()
            .get_global_str("theme")
            .as_deref(),
        Some("light")
    );
}
