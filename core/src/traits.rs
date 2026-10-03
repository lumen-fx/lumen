//! Traits identifying backend roles, plus the [`Bindable`] trait declaring a component as a property-bus participant.
//!
//! Concrete backends register systems via a [`crate::app::Plugin`] into the appropriate [`crate::tick::TickStage`].
//! [`Renderer`] and [`A11yBackend`] declare what a caller drives them with each frame, so a window backend can drive
//! any renderer and any accessibility bridge without naming one; [`LayoutEngine`] and [`WindowBackend`] are
//! type-level identifiers only.

use crate::property_store::PropertyValue;
use bevy_ecs::component::Component;
use bevy_ecs::world::World;
use raw_window_handle::{HasDisplayHandle, HasWindowHandle};
use std::any::Any;
use std::sync::Arc;
use thiserror::Error;

/// A live OS window a [`Renderer`] presents into.
///
/// The window backend owns the window and shares it as an
/// `Arc<dyn RenderTarget>`. The renderer keeps that handle for as long as
/// it holds a surface, so the window outlives every GPU object bound to
/// it. The two handle traits are the platform-neutral vocabulary every
/// desktop graphics API already speaks.
pub trait RenderTarget: HasWindowHandle + HasDisplayHandle + Send + Sync + 'static {
    /// Drawable size in physical pixels.
    fn physical_size(&self) -> (u32, u32);
}

/// Where a [`Renderer`] puts its frames.
#[derive(Clone)]
pub enum FrameTarget {
    /// An OS window: each frame is presented on screen.
    Window(Arc<dyn RenderTarget>),
    /// An image of `width` x `height` physical pixels that never reaches a
    /// screen. Headless runs, screenshots, and tests read it back through
    /// [`crate::render_world::SurfaceCapture`].
    Offscreen {
        /// Width in physical pixels.
        width: u32,
        /// Height in physical pixels.
        height: u32,
    },
}

impl std::fmt::Debug for FrameTarget {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Window(window) => f
                .debug_tuple("Window")
                .field(&window.physical_size())
                .finish(),
            Self::Offscreen { width, height } => f
                .debug_struct("Offscreen")
                .field("width", width)
                .field("height", height)
                .finish(),
        }
    }
}

/// Why a renderer could not bind to its target or put a frame on it.
#[derive(Debug, Error)]
pub enum RenderError {
    /// Binding the renderer to its target failed: no adapter, no device,
    /// or no usable surface format.
    #[error("renderer init failed: {0}")]
    Init(String),
    /// Encoding, submitting, or presenting the frame failed.
    #[error("present failed: {0}")]
    Present(String),
    /// A call that needs a target arrived before [`Renderer::attach`].
    #[error("renderer is not attached to a target")]
    Detached,
}

/// What the caller knows about the frame it is asking for.
#[derive(Clone, Copy, Debug, Default)]
pub struct FrameRequest {
    /// The tick reported that render-relevant state changed. A clear flag
    /// means nothing in the world moved, so the last presented frame is
    /// still correct.
    pub dirty: bool,
    /// The target was just recreated (resize, DPI change), so whatever
    /// the renderer had buffered is gone and the frame must be redrawn in
    /// full even when the scene is unchanged.
    pub force_full: bool,
}

/// A render backend's renderer: everything between the retained scene and
/// the pixels.
///
/// The renderer owns scene assembly, encoding caches, damage tracking, and
/// whatever its target needs (a swap chain, a window buffer, an offscreen
/// image). No graphics-API type crosses the boundary, so a caller compiles
/// without naming one. Frames are driven from the render world, which
/// already holds the retained scene, the viewport, the text shaper, and the
/// screenshot channel.
///
/// Construction does no work. [`Self::attach`] binds the renderer to a
/// [`FrameTarget`]: a window backend attaches a window once it exists, and
/// a headless launch attaches an offscreen image and hands the renderer to
/// [`crate::render_backend::install_offscreen`], whose render-world system
/// drives it each frame. A pending [`crate::render_world::SurfaceCapture`]
/// request is answered from the frame [`Self::present`] puts up, on either
/// kind of target.
pub trait Renderer: Send + 'static {
    /// Bind to `target`. Calling it again rebinds the renderer. A
    /// [`RenderError::Init`] failure is what lets a launch try the next
    /// backend.
    fn attach(&mut self, target: FrameTarget) -> Result<(), RenderError>;

    /// Reconfigure for a new physical size. Returns `true` when the size
    /// actually changed, so callers can drop the relayout and repaint a
    /// duplicate resize event would otherwise force.
    fn resize(&mut self, width: u32, height: u32) -> bool;

    /// Whether [`Self::present`] would put anything new up. The renderer
    /// answers, because only it knows what its buffers still hold and how
    /// precisely it can compare this scene against the last one it painted.
    fn wants_present(&mut self, render_world: &mut World, request: FrameRequest) -> bool;

    /// Paint, submit, and present one frame from the render world.
    fn present(&mut self, render_world: &mut World) -> Result<(), RenderError>;

    /// Release the target and everything behind it. A window backend calls
    /// this while the platform connection is still alive, because tearing a
    /// surface down after the display connection closes crashes some
    /// drivers.
    fn detach(&mut self);
}

