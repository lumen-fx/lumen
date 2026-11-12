//! Drag and drop through the SDK's handlers: `App::on_drag_start` and
//! `App::on_drop` run with the drag's payload readable by MIME type and the
//! drop's position local to the target as it is painted.

// In an app this is the published `lumenui` crate; inside the workspace the
// engine crate it re-exports stands in for it under the same name.
extern crate lumenui_engine as lumenui;

use std::sync::{Mutex, MutexGuard, PoisonError};

use lumenui::bevy_ecs::hierarchy::ChildOf;
use lumenui::lumen_core::input::ScrollOffset;
use lumenui::lumen_os_dnd::mime::{MimeKind, MimePayload};
use lumenui::lumen_os_dnd::{DragStarted, DropAccepted, DropEffect};
use lumenui::prelude::*;

// An app publishes process-global state, so the tests here run one app at a
// time.
static APP_ISOLATION: Mutex<()> = Mutex::new(());

fn isolate() -> MutexGuard<'static, ()> {
    APP_ISOLATION.lock().unwrap_or_else(PoisonError::into_inner)
}

fn signal(app: &EcsApp, name: &str) -> Option<String> {
    app.world
        .resource::<PropertyStore>()
        .get_global_str(name)
        .map(|s| s.to_string())
}

/// A drop on a target inside a scrolled container runs its `on_drop`, which
/// reads every kind the drag carries and where on the target it landed.
#[test]
fn on_drop_reads_the_payload_and_the_local_position() {
    let _guard = isolate();
    let mut app = lumenui::App::from_markup("<root/>")
        .on_drop("viewport", |ui, ev| {
            assert_eq!(ev.kind(), EventKind::Drop);
            assert_eq!(ev.id(), Some("viewport"));
            assert_eq!(ev.types(), ["application/x-asset", "text/plain"]);
            assert_eq!(ev.data("text").as_deref(), Some("Rock"));
            assert_eq!(ev.data("image/png"), None);
            assert_eq!(ev.position(), Some(Vec2::new(110.0, 45.0)));
            assert_eq!(ev.local(), Some(Vec2::new(10.0, 15.0)));
            ui.set(
                "dropped",
                ev.data("application/x-asset").unwrap_or_default(),
            );
        })
        .build_bare();
    let list = app.world.spawn(ScrollOffset(Vec2::new(0.0, 20.0))).id();
    let viewport = app
        .world
        .spawn((
            LumenId("viewport".into()),
            Transform::new(Vec2::new(100.0, 50.0), Vec2::new(200.0, 100.0)),
            ChildOf(list),
        ))
        .id();
    let payload = MimePayload::new()
        .with(MimeKind::from("application/x-asset"), b"rock.png".to_vec())
        .with(MimeKind::TextPlain, b"Rock".to_vec());
    app.world.write_message(DropAccepted {
        source: None,
        target: viewport,
        payload,
        effect: DropEffect::Copy,
        position: Vec2::new(110.0, 45.0),
    });
    app.tick();
    assert_eq!(signal(&app, "dropped").as_deref(), Some("rock.png"));
}

/// A drag starting on an element runs its `on_drag_start` with the payload.
#[test]
fn on_drag_start_reads_the_payload() {
    let _guard = isolate();
    let mut app = lumenui::App::from_markup("<root/>")
        .on_drag_start("card", |ui, ev| {
            assert_eq!(ev.kind(), EventKind::DragStart);
            ui.set("started", ev.data("text/plain").unwrap_or_default());
        })
        .build_bare();
    let card = app.world.spawn(LumenId("card".into())).id();
    let icon = app.world.spawn(ChildOf(card)).id();
    app.world.write_message(DragStarted {
        source: icon,
        payload: "card-3".into(),
        position: Vec2::new(4.0, 4.0),
    });
    app.tick();
    assert_eq!(signal(&app, "started").as_deref(), Some("card-3"));
}
