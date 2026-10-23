//! Pointer lock: who may hold it, and when it ends.
//!
//! An element asks with a [`PointerLockAsk`]; this module grants it the way
//! a browser does, so a stuck producer or a runaway script cannot trap the
//! user's pointer:
//!
//! - only the element that has keyboard focus gets the lock, and only during
//!   or just after a press of a key or a pointer button
//!   ([`ACTIVATION_WINDOW`]);
//! - Escape always releases it, as does the window losing focus or the
//!   element going away.
//!
//! A granted ask becomes the [`PointerLockRequest`] the window backend
//! applies, and the backend reports what took effect in
//! [`PointerLockState`]. A run with no backend that applies locks
//! ([`PointerLockBackend`] absent: a headless run) takes the request as the
//! state at once, so tests drive it. Every change of the state is announced
//! with a [`PointerLockChanged`]; a refused ask with a [`PointerLockError`].

use bevy_ecs::message::{MessageReader, MessageWriter};
use bevy_ecs::prelude::*;
use lumen_core::input::{
    FocusTracker, Key, KeyPressed, NamedKey, PointerLockAsk, PointerLockBackend,
    PointerLockChanged, PointerLockError, PointerLockRequest, PointerLockState, PointerPressed,
    WindowFocused,
};
use lumen_core::time::{Duration, Instant};

/// How long after a press an element may still take the lock: the press
/// that asked for it, handled a tick or two later, counts.
pub const ACTIVATION_WINDOW: Duration = Duration::from_millis(1000);

/// When the user last pressed a key or a pointer button.
#[derive(Resource, Clone, Copy, Debug, Default)]
pub struct UserActivation {
    /// The latest press, if any.
    pub last: Option<Instant>,
}

impl UserActivation {
    /// Whether a press happened within [`ACTIVATION_WINDOW`] of `now`.
    pub fn is_active(&self, now: Instant) -> bool {
        self.last
            .is_some_and(|at| now.saturating_duration_since(at) <= ACTIVATION_WINDOW)
    }
}

/// Make sure the messages and resources the pointer-lock systems use exist,
/// for an app assembled without the core's defaults (a test world).
pub fn install(app: &mut lumen_core::app::App) {
    use bevy_ecs::message::{MessageRegistry, Messages};
    fn message<T: bevy_ecs::message::Message>(world: &mut World) {
        if !world.contains_resource::<Messages<T>>() {
            MessageRegistry::register_message::<T>(world);
        }
    }
    let world = &mut app.world;
    message::<PointerLockAsk>(world);
    message::<PointerLockChanged>(world);
    message::<PointerLockError>(world);
    world.init_resource::<PointerLockRequest>();
    world.init_resource::<PointerLockState>();
    world.init_resource::<UserActivation>();
}

/// Grant, refuse and end pointer locks. Runs once a tick, after focus moved
/// for this tick's presses.
pub fn grant_pointer_lock(
    mut asks: MessageReader<PointerLockAsk>,
    (mut presses, mut keys, window_focus): (
        MessageReader<PointerPressed>,
        MessageReader<KeyPressed>,
        Option<MessageReader<WindowFocused>>,
    ),
    mut activation: ResMut<UserActivation>,
    focus: Res<FocusTracker>,
    mut request: ResMut<PointerLockRequest>,
    mut errors: MessageWriter<PointerLockError>,
    alive: Query<()>,
) {
    let now = Instant::now();
    if presses.read().last().is_some() {
        activation.last = Some(now);
    }
    let mut escape = false;
    for key in keys.read() {
        activation.last = Some(now);
        escape |= matches!(key.key, Key::Named(NamedKey::Escape));
    }
    let lost_focus = window_focus.is_some_and(|mut w| w.read().any(|f| !f.focused));
    for ask in asks.read() {
        match ask.entity {
            None => request.owner = None,
            Some(entity) => {
                let focused = focus.0 == Some(entity);
                if focused && (activation.is_active(now) || request.owner == Some(entity)) {
                    request.owner = Some(entity);
                } else {
                    errors.write(PointerLockError { entity });
                }
            }
        }
    }
    let gone = request.owner.is_some_and(|e| alive.get(e).is_err());
    if (escape || lost_focus || gone) && request.owner.is_some() {
        request.owner = None;
    }
}

