//! The platform bridge: an AccessKit adapter bound to a winit window.
//!
//! The window backend owns the window and the event loop but knows nothing
//! about AccessKit; it holds a [`lumen_core::traits::A11yBackend`] and
//! calls three methods on it. This module is the implementation behind
//! that trait, and the only place in Lumen where winit and AccessKit meet.
//!
//! ## Threads
//!
//! Assistive technologies live in another process and reach the app
//! through a platform adapter that may call back on any thread (the AT-SPI
//! bus thread on Linux, the UI thread on Windows and macOS). Requests are
//! therefore queued and applied in [`WinitA11yBridge::pump`], on the main
//! thread, right before the tick that reacts to them. Queuing wakes the
//! event loop, so a screen reader clicking a button paints the result
//! immediately instead of waiting for unrelated input.

use crate::{entity_click_point, node_to_entity, sync_a11y_tree_initial, take_pending_tree_update};
use accesskit::{
    Action, ActionData, ActionHandler, ActionRequest, ActivationHandler, DeactivationHandler,
    TreeUpdate,
};
use accesskit_winit::Adapter;
use bevy_ecs::message::Messages;
use bevy_ecs::prelude::*;
use lumen_core::components::{
    A11yContextMenuRequests, A11yScrollIntoViewRequests, A11yState, DirtyA11y, SliderValue,
    TextContent,
};
use lumen_core::input::{ClickEvent, FocusTracker, FocusVisible, Focused, PointerButton};
use lumen_core::traits::A11yBackend;
use std::any::Any;
use std::sync::{Arc, Mutex};
use winit::event::WindowEvent;
use winit::event_loop::ActiveEventLoop;
use winit::window::Window;

/// Nudges a parked event loop so a queued request is serviced this frame.
pub type WakeFn = Arc<dyn Fn() + Send + Sync>;

/// One request from an assistive technology, waiting for the main thread.
enum Pending {
    /// The platform asked for a full tree.
    InitialTree,
    /// The platform asked the app to do something.
    Action(ActionRequest),
}

/// Thread-safe hand-off between the platform adapter's callbacks and the
/// main thread.
#[derive(Clone)]
struct Inbox {
    queue: Arc<Mutex<Vec<Pending>>>,
    wake: WakeFn,
}

impl Inbox {
    fn push(&self, item: Pending) {
        if let Ok(mut queue) = self.queue.lock() {
            queue.push(item);
        }
        (self.wake)();
    }

    fn drain(&self) -> Vec<Pending> {
        match self.queue.lock() {
            Ok(mut queue) => std::mem::take(&mut *queue),
            Err(_) => Vec::new(),
        }
    }
}

impl ActivationHandler for Inbox {
    /// The world lives on the main thread, so the tree cannot be built
    /// here. Queue the request and return `None`; [`WinitA11yBridge::pump`]
    /// builds the full tree and pushes it on the next frame, which is the
    /// asynchronous answer AccessKit documents for exactly this case.
    fn request_initial_tree(&mut self) -> Option<TreeUpdate> {
        self.push(Pending::InitialTree);
        None
    }
}

impl ActionHandler for Inbox {
    fn do_action(&mut self, request: ActionRequest) {
        self.push(Pending::Action(request));
    }
}

impl DeactivationHandler for Inbox {
    /// Nothing to drop: the tree cache is rebuilt from the world whenever
    /// an assistive technology comes back.
    fn deactivate_accessibility(&mut self) {}
}

/// An AccessKit adapter bound to one winit window.
pub struct WinitA11yBridge {
    adapter: Adapter,
    window: Arc<Window>,
    inbox: Inbox,
}

/// Bind AccessKit to a live winit window.
///
/// `wake` is called whenever an assistive technology queues a request, so
/// a parked event loop wakes up and pumps it.
pub fn winit_bridge(
    event_loop: &ActiveEventLoop,
    window: Arc<Window>,
    wake: WakeFn,
) -> Box<dyn A11yBackend> {
    let inbox = Inbox {
        queue: Arc::new(Mutex::new(Vec::new())),
        wake,
    };
    let adapter = Adapter::with_direct_handlers(
        event_loop,
        &window,
        inbox.clone(),
        inbox.clone(),
        inbox.clone(),
    );
    Box::new(WinitA11yBridge {
        adapter,
        window,
        inbox,
    })
}

