// Drives the real pipeline (parse -> cascade -> spawn -> layout -> input),
// which needs `RunOptions` / `build_headless_app`; lumenc only exposes those
// under `dev-run`.
#![cfg(feature = "dev-run")]

//! The error banner a run shows when the app's script does not load: it
//! names the file, line and column, shows the line, can be selected from and
//! copied, and its Copy button copies the whole report. The clipboard is an
//! in-memory one, never the desktop's.

use std::path::PathBuf;

use bevy_ecs::entity::Entity;
use bevy_ecs::query::With;
use glam::Vec2;
use lumen_core::app::App;
use lumen_core::components::{DomHidden, ReadOnly, TextContent, TextInput, Transform};
use lumen_core::input::{
    Key, KeyCode, KeyPressed, Modifiers, PointerButton, PointerMoved, PointerPressed,
    PointerReleased, PointerState,
};
use lumen_os_clipboard::ClipboardHost;
use lumenc::RunOptions;
use lumenc::run::{ErrorBanner, ErrorBannerCopy, ErrorBannerMarker, build_headless_app};

const BROKEN: &str = "import \"lumen.cdl\";\nfn on_ready() {\n    let n = 1;\n    \
                      missing_function(n);\n}\nfn main() {}\n";

/// An app whose script calls a function that does not exist, built and
/// ticked until the banner is up, with an in-memory clipboard in place of
/// the system one.
fn broken_app(tag: &str) -> (App, PathBuf) {
    let dir =
        std::env::temp_dir().join(format!("lumenc_error_banner_{tag}_{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    let src = dir.join("src");
    std::fs::create_dir_all(&src).unwrap();
    std::fs::write(
        src.join("main.lmn"),
        "<root>\n  <button id=\"under\" width=\"100%\" height=\"100%\" text=\"app\" />\n  \
         <script src=\"main.cdl\" />\n</root>\n",
    )
    .unwrap();
    std::fs::write(src.join("main.cdl"), BROKEN).unwrap();
    std::fs::write(
        dir.join("lumen.toml"),
        "[mcp]\nport = 0\n\n[render]\nbackend = \"cpu\"\n",
    )
    .unwrap();
    let mut opts = RunOptions::new(&dir).with_parser(lumenc::default_parser());
    opts.hot_reload = false;
    let (mut app, _window) = build_headless_app(opts).expect("build_headless_app");
    app.world.insert_non_send(ClipboardHost::in_memory());
    lumen_core::window::primary_viewport_mut(&mut app.world).size = Vec2::new(900.0, 600.0);
    for _ in 0..4 {
        app.tick();
    }
    (app, dir)
}

/// The banner's read-only text field.
fn field(app: &mut App) -> Entity {
    let mut q = app
        .world
        .query_filtered::<Entity, (With<TextInput>, With<ReadOnly>, With<DomHidden>)>();
    let found: Vec<Entity> = q.iter(&app.world).collect();
    assert_eq!(found.len(), 1, "one read-only field in the banner");
    found[0]
}

fn text_of(app: &App, entity: Entity) -> String {
    app.world
        .get::<TextContent>(entity)
        .map(|t| t.0.clone())
        .unwrap_or_default()
}

fn center(app: &App, entity: Entity) -> Vec2 {
    let t = app
        .world
        .get::<Transform>(entity)
        .copied()
        .expect("laid out");
    t.absolute + t.size / 2.0
}

fn pointer(app: &mut App, at: Vec2, press: Option<bool>) {
    app.world.resource_mut::<PointerState>().position = Some(at);
    app.world.write_message(PointerMoved {
        position: at,
        local: None,
        pointer: Default::default(),
    });
    match press {
        Some(true) => {
            app.world.resource_mut::<PointerState>().primary_down = true;
            app.world.write_message(PointerPressed {
                position: at,
                button: PointerButton::Primary,
                local: None,
                pointer: Default::default(),
            });
        }
        Some(false) => {
            app.world.resource_mut::<PointerState>().primary_down = false;
            app.world.write_message(PointerReleased {
                position: at,
                button: PointerButton::Primary,
                local: None,
                pointer: Default::default(),
            });
        }
        None => {}
    }
    app.tick();
}

fn chord(app: &mut App, ch: &str, ctrl: bool) {
    app.world.write_message(KeyPressed {
        key: Key::Character(ch.to_string()),
        code: KeyCode::Unidentified,
        modifiers: Modifiers {
            ctrl,
            ..Modifiers::default()
        },
        repeat: false,
        time_stamp: lumen_core::time::Instant::now(),
    });
    app.tick();
}

fn clipboard(app: &App) -> String {
    app.world.non_send::<ClipboardHost>().read_text()
}

/// The banner reads `file:line:col` and shows the offending line under it,
/// in a field that is marked read-only and kept out of the script's DOM.
#[test]
fn the_banner_points_at_the_line() {
    let (mut app, dir) = broken_app("points");
    let banners = app
        .world
        .query_filtered::<Entity, With<ErrorBannerMarker>>()
        .iter(&app.world)
        .count();
    assert_eq!(banners, 1, "the banner is up");
    let field = field(&mut app);
    let text = text_of(&app, field);
    let file = dir.join("src").join("main.cdl");
    assert!(
        text.contains(&format!("--> {}:4:", file.display())),
        "{text}"
    );
    assert!(text.contains("4 |     missing_function(n);"), "{text}");
    assert_eq!(
        Some(text),
        app.world
            .resource::<ErrorBanner>()
            .0
            .as_ref()
            .map(ToString::to_string),
        "the field holds the whole report"
    );
    let _ = std::fs::remove_dir_all(&dir);
}

/// Clicking Copy puts the whole report on the clipboard. The click goes
/// through the hit test, so the banner takes it from the app beneath.
#[test]
fn the_copy_button_copies_the_report() {
    let (mut app, dir) = broken_app("copy");
    let field = field(&mut app);
    let report = text_of(&app, field);
    let copy = app
        .world
        .query_filtered::<Entity, With<ErrorBannerCopy>>()
        .single(&app.world)
        .expect("one Copy button");
    let at = center(&app, copy);
    pointer(&mut app, at, None);
    pointer(&mut app, at, Some(true));
    pointer(&mut app, at, Some(false));
    app.tick();
    assert_eq!(clipboard(&app), report);
    let _ = std::fs::remove_dir_all(&dir);
}

/// The report is selectable: a drag across it selects text, Ctrl+A then
/// Ctrl+C copies all of it, and typing changes nothing.
#[test]
fn the_report_can_be_selected_and_copied_but_not_edited() {
    let (mut app, dir) = broken_app("select");
    let field = field(&mut app);
    let report = text_of(&app, field);
    let t = *app.world.get::<Transform>(field).expect("laid out");
    let start = t.absolute + Vec2::new(4.0, 6.0);
    let end = t.absolute + Vec2::new(t.size.x * 0.5, 6.0);
    pointer(&mut app, start, None);
    pointer(&mut app, start, Some(true));
    pointer(&mut app, end, None);
    pointer(&mut app, end, Some(false));
    let input = app.world.get::<TextInput>(field).expect("text input");
    assert!(
        input.selection_anchor.is_some_and(|a| a != input.cursor),
        "a drag selects part of the report"
    );

    chord(&mut app, "a", true);
    chord(&mut app, "c", true);
    assert_eq!(clipboard(&app), report);

    chord(&mut app, "x", false);
    chord(&mut app, "v", true);
    chord(&mut app, "x", true);
    assert_eq!(text_of(&app, field), report, "the report is read-only");
    let _ = std::fs::remove_dir_all(&dir);
}
