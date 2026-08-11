//! Marker traits identifying backend roles, plus the [`Bindable`] trait declaring a component as a property-bus participant.
//!
//! Concrete backends register systems via a [`crate::app::Plugin`] into the appropriate [`crate::tick::TickStage`]; the marker
//! traits provide a type-level identifier only.

use crate::property_store::PropertyValue;
use bevy_ecs::component::Component;

/// Marker trait implemented by render backends. The accompanying plugin installs the backend as a (possibly `NonSend`) render-world resource and registers systems into [`crate::render_world::RenderStage`].
pub trait Renderer: 'static {}

/// Marker trait implemented by layout engines. Plugins register systems into [`crate::tick::TickStage::LayoutSync`].
pub trait LayoutEngine: Send + Sync {}

/// Marker trait implemented by window backends. Plugins register systems into [`crate::tick::TickStage::Input`].
pub trait WindowBackend: Send + Sync {}

/// Marker trait implemented by accessibility bridges. Plugins register systems into [`crate::tick::TickStage::A11ySync`].
pub trait A11yBackend: Send + Sync {}

/// Marker trait implemented by async-task runtimes.
pub trait Spawn: Send + Sync {}

/// Marker trait implemented by one-shot timer runtimes.
pub trait Timer: Send + Sync {}

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