/// A boxed renderer is a renderer, so a launch can hand the one the
/// registry built to anything generic over [`Renderer`].
impl<R: Renderer + ?Sized> Renderer for Box<R> {
    fn attach(&mut self, target: FrameTarget) -> Result<(), RenderError> {
        (**self).attach(target)
    }

    fn resize(&mut self, width: u32, height: u32) -> bool {
        (**self).resize(width, height)
    }

    fn wants_present(&mut self, render_world: &mut World, request: FrameRequest) -> bool {
        (**self).wants_present(render_world, request)
    }

    fn present(&mut self, render_world: &mut World) -> Result<(), RenderError> {
        (**self).present(render_world)
    }

    fn detach(&mut self) {
        (**self).detach();
    }
}

/// Marker trait implemented by layout engines. Plugins register systems into [`crate::tick::TickStage::LayoutSync`].
pub trait LayoutEngine: Send + Sync {}

/// Marker trait implemented by window backends. Plugins register systems into [`crate::tick::TickStage::Input`].
pub trait WindowBackend: Send + Sync {}

/// The bridge between the ECS world and the platform accessibility API.
///
/// The world-side half (walking the tree, translating roles and states)
/// runs as a system in [`crate::tick::TickStage::A11ySync`] and leaves a
/// pending update behind; the three methods here are what a window
/// backend calls to keep the platform in step with it. Assistive
/// technologies deliver their requests on their own threads, so an
/// implementation queues them and applies the queue in [`Self::pump`], on
/// the main thread, before the tick that reacts to them.
pub trait A11yBackend: 'static {
    /// Feed a platform window event to the bridge, before the window
    /// backend handles it. The event is the window backend's own type; an
    /// implementation downcasts it and ignores what it does not know.
    fn window_event(&mut self, event: &dyn Any);

    /// Apply queued assistive-technology requests (focus, click, value
    /// changes, scroll-into-view) to the world.
    fn pump(&mut self, world: &mut World);

    /// Publish the tree update the A11ySync stage built, if one is
    /// pending and an assistive technology is listening.
    fn publish(&mut self, world: &mut World);
}

// The async seam carries value types (boxed futures, the service resources
// that hold the selected backend), so it lives in [`crate::task`]. Re-exported
// here because backends implement it alongside the traits above.
pub use crate::task::{Spawn, Timer};

/// Declares that a [`Component`] participates in the entity-property bus exposed by [`crate::property_store::PropertyStore`].
///
/// The intent is to collapse the `BindText` / `BindChecked` / `BindValue` zoo onto a single, type-erased property
/// pipeline. The trait defines the shape; there is no registration call on [`crate::app::App`] yet, so implementing
/// it does not wire anything up, and no component in the workspace implements it yet. The shape it is designed
/// for is [`crate::components::TextContent`] (`NAME = "text"`, `Value = Arc<str>`).
pub trait Bindable: Component {
    /// Bus name for this component. Markup `bind-<NAME>="signal"` wires `PropertyKey::Entity(e, NAME)` to `PropertyKey::Global("signal")`.
    const NAME: &'static str;

    /// Typed value carried over the bus. Must round-trip through [`PropertyValue`].
    type Value: Into<PropertyValue> + From<PropertyValue>;

    /// Reads the component into its bus value.
    fn read(&self) -> Self::Value;

    /// Writes a bus value into the component.
    fn write(&mut self, v: Self::Value);
}

#[cfg(test)]
mod tests {
    use super::{FrameRequest, FrameTarget, RenderError};

    /// The default frame request asks for nothing. A window backend fills
    /// it in from what the tick reported, so a default that leaned the
    /// other way would repaint every frame of an idle app.
    #[test]
    fn the_default_frame_request_asks_for_nothing() {
        let request = FrameRequest::default();
        assert!(!request.dirty);
        assert!(!request.force_full);
    }

    /// Render errors are printed to a person whose app just failed to
    /// start or failed to draw, so each one has to say which of the two
    /// happened and carry the backend's own reason.
    #[test]
    fn render_errors_say_what_failed() {
        assert_eq!(
            RenderError::Init("no adapter".into()).to_string(),
            "renderer init failed: no adapter",
        );
        assert_eq!(
            RenderError::Present("device lost".into()).to_string(),
            "present failed: device lost",
        );
        assert_eq!(
            RenderError::Detached.to_string(),
            "renderer is not attached to a target",
        );
    }

    /// An offscreen target prints its size, for the message a launch
    /// writes when it cannot bind one.
    #[test]
    fn an_offscreen_target_prints_its_size() {
        let target = FrameTarget::Offscreen {
            width: 320,
            height: 200,
        };
        let printed = format!("{target:?}");
        assert!(
            printed.contains("320") && printed.contains("200"),
            "{printed}"
        );
    }
}
