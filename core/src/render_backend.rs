//! The render backends a binary carries, how a launch picks one, and the
//! render-world system that drives a renderer drawing offscreen.
//!
//! The core names no backend. Each one registers a [`RenderBackend`] into the
//! app's [`RenderBackends`] while the app is built, from its own crate, and a
//! launch path asks the registry for what the app's configuration names: one
//! backend by name, or every backend in priority order so a launch can fall
//! through to the next one when the first cannot start. A binary that does
//! not link a backend has no entry for it, which is how an app pays only for
//! the renderer it uses.

use crate::app::App;
use crate::render_world::{RenderStage, Viewport, install_extract_pipeline};
use crate::traits::{FrameRequest, Renderer};
use bevy_ecs::prelude::Resource;
use bevy_ecs::world::World;

/// One render backend: its name, its rank under `auto`, and how to make its
/// renderer.
#[derive(Clone, Copy, Debug)]
pub struct RenderBackend {
    /// The name an app's configuration selects it by.
    pub name: &'static str,
    /// Trial order when the configuration names no backend: higher first.
    pub priority: i32,
    /// A renderer bound to nothing yet. Construction does no work; binding
    /// to a window or an offscreen image happens in [`Renderer::attach`],
    /// and a failure there is what lets a launch try the next backend.
    pub renderer: fn() -> Box<dyn Renderer>,
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

/// Put `renderer`, already attached to a
/// [`crate::traits::FrameTarget::Offscreen`] image, into `app`.
///
/// The renderer becomes a non-send render-world resource of type `R`, and a
/// system in [`RenderStage::Render`] drives it through [`Renderer`] each
/// frame the tick renders: it resizes the image to the viewport's physical
/// size and presents when the renderer says the frame changed. Also installs
/// the extract pipeline the renderer reads the retained scene from.
pub fn install_offscreen<R: Renderer>(app: &mut App, renderer: R) {
    install_extract_pipeline(app);
    app.render_world.insert_non_send(renderer);
    app.add_render_systems(RenderStage::Render, present_offscreen::<R>);
}

/// The viewport's size in physical pixels, at least one each way.
fn physical_size(viewport: &Viewport) -> (u32, u32) {
    let dpr = viewport.scale_factor.max(0.01);
    (
        (viewport.size.x * dpr).max(1.0) as u32,
        (viewport.size.y * dpr).max(1.0) as u32,
    )
}

/// Render-world system driving the offscreen renderer installed as `R`.
///
/// The renderer is lifted out of the world for the call, because
/// [`Renderer::present`] reads the same world it lives in.
fn present_offscreen<R: Renderer>(world: &mut World) {
    let Some(mut renderer) = world.remove_non_send::<R>() else {
        return;
    };
    let (width, height) = world
        .get_resource::<Viewport>()
        .map(physical_size)
        .unwrap_or((1, 1));
    let resized = renderer.resize(width, height);
    // The tick runs the render schedule only when its frame is dirty, so the
    // request always is; a fresh image holds no frame and repaints in full.
    let request = FrameRequest {
        dirty: true,
        force_full: resized,
    };
    if renderer.wants_present(world, request)
        && let Err(e) = renderer.present(world)
    {
        eprintln!("lumen: offscreen render failed: {e}");
    }
    world.insert_non_send(renderer);
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::traits::{FrameTarget, RenderError};
    use bevy_ecs::world::World;

    /// A renderer that records what it was asked and answers `true` to
    /// every present gate, so the offscreen driver can be watched.
    #[derive(Default)]
    struct Recorder {
        attached: Option<(u32, u32)>,
        requests: Vec<FrameRequest>,
        presents: usize,
    }

    impl Renderer for Recorder {
        fn attach(&mut self, target: FrameTarget) -> Result<(), RenderError> {
            match target {
                FrameTarget::Offscreen { width, height } => {
                    self.attached = Some((width, height));
                    Ok(())
                }
                FrameTarget::Window(_) => Err(RenderError::Init("no windows here".into())),
            }
        }
        fn resize(&mut self, width: u32, height: u32) -> bool {
            let changed = self.attached != Some((width, height));
            self.attached = Some((width, height));
            changed
        }
        fn wants_present(&mut self, _: &mut World, request: FrameRequest) -> bool {
            self.requests.push(request);
            true
        }
        fn present(&mut self, _: &mut World) -> Result<(), RenderError> {
            self.presents += 1;
            Ok(())
        }
        fn detach(&mut self) {
            self.attached = None;
        }
    }

    fn renderer() -> Box<dyn Renderer> {
        Box::new(Recorder::default())
    }

    fn backend(name: &'static str, priority: i32) -> RenderBackend {
        RenderBackend {
            name,
            priority,
            renderer,
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
    /// constructor: the renderer it builds binds to the targets it supports
    /// and refuses the ones it does not, with the reason intact.
    #[test]
    fn a_selected_backend_builds_the_renderer_it_registered() {
        let mut backends = RenderBackends::default();
        backends.register(backend("test", 1));
        let [test] = backends.select(None).expect("one").try_into().expect("one");

        let mut renderer = (test.renderer)();
        renderer
            .attach(FrameTarget::Offscreen {
                width: 8,
                height: 8,
            })
            .expect("offscreen binds");
        assert!(!renderer.resize(8, 8));
        assert!(renderer.resize(9, 8));
        renderer.detach();
    }

    /// The offscreen driver sizes the image from the viewport in physical
    /// pixels, asks for a frame on every rendered tick, and forces a full
    /// one when the size changed.
    #[test]
    fn the_offscreen_driver_follows_the_viewport() {
        let mut app = App::new();
        let mut recorder = Recorder::default();
        recorder
            .attach(FrameTarget::Offscreen {
                width: 20,
                height: 10,
            })
            .expect("binds");
        install_offscreen(&mut app, recorder);
        for world in [&mut app.world, &mut app.render_world] {
            let mut viewport = world.resource_mut::<Viewport>();
            viewport.size = glam::Vec2::new(10.0, 5.0);
            viewport.scale_factor = 2.0;
        }

        app.tick();
        let recorder = app.render_world.non_send::<Recorder>();
        assert_eq!(recorder.attached, Some((20, 10)));
        assert_eq!(recorder.presents, 1);
        assert!(recorder.requests[0].dirty);
        assert!(!recorder.requests[0].force_full, "the size did not change");

        for world in [&mut app.world, &mut app.render_world] {
            world.resource_mut::<Viewport>().size = glam::Vec2::new(12.0, 5.0);
        }
        app.tick();
        let recorder = app.render_world.non_send::<Recorder>();
        assert_eq!(recorder.attached, Some((24, 10)));
        assert!(recorder.requests[1].force_full, "a resize repaints in full");
    }
}
