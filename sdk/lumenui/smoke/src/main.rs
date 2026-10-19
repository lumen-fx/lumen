//! Exercises what an app needs from `lumenui` alone: a UI its build script
//! compiled from `lumen.toml` (an included part, a widget of the app's own,
//! a `<for>` list), an `on_ready` handler that fills the list, systems,
//! resources, components, messages, run conditions, and a headless run.
//! Exits non-zero when any of it does not hold.

use std::sync::Mutex;

use lumenui::prelude::*;

#[derive(Resource, Default)]
struct Ticks(u32);

#[derive(Component)]
struct Spawned;

#[derive(Message)]
struct Pinged;

#[derive(Component, Clone, Debug, Default, Widget)]
#[widget(tag = "smoke-gauge")]
struct Gauge {
    #[widget(prop)]
    label: String,
}

/// What the systems saw, read back after the run.
static REPORT: Mutex<Report> = Mutex::new(Report {
    counted: 0,
    pings: 0,
    spawned: 0,
    gauges: 0,
    greeting: false,
    items: 0,
});

struct Report {
    counted: u32,
    pings: usize,
    spawned: usize,
    gauges: usize,
    greeting: bool,
    items: usize,
}

fn count(mut ticks: ResMut<Ticks>, mut out: MessageWriter<Pinged>) {
    ticks.0 += 1;
    out.write(Pinged);
}

fn under_three(ticks: Res<Ticks>) -> bool {
    ticks.0 < 3
}

fn spawn_once(mut commands: Commands, existing: Query<(), With<Spawned>>) {
    if existing.is_empty() {
        commands.spawn(Spawned);
    }
}

fn report(
    ticks: Res<Ticks>,
    mut pings: MessageReader<Pinged>,
    spawned: Query<(), With<Spawned>>,
    tags: Query<&LumenTag>,
    ids: Query<&LumenId>,
    texts: Query<&TextContent>,
) {
    let mut r = REPORT.lock().unwrap();
    r.counted = ticks.0;
    r.pings += pings.read().count();
    r.spawned = spawned.iter().count();
    r.gauges = tags.iter().filter(|t| &*t.0 == "smoke-gauge").count();
    r.greeting = ids.iter().any(|id| id.0 == "hi");
    r.items = texts.iter().filter(|t| t.0.starts_with("item ")).count();
}

fn main() {
    let result = lumenui::app!()
        .add_plugin(GaugePlugin)
        .insert_resource(Ticks::default())
        .add_message::<Pinged>()
        .on_ready(|ui| {
            ui.set_list(
                "items",
                (1..=3).map(|i| {
                    Record::new()
                        .with("id", i)
                        .with("name", format!("item {i}"))
                }),
            );
        })
        .add_systems(
            TickStage::Systems,
            (count.run_if(under_three), spawn_once, report).chain(),
        )
        .run_headless(6);
    if let Err(e) = result {
        eprintln!("lumenui-smoke: the app did not run: {e}");
        std::process::exit(1);
    }

    let r = REPORT.lock().unwrap();
    println!(
        "lumenui-smoke: counted {} ticks, {} pings, {} spawned, {} gauges, greeting {}, {} items",
        r.counted, r.pings, r.spawned, r.gauges, r.greeting, r.items
    );
    if r.counted != 3
        || r.pings != 3
        || r.spawned != 1
        || r.gauges != 1
        || !r.greeting
        || r.items != 3
    {
        eprintln!(
            "lumenui-smoke: expected 3 ticks, 3 pings, 1 spawned, 1 gauge, the greeting, 3 items"
        );
        std::process::exit(1);
    }
    println!("lumenui-smoke: ok");
}
