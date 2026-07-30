//! `lumen-devtools` - dev-only, in-window devtools overlay for Lumen.
//!
//! Chrome-webtools-style tabbed panel (Elements / Signals + Performance /
//! Network) that floats over the running app, toggled with **F12**. The UI
//! is authored in Lumen's own `.lmn` + `.css` pipeline (shipped as embedded
//! assets) and mounted as a second root lifted into the top paint band
//! ([`lumen_core::render_world::OverlayLayer`]) by the lumenc dev-mount.
//!
//! ## Data sources (all in-process, no server)
//! * **Elements** and **Signals + Perf** read the shared
//!   [`lumen_mcp::SnapshotHandle`] resource - the same per-tick snapshot the
//!   `LumenMcpPlugin` builders populate. The overlay never opens the MCP/TCP
//!   server; it just reads the resource. The devtools plugin also forces the
//!   snapshot schedule to sample every tick so the panel is live.
//! * **Network** reads a dev-only bounded ring ([`network::NetworkCapture`])
//!   fed by [`lumen_core::net_capture`], which the scripting `fetch()` /
//!   `http()` layer reports to.
//!
//! The overlay tags its own entities with [`DevtoolsMarker`] so the Elements
//! tab excludes them - devtools never inspects itself.
//!
//! ## Gating
//! This crate is compiled in only behind lumenc's off-by-default `devtools`
//! cargo feature (which `lumenc run` enables). It is absent from release /
//! bundle builds.

use std::collections::HashSet;

use bevy_ecs::message::MessageReader;
use bevy_ecs::prelude::*;
use lumen_core::app::{App, Plugin};
use lumen_core::components::{LumenId, TextContent, Visible};
use lumen_core::input::{ClickEvent, Key, KeyPressed};
use lumen_core::tick::TickStage;
use lumen_mcp::SnapshotHandle;

pub mod format;
pub mod network;

pub use network::{NetEntry, NetworkCapture};

/// Embedded overlay markup (authored `.lmn`).
pub const OVERLAY_LMN: &str = include_str!("assets/overlay.lmn");
/// Embedded overlay stylesheet (authored `.css`).
pub const OVERLAY_CSS: &str = include_str!("assets/overlay.css");
/// Embedded overlay interaction script (authored `.rhai`, reference form).
pub const OVERLAY_RHAI: &str = include_str!("assets/overlay.rhai");

/// `LumenId` of the single data-driven body text entity in the overlay.
pub const BODY_ID: &str = "dt-body";

/// Environment variable that opens the overlay at startup (instead of the
/// default hidden-until-F12). Useful for headless / automated verification
/// where there is no keyboard to press F12. Any non-empty, non-`0` value
/// enables it.
pub const OPEN_ENV: &str = "LUMEN_DEVTOOLS_OPEN";

/// Whether [`OPEN_ENV`] requests the overlay open at startup.
pub fn env_open() -> bool {
    std::env::var(OPEN_ENV)
        .map(|v| !v.is_empty() && v != "0")
        .unwrap_or(false)
}

/// Marker on the overlay root entity. Used by the F12 toggle to flip the
/// whole subtree's [`Visible`] state.
#[derive(Component, Clone, Copy, Debug, Default)]
pub struct DevtoolsRoot;

/// Marker on every entity belonging to the devtools overlay subtree. The
/// Elements tab excludes these so the overlay never inspects itself. The
/// lumenc dev-mount stamps it across the whole spawned subtree.
#[derive(Component, Clone, Copy, Debug, Default)]
pub struct DevtoolsMarker;

/// Which tab is currently shown.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Default)]
pub enum Tab {
    /// Live element tree.
    #[default]
    Elements,
    /// Global signals + frame/tick performance.
    Signals,
    /// Captured HTTP requests.
    Network,
}

/// Overlay UI state: visibility + active tab.
#[derive(Resource, Clone, Copy, Debug)]
pub struct DevtoolsState {
    /// Whether the overlay is currently shown.
    pub visible: bool,
    /// The active tab.
    pub tab: Tab,
}

impl Default for DevtoolsState {
    fn default() -> Self {
        // Hidden until the user presses F12.
        Self {
            visible: false,
            tab: Tab::Elements,
        }
    }
}

/// The devtools plugin. Registers overlay state, the network-capture ring +
/// sink, and the per-tick systems that drive the F12 toggle, tab switching,
/// and body refresh. Does NOT spawn the overlay markup - that is the lumenc
/// dev-mount's job (it owns the parser). See [`crate::mount_marks`].
#[derive(Debug, Default)]
pub struct DevtoolsPlugin;

