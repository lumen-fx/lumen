//! The fill-holding steps the state-swap widgets share.
//!
//! A widget that paints a state over an element's own fill (a checked
//! box, a selected tab, a hover tint) holds that fill in [`StyleOverrides`]
//! at its layer, rather than writing it over the cascade's, so a restyle
//! keeps it and letting go lands on the fill the element's styles give it
//! then.

use bevy_ecs::prelude::Mut;
use lumen_core::components::{Color, Fill, Visuals};
use lumen_core::style_overrides::{Live, StyleFields, StyleOverrides, StyleOwner, StyleValues};

/// The solid fill under `O`'s layer: what the element paints with `O`
/// let go. `None` for a gradient or no fill.
pub(crate) fn fill_below<O: StyleOwner>(
    overrides: &StyleOverrides,
    vis: &Visuals,
) -> Option<Color> {
    let live = Live {
        visuals: Some(vis),
        ..Live::default()
    };
    overrides
        .below::<O>(StyleFields::FILL, &live)
        .fill
        .flatten()
        .as_ref()
        .and_then(Fill::as_solid)
}

/// Hold `color` as `O`'s fill and paint what the element shows with it.
/// Nothing is written when `O` already holds that colour.
pub(crate) fn hold_fill<O: StyleOwner>(
    overrides: &mut StyleOverrides,
    vis: &mut Mut<Visuals>,
    color: Color,
) {
    if overrides.held_by::<O>().and_then(|v| v.fill.as_ref()) == Some(&Some(Fill::Solid(color))) {
        return;
    }
    let live = Live {
        visuals: Some(vis),
        ..Live::default()
    };
    let held = StyleValues {
        fill: Some(Some(Fill::Solid(color))),
        ..StyleValues::default()
    };
    overrides.hold::<O>(held, &live).write_visuals(vis);
}

/// Let go of `O`'s fill, if it holds one, and paint what the element shows
/// without it.
pub(crate) fn release_fill<O: StyleOwner>(overrides: &mut StyleOverrides, vis: &mut Mut<Visuals>) {
    if overrides.held_by::<O>().is_some() {
        overrides.release::<O>().write_visuals(vis);
    }
}