impl A11yBackend for WinitA11yBridge {
    fn window_event(&mut self, event: &dyn Any) {
        if let Some(event) = event.downcast_ref::<WindowEvent>() {
            self.adapter.process_event(&self.window, event);
        }
    }

    fn pump(&mut self, world: &mut World) {
        for update in apply_requests(world, self.inbox.drain()) {
            self.adapter.update_if_active(|| update);
        }
    }

    fn publish(&mut self, world: &mut World) {
        // The tree is built in `TickStage::A11ySync`; this only forwards
        // it. `None` means nothing accessibility-relevant changed, so the
        // AT-SPI / UIA / NSAccessibility wake-up is skipped.
        if let Some(update) = take_pending_tree_update(world) {
            self.adapter.update_if_active(|| update);
        }
    }
}

/// Apply queued requests to the world, returning the tree updates the
/// platform must be told about afterwards.
///
/// Split from [`WinitA11yBridge::pump`] so the world-side half runs, and
/// is tested, without a window: everything above this line is the platform
/// adapter, everything below is ECS state.
fn apply_requests(world: &mut World, pending: Vec<Pending>) -> Vec<TreeUpdate> {
    let mut updates = Vec::new();
    for request in pending {
        match request {
            Pending::InitialTree => {
                // The handshake arrives before any tick has run
                // `sync_a11y_tree`, so drive a full build here. The update
                // is always produced: the forced build clears the cached
                // focus, which is one of the two conditions the sync skips
                // on, and marks an entity dirty, which is the other.
                // Platform adapters hold a placeholder tree until this
                // lands, so it has to be a full one.
                sync_a11y_tree_initial(world);
                if let Some(update) = take_pending_tree_update(world) {
                    updates.push(update);
                }
            }
            Pending::Action(request) => handle_action(world, &request),
        }
    }
    updates
}

