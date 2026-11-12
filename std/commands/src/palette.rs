//! The command palette: a popover near the top of the window holding a
//! search field and the enabled commands whose labels hold every word typed,
//! each with its chord. The arrow keys move the highlight, Enter runs the
//! highlighted command, Escape closes it.
//!
//! A `<command-palette>` element is where it sits: a strip across the top
//! of the window that the panel opens below, centred. An app that writes
//! none gets one under its root the first time the palette opens.

use bevy_ecs::hierarchy::Children;
use bevy_ecs::prelude::*;
use bevy_ecs::system::RunSystemOnce;
use lumen_module::lumen_core::components::{DomHidden, LumenClasses, StyleDirty, TextContent};
use lumen_module::lumen_core::input::{FocusTracker, Focused, Key, NamedKey};
use lumen_module::lumen_core::property_store::PropertyStore;
use lumen_module::lumen_ir::layout_ir::{
    Attributes, Element, IfModeSpec, PopoverModeSpec, PopoverSpec,
};
use lumen_module::lumen_primitives::Popover;
use lumen_module::lumen_scene::spawn::{Placeholders, spawn_subtree};

use crate::model::Shared;
use crate::plugin::{parent, text};

/// The signal the palette's open state lives in.
pub const OPEN_SIGNAL: &str = "__popover_open:lumen-command-palette";

/// The palette's search field.
#[derive(Component, Clone, Copy, Debug, Default)]
pub struct PaletteInput;

/// One row of the palette's list, and the command it runs.
#[derive(Component, Clone, Debug)]
pub struct PaletteRow(pub String);

/// The palette's parts and state.
#[derive(Resource, Debug, Default)]
pub struct Palette {
    host: Option<Entity>,
    panel: Option<Entity>,
    input: Option<Entity>,
    list: Option<Entity>,
    rows: Vec<(Entity, String)>,
    highlight: usize,
    open: bool,
    return_focus: Option<Entity>,
}

impl Palette {
    /// Whether the palette is open.
    pub fn is_open(&self) -> bool {
        self.open
    }

    /// The commands its list shows, in order.
    pub fn shown(&self) -> Vec<String> {
        self.rows.iter().map(|(_, id)| id.clone()).collect()
    }

    /// The command its highlight is on.
    pub fn highlighted(&self) -> Option<&str> {
        self.rows.get(self.highlight).map(|(_, id)| id.as_str())
    }
}

/// Whether the palette is open.
pub fn is_open(world: &World) -> bool {
    world
        .get_resource::<Palette>()
        .is_some_and(Palette::is_open)
}

fn element(tag: &str, class: &str) -> Element {
    Element {
        tag: tag.to_string(),
        attrs: Box::new(Attributes {
            classes: vec![class.to_string()],
            ..Attributes::default()
        }),
        ..Element::default()
    }
}

fn label(class: &str, value: &str) -> Element {
    let mut el = element("label", class);
    el.attrs.text = Some(value.to_string());
    el
}

/// Take `host` as the palette's place, building its panel inside it. A
/// second `<command-palette>` is left as it is.
pub fn adopt_host(world: &mut World, host: Entity) {
    let current = world.resource::<Palette>().host;
    if current.is_some_and(|h| h != host && world.get_entity(h).is_ok()) {
        lumen_module::lumen_core::warn_line!(
            "lumen-commands: a second <command-palette> is left empty; one is enough"
        );
        return;
    }
    if current == Some(host) && world.resource::<Palette>().panel.is_some() {
        return;
    }
    let mut input = element("input", "command-palette-input");
    input.attrs.placeholder = Some(text("lumen-commands-palette-placeholder", "Type a command"));
    let mut panel = element("column", "command-palette-panel");
    panel.attrs.popover = Some(PopoverSpec {
        open_signal: OPEN_SIGNAL.to_string(),
        mode: PopoverModeSpec::Auto,
        anchor: None,
        match_anchor_width: false,
    });
    panel.attrs.if_signal = Some(OPEN_SIGNAL.to_string());
    panel.attrs.if_mode = IfModeSpec::Mounted;
    // An input starts with empty text, as one the markup wrote does, or the
    // text pipeline has nothing to type into.
    input.attrs.text = Some(String::new());
    // The list scrolls inside its height cap.
    let mut list = element("column", "command-palette-list");
    list.attrs.scroll = Some(lumen_module::lumen_ir::layout_ir::ScrollAxisSpec::Y);
    panel.children = vec![input, list];
    let panel = spawn_subtree(world, &panel, Some(host), Placeholders::Resolved);
    let _ = world.run_system_once(lumen_module::lumen_scene::spawn::reconcile_if_blocks);
    if let Some(mut p) = world.get_mut::<Popover>(panel) {
        // Placed against the strip the host draws across the window.
        p.invoker = Some(host);
    }
    let mut input = None;
    let mut list = None;
    for e in subtree(world, panel) {
        let class = |c: &str| {
            world
                .get::<LumenClasses>(e)
                .is_some_and(|l| l.0.iter().any(|x| &**x == c))
        };
        if class("command-palette-input") {
            input = Some(e);
        } else if class("command-palette-list") {
            list = Some(e);
        }
        world.entity_mut(e).insert((DomHidden, StyleDirty));
    }
    if let Some(i) = input {
        world.entity_mut(i).insert(PaletteInput);
    }
    let mut palette = world.resource_mut::<Palette>();
    palette.host = Some(host);
    palette.panel = Some(panel);
    palette.input = input;
    palette.list = list;
}

