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
    Border, Color, DirtyLayout, Disabled, Edges, Fill, FlexDirection, Length, LumenId, Style,
    TextContent, TextStyle, TextWrap, Transform, Visible, Visuals,
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
/// `LumenId` of the inspect pane's text-content edit input.
pub const EDIT_ID: &str = "dt-edit";

/// Environment variable that opens the overlay at startup (instead of the
/// default hidden-until-F12). Useful for headless / automated verification
/// where there is no keyboard to press F12. Any non-empty, non-`0` value
/// enables it.
pub const OPEN_ENV: &str = "LUMEN_DEVTOOLS_OPEN";

// Dynamic-state palette, the Chrome DevTools dark scheme. Mirrors
// assets/overlay.css - the stylesheet cannot reach entities spawned after
// the mount, so the interactive states carry their one Rust fallback here
// (same contract as the tooltip defaults).
const TAB_TEXT: Color = Color::from_rgba8([0x9a, 0xa0, 0xa6, 0xff]);
const TAB_TEXT_ACTIVE: Color = Color::from_rgba8([0xe8, 0xea, 0xed, 0xff]);
const TAB_UNDERLINE: Color = Color::from_rgba8([0x8a, 0xb4, 0xf8, 0xff]);
const TAB_FILL_HOVER: Color = Color::from_rgba8([0x35, 0x36, 0x3a, 0xff]);
const ROW_FILL_HOVER: Color = Color::from_rgba8([0x2f, 0x30, 0x33, 0xff]);
const ROW_FILL_SELECTED: Color = Color::from_rgba8([0x21, 0x41, 0x66, 0xff]);
// Markup syntax colors (Chrome Elements panel).
const TAG_COLOR: Color = Color::from_rgba8([0x5d, 0xb0, 0xd7, 0xff]);
const META_COLOR: Color = Color::from_rgba8([0xf2, 0x8b, 0x54, 0xff]);
const DIM_COLOR: Color = Color::from_rgba8([0x9a, 0xa0, 0xa6, 0xff]);
const FLAG_COLOR: Color = Color::from_rgba8([0xd7, 0xae, 0xfb, 0xff]);
// Element highlight + its tag tooltip.
const HIGHLIGHT_FILL: Color = Color::from_rgba8([0x6f, 0xa8, 0xdc, 0x66]);
const HIGHLIGHT_BORDER: Color = Color::from_rgba8([0x6f, 0xa8, 0xdc, 0xcc]);
const TIP_FILL: Color = Color::from_rgba8([0x20, 0x21, 0x24, 0xf2]);
const TIP_BORDER: Color = Color::from_rgba8([0x3c, 0x40, 0x43, 0xff]);
const TIP_TEXT: Color = Color::from_rgba8([0xe8, 0xea, 0xed, 0xff]);
const ROW_FONT_PX: f32 = 12.0;
const ROW_HEIGHT_PX: f32 = 18.0;
const ROW_INDENT_PX: f32 = 12.0;
const TIP_FONT_PX: f32 = 11.0;
const TIP_HEIGHT_PX: f32 = 18.0;

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

