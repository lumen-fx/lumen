//! `lumen-devtools` - dev-only, in-window devtools panel for Lumen.
//!
//! Chrome-devtools-style tabbed panel (Elements / Signals + Performance /
//! Network) docked to the window's right edge, toggled with **F12**. While
//! open it writes [`lumen_core::render_world::DockInsets`], so the app
//! reflows into the remaining width instead of being covered. The static
//! chrome is authored in Lumen's own `.lmn` + `.css` pipeline (shipped as
//! embedded assets) and mounted as a second root lifted into the top paint
//! band ([`lumen_core::render_world::OverlayLayer`]) by the lumenc
//! dev-mount.
//!
//! The Elements tab is one spawned row entity per element: hovering a row
//! outlines that element in the app, clicking selects it and fills the
//! inspect pane, and the Pick tab-button arms hover-to-inspect directly on
//! the app (the click that picks still reaches the app). Entities spawned
//! after the mount are outside the overlay stylesheet's reach, so the
//! dynamic states (row/tab hover, selection, the highlight box) are styled
//! here with constants mirroring the `.css` palette.
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
//! Compiled in behind lumenc's `devtools` cargo feature (a lumenc default;
//! absent from a `--no-default-features` build and from every shipped app).

use std::collections::HashSet;

use bevy_ecs::hierarchy::ChildOf;
use bevy_ecs::message::MessageReader;
use bevy_ecs::prelude::*;
use lumen_core::app::{App, Plugin};
use lumen_core::components::{
    Border, Color, DirtyLayout, Disabled, Edges, Fill, Length, LumenId, Style, TextContent,
    TextStyle, TextWrap, Transform, Visible, Visuals,
};
use lumen_core::input::{ClickEvent, Hovered, Key, KeyPressed, ScrollOffset};
use lumen_core::render_world::{DockInsets, OverlayLayer};
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

/// `LumenId` of the text-blob body entity (Signals / Network tabs, and the
/// Elements empty-state hint).
pub const BODY_ID: &str = "dt-body";
/// `LumenId` of the column the Elements rows are spawned under.
pub const ROWS_ID: &str = "dt-rows";
/// `LumenId` of the docked panel column (its width feeds [`DockInsets`]).
pub const PANEL_ID: &str = "dt-panel";
/// `LumenId` of the inspect pane container.
pub const INSPECT_ID: &str = "dt-inspect";
/// `LumenId` of the inspect pane body text.
pub const INSPECT_BODY_ID: &str = "dt-inspect-body";
/// `LumenId` of the pick-mode toggle button.
pub const PICK_ID: &str = "dt-pick";

/// Environment variable that opens the overlay at startup (instead of the
/// default hidden-until-F12). Useful for headless / automated verification
/// where there is no keyboard to press F12. Any non-empty, non-`0` value
/// enables it.
pub const OPEN_ENV: &str = "LUMEN_DEVTOOLS_OPEN";

// Dynamic-state palette. Mirrors assets/overlay.css - the stylesheet cannot
// reach entities spawned after the mount, so the interactive states carry
// their one Rust fallback here (same contract as the tooltip defaults).
const TAB_FILL: Color = Color::from_rgba8([0x21, 0x25, 0x2c, 0xff]);
const TAB_FILL_HOVER: Color = Color::from_rgba8([0x2b, 0x30, 0x3a, 0xff]);
const TAB_FILL_ACTIVE: Color = Color::from_rgba8([0x3a, 0x68, 0xd8, 0xff]);
const TAB_TEXT: Color = Color::from_rgba8([0xc3, 0xc8, 0xd0, 0xff]);
const TAB_TEXT_ACTIVE: Color = Color::from_rgba8([0xff, 0xff, 0xff, 0xff]);
const ROW_TEXT: Color = Color::from_rgba8([0xc9, 0xcd, 0xd4, 0xff]);
const ROW_FILL_HOVER: Color = Color::from_rgba8([0x22, 0x26, 0x2e, 0xff]);
const ROW_FILL_SELECTED: Color = Color::from_rgba8([0x2f, 0x4d, 0x80, 0xff]);
const HIGHLIGHT_FILL: Color = Color::from_rgba8([0x4d, 0x8c, 0xf2, 0x48]);
const HIGHLIGHT_BORDER: Color = Color::from_rgba8([0x4a, 0x90, 0xe2, 0xe6]);
const ROW_FONT_PX: f32 = 12.0;
const ROW_HEIGHT_PX: f32 = 17.0;
const ROW_INDENT_PX: f32 = 12.0;

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