impl Plugin for DevtoolsPlugin {
    fn build(self, app: &mut App) {
        app.world.init_resource::<DevtoolsState>();
        app.world.init_resource::<NetworkCapture>();
        // Honour the open-at-startup env override (no keyboard in headless).
        if env_open() {
            app.world.resource_mut::<DevtoolsState>().visible = true;
        }

        // Install the process-wide HTTP capture sink so the scripting
        // fetch/http layer's (otherwise no-op) reports start flowing.
        lumen_core::net_capture::init_net_capture();

        // The overlay wants live data. If the MCP snapshot plugin is present
        // (the lumenc default), drop its 1 Hz throttle to sample every tick.
        if let Some(mut sched) = app
            .world
            .get_resource_mut::<lumen_mcp::McpSnapshotSchedule>()
        {
            sched.interval = std::time::Duration::ZERO;
        }
        // If no snapshot handle exists (MCP disabled via `[mcp] port = 0`),
        // insert an empty one so reads don't fail; the Elements/Signals tabs
        // then show the "no snapshot" hint until a snapshot producer runs.
        if app.world.get_resource::<SnapshotHandle>().is_none() {
            app.world.init_resource::<SnapshotHandle>();
        }

        app.add_systems(TickStage::Input, network::drain_network_capture);
        app.add_systems(TickStage::Input, toggle_devtools_on_f12);
        app.add_systems(TickStage::Systems, switch_tab_on_click);
        app.add_systems(TickStage::A11ySync, refresh_body);
    }
}

/// System: F12 toggles the overlay. Flips [`DevtoolsState::visible`] and
/// mirrors it onto the [`DevtoolsRoot`] entity's [`Visible`] component (the
/// render extract, hit-test, and layout all honour `Visible(false)` on the
/// subtree).
pub fn toggle_devtools_on_f12(
    mut reader: MessageReader<KeyPressed>,
    mut state: ResMut<DevtoolsState>,
    mut roots: Query<&mut Visible, With<DevtoolsRoot>>,
) {
    let mut toggled = false;
    for ev in reader.read() {
        if let Key::Character(s) = &ev.key
            && s == "F12"
        {
            toggled = !toggled; // even count of F12s in one tick = no-op
        }
    }
    if !toggled {
        return;
    }
    state.visible = !state.visible;
    for mut v in &mut roots {
        v.0 = state.visible;
    }
}

/// System: clicking a tab button (`id="dt-tab-elements|signals|network"`)
/// switches the active tab and mirrors the choice into the `dt_tab` signal
/// (parity with the reference `overlay.rhai`).
pub fn switch_tab_on_click(
    mut reader: MessageReader<ClickEvent>,
    mut state: ResMut<DevtoolsState>,
    ids: Query<&LumenId>,
    store: Option<ResMut<lumen_core::property_store::PropertyStore>>,
) {
    let mut new_tab: Option<Tab> = None;
    for ev in reader.read() {
        if let Ok(id) = ids.get(ev.entity) {
            new_tab = match id.0.as_str() {
                "dt-tab-elements" => Some(Tab::Elements),
                "dt-tab-signals" => Some(Tab::Signals),
                "dt-tab-network" => Some(Tab::Network),
                _ => new_tab,
            };
        }
    }
    if let Some(tab) = new_tab {
        state.tab = tab;
        if let Some(mut store) = store {
            let name = match tab {
                Tab::Elements => "elements",
                Tab::Signals => "signals",
                Tab::Network => "network",
            };
            store.set_global_str("dt_tab", name);
        }
    }
}

/// System: rebuild the `dt-body` text from the active tab's data source.
/// Runs only while the overlay is visible. Excludes devtools' own entities
/// from the Elements tree.
pub fn refresh_body(
    state: Res<DevtoolsState>,
    snapshot: Res<SnapshotHandle>,
    network: Res<NetworkCapture>,
    own: Query<Entity, With<DevtoolsMarker>>,
    mut bodies: Query<(&LumenId, &mut TextContent)>,
) {
    if !state.visible {
        return;
    }
    let excluded: HashSet<u64> = own.iter().map(|e| e.to_bits()).collect();

    let text = {
        let Ok(snap) = snapshot.0.read() else {
            return;
        };
        match state.tab {
            Tab::Elements => format::format_elements(&snap, &excluded),
            Tab::Signals => format::format_signals(&snap),
            Tab::Network => format::format_network(&network),
        }
    };

    for (id, mut content) in &mut bodies {
        if id.0 == BODY_ID {
            if content.0 != text {
                content.0 = text.clone();
            }
            break;
        }
    }
}

/// Helper for the lumenc dev-mount: stamp [`DevtoolsMarker`] + [`DevtoolsRoot`]
/// onto the freshly spawned overlay subtree, and seed [`Visible(false)`] on
/// the root so the overlay starts hidden. `root` is the entity returned by
/// the spawner; `descendants` are all entities in its subtree (including
/// `root`).
///
/// Marking is done here (rather than in the mount) so the marker component
/// stays owned by this crate and the mount's lumenc footprint stays tiny.
/// `visible` seeds the root's [`Visible`] state (normally `false` -
/// hidden until F12 - unless [`OPEN_ENV`] requested startup-open).
pub fn mount_marks(world: &mut World, root: Entity, descendants: &[Entity], visible: bool) {
    for &e in descendants {
        if let Ok(mut em) = world.get_entity_mut(e) {
            em.insert(DevtoolsMarker);
        }
    }
    if let Ok(mut em) = world.get_entity_mut(root) {
        em.insert((DevtoolsRoot, Visible(visible)));
    }
}
