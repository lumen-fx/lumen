//! The `render-gpu` capability: this backend in the app's render-backend
//! registry, under the name `gpu`.

use lumen_capability::{CapabilityEnv, Select};
use lumen_core::app::App;
use lumen_core::render_backend::{RenderBackend, register_render_backend};
use lumen_core::traits::Renderer;

use crate::WgpuRenderer;

/// The name `[render] backend` selects this backend by.
pub const NAME: &str = "gpu";

/// Ahead of the CPU backend: an `auto` launch tries the GPU first.
pub const PRIORITY: i32 = 100;

/// A static package carries this backend when `[render] backend` is `gpu`
/// or `auto`, the default.
pub const SELECT: Select = Select::OnConfig {
    key: "render.backend",
    any_of: &["gpu", "auto"],
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
            renderer,
        },
    );
}

fn renderer() -> Box<dyn Renderer> {
    Box::new(WgpuRenderer::new())
}
