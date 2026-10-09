// Drives the real hot-reload path (parse -> spawn -> edit -> respawn), which
// needs `RunOptions` / `build_headless_app`; lumenc only exposes those under
// `dev-run`.
#![cfg(feature = "dev-run")]

//! A hot reload keeps the state a running app gave its elements, and takes
//! everything else from the edited markup. Text is both: an element's
//! authored text is markup, while what a user typed or a script set is state.

use std::time::{Duration, Instant};

use lumen_core::app::App;
use lumen_core::components::{LumenId, TextContent};
use lumenc::RunOptions;
use lumenc::run::build_headless_app;

fn text_of(app: &mut App, id: &str) -> String {
    let mut q = app.world.query::<(&LumenId, &TextContent)>();
    q.iter(&app.world)
        .find(|(name, _)| name.0 == id)
        .map(|(_, t)| t.0.clone())
        .unwrap_or_else(|| panic!("no element with id `{id}`"))
}

const BEFORE: &str = r#"<root>
  <button id="bump" width="120px" height="48px" text="+1" />
  <input id="field" text="" />
</root>
"#;

const AFTER: &str = r#"<root>
  <button id="bump" width="120px" height="48px" text="plus" />
  <input id="field" text="" />
</root>
"#;

#[test]
fn a_reload_takes_edited_text_and_keeps_typed_text() {
    let dir = std::env::temp_dir().join(format!("lumenc_hot_text_{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(dir.join("src")).unwrap();
    std::fs::write(
        dir.join("lumen.toml"),
        "[app]\nentry = \"main.lmn\"\n\n[mcp]\nport = 0\n\n[runtime]\nhot_reload = true\n",
    )
    .unwrap();
    let markup = dir.join("src/main.lmn");
    std::fs::write(&markup, BEFORE).unwrap();

    let opts = RunOptions::new(&dir).with_parser(lumenc::default_parser());
    let (mut app, _window) = build_headless_app(opts).expect("build_headless_app");
    for _ in 0..4 {
        app.tick();
    }
    assert_eq!(text_of(&mut app, "bump"), "+1");

    // What a user typed is the app's state, not the markup's.
    {
        let mut q = app.world.query::<(&LumenId, &mut TextContent)>();
        for (id, mut text) in q.iter_mut(&mut app.world) {
            if id.0 == "field" {
                text.0 = "typed".to_string();
            }
        }
    }

    // A later mtime than the first write, on any filesystem clock.
    std::thread::sleep(Duration::from_millis(1100));
    std::fs::write(&markup, AFTER).unwrap();
    let deadline = Instant::now() + Duration::from_secs(20);
    while text_of(&mut app, "bump") != "plus" {
        assert!(
            Instant::now() < deadline,
            "the edited text never landed: still {:?}",
            text_of(&mut app, "bump")
        );
        app.tick();
        std::thread::sleep(Duration::from_millis(20));
    }
    assert_eq!(text_of(&mut app, "field"), "typed", "typed text survives");
    let _ = std::fs::remove_dir_all(&dir);
}
