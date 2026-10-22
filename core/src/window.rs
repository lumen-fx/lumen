//! The app's windows: backend-neutral descriptions, the window entities the
//! app runs with, and which window each element of the document shows in.
//!
//! Every launch path (the CLI, the C-ABI, the SDKs) resolves the same
//! values - size, title, clear color, chrome, menu bar - before a window
//! exists, and a window backend consumes them when it opens the primary
//! window ([`WindowOptions`]). The menu tree ([`MenuModel`], [`Menu`],
//! [`MenuEntry`]) is pure data: it says what the menu bar contains, not how
//! a platform draws it. `lumen-os-menu` re-exports these types and owns the
//! native attach.
//!
//! ## Window entities
//!
//! Once the app runs, each window is an entity in the main world carrying
//! [`Window`] (the state the app wants the window to have), its
//! [`WindowViewport`] (the size and scale the window lays out and draws at)
//! and its [`WindowRuntime`] (what the platform reports back). The first one,
//! spawned by [`crate::app::App::new`], also carries [`PrimaryWindow`]. A
//! window backend creates a platform window for every window entity, applies
//! a change to [`Window`] to it, and writes back what the platform settled
//! on; spawning a [`Window`] entity is how a second window opens, and
//! despawning it closes that window.
//!
//! ## One document, several windows
//!
//! All windows show parts of one element tree. An element carrying
//! [`WindowOf`] is a surface root: it and everything under it, down to the
//! next surface root, show in that window, laid out against that window's
//! viewport, painted into that window's frame, and hit-tested against that
//! window's pointer. Everything else shows in the primary window. A surface
//! root stays in the tree where it is, so styles inherit through it, events
//! bubble through it, and `:hover` reaches its ancestors.
//! What it escapes is its ancestors' clip, transform, opacity, scroll offset,
//! and `display: none`, the way CSS's top layer does. [`WindowPartitions`]
//! records which window each element is in.
//!
//! ## Closing and quitting
//!
//! The app asks for a window to close, or for the whole app to quit, with
//! [`WindowRequest`] and [`QuitRequest`]; the driver answers both through the
//! [`crate::input::CloseRequest`] veto path in
//! [`crate::window_backend::run_close_request`]. A secondary window that
//! closes first announces [`WindowClosing`], so whatever put content into it
//! can move that content out, and is then despawned. Closing the primary
//! window ends the app, unless [`QuitPolicy::Explicit`] keeps it running.

use std::collections::HashMap;

use bevy_ecs::hierarchy::Children;
use bevy_ecs::prelude::*;
use bevy_ecs::system::SystemParam;
use glam::{IVec2, Vec2};

use crate::components::Color;

/// What kind of window a [`Window`] is, which decides its decorations and
/// whether it belongs to another window.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash)]
pub enum WindowKind {
    /// An ordinary top-level window with its own taskbar entry.
    #[default]
    Normal,
    /// A dialog: owned by [`Window::owner`] (above it, minimized with it, no
    /// taskbar entry where the platform allows) with dialog decorations.
    Dialog,
    /// A tool window: owned like a dialog, with the platform's small tool
    /// window chrome where it has one.
    Tool,
}

/// A window as the app wants it to be.
///
/// App code (the `window.set_title` / `window.set_size` script calls, the C
/// ABI, the SDKs, a Rust system) writes the fields; the window backend
/// applies what changed to the platform window after the tick, and writes
/// back the size the platform settled on when the window is resized from
/// outside the app. What the window has right now is in its
/// [`WindowViewport`] and [`WindowRuntime`].
///
/// Spawning an entity with a `Window` opens a window; the required
/// [`WindowViewport`] and [`WindowRuntime`] come with it.
#[derive(Component, Clone, Debug, PartialEq)]
#[require(WindowViewport, WindowRuntime)]
pub struct Window {
    /// Window title.
    pub title: String,
    /// Inner size in logical pixels.
    pub size: Vec2,
    /// What kind of window this is.
    pub kind: WindowKind,
    /// Whether the window is shown. A hidden window keeps its content and
    /// its accessibility bridge; it paints nothing.
    pub visible: bool,
    /// Whether the user can resize the window.
    pub resizable: bool,
    /// Whether the platform draws a title bar and border.
    pub decorations: bool,
    /// Where the window's outer top-left corner goes, in physical pixels of
    /// the desktop ([`Monitors`]). `None` lets the platform place it. A
    /// platform that cannot position windows (Wayland) ignores it.
    pub position: Option<IVec2>,
    /// Smallest inner size, in logical pixels.
    pub min_size: Option<Vec2>,
    /// Largest inner size, in logical pixels.
    pub max_size: Option<Vec2>,
    /// The window this one belongs to: a [`WindowKind::Dialog`] or
    /// [`WindowKind::Tool`] window stays above its owner and minimizes
    /// with it. `None` for a top-level window.
    pub owner: Option<Entity>,
}

