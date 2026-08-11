// This suite exercises the linked runtime via `build_headless_app` /
// `RunOptions`, which lumenc only exposes under the `dev-run` feature.
// Gate the whole file so a thin (`--no-default-features`) `--all-targets`
// build compiles it out instead of failing on the missing symbols.
#![cfg(feature = "dev-run")]

//! End-to-end in-app drag-and-drop on the real `apps/kanban` app.
//!
//! Boots the kanban app through the same headless plugin stack `lumenc
//! run --headless` uses (no window, no GPU), drives a card drag from its
//! current lane onto a different lane by injecting the `DragStartEvent` /
//! `DragEndEvent` the pointer gesture would produce, and asserts the
//! card's `col` field flipped to the target lane - proving the
//! `DragEnd -> DropTarget hit-test -> DropAccepted -> on_drop -> reactive
//! rebuild` pipeline works against the actual markup + script.
//!
//! Runs headless (no real window) per the repo's automation policy.

use bevy_ecs::message::Messages;
use glam::Vec2;
use lumen_core::components::{LumenId, Transform};
use lumen_core::input::{DragEndEvent, DragStartEvent};
use lumen_core::render_world::Viewport;
use lumen_core::signals::ArraySignals;
use lumen_os_dnd::DragSource;
use lumenc::{RunOptions, build_headless_app};

/// Copy the real kanban app into an isolated temp dir with the MCP server
/// disabled (`port = 0`) so the test never binds a TCP port or races a
/// sibling run.
fn isolated_kanban() -> std::path::PathBuf {
    let src = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../..")
        .join("apps")
        .join("kanban");
    let dir = std::env::temp_dir().join(format!("lumenc-kanban-dnd-{}", std::process::id()));
    std::fs::create_dir_all(&dir).expect("mkdir temp app");
    for f in ["main.lmn", "main.css", "main.rhai"] {
        std::fs::copy(src.join(f), dir.join(f)).unwrap_or_else(|e| panic!("copy {f}: {e}"));
    }
    std::fs::write(
        dir.join("lumen.toml"),
        "[app]\nentry = \"main.lmn\"\n\n[window]\nsize = [1040, 720]\n\n[mcp]\nport = 0\n",
    )
    .expect("write lumen.toml");
    dir
}

/// Read the `col` field of the card whose `id` == `id` from the live
/// `cards` array signal.
fn card_col(app: &lumen_core::app::App, id: &str) -> Option<String> {
    let arr = app.world.resource::<ArraySignals>();
    arr.get("cards")?
        .iter()
        .find(|it| it.get("id").map(String::as_str) == Some(id))
        .and_then(|it| it.get("col").cloned())
}

/// `col` value -> the lane element id it renders into.
fn lane_id_for(col: &str) -> &'static str {
    match col {
        "backlog" => "lane-backlog",
        "doing" => "lane-doing",
        _ => "lane-done",
    }
}

#[test]
fn dragging_a_card_onto_another_lane_moves_it() {
    // The full app build + cascade + taffy layout recurses deeper than a
    // default 2 MiB test-thread stack; run the case on a roomy stack (the
    // windowed path runs on the 8 MiB main thread).
    std::thread::Builder::new()
        .stack_size(32 * 1024 * 1024)
        .spawn(run_case)
        .expect("spawn test thread")
        .join()
        .expect("kanban dnd case");
}

fn run_case() {
    let dir = isolated_kanban();
    let mut opts = RunOptions::new(&dir);
    opts.hot_reload = false; // no notify watcher thread in a test
    let (mut app, _winit) = build_headless_app(opts).expect("build headless kanban");

    // Give layout a real viewport so drop targets get non-zero bounds.
    app.world.resource_mut::<Viewport>().size = Vec2::new(1040.0, 720.0);

    // Settle: on_start seeds `cards`, refresh_board fans them into the
    // per-lane view arrays, the `<for>` reconciler spawns card rows, and
    // taffy lays them out.
    for _ in 0..20 {
        app.tick();
    }

    // Pick any spawned card (a DnD source carries its id as payload).
    let (card_entity, card_id) = {
        let mut q = app.world.query::<(bevy_ecs::entity::Entity, &DragSource)>();
        q.iter(&app.world)
            .next()
            .map(|(e, s)| (e, s.payload.text().unwrap_or_default()))
            .expect("at least one draggable card spawned")
    };
    assert!(!card_id.is_empty(), "card publishes an id payload");

    let from_col = card_col(&app, &card_id).expect("card has a col");
    // Choose a different destination lane than the card's current one.
    let to_col = if from_col == "done" {
        "backlog"
    } else {
        "done"
    };
    let target_lane = lane_id_for(to_col);

    // Locate the destination lane's on-screen centre.
    let lane_center = {
        let mut q = app.world.query::<(&LumenId, &Transform)>();
        let mut center = None;
        for (id, t) in q.iter(&app.world) {
            if id.0.as_str() == target_lane {
                center = Some(t.absolute + t.size * 0.5);
            }
        }
        center.unwrap_or_else(|| panic!("lane `{target_lane}` not laid out"))
    };
    assert!(
        lane_center.x > 0.0 && lane_center.y > 0.0,
        "lane centre must be positive ({lane_center:?}) - layout ran"
    );

    // Open the drag session (what crossing the drag threshold emits).
    app.world
        .resource_mut::<Messages<DragStartEvent>>()
        .write(DragStartEvent {
            entity: card_entity,
            start: Vec2::new(1.0, 1.0),
            position: Vec2::new(1.0, 1.0),
        });
    app.tick();

    // Release over the destination lane (what lifting the button emits).
    app.world
        .resource_mut::<Messages<DragEndEvent>>()
        .write(DragEndEvent {
            entity: card_entity,
            position: lane_center,
        });
    // Drop -> on_drop -> cards.set -> refresh_board -> reconcile.
    for _ in 0..12 {
        app.tick();
    }

    let now_col = card_col(&app, &card_id).expect("card still exists after move");
    assert_eq!(
        now_col, to_col,
        "card `{card_id}` should have moved from `{from_col}` to `{to_col}`, got `{now_col}`"
    );

    let _ = std::fs::remove_dir_all(&dir);
}