fn subtree(world: &World, root: Entity) -> Vec<Entity> {
    let mut out = Vec::new();
    let mut stack = vec![root];
    while let Some(e) = stack.pop() {
        out.push(e);
        if let Some(kids) = world.get::<Children>(e) {
            stack.extend(kids.iter());
        }
    }
    out
}

/// Make a `<command-palette>` under the document root, for an app that
/// wrote none.
fn ensure_host(world: &mut World) -> bool {
    let has = world
        .resource::<Palette>()
        .host
        .is_some_and(|h| world.get_entity(h).is_ok());
    if has {
        return true;
    }
    let Some(root) = world
        .get_resource::<lumen_module::lumen_scene::spawn::DocumentRoot>()
        .map(|r| r.0)
    else {
        return false;
    };
    let host = spawn_subtree(
        world,
        &Element {
            tag: "command-palette".to_string(),
            ..Element::default()
        },
        Some(root),
        Placeholders::Resolved,
    );
    world.entity_mut(host).insert((DomHidden, StyleDirty));
    // The tag observer queued it too; adopting twice is a no-op.
    adopt_host(world, host);
    true
}

/// Open the palette with an empty search and focus in its field.
pub fn open(world: &mut World, shared: &Shared) {
    if is_open(world) || !ensure_host(world) {
        return;
    }
    let focus = world.resource::<FocusTracker>().0;
    let input = world.resource::<Palette>().input;
    {
        let mut p = world.resource_mut::<Palette>();
        p.open = true;
        p.highlight = 0;
        p.return_focus = focus;
    }
    world
        .resource_mut::<PropertyStore>()
        .set_global_bool(OPEN_SIGNAL, true);
    if let Some(i) = input {
        set_text(world, i, "");
        move_focus(world, Some(i));
    }
    refresh(world, shared);
}

/// Close the palette and give focus back to where it was.
pub fn close(world: &mut World) {
    if !is_open(world) {
        return;
    }
    world
        .resource_mut::<PropertyStore>()
        .set_global_bool(OPEN_SIGNAL, false);
    closed(world);
}

/// The palette closed: by a call, or by a press outside it.
fn closed(world: &mut World) {
    let back = {
        let mut p = world.resource_mut::<Palette>();
        p.open = false;
        p.return_focus.take()
    };
    let current = world.resource::<FocusTracker>().0;
    let input = world.resource::<Palette>().input;
    if current.is_none() || current == input {
        move_focus(world, back.filter(|b| world.get_entity(*b).is_ok()));
    }
}

/// Notice the palette closing on its own (a press outside it).
pub fn follow_signal(world: &mut World) {
    if is_open(world)
        && world
            .resource::<PropertyStore>()
            .get_global_bool(OPEN_SIGNAL)
            != Some(true)
    {
        closed(world);
    }
}

fn move_focus(world: &mut World, to: Option<Entity>) {
    let prev = world.resource::<FocusTracker>().0;
    if prev == to {
        return;
    }
    if let Some(prev) = prev
        && let Ok(mut e) = world.get_entity_mut(prev)
    {
        e.remove::<Focused>();
    }
    if let Some(to) = to {
        world.entity_mut(to).insert(Focused);
    }
    world.resource_mut::<FocusTracker>().0 = to;
}