impl Default for Window {
    fn default() -> Self {
        Self {
            title: String::new(),
            size: Vec2::new(800.0, 600.0),
            kind: WindowKind::Normal,
            visible: true,
            resizable: true,
            decorations: true,
            position: None,
            min_size: None,
            max_size: None,
            owner: None,
        }
    }
}

impl Window {
    /// A shown, resizable, decorated top-level window titled `title` with an
    /// inner size of `size` logical pixels.
    pub fn new(title: impl Into<String>, size: Vec2) -> Self {
        Self {
            title: title.into(),
            size,
            ..Self::default()
        }
    }
}

/// The size and scale a window lays out and draws at, in both worlds.
///
/// Main world: on every window entity, written by the window backend (or the
/// headless stand-in) whenever the platform window's size or scale changes.
/// Layout solves the window's roots against [`Self::size`], media queries in
/// the window evaluate against it, and the window's frame is
/// `size * scale_factor` physical pixels. Render world: on the window's
/// render-side entity ([`crate::render_world::RenderWindow`]), copied across
/// by the extract step, where the renderer reads it.
#[derive(Component, Clone, Debug, PartialEq)]
pub struct WindowViewport {
    /// Inner size in logical pixels: the platform's physical size divided
    /// by [`Self::scale_factor`], the coordinate space CSS and pointer
    /// events use.
    pub size: Vec2,
    /// Logical to physical multiplier of the monitor the window is on.
    /// `1.0` until a backend reports one, so code can multiply through
    /// unconditionally.
    pub scale_factor: f32,
    /// Background the window's frame is cleared to, behind every pixel the
    /// window's content leaves uncovered.
    pub clear: Color,
}

impl Default for WindowViewport {
    fn default() -> Self {
        Self {
            size: Vec2::new(800.0, 600.0),
            scale_factor: 1.0,
            clear: Color::rgba(0.0, 0.0, 0.0, 0.0),
        }
    }
}

impl WindowViewport {
    /// The frame's size in physical pixels, at least one each way.
    pub fn physical_size(&self) -> (u32, u32) {
        let dpr = self.scale_factor.max(0.01);
        (
            (self.size.x * dpr).max(1.0) as u32,
            (self.size.y * dpr).max(1.0) as u32,
        )
    }
}

/// What the platform reports about a window, written by the window backend.
#[derive(Component, Clone, Debug, Default, PartialEq)]
pub struct WindowRuntime {
    /// The window has keyboard focus.
    pub focused: bool,
    /// The window is fully covered or off-screen: it presents nothing.
    pub occluded: bool,
    /// The window is minimized.
    pub minimized: bool,
    /// The window is maximized.
    pub maximized: bool,
    /// The outer top-left corner in physical desktop pixels, where the
    /// platform tells (not on Wayland).
    pub position: Option<IVec2>,
    /// The name of the monitor the window is on, as [`Monitors`] lists it.
    pub monitor: Option<String>,
}

/// Marks the app's first window: the one a launch opens, the one the
/// document root shows in, and the window an element with no surface root
/// above it belongs to.
#[derive(Component, Clone, Copy, Debug, Default)]
pub struct PrimaryWindow;

/// A name for a window, for tools and scripts that pick a window by name
/// (`lumenc screenshot --window <name>`, the MCP `window` argument).
#[derive(Component, Clone, Debug, PartialEq, Eq, Hash)]
pub struct WindowName(pub String);

