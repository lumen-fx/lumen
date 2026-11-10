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

#[test]
fn a_spring_plays_for_its_settle_time_and_overshoots() {
    let mut h = Harness::new(
        r#"<root><tile id="t" class="p" width="40" height="40"/></root>"#,
        "@keyframes pop { from { scale: 0 } to { scale: 1 } }\n\
         .p { animation: pop spring(1 100 10) forwards; }",
    );
    let t = h.by_id("t");
    let duration = h.app.world.get::<RunningAnimations>(t).unwrap().0[0]
        .spec
        .duration_ms;
    assert!(duration > 300, "the settle time: {duration}ms");
    let scale = |h: &Harness| {
        h.app
            .world
            .get::<ElementTransform>(t)
            .map_or(1.0, |x| x.scale.0)
    };
    // An underdamped spring passes its target before it settles.
    let peak = (1..20)
        .map(|k| {
            h.at(t, u64::from(duration) * k / 20);
            scale(&h)
        })
        .fold(0.0f32, f32::max);
    assert!(peak > 1.05, "it overshoots: {peak}");
    h.at(t, u64::from(duration));
    assert!(near(scale(&h), 1.0));
}

impl Harness {
    fn set_signal(&mut self, name: &str, value: &str) {
        self.app
            .world
            .resource_mut::<lumen_core::property_store::PropertyStore>()
            .set_global_str(name, value);
        self.ticks(3);
    }

    fn find(&mut self, id: &str) -> Option<Entity> {
        let mut q = self.app.world.query::<(Entity, &LumenId)>();
        q.iter(&self.app.world)
            .find(|(_, l)| l.0 == id)
            .map(|(e, _)| e)
    }
}

#[test]
fn an_entering_element_transitions_from_its_starting_style() {
    let mut h = Harness::new(
        r#"<root><if signal="open"><tile id="p" class="panel" width="40" height="40"/></if></root>"#,
        ".panel { opacity: 1; translate: 0 0; transition: opacity 1s linear, translate 1s linear; }\n\
         @starting-style { .panel { opacity: 0.2; translate: 0 30px; } }",
    );
    assert!(h.find("p").is_none());
    h.set_signal("open", "true");
    let p = h.find("p").expect("mounted");
    let active = h
        .app
        .world
        .get::<lumen_primitives::ActiveTransitions>(p)
        .expect("entering transitions");
    let opacity = active
        .on(lumen_core::style_overrides::StyleFields::OPACITY)
        .expect("an opacity transition");
    assert_eq!(
        opacity.from.opacity,
        Some(Some(0.2)),
        "from the starting style"
    );
    assert!(
        active
            .on(lumen_core::style_overrides::StyleFields::TRANSFORM)
            .is_some(),
        "the translate transitions too"
    );
    assert!(h.opacity(p) < 0.5, "shows near its start: {}", h.opacity(p));
}

#[test]
fn a_leaving_element_plays_its_exit_before_it_goes() {
    let mut h = Harness::new(
        r#"<root><if signal="open"><tile id="t" class="toast" width="40" height="40"/></if></root>"#,
        "@keyframes out { to { opacity: 0; } }\n\
         .toast.leaving { animation: out 1s linear forwards; }",
    );
    h.set_signal("open", "true");
    let t = h.find("t").expect("mounted");
    h.set_signal("open", "false");
    assert_eq!(h.find("t"), Some(t), "held in place while its exit runs");
    assert!(
        h.app
            .world
            .get::<lumen_core::components::LumenClasses>(t)
            .is_some_and(|c| c.has("leaving"))
    );
    assert_eq!(h.running(t), 1, "the exit runs");
    h.at(t, 500);
    assert!(near(h.opacity(t), 0.5), "halfway out: {}", h.opacity(t));
    h.resume_from(t, 1100);
    h.ticks(3);
    assert!(h.find("t").is_none(), "gone once the exit has run");
}

#[test]
fn a_dropped_row_leaves_after_its_exit_and_the_rest_stay_in_order() {
    let mut h = Harness::new(
        r#"<root><for each="items" key="id"><tile id="{row.id}" class="row" width="40" height="10"/></for></root>"#,
        "@keyframes out { to { opacity: 0; } }\n\
         .row.leaving { animation: out 1s linear forwards; }",
    );
    let set = |h: &mut Harness, ids: &[&str]| {
        let rows = ids
            .iter()
            .map(|id| std::collections::HashMap::from([("id".to_string(), id.to_string())]))
            .collect::<Vec<_>>();
        h.app
            .world
            .resource_mut::<lumen_core::signals::ArraySignals>()
            .set("items", rows);
        h.ticks(3);
    };
    set(&mut h, &["a", "b", "c"]);
    let b = h.find("b").expect("row b");
    set(&mut h, &["a", "c"]);
    assert_eq!(
        h.find("b"),
        Some(b),
        "the dropped row stays while it leaves"
    );
    set(&mut h, &["a", "c", "d"]);
    assert!(
        h.find("d").is_some(),
        "the list keeps reconciling around it"
    );
    h.resume_from(b, 1100);
    h.ticks(3);
    assert!(h.find("b").is_none());
    assert!(h.find("a").is_some() && h.find("c").is_some());
}

