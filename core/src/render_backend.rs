//! The render backends a binary carries, and how a launch picks one.
//!
//! The core names no backend. Each one registers a [`RenderBackend`] into the
//! app's [`RenderBackends`] while the app is built, from its own crate, and a
//! launch path asks the registry for what the app's configuration names: one
//! backend by name, or every backend in priority order so a launch can fall
//! through to the next one when the first cannot start. A binary that does
//! not link a backend has no entry for it, which is how an app pays only for
//! the renderer it uses.

use crate::app::App;
use crate::traits::SurfaceRenderer;
use bevy_ecs::prelude::Resource;

/// A renderer that draws into an offscreen target, built before the app is
/// installed into and ready to paint once it is.
///
/// Construction is the expensive half (a device, a rasterizer), and it needs
/// nothing from the app, so a launch can build one on another thread while
/// the app builds.
pub trait OffscreenRenderer: Send {
    /// Put this renderer into `app`. From then on its render-world system
    /// paints the retained scene each frame at the viewport's size, and
    /// answers [`crate::render_world::SurfaceCapture`] requests from the last
    /// frame it painted.
    fn install(self: Box<Self>, app: &mut App);
}

/// One render backend: how to make each kind of renderer it offers.
#[derive(Clone, Copy, Debug)]
pub struct RenderBackend {
    /// The name an app's configuration selects it by.
    pub name: &'static str,
    /// Trial order when the configuration names no backend: higher first.
    pub priority: i32,
    /// A renderer that presents into a window. Construction does no work;
    /// binding to the window happens in [`SurfaceRenderer::attach`], and a
    /// failure there is what lets a launch try the next backend.
    pub surface: fn() -> Box<dyn SurfaceRenderer>,
    /// A renderer that draws offscreen at `width` x `height` physical
    /// pixels, or why this machine cannot run one.
    pub offscreen: fn(u32, u32) -> Result<Box<dyn OffscreenRenderer>, String>,
}

/// Main-world registry of the render backends this binary carries.
#[derive(Resource, Clone, Debug, Default)]
pub struct RenderBackends {
    entries: Vec<RenderBackend>,
}

impl RenderBackends {
    /// Add `backend`, replacing one registered under the same name.
    pub fn register(&mut self, backend: RenderBackend) {
        self.entries.retain(|b| b.name != backend.name);
        self.entries.push(backend);
    }

    /// The backend registered as `name`.
    pub fn get(&self, name: &str) -> Option<&RenderBackend> {
        self.entries.iter().find(|b| b.name == name)
    }

    /// Every backend, highest priority first; equal priorities in name order,
    /// so the order is the same in every process.
    pub fn by_priority(&self) -> Vec<RenderBackend> {
        let mut list = self.entries.clone();
        list.sort_by(|a, b| b.priority.cmp(&a.priority).then(a.name.cmp(b.name)));
        list
    }

    /// The registered names, sorted, for a message naming what is available.
    pub fn names(&self) -> Vec<&'static str> {
        let mut names: Vec<&'static str> = self.entries.iter().map(|b| b.name).collect();
        names.sort_unstable();
        names
    }

    /// The backends a launch should try, in order: the one `choice` names,
    /// or every backend by priority when `choice` is `None`. An error names
    /// what was asked for and what this binary carries.
    pub fn select(&self, choice: Option<&str>) -> Result<Vec<RenderBackend>, String> {
        match choice {
            Some(name) => self.get(name).map(|b| vec![*b]).ok_or_else(|| {
                format!(
                    "the app asks for the '{name}' render backend, and this build carries {}",
                    describe(&self.names())
                )
            }),
            None if self.entries.is_empty() => {
                Err("this build carries no render backend".to_string())
            }
            None => Ok(self.by_priority()),
        }
    }
}

/// `"none"`, `"only 'a'"`, or `"'a' and 'b'"`, for an error message.
fn describe(names: &[&str]) -> String {
    match names {
        [] => "none".to_string(),
        [one] => format!("only '{one}'"),
        many => {
            let quoted: Vec<String> = many.iter().map(|n| format!("'{n}'")).collect();
            let (last, rest) = quoted.split_last().expect("at least two");
            format!("{} and {last}", rest.join(", "))
        }
    }
}