/// Marks an element as a surface root: it and its subtree show in the window
/// `0` names.
///
/// The element keeps its place in the tree. It is laid out as a layout root
/// of its own against the window's viewport and escapes its ancestors' clip,
/// transform, opacity, scroll offset and `display: none`; inherited styles,
/// event bubbling and pseudo-class chains go through it as through any other
/// element. Despawning the window despawns its surface roots; an owner that
/// wants to keep the content moves it out on [`WindowClosing`].
#[derive(Component, Clone, Copy, Debug, PartialEq, Eq)]
#[relationship(relationship_target = SurfaceRoots)]
pub struct WindowOf(#[entities] pub Entity);

impl WindowOf {
    /// The window this surface root shows in.
    pub fn window(&self) -> Entity {
        self.0
    }
}

/// The surface roots that show in a window. Maintained by bevy from
/// [`WindowOf`]; never written directly.
#[derive(Component, Debug, Default, PartialEq, Eq)]
#[relationship_target(relationship = WindowOf, linked_spawn)]
pub struct SurfaceRoots(Vec<Entity>);

impl SurfaceRoots {
    /// The surface roots, in the order they were added.
    pub fn roots(&self) -> &[Entity] {
        &self.0
    }
}

/// The element that last had keyboard focus in a window, kept while another
/// window has it so focus returns there when the window does.
#[derive(Component, Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct LastFocus(pub Option<Entity>);

/// Marks a window whose input is refused while another window is modal over
/// it. Pointer and key input to it, and assistive-technology actions on its
/// elements, are dropped; a press on it raises the window that blocks it.
#[derive(Component, Clone, Copy, Debug, PartialEq, Eq)]
pub struct InputBlocked {
    /// The window that blocks this one, raised when the user presses here.
    pub by: Entity,
}

/// One monitor of the desktop.
#[derive(Clone, Debug, PartialEq)]
pub struct MonitorInfo {
    /// The name the platform gives the monitor.
    pub name: String,
    /// Top-left corner in physical desktop pixels.
    pub position: IVec2,
    /// Size in physical pixels.
    pub size: glam::UVec2,
    /// The part of the monitor windows are placed in (no task bar, no
    /// dock), in physical desktop pixels: `(top-left, size)`.
    pub work_area: (IVec2, glam::UVec2),
    /// Logical to physical multiplier of the monitor.
    pub scale: f32,
}

impl MonitorInfo {
    /// Whether the physical desktop point `p` lies on this monitor.
    pub fn contains(&self, p: IVec2) -> bool {
        let end = self.position + self.size.as_ivec2();
        p.x >= self.position.x && p.y >= self.position.y && p.x < end.x && p.y < end.y
    }
}

/// The desktop's monitors, in the platform's order; the first is the
/// primary monitor. Written by the window backend (or the headless stand-in)
/// for placing windows and remembering where they were.
#[derive(Resource, Clone, Debug, Default, PartialEq)]
pub struct Monitors(pub Vec<MonitorInfo>);

impl Monitors {
    /// The monitor the physical desktop point `p` is on.
    pub fn at(&self, p: IVec2) -> Option<&MonitorInfo> {
        self.0.iter().find(|m| m.contains(p))
    }

    /// The monitor called `name`.
    pub fn named(&self, name: &str) -> Option<&MonitorInfo> {
        self.0.iter().find(|m| m.name == name)
    }
}

/// What closing the primary window does.
#[derive(Resource, Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum QuitPolicy {
    /// Closing the primary window quits the app, and every other window
    /// closes with it (`[app] quit = "primary"`, the default).
    #[default]
    Primary,
    /// Closing the primary window hides it and every other window; the app
    /// keeps running until it calls `app.quit()` (`[app] quit = "explicit"`,
    /// for tray apps).
    Explicit,
}

/// The primary window's entity, if the app has one.
pub fn primary_window(world: &mut World) -> Option<Entity> {
    world
        .query_filtered::<Entity, With<PrimaryWindow>>()
        .iter(world)
        .next()
}

