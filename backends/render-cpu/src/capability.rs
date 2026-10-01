//! The `render-cpu` capability: this backend in the app's render-backend
//! registry, under the name `cpu`.

use lumen_capability::{CapabilityEnv, Select};
use lumen_core::app::App;
use lumen_core::render_backend::{OffscreenRenderer, RenderBackend, register_render_backend};
use lumen_core::traits::SurfaceRenderer;

use crate::{CpuRenderer, CpuRendererPlugin, CpuSurfaceRenderer};

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
    register_render_backend(
        app,
        RenderBackend {
            name: NAME,
            priority: PRIORITY,
            surface,
            offscreen,
        },
    );
}

fn surface() -> Box<dyn SurfaceRenderer> {
    Box::new(CpuSurfaceRenderer::new())
}

fn offscreen(width: u32, height: u32) -> Result<Box<dyn OffscreenRenderer>, String> {
    Ok(Box::new(CpuRenderer::new(width, height)))
}

impl OffscreenRenderer for CpuRenderer {
    fn install(self: Box<Self>, app: &mut App) {
        app.add_plugin(CpuRendererPlugin::from(*self));
    }
}
