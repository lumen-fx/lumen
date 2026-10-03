//! The layout engines a binary carries, how a launch installs one, and the
//! system set that marks where layout is solved.
//!
//! Each engine registers a [`LayoutBackend`] into the app's
//! [`LayoutBackends`] from its own crate, and the launch installs the
//! highest-priority one with [`install_layout`]. The core and the runtime
//! name no engine.

use crate::app::App;
use crate::backends::{BackendEntry, Backends, missing, registered};
use crate::traits::LayoutEngine;
use bevy_ecs::schedule::SystemSet;

/// One layout engine: its name, its rank, and how to make it.
#[derive(Clone, Copy, Debug)]
pub struct LayoutBackend {
    /// The name the engine is registered under.
    pub name: &'static str,
    /// Install order preference: the highest-priority engine is the one a
    /// launch installs.
    pub priority: i32,
    /// The engine, not yet installed. Construction does no work;
    /// [`LayoutEngine::install`] does.
    pub engine: fn() -> Box<dyn LayoutEngine>,
}

impl BackendEntry for LayoutBackend {
    const KIND: &'static str = "layout";

    fn name(&self) -> &'static str {
        self.name
    }

    fn priority(&self) -> i32 {
        self.priority
    }
}

/// Main-world registry of the layout engines this binary carries. An
/// engine's capability install adds itself with
/// [`crate::backends::register_backend`].
pub type LayoutBackends = Backends<LayoutBackend>;

/// The systems in [`crate::tick::TickStage::LayoutSync`] that write each
/// entity's [`crate::components::Transform`] for this tick.
///
/// A system that reads this tick's boxes (keeping a caret in view, placing
/// an overlay) runs `.after(LayoutSolve)` and works under any engine.
#[derive(SystemSet, Clone, Copy, Debug, Hash, PartialEq, Eq)]
pub struct LayoutSolve;

/// Install the highest-priority layout engine registered into `app`, and
/// return its name. An error says the build carries none.
pub fn install_layout(app: &mut App) -> Result<&'static str, String> {
    let backend = registered::<LayoutBackend>(app)
        .preferred()
        .ok_or_else(missing::<LayoutBackend>)?;
    (backend.engine)().install(app);
    Ok(backend.name)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::backends::register_backend;
    use crate::components::Transform;
    use crate::tick::TickStage;
    use bevy_ecs::prelude::*;

    /// Moves every box one pixel right per tick, in [`LayoutSolve`].
    struct Nudge;

    fn nudge(mut boxes: Query<&mut Transform>) {
        for mut t in &mut boxes {
            t.absolute.x += 1.0;
        }
    }

    impl LayoutEngine for Nudge {
        fn install(self: Box<Self>, app: &mut App) {
            app.add_systems(TickStage::LayoutSync, nudge.in_set(LayoutSolve));
        }
    }

    fn nudge_engine() -> Box<dyn LayoutEngine> {
        Box::new(Nudge)
    }

    /// Records the x a box had when it ran.
    #[derive(Resource, Default)]
    struct Seen(Vec<f32>);

    fn after_solve(boxes: Query<&Transform>, mut seen: ResMut<Seen>) {
        seen.0.extend(boxes.iter().map(|t| t.absolute.x));
    }

    /// A build with no engine says so instead of laying nothing out.
    #[test]
    fn a_build_without_an_engine_says_so() {
        let mut app = App::new();
        assert_eq!(
            install_layout(&mut app).expect_err("none registered"),
            "this build carries no layout backend"
        );
    }

    /// The launch installs the engine the registry ranks first, and a
    /// system ordered after [`LayoutSolve`] sees the boxes it wrote this
    /// tick.
    #[test]
    fn the_preferred_engine_installs_and_its_solve_runs_first() {
        let mut app = App::new();
        register_backend(
            &mut app,
            LayoutBackend {
                name: "nudge",
                priority: 1,
                engine: nudge_engine,
            },
        );
        app.world.init_resource::<Seen>();
        app.add_systems(TickStage::LayoutSync, after_solve.after(LayoutSolve));
        assert_eq!(install_layout(&mut app), Ok("nudge"));
        app.world.spawn(Transform::default());

        app.tick();
        app.tick();
        assert_eq!(app.world.resource::<Seen>().0, [1.0, 2.0]);
    }
}
