// Full-pipeline tests that name `lumenc::spawn` / `RunOptions` /
// `build_headless_app`, which lumenc only exposes under the `dev-run`
// feature. Gate the whole file so a thin (`--no-default-features`)
// `--all-targets` build compiles it out instead of failing on the missing
// symbols.
#![cfg(feature = "dev-run")]

//! Full-pipeline integration tests (parse -> spawn -> tick).
//!
//! These live in `lumenc` rather than `lumen-runtime` because they need the
//! markup/CSS front-end and the runtime as the *same* crate instance: the
//! runtime links no parser (it is injected via `SourceParser`), and only
//! `lumenc` supplies one (`default_parser()`). Building the app window-free
//! goes through the now-public `lumen_runtime::run::build_app` (re-exported as
//! `lumenc::run::build_app`); the injected parser is wired per call.

use bevy_ecs::prelude::*;
use lumen_core::app::App;
use lumen_core::components::LumenId;
use lumen_core::input::{ClickEvent, PointerButton};
use lumen_script::ScriptCommand;
use lumen_script_rhai::RhaiHost;
use lumenc::run::{ErrorBanner, build_app};
use lumenc::{RunError, RunOptions};
use std::path::PathBuf;
use std::time::{Duration, Instant};

/// Building an `App` drains the process-global external-property channel in
/// `lumen_core::property_store`, so two tests running in parallel consume
/// each other's pending writes: the bus test pushes a value, another test's
/// `App` drains it, and the value is gone by the time the host looks. Every
/// test in this binary takes this guard, which costs nothing at this suite's
/// size and makes the bus test deterministic.
fn serial() -> std::sync::MutexGuard<'static, ()> {
    static GUARD: std::sync::Mutex<()> = std::sync::Mutex::new(());
    GUARD
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner())
}

mod pipeline_integration_tests {
    //! Headless full-pipeline regression tests.
    //!
    //! These guard the exact gap that slipped past every unit test on the
    //! perf dirty-gating landing: each individual system was green in
    //! isolation, but the end-to-end wiring - script `on_start` ->
    //! `ScriptCommand` -> `PropertyStore` -> `bind-text` / `<for>` - was
    //! broken because the pull-binding readers were not ordered after the
    //! command applier, so the per-tick dirty queue was cleared before the
    //! bindings ever observed the write.
    use super::*;
    use lumen_core::components::TextContent;