/// Marker on every entity belonging to the devtools overlay subtree
/// (including rows and the highlight box spawned after the mount). The
/// Elements tab excludes these so the overlay never inspects itself. The
/// lumenc dev-mount stamps it across the whole spawned subtree.
#[derive(Component, Clone, Copy, Debug, Default)]
pub struct DevtoolsMarker;

/// One Elements row; the payload is the entity bits of the app element the
/// row describes.
#[derive(Component, Clone, Copy, Debug)]
pub struct RowTarget(pub u64);

/// Marker on the single element-highlight box (the translucent rectangle
/// drawn over the hovered / selected element, Chrome-style).
#[derive(Component, Clone, Copy, Debug, Default)]
pub struct HighlightBox;

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

/// Overlay UI state: visibility, active tab, selection, pick mode.
#[derive(Resource, Clone, Copy, Debug)]
pub struct DevtoolsState {
    /// Whether the overlay is currently shown.
    pub visible: bool,
    /// The active tab.
    pub tab: Tab,
    /// Entity bits of the selected app element, if any.
    pub selected: Option<u64>,
    /// Pick mode: hovering the app outlines the hovered element, clicking
    /// selects it (and disarms the mode).
    pub picking: bool,
}

impl Default for DevtoolsState {
    fn default() -> Self {
        // Hidden until the user presses F12.
        Self {
            visible: false,
            tab: Tab::Elements,
            selected: None,
            picking: false,
        }
    }
}

/// The devtools plugin. Registers overlay state, the network-capture ring +
/// sink, the highlight box, and the per-tick systems that drive the F12
/// toggle, tab/row/pick interaction, the dock inset, and the panel refresh.
/// Does not spawn the overlay markup - that is the lumenc dev-mount's job
/// (it owns the parser). See [`crate::mount_marks`].
#[derive(Debug, Default)]
pub struct DevtoolsPlugin;

