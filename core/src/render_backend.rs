//! The render backends a binary carries, how a launch picks one, and the
//! render-world system that drives a renderer drawing offscreen.
//!
//! The core names no backend. Each one registers a [`RenderBackend`] into the
//! app's [`RenderBackends`] while the app is built, from its own crate, and a
//! launch path asks the registry for what the app's configuration names: one
//! backend by name, or every backend in priority order so a launch can fall
//! through to the next one when the first cannot start. A binary that does
//! not link a backend has no entry for it, which is how an app pays only for
//! the renderer it uses. The registry is the one every backend kind shares;
//! see [`crate::backends`].

use crate::app::App;
use crate::backends::{BackendEntry, Backends};
use crate::render_world::{RenderStage, Viewport, install_extract_pipeline};
use crate::traits::{FrameRequest, Renderer};
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

impl BackendEntry for RenderBackend {
    const KIND: &'static str = "render";

    fn name(&self) -> &'static str {
        self.name
    }

    fn priority(&self) -> i32 {
        self.priority
    }
}

/// Main-world registry of the render backends this binary carries. A
/// backend's capability install adds itself with
/// [`crate::backends::register_backend`].
pub type RenderBackends = Backends<RenderBackend>;

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

    /// A render backend the app names and the build lacks is reported as a
    /// render backend, by the name the configuration used.
    #[test]
    fn a_missing_render_backend_says_render() {
        let mut backends = RenderBackends::default();
        backends.register(backend("gpu", 1));
        let err = backends.select(Some("cpu")).expect_err("missing");
        assert_eq!(
            err,
            "the app asks for the 'cpu' render backend, and this build carries only 'gpu'"
        );
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
