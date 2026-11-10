//! CSS keyframe animations on the desktop: `@keyframes` and the
//! `animation-*` properties.
//!
//! The cascade resolves an element's `animation` declarations like any other
//! property and leaves them on the element as syntax; the stylesheet keeps
//! its `@keyframes` blocks as authored. This crate runs them: it starts an
//! animation when its name appears on an element and cancels it when the
//! name goes away, samples it each tick through the shared value math in
//! [`lumen_core::motion`], and holds the result in
//! [`StyleOverrides`](lumen_core::style_overrides::StyleOverrides) at the
//! rank CSS gives animations. It raises `animationstart`,
//! `animationiteration`, `animationend` and `animationcancel` on the element
//! as custom events.
//!
//! An animation asks for frames only while it moves on screen: a delay, a
//! `steps()` curve and an element scrolled out of view wake the loop at the
//! instant something changes instead, and a paused or finished animation
//! asks for nothing.
//!
//! The crate is an engine capability. An app whose sources never mention an
//! animation is packaged without it, and the core names none of it.

#![warn(missing_docs)]

#[cfg(target_arch = "wasm32")]
pub mod browser;
pub mod capability;
pub mod driver;
pub mod keyframes;
pub mod script;
pub mod timing;

use bevy_ecs::prelude::*;
use lumen_core::prelude::{App, Plugin, TickStage};

pub use driver::{AnimationHolder, Running, RunningAnimations};
pub use keyframes::KeyframeSet;

/// Ordering label for the animation systems.
#[derive(bevy_ecs::schedule::SystemSet, Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct AnimationSet;

/// Plugin: the keyframe set and the animation driver.
pub struct AnimationsPlugin;

impl Plugin for AnimationsPlugin {
    fn build(self, app: &mut App) {
        app.world.init_resource::<KeyframeSet>();
        if !app
            .world
            .contains_resource::<bevy_ecs::message::Messages<lumen_script::CustomEvent>>()
        {
            bevy_ecs::message::MessageRegistry::register_message::<lumen_script::CustomEvent>(
                &mut app.world,
            );
        }
        // `animations::animate` and its controls, in every script host and
        // through `lumen_call`.
        let bus = script::ScriptAnimations::default();
        lumen_script::ScriptFnAppExt::add_script_fns(app, script::script_fns(bus.0.clone()));
        app.world.insert_resource(bus);
        // After the cascade commits this tick's declarations and the state
        // rules settle, so a class or a hover that adds a name starts it the
        // same tick; within the systems stage, so the values it holds lay
        // out and paint this frame.
        app.add_systems(
            TickStage::Systems,
            (
                driver::sync_keyframes,
                driver::apply_script_requests,
                driver::reconcile_animations,
                driver::sample_animations,
            )
                .chain()
                .in_set(AnimationSet)
                .after(lumen_scene::style_sheet::Restyle)
                .after(lumen_primitives::state_style::apply_state_visuals),
        );
    }
}
