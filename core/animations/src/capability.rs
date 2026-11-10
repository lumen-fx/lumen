//! The `animations` capability: the keyframe engine, for an app that
//! declares an animation.

use lumen_capability::{CapabilityEnv, Select};
use lumen_core::app::App;
use lumen_script::ScriptFnAppExt;
use std::sync::Arc;

/// What a static package looks for in the app's sources before it carries
/// the keyframe engine: a `@keyframes` block, an `animation` property, or a
/// script calling `animate(`. The scan errs toward yes.
pub const SELECT: Select = Select::OnUse(&["@keyframes", "animation", "animate("]);

/// Install the subsystem. What the capability crate beside this one
/// registers.
pub fn install(app: &mut App, _env: &CapabilityEnv) {
    app.add_plugin(crate::AnimationsPlugin);
}

/// Register `animations::animate` and its controls alone, for a compile to
/// declare them to the program.
pub fn declare(app: &mut App) {
    app.add_script_fns(crate::script::script_fns(Arc::default()));
}