/// Register `backend` into `app`'s [`RenderBackends`], creating the registry
/// on first use.
pub fn register_render_backend(app: &mut App, backend: RenderBackend) {
    app.world
        .get_resource_or_insert_with(RenderBackends::default)
        .register(backend);
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::traits::{FrameRequest, RenderTarget, Renderer, SurfaceError};
    use bevy_ecs::world::World;
    use std::sync::Arc;

    struct Nothing;
    impl Renderer for Nothing {}
    impl SurfaceRenderer for Nothing {
        fn attach(&mut self, _: Arc<dyn RenderTarget>) -> Result<(), SurfaceError> {
            Ok(())
        }
        fn resize(&mut self, _: u32, _: u32) -> bool {
            false
        }
        fn wants_present(&mut self, _: &mut World, _: FrameRequest) -> bool {
            false
        }
        fn present(&mut self, _: &mut World) -> Result<(), SurfaceError> {
            Ok(())
        }
        fn detach(&mut self) {}
    }

    fn surface() -> Box<dyn SurfaceRenderer> {
        Box::new(Nothing)
    }

    fn offscreen(_: u32, _: u32) -> Result<Box<dyn OffscreenRenderer>, String> {
        Err("not in a test".to_string())
    }

    fn backend(name: &'static str, priority: i32) -> RenderBackend {
        RenderBackend {
            name,
            priority,
            surface,
            offscreen,
        }
    }

    #[test]
    fn a_name_selects_one_backend_and_none_selects_all_by_priority() {
        let mut backends = RenderBackends::default();
        backends.register(backend("slow", 0));
        backends.register(backend("fast", 10));
        backends.register(backend("also-slow", 0));

        let named = backends.select(Some("slow")).expect("registered");
        assert_eq!(named.len(), 1);
        assert_eq!(named[0].name, "slow");

        let order: Vec<&str> = backends
            .select(None)
            .expect("some")
            .iter()
            .map(|b| b.name)
            .collect();
        assert_eq!(order, ["fast", "also-slow", "slow"]);
    }

    #[test]
    fn asking_for_a_backend_the_build_lacks_says_what_it_has() {
        let mut backends = RenderBackends::default();
        let none = backends.select(Some("cpu")).expect_err("empty");
        assert!(none.contains("'cpu'") && none.contains("none"), "{none}");
        assert!(backends.select(None).is_err());

        backends.register(backend("gpu", 10));
        let one = backends.select(Some("cpu")).expect_err("missing");
        assert!(one.contains("only 'gpu'"), "{one}");

        backends.register(backend("other", 0));
        backends.register(backend("third", 0));
        let many = backends.select(Some("cpu")).expect_err("missing");
        assert!(many.contains("'gpu', 'other' and 'third'"), "{many}");
    }

    #[test]
    fn registering_a_name_again_replaces_it() {
        let mut app = App::new();
        register_render_backend(&mut app, backend("gpu", 1));
        register_render_backend(&mut app, backend("gpu", 5));
        let backends = app.world.resource::<RenderBackends>();
        assert_eq!(backends.names(), ["gpu"]);
        assert_eq!(backends.get("gpu").map(|b| b.priority), Some(5));
    }

    /// What a launch gets back from the registry is the backend's own
    /// constructors: the surface renderer it builds and the offscreen
    /// failure it reports reach the caller unchanged.
    #[test]
    fn a_selected_backend_builds_the_renderers_it_registered() {
        let mut backends = RenderBackends::default();
        backends.register(backend("gpu", 1));
        let [gpu] = backends.select(None).expect("one").try_into().expect("one");

        let mut surface = (gpu.surface)();
        let mut world = World::new();
        assert!(!surface.resize(8, 8));
        let request = FrameRequest {
            dirty: true,
            force_full: false,
        };
        assert!(!surface.wants_present(&mut world, request));
        assert!(surface.present(&mut world).is_ok());
        surface.detach();

        let offscreen = (gpu.offscreen)(8, 8).err();
        assert_eq!(offscreen.as_deref(), Some("not in a test"));
    }
}