/// Marker on the highlight's tag tooltip (the `<tag>#id WxH` chip Chrome
/// shows next to the highlighted element).
#[derive(Component, Clone, Copy, Debug, Default)]
pub struct HighlightTip;

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
        // Its tag tooltip chip, positioned beside the box each tick.
        app.world.spawn((
            HighlightTip,
            DevtoolsMarker,
            Disabled,
            OverlayLayer,
            Visible(false),
            Transform::default(),
            TextContent(String::new()),
            TextStyle {
                color: TIP_TEXT,
                size_px: TIP_FONT_PX,
                wrap: TextWrap::None,
                ..Default::default()
            },
            Visuals {
                fill: Some(Fill::Solid(TIP_FILL)),
                radius: 3.0,
                border: Some(Border::uniform(Edges::all(1.0), TIP_BORDER)),
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
/// its element; while picking, a click anywhere on the app selects the
/// clicked element and disarms the mode (the click still reaches the app);
/// and the inspect-pane actions edit the selected element in the running
/// app - toggle its visibility, despawn its subtree, or replace its text
/// content with what was typed into the edit input.
#[allow(clippy::too_many_arguments)]
pub fn handle_clicks(
    mut commands: Commands,
    mut reader: MessageReader<ClickEvent>,
    mut state: ResMut<DevtoolsState>,
    ids: Query<(Entity, &LumenId)>,
    rows: Query<&RowTarget>,
    own: Query<(), With<DevtoolsMarker>>,
    vis: Query<&Visible>,
    texts: Query<&TextContent>,
    store: Option<ResMut<lumen_core::property_store::PropertyStore>>,
) {
    let mut new_tab: Option<Tab> = None;
    for ev in reader.read() {
        if let Ok(row) = rows.get(ev.entity) {
            state.selected = Some(row.0);
            continue;
        }
        if let Ok((_, id)) = ids.get(ev.entity) {
            let selected = state.selected.and_then(Entity::try_from_bits);
            match id.0.as_str() {
                "dt-tab-elements" => new_tab = Some(Tab::Elements),
                "dt-tab-signals" => new_tab = Some(Tab::Signals),
                "dt-tab-network" => new_tab = Some(Tab::Network),
                PICK_ID => {
                    state.picking = !state.picking;
                    state.tab = Tab::Elements;
                    continue;
                }
                "dt-act-hide" => {
                    if let Some(target) = selected
                        && let Ok(mut e) = commands.get_entity(target)
                    {
                        let shown = vis.get(target).map(|v| v.0).unwrap_or(true);
                        e.insert(Visible(!shown));
                    }
                    continue;
                }
                "dt-act-del" => {
                    if let Some(target) = selected
                        && let Ok(mut e) = commands.get_entity(target)
                    {
                        e.despawn();
                        state.selected = None;
                    }
                    continue;
                }
                "dt-act-apply" => {
                    let typed = ids
                        .iter()
                        .find(|(_, id)| id.0 == EDIT_ID)
                        .and_then(|(e, _)| texts.get(e).ok())
                        .map(|t| t.0.clone());
                    // Only elements that already show text take an edit -
                    // pasting a TextContent onto a container would not
                    // render anything sensible.
                    if let (Some(target), Some(new_text)) = (selected, typed)
                        && texts.get(target).is_ok()
                        && let Ok(mut e) = commands.get_entity(target)
                    {
                        e.insert((TextContent(new_text), DirtyLayout));
                    }
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
        // The row container carries the target + hit-test Visuals; each
        // label part is a child with its own syntax color (only the
        // container is a hit candidate, so hover/click land on the row).
        let row = commands
            .spawn((
                DevtoolsMarker,
                RowTarget(r.id),
                ChildOf(container),
                Style {
                    width: Length::Percent(100.0),
                    height: Length::Px(ROW_HEIGHT_PX),
                    flex_direction: FlexDirection::Row,
                    padding: Edges {
                        left: 8.0 + r.depth as f32 * ROW_INDENT_PX,
                        ..Default::default()
                    },
                    ..Default::default()
                },
                Visuals::default(),
            ))
            .id();
        let parts: [(&str, Color); 4] = [
            (&r.tag, TAG_COLOR),
            (&r.meta, META_COLOR),
            (&r.dims, DIM_COLOR),
            (&r.flags, FLAG_COLOR),
        ];
        for (text, color) in parts {
            if text.is_empty() {
                continue;
            }
            commands.spawn((
                DevtoolsMarker,
                ChildOf(row),
                Style {
                    height: Length::Px(ROW_HEIGHT_PX),
                    ..Default::default()
                },
                TextContent(text.to_string()),
                TextStyle {
                    color,
                    size_px: ROW_FONT_PX,
                    wrap: TextWrap::None,
                    ..Default::default()
                },
            ));
        }
    }
    commands.entity(container).insert(DirtyLayout);
    *last = rows;
}

/// System: tab-button visuals - active tab blue, hovered tab lifted, Pick
/// button blue while pick mode is armed. Writes only on change so a static
/// panel raises no frame dirt.
#[allow(clippy::type_complexity)]
pub fn style_tabs(
    mut commands: Commands,
    state: Res<DevtoolsState>,
    mut tabs: Query<(
        Entity,
        &LumenId,
        Option<&mut Visuals>,
        &mut TextStyle,
        Option<&Hovered>,
    )>,
) {
    for (entity, id, visuals, mut text, hovered) in &mut tabs {
        let active = match id.0.as_str() {
            "dt-tab-elements" => state.tab == Tab::Elements,
            "dt-tab-signals" => state.tab == Tab::Signals,
            "dt-tab-network" => state.tab == Tab::Network,
            PICK_ID => state.picking,
            _ => continue,
        };
        // Chrome-style flat tabs: no fill at rest, subtle fill on hover, and
        // the active tab gets bright text plus a blue bottom underline.
        let fill = if hovered.is_some() {
            Some(Fill::Solid(TAB_FILL_HOVER))
        } else {
            None
        };
        let border = active.then(|| Border {
            widths: Edges {
                bottom: 2.0,
                ..Default::default()
            },
            color: TAB_UNDERLINE,
            side_colors: None,
        });
        let color = if active { TAB_TEXT_ACTIVE } else { TAB_TEXT };
        match visuals {
            Some(mut visuals) => {
                if visuals.fill != fill {
                    visuals.fill = fill;
                }
                if visuals.border != border {
                    visuals.border = border;
                }
            }
            // A flat tab gets no Visuals from the stylesheet (no static
            // background), which would also keep it out of hit-testing -
            // seed the component so the button is clickable.
            None => {
                commands.entity(entity).insert(Visuals {
                    fill,
                    border,
                    ..Default::default()
                });
            }
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
#[allow(clippy::too_many_arguments, clippy::type_complexity)]
pub fn update_highlight(
    state: Res<DevtoolsState>,
    snapshot: Res<SnapshotHandle>,
    hovered_rows: Query<&RowTarget, With<Hovered>>,
    hovered_app: Query<Entity, (With<Hovered>, Without<DevtoolsMarker>)>,
    transforms: Query<&Transform, (Without<HighlightBox>, Without<HighlightTip>)>,
    parents: Query<&ChildOf>,
    scrolls: Query<&ScrollOffset>,
    mut boxes: Query<(&mut Transform, &mut Visible), (With<HighlightBox>, Without<HighlightTip>)>,
    mut tips: Query<
        (&mut Transform, &mut Visible, &mut TextContent),
        (With<HighlightTip>, Without<HighlightBox>),
    >,
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

    // The tag tooltip chip: `<tag>#id.class  WxH`, sitting just above the
    // box (below it when the box touches the top edge).
    let tip = rect.and_then(|(origin, size)| {
        let target = target?;
        let snap = snapshot.0.read().ok()?;
        let i = snap.inspect.get(&target.to_bits())?;
        let mut label = format!(" <{}>", i.tag.as_deref().unwrap_or("node"));
        if let Some(id) = &i.lumen_id {
            label.push('#');
            label.push_str(id);
        }
        for c in &i.classes {
            label.push('.');
            label.push_str(c);
        }
        label.push_str(&format!("  {:.0}x{:.0} ", size.x, size.y));
        let width = label.chars().count() as f32 * (TIP_FONT_PX * 0.62) + 8.0;
        let y = if origin.y >= TIP_HEIGHT_PX + 4.0 {
            origin.y - TIP_HEIGHT_PX - 2.0
        } else {
            origin.y + size.y + 2.0
        };
        Some((
            glam::Vec2::new(origin.x.max(0.0), y.max(0.0)),
            glam::Vec2::new(width, TIP_HEIGHT_PX),
            label,
        ))
    });
    for (mut transform, mut visible, mut text) in &mut tips {
        match &tip {
            Some((origin, size, label)) => {
                if transform.absolute != *origin || transform.size != *size {
                    transform.absolute = *origin;
                    transform.size = *size;
                }
                if text.0 != *label {
                    text.0 = label.clone();
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
#[allow(clippy::type_complexity)]
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
