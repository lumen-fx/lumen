// Drives the real pipeline (parse -> cascade -> spawn -> layout), which needs
// `RunOptions` / `build_headless_app`; lumenc only exposes those under
// `dev-run`.
#![cfg(feature = "dev-run")]

//! Sizing and painting a `<dropdown>`.
//!
//! The tag expands into a header button over a floating panel, and the box
//! holding them is what an author sizes: `width` on the markup and a
//! `dropdown` rule in a stylesheet both have to reach it, the same way
//! they reach a `<button>`. The parts underneath take their metrics from
//! the user-agent sheet, so a skin can change them.
//!
//! Headless: no window, no GPU.

use lumen_core::app::App;
use lumen_core::components::{LumenClasses, LumenId, Transform, Visuals};
use lumen_core::property_store::PropertyStore;
use lumenc::RunOptions;
use lumenc::run::build_headless_app;

fn build(markup: &str, css: &str) -> App {
    let dir =
        std::env::temp_dir().join(format!("lumenc_dropdown_sty_{}_{}", std::process::id(), {
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
    let (mut app, _window) = build_headless_app(opts).expect("build_headless_app");
    for _ in 0..6 {
        app.tick();
    }
    let _ = std::fs::remove_dir_all(&dir);
    app
}

/// The laid-out rect of the first element carrying `class`.
fn rect_of_class(app: &mut App, class: &str) -> Transform {
    let mut q = app.world.query::<(&LumenClasses, &Transform)>();
    q.iter(&app.world)
        .find(|(classes, _)| classes.0.iter().any(|c| c.as_ref() == class))
        .map(|(_, t)| *t)
        .unwrap_or_else(|| panic!("no element with class `{class}`"))
}

/// A `<dropdown>` inside a `<row>`, so nothing stretches it and its width
/// is whatever it asks for.
const IN_A_ROW: &str = r##"<root>
  <row>
    <dropdown id="picker" bind-value="fruit" width="300">
      <option value="a" label="Apple"/>
      <option value="b" label="Banana"/>
    </dropdown>
  </row>
</root>"##;

const UNSIZED: &str = r##"<root>
  <row>
    <dropdown bind-value="fruit">
      <option value="a" label="Apple"/>
      <option value="b" label="Banana"/>
    </dropdown>
  </row>
</root>"##;

/// `width` on the markup sizes the control, and the closed face fills it.
#[test]
fn a_width_on_the_markup_sizes_the_control() {
    let mut app = build(IN_A_ROW, "");
    assert_eq!(rect_of_class(&mut app, "dropdown").size.x, 300.0);
    assert_eq!(rect_of_class(&mut app, "dropdown-button").size.x, 300.0);
}

/// A rule written against the tag reaches the same box, so the control can
/// be sized from a stylesheet like any other widget.
#[test]
fn a_rule_on_the_tag_sizes_the_control() {
    let mut app = build(UNSIZED, "dropdown { width: 260; }");
    assert_eq!(rect_of_class(&mut app, "dropdown").size.x, 260.0);
}

/// `min-width` too, which is the floor a control that would otherwise fit
/// its text needs.
#[test]
fn a_min_width_on_the_tag_holds_the_control_open() {
    let mut app = build(UNSIZED, "dropdown { min-width: 240; }");
    assert!(
        rect_of_class(&mut app, "dropdown").size.x >= 240.0,
        "min-width must floor the control, got {}",
        rect_of_class(&mut app, "dropdown").size.x
    );
}

/// `padding` insets the parts from the box's edge.
#[test]
fn padding_on_the_tag_insets_the_face() {
    let mut app = build(IN_A_ROW, "dropdown { padding: 6; }");
    let box_rect = rect_of_class(&mut app, "dropdown");
    let face = rect_of_class(&mut app, "dropdown-button");
    assert_eq!(face.absolute.x - box_rect.absolute.x, 6.0);
    assert_eq!(face.size.x, box_rect.size.x - 12.0);
}

/// `bg` paints the box.
#[test]
fn a_background_on_the_tag_paints_the_control() {
    let mut app = build(IN_A_ROW, "dropdown { bg: #123456; }");
    let painted = {
        let mut q = app.world.query::<(&LumenClasses, &Visuals)>();
        q.iter(&app.world).any(|(classes, visuals)| {
            classes.0.iter().any(|c| c.as_ref() == "dropdown") && visuals.fill.is_some()
        })
    };
    assert!(painted, "a `dropdown` rule's fill must reach the control");
}

/// `id` names the control, so a script can find it and `#id` can style it.
#[test]
fn an_id_on_the_markup_names_the_control() {
    let mut app = build(IN_A_ROW, "");
    let named = {
        let mut q = app.world.query::<(&LumenId, &LumenClasses)>();
        q.iter(&app.world).any(|(id, classes)| {
            id.0 == "picker" && classes.0.iter().any(|c| c.as_ref() == "dropdown")
        })
    };
    assert!(named, "the authored id must land on the control");
}

/// The closed face's height is a theme metric rather than a number in the
/// parser, so a stylesheet can change it.
#[test]
fn the_face_height_comes_from_the_stylesheet() {
    let mut app = build(IN_A_ROW, ".dropdown-button { min-height: 52; }");
    assert_eq!(rect_of_class(&mut app, "dropdown-button").size.y, 52.0);
}

/// So is an option row's.
#[test]
fn an_option_row_height_comes_from_the_stylesheet() {
    let mut app = build(IN_A_ROW, ".dropdown-option { min-height: 44; }");
    app.world
        .resource_mut::<PropertyStore>()
        .set_global_bool("__dropdown_open:fruit", true);
    for _ in 0..4 {
        app.tick();
    }
    assert_eq!(rect_of_class(&mut app, "dropdown-option").size.y, 44.0);
}