#[test]
fn a_scroll_timeline_follows_the_scroll_and_asks_for_no_frames() {
    let mut h = Harness::new(
        r#"<root><scroll id="s" class="port"><column id="content" class="content"><tile id="bar" class="bar" width="40" height="10"/><tile class="gap" width="10" height="400"/><tile id="v" class="seen" width="40" height="100"/></column></scroll></root>"#,
        "@keyframes reveal { from { opacity: 0; } to { opacity: 1; } }\n\
         .port { height: 200px; width: 300px; scrollbar-width: none; }\n\
         .content { height: 1200px; flex-shrink: 0; }\n\
         .bar { animation: reveal linear both; animation-timeline: scroll(nearest); }\n\
         .gap { flex-shrink: 0; }\n\
         .seen { flex-shrink: 0; animation: reveal linear both; animation-timeline: view(); }",
    );
    let s = h.by_id("s");
    let bar = h.by_id("bar");
    let v = h.by_id("v");
    // Let the overlay scrollbar's own fade-out run its course first.
    let deadline = std::time::Instant::now() + Duration::from_secs(5);
    while h.app.world.resource::<AnimationsActive>().get() && std::time::Instant::now() < deadline {
        std::thread::sleep(Duration::from_millis(10));
        h.ticks(1);
    }
    assert!(near(h.opacity(bar), 0.0), "at the top: {}", h.opacity(bar));
    assert!(near(h.opacity(v), 0.0), "below the view: {}", h.opacity(v));
    h.ticks(2);
    assert!(
        !h.app.world.resource::<AnimationsActive>().get(),
        "a scroll timeline asks for no frames"
    );
    // Halfway down the 1000px the content scrolls.
    h.app
        .world
        .get_mut::<lumen_core::input::ScrollOffset>(s)
        .unwrap()
        .0
        .y = 500.0;
    h.ticks(2);
    assert!(near(h.opacity(bar), 0.5), "halfway: {}", h.opacity(bar));
    // The 100px box laid out at 410px sits 90px under the top of a 200px
    // view scrolled to 500: past halfway through crossing it.
    let seen = h.opacity(v);
    assert!(seen > 0.5 && seen < 1.0, "crossing the view: {seen}");
}

#[test]
fn a_script_animation_plays_pauses_reverses_and_reports_its_end() {
    use lumen_script::{ScriptNs, ScriptValue, call_script_fn};
    use std::collections::HashMap;
    let mut h = Harness::new(
        r#"<root><tile id="box" width="40" height="40"/></root>"#,
        "",
    );
    let b = h.by_id("box");
    let ns = ScriptNs::Named("animations".to_string());
    let frame = |o: f64| {
        ScriptValue::Map(HashMap::from([
            ("opacity".to_string(), ScriptValue::Str(o.to_string())),
            (
                "transform".to_string(),
                ScriptValue::Str(format!("translateX({}px)", o * 100.0)),
            ),
        ]))
    };
    let options = ScriptValue::Map(HashMap::from([
        ("duration".to_string(), ScriptValue::F64(1000.0)),
        ("fill".to_string(), ScriptValue::Str("forwards".into())),
    ]));
    let id = match call_script_fn(
        &ns,
        "animate",
        &[
            ScriptValue::Str("box".into()),
            ScriptValue::Array(vec![frame(0.0), frame(1.0)]),
            options,
        ],
    ) {
        Ok(ScriptValue::I64(id)) => id,
        other => panic!("animate returned {other:?}"),
    };
    let control = |name: &str, extra: Option<ScriptValue>| {
        let mut args = vec![ScriptValue::I64(id)];
        args.extend(extra);
        call_script_fn(&ns, name, &args).expect(name)
    };
    h.ticks(1);
    assert_eq!(h.running(b), 1);
    control("set_current_time", Some(ScriptValue::F64(500.0)));
    control("pause", None);
    h.ticks(2);
    assert!(
        near(h.opacity(b), 0.5),
        "seeked and paused: {}",
        h.opacity(b)
    );
    let tx = h
        .app
        .world
        .get::<ElementTransform>(b)
        .map(|t| t.functions.clone());
    assert_eq!(
        tx,
        Some(vec![lumen_core::transform::TransformFn::Translate(
            lumen_core::transform::LengthPct::px(50.0),
            lumen_core::transform::LengthPct::default()
        )])
    );
    assert_eq!(
        control("play_state", None),
        ScriptValue::Str("paused".into())
    );
    match control("current_time", None) {
        ScriptValue::F64(ms) => assert!((ms - 500.0).abs() < 1.0, "{ms}"),
        other => panic!("{other:?}"),
    }
    control("finish", None);
    h.ticks(2);
    assert!(near(h.opacity(b), 1.0), "finished, filled forwards");
    assert_eq!(
        control("play_state", None),
        ScriptValue::Str("finished".into())
    );
    assert!(h.kinds().contains(&"animationend"));
    control("reverse", None);
    control("pause", None);
    control("set_current_time", Some(ScriptValue::F64(250.0)));
    h.ticks(2);
    assert!(
        near(h.opacity(b), 0.25),
        "reversed and seeked: {}",
        h.opacity(b)
    );
    assert_eq!(control("playback_rate", None), ScriptValue::F64(-1.0));
    control("cancel", None);
    h.ticks(2);
    assert!(near(h.opacity(b), 1.0), "cancelled: back to its own value");
    assert_eq!(control("play_state", None), ScriptValue::Str("idle".into()));
    assert!(h.kinds().contains(&"animationcancel"));
    assert!(
        call_script_fn(
            &ns,
            "animate",
            &[
                ScriptValue::Str("nope".into()),
                ScriptValue::Array(vec![frame(0.0)]),
                ScriptValue::Map(HashMap::new()),
            ],
        )
        .is_err(),
        "an unknown id is an error"
    );
}
