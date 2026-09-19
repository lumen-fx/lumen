//! Drag primitive: threshold + state machine.
//!
//! State per entity tracked via the [`DragState`] component:
//!
//! * `Pending { start }` - pointer pressed on entity, hasn't moved past
//!   threshold yet.
//! * `Active { start, last }` - threshold crossed, drag is in progress.
//!
//! Transitions:
//!
//! ```text
//! (no state)
//!   --(PointerPressed on Hovered)--> Pending
//! Pending
//!   --(PointerMoved, dist > threshold)--> Active + DragStartEvent
//!   --(PointerReleased)--> (no state)
//! Active
//!   --(PointerMoved)--> Active + DragMoveEvent
//!   --(PointerReleased)--> (no state) + DragEndEvent
//! ```
//!
//! Threshold is configured via [`DragConfig`] (default 4 px).
//!
//! Reads `PointerPressed` from `lumen-input::dispatch_clicks` indirectly:
//! we look at entities that gain [`Pressed`] (so the existing dispatch
//! pipeline already filters to the hovered entity).

use bevy_ecs::message::{MessageReader, MessageWriter};
use bevy_ecs::prelude::*;
use glam::Vec2;
use lumen_core::prelude::*;

/// Opt-in: entities with this marker have their [`Transform::absolute`]
/// translated by each [`DragMoveEvent`] delta. Without this, drag events
/// still fire but the entity stays put - apps that want different drag
/// semantics (scrollbars, marquee selection) read DragMove themselves.
#[derive(Component, Clone, Copy, Debug, Default)]
pub struct Draggable;

/// Default pixel threshold before a press graduates to a drag.
pub const DEFAULT_DRAG_THRESHOLD_PX: f32 = 4.0;

/// Tunables for the drag recognizer.
#[derive(Resource, Clone, Copy, Debug)]
pub struct DragConfig {
    /// Pointer distance from press point that triggers `DragStartEvent`.
    pub threshold_px: f32,
}

impl Default for DragConfig {
    fn default() -> Self {
        Self {
            threshold_px: DEFAULT_DRAG_THRESHOLD_PX,
        }
    }
}

/// Per-entity drag state. Attached on Pressed insert, removed on release.
#[derive(Component, Clone, Copy, Debug)]
pub enum DragState {
    /// Press observed; drag not yet started.
    Pending {
        /// Pointer position when the press began.
        start: Vec2,
    },
    /// Drag in progress.
    Active {
        /// Pointer position at press start.
        start: Vec2,
        /// Last reported pointer position; subtracted from next position
        /// to compute `DragMoveEvent.delta`.
        last: Vec2,
    },
}

/// Plugin: installs [`DragConfig`] and registers the three drag systems.
#[derive(Default)]
pub struct DragPlugin {
    /// Initial config; users can mutate the resource at runtime.
    pub config: DragConfig,
}

impl Plugin for DragPlugin {
    fn build(self, app: &mut App) {
        app.world.insert_resource(self.config);
        // attach_drag_pending must run AFTER lumen-input::dispatch_clicks
        // because that's where Pressed is inserted (via Commands; deferred).
        // The .after() pulls in an ApplyDeferred sync point so attach can
        // see the freshly-inserted Pressed component in the same tick.
        app.add_systems(
            TickStage::Systems,
            attach_drag_pending.after(lumen_input::dispatch_clicks),
        );
        app.add_systems(
            TickStage::Systems,
            update_drag_on_move.after(attach_drag_pending),
        );
        app.add_systems(
            TickStage::Systems,
            translate_draggable.after(update_drag_on_move),
        );
        app.add_systems(
            TickStage::Systems,
            release_drag_on_unpress.after(translate_draggable),
        );
    }
}

/// Generic system: every [`DragMoveEvent`] for an entity carrying the
/// [`Draggable`] marker adds the delta to its [`Transform::absolute`].
pub fn translate_draggable(
    mut moves: MessageReader<DragMoveEvent>,
    mut q: Query<&mut Transform, With<Draggable>>,
) {
    for ev in moves.read() {
        if let Ok(mut t) = q.get_mut(ev.entity) {
            t.absolute += ev.delta;
        }
    }
}

/// On Pressed-without-DragState: attach `DragState::Pending` with the
/// current pointer position as the press anchor.
pub fn attach_drag_pending(
    mut commands: Commands,
    pointer: Res<PointerState>,
    just_pressed: Query<Entity, (With<Pressed>, Without<DragState>)>,
) {
    let Some(p) = pointer.position else {
        return;
    };
    for e in &just_pressed {
        commands.entity(e).insert(DragState::Pending { start: p });
    }
}

/// Walk `PointerMoved` events: promote Pending -> Active when distance
/// exceeds threshold; emit `DragMoveEvent` while Active.
pub fn update_drag_on_move(
    cfg: Res<DragConfig>,
    mut moves: MessageReader<PointerMoved>,
    mut q: Query<(Entity, &mut DragState)>,
    mut starts: MessageWriter<DragStartEvent>,
    mut deltas: MessageWriter<DragMoveEvent>,
) {
    let pending: Vec<Vec2> = moves.read().map(|m| m.position).collect();
    if pending.is_empty() {
        return;
    }
    // Use only the latest movement per frame for state transitions;
    // emit one DragMoveEvent per individual move so apps can replay paths.
    let latest = *pending.last().unwrap();

    for (entity, mut state) in &mut q {
        match *state {
            DragState::Pending { start } => {
                if (latest - start).length() >= cfg.threshold_px {
                    *state = DragState::Active {
                        start,
                        last: latest,
                    };
                    starts.write(DragStartEvent {
                        entity,
                        start,
                        position: latest,
                    });
                }
            }
            DragState::Active { start, mut last } => {
                for pos in &pending {
                    let delta = *pos - last;
                    if delta == Vec2::ZERO {
                        continue;
                    }
                    deltas.write(DragMoveEvent {
                        entity,
                        position: *pos,
                        delta,
                    });
                    last = *pos;
                }
                *state = DragState::Active { start, last };
            }
        }
    }
}

/// On Pressed -> not-Pressed: remove `DragState`; if it was `Active`, emit
/// `DragEndEvent` with the final pointer position.
pub fn release_drag_on_unpress(
    mut commands: Commands,
    pointer: Res<PointerState>,
    released: Query<(Entity, &DragState), Without<Pressed>>,
    mut ends: MessageWriter<DragEndEvent>,
) {
    let p = pointer.position.unwrap_or(Vec2::ZERO);
    for (entity, state) in &released {
        if let DragState::Active { .. } = state {
            ends.write(DragEndEvent {
                entity,
                position: p,
            });
        }
        commands.entity(entity).remove::<DragState>();
    }
}
