//! The `render-cpu` capability: this backend in the app's render-backend
//! registry, under the name `cpu`.

use lumen_capability::{CapabilityEnv, Select};
use lumen_core::app::App;
use lumen_core::backends::register_backend;
use lumen_core::render_backend::RenderBackend;
use lumen_core::traits::Renderer;

use crate::CpuRenderer;

/// The name `[render] backend` selects this backend by.
pub const NAME: &str = "cpu";

/// Behind the GPU backend: an `auto` launch reaches the CPU when no GPU
/// starts.
pub const PRIORITY: i32 = 0;

/// A static package carries this backend when `[render] backend` is `cpu`
/// or `auto`, the default.
pub const SELECT: Select = Select::OnConfig {
    key: "render.backend",
    any_of: &["cpu", "auto"],
    default: "auto",
};

/// Register the backend. What the capability crate beside this one
/// registers.
pub fn install(app: &mut App, _env: &CapabilityEnv) {
    register_backend(
        app,
        RenderBackend {
            name: NAME,
            priority: PRIORITY,
            renderer,
        },
    );
}

fn renderer() -> Box<dyn Renderer> {
    Box::new(CpuRenderer::new())
}

#[cfg(test)]
mod tests {
    use super::*;
    use lumen_core::render_backend::{RenderBackends, install_offscreen};
    use lumen_core::render_world::SurfaceCapture;
    use lumen_core::traits::FrameTarget;

    /// Installing the capability registers `cpu` at its priority, and the
    /// renderer it registers binds to an offscreen image and paints once
    /// the launch installs it.
    #[test]
    fn the_capability_registers_a_working_cpu_backend() {
        let mut app = App::new();
        install(
            &mut app,
            &CapabilityEnv::new(".", toml::Table::new(), String::new(), false),
        );
        let backends = app.world.resource::<RenderBackends>().clone();
        let [cpu] = backends
            .select(Some(NAME))
            .expect("registered")
            .try_into()
            .expect("one");
        assert_eq!(cpu.priority, PRIORITY);

        let mut renderer = (cpu.renderer)();
        assert!(!renderer.resize(4, 4), "nothing to resize before a target");
        renderer
            .attach(FrameTarget::Offscreen {
                width: 4,
                height: 4,
            })
            .expect("the CPU always starts");
        let mut app = App::new();
        install_offscreen(&mut app, renderer);
        let capture = SurfaceCapture::default();
        capture.request();
        app.render_world.insert_resource(capture.clone());
        app.tick();
        let frame = capture
            .read()
            .expect("the first frame answered the capture");
        assert_eq!(
            (frame.width, frame.height),
            (800, 600),
            "sized to the viewport"
        );
    }
}
