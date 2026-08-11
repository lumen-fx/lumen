//! Bounded command queue for off-main-thread mutations of ECS state.
//!
//! - Capacity defaults to [`DEFAULT_QUEUE_CAPACITY`] (8192) and is configurable via [`CommandQueue::with_capacity`].
//! - Overflow drops the command, logs, and emits a [`CommandQueueOverflow`] message.
//! - The [`TickStage::CommandDrain`](crate::tick::TickStage::CommandDrain) stage drains the queue on the main thread between `Input` and `Systems`.

use crate::property_store::{PropertyKey, PropertyStore, PropertyValue};
use bevy_ecs::prelude::*;
use crossbeam_channel::{Receiver, Sender, TrySendError, bounded};
use std::any::{Any, TypeId};
use std::collections::HashMap;
use std::sync::Arc;

/// Default queue capacity. Override via [`CommandQueue::with_capacity`].
pub const DEFAULT_QUEUE_CAPACITY: usize = 8192;

/// A deferred command applied during [`TickStage::CommandDrain`](crate::tick::TickStage::CommandDrain).
pub enum Command {
    /// Carries an asynchronously decoded asset payload.
    AssetLoaded {
        /// Identifier supplied by the requester when the asset was requested.
        callback_id: u64,
        /// Decoded payload (image bytes, etc.) typed by the consumer.
        payload: Box<dyn Any + Send>,
    },
    /// Carries an HTTP response payload.
    NetworkResponse {
        /// Identifier supplied at request time.
        callback_id: u64,
        /// HTTP status code.
        status: u16,
        /// Response body bytes.
        body: Vec<u8>,
    },
    /// Carries a script-produced state mutation.
    ScriptUpdate(Box<dyn Any + Send>),
    /// Writes a typed value into the [`PropertyStore`].
    ///
    /// Drained in [`TickStage::CommandDrain`](crate::tick::TickStage::CommandDrain) via [`apply_property_commands`].
    /// The per-`App` queue is the intended home for external property writes, but the global `EXTERNAL_TX` /
    /// `EXTERNAL_RX` ring in `property_store` is still what the C-ABI crate and the async tasks use; this variant has
    /// no producer yet.
    SetProperty {
        /// Target property key.
        key: PropertyKey,
        /// New value to write.
        value: PropertyValue,
    },
    /// Strongly-typed custom command, dispatched via [`CommandRegistry`].
    ///
    /// Replaces blind `Custom(Box<dyn Any>)` downcasts: the receiving plugin registers a handler
    /// for its concrete payload type with [`crate::app::App::register_command`] and the drain
    /// dispatches by [`TypeId`].
    Typed {
        /// Concrete payload type id used to route the command to its registered handler.
        type_id: TypeId,
        /// Boxed payload.
        payload: Box<dyn Any + Send>,
    },
    /// Free-form variant retained for backward compatibility with callers that have not yet migrated to [`Self::Typed`].
    Custom(Box<dyn Any + Send>),
}

/// Message emitted when [`CommandQueue::try_push`] returns `Err` because the queue is full.
#[derive(Message, Clone, Copy, Debug)]
pub struct CommandQueueOverflow;

/// Producer-side ECS resource. Cloneable for sharing across worker threads.
#[derive(Resource, Clone)]
pub struct CommandQueue {
    tx: Sender<Command>,
}

impl CommandQueue {
    /// Returns a `(CommandQueue, CommandReceiver)` pair with [`DEFAULT_QUEUE_CAPACITY`].
    /// Insert the queue as a resource; pass the receiver to the drain system.
    pub fn new() -> (Self, CommandReceiver) {
        Self::with_capacity(DEFAULT_QUEUE_CAPACITY)
    }

    /// Returns a `(CommandQueue, CommandReceiver)` pair with the explicit capacity `cap`.
    pub fn with_capacity(cap: usize) -> (Self, CommandReceiver) {
        let (tx, rx) = bounded(cap);
        (Self { tx }, CommandReceiver { rx })
    }