/// The primary window's entity, read through [`WindowPartitions`] so a
/// shared borrow of the world is enough.
pub fn primary_entity(world: &World) -> Option<Entity> {
    world.get_resource::<WindowPartitions>()?.primary()
}

/// The primary window's viewport, read through [`WindowPartitions`] so a
/// shared borrow of the world is enough.
pub fn primary_viewport(world: &World) -> Option<&WindowViewport> {
    let primary = world.get_resource::<WindowPartitions>()?.primary()?;
    world.get::<WindowViewport>(primary)
}

/// The primary window's viewport, to write: what a test or a runner sizes
/// the app's window through.
///
/// # Panics
///
/// When the world has no primary window ([`crate::app::App::new`] spawns
/// one; a bare world gets one from [`spawn_primary_window`]).
pub fn primary_viewport_mut(world: &mut World) -> Mut<'_, WindowViewport> {
    let primary = world
        .get_resource::<WindowPartitions>()
        .and_then(WindowPartitions::primary)
        .expect("the world has a primary window");
    world
        .get_mut::<WindowViewport>(primary)
        .expect("a window carries a viewport")
}

/// Spawn the primary window into `world` with `viewport`, and the
/// [`WindowPartitions`] that point at it. [`crate::app::App::new`] does this
/// for an app; a test building systems on a bare world calls it to give them
/// a window to lay out and draw in.
pub fn spawn_primary_window(world: &mut World, viewport: WindowViewport) -> Entity {
    let window = world
        .spawn((
            Window {
                size: viewport.size,
                ..Window::default()
            },
            viewport,
            PrimaryWindow,
        ))
        .id();
    world.insert_resource(WindowPartitions::new(window));
    window
}

/// Which window each element of the document shows in.
///
/// Elements under a surface root ([`WindowOf`]), the root included, are
/// listed with their window; every other element is in the primary window.
/// With one window, which is every app that never opens a second, the map is
/// empty and every question answers "the primary window" without a lookup.
///
/// Refreshed by [`sync_window_partitions`] at the start of the tick and again
/// before layout, so a reparent made by a system this tick is laid out in the
/// right window.
#[derive(Resource, Debug, Default, Clone)]
pub struct WindowPartitions {
    primary: Option<Entity>,
    map: HashMap<Entity, Entity>,
    roots: Vec<(Entity, Entity)>,
}

impl WindowPartitions {
    /// Partitions of an app whose only window is `primary`.
    pub fn new(primary: Entity) -> Self {
        Self {
            primary: Some(primary),
            map: HashMap::new(),
            roots: Vec::new(),
        }
    }

    /// The primary window.
    pub fn primary(&self) -> Option<Entity> {
        self.primary
    }

    /// Whether every element is in the primary window: no surface root
    /// exists. Walkers take their one-window path on this.
    pub fn is_single(&self) -> bool {
        self.roots.is_empty()
    }

    /// The window `element` shows in: its nearest surface root's window, or
    /// the primary window.
    pub fn window_of(&self, element: Entity) -> Option<Entity> {
        self.map.get(&element).copied().or(self.primary)
    }

    /// Whether `element` shows in `window`.
    pub fn in_window(&self, element: Entity, window: Entity) -> bool {
        self.window_of(element) == Some(window)
    }

    /// Whether `element` shows in a window other than the primary one.
    pub fn is_secondary(&self, element: Entity) -> bool {
        self.map.contains_key(&element)
    }

    /// Every surface root with its window, in entity order.
    pub fn surface_roots(&self) -> &[(Entity, Entity)] {
        &self.roots
    }

    /// The surface roots that show in `window`.
    pub fn roots_of(&self, window: Entity) -> impl Iterator<Item = Entity> + '_ {
        self.roots
            .iter()
            .filter(move |(_, w)| *w == window)
            .map(|(r, _)| *r)
    }
}

