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
use crate::render_world::{RenderStage, RenderWindow, install_extract_pipeline};
use crate::traits::{FrameRequest, FrameTarget, Renderer};
use crate::window::WindowViewport;
use bevy_ecs::entity::Entity;
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

/// What kind of device a renderer draws on.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash)]
pub enum DeviceKind {
    /// A graphics card of its own.
    DiscreteGpu,
    /// A GPU sharing the CPU's package and memory.
    IntegratedGpu,
    /// A GPU a hypervisor hands the machine.
    VirtualGpu,
    /// The CPU: a software rasterizer, or a GPU API emulated on the CPU.
    Cpu,
    /// The renderer cannot tell.
    #[default]
    Unknown,
}

/// Device memory the renderer's own allocations hold, as far as the graphics
/// API reports it. Not the device's capacity: no portable API tells that.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash)]
pub struct DeviceMemory {
    /// Bytes in live allocations: textures, buffers, pipelines' scratch.
    pub allocated: u64,
    /// Bytes in the memory blocks those allocations are carved from, free
    /// space inside them included.
    pub reserved: u64,
}

/// Which renderer draws the app and on what: what an app reads to show its
/// renderer, or to size what it keeps on the device.
///
/// A main-world resource, present once a renderer has drawn a frame. The
/// renderer reports it through [`crate::traits::Renderer::info`]; a launch
/// that started a renderer publishes it into the render world after each
/// frame, and [`crate::app::App::tick`] copies it across. Fields a renderer
/// cannot tell stay empty.
#[derive(bevy_ecs::resource::Resource, Clone, Debug, Default, PartialEq, Eq)]
pub struct RendererInfo {
    /// The backend's name in [`RenderBackends`], the one `[render] backend`
    /// selects it by: under `auto`, which of them started.
    pub backend: &'static str,
    /// The device, as its driver names it.
    pub device: String,
    /// What kind of device it is.
    pub device_kind: DeviceKind,
    /// The PCI vendor id, or 0.
    pub vendor_id: u32,
    /// The PCI device id, or 0.
    pub device_id: u32,
    /// The graphics API the renderer drives it through: `Vulkan`, `Metal`,
    /// `Dx12`, `Gl`; empty for a CPU rasterizer.
    pub api: String,
    /// The driver, and its version where it says.
    pub driver: String,
    /// The longest side of a texture the device makes, in pixels, or 0 when
    /// it has no such limit.
    pub max_texture_side: u32,
    /// What the renderer's allocations hold, when the API reports it.
    pub memory: Option<DeviceMemory>,
}

/// Put `info` into `world` unless it is already there, so a reader's change
/// detection sees a change only when the renderer's answer moved.
pub fn publish_renderer_info(world: &mut World, info: RendererInfo) {
    if world.get_resource::<RendererInfo>() != Some(&info) {
        world.insert_resource(info);
    }
}

/// Put `renderer` into `app` to draw every window offscreen.
///
/// The renderer becomes a non-send render-world resource of type `R`, and a
/// system in [`RenderStage::Render`] drives it through [`Renderer`] each
/// frame the tick renders: it attaches an offscreen image for each window
/// the renderer has no target for yet (a renderer the launch attached to the
/// primary window already keeps that one), sizes each image to its window's
/// viewport in physical pixels, presents each shown window when the renderer
/// says its frame changed, and releases the image of a window that closed.
/// Also installs the extract pipeline the renderer reads the retained scene
/// from.
pub fn install_offscreen<R: Renderer>(app: &mut App, renderer: R) {
    install_extract_pipeline(app);
    app.render_world.insert_non_send(renderer);
    app.add_render_systems(RenderStage::Render, present_offscreen::<R>);
}

