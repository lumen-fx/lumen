//! What an element and the document declare about motion, kept where a
//! subsystem that runs it can read it, and the live stylesheet the cascade
//! resolves against.
//!
//! The cascade resolves `animation-*` declarations like any other property
//! and leaves them here, on the element, as syntax; nothing in the scene runs
//! them. An optional subsystem that does reads them back, along with the
//! `@keyframes` blocks of [`RuntimeStylesheet`] and the [`LastMediaContext`]
//! those blocks are written inside.

use bevy_ecs::prelude::*;
use lumen_core::style_overrides::StyleFields;
use lumen_ir::animation::AnimationDeclsIr;
use lumen_ir::css::{MediaContext, Stylesheet};
use lumen_ir::layout_ir::StateAnimationsIr;

/// The system that re-runs the cascade for a tick's style changes and
/// commits the result onto the elements. A system that reads what the
/// cascade resolved this tick (the animations an element declares) runs
/// after it.
#[derive(bevy_ecs::schedule::SystemSet, Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct Restyle;

/// The combined (UA, skin and app) stylesheet the cascade resolves against,
/// kept live: the restyle re-runs it on a theme or media flip without
/// reading a file, and a hot reload replaces it.
#[derive(Resource, Clone, Default)]
pub struct RuntimeStylesheet(pub Stylesheet);

/// The [`MediaContext`] the primary window's `@media` rules last resolved
/// against.
#[derive(Resource, Clone, Copy, Debug, PartialEq)]
pub struct LastMediaContext(pub MediaContext);

/// An element's `animation` and `animation-*` declarations, as the cascade
/// resolved them. Carried only by an element that declares one.
#[derive(Component, Clone, Debug, Default, PartialEq)]
pub struct AnimationDeclarations(pub AnimationDeclsIr);

/// The `animation-*` declarations an element's state rules set (`:hover {
/// animation: ... }`), each laid over [`AnimationDeclarations`] while the
/// element is in that state. Carried only by an element whose state rules
/// set one.
#[derive(Component, Clone, Debug, Default, PartialEq)]
pub struct StateAnimations(pub StateAnimationsIr);

/// The style fields an element's `!important` declarations set. An
/// animation leaves them alone, as CSS ranks important declarations above
/// animations. Carried only by an element that has one.
#[derive(Component, Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct ImportantFields(pub StyleFields);

/// The held style field a property the cascade files under `name` lives
/// in, for a property a holder can hold.
pub fn style_field_of(name: &str) -> Option<StyleFields> {
    Some(match name {
        "bg" => StyleFields::FILL,
        "opacity" => StyleFields::OPACITY,
        "text-color" => StyleFields::TEXT_COLOR,
        "border"
        | "border-color"
        | "border-top-color"
        | "border-right-color"
        | "border-bottom-color"
        | "border-left-color" => StyleFields::BORDER,
        "shadow" | "box-shadow" => StyleFields::SHADOWS,
        "radius"
        | "border-top-left-radius"
        | "border-top-right-radius"
        | "border-bottom-right-radius"
        | "border-bottom-left-radius" => StyleFields::RADIUS,
        "transform" | "translate" | "rotate" | "scale" | "transform-origin" => {
            StyleFields::TRANSFORM
        }
        _ => return None,
    })
}