/// Refresh [`WindowPartitions`] from the surface roots in the world.
///
/// With no surface root this checks one empty query and returns, writing
/// nothing; otherwise it walks each surface root's subtree, stopping at the
/// surface roots nested inside it, which own their own subtrees.
pub fn sync_window_partitions(
    partitions: ResMut<WindowPartitions>,
    roots: Query<(Entity, &WindowOf)>,
    children: Query<&Children>,
    primary: Query<Entity, With<PrimaryWindow>>,
) {
    let primary = primary.iter().next();
    if roots.is_empty() {
        if !partitions.roots.is_empty() || partitions.primary != primary {
            let parts = partitions.into_inner();
            parts.map.clear();
            parts.roots.clear();
            parts.primary = primary;
        }
        return;
    }
    let mut found: Vec<(Entity, Entity)> = roots.iter().map(|(e, w)| (e, w.window())).collect();
    found.sort_by_key(|(e, _)| *e);
    let mut map = HashMap::new();
    for &(root, window) in &found {
        let mut stack = vec![root];
        while let Some(e) = stack.pop() {
            if e != root && roots.contains(e) {
                // A nested surface root fills its own subtree.
                continue;
            }
            map.insert(e, window);
            if let Ok(kids) = children.get(e) {
                stack.extend(kids.iter());
            }
        }
    }
    if partitions.map != map || partitions.roots != found || partitions.primary != primary {
        let parts = partitions.into_inner();
        parts.map = map;
        parts.roots = found;
        parts.primary = primary;
    }
}

/// The window viewports, looked up by window or by the element that shows in
/// one.
#[derive(SystemParam)]
pub struct Viewports<'w, 's> {
    partitions: Option<Res<'w, WindowPartitions>>,
    viewports: Query<'w, 's, &'static WindowViewport>,
}

impl Viewports<'_, '_> {
    /// The primary window's viewport.
    pub fn primary(&self) -> Option<&WindowViewport> {
        let primary = self.partitions.as_ref()?.primary()?;
        self.viewports.get(primary).ok()
    }

    /// The viewport of `window`.
    pub fn of_window(&self, window: Entity) -> Option<&WindowViewport> {
        self.viewports.get(window).ok()
    }

    /// The viewport of the window `element` shows in.
    pub fn of(&self, element: Entity) -> Option<&WindowViewport> {
        let window = self.partitions.as_ref()?.window_of(element)?;
        self.viewports.get(window).ok()
    }

    /// The window `element` shows in.
    pub fn window_of(&self, element: Entity) -> Option<Entity> {
        self.partitions.as_ref()?.window_of(element)
    }

    /// Whether every element is in the primary window.
    pub fn is_single(&self) -> bool {
        self.partitions.as_ref().is_none_or(|p| p.is_single())
    }
}

/// A request from the app to its window backend.
///
/// The backend reads these after each tick and obeys them; a run with no
/// display obeys them the same way, so a script that closes its window
/// behaves alike headless and on screen.
#[derive(Message, Clone, Copy, Debug, PartialEq, Eq)]
pub enum WindowRequest {
    /// Close the window (`window.close()`). The close runs the same veto
    /// path the window's close button does: [`crate::input::CloseRequest`]
    /// with [`crate::input::CloseReason::WindowClose`], and an `on_close`
    /// hook can keep the window open.
    Close(Entity),
    /// Raise the window and give it keyboard focus: an element in it was
    /// focused from another window, or a blocked window was pressed.
    Focus(Entity),
}

/// A request from the app to quit (`app.quit()`).
///
/// Every window is asked to close through the
/// [`crate::input::CloseRequest`] veto path, with
/// [`crate::input::CloseReason::AppQuit`]; a veto cancels the quit.
#[derive(Message, Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct QuitRequest;

/// A secondary window is about to close: its close request was not vetoed.
///
/// Written during the tick before the window entity despawns, so whatever
/// put content into the window (a dock, the devtools popout) can move that
/// content out first. Surface roots still pointing at the window when it
/// despawns go with it.
#[derive(Message, Clone, Copy, Debug, PartialEq, Eq)]
pub struct WindowClosing {
    /// The window that closes.
    pub window: Entity,
}