/// Render-world system driving the offscreen renderer installed as `R`.
///
/// The renderer is lifted out of the world for the call, because
/// [`Renderer::present`] reads the same world it lives in.
fn present_offscreen<R: Renderer>(world: &mut World) {
    let Some(mut renderer) = world.remove_non_send::<R>() else {
        return;
    };
    let windows: Vec<(Entity, (u32, u32), bool)> = {
        let mut q = world.query::<(&RenderWindow, &WindowViewport)>();
        let mut windows: Vec<_> = q
            .iter(world)
            .map(|(w, v)| (w.window, v.physical_size(), w.visible))
            .collect();
        windows.sort_by_key(|(w, ..)| *w);
        windows
    };
    for target in renderer.targets() {
        if !windows.iter().any(|(w, ..)| *w == target) {
            renderer.detach(target);
        }
    }
    let attached = renderer.targets();
    let mut presented = false;
    for (target, (width, height), visible) in windows {
        if !visible {
            continue;
        }
        let fresh = !attached.contains(&target);
        if fresh && let Err(e) = renderer.attach(target, FrameTarget::Offscreen { width, height }) {
            eprintln!("lumen: offscreen target for a window failed: {e}");
            continue;
        }
        let resized = renderer.resize(target, width, height);
        // The tick runs the render schedule only when its frame is dirty, so the
        // request always is; a fresh image holds no frame and repaints in full.
        let request = FrameRequest {
            dirty: true,
            force_full: resized || fresh,
        };
        if renderer.wants_present(target, world, request) {
            if let Err(e) = renderer.present(target, world) {
                eprintln!("lumen: offscreen render failed: {e}");
            }
            presented = true;
        }
    }
    if presented {
        publish_renderer_info(world, renderer.info());
    }
    world.insert_non_send(renderer);
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::traits::{FrameTarget, RenderError};
    use crate::window::{Window, primary_viewport_mut};
    use bevy_ecs::world::World;
    use std::collections::BTreeMap;

    /// A renderer that records what it was asked and answers `true` to
    /// every present gate, so the offscreen driver can be watched.
    #[derive(Default)]
    struct Recorder {
        attached: BTreeMap<Entity, (u32, u32)>,
        requests: Vec<(Entity, FrameRequest)>,
        presents: usize,
    }

    impl Recorder {
        fn size(&self, target: Entity) -> Option<(u32, u32)> {
            self.attached.get(&target).copied()
        }
    }

    impl Renderer for Recorder {
        fn attach(&mut self, target: Entity, frame: FrameTarget) -> Result<(), RenderError> {
            match frame {
                FrameTarget::Offscreen { width, height } => {
                    self.attached.insert(target, (width, height));
                    Ok(())
                }
                FrameTarget::Window(_) => Err(RenderError::Init("no windows here".into())),
            }
        }
        fn resize(&mut self, target: Entity, width: u32, height: u32) -> bool {
            let changed = self.attached.get(&target) != Some(&(width, height));
            self.attached.insert(target, (width, height));
            changed
        }
        fn wants_present(&mut self, target: Entity, _: &mut World, request: FrameRequest) -> bool {
            self.requests.push((target, request));
            true
        }
        fn present(&mut self, _: Entity, world: &mut World) -> Result<(), RenderError> {
            self.presents += 1;
            let mut stamps = world.resource_mut::<crate::tick::RenderTimestamps>();
            stamps.mark_encoded();
            stamps.mark_submitted();
            Ok(())
        }
        fn info(&mut self) -> RendererInfo {
            RendererInfo {
                backend: "test",
                device: format!("recorder after {} frames", self.presents),
                ..RendererInfo::default()
            }
        }
        fn targets(&self) -> Vec<Entity> {
            self.attached.keys().copied().collect()
        }
        fn detach(&mut self, target: Entity) {
            self.attached.remove(&target);
        }
        fn detach_all(&mut self) {
            self.attached.clear();
        }
    }

    fn target() -> Entity {
        Entity::from_raw_u32(7).expect("an entity id")
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
            .attach(
                target(),
                FrameTarget::Offscreen {
                    width: 8,
                    height: 8,
                },
            )
            .expect("offscreen binds");
        assert!(!renderer.resize(target(), 8, 8));
        assert!(renderer.resize(target(), 9, 8));
        assert_eq!(renderer.targets(), vec![target()]);
        renderer.detach(target());
        assert!(renderer.targets().is_empty());
        renderer.detach_all();
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
        let primary = crate::window::primary_window(&mut app.world).expect("a window");
        let mut recorder = Recorder::default();
        recorder
            .attach(
                primary,
                FrameTarget::Offscreen {
                    width: 20,
                    height: 10,
                },
            )
            .expect("binds");
        install_offscreen(&mut app, recorder);
        {
            let mut viewport = primary_viewport_mut(&mut app.world);
            viewport.size = glam::Vec2::new(10.0, 5.0);
            viewport.scale_factor = 2.0;
        }

        app.tick();
        let recorder = app.render_world.non_send::<Recorder>();
        assert_eq!(recorder.size(primary), Some((20, 10)));
        assert_eq!(recorder.presents, 1);
        assert!(recorder.requests[0].1.dirty);
        assert!(
            !recorder.requests[0].1.force_full,
            "the size did not change"
        );

        primary_viewport_mut(&mut app.world).size = glam::Vec2::new(12.0, 5.0);
        app.tick();
        let recorder = app.render_world.non_send::<Recorder>();
        assert_eq!(recorder.size(primary), Some((24, 10)));
        assert!(
            recorder.requests[1].1.force_full,
            "a resize repaints in full"
        );
    }

    /// A second window gets an image of its own on the same renderer, sized
    /// to its own viewport and painted in full first; a hidden one is left
    /// alone, and a closed one's image is released.
    #[test]
    fn the_offscreen_driver_draws_every_window() {
        let mut app = App::new();
        let primary = crate::window::primary_window(&mut app.world).expect("a window");
        install_offscreen(&mut app, Recorder::default());
        let second = app
            .world
            .spawn(Window::new("Second", glam::Vec2::new(30.0, 20.0)))
            .id();
        app.world.get_mut::<WindowViewport>(second).unwrap().size = glam::Vec2::new(30.0, 20.0);
        let hidden = app
            .world
            .spawn(Window {
                visible: false,
                ..Window::default()
            })
            .id();
        app.tick();
        let recorder = app.render_world.non_send::<Recorder>();
        assert_eq!(recorder.size(second), Some((30, 20)));
        assert!(recorder.size(primary).is_some());
        assert_eq!(recorder.size(hidden), None, "a hidden window draws nothing");
        assert!(
            recorder
                .requests
                .iter()
                .any(|(t, r)| *t == second && r.force_full),
            "a fresh image repaints in full"
        );

        app.world.entity_mut(second).despawn();
        app.world
            .resource_mut::<crate::render_world::FrameDirty>()
            .dirty = true;
        app.tick();
        let recorder = app.render_world.non_send::<Recorder>();
        assert_eq!(recorder.size(second), None, "a closed window's image goes");
        assert!(recorder.size(primary).is_some());
    }

    /// #527: a tick records when each of its phases ended, the renderer's
    /// own instants included, where a system on the next tick reads them;
    /// a tick that painted nothing says so.
    #[test]
    fn a_tick_records_its_phases_for_the_next_one() {
        let mut app = App::new();
        install_offscreen(&mut app, Recorder::default());
        app.tick();
        let first = *app.world.resource::<crate::tick::FrameTiming>();
        assert_eq!(first.frame, 1);
        assert!(first.painted());
        let (extract, encoded, submitted, end) = (
            first.extract_end.expect("extracted"),
            first.encoded.expect("the renderer stamped its encode"),
            first.submitted.expect("the renderer stamped its submit"),
            first.render_end.expect("rendered"),
        );
        assert!(first.start <= first.update_end && first.update_end <= extract);
        assert!(extract <= encoded && encoded <= submitted && submitted <= end);
        assert_eq!(
            first.presented, None,
            "an offscreen image is never on screen"
        );
        assert_eq!(first.end(), end);

        // What a system sees during the next tick is the tick before.
        #[derive(bevy_ecs::resource::Resource, Default)]
        struct Seen(u64);
        app.world.init_resource::<Seen>();
        app.add_systems(
            crate::tick::TickStage::Systems,
            |timing: bevy_ecs::system::Res<crate::tick::FrameTiming>,
             mut seen: bevy_ecs::system::ResMut<Seen>| seen.0 = timing.frame,
        );
        app.world
            .resource_mut::<crate::render_world::FrameDirty>()
            .dirty = false;
        app.tick();
        assert_eq!(app.world.resource::<Seen>().0, 1);
        let idle = *app.world.resource::<crate::tick::FrameTiming>();
        assert_eq!(idle.frame, 2);
        assert!(!idle.painted(), "a tick that was not dirty painted nothing");
        assert_eq!(idle.encoded, None);
    }

    /// #528: what the renderer says about itself reaches the main world
    /// after it draws, and is written again only when it changed.
    #[test]
    fn the_renderer_reports_itself_after_it_draws() {
        use bevy_ecs::change_detection::DetectChanges;
        let mut app = App::new();
        install_offscreen(&mut app, Recorder::default());
        assert!(app.world.get_resource::<RendererInfo>().is_none());
        app.tick();
        let info = app.world.resource::<RendererInfo>();
        assert_eq!(info.backend, "test");
        assert_eq!(info.device, "recorder after 1 frames");
        let first = app.world.resource_ref::<RendererInfo>().last_changed();
        app.world
            .resource_mut::<crate::render_world::FrameDirty>()
            .dirty = false;
        app.tick();
        assert_eq!(
            app.world.resource_ref::<RendererInfo>().last_changed(),
            first,
            "a tick that drew nothing leaves it alone"
        );
    }
}
