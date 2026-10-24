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
  <script src="main.cdl" />
</root>
"#;

const SCRIPT: &str = r#"import "lumen.cdl";

fn on_start() {
    lumen::signal_array_set("rows", [{"id": "a"}, {"id": "b"}, {"id": "c"}]);
}

fn main() {}
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
    std::fs::write(src.join("main.cdl"), SCRIPT).unwrap();
    // Port 0 keeps parallel test binaries off a shared socket.
    std::fs::write(dir.join("lumen.toml"), "[mcp]\nport = 0\n").unwrap();

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

/// A hierarchy list, its rows virtualized or not: each row's name is
/// styled by the row's classes, and a plain part by the list's id.
fn hier_markup(virtualized: bool) -> String {
    format!(
        r#"<root id="app">
  <scroll id="list" style="height: 200px">
    <for each="rows" key="id" virtualized="{virtualized}" row-height="22">
      <row class="hier-row {{row.depth}} {{row.sel}} {{row.kind}}" id="r-{{row.id}}">
        <label class="hier-name" id="n-{{row.id}}" text="{{row.id}}" />
        <label class="plain" id="p-{{row.id}}" text="{{row.id}}" />
      </row>
    </for>
  </scroll>
</root>
"#
    )
}

const HIER_CSS: &str = r#"
.hier-row.runtime .hier-name { text-color: #ff0000; }
.hier-name { text-color: #ffffff; }
#list .plain { text-color: #00ff00; }
.plain { text-color: #ffffff; }
"#;

fn hier_row(id: &str, kind: &str) -> lumen_core::signals::ArrayItem {
    [("id", id), ("depth", "d1"), ("sel", ""), ("kind", kind)]
        .into_iter()
        .map(|(k, v)| (k.to_string(), v.to_string()))
        .collect()
}

/// The text color of each row's name, by row id, and whether every row's
/// plain part took the list's rule, once `rows` is set and the app has
/// settled.
fn name_colors(
    app: &mut lumen_core::prelude::App,
    rows: Vec<lumen_core::signals::ArrayItem>,
) -> (Vec<(String, Option<Color>)>, bool) {
    use lumen_core::components::TextStyle;
    app.world
        .resource_mut::<lumen_core::signals::ArraySignals>()
        .set("rows", rows);
    for _ in 0..8 {
        app.tick();
    }
    let mut q = app.world.query::<(&LumenId, Option<&TextStyle>)>();
    let mut out: Vec<(String, Option<Color>)> = q
        .iter(&app.world)
        .filter(|(id, _)| id.0.starts_with("n-"))
        .map(|(id, t)| (id.0.clone(), t.map(|t| t.color)))
        .collect();
    out.sort_by(|a, b| a.0.cmp(&b.0));
    let plain: Vec<(String, Option<Color>)> = q
        .iter(&app.world)
        .filter(|(id, _)| id.0.starts_with("p-"))
        .map(|(id, t)| (id.0.clone(), t.map(|t| t.color)))
        .collect();
    let listed = plain.len() == out.len()
        && plain
            .iter()
            .all(|(_, c)| c.is_some_and(|c| near(c, "#00ff00")));
    (out, listed)
}

/// A class a row's record gives an ancestor is one a descendant rule reads:
/// the row's parts are styled as the row's classes say, from the source and
/// from the compiled artifact alike, again when the records change, and on a
/// row that mounts after the list first showed; and a rule that reads the
/// list's id reaches every row's parts.
#[test]
fn a_class_from_the_record_styles_the_rows_descendants() {
    for (compiled, virtualized) in [(false, false), (false, true), (true, true)] {
        let dir = std::env::temp_dir().join(format!(
            "lumen_for_row_descendant_{}_{compiled}_{virtualized}",
            std::process::id()
        ));
        let src = dir.join("src");
        std::fs::create_dir_all(&src).unwrap();
        std::fs::write(src.join("main.lmn"), hier_markup(virtualized)).unwrap();
        std::fs::write(src.join("main.css"), HIER_CSS).unwrap();
        std::fs::write(dir.join("lumen.toml"), "[mcp]\nport = 0\n").unwrap();
        let opts = if compiled {
            let compiled = lumenc::compile_app(&dir).expect("the app compiles");
            let bytes = lumen_ir::artifact::serialize(&compiled).expect("serializes");
            RunOptions::new(&dir).with_artifact_bytes(bytes)
        } else {
            RunOptions::new(&dir).with_parser(lumenc::default_parser())
        };
        let (mut app, _window) = build_headless_app(opts).expect("build_headless_app");
        let first = name_colors(
            &mut app,
            vec![hier_row("a", "runtime"), hier_row("b", "scene")],
        );
        // The records swap kinds: the same rows restyle their parts.
        let swapped = name_colors(
            &mut app,
            vec![hier_row("a", "scene"), hier_row("b", "runtime")],
        );
        // A row that arrives later is styled the same way.
        let grown = name_colors(
            &mut app,
            vec![
                hier_row("a", "scene"),
                hier_row("b", "runtime"),
                hier_row("c", "runtime"),
            ],
        );
        let _ = std::fs::remove_dir_all(&dir);

        let case = format!("compiled {compiled}, virtualized {virtualized}");
        let red = |c: &Option<Color>| c.is_some_and(|c| near(c, "#ff0000"));
        let white = |c: &Option<Color>| c.is_some_and(|c| near(c, "#ffffff"));
        let (first, first_listed) = first;
        let (swapped, swapped_listed) = swapped;
        let (grown, grown_listed) = grown;
        assert!(
            first.len() == 2 && red(&first[0].1) && white(&first[1].1),
            "{case}: the runtime row's name is red, the other's white: {first:?}"
        );
        assert!(
            swapped.len() == 2 && white(&swapped[0].1) && red(&swapped[1].1),
            "{case}: the names follow the rows' new kinds: {swapped:?}"
        );
        assert!(
            grown.len() == 3 && white(&grown[0].1) && red(&grown[1].1) && red(&grown[2].1),
            "{case}: a row mounted later is styled like the others: {grown:?}"
        );
        assert!(
            first_listed && swapped_listed && grown_listed,
            "{case}: every row's plain part takes the list's rule"
        );
    }
}
