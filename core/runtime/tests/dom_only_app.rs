//! A lookup made on a thread that ticks no app reads the process's only app.
//! Its own test binary, so no other test's app is alive beside it.

use lumen_ir::artifact::{self, CompiledApp};
use lumen_ir::layout_ir::{Attributes, Element, LayoutIR};
use lumen_runtime::{RunOptions, build_headless_app};
use lumen_script::node_query;

#[test]
fn another_thread_reads_the_only_app() {
    let dir = std::env::temp_dir().join(format!("lumen_dom_only_app_{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    let button = Element {
        tag: "button".to_string(),
        attrs: Box::new(Attributes {
            id: Some("save".to_string()),
            ..Default::default()
        }),
        ..Default::default()
    };
    let ir = LayoutIR {
        root: Element {
            tag: "root".to_string(),
            children: vec![button],
            ..Default::default()
        },
        ..Default::default()
    };
    let bytes = artifact::serialize(&CompiledApp {
        ir,
        ..Default::default()
    })
    .expect("serialize fixture artifact");
    let mut opts = RunOptions::new(&dir).with_artifact_bytes(bytes);
    opts.bounded = true;
    let (mut app, _window) = build_headless_app(opts).expect("build headless app");
    for _ in 0..3 {
        app.tick();
    }

    let mine = node_query::run_get_by_id("save");
    let elsewhere = std::thread::spawn(|| node_query::run_get_by_id("save"))
        .join()
        .expect("lookup thread");
    assert!(mine.is_some(), "the ticking thread finds #save");
    assert_eq!(
        elsewhere, mine,
        "a thread that ticks nothing reads the only app"
    );
    let _ = std::fs::remove_dir_all(&dir);
}
