// Needs `RunOptions` / `build_headless_app`, only available under `dev-run`.
#![cfg(feature = "dev-run")]

//! Zebra striping a `<for>` list, headless, from real sources.
//!
//! Rows are built one at a time from a template, so the cascade that decides
//! `:nth-child()` for a row has to be told where that row landed. When it was
//! not, every row read as child 1 and `:nth-child(odd)` painted the whole
//! list.

use lumen_core::components::{Color, Fill, LumenId, Visuals};
use lumenc::RunOptions;
use lumenc::run::build_headless_app;

const MARKUP: &str = r#"<root id="app">
  <column id="list">
    <for each="rows" key="id">
      <tile class="row" id="r-{row.id}" />
    </for>
  </column>
  <script src="main.rhai" />
</root>
"#;

const SCRIPT: &str = r#"
fn on_start() {
  let list = [];
  let a = #{}; a.id = "a"; list.push(a);
  let b = #{}; b.id = "b"; list.push(b);
  let c = #{}; c.id = "c"; list.push(c);
  signal_array("rows").set(list);
}
"#;

const CSS: &str = r#"
.row { bg: #ffffff; }
.row:nth-child(odd) { bg: #ff0000; }
"#;

/// Whether a fill matches an `#rrggbb` literal, with the tolerance the
/// byte-to-f32 channel conversion needs.
fn near(a: Color, hex: &str) -> bool {
    let byte = |i: usize| {
        u8::from_str_radix(&hex[1 + i * 2..3 + i * 2], 16).expect("#rrggbb literal") as f32 / 255.0
    };
    (a.r - byte(0)).abs() < 0.01 && (a.g - byte(1)).abs() < 0.01 && (a.b - byte(2)).abs() < 0.01
}

#[test]
fn nth_child_stripes_a_for_list() {
    let dir =
        std::env::temp_dir().join(format!("lumen_for_structural_{}_{}", std::process::id(), {
            static SEQ: std::sync::atomic::AtomicUsize = std::sync::atomic::AtomicUsize::new(0);
            SEQ.fetch_add(1, std::sync::atomic::Ordering::Relaxed)
        }));
    let src = dir.join("src");
    std::fs::create_dir_all(&src).unwrap();
    std::fs::write(src.join("main.lmn"), MARKUP).unwrap();
    std::fs::write(src.join("main.css"), CSS).unwrap();
    std::fs::write(src.join("main.rhai"), SCRIPT).unwrap();
    // Port 0 keeps parallel test binaries off a shared socket.
    std::fs::write(
        dir.join("lumen.toml"),
        "[mcp]\nport = 0\n\n[script]\nengine = \"rhai\"\n",
    )
    .unwrap();

    let opts = RunOptions::new(&dir).with_parser(lumenc::default_parser());
    let (mut app, _window) = build_headless_app(opts).expect("build_headless_app");
    for _ in 0..8 {
        app.tick();
    }

    let mut q = app.world.query::<(&LumenId, &Visuals)>();
    let mut rows: Vec<(String, Color)> = q
        .iter(&app.world)
        .filter(|(id, _)| id.0.starts_with("r-"))
        .filter_map(|(id, v)| {
            v.fill
                .as_ref()
                .and_then(Fill::as_solid)
                .map(|c| (id.0.clone(), c))
        })
        .collect();
    rows.sort_by(|a, b| a.0.cmp(&b.0));
    let _ = std::fs::remove_dir_all(&dir);

    assert_eq!(rows.len(), 3, "three rows painted: {rows:?}");
    assert!(near(rows[0].1, "#ff0000"), "row 1 is odd");
    assert!(near(rows[1].1, "#ffffff"), "row 2 is even");
    assert!(near(rows[2].1, "#ff0000"), "row 3 is odd");
}