/// Take the request as the state when nothing else applies it, and announce
/// every change of the state.
pub fn settle_pointer_lock(
    backend: Option<Res<PointerLockBackend>>,
    request: Res<PointerLockRequest>,
    mut state: ResMut<PointerLockState>,
    mut previous: Local<Option<Entity>>,
    mut changes: MessageWriter<PointerLockChanged>,
) {
    if backend.is_none() && state.owner != request.owner {
        state.owner = request.owner;
    }
    if state.owner != *previous {
        changes.write(PointerLockChanged {
            owner: state.owner,
            previous: *previous,
        });
        *previous = state.owner;
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use bevy_ecs::message::Messages;
    use bevy_ecs::schedule::Schedule;
    use lumen_core::input::{KeyCode, Modifiers};

    /// The two systems as the app runs them, keeping their state between
    /// runs.
    struct Systems(Schedule);

    fn world() -> (World, Systems) {
        let mut world = World::new();
        world.init_resource::<Messages<PointerLockAsk>>();
        world.init_resource::<Messages<PointerPressed>>();
        world.init_resource::<Messages<KeyPressed>>();
        world.init_resource::<Messages<WindowFocused>>();
        world.init_resource::<Messages<PointerLockError>>();
        world.init_resource::<Messages<PointerLockChanged>>();
        world.init_resource::<UserActivation>();
        world.init_resource::<FocusTracker>();
        world.init_resource::<PointerLockRequest>();
        world.init_resource::<PointerLockState>();
        let mut schedule = Schedule::default();
        schedule.add_systems((grant_pointer_lock, settle_pointer_lock).chain());
        (world, Systems(schedule))
    }

    fn run(
        world: &mut World,
        systems: &mut Systems,
    ) -> (Vec<PointerLockChanged>, Vec<PointerLockError>) {
        systems.0.run(world);
        let changes = world
            .resource_mut::<Messages<PointerLockChanged>>()
            .drain()
            .collect();
        let errors = world
            .resource_mut::<Messages<PointerLockError>>()
            .drain()
            .collect();
        world.resource_mut::<Messages<PointerLockAsk>>().clear();
        world.resource_mut::<Messages<PointerPressed>>().clear();
        world.resource_mut::<Messages<KeyPressed>>().clear();
        world.resource_mut::<Messages<WindowFocused>>().clear();
        (changes, errors)
    }

    fn escape() -> KeyPressed {
        KeyPressed {
            key: Key::Named(NamedKey::Escape),
            code: KeyCode::Escape,
            modifiers: Modifiers::default(),
            repeat: false,
            time_stamp: Instant::now(),
        }
    }

    /// A focused element that was just pressed gets the lock; one without
    /// focus, or asked with no press before it, is refused.
    #[test]
    fn only_a_focused_element_after_a_press_takes_the_lock() {
        let (mut world, mut systems) = world();
        let a = world.spawn_empty().id();
        let b = world.spawn_empty().id();
        world.resource_mut::<FocusTracker>().0 = Some(a);

        world.write_message(PointerLockAsk { entity: Some(a) });
        let (_, errors) = run(&mut world, &mut systems);
        assert_eq!(errors, vec![PointerLockError { entity: a }], "no press yet");

        world.write_message(PointerPressed::default());
        world.write_message(PointerLockAsk { entity: Some(b) });
        world.write_message(PointerLockAsk { entity: Some(a) });
        let (changes, errors) = run(&mut world, &mut systems);
        assert_eq!(
            errors,
            vec![PointerLockError { entity: b }],
            "b has no focus"
        );
        assert_eq!(
            changes,
            vec![PointerLockChanged {
                owner: Some(a),
                previous: None
            }]
        );
        assert_eq!(world.resource::<PointerLockState>().owner, Some(a));
    }

    /// Escape, the window losing focus, an exit ask and the element going
    /// away each end the lock.
    #[test]
    fn the_lock_ends_on_escape_focus_loss_exit_and_despawn() {
        let (mut world, mut systems) = world();
        let a = world.spawn_empty().id();
        world.resource_mut::<FocusTracker>().0 = Some(a);
        let lock = |world: &mut World, systems: &mut Systems| {
            world.write_message(PointerPressed::default());
            world.write_message(PointerLockAsk { entity: Some(a) });
            run(world, systems);
            assert_eq!(world.resource::<PointerLockState>().owner, Some(a));
        };

        lock(&mut world, &mut systems);
        world.write_message(escape());
        let (changes, _) = run(&mut world, &mut systems);
        assert_eq!(changes[0].owner, None, "escape releases");

        lock(&mut world, &mut systems);
        let window = world.spawn_empty().id();
        world.write_message(WindowFocused {
            window,
            focused: false,
        });
        run(&mut world, &mut systems);
        assert_eq!(world.resource::<PointerLockState>().owner, None, "blur");

        lock(&mut world, &mut systems);
        world.write_message(PointerLockAsk { entity: None });
        run(&mut world, &mut systems);
        assert_eq!(world.resource::<PointerLockState>().owner, None, "exit");

        lock(&mut world, &mut systems);
        world.despawn(a);
        run(&mut world, &mut systems);
        assert_eq!(world.resource::<PointerLockState>().owner, None, "gone");
    }

    /// With a backend that applies locks, the request waits for it.
    #[test]
    fn a_backend_applies_the_request_itself() {
        let (mut world, mut systems) = world();
        world.insert_resource(PointerLockBackend);
        let a = world.spawn_empty().id();
        world.resource_mut::<FocusTracker>().0 = Some(a);
        world.write_message(PointerPressed::default());
        world.write_message(PointerLockAsk { entity: Some(a) });
        let (changes, _) = run(&mut world, &mut systems);
        assert!(changes.is_empty());
        assert_eq!(world.resource::<PointerLockRequest>().owner, Some(a));
        assert_eq!(world.resource::<PointerLockState>().owner, None);
    }
}
