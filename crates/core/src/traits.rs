//! Traits identifying backend roles, plus the [`Bindable`] trait declaring a component as a property-bus participant.
//!
//! Concrete backends register systems via a [`crate::app::Plugin`] into the appropriate [`crate::tick::TickStage`]. Most of
//! these are type-level identifiers only; [`SurfaceRenderer`] and [`A11yBackend`] additionally declare what a window backend
//! calls on them each frame, so a window backend can drive any renderer and any accessibility bridge without naming one.

use crate::property_store::PropertyValue;
use bevy_ecs::component::Component;
use bevy_ecs::world::World;
use raw_window_handle::{HasDisplayHandle, HasWindowHandle};
use std::any::Any;
use std::sync::Arc;
use thiserror::Error;

/// Marker trait implemented by render backends. The accompanying plugin installs the backend as a (possibly `NonSend`) render-world resource and registers systems into [`crate::render_world::RenderStage`].
pub trait Renderer: 'static {}

/// A live OS window a [`SurfaceRenderer`] presents into.
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

/// Why a renderer could not bind to a window or put a frame on it.
#[derive(Debug, Error)]
pub enum SurfaceError {
    /// Binding the renderer to the window failed: no adapter, no device,
    /// or no usable surface format.
    #[error("surface init failed: {0}")]
    Init(String),
    /// Encoding or submitting the frame failed.
    #[error("present failed: {0}")]
    Present(String),
    /// A call that needs a surface arrived before [`SurfaceRenderer::attach`].
    #[error("renderer is not attached to a window")]
    Detached,
}

/// What the window backend knows about the frame it is asking for.
#[derive(Clone, Copy, Debug, Default)]
pub struct FrameRequest {
    /// The tick reported that render-relevant state changed. A clear flag
    /// means nothing in the world moved, so the last presented frame is
    /// still correct.
    pub dirty: bool,
    /// The surface was just recreated (resize, DPI change), so whatever
    /// the renderer had buffered is gone and the frame must be redrawn in
    /// full even when the scene is unchanged.
    pub force_full: bool,
}

/// A renderer that presents into an OS window.
///
/// The window backend owns the window, the input, and the event loop. The
/// renderer owns everything between the retained scene and the pixels:
/// scene assembly, encoding caches, damage tracking, and the swap chain.
/// No graphics-API type crosses the boundary, so a window backend
/// compiles without naming one.
///
/// Frames are driven from the render world, which already holds the
/// retained scene, the viewport, the text shaper, and the screenshot
/// channel.
pub trait SurfaceRenderer: Renderer {
    /// Bind to a window and prepare a swap chain for it. Called once the
    /// window exists; calling it again rebinds the renderer.
    fn attach(&mut self, target: Arc<dyn RenderTarget>) -> Result<(), SurfaceError>;

    /// Reconfigure for a new physical size. Returns `true` when the size
    /// actually changed, so callers can drop the relayout and repaint a
    /// duplicate resize event would otherwise force.
    fn resize(&mut self, width: u32, height: u32) -> bool;

    /// Whether [`Self::present`] would put anything new on screen. The
    /// renderer answers, because only it knows what its buffers still
    /// hold and how precisely it can compare this scene against the last
    /// one it painted.
    fn wants_present(&mut self, render_world: &mut World, request: FrameRequest) -> bool;

    /// Encode, submit, and present one frame from the render world.
    fn present(&mut self, render_world: &mut World) -> Result<(), SurfaceError>;

    /// Release the swap chain and everything behind it. The window
    /// backend calls this while the platform connection is still alive,
    /// because tearing a surface down after the display connection closes
    /// crashes some drivers.
    fn detach(&mut self);
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
