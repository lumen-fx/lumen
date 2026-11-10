// Drives the real pipeline (parse -> cascade -> spawn -> layout), which needs
// `RunOptions` / `build_headless_app`; lumenc only exposes those under
// `dev-run`.
#![cfg(feature = "dev-run")]

//! CSS keyframe animations on the desktop, through the whole pipeline: the
//! cascade resolves `animation` declarations, the keyframe engine runs them,
//! and what it holds reaches the components that paint.
//!
//! Time is moved by shifting an animation's start back rather than by
//! sleeping, so every sample lands where the test says.
//!
//! Headless: no window, no GPU.

use std::time::Duration;

use bevy_ecs::entity::Entity;
use bevy_ecs::message::Messages;
use lumen_animations::RunningAnimations;
use lumen_core::app::App;
use lumen_core::components::{Fill, LumenId, Opacity, Visuals};
use lumen_core::render_world::AnimationsActive;
use lumen_core::tick::WakeDeadline;
use lumen_core::transform::ElementTransform;
use lumen_script::CustomEvent;
use lumenc::RunOptions;
use lumenc::run::build_headless_app;

static SERIAL: std::sync::Mutex<()> = std::sync::Mutex::new(());

struct Harness {
    app: App,
    events: Vec<(String, String, f64)>,
    _guard: std::sync::MutexGuard<'static, ()>,
}

