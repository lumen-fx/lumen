//! The end-to-end fixture module: a real [`lumen_module::Plugin`] with its
//! own component and resource, a per-tick mutating system, a query over a
//! host component (`TextContent`, spawned from the app's markup), a config
//! read, and config-driven panic paths. The subprocess harness in
//! `crates/lumen-module/tests` asserts on the lines it prints.

#[cfg(not(windows))]
mod module {
    use bevy_ecs::prelude::*;
    use lumen_core::components::TextContent;
    use lumen_core::tick::TickStage;
    use lumen_module::{App, ModuleConfig, Plugin, lumen_module};
    use lumen_script::{ScriptCommand, ScriptFn, ScriptFnAppExt, ScriptTy, ScriptValue};

    /// The module's own resource, mutated every tick.
    #[derive(Resource, Default)]
    struct FixtureTicks(u64);

    /// Which tick the system panics at, when the config asks for one.
    #[derive(Resource, Default)]
    struct PanicAt(Option<u64>);

    /// The module's own component, spawned at install and counted per tick.
    #[derive(Component)]
    struct FixtureMark;

    struct FixturePlugin {
        units: String,
        panic_at: Option<u64>,
    }

    impl Plugin for FixturePlugin {
        fn build(self, app: &mut App) {
            println!("module-install units={}", self.units);
            app.world.insert_resource(FixtureTicks::default());
            app.world.insert_resource(PanicAt(self.panic_at));
            app.world.spawn(FixtureMark);
            app.add_systems(TickStage::Systems, fixture_tick);
            // A module reaches the app's scripts through the same one
            // registry an in-process plugin uses - shared engine, shared
            // resource. The signal write makes the round trip assertable
            // from the harness in every language.
            app.add_script_fn(
                ScriptFn::new("module_double")
                    .param("n", ScriptTy::Int)
                    .build(|cx| {
                        let doubled = cx.int_arg(0) * 2;
                        cx.emit(ScriptCommand::SetSignal {
                            name: "module_doubled".to_string(),
                            value: doubled.to_string(),
                        });
                        Ok(ScriptValue::I64(doubled))
                    }),
            );
        }
    }

    fn fixture_tick(
        mut ticks: ResMut<FixtureTicks>,
        panic_at: Res<PanicAt>,
        texts: Query<&TextContent>,
        marks: Query<&FixtureMark>,
    ) {
        ticks.0 += 1;
        println!(
            "module-tick n={} texts={} marks={}",
            ticks.0,
            texts.iter().count(),
            marks.iter().count()
        );
        if panic_at.0 == Some(ticks.0) {
            panic!("fixture module panics at tick {} on request", ticks.0);
        }
    }

    lumen_module!(|config: ModuleConfig| {
        if config.bool("panic_in_ctor").unwrap_or(false) {
            panic!("fixture module constructor panics on request");
        }
        FixturePlugin {
            units: config.str("units").unwrap_or("unset").to_string(),
            panic_at: config.int("panic_at_tick").map(|n| n as u64),
        }
    });
}