fn set_text(world: &mut World, e: Entity, value: &str) {
    if world.get::<TextContent>(e).is_some_and(|t| t.0 != value) {
        world.entity_mut(e).insert(TextContent(value.to_string()));
        if let Some(mut input) = world.get_mut::<lumen_module::lumen_core::components::TextInput>(e)
        {
            input.cursor = 0;
            input.selection_anchor = None;
        }
    }
}

/// Answer a key while the palette is open; whether it was the palette's.
pub fn hear_key(world: &mut World, shared: &Shared, key: &Key) -> bool {
    match key {
        Key::Named(NamedKey::Escape) => {
            close(world);
            true
        }
        Key::Named(NamedKey::ArrowDown | NamedKey::ArrowUp) => {
            let down = *key == Key::Named(NamedKey::ArrowDown);
            {
                let mut p = world.resource_mut::<Palette>();
                let n = p.rows.len();
                if n > 0 {
                    p.highlight = if down {
                        (p.highlight + 1) % n
                    } else {
                        (p.highlight + n - 1) % n
                    };
                }
            }
            mark_highlight(world);
            true
        }
        Key::Named(NamedKey::Enter) => {
            let id = world
                .resource::<Palette>()
                .highlighted()
                .map(str::to_string);
            close(world);
            if let Some(id) = id {
                shared.lock().run(&id);
            }
            true
        }
        _ => false,
    }
}

/// The command of the palette row `clicked` is in.
pub fn row_at(world: &World, clicked: Entity) -> Option<String> {
    let mut cur = Some(clicked);
    while let Some(e) = cur {
        if let Some(r) = world.get::<PaletteRow>(e) {
            return Some(r.0.clone());
        }
        cur = parent(world, e);
    }
    None
}

/// Rebuild the open palette's list from its search and the registry.
pub fn refresh(world: &mut World, shared: &Shared) {
    if !is_open(world) {
        return;
    }
    let (input, list, old) = {
        let mut p = world.resource_mut::<Palette>();
        (p.input, p.list, std::mem::take(&mut p.rows))
    };
    let Some(list) = list else { return };
    let query = input
        .and_then(|i| world.get::<TextContent>(i))
        .map(|t| t.0.clone())
        .unwrap_or_default();
    let matches = shared.lock().matching(&query);
    for (e, _) in old {
        if let Ok(e) = world.get_entity_mut(e) {
            e.despawn();
        }
    }
    let leftovers: Vec<Entity> = world
        .get::<Children>(list)
        .map(|c| c.to_vec())
        .unwrap_or_default();
    for e in leftovers {
        world.entity_mut(e).despawn();
    }
    let mut rows = Vec::new();
    if matches.is_empty() {
        let empty = label(
            "command-palette-empty",
            &text("lumen-commands-palette-empty", "No commands"),
        );
        let e = spawn_subtree(world, &empty, Some(list), Placeholders::Resolved);
        world.entity_mut(e).insert((DomHidden, StyleDirty));
    }
    for (id, name, chord) in matches {
        let mut row = element("row", "command-palette-item");
        row.children = vec![
            label("command-palette-label", &name),
            label("command-palette-keys", &chord),
        ];
        let e = spawn_subtree(world, &row, Some(list), Placeholders::Resolved);
        for part in subtree(world, e) {
            world.entity_mut(part).insert((DomHidden, StyleDirty));
        }
        world.entity_mut(e).insert(PaletteRow(id.clone()));
        rows.push((e, id));
    }
    {
        let mut p = world.resource_mut::<Palette>();
        p.rows = rows;
        p.highlight = 0;
    }
    // The list is a scroll container, which layout re-solves inside the box
    // it already had (lumen-fx/lumen#634); its height follows its rows up to
    // the cap, so the panel around it lays out again.
    if let Some(panel) = parent(world, list) {
        world
            .entity_mut(panel)
            .insert(lumen_module::lumen_core::components::DirtyLayout);
    }
    mark_highlight(world);
}

/// Put the `highlighted` class on the highlighted row only.
fn mark_highlight(world: &mut World) {
    let (rows, at) = {
        let p = world.resource::<Palette>();
        (p.rows.clone(), p.highlight)
    };
    for (i, (e, _)) in rows.iter().enumerate() {
        let on = i == at;
        let Some(mut classes) = world.get::<LumenClasses>(*e).cloned() else {
            continue;
        };
        let has = classes.0.iter().any(|c| &**c == "highlighted");
        if has != on {
            if on {
                classes.0.push("highlighted".into());
            } else {
                classes.0.retain(|c| &**c != "highlighted");
            }
            world.entity_mut(*e).insert((classes, StyleDirty));
        }
    }
}
