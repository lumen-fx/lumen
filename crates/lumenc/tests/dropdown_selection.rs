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

/// A value no option declares is shown as it stands, which is what leaves
/// a placeholder in place until something selects an option.
#[test]
fn a_value_no_option_declares_is_shown_as_it_stands() {
    let mut app = build(FRUIT);
    select(&mut app, "fruit", "kiwi");
    assert_eq!(header_text(&mut app), "kiwi");
}
