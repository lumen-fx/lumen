// Drives the linked runtime with every script host, which needs
// `RunOptions` / `build_headless_app`; lumenc only exposes those under
// `dev-run`.
#![cfg(feature = "dev-run")]

//! A module's custom element event, end to end: raised from Rust, heard by
//! a candela, a Rhai and a Lua handler bound with `n.on`, its detail read in
//! each, and a handler's `prevent_default` reported back to whoever raised
//! it.

use bevy_ecs::entity::Entity;
use bevy_ecs::message::MessageReader;
use bevy_ecs::resource::Resource;
use bevy_ecs::schedule::IntoScheduleConfigs;
use bevy_ecs::system::ResMut;
use lumen_core::app::App;
use lumen_core::components::LumenId;
use lumen_core::prelude::TickStage;
use lumen_core::property_store::PropertyStore;
use lumen_script::{CustomEvent, CustomEventDelivered, ScriptSet};
use lumenc::RunOptions;
use lumenc::run::build_headless_app;

const MARKUP: &str = r#"<root>
  <column id="dock">
    <column id="panel"/>
  </column>
  <script src="main.cdl"/>
  <script src="side.rhai"/>
  <script src="side.lua"/>
</root>"#;

const CANDELA: &str = r#"import "lumen.cdl";
fn on_close(ev: int) {
    let detail = lumen::event_detail(ev) as {string: any};
    let reason = detail["reason"] as string;
    lumen::signal_set("candela_heard", lumen::event_type(ev) + ":" + reason);
    lumen::event_prevent_default(ev);
}
fn on_ready() {
    lumen::event_on(lumen::node_get_by_id("dock"), "panelclose", "on_close");
}
fn main() {}
"#;

const RHAI: &str = r#"fn on_ready() {
    get_by_id("dock").on("panelclose", |e| {
        signal("rhai_heard", "").set(e.event_type() + ":" + e.detail().reason);
    });
}
"#;

const LUA: &str = r#"function on_ready()
    get_by_id("dock"):on("panelclose", function(e)
        signal("lua_heard", ""):set(e:event_type() .. ":" .. e:detail().reason)
    end)
end
"#;

#[derive(Resource, Default)]
struct Delivered(Vec<(String, bool)>);

fn record(mut seen: MessageReader<CustomEventDelivered>, mut out: ResMut<Delivered>) {
    out.0.extend(
        seen.read()
            .map(|d| (d.event.name.clone(), d.default_prevented)),
    );
}

fn build() -> App {
    let dir = std::env::temp_dir().join(format!("lumenc_custom_events_{}", std::process::id()));
    let src = dir.join("src");
    std::fs::create_dir_all(&src).unwrap();
    for (name, text) in [
        ("main.lmn", MARKUP),
        ("main.cdl", CANDELA),
        ("side.rhai", RHAI),
        ("side.lua", LUA),
    ] {
        std::fs::write(src.join(name), text).unwrap();
    }
    std::fs::write(
        dir.join("lumen.toml"),
        "[mcp]\nport = 0\n\n[render]\nbackend = \"cpu\"\n",
    )
    .unwrap();
    let mut opts = RunOptions::new(&dir).with_parser(lumenc::default_parser());
    opts.hot_reload = false;
    let (mut app, _window) = build_headless_app(opts).expect("build_headless_app");
    app.world.init_resource::<Delivered>();
    app.add_systems(TickStage::Systems, record.after(ScriptSet::DomState));
    for _ in 0..4 {
        app.tick();
    }
    let _ = std::fs::remove_dir_all(&dir);
    app
}

fn entity(app: &mut App, id: &str) -> Entity {
    let mut q = app.world.query::<(Entity, &LumenId)>();
    q.iter(&app.world)
        .find(|(_, name)| name.0 == id)
        .map(|(e, _)| e)
        .unwrap_or_else(|| panic!("no element `{id}`"))
}

fn signal(app: &App, name: &str) -> Option<String> {
    app.world
        .resource::<PropertyStore>()
        .get_global_str(name)
        .map(|v| v.to_string())
}

#[test]
fn a_custom_event_reaches_every_host_and_reports_the_cancel() {
    let mut app = build();
    let panel = entity(&mut app, "panel");
    let mut close = CustomEvent::new(panel, "panelclose").with("reason", "user");
    close.cancelable = true;
    app.world.write_message(close);
    for _ in 0..3 {
        app.tick();
    }
    assert_eq!(
        signal(&app, "candela_heard").as_deref(),
        Some("panelclose:user")
    );
    assert_eq!(
        signal(&app, "rhai_heard").as_deref(),
        Some("panelclose:user")
    );
    assert_eq!(
        signal(&app, "lua_heard").as_deref(),
        Some("panelclose:user")
    );
    assert_eq!(
        app.world.resource::<Delivered>().0,
        vec![("panelclose".to_string(), true)],
        "the candela handler's prevent_default reached the raiser"
    );
}