/// Fallback GPU clear color painted before the very first frame - what a
/// user sees for an instant during window creation, and behind any pixel
/// the app's tree doesn't cover. This is the single Rust-side source of
/// truth for that fallback; `lumen_runtime::run::RunOptions::clear` (the
/// value every launch path reaches - CLI, FFI, SDK) defaults to it too,
/// so the two never drift apart the way they used to.
///
/// A resolved `--lumen-window-bg` custom property (from the app's own
/// `:root` or its active skin) overrides this at build time - see
/// `lumen_runtime::run::app_build::build_app`. This constant is only what
/// remains when no layer defines that token, or a caller constructs
/// [`WindowOptions`] directly without going through `RunOptions` at all.
pub const DEFAULT_CLEAR: Color = Color::rgb(0.07, 0.07, 0.09);

/// Everything a window backend needs to open the app's primary window.
pub struct WindowOptions {
    /// Initial window inner size in logical pixels.
    pub size: (u32, u32),
    /// Window title.
    pub title: String,
    /// Background clear color (the primary window's
    /// [`WindowViewport::clear`]). See [`DEFAULT_CLEAR`] for the fallback
    /// this defaults to.
    pub clear: Color,
    /// Maximize the window on launch. Useful for full-bleed app demos.
    pub maximized: bool,
    /// Suppress OS window chrome (title bar, borders, close/min/max
    /// buttons). Custom title bars and per-platform window drag must
    /// be implemented inside the app - see the `<title-bar drag>`
    /// region.
    pub frameless: bool,
    /// Initial outer position in physical pixels. `None` lets the OS
    /// place the window (the default). Wired up so callers can
    /// restore the last-known window position from disk.
    pub start_position: Option<(i32, i32)>,
    /// Callback invoked once per close with the current window
    /// position, inner size (logical), and maximized flag. `None`
    /// (the default) disables state persistence. Implementations
    /// typically serialise to disk via
    /// `lumenc::window_state::save`.
    pub on_close_state: Option<Box<dyn FnOnce(WindowGeometry) + Send>>,
    /// Optional native menu bar spec. Built by the embedder from
    /// the markup `<menubar>` element. `None` = no menu bar.
    pub menubar: Option<MenuModel>,
}

impl Default for WindowOptions {
    fn default() -> Self {
        Self {
            size: (800, 600),
            title: "Lumen".into(),
            clear: DEFAULT_CLEAR,
            maximized: false,
            frameless: false,
            start_position: None,
            on_close_state: None,
            menubar: None,
        }
    }
}

/// Snapshot of geometry handed to [`WindowOptions::on_close_state`] so
/// the embedder can persist `(position, size, maximized)` to disk.
#[derive(Debug, Clone, Copy)]
pub struct WindowGeometry {
    /// Outer window position in physical pixels (winit
    /// `Window::outer_position`).
    pub position: Option<(i32, i32)>,
    /// Inner window size in logical pixels.
    pub size: (u32, u32),
    /// Last maximized state.
    pub maximized: bool,
}

/// The app's native menu bar, as parsed from `<menubar>`.
#[derive(Debug, Clone, Default)]
pub struct MenuModel {
    /// Top-level submenus, in source order.
    pub menus: Vec<Menu>,
}

/// One submenu (`<menu label="File">...</menu>`).
#[derive(Debug, Clone)]
pub struct Menu {
    /// Display label shown in the menu bar.
    pub label: String,
    /// Items inside the submenu.
    pub items: Vec<MenuEntry>,
}

/// One submenu entry - either a clickable item or a separator.
#[derive(Debug, Clone)]
pub enum MenuEntry {
    /// `<menuitem id label accel?>`.
    Item {
        /// `id="..."` - dispatched as `MenuClicked { id }` to scripts.
        id: String,
        /// Display label.
        label: String,
        /// Optional accelerator string in muda format.
        accelerator: Option<String>,
    },
    /// `<separator />`.
    Separator,
}

#[cfg(test)]
mod tests {
    use super::*;
    use bevy_ecs::hierarchy::ChildOf;
    use bevy_ecs::schedule::Schedule;

    fn partitions(world: &mut World) -> WindowPartitions {
        let mut schedule = Schedule::default();
        schedule.add_systems(sync_window_partitions);
        schedule.run(world);
        world.resource::<WindowPartitions>().clone()
    }