    /// Sends `cmd` non-blockingly. Returns `Err(TrySendError::Full)` when the channel is full; never blocks.
    pub fn try_push(&self, cmd: Command) -> Result<(), TrySendError<Command>> {
        self.tx.try_send(cmd)
    }

    /// Borrows the underlying sender for adapter code (the C-ABI crate, async tasks) that needs to clone the channel across threads.
    pub fn sender(&self) -> &Sender<Command> {
        &self.tx
    }
}

/// Consumer-side ECS resource, registered as a non-Send resource since the drain runs on the main thread.
#[derive(Resource)]
pub struct CommandReceiver {
    rx: Receiver<Command>,
}

impl CommandReceiver {
    /// Returns an iterator that yields commands via `try_recv` and stops as soon as the channel is empty; never blocks.
    pub fn drain(&mut self) -> impl Iterator<Item = Command> + '_ {
        std::iter::from_fn(|| self.rx.try_recv().ok())
    }

    /// Borrows the underlying receiver for adapter code.
    pub fn receiver(&self) -> &Receiver<Command> {
        &self.rx
    }
}

/// Handler invoked when a [`Command::Typed`] with a matching [`TypeId`] is drained.
pub type CommandHandlerFn = Arc<dyn Fn(&mut World, Box<dyn Any + Send>) + Send + Sync>;

/// Resource holding the per-`TypeId` handler table for [`Command::Typed`].
///
/// Populated by [`crate::app::App::register_command`]. Plugins that author new command kinds register their handler once
/// at build-time and then push `Command::Typed { type_id: TypeId::of::<MyCmd>(), payload }` from any thread.
#[derive(Resource, Default, Clone)]
pub struct CommandRegistry {
    handlers: HashMap<TypeId, CommandHandlerFn>,
}

impl CommandRegistry {
    /// Registers `handler` for payloads of type `T`. Subsequent registrations for the same type overwrite the prior entry.
    pub fn register<T, F>(&mut self, handler: F)
    where
        T: Any + Send,
        F: Fn(&mut World, Box<T>) + Send + Sync + 'static,
    {
        let f: CommandHandlerFn = Arc::new(move |world, payload| {
            if let Ok(typed) = payload.downcast::<T>() {
                handler(world, typed);
            }
        });
        self.handlers.insert(TypeId::of::<T>(), f);
    }

    /// Returns the registered handler for `type_id`, if any.
    pub fn lookup(&self, type_id: &TypeId) -> Option<CommandHandlerFn> {
        self.handlers.get(type_id).cloned()
    }
}

/// Drains [`Command::SetProperty`] and [`Command::Typed`] entries from the [`CommandReceiver`] and applies them to the
/// [`PropertyStore`] / [`CommandRegistry`] respectively.
///
/// Other [`Command`] variants are intentionally dropped by this drain - their owning plugins install dedicated drains.
///
/// Not auto-installed by [`crate::app::App::new`]. `lumen-runtime` registers it in the
/// [`crate::tick::TickStage::CommandDrain`] stage when it builds an app; an embedder assembling its own `App`
/// adds it the same way.
pub fn apply_property_commands(world: &mut World) {
    let mut to_apply: Vec<(PropertyKey, PropertyValue)> = Vec::new();
    let mut typed_dispatch: Vec<(TypeId, Box<dyn Any + Send>)> = Vec::new();
    if let Some(mut recv) = world.get_resource_mut::<CommandReceiver>() {
        for cmd in recv.drain() {
            match cmd {
                Command::SetProperty { key, value } => to_apply.push((key, value)),
                Command::Typed { type_id, payload } => typed_dispatch.push((type_id, payload)),
                _ => {}
            }
        }
    }
    if let Some(mut store) = world.get_resource_mut::<PropertyStore>() {
        for (k, v) in to_apply {
            store.set(k, v);
        }
    }
    let registry = world.get_resource::<CommandRegistry>().cloned();
    if let Some(registry) = registry {
        for (tid, payload) in typed_dispatch {
            if let Some(handler) = registry.lookup(&tid) {
                handler(world, payload);
            }
        }
    }
}