/// Apply an [`accesskit::ActionRequest`] (a screen reader asking to focus
/// or click a node, step a slider, expand a disclosure) to the ECS world.
///
/// Maps the requested `NodeId` back to its `Entity` and dispatches to the
/// matching ECS channel: focus marker swap, click event, slider mutation,
/// expand / collapse marker, scroll request, or context-menu request.
pub fn handle_action(world: &mut World, req: &ActionRequest) {
    let Some(entity) = node_to_entity(req.target_node) else {
        return;
    };
    match req.action {
        Action::Focus => {
            if let Some(prev) = world.resource::<FocusTracker>().0
                && prev != entity
                && world.get_entity(prev).is_ok()
            {
                world.entity_mut(prev).remove::<(Focused, FocusVisible)>();
            }
            if world.get_entity(entity).is_ok() {
                // Assistive-tech focus counts as keyboard-like for the
                // `:focus-visible` heuristic (matches browser behavior).
                world.entity_mut(entity).insert((Focused, FocusVisible));
                world.resource_mut::<FocusTracker>().0 = Some(entity);
            }
        }
        Action::Blur => {
            if let Some(prev) = world.resource::<FocusTracker>().0
                && prev == entity
                && world.get_entity(prev).is_ok()
            {
                world.entity_mut(prev).remove::<(Focused, FocusVisible)>();
                world.resource_mut::<FocusTracker>().0 = None;
            }
        }
        Action::Click => {
            // Use the entity centre point, not `Vec2::ZERO`. Downstream
            // hit-test / hover / scroll consumers route on world-space
            // coordinates.
            let pos = entity_click_point(world, entity);
            if let Some(mut msgs) = world.get_resource_mut::<Messages<ClickEvent>>() {
                msgs.write(ClickEvent {
                    entity,
                    position: pos,
                    button: PointerButton::Primary,
                });
            }
        }
        Action::Increment | Action::Decrement => {
            // Step the SliderValue by its A11yValue.step (or a default of
            // (max-min)/100). Slider primitives reach this through
            // change-detection on SliderValue.
            let dir = if matches!(req.action, Action::Increment) {
                1.0
            } else {
                -1.0
            };
            if let Some(mut sv) = world.get_mut::<SliderValue>(entity) {
                let span = (sv.max - sv.min).abs();
                let step = if span > 0.0 { span / 100.0 } else { 1.0 };
                let next = (sv.value + dir * step).clamp(sv.min, sv.max);
                sv.value = next;
            }
            if world.get_entity(entity).is_ok() {
                world.entity_mut(entity).insert(DirtyA11y);
            }
        }
        Action::SetValue => match &req.data {
            Some(ActionData::NumericValue(v)) => {
                if let Some(mut sv) = world.get_mut::<SliderValue>(entity) {
                    sv.value = (*v as f32).clamp(sv.min, sv.max);
                }
                if world.get_entity(entity).is_ok() {
                    world.entity_mut(entity).insert(DirtyA11y);
                }
            }
            Some(ActionData::Value(s)) => {
                if let Some(mut tc) = world.get_mut::<TextContent>(entity) {
                    tc.0 = s.to_string();
                }
                if world.get_entity(entity).is_ok() {
                    world.entity_mut(entity).insert(DirtyA11y);
                }
            }
            _ => {}
        },
        Action::ReplaceSelectedText => {
            if let Some(ActionData::Value(s)) = &req.data
                && let Some(mut tc) = world.get_mut::<TextContent>(entity)
            {
                tc.0 = s.to_string();
            }
        }
        Action::ScrollIntoView => {
            // Mark the entity for primitive scroll consumers to bring into
            // the visible area on the next tick. Primitives read
            // `A11yScrollIntoViewRequests` from a shared resource owned by
            // lumen-core, so the primitives crate can consume it without
            // depending on any backend.
            world
                .get_resource_or_insert_with::<A11yScrollIntoViewRequests>(Default::default)
                .targets
                .push(entity);
        }
        Action::Expand | Action::Collapse => {
            // Flip the EXPANDED bit on the entity's A11yState so primitives
            // (disclosure triangles, tree items, comboboxes) can react.
            let want = matches!(req.action, Action::Expand);
            if let Some(mut st) = world.get_mut::<A11yState>(entity) {
                if want {
                    st.insert(A11yState::EXPANDED);
                } else {
                    st.remove(A11yState::EXPANDED);
                }
            } else if world.get_entity(entity).is_ok() {
                let mut st = A11yState::default();
                if want {
                    st |= A11yState::EXPANDED;
                }
                world.entity_mut(entity).insert(st);
            }
            if world.get_entity(entity).is_ok() {
                world.entity_mut(entity).insert(DirtyA11y);
            }
        }
        Action::ShowContextMenu => {
            world
                .get_resource_or_insert_with::<A11yContextMenuRequests>(Default::default)
                .targets
                .push(entity);
        }
        _ => {}
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::entity_to_node;
    use accesskit::TreeId;
    use lumen_core::components::Transform;

    fn world_with_focus() -> World {
        let mut world = World::new();
        world.init_resource::<FocusTracker>();
        world
    }

    fn request(entity: Entity, action: Action) -> ActionRequest {
        ActionRequest {
            action,
            target_tree: TreeId::ROOT,
            target_node: entity_to_node(entity),
            data: None,
        }
    }

    /// Focus arriving from a screen reader moves the focus marker and the
    /// tracker, and counts as keyboard-like focus so `:focus-visible`
    /// styling applies - the same state a Tab press would leave behind.
    #[test]
    fn focus_action_moves_focus() {
        let mut world = world_with_focus();
        let first = world.spawn_empty().id();
        let second = world.spawn_empty().id();

        handle_action(&mut world, &request(first, Action::Focus));
        assert_eq!(world.resource::<FocusTracker>().0, Some(first));
        assert!(world.get::<FocusVisible>(first).is_some());

        handle_action(&mut world, &request(second, Action::Focus));
        assert_eq!(world.resource::<FocusTracker>().0, Some(second));
        assert!(world.get::<Focused>(first).is_none());

        handle_action(&mut world, &request(second, Action::Blur));
        assert_eq!(world.resource::<FocusTracker>().0, None);
    }

    /// A click from an assistive technology carries the entity centre, not
    /// the origin, so hit-test and hover consumers route it to the same
    /// place a real pointer click would.
    #[test]
    fn click_action_targets_the_entity_centre() {
        let mut world = world_with_focus();
        world.init_resource::<Messages<ClickEvent>>();
        let entity = world
            .spawn(Transform {
                absolute: glam::Vec2::new(10.0, 20.0),
                size: glam::Vec2::new(40.0, 10.0),
                ..Default::default()
            })
            .id();

        handle_action(&mut world, &request(entity, Action::Click));

        let msgs = world.resource::<Messages<ClickEvent>>();
        let click = msgs
            .iter_current_update_messages()
            .next()
            .copied()
            .expect("click event emitted");
        assert_eq!(click.entity, entity);
        assert_eq!(click.position, glam::Vec2::new(30.0, 25.0));
    }

    /// Stepping a slider clamps to its range and marks the entity dirty so
    /// the next A11ySync pass reports the new value back.
    #[test]
    fn increment_steps_and_clamps_the_slider() {
        let mut world = world_with_focus();
        let entity = world
            .spawn(SliderValue {
                value: 100.0,
                min: 0.0,
                max: 100.0,
                step: None,
            })
            .id();

        handle_action(&mut world, &request(entity, Action::Increment));
        assert_eq!(world.get::<SliderValue>(entity).unwrap().value, 100.0);
        assert!(world.get::<DirtyA11y>(entity).is_some());

        handle_action(&mut world, &request(entity, Action::Decrement));
        assert_eq!(world.get::<SliderValue>(entity).unwrap().value, 99.0);
    }

    /// Expand and collapse flip one state bit, inserting the state
    /// component when the entity does not carry one yet.
    #[test]
    fn expand_and_collapse_flip_the_state_bit() {
        let mut world = world_with_focus();
        let entity = world.spawn_empty().id();

        handle_action(&mut world, &request(entity, Action::Expand));
        assert!(
            world
                .get::<A11yState>(entity)
                .unwrap()
                .contains(A11yState::EXPANDED)
        );

        handle_action(&mut world, &request(entity, Action::Collapse));
        assert!(
            !world
                .get::<A11yState>(entity)
                .unwrap()
                .contains(A11yState::EXPANDED)
        );
    }

    /// Setting a value covers both shapes an assistive technology sends:
    /// a number for a slider and a string for a text control. Both mark
    /// the entity dirty so the next sync reports what the app now holds.
    #[test]
    fn set_value_writes_numbers_to_sliders_and_strings_to_text() {
        let mut world = world_with_focus();
        let slider = world
            .spawn(SliderValue {
                value: 10.0,
                min: 0.0,
                max: 50.0,
                step: None,
            })
            .id();
        let field = world.spawn(TextContent("before".into())).id();

        let mut numeric = request(slider, Action::SetValue);
        numeric.data = Some(ActionData::NumericValue(999.0));
        handle_action(&mut world, &numeric);
        assert_eq!(
            world.get::<SliderValue>(slider).unwrap().value,
            50.0,
            "a value past the maximum clamps rather than escaping the range",
        );
        assert!(world.get::<DirtyA11y>(slider).is_some());

        let mut text = request(field, Action::SetValue);
        text.data = Some(ActionData::Value("after".into()));
        handle_action(&mut world, &text);
        assert_eq!(world.get::<TextContent>(field).unwrap().0, "after");
        assert!(world.get::<DirtyA11y>(field).is_some());

        // A value request carrying nothing changes nothing.
        let empty = request(field, Action::SetValue);
        handle_action(&mut world, &empty);
        assert_eq!(world.get::<TextContent>(field).unwrap().0, "after");
    }

    /// Replacing the selection is how dictation and braille input arrive.
    /// It rewrites the text and, unlike `SetValue`, says nothing about
    /// entities that carry no text.
    #[test]
    fn replacing_selected_text_rewrites_the_field() {
        let mut world = world_with_focus();
        let field = world.spawn(TextContent("old".into())).id();
        let bare = world.spawn_empty().id();

        let mut replace = request(field, Action::ReplaceSelectedText);
        replace.data = Some(ActionData::Value("new".into()));
        handle_action(&mut world, &replace);
        assert_eq!(world.get::<TextContent>(field).unwrap().0, "new");

        let mut on_bare = request(bare, Action::ReplaceSelectedText);
        on_bare.data = Some(ActionData::Value("ignored".into()));
        handle_action(&mut world, &on_bare);
        assert!(world.get::<TextContent>(bare).is_none());
    }

    /// Scroll-into-view and context-menu requests land in the shared
    /// core resources the primitives crate drains, rather than in anything
    /// backend-specific.
    #[test]
    fn scroll_and_context_menu_requests_queue_for_primitives() {
        let mut world = world_with_focus();
        let entity = world.spawn_empty().id();

        handle_action(&mut world, &request(entity, Action::ScrollIntoView));
        handle_action(&mut world, &request(entity, Action::ShowContextMenu));

        assert_eq!(
            world.resource::<A11yScrollIntoViewRequests>().targets,
            vec![entity]
        );
        assert_eq!(
            world.resource::<A11yContextMenuRequests>().targets,
            vec![entity]
        );
    }

    /// An inbox that counts the wakes it sends, standing in for the event
    /// loop the real one nudges.
    fn counting_inbox() -> (Inbox, Arc<std::sync::atomic::AtomicUsize>) {
        let wakes = Arc::new(std::sync::atomic::AtomicUsize::new(0));
        let counter = wakes.clone();
        let inbox = Inbox {
            queue: Arc::new(Mutex::new(Vec::new())),
            wake: Arc::new(move || {
                counter.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
            }),
        };
        (inbox, wakes)
    }

    /// Assistive technologies call in on their own threads, so every
    /// handler queues its request and wakes the loop rather than touching
    /// the world. The initial-tree ask answers `None` on the spot and is
    /// honoured from the queue instead, which is the asynchronous path
    /// AccessKit documents for an app that cannot build a tree off-thread.
    #[test]
    fn platform_callbacks_queue_and_wake_instead_of_touching_the_world() {
        let (mut inbox, wakes) = counting_inbox();
        let entity = Entity::from_raw_u32(7).expect("valid entity id");

        assert!(inbox.request_initial_tree().is_none());
        inbox.do_action(request(entity, Action::Click));
        inbox.deactivate_accessibility();

        assert_eq!(wakes.load(std::sync::atomic::Ordering::Relaxed), 2);
        let drained = inbox.drain();
        assert_eq!(drained.len(), 2);
        assert!(matches!(drained[0], Pending::InitialTree));
        assert!(matches!(drained[1], Pending::Action(_)));
        // Draining empties the queue, so the next frame does not replay it.
        assert!(inbox.drain().is_empty());
    }

    /// Draining applies the actions to the world and hands back the tree
    /// updates the platform still needs, keeping the two halves of a pump
    /// separable: world mutation here, adapter calls at the call site.
    #[test]
    fn draining_applies_actions_and_returns_the_handshake_tree() {
        use lumen_core::components::Transform;

        let mut world = world_with_focus();
        let entity = world
            .spawn(Transform {
                absolute: glam::Vec2::ZERO,
                size: glam::Vec2::new(30.0, 30.0),
                ..Default::default()
            })
            .id();

        let updates = apply_requests(
            &mut world,
            vec![
                Pending::Action(request(entity, Action::Focus)),
                Pending::InitialTree,
            ],
        );

        assert_eq!(world.resource::<FocusTracker>().0, Some(entity));
        assert_eq!(updates.len(), 1, "only the handshake produces a tree");
        assert!(!updates[0].nodes.is_empty());
    }

    /// The initial-tree handshake always has an answer. It arrives before
    /// the first tick, and platform adapters sit on a placeholder tree
    /// until a full one lands, so the forced build must publish one even
    /// on a world that has never synced and has nothing focused.
    #[test]
    fn the_initial_handshake_publishes_a_full_tree() {
        use lumen_core::components::{A11yLabel, Transform};

        let mut world = world_with_focus();
        world.spawn((
            Transform {
                absolute: glam::Vec2::ZERO,
                size: glam::Vec2::new(80.0, 24.0),
                ..Default::default()
            },
            A11yLabel("Save".into()),
        ));

        sync_a11y_tree_initial(&mut world);
        let update = take_pending_tree_update(&mut world).expect("handshake publishes a tree");
        assert!(
            !update.nodes.is_empty(),
            "the published tree must carry nodes, not just a focus pointer",
        );
    }

    /// A request naming the synthetic root node has no entity behind it
    /// and must be dropped rather than mapped onto entity 0.
    #[test]
    fn root_node_requests_are_ignored() {
        let mut world = world_with_focus();
        let req = ActionRequest {
            action: Action::Focus,
            target_tree: TreeId::ROOT,
            target_node: crate::ROOT_NODE,
            data: None,
        };
        handle_action(&mut world, &req);
        assert_eq!(world.resource::<FocusTracker>().0, None);
    }
}