    /// A window comes with the viewport and runtime state it needs, and a
    /// bare world gets a primary window the viewport helpers find.
    #[test]
    fn a_window_brings_its_viewport_and_runtime() {
        let mut world = World::new();
        let primary = spawn_primary_window(
            &mut world,
            WindowViewport {
                size: Vec2::new(320.0, 200.0),
                ..WindowViewport::default()
            },
        );
        assert_eq!(primary_window(&mut world), Some(primary));
        assert_eq!(
            primary_viewport(&world).map(|v| v.size),
            Some(Vec2::new(320.0, 200.0))
        );
        primary_viewport_mut(&mut world).scale_factor = 2.0;
        assert_eq!(
            primary_viewport(&world).unwrap().physical_size(),
            (640, 400)
        );

        let second = world
            .spawn(Window::new("Second", Vec2::new(10.0, 10.0)))
            .id();
        assert!(world.get::<WindowViewport>(second).is_some());
        assert_eq!(
            world.get::<WindowRuntime>(second),
            Some(&WindowRuntime::default())
        );
    }

    /// With no surface root every element is in the primary window and the
    /// partitions stay empty; a surface root claims its subtree down to a
    /// nested surface root, which claims its own.
    #[test]
    fn surface_roots_partition_the_tree() {
        let mut world = World::new();
        let primary = spawn_primary_window(&mut world, WindowViewport::default());
        let root = world.spawn_empty().id();
        let dock = world.spawn(ChildOf(root)).id();
        let leaf = world.spawn(ChildOf(dock)).id();
        let parts = partitions(&mut world);
        assert!(parts.is_single());
        assert_eq!(parts.window_of(leaf), Some(primary));

        let popout = world.spawn(Window::default()).id();
        let inner = world.spawn(Window::default()).id();
        let surface = world.spawn((ChildOf(dock), WindowOf(popout))).id();
        let panel = world.spawn(ChildOf(surface)).id();
        let nested = world.spawn((ChildOf(panel), WindowOf(inner))).id();
        let deep = world.spawn(ChildOf(nested)).id();
        let parts = partitions(&mut world);
        assert!(!parts.is_single());
        assert_eq!(parts.window_of(dock), Some(primary));
        assert_eq!(parts.window_of(surface), Some(popout));
        assert_eq!(parts.window_of(panel), Some(popout));
        assert!(parts.in_window(deep, inner));
        assert!(parts.is_secondary(nested));
        assert!(!parts.is_secondary(leaf));
        assert_eq!(parts.roots_of(popout).collect::<Vec<_>>(), vec![surface]);
        assert_eq!(parts.surface_roots().len(), 2);

        // Closing the window takes its surface roots with it, and the
        // partitions fall back to one window.
        world.entity_mut(inner).despawn();
        world.entity_mut(popout).despawn();
        assert!(world.get_entity(surface).is_err());
        let parts = partitions(&mut world);
        assert!(parts.is_single());
        assert_eq!(parts.window_of(leaf), Some(primary));
    }

    /// Monitors answer which one a desktop point is on, and by name.
    #[test]
    fn monitors_place_a_point() {
        let monitors = Monitors(vec![
            MonitorInfo {
                name: "left".into(),
                position: IVec2::ZERO,
                size: glam::UVec2::new(1920, 1080),
                work_area: (IVec2::ZERO, glam::UVec2::new(1920, 1040)),
                scale: 1.0,
            },
            MonitorInfo {
                name: "right".into(),
                position: IVec2::new(1920, 0),
                size: glam::UVec2::new(2560, 1440),
                work_area: (IVec2::new(1920, 0), glam::UVec2::new(2560, 1440)),
                scale: 2.0,
            },
        ]);
        assert_eq!(monitors.at(IVec2::new(10, 10)).unwrap().name, "left");
        assert_eq!(monitors.at(IVec2::new(2000, 10)).unwrap().scale, 2.0);
        assert!(monitors.at(IVec2::new(-1, 0)).is_none());
        assert_eq!(monitors.named("right").unwrap().position.x, 1920);
    }
}