    /// Build the full app from inline markup (no disk I/O beyond a scratch
    /// dir for `lumen.toml` defaults + asset resolution), install the
    /// window plugin's window-free half, and tick it `ticks` times.
    fn build_and_tick(markup: &str, ticks: u32) -> App {
        // Unique scratch dir so parallel test threads don't collide, and so
        // the default `lumen.toml` (absent) resolves to built-in defaults.
        let dir = std::env::temp_dir().join(format!(
            "lumenc_pipeline_it_{}_{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        std::fs::create_dir_all(&dir).unwrap();
        // Disable the MCP server so the test doesn't spawn a thread that
        // binds a TCP port (parallel tests would collide on 7878).
        std::fs::write(
            dir.join("lumen.toml"),
            "[mcp]\nport = 0\n\n[script]\nengine = \"rhai\"\n",
        )
        .unwrap();

        let opts = RunOptions::new(&dir)
            .with_parser(lumenc::default_parser())
            .with_markup(markup.to_string());
        let (mut app, _winit) = build_app(opts).expect("build_app");
        // Same window-free plugin the headless entry point installs.
        app.add_plugin(lumen_window_winit::WinitPlugin);
        for _ in 0..ticks {
            app.tick();
        }
        let _ = std::fs::remove_dir_all(&dir);
        app
    }

    /// Collect every `TextContent` string currently in the world.
    fn all_texts(app: &mut App) -> Vec<String> {
        let mut q = app.world.query::<&TextContent>();
        q.iter(&app.world).map(|t| t.0.clone()).collect()
    }

    /// W2 text-editing core, end-to-end through the real `build_app`
    /// wiring: `<input>` gets a TextBuffer, a press focuses it and
    /// places the caret, typing goes through the buffer model, and the
    /// caret-keep-visible system produces a horizontal scroll offset
    /// once the value outgrows the field.
    #[test]
    fn input_press_type_and_caret_scroll_end_to_end() {
        let _serial = crate::serial();
        use lumen_core::components::{TextInput, TextInputScroll, Transform};
        use lumen_core::input::{
            Key, KeyPressed, Modifiers, PointerButton, PointerMoved, PointerPressed, PointerState,
        };
        // `bg` gives the input a `Visuals` so it's a hit-test candidate -
        // same as the default skin's `input { bg: ... }` rule provides.
        let mut app = build_and_tick(
            r##"<root><input bg="#223344" placeholder="hint" /></root>"##,
            2,
        );

        let (input_e, t) = {
            let mut q = app
                .world
                .query_filtered::<(Entity, &Transform), With<TextInput>>();
            let (e, t) = q.single(&app.world).expect("one input");
            (e, *t)
        };
        assert!(
            app.world
                .get::<lumen_core::text_model::TextBuffer>(input_e)
                .is_some(),
            "TextEditPlugin attaches the buffer through the lumenc runtime"
        );
        assert!(t.size.x > 0.0, "layout ran");

        // Press inside the field.
        let p = t.absolute + t.size * 0.5;
        app.world.resource_mut::<PointerState>().position = Some(p);
        app.world
            .resource_mut::<bevy_ecs::message::Messages<PointerMoved>>()
            .write(PointerMoved { position: p });
        app.world.resource_mut::<PointerState>().primary_down = true;
        app.world
            .resource_mut::<bevy_ecs::message::Messages<PointerPressed>>()
            .write(PointerPressed {
                position: p,
                button: PointerButton::Primary,
            });
        app.tick();
        assert_eq!(
            app.world.resource::<lumen_core::input::FocusTracker>().0,
            Some(input_e),
            "press focuses the input"
        );

        // Type well past the field width; the caret-keep-visible system
        // must produce a positive x offset.
        for _ in 0..80 {
            app.world
                .resource_mut::<bevy_ecs::message::Messages<KeyPressed>>()
                .write(KeyPressed {
                    key: Key::Character("m".into()),
                    modifiers: Modifiers::default(),
                    repeat: false,
                });
        }
        app.tick();
        let ti = app.world.get::<TextInput>(input_e).unwrap();
        assert_eq!(ti.cursor, 80, "caret trails the typed text");
        let scroll = app
            .world
            .get::<TextInputScroll>(input_e)
            .expect("long value gets a caret-keep-visible offset");
        assert!(
            scroll.offset.x > 0.0,
            "caret scrolled into view (offset {:?})",
            scroll.offset
        );
    }

    const MARKUP: &str = r#"
<root>
  <label id="direct" bind-text="who" />
  <label id="derived" bind-text="greeting" />
  <for each="todos" key="id">
    <row>
      <label text="{label}" />
    </row>
  </for>
  <script>
    fn on_start() {
        let who = signal("who", "");
        signal("who", "").set("world");
        derive("greeting", [who], |s| "hi, " + s);
        let todos = signal_array("todos");
        todos.set([
            #{ id: "1", label: "alpha" },
            #{ id: "2", label: "beta" },
            #{ id: "3", label: "gamma" },
        ]);
    }
  </script>
</root>
"#;

    #[test]
    fn bind_text_and_for_rows_populate_from_on_start() {
        let _serial = crate::serial();
        // A handful of ticks lets the on_start command backlog drain,
        // derivations compute, and the reconciler spawn rows.
        let mut app = build_and_tick(MARKUP, 6);
        let texts = all_texts(&mut app);

        // (a) direct signal set -> bound label.
        assert!(
            texts.iter().any(|t| t == "world"),
            "direct bind-text label empty; TextContents = {texts:?}"
        );
        // (a') derive()-driven bound label.
        assert!(
            texts.iter().any(|t| t == "hi, world"),
            "derived bind-text label empty; TextContents = {texts:?}"
        );
        // (b) <for> rows spawned (one label per row).
        for want in ["alpha", "beta", "gamma"] {
            assert!(
                texts.iter().any(|t| t == want),
                "<for> row '{want}' missing; TextContents = {texts:?}"
            );
        }
    }

    // Bindings and a nested `<for>` mounted inside a `<tab>` panel - which
    // the parser compiles to an `<if eq="...">` gate whose body is spawned by
    // the reconciler's `spawn_body_child`, not the top-level `spawn_element`.
    // This is the widget-garden failure mode: every bound label lived inside
    // a tab, so `spawn_body_child` (missing the `bind` + nested-marker
    // handling) produced labels with no `BindText` and a `<for>` with no
    // `ForMarker` - permanently blank labels and raw `{placeholder}` rows.
    const TAB_MARKUP: &str = r#"
<root>
  <tabs bind-value="tab">
    <tab name="main" label="Main">
      <label bind-text="greeting" />
      <for each="todos" key="id">
        <row>
          <label text="{label}" />
        </row>
      </for>
    </tab>
    <tab name="other" label="Other">
      <label text="second-tab" />
    </tab>
  </tabs>
  <script>
    fn on_start() {
        let who = signal("who", "");
        signal("who", "").set("world");
        derive("greeting", [who], |s| "hi, " + s);
        let todos = signal_array("todos");
        todos.set([
            #{ id: "1", label: "alpha" },
            #{ id: "2", label: "beta" },
        ]);
    }
  </script>
</root>
"#;

    #[test]
    fn bind_text_and_for_inside_tab_panel_populate() {
        let _serial = crate::serial();
        let mut app = build_and_tick(TAB_MARKUP, 8);
        let texts = all_texts(&mut app);
        // bind-text inside the (if-gated) tab panel.
        assert!(
            texts.iter().any(|t| t == "hi, world"),
            "bind-text inside tab panel empty; TextContents = {texts:?}"
        );
        // Nested <for> inside the tab panel reconciles its rows.
        for want in ["alpha", "beta"] {
            assert!(
                texts.iter().any(|t| t == want),
                "nested <for> row '{want}' missing (raw placeholder?); TextContents = {texts:?}"
            );
        }
        // The literal `{label}` template must not survive unreconciled.
        assert!(
            !texts.iter().any(|t| t.contains("{label}")),
            "unreconciled raw placeholder leaked; TextContents = {texts:?}"
        );
    }

    #[test]
    fn signal_write_after_startup_updates_text_within_one_tick() {
        let _serial = crate::serial();
        // Settle the initial pipeline first.
        let mut app = build_and_tick(MARKUP, 6);
        assert!(all_texts(&mut app).iter().any(|t| t == "world"));

        // Post-startup external signal write (the FFI / background-thread
        // path): lands on the property bus, drained next tick's CommandDrain,
        // observed by apply_text_bindings the same tick.
        lumen_core::property_store::push_external_property(
            lumen_core::property_store::PropertyKey::Global(std::sync::Arc::from("who")),
            lumen_core::property_store::PropertyValue::Str(std::sync::Arc::from("mars")),
        );
        // One tick to drain + apply.
        app.tick();
        let texts = all_texts(&mut app);
        assert!(
            texts.iter().any(|t| t == "mars"),
            "post-startup signal write did not reach TextContent within one tick; \
             TextContents = {texts:?}"
        );
        // RC1: the derivation must recompute from the post-startup dep
        // write too - the dep's dirty flag is only up for one tick, so
        // `apply_derivations` has to observe it before `A11ySync` clears
        // the queue.
        assert!(
            texts.iter().any(|t| t == "hi, mars"),
            "derive() did not recompute after a post-startup dep write; \
             TextContents = {texts:?}"
        );
    }

    // --- RC1: derive() must recompute after startup (click path) ---------
    //
    // The historical bug: `apply_script_commands` was ordered
    // `.after(apply_derivations::<H>)`, so a click handler's `SetSignal` dep
    // write always landed after the derivation pass had run, and
    // `clear_property_store_dirty` wiped the dirty flag the same tick -
    // derived signals froze at their startup value forever (counter stuck
    // at "clicks: 0" through any number of confirmed ClickEvents).
    // Two identical bump buttons: the second assertion clicks `bump2`
    // because two rapid clicks on the SAME entity fall inside the
    // double-click window and route to `on_double_click` (by design),
    // which would test press semantics rather than derivation recompute.
    const CLICK_MARKUP: &str = r#"
<root>
  <button id="bump" text="+1" />
  <button id="bump2" text="+1 too" />
  <label id="derived" bind-text="click_label" />
  <label id="derived2" bind-text="click_label2" />
  <script>
    fn on_start() {
        signal("count", 0);
        derive("click_label", ["count"], |n| "clicks: " + n);
        derive("click_label2", ["click_label"], |s| s + "!");
    }
    fn on_click(id) {
        if id == "bump" || id == "bump2" {
            let c = signal("count", 0);
            c.set(c.get() + 1);
        }
    }
  </script>
</root>
"#;

    /// Simulate a click on the entity carrying LumenId `id`.
    fn click_on(app: &mut App, id: &str) {
        let target = {
            let mut q = app.world.query::<(Entity, &LumenId)>();
            q.iter(&app.world)
                .find(|(_, lid)| lid.0 == id)
                .map(|(e, _)| e)
                .unwrap_or_else(|| panic!("no entity with LumenId {id:?}"))
        };
        app.world.write_message(ClickEvent {
            entity: target,
            position: glam::Vec2::ZERO,
            button: PointerButton::Primary,
        });
    }

    #[test]
    fn click_recomputes_derived_signal_and_chained_derivation() {
        let _serial = crate::serial();
        let mut app = build_and_tick(CLICK_MARKUP, 6);
        // Initial derivation pass (pending_initial path) + in-tick cascade
        // for the derived-of-derived.
        let texts = all_texts(&mut app);
        assert!(
            texts.iter().any(|t| t == "clicks: 0"),
            "initial derived value missing; TextContents = {texts:?}"
        );
        assert!(
            texts.iter().any(|t| t == "clicks: 0!"),
            "initial derived-of-derived value missing; TextContents = {texts:?}"
        );

        // Click. One tick: dispatch -> on_click -> SetSignal count=1 applied
        // -> derivations recompute (incl. chained) -> bindings pull.
        click_on(&mut app, "bump");
        app.tick();
        let texts = all_texts(&mut app);
        assert!(
            texts.iter().any(|t| t == "clicks: 1"),
            "derive() frozen after click (RC1 regression); TextContents = {texts:?}"
        );
        assert!(
            texts.iter().any(|t| t == "clicks: 1!"),
            "derived-of-derived frozen after click; TextContents = {texts:?}"
        );

        // Second click keeps working (dirty window re-opens every write).
        // Different button: a rapid same-entity second click would be
        // folded into a double-click (see CLICK_MARKUP comment).
        click_on(&mut app, "bump2");
        app.tick();
        let texts = all_texts(&mut app);
        assert!(
            texts.iter().any(|t| t == "clicks: 2"),
            "derive() stopped recomputing on the second click; TextContents = {texts:?}"
        );
        assert!(
            texts.iter().any(|t| t == "clicks: 2!"),
            "chained derivation stopped on the second click; TextContents = {texts:?}"
        );
    }

    // --- RC7: signal(name, default) publishes its default ----------------
    #[test]
    fn declared_but_never_set_signal_renders_its_default() {
        let _serial = crate::serial();
        let markup = r#"
<root>
  <label id="vol" bind-text="volume" />
  <label id="weight" bind-text="weight" />
  <script>
    fn on_start() {
        signal("volume", 42);
        signal("weight", "medium");
    }
  </script>
</root>
"#;
        let mut app = build_and_tick(markup, 4);
        let texts = all_texts(&mut app);
        assert!(
            texts.iter().any(|t| t == "42"),
            "bind-text of a declared-but-never-set int signal is blank; \
             TextContents = {texts:?}"
        );
        assert!(
            texts.iter().any(|t| t == "medium"),
            "bind-text of a declared-but-never-set string signal is blank; \
             TextContents = {texts:?}"
        );
    }

    #[test]
    fn signal_default_does_not_clobber_preexisting_external_value() {
        let _serial = crate::serial();
        // The SDK / FFI path: a value pushed onto the external property
        // bus BEFORE the script declares the signal must win over the
        // declaration default. Host-level (no App) so the assertion
        // doesn't depend on which parallel test's tick drains the
        // process-global bus.
        use lumen_script::{ScriptContext, ScriptValue};
        lumen_core::property_store::init_external_properties();
        lumen_core::property_store::push_external_property(
            lumen_core::property_store::PropertyKey::Global(std::sync::Arc::from(
                "prewritten_volume_rc7",
            )),
            lumen_core::property_store::PropertyValue::I64(77),
        );
        let mut host = RhaiHost::new();
        host.load(r#"fn on_start() { signal("prewritten_volume_rc7", 42); }"#)
            .expect("load");
        let cmds = host.call_event_no_args("on_start").expect("on_start");
        assert!(
            !cmds.iter().any(|c| matches!(
                c,
                ScriptCommand::SetSignal { name, .. } if name == "prewritten_volume_rc7"
            )),
            "signal() published its default over a pre-existing external write; cmds = {cmds:?}"
        );
        // The host mirror is seeded from the pre-existing value, not the
        // declaration default.
        assert_eq!(
            host.root_context().get("prewritten_volume_rc7"),
            Some(ScriptValue::I64(77)),
            "host mirror not seeded from the pre-existing external value"
        );
    }

    // --- RC6: `lumenc check` compiles the script; run + check agree ------

    /// A script whose single expression nests `depth` parenthesised adds.
    fn nested_expr_script_markup(depth: usize) -> String {
        format!(
            "<root>\n  <label text=\"hi\" />\n  <script>\n    fn on_start() {{ let x = {}1{}; print(x); }}\n  </script>\n</root>\n",
            "(1+".repeat(depth),
            ")".repeat(depth),
        )
    }

    /// Write a minimal on-disk app (markup + mcp-off lumen.toml) and
    /// return its dir.
    fn write_app_dir(markup: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!(
            "lumenc_check_it_{}_{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(
            dir.join("lumen.toml"),
            "[mcp]\nport = 0\n\n[script]\nengine = \"rhai\"\n",
        )
        .unwrap();
        std::fs::write(dir.join("main.lmn"), markup).unwrap();
        dir
    }

    #[test]
    fn check_rejects_script_that_would_die_at_load_and_run_agrees() {
        let _serial = crate::serial();
        // 600 nested exprs > the deliberate 512 cap: load would fail.
        let dir = write_app_dir(&nested_expr_script_markup(600));
        let check = lumenc::check_app(&dir);
        assert!(
            matches!(check, Err(RunError::Script(_))),
            "check false-passed a script that dies at load: {check:?}"
        );
        // Run agreement: build_app still opens the app (window renders),
        // but records the failure prominently.
        let (app, _winit) = build_app(RunOptions::new(&dir).with_parser(lumenc::default_parser()))
            .expect("build_app");
        assert!(
            app.world
                .get_resource::<lumen_script_rhai::ScriptLoadFailure>()
                .is_some(),
            "run did not record the script load failure"
        );
        assert!(
            app.world.resource::<ErrorBanner>().0.is_some(),
            "script load failure not surfaced in the in-app error banner"
        );
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn check_accepts_script_deeper_than_rhai_default_limits() {
        let _serial = crate::serial();
        // 100 nested exprs: over Rhai's old default cap (64) that used to
        // kill real apps, under our deliberate 512. Both check and run
        // must accept it.
        let dir = write_app_dir(&nested_expr_script_markup(100));
        lumenc::check_app(&dir).expect("check should accept a 100-deep expression");
        let (app, _winit) = build_app(RunOptions::new(&dir).with_parser(lumenc::default_parser()))
            .expect("build_app");
        assert!(
            app.world
                .get_resource::<lumen_script_rhai::ScriptLoadFailure>()
                .is_none(),
            "run rejected a script check accepted (limits diverged)"
        );
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// Widget-driven store writes (toggle flip here) must be observed by
    /// the derivation pass AND the pull-binding readers on the same tick:
    /// the dirty queue is cleared at end of tick, so a reader or the
    /// derivation scheduled before the push misses the write forever and
    /// the bound label freezes at its spawn value. Live repro: the widget
    /// garden's `toggle_status` label never left "dark mode".
    #[test]
    fn toggle_flip_recomputes_derived_signal_into_bound_label() {
        let _serial = crate::serial();
        use lumen_core::components::{Toggleable, Transform};
        use lumen_core::input::{PointerButton, PointerMoved, PointerPressed, PointerState};
        let markup = r##"<root>
  <toggle id="dark-toggle" bind-checked="dark" bg="#334455" />
  <label id="status" bind-text="toggle_status" />
  <script>
    fn on_start() {
        let dark = signal("dark", true);
        derive("toggle_status", [dark], |v| if v == "true" || v == true { "dark mode" } else { "light mode" });
    }
  </script>
</root>"##;
        let mut app = build_and_tick(markup, 3);
        assert!(
            all_texts(&mut app).iter().any(|t| t == "dark mode"),
            "initial derived label missing: {:?}",
            all_texts(&mut app)
        );

        let t = {
            let mut q = app.world.query_filtered::<&Transform, With<Toggleable>>();
            *q.single(&app.world).expect("one toggle")
        };
        let p = t.absolute + t.size * 0.5;
        app.world.resource_mut::<PointerState>().position = Some(p);
        app.world
            .resource_mut::<bevy_ecs::message::Messages<PointerMoved>>()
            .write(PointerMoved { position: p });
        app.world.resource_mut::<PointerState>().primary_down = true;
        app.world
            .resource_mut::<bevy_ecs::message::Messages<PointerPressed>>()
            .write(PointerPressed {
                position: p,
                button: PointerButton::Primary,
            });
        app.tick();
        app.world.resource_mut::<PointerState>().primary_down = false;
        app.world
            .resource_mut::<bevy_ecs::message::Messages<lumen_core::input::PointerReleased>>()
            .write(lumen_core::input::PointerReleased {
                position: p,
                button: PointerButton::Primary,
            });
        app.tick();
        // One extra settle tick: the flip tick writes the store; readers
        // must already have seen it, but an extra tick must not undo it.
        app.tick();

        let flipped = {
            let mut q = app.world.query::<&Toggleable>();
            !q.single(&app.world).expect("one toggle").checked
        };
        assert!(flipped, "click did not flip the toggle");
        assert!(
            all_texts(&mut app).iter().any(|t| t == "light mode"),
            "derived label frozen after toggle flip: {:?}",
            all_texts(&mut app)
        );
    }
}

#[cfg(test)]
mod feel_wave_tests {
    //! Feel-wave integration: dialog fade-in via `transition: opacity`,
    //! and CSS `scrollbar-color` / `scrollbar-width` landing on the
    //! runtime `ScrollbarStyle`.
    use super::*;
    use lumen_core::components::Opacity;

    fn build_with_css(markup: &str, css: &str) -> App {
        let dir = std::env::temp_dir().join(format!(
            "lumenc_feel_{}_{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(
            dir.join("lumen.toml"),
            "[mcp]\nport = 0\n\n[script]\nengine = \"rhai\"\n",
        )
        .unwrap();
        let opts = RunOptions::new(&dir)
            .with_parser(lumenc::default_parser())
            .with_markup(markup.to_string())
            .with_css(css.to_string());
        let (mut app, _winit) = build_app(opts).expect("build_app");
        app.add_plugin(lumen_window_winit::WinitPlugin);
        for _ in 0..4 {
            app.tick();
        }
        let _ = std::fs::remove_dir_all(&dir);
        app
    }

    /// Opening a `<dialog>` that declares `transition: opacity` fades in
    /// (mount-direction transition): opacity starts near 0 and settles
    /// at 1 once the tween completes.
    #[test]
    fn dialog_open_fades_in() {
        let _serial = crate::serial();
        let markup = r#"
<root>
  <dialog open="open">
    <label text="hello" />
  </dialog>
</root>
"#;
        let css = "dialog { bg: #00000099; transition: opacity 60ms linear; }\n";
        let mut app = build_with_css(markup, css);

        // Find the dialog entity.
        let dialog = {
            let mut q = app
                .world
                .query_filtered::<Entity, With<lumenc::spawn::DialogMarker>>();
            q.single(&app.world).expect("dialog entity")
        };
        assert!(
            app.world
                .get::<lumen_primitives::TransitionSpecs>(dialog)
                .is_some(),
            "CSS transition declaration must land as TransitionSpecs"
        );

        // Open it.
        app.world
            .resource_mut::<lumen_core::property_store::PropertyStore>()
            .set_global_str("open", "true");
        app.tick(); // reconcile: show + start fade
        app.tick(); // first sampled frame
        let early = app
            .world
            .get::<Opacity>(dialog)
            .map(|o| o.0)
            .expect("fade inserts Opacity");
        assert!(
            early < 1.0,
            "entering dialog starts transparent and fades (opacity {early})"
        );
        assert!(
            app.world
                .get::<lumen_primitives::OpacityTransition>(dialog)
                .is_some(),
            "opacity tween active right after open"
        );
        // Let the 60 ms tween finish.
        std::thread::sleep(std::time::Duration::from_millis(90));
        app.tick();
        app.tick();
        let done = app.world.get::<Opacity>(dialog).map(|o| o.0).unwrap();
        assert!(
            (done - 1.0).abs() < 1e-4,
            "fade settles at full opacity (got {done})"
        );
        assert!(
            app.world
                .get::<lumen_primitives::OpacityTransition>(dialog)
                .is_none(),
            "tween retires once done"
        );
    }

    /// CSS `scrollbar-color` + `scrollbar-width` reach the runtime
    /// `ScrollbarStyle` component on the scroll container.
    #[test]
    fn scrollbar_css_lands_on_component() {
        let _serial = crate::serial();
        let markup = r#"
<root>
  <scroll height="200" width="300">
    <column height="900" />
  </scroll>
</root>
"#;
        let css = "scroll { scrollbar-color: #ff0000 #00ff0080; scrollbar-width: thin; }\n";
        let mut app = build_with_css(markup, css);
        let style = {
            let mut q = app
                .world
                .query_filtered::<&lumen_core::input::ScrollbarStyle, With<lumen_core::input::Scroll>>();
            *q.single(&app.world).expect("scrollbar style on scroller")
        };
        assert!((style.thumb.r - 1.0).abs() < 0.01);
        assert!(style.track.is_some(), "second color = explicit track");
        assert_eq!(style.width, lumen_core::input::ScrollbarWidthMode::Thin);
        // Overflowing content + settled layout => the fade state is
        // attached and the extract produced a bar draw list.
        let has_state = {
            let mut q = app
                .world
                .query_filtered::<(), With<lumen_core::input::ScrollbarState>>();
            q.iter(&app.world).count() > 0
        };
        assert!(has_state, "ScrollbarState auto-attaches to scrollers");
        let bars = {
            let mut q = app
                .render_world
                .query::<&lumen_core::render_world::ExtractedScrollbar>();
            q.iter(&app.render_world).count()
        };
        assert_eq!(bars, 1, "overflowing scroller extracts one bar list");
    }
}

#[cfg(test)]
mod virtualization_tests {
    //! Virtualized `<for>` performance + windowed-reuse regression tests.
    //!
    //! The lag report ("virtualized list is a bit laggy") traced to the
    //! reconciler despawning and respawning the ENTIRE visible window -
    //! with a full per-row CSS cascade - on every 1-row window shift while
    //! wheel-scrolling. These tests pin the fixed behavior: rows whose
    //! `(index, key)` is unchanged across a window shift keep their
    //! entities, and the ignored profile test below measures ms/frame on a
    //! 5 000-row grid under continuous wheel scroll.
    use super::*;
    use bevy_ecs::message::Messages;
    use lumen_core::input::MouseWheel;
    use lumen_core::signals::{ArrayItem, ArraySignals};

    /// Build a virtualized 5k-row app headlessly. The array is seeded
    /// directly into `ArraySignals` (not via Rhai) so setup cost stays out
    /// of the measurements.
    fn build_virtual_grid(rows: usize) -> App {
        let dir = std::env::temp_dir().join(format!(
            "lumenc_virt_{}_{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(
            dir.join("lumen.toml"),
            "[mcp]\nport = 0\n\n[script]\nengine = \"rhai\"\n",
        )
        .unwrap();
        let markup = r#"
<root>
  <scroll height="600" width="800">
    <for each="rows" key="id" virtualized="true" row-height="32">
      <row height="32" class="grid-row">
        <label text="{name}" class="cell" />
        <label text="{value}" class="cell" />
      </row>
    </for>
  </scroll>
</root>
"#;
        let css = r#"
.grid-row { bg: #10233a; padding: 4 8; }
.grid-row:hover { bg: #1a3252; }
.cell { text-color: #dde6f0; font-size: 13; }
"#;
        let opts = RunOptions::new(&dir)
            .with_parser(lumenc::default_parser())
            .with_markup(markup.to_string())
            .with_css(css.to_string());
        let (mut app, _winit) = build_app(opts).expect("build_app");
        app.add_plugin(lumen_window_winit::WinitPlugin);
        let items: Vec<ArrayItem> = (0..rows)
            .map(|i| {
                let mut m = ArrayItem::new();
                m.insert("id".into(), format!("row-{i}"));
                m.insert("name".into(), format!("Item {i}"));
                m.insert("value".into(), format!("{}", i * 7 % 100));
                m
            })
            .collect();
        app.world.resource_mut::<ArraySignals>().set("rows", items);
        // Settle: spawn window, run layout, publish transforms.
        for _ in 0..6 {
            app.tick();
        }
        let _ = std::fs::remove_dir_all(&dir);
        app
    }

    fn wheel(app: &mut App, dy: f32) {
        app.world
            .resource_mut::<Messages<MouseWheel>>()
            .write(MouseWheel {
                delta: glam::Vec2::new(0.0, dy),
                position: glam::Vec2::new(400.0, 300.0),
            });
    }

    /// Entities of the for-block's children plus their descendants,
    /// with the row key each was spawned for (read from the label text).
    fn row_entities(app: &mut App) -> Vec<(Entity, String)> {
        use bevy_ecs::hierarchy::Children;
        let mut q = app
            .world
            .query::<(Entity, &lumenc::spawn::ForMarker, Option<&Children>)>();
        let Some((_, _, Some(children))) = q.iter(&app.world).next() else {
            return Vec::new();
        };
        let kids: Vec<Entity> = children.iter().collect();
        let mut out = Vec::new();
        for kid in kids {
            // First descendant label text (BFS = document order)
            // identifies the row.
            let mut label = String::new();
            let mut queue = std::collections::VecDeque::from([kid]);
            while let Some(e) = queue.pop_front() {
                if let Some(t) = app.world.get::<lumen_core::components::TextContent>(e)
                    && !t.0.is_empty()
                {
                    label = t.0.clone();
                    break;
                }
                if let Some(c) = app.world.get::<bevy_ecs::hierarchy::Children>(e) {
                    queue.extend(c.iter());
                }
            }
            out.push((kid, label));
        }
        out
    }

    /// Spec section 15.3 (windowed reuse): scrolling the window by a few rows
    /// must not respawn rows that stay inside the window - their entities
    /// survive the shift.
    #[test]
    fn window_shift_reuses_overlapping_row_entities() {
        let _serial = crate::serial();
        let mut app = build_virtual_grid(500);
        let before = row_entities(&mut app);
        assert!(
            before.len() > 10,
            "expected a populated window, got {} rows",
            before.len()
        );

        // Scroll down ~3 rows (96 px at row-height 32).
        wheel(&mut app, -96.0);
        for _ in 0..3 {
            app.tick();
        }
        let after = row_entities(&mut app);
        assert!(!after.is_empty());

        let before_map: std::collections::HashMap<&str, Entity> =
            before.iter().map(|(e, k)| (k.as_str(), *e)).collect();
        let mut overlapping = 0;
        let mut reused = 0;
        for (e, key) in &after {
            if let Some(prev) = before_map.get(key.as_str()) {
                overlapping += 1;
                if prev == e {
                    reused += 1;
                }
            }
        }
        assert!(
            overlapping > 5,
            "test setup: windows must overlap (got {overlapping})"
        );
        assert_eq!(
            reused, overlapping,
            "rows still inside the window must keep their entities \
             ({reused}/{overlapping} reused)"
        );
    }

    /// Key change at a fixed index must respawn that row (keyed reconcile
    /// semantics survive the reuse optimisation).
    #[test]
    fn key_change_inside_window_respawns_that_row() {
        let _serial = crate::serial();
        let mut app = build_virtual_grid(100);
        let before = row_entities(&mut app);
        assert!(!before.is_empty());

        // Mutate row 2's key + payload.
        {
            let mut arrays = app.world.resource_mut::<ArraySignals>();
            let mut items = arrays.get("rows").unwrap().to_vec();
            items[2].insert("id".into(), "row-2-replaced".into());
            items[2].insert("name".into(), "Replaced".into());
            arrays.set("rows", items);
        }
        for _ in 0..3 {
            app.tick();
        }
        let after = row_entities(&mut app);
        assert!(
            after.iter().any(|(_, k)| k == "Replaced"),
            "replaced row must appear; rows = {:?}",
            after.iter().map(|(_, k)| k.clone()).collect::<Vec<_>>()
        );
        // Unchanged sibling rows keep their entities.
        let before_map: std::collections::HashMap<&str, Entity> =
            before.iter().map(|(e, k)| (k.as_str(), *e)).collect();
        let kept = after
            .iter()
            .filter(|(e, k)| before_map.get(k.as_str()) == Some(e))
            .count();
        assert!(
            kept >= after.len() - 2,
            "only the replaced row may respawn (kept {kept}/{})",
            after.len()
        );
    }

    /// Manual profile harness: `cargo test -p lumenc --release \
    /// virt_scroll_profile -- --ignored --nocapture`. Prints avg / p95 /
    /// max tick time over 120 continuously-scrolling frames on 5k rows.
    /// Small-array control: distinguishes costs scaling with total row
    /// count from costs scaling with the mounted window.
    #[test]
    #[ignore = "manual profiling harness"]
    fn virt_scroll_profile_small() {
        let _serial = crate::serial();
        let mut app = build_virtual_grid(200);
        const FRAMES: usize = 60;
        let mut times = Vec::with_capacity(FRAMES);
        for _ in 0..FRAMES {
            wheel(&mut app, -30.0);
            let t0 = Instant::now();
            app.tick();
            times.push(t0.elapsed());
        }
        let avg: Duration = times.iter().sum::<Duration>() / FRAMES as u32;
        println!(
            "virt-scroll 200 rows: scroll avg {:.3} ms",
            avg.as_secs_f64() * 1e3
        );
    }

    /// Slow scroll (window shifts only every ~8 frames): separates the
    /// cost of a scroll-dirty frame from the cost of a row-mount frame.
    #[test]
    #[ignore = "manual profiling harness"]
    fn virt_scroll_profile_slow() {
        let _serial = crate::serial();
        let mut app = build_virtual_grid(5000);
        const FRAMES: usize = 64;
        let mut times = Vec::with_capacity(FRAMES);
        for _ in 0..FRAMES {
            wheel(&mut app, -4.0);
            let t0 = Instant::now();
            app.tick();
            times.push(t0.elapsed());
        }
        let line: Vec<String> = times
            .iter()
            .map(|d| format!("{:.1}", d.as_secs_f64() * 1e3))
            .collect();
        println!("virt-scroll slow per-frame ms: {}", line.join(" "));
    }

    #[test]
    #[ignore = "manual profiling harness"]
    fn virt_scroll_profile() {
        let _serial = crate::serial();
        let mut app = build_virtual_grid(5000);
        const FRAMES: usize = 120;

        // Idle baseline: ticks with no input after settling.
        let mut idle = Vec::with_capacity(30);
        for _ in 0..30 {
            let t0 = Instant::now();
            app.tick();
            idle.push(t0.elapsed());
        }
        let idle_avg: Duration = idle.iter().sum::<Duration>() / idle.len() as u32;

        let entities = app.world.entities().len();
        let mut times = Vec::with_capacity(FRAMES);
        for _ in 0..FRAMES {
            wheel(&mut app, -30.0); // ~1 row/frame at row-height 32
            let t0 = Instant::now();
            app.tick();
            times.push(t0.elapsed());
        }
        let mut sorted = times.clone();
        sorted.sort();
        let avg: Duration = times.iter().sum::<Duration>() / FRAMES as u32;
        println!(
            "virt-scroll 5k rows ({entities} entities): idle avg {:.3} ms | scroll avg {:.3} ms, p50 {:.3} ms, p95 {:.3} ms, max {:.3} ms",
            idle_avg.as_secs_f64() * 1e3,
            avg.as_secs_f64() * 1e3,
            sorted[FRAMES / 2].as_secs_f64() * 1e3,
            sorted[FRAMES * 95 / 100].as_secs_f64() * 1e3,
            sorted[FRAMES - 1].as_secs_f64() * 1e3,
        );

        // Stage breakdown: replicate App::tick piecewise (fields are pub).
        let mut main_t = Duration::ZERO;
        let mut extract_t = Duration::ZERO;
        let mut render_t = Duration::ZERO;
        const BFRAMES: usize = 60;
        for _ in 0..BFRAMES {
            wheel(&mut app, -30.0);
            if let Some(mut tick) = app.world.get_resource_mut::<lumen_core::tick::Tick>() {
                tick.advance();
            }
            let t0 = Instant::now();
            app.world.run_schedule(lumen_core::app::Tick);
            main_t += t0.elapsed();
            let dirty = app
                .world
                .get_resource::<lumen_core::render_world::FrameDirty>()
                .map(|f| f.dirty)
                .unwrap_or(true);
            if !dirty {
                continue;
            }
            let t1 = Instant::now();
            lumen_core::render_world::clear_extracted(&mut app.render_world);
            let fns = app.extract_fns.clone();
            for f in fns {
                f(&mut app.world, &mut app.render_world);
            }
            extract_t += t1.elapsed();
            let t2 = Instant::now();
            app.render_world
                .run_schedule(lumen_core::render_world::ExtractSchedule);
            app.render_world
                .run_schedule(lumen_core::render_world::Render);
            render_t += t2.elapsed();
        }
        println!(
            "breakdown over {BFRAMES} scroll frames: main {:.3} ms/f, extract {:.3} ms/f, render {:.3} ms/f",
            main_t.as_secs_f64() * 1e3 / BFRAMES as f64,
            extract_t.as_secs_f64() * 1e3 / BFRAMES as f64,
            render_t.as_secs_f64() * 1e3 / BFRAMES as f64,
        );
    }
}

/// AOT round-trip: parse -> compile -> serialize -> deserialize -> spawn, and
/// assert the artifact path spawns an identical world to the parse-directly
/// path. Guards the whole `lumenc build` -> parser-free `run --artifact` chain.
#[cfg(test)]
mod aot_roundtrip_tests {
    use super::*;
    use lumen_core::components::{Fill, TextContent, Visuals};

    /// Small but non-trivial app: CSS cascade (class -> bg + radius), inline
    /// text, and a skinless root. Exercises the combined-stylesheet path.
    const MARKUP: &str = r#"
<root>
  <column class="panel">
    <label class="title" text="Hello AOT" />
    <tile class="card" />
    <label text="second" />
  </column>
</root>
"#;
    const CSS: &str = r#"
.panel { padding: 8px; gap: 4px; }
.title { text-color: #ff8800; font-size: 20px; }
.card  { bg: #223344; radius: 6px; width: 120px; height: 40px; }
"#;

    fn write_app(markup: &str, css: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!(
            "lumenc_aot_rt_{}_{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        std::fs::create_dir_all(&dir).unwrap();
        // MCP off so parallel tests don't collide on a TCP port.
        std::fs::write(
            dir.join("lumen.toml"),
            "[mcp]\nport = 0\n\n[script]\nengine = \"rhai\"\n",
        )
        .unwrap();
        std::fs::write(dir.join("main.lmn"), markup).unwrap();
        std::fs::write(dir.join("main.css"), css).unwrap();
        dir
    }

    /// Entity count, sorted text, and the sorted solid fills as
    /// `(r, g, b, a, radiusx100)` tuples.
    type SpawnSnapshot = (usize, Vec<String>, Vec<(u32, u32, u32, u32, u32)>);

    /// Observable render inputs after spawn: entity count, sorted text, and
    /// the sorted solid fills + radii the cascade produced. Two worlds with
    /// equal snapshots paint identically.
    fn snapshot(app: &mut App) -> SpawnSnapshot {
        let count = app.world.query::<Entity>().iter(&app.world).count();
        let mut texts: Vec<String> = {
            let mut q = app.world.query::<&TextContent>();
            q.iter(&app.world).map(|t| t.0.clone()).collect()
        };
        texts.sort();
        let mut visuals: Vec<(u32, u32, u32, u32, u32)> = {
            let mut q = app.world.query::<&Visuals>();
            q.iter(&app.world)
                .map(|v| {
                    let (r, g, b, a) = match &v.fill {
                        Some(Fill::Solid(c)) => (
                            (c.r * 255.0).round() as u32,
                            (c.g * 255.0).round() as u32,
                            (c.b * 255.0).round() as u32,
                            (c.a * 255.0).round() as u32,
                        ),
                        _ => (0, 0, 0, 0),
                    };
                    (r, g, b, a, (v.radius * 100.0).round() as u32)
                })
                .collect()
        };
        visuals.sort();
        (count, texts, visuals)
    }

    fn build_and_tick(opts: RunOptions, ticks: u32) -> App {
        let (mut app, _winit) = build_app(opts).expect("build_app");
        app.add_plugin(lumen_window_winit::WinitPlugin);
        for _ in 0..ticks {
            app.tick();
        }
        app
    }

    #[test]
    fn artifact_spawns_identically_to_parsed_source() {
        let _serial = crate::serial();
        let dir = write_app(MARKUP, CSS);

        // Path A - parse from source directly.
        let mut app_a = build_and_tick(
            RunOptions::new(&dir).with_parser(lumenc::default_parser()),
            4,
        );
        let snap_a = snapshot(&mut app_a);

        // Compile -> serialize -> deserialize (the full artifact codec), then
        // spawn from the decoded artifact with no parser involvement.
        let compiled = lumenc::compile_app(&dir).expect("compile_app");
        let bytes = lumenc::artifact::serialize(&compiled).expect("serialize");
        let decoded = lumenc::artifact::deserialize(&bytes).expect("deserialize");
        let art_path = dir.join("app.lmna");
        lumenc::artifact::write(&art_path, &decoded).expect("write artifact");

        let mut app_b = build_and_tick(
            RunOptions::new(&dir)
                .with_parser(lumenc::default_parser())
                .with_artifact(&art_path),
            4,
        );
        let snap_b = snapshot(&mut app_b);

        assert_eq!(
            snap_a, snap_b,
            "artifact-spawned world diverged from parse-directly world"
        );

        // Byte-level fidelity: re-encoding the decoded artifact reproduces
        // the original bytes exactly (IR + cascaded stylesheet round-trip).
        let reencoded = lumenc::artifact::serialize(&decoded).expect("re-serialize");
        assert_eq!(bytes, reencoded, "artifact re-serialization not stable");

        let _ = std::fs::remove_dir_all(&dir);
    }
}
