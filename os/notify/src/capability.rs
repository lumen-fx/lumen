//! The `os-notify` capability: the notification service, the per-tick
//! action-button drain, and the two notification commands.
//!
//! `[app] id` becomes the notification app id: Windows keys toasts off the
//! AppUserModelID and macOS off the bundle id, so without it a notification
//! is attributed to whatever binary happens to be running. The service opens
//! no thread and no connection, so it is installed for every app.

use bevy_ecs::message::{MessageReader, Messages};
use bevy_ecs::prelude::*;
use lumen_capability::{CapabilityEnv, Select};
use lumen_core::app::App;
use lumen_core::tick::TickStage;
use lumen_script::{ScriptCommand, ScriptCommandEvent, ScriptSet};

use crate::{Notification, NotificationService, parse_actions, parse_options};

/// What a static package looks for in the app's sources before it
/// carries this subsystem.
pub const SELECT: Select = Select::OnUse(&["notify"]);

/// Install the subsystem. What the capability crate beside this one
/// registers.
pub fn install(app: &mut App, env: &CapabilityEnv) {
    let service = match env.declared_app_id.as_deref() {
        Some(id) => NotificationService::new().with_app_id(id),
        None => NotificationService::new(),
    };
    app.world.insert_resource(service);
    app.add_systems(TickStage::Systems, crate::poll_notification_actions);
    app.world.init_resource::<Messages<ScriptCommandEvent>>();
    app.add_systems(
        TickStage::Systems,
        apply_notify_commands
            .after(ScriptSet::Tick)
            .after(ScriptSet::Dispatch)
            .after(ScriptSet::DomInput)
            .after(ScriptSet::Frame)
            .after(ScriptSet::Fill),
    );
}

/// Send the notifications the scripts ask for. Fire-and-forget: the call
/// returns once the daemon accepts the spec and the popup lives on the OS
/// side. A missing daemon logs through the service rather than killing the
/// app.
fn apply_notify_commands(
    mut events: MessageReader<ScriptCommandEvent>,
    notifier: Res<NotificationService>,
) {
    for ev in events.read() {
        match &ev.0 {
            ScriptCommand::Notify { title, body } => {
                notifier.send_simple(title, body);
            }
            ScriptCommand::NotifyEx {
                id,
                title,
                body,
                options,
                actions,
            } => {
                let options = parse_options(options);
                notifier.send(&Notification {
                    id: id.clone(),
                    title: title.clone(),
                    body: body.clone(),
                    icon: options.icon,
                    urgency: options.urgency,
                    actions: parse_actions(actions),
                });
            }
            _ => {}
        }
    }
}