impl Plugin for DevtoolsPlugin {
    fn build(self, app: &mut App) {
        app.world.init_resource::<DevtoolsState>();
        app.world.init_resource::<NetworkCapture>();
        app.world.init_resource::<DockInsets>();
        // Honour the open-at-startup env override (no keyboard in headless).
        if env_open() {
            app.world.resource_mut::<DevtoolsState>().visible = true;
        }

        // The element-highlight box: its own top-layer root, placed over the
        // target by writing its `Transform` directly - it deliberately has no
        // `Style`, so layout never touches it (an inset on a layout root is
        // ignored; the box would pin to the origin). `Disabled` keeps it out
        // of hit-testing so it never steals the hover it is visualizing.
        app.world.spawn((
            HighlightBox,
            DevtoolsMarker,
            Disabled,
            OverlayLayer,
            Visible(false),
            Transform::default(),
            Visuals {
                fill: Some(Fill::Solid(HIGHLIGHT_FILL)),
                border: Some(Border::uniform(Edges::all(1.0), HIGHLIGHT_BORDER)),
                ..Default::default()
            },
        ));

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
        app.add_systems(TickStage::Systems, handle_clicks);
        app.add_systems(TickStage::Systems, rebuild_element_rows);
        app.add_systems(TickStage::Systems, style_tabs);
        app.add_systems(TickStage::Systems, style_rows);
        app.add_systems(TickStage::Systems, sync_dock_inset);
        app.add_systems(TickStage::Systems, update_highlight);
        app.add_systems(TickStage::A11ySync, refresh_panes);
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

/// System: route overlay clicks. Tab buttons switch the active tab (and
/// mirror it into the `dt_tab` signal, parity with the reference
/// `overlay.rhai`); the Pick button arms pick mode; an Elements row selects
/// its element; and while picking, a click anywhere on the app selects the
/// clicked element and disarms the mode (the click still reaches the app).
pub fn handle_clicks(
    mut reader: MessageReader<ClickEvent>,
    mut state: ResMut<DevtoolsState>,
    ids: Query<&LumenId>,
    rows: Query<&RowTarget>,
    own: Query<(), With<DevtoolsMarker>>,
    store: Option<ResMut<lumen_core::property_store::PropertyStore>>,
) {
    let mut new_tab: Option<Tab> = None;
    for ev in reader.read() {
        if let Ok(row) = rows.get(ev.entity) {
            state.selected = Some(row.0);
            continue;
        }
        if let Ok(id) = ids.get(ev.entity) {
            match id.0.as_str() {
                "dt-tab-elements" => new_tab = Some(Tab::Elements),
                "dt-tab-signals" => new_tab = Some(Tab::Signals),
                "dt-tab-network" => new_tab = Some(Tab::Network),
                PICK_ID => {
                    state.picking = !state.picking;
                    state.tab = Tab::Elements;
                    continue;
                }
                _ => {}
            }
            if new_tab.is_some() {
                continue;
            }
        }
        // Not an overlay control: while picking, a click on app content
        // selects the clicked element.
        if state.picking && own.get(ev.entity).is_err() {
            state.selected = Some(ev.entity.to_bits());
            state.picking = false;
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

/// System: keep one spawned row entity per Elements line. Rows rebuild only
/// when the flattened tree actually changes; leaving the Elements tab (or
/// closing the overlay) despawns them.
pub fn rebuild_element_rows(
    mut commands: Commands,
    state: Res<DevtoolsState>,
    snapshot: Res<SnapshotHandle>,
    own: Query<Entity, With<DevtoolsMarker>>,
    ids: Query<(Entity, &LumenId)>,
    existing: Query<Entity, With<RowTarget>>,
    mut last: Local<Vec<format::ElementRow>>,
) {
    if !state.visible || state.tab != Tab::Elements {
        if !last.is_empty() {
            for e in &existing {
                commands.entity(e).despawn();
            }
            last.clear();
        }
        return;
    }
    let excluded: HashSet<u64> = own.iter().map(|e| e.to_bits()).collect();
    let rows = {
        let Ok(snap) = snapshot.0.read() else {
            return;
        };
        format::element_rows(&snap, &excluded)
    };
    if *last == rows {
        return;
    }
    for e in &existing {
        commands.entity(e).despawn();
    }
    let Some(container) = ids.iter().find(|(_, id)| id.0 == ROWS_ID).map(|(e, _)| e) else {
        return;
    };
    for r in &rows {
        commands.spawn((
            DevtoolsMarker,
            RowTarget(r.id),
            ChildOf(container),
            Style {
                width: Length::Percent(100.0),
                height: Length::Px(ROW_HEIGHT_PX),
                padding: Edges {
                    left: 4.0 + r.depth as f32 * ROW_INDENT_PX,
                    ..Default::default()
                },
                ..Default::default()
            },
            TextContent(r.label.clone()),
            TextStyle {
                color: ROW_TEXT,
                size_px: ROW_FONT_PX,
                wrap: TextWrap::None,
                ..Default::default()
            },
            // A fill-less Visuals makes the row hit-testable (hover + click).
            Visuals::default(),
        ));
    }
    commands.entity(container).insert(DirtyLayout);
    *last = rows;
}

/// System: tab-button visuals - active tab blue, hovered tab lifted, Pick
/// button blue while pick mode is armed. Writes only on change so a static
/// panel raises no frame dirt.
pub fn style_tabs(
    state: Res<DevtoolsState>,
    mut tabs: Query<(&LumenId, &mut Visuals, &mut TextStyle, Option<&Hovered>)>,
) {
    for (id, mut visuals, mut text, hovered) in &mut tabs {
        let active = match id.0.as_str() {
            "dt-tab-elements" => state.tab == Tab::Elements,
            "dt-tab-signals" => state.tab == Tab::Signals,
            "dt-tab-network" => state.tab == Tab::Network,
            PICK_ID => state.picking,
            _ => continue,
        };
        let fill = if active {
            TAB_FILL_ACTIVE
        } else if hovered.is_some() {
            TAB_FILL_HOVER
        } else {
            TAB_FILL
        };
        let color = if active { TAB_TEXT_ACTIVE } else { TAB_TEXT };
        if visuals.fill != Some(Fill::Solid(fill)) {
            visuals.fill = Some(Fill::Solid(fill));
        }
        if text.color != color {
            text.color = color;
        }
    }
}

/// System: row visuals - hovered row lifted, selected row blue. Writes only
/// on change.
pub fn style_rows(
    state: Res<DevtoolsState>,
    mut rows: Query<(&RowTarget, &mut Visuals, Option<&Hovered>)>,
) {
    for (target, mut visuals, hovered) in &mut rows {
        let fill = if state.selected == Some(target.0) {
            Some(Fill::Solid(ROW_FILL_SELECTED))
        } else if hovered.is_some() {
            Some(Fill::Solid(ROW_FILL_HOVER))
        } else {
            None
        };
        if visuals.fill != fill {
            visuals.fill = fill;
        }
    }
}

/// System: while the panel is visible, reserve its width from the layout
/// viewport ([`DockInsets`]) so the app reflows beside it instead of being
/// covered. The width is read from the panel's own laid-out box, so a CSS
/// resize of `.dt-panel` follows automatically (one tick behind).
pub fn sync_dock_inset(
    state: Res<DevtoolsState>,
    panels: Query<(&LumenId, &Transform)>,
    mut insets: ResMut<DockInsets>,
) {
    let width = panels
        .iter()
        .find(|(id, _)| id.0 == PANEL_ID)
        .map(|(_, t)| t.size.x)
        .unwrap_or(0.0);
    let want = if state.visible { width } else { 0.0 };
    if insets.right != want {
        insets.right = want;
    }
}

/// System: position the highlight box over the current inspect target -
/// the app element under the pointer while picking, else the hovered row's
/// element, else the selection. Hidden when there is no target.
pub fn update_highlight(
    state: Res<DevtoolsState>,
    hovered_rows: Query<&RowTarget, With<Hovered>>,
    hovered_app: Query<Entity, (With<Hovered>, Without<DevtoolsMarker>)>,
    transforms: Query<&Transform, Without<HighlightBox>>,
    parents: Query<&ChildOf>,
    scrolls: Query<&ScrollOffset>,
    mut boxes: Query<(&mut Transform, &mut Visible), With<HighlightBox>>,
) {
    let target: Option<Entity> = if !state.visible {
        None
    } else if state.picking {
        hovered_app.iter().next()
    } else if let Some(row) = hovered_rows.iter().next() {
        Entity::try_from_bits(row.0)
    } else {
        state
            .selected
            .filter(|_| state.tab == Tab::Elements)
            .and_then(Entity::try_from_bits)
    };

    let rect = target.and_then(|e| {
        let t = transforms.get(e).ok()?;
        // On-screen origin = layout-absolute minus ancestor scroll offsets
        // (same correction hit-testing applies).
        let mut origin = t.absolute;
        let mut cur = e;
        while let Ok(p) = parents.get(cur) {
            let parent = p.parent();
            if let Ok(off) = scrolls.get(parent) {
                origin -= off.0;
            }
            cur = parent;
        }
        Some((origin, t.size))
    });

    for (mut transform, mut visible) in &mut boxes {
        match rect {
            Some((origin, size)) => {
                if transform.absolute != origin || transform.size != size {
                    transform.absolute = origin;
                    transform.size = size;
                }
                if !visible.0 {
                    visible.0 = true;
                }
            }
            None => {
                if visible.0 {
                    visible.0 = false;
                }
            }
        }
    }
}

/// System: per-tab pane wiring - which of the rows column / text body /
/// inspect pane is visible, and the text the visible ones carry. Runs only
/// while the overlay is shown.
pub fn refresh_panes(
    mut commands: Commands,
    state: Res<DevtoolsState>,
    snapshot: Res<SnapshotHandle>,
    network: Res<NetworkCapture>,
    rows: Query<(), With<RowTarget>>,
    mut panes: Query<(
        Entity,
        &LumenId,
        Option<&mut TextContent>,
        Option<&mut Visible>,
    )>,
) {
    if !state.visible {
        return;
    }
    let elements = state.tab == Tab::Elements;
    let have_rows = !rows.is_empty();

    let (body_text, inspect_text) = {
        let Ok(snap) = snapshot.0.read() else {
            return;
        };
        let body = match state.tab {
            Tab::Elements if !have_rows => {
                Some("(no snapshot yet - is the MCP/snapshot plugin enabled?)".to_string())
            }
            Tab::Elements => None,
            Tab::Signals => Some(format::format_signals(&snap)),
            Tab::Network => Some(format::format_network(&network)),
        };
        let inspect = state
            .selected
            .and_then(|bits| snap.inspect.get(&bits))
            .map(format::format_inspect);
        (body, inspect)
    };

    for (entity, id, text, visible) in &mut panes {
        let (want_visible, want_text): (bool, Option<&str>) = match id.0.as_str() {
            BODY_ID => (body_text.is_some(), body_text.as_deref()),
            ROWS_ID => (elements && have_rows, None),
            INSPECT_ID => (elements && inspect_text.is_some(), None),
            INSPECT_BODY_ID => (true, inspect_text.as_deref()),
            _ => continue,
        };
        match visible {
            Some(mut v) => {
                if v.0 != want_visible {
                    v.0 = want_visible;
                }
            }
            // IR-spawned entities don't all carry Visible; seed it so the
            // toggle takes effect instead of silently missing the query.
            None => {
                commands.entity(entity).insert(Visible(want_visible));
            }
        }
        if let (Some(mut content), Some(want)) = (text, want_text)
            && content.0 != want
        {
            content.0 = want.to_string();
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
        // OverlayLayer lifts the subtree into the top paint band: the overlay
        // is a second root spawned before the app's, so without it the app's
        // opaque background paints over the whole panel.
        em.insert((DevtoolsRoot, Visible(visible), OverlayLayer));
    }
}