impl Harness {
    fn new(markup: &str, css: &str) -> Self {
        let guard = SERIAL.lock().unwrap_or_else(|e| e.into_inner());
        let dir = std::env::temp_dir().join(format!("lumenc_anim_{}_{}", std::process::id(), {
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
        let (app, _window) = build_headless_app(opts).expect("build_headless_app");
        let _ = std::fs::remove_dir_all(&dir);
        let mut h = Self {
            app,
            events: Vec::new(),
            _guard: guard,
        };
        h.ticks(4);
        h
    }

    fn ticks(&mut self, n: usize) {
        for _ in 0..n {
            self.app.tick();
            self.collect();
        }
    }

    /// Every animation event raised since the last look, as
    /// `(name, animation_name, elapsed_time)`.
    fn collect(&mut self) {
        let Some(mut messages) = self.app.world.get_resource_mut::<Messages<CustomEvent>>() else {
            return;
        };
        for ev in messages.drain() {
            if !ev.name.starts_with("animation") {
                continue;
            }
            let name = match ev.detail.get("animation_name") {
                Some(lumen_script::ScriptValue::Str(s)) => s.clone(),
                _ => String::new(),
            };
            let elapsed = match ev.detail.get("elapsed_time") {
                Some(lumen_script::ScriptValue::F64(f)) => *f,
                _ => -1.0,
            };
            self.events.push((ev.name.clone(), name, elapsed));
        }
    }

    fn by_id(&mut self, id: &str) -> Entity {
        let mut q = self.app.world.query::<(Entity, &LumenId)>();
        q.iter(&self.app.world)
            .find(|(_, l)| l.0 == id)
            .map(|(e, _)| e)
            .unwrap_or_else(|| panic!("no element with id `{id}`"))
    }

    /// Hold every animation on `e` at `ms` into its timeline and tick, so
    /// the sample lands exactly there however long the ticks take.
    fn at(&mut self, e: Entity, ms: u64) {
        if let Some(mut running) = self.app.world.get_mut::<RunningAnimations>(e) {
            for r in running.0.iter_mut() {
                r.paused_at = Some(ms as f32 / 1000.0);
            }
        }
        self.ticks(1);
    }

    /// Let every animation on `e` run on from `ms` into its timeline.
    fn resume_from(&mut self, e: Entity, ms: u64) {
        let now = self.app.world.resource::<lumen_core::tick::Tick>().now;
        if let Some(mut running) = self.app.world.get_mut::<RunningAnimations>(e) {
            for r in running.0.iter_mut() {
                r.paused_at = None;
                r.start = now - Duration::from_millis(ms);
            }
        }
        self.ticks(1);
    }

    fn opacity(&self, e: Entity) -> f32 {
        self.app.world.get::<Opacity>(e).map_or(1.0, |o| o.0)
    }

    fn pointer(&mut self, pos: glam::Vec2) {
        self.app
            .world
            .resource_mut::<lumen_core::input::PointerState>()
            .position = Some(pos);
        self.app
            .world
            .write_message(lumen_core::input::PointerMoved {
                position: pos,
                local: None,
                pointer: Default::default(),
            });
        self.ticks(2);
    }

    fn hover(&mut self, e: Entity) {
        let t = *self
            .app
            .world
            .get::<lumen_core::components::Transform>(e)
            .expect("laid out");
        self.pointer(t.absolute + t.size * 0.5);
    }

    fn running(&self, e: Entity) -> usize {
        self.app
            .world
            .get::<RunningAnimations>(e)
            .map_or(0, |r| r.0.len())
    }

    fn kinds(&self) -> Vec<&str> {
        self.events.iter().map(|(k, _, _)| k.as_str()).collect()
    }
}

/// Within what a debug-build tick can add to a timeline between two
/// samples.
fn near(a: f32, b: f32) -> bool {
    (a - b).abs() < 0.06
}

#[test]
fn a_keyframe_animation_plays_and_lets_go() {
    let mut h = Harness::new(
        r##"<root><tile id="t" class="fade" width="40" height="40" bg="#336699"/></root>"##,
        "@keyframes fade { from { opacity: 0 } to { opacity: 1 } }\n\
         .fade { animation: fade 1000ms linear; }",
    );
    let t = h.by_id("t");
    assert_eq!(h.running(t), 1, "the animation runs");
    assert!(
        h.app.world.resource::<AnimationsActive>().get(),
        "a running animation keeps the loop drawing"
    );
    h.at(t, 0);
    assert!(near(h.opacity(t), 0.0), "the first frame: {}", h.opacity(t));
    h.at(t, 500);
    assert!(near(h.opacity(t), 0.5), "halfway: {}", h.opacity(t));
    h.resume_from(t, 1100);
    h.ticks(1);
    assert!(
        near(h.opacity(t), 1.0),
        "lets go to the element's own value"
    );
    assert!(
        !h.app.world.resource::<AnimationsActive>().get(),
        "an ended animation asks for no frames"
    );
    assert_eq!(h.kinds(), ["animationstart", "animationend"]);
    assert_eq!(h.events[1].1, "fade");
    assert!(
        (h.events[1].2 - 1.0).abs() < 1e-6,
        "elapsed is the active duration"
    );
}

#[test]
fn fill_mode_iterations_and_direction() {
    let mut h = Harness::new(
        r#"<root><tile id="t" class="a" width="40" height="40"/></root>"#,
        "@keyframes spin { to { rotate: 90deg } }\n\
         .a { animation: spin 2s linear 2 alternate forwards; }",
    );
    let t = h.by_id("t");
    h.at(t, 3000);
    let rot = |h: &Harness| {
        h.app
            .world
            .get::<ElementTransform>(t)
            .map_or(0.0, |x| x.rotate)
    };
    assert!(
        (rot(&h) - 45.0).abs() < 0.5,
        "second iteration runs back: {}",
        rot(&h)
    );
    h.resume_from(t, 4100);
    h.ticks(1);
    assert!(near(rot(&h), 0.0), "alternate ends where it began");
    assert_eq!(h.running(t), 1, "forwards keeps holding the end");
    assert_eq!(
        h.kinds(),
        ["animationstart", "animationiteration", "animationend"]
    );
    assert!(
        !h.app.world.resource::<AnimationsActive>().get(),
        "an ended animation asks for no frames"
    );
}

#[test]
fn a_state_rule_starts_and_cancels_an_animation() {
    let mut h = Harness::new(
        r#"<root><tile id="t" class="b" width="40" height="40"/></root>"#,
        "@keyframes pulse { 50% { opacity: 0.2 } }\n\
         .b:hover { animation: pulse 1s infinite; }",
    );
    let t = h.by_id("t");
    assert_eq!(h.running(t), 0);
    h.hover(t);
    assert_eq!(h.running(t), 1, "hover starts it");
    h.at(t, 500);
    assert!(near(h.opacity(t), 0.2), "the 50% frame: {}", h.opacity(t));
    h.pointer(glam::Vec2::new(700.0, 500.0));
    assert_eq!(h.running(t), 0, "leaving cancels it");
    assert!(near(h.opacity(t), 1.0));
    assert_eq!(h.kinds(), ["animationstart", "animationcancel"]);
}

#[test]
fn an_important_declaration_beats_an_animation() {
    let mut h = Harness::new(
        r##"<root><tile id="t" class="c" width="40" height="40" bg="#ff0000"/></root>"##,
        "@keyframes fade { from { opacity: 0; background: #0000ff } to { opacity: 0 } }\n\
         .c { opacity: 0.6 !important; animation: fade 1s; }",
    );
    let t = h.by_id("t");
    h.at(t, 300);
    assert!(near(h.opacity(t), 0.6), "important wins: {}", h.opacity(t));
    let fill = h.app.world.get::<Visuals>(t).and_then(|v| v.fill.clone());
    assert!(
        matches!(fill, Some(Fill::Solid(c)) if c.b > 0.05 && c.r < 0.95),
        "the background still animates: {fill:?}"
    );
}

#[test]
fn steps_and_delays_wake_the_loop_instead_of_running_frames() {
    let mut h = Harness::new(
        r#"<root><tile id="s" class="s" width="40" height="40"/><tile id="d" class="d" width="40" height="40"/></root>"#,
        "@keyframes blink { from { opacity: 0 } to { opacity: 1 } }\n\
         .s { animation: blink 10s steps(2) infinite; }\n\
         .d { animation: blink 1s 5s; }",
    );
    let s = h.by_id("s");
    let d = h.by_id("d");
    h.ticks(1);
    assert!(
        !h.app.world.resource::<AnimationsActive>().get(),
        "neither asks for a frame per vsync"
    );
    let wake = h
        .app
        .world
        .resource::<WakeDeadline>()
        .get()
        .expect("a timed wake");
    let start = h.app.world.get::<RunningAnimations>(s).unwrap().0[0].start;
    let ahead = wake.saturating_duration_since(start).as_millis();
    assert!(
        (4999..=5020).contains(&ahead),
        "the next step jump, {ahead}ms in"
    );
    assert!(near(h.opacity(s), 0.0));
    h.at(s, 5000);
    assert!(near(h.opacity(s), 0.5), "stepped: {}", h.opacity(s));
    assert!(near(h.opacity(d), 1.0), "the delayed one shows nothing yet");
}

#[test]
fn an_animation_off_screen_asks_for_no_frames() {
    let mut h = Harness::new(
        r#"<root><column height="600"><tile class="pad" width="10"/><tile id="t" class="o" width="40" height="40"/></column></root>"#,
        "@keyframes fade { from { opacity: 0 } to { opacity: 1 } }\n\
         .pad { height: 2000px; flex-shrink: 0; }\n\
         .o { flex-shrink: 0; animation: fade 1s infinite; }",
    );
    let t = h.by_id("t");
    h.ticks(1);
    assert_eq!(h.running(t), 1);
    assert!(
        !h.app.world.resource::<AnimationsActive>().get(),
        "below the window, it does not keep the loop awake"
    );
    assert!(
        h.app.world.resource::<WakeDeadline>().get().is_some(),
        "but it wakes for its next iteration event"
    );
}

#[test]
fn reduced_motion_follows_the_os_setting() {
    let mut h = Harness::new(
        r#"<root><tile id="t" class="f" width="40" height="40"/></root>"#,
        "@media (prefers-reduced-motion: no-preference) {\n\
           @keyframes fade { from { opacity: 0 } to { opacity: 1 } }\n\
         }\n\
         .f { animation: fade 1s linear infinite; }",
    );
    let t = h.by_id("t");
    h.at(t, 250);
    assert!(
        near(h.opacity(t), 0.25),
        "the animation runs: {}",
        h.opacity(t)
    );
    h.app
        .world
        .resource_mut::<lumen_core::components::StyleManager>()
        .set_reduce_motion(true);
    h.ticks(3);
    assert!(
        near(h.opacity(t), 1.0),
        "the block no longer applies once the OS asks for less motion"
    );
    assert!(!h.app.world.resource::<AnimationsActive>().get());
}

#[test]
fn a_transition_outranks_a_running_animation() {
    let mut h = Harness::new(
        r#"<root><tile id="t" class="g" width="40" height="40"/></root>"#,
        "@keyframes dim { from { opacity: 0.2 } to { opacity: 0.2 } }\n\
         .g { opacity: 1; transition: opacity 10s linear; animation: dim 100s infinite; }\n\
         .g.low { opacity: 0.6; }",
    );
    let t = h.by_id("t");
    h.ticks(1);
    assert!(near(h.opacity(t), 0.2), "the animation holds it");
    lumen_script::node_query::push_external_dom_command(lumen_script::ScriptCommand::SetClasses {
        target_id: "t".to_string(),
        classes: "g low".to_string(),
    });
    h.ticks(3);
    // CSS orders transitions above animations: the transition runs from
    // what showed toward the new value, over the animation.
    let shown = h.opacity(t);
    assert!(
        shown < 0.25,
        "the transition starts where the animation was: {shown}"
    );
    assert!(
        h.app
            .world
            .get::<lumen_primitives::ActiveTransitions>(t)
            .is_some()
    );
}

#[test]
fn a_frame_setting_two_properties_blends_each() {
    let mut h = Harness::new(
        r#"<root><tile id="t" class="x" width="40" height="40"/></root>"#,
        "@keyframes away { to { opacity: 0; transform: scale(0.3); } }\n\
         .x { animation: away 2s linear -1s paused both; }",
    );
    h.ticks(3);
    let t = h.by_id("t");
    assert!(near(h.opacity(t), 0.5), "halfway out: {}", h.opacity(t));
    let functions = h
        .app
        .world
        .get::<ElementTransform>(t)
        .map(|x| x.functions.clone());
    assert_eq!(
        functions,
        Some(vec![lumen_core::transform::TransformFn::Scale(0.65, 0.65)])
    );
}
