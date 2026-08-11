//! One-time idle-baseline capture shared by the state-swap widgets.

use bevy_ecs::prelude::{Commands, Component, Entity};

/// Capture an idle/resting baseline into component `T` exactly once,
/// then read it back on every later tick.
///
/// Widgets that swap a visual property (fill, colour, ...) toward an
/// interaction state need the *resting* value to return to. That resting
/// value is only observable on the very first sync, before the first
/// swap can overwrite it, and it is the current live value even for a
/// widget spawned already in the active state. This records `current`
/// into the `T` baseline component the first time through (when `stored`
/// is `None`) and thereafter reads the stored copy back via `project`,
/// so the captured baseline survives later mutation of the live visual.
///
/// - `stored`: the baseline component already attached, if any.
/// - `current`: the live value seen this tick (the resting value on the
///   first, capturing call).
/// - `project`: read the stored value out of the baseline component.
/// - `wrap`: build the baseline component to insert on first capture.
pub fn capture_baseline<T, V>(
    commands: &mut Commands,
    entity: Entity,
    stored: Option<&T>,
    current: V,
    project: impl FnOnce(&T) -> V,
    wrap: impl FnOnce(V) -> T,
) -> V
where
    T: Component,
    V: Clone,
{
    match stored {
        Some(b) => project(b),
        None => {
            commands.entity(entity).insert(wrap(current.clone()));
            current
        }
    }
}
