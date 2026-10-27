//! A tab strip with more tabs than room.
//!
//! Tabs shrink to their CSS `min-width` first. Past that the strip scrolls:
//! the wheel over it moves it, the shown tab is kept in view, and a button
//! at the strip's end lists every tab of the stack in a popup. The button
//! exists only while the strip overflows. The strip clips its tabs and the
//! module moves them along, so nothing scrolls, measures or repaints while
//! no strip changes.

use bevy_ecs::hierarchy::ChildOf;
use bevy_ecs::prelude::*;
use glam::Vec2;
use lumen_module::lumen_core::components::{
    A11yLabel, A11yRole, LumenClasses, TextContent, Transform,
};
use lumen_module::lumen_ir::menu::MenuRow;
use lumen_module::lumen_primitives::TabStripButton;

use crate::adopt::{hide, spawn_chrome, unhide};
use crate::realize::{ACTIVE_TAB, DockStack, alive, children_of, set_classes, set_inline};
use crate::state::Shared;

/// On a dock stack's tab strip: the stack it belongs to.
#[derive(Component, Clone, Copy, Debug, PartialEq, Eq)]
pub struct DockStrip {
    /// The stack.
    pub stack: Entity,
}

/// How far a strip is scrolled, and what that was measured against.
#[derive(Component, Clone, Copy, Debug, Default, PartialEq)]
pub struct StripScroll {
    /// Logical pixels the tabs are moved left by.
    pub offset: f32,
    /// The button the offset is written on.
    pub first: Option<Entity>,
    /// The shown tab's button when the strip was last fitted.
    pub active: Option<Entity>,
    /// Whether the strip overflowed.
    pub overflowing: bool,
}

/// On a stack's overflow button: the stack.
#[derive(Component, Clone, Copy, Debug, PartialEq, Eq)]
pub struct DockOverflowButton {
    /// The stack.
    pub stack: Entity,
}

/// On an item of a stack's tab list: the panel it shows.
#[derive(Component, Clone, Debug, PartialEq, Eq)]
pub struct DockOverflowItem(pub String);

/// The id of the tab list popup.
pub const LIST_ID: &str = "lumen-dock-tab-list";

/// The overflow button's mark, a right-pointing double angle, until a
/// stylesheet draws its own.
const MARK: &str = "\u{bb}";

/// The width the overflow button takes when the stylesheet has not laid it
/// out yet.
const BUTTON_WIDTH: f32 = 28.0;

/// The panel the tab list item `e` shows, if `e` is one or lies in one.
pub fn item_panel(
    items: &Query<&DockOverflowItem>,
    parents: &Query<&ChildOf>,
    e: Entity,
) -> Option<String> {
    let mut cur = Some(e);
    for _ in 0..3 {
        let c = cur?;
        if let Ok(item) = items.get(c) {
            return Some(item.0.clone());
        }
        cur = parents.get(c).ok().map(ChildOf::parent);
    }
    None
}

/// Where a strip's tabs sit: the strip's rectangle and each button's, in
/// window coordinates, with the shown one marked.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct StripGeometry {
    /// The strip's left edge and width.
    pub strip: (f32, f32),
    /// Each button's left edge and width, in order, the offset applied.
    pub buttons: Vec<(f32, f32)>,
    /// The index of the shown tab's button.
    pub active: Option<usize>,
}

/// What fitting a strip decides.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Fit {
    /// Whether the tabs do not all fit.
    pub overflowing: bool,
    /// How far the tabs are moved left.
    pub offset: f32,
}

/// Fit a strip: whether it overflows, and how far to move its tabs so the
/// offset stays in range, the wheel's `scroll` applies, and the shown tab
/// stays in view when `follow_active` says it changed. `offset` is the one
/// in effect when `g` was measured; `button` is the overflow button's width.
pub fn fit(g: &StripGeometry, offset: f32, scroll: f32, follow_active: bool, button: f32) -> Fit {
    let (Some(first), Some(last)) = (g.buttons.first(), g.buttons.last()) else {
        return Fit {
            overflowing: false,
            offset: 0.0,
        };
    };
    // Where the tabs start when not moved, and how wide they are together.
    let start = first.0 + offset;
    let content = last.0 + last.1 - first.0;
    let room = g.strip.0 + g.strip.1 - start;
    let overflowing = content > room + 0.5;
    let visible = if overflowing {
        (room - button).max(0.0)
    } else {
        room
    };
    let max = (content - visible).max(0.0);
    let mut offset = (offset + scroll).clamp(0.0, max);
    if follow_active
        && let Some(i) = g.active
        && let Some(&(x, w)) = g.buttons.get(i)
    {
        // The tab's place from the start of the tabs, unmoved.
        let left = x - first.0;
        if left < offset {
            offset = left;
        } else if left + w > offset + visible {
            offset = (left + w - visible).min(max);
        }
    }
    if !overflowing {
        offset = 0.0;
    }
    Fit {
        overflowing,
        offset,
    }
}

fn rect(world: &World, e: Entity) -> Option<(Vec2, Vec2)> {
    let t = world.get::<Transform>(e)?;
    Some((t.absolute, t.size))
}

/// The overflow button of `stack`, if it has one.
fn button_of(world: &World, stack: Entity) -> Option<Entity> {
    children_of(world, stack)
        .into_iter()
        .find(|&k| world.get::<DockOverflowButton>(k).is_some())
}

/// Fit every dock strip to its tabs. `wheel` is a wheel turn over the strip
/// it names, in logical pixels along the strip.
pub fn fit_strips(world: &mut World, wheel: Option<(Entity, f32)>) {
    let mut q = world.query::<(Entity, &DockStrip)>();
    let strips: Vec<(Entity, Entity)> = q.iter(world).map(|(e, s)| (e, s.stack)).collect();
    for (strip, stack) in strips {
        if !alive(world, stack) || !alive(world, strip) {
            continue;
        }
        let Some((so, ss)) = rect(world, strip) else {
            continue;
        };
        let buttons: Vec<Entity> = children_of(world, strip)
            .into_iter()
            .filter(|&b| world.get::<TabStripButton>(b).is_some())
            .collect();
        let state = world.get::<StripScroll>(strip).copied().unwrap_or_default();
        let active_i = buttons.iter().position(|&b| {
            world
                .get::<LumenClasses>(b)
                .is_some_and(|c| c.has(ACTIVE_TAB))
        });
        let g = StripGeometry {
            strip: (so.x, ss.x),
            buttons: buttons
                .iter()
                .filter_map(|&b| rect(world, b).map(|(o, s)| (o.x, s.x)))
                .collect(),
            active: active_i,
        };
        if g.buttons.len() != buttons.len() {
            continue;
        }
        let active = active_i.map(|i| buttons[i]);
        let scroll = wheel.filter(|(s, _)| *s == strip).map_or(0.0, |(_, d)| d);
        let button_w = button_of(world, stack)
            .and_then(|b| rect(world, b))
            .map(|(_, s)| s.x)
            .filter(|w| *w > 1.0)
            .unwrap_or(BUTTON_WIDTH);
        // The shown tab is brought into view when it changes or the strip
        // starts overflowing, not on every fit: the wheel moves it away.
        let follow = active != state.active || !state.overflowing;
        let f = fit(&g, state.offset, scroll, follow && scroll == 0.0, button_w);
        // The offset rides on the first button's margin.
        let first = buttons.first().copied();
        if state.first != first
            && let Some(old) = state.first
            && alive(world, old)
        {
            set_inline(world, old, "margin", None);
        }
        if let Some(first) = first {
            let margin = (f.offset > 0.0).then(|| format!("0 0 0 {}", -f.offset.round()));
            set_inline(world, first, "margin", margin);
        }
        set_classes(
            world,
            stack,
            if f.overflowing { &["overflowing"] } else { &[] },
            &["overflowing"],
        );
        match (f.overflowing, button_of(world, stack)) {
            (true, Some(b)) => unhide(world, b),
            (true, None) => {
                let b = spawn_chrome(world, "button", &["dock-tabs-overflow"], Some(stack));
                let label = crate::plugin::text("lumen-dock-more-tabs", "More tabs");
                world.entity_mut(b).insert((
                    DockOverflowButton { stack },
                    A11yRole::Button,
                    A11yLabel(label),
                    TextContent(MARK.into()),
                ));
            }
            (false, Some(b)) => hide(world, b),
            (false, None) => {}
        }
        let next = StripScroll {
            offset: f.offset,
            first,
            active,
            overflowing: f.overflowing,
        };
        if world.get::<StripScroll>(strip) != Some(&next) {
            world.entity_mut(strip).insert(next);
        }
    }
}

/// Open the list of every tab of `stack` under its overflow button.
pub fn open_list(world: &mut World, stack: Entity) {
    let Some(id) = world.get::<DockStack>(stack).map(|s| s.id.clone()) else {
        return;
    };
    let shared = world.resource::<Shared>().clone();
    let panels: Vec<(String, String)> = {
        let core = shared.lock();
        core.ws
            .stack(&id)
            .map(|s| {
                s.panels
                    .iter()
                    .filter_map(|p| {
                        let r = core.panels.get(p)?;
                        r.entity.map(|_| (p.clone(), r.attrs.title.clone()))
                    })
                    .collect()
            })
            .unwrap_or_default()
    };
    let Some((first, _)) = panels.first() else {
        return;
    };
    let Some(container) = crate::menu::popup_container(world, &shared, first) else {
        return;
    };
    let at = button_of(world, stack)
        .and_then(|b| rect(world, b))
        .map_or(Vec2::ZERO, |(o, s)| Vec2::new(o.x + s.x, o.y + s.y));
    let rows: Vec<MenuRow> = panels
        .iter()
        .enumerate()
        .map(|(i, (_, title))| MenuRow::Item {
            id: format!("{LIST_ID}-{i}"),
            label: title.clone(),
            disabled: false,
        })
        .collect();
    let (_, items) = crate::menu::open_popup(world, container, LIST_ID, &rows, at);
    for (e, item) in items {
        let index = item
            .strip_prefix(LIST_ID)
            .and_then(|r| r.strip_prefix('-'))
            .and_then(|n| n.parse::<usize>().ok());
        if let Some((panel, _)) = index.and_then(|i| panels.get(i)) {
            world.entity_mut(e).insert(DockOverflowItem(panel.clone()));
        }
    }
}

/// The dock strip `e` is, or lies in.
pub fn strip_under(world: &World, e: Entity) -> Option<Entity> {
    let mut cur = Some(e);
    for _ in 0..6 {
        let c = cur?;
        if world.get::<DockStrip>(c).is_some() {
            return Some(c);
        }
        cur = world.get::<ChildOf>(c).map(ChildOf::parent);
    }
    None
}

/// Mark the strip of each of the dock's stacks, so it is fitted when it or
/// its tabs change.
pub fn mark_strip(world: &mut World, stack: Entity, strip: Entity) {
    if world.get::<DockStrip>(strip).is_none() {
        world.entity_mut(strip).insert(DockStrip { stack });
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn geometry(widths: &[f32], strip: f32, offset: f32, active: Option<usize>) -> StripGeometry {
        let mut x = 10.0 - offset;
        let mut buttons = Vec::new();
        for w in widths {
            buttons.push((x, *w));
            x += w;
        }
        StripGeometry {
            strip: (10.0, strip),
            buttons,
            active,
        }
    }

    #[test]
    fn a_strip_that_fits_does_not_scroll() {
        let g = geometry(&[50.0, 60.0], 200.0, 0.0, Some(1));
        assert_eq!(
            fit(&g, 0.0, 30.0, true, 28.0),
            Fit {
                overflowing: false,
                offset: 0.0
            }
        );
        assert_eq!(
            fit(&StripGeometry::default(), 5.0, 0.0, true, 28.0),
            Fit {
                overflowing: false,
                offset: 0.0
            }
        );
    }

    #[test]
    fn the_wheel_scrolls_within_the_tabs() {
        // 300 px of tabs in a 200 px strip: 128 visible beside the button
        // would not do; 172 do, so the offset goes up to 128.
        let g = geometry(&[100.0, 100.0, 100.0], 200.0, 0.0, None);
        let f = fit(&g, 0.0, 50.0, false, 28.0);
        assert!(f.overflowing);
        assert_eq!(f.offset, 50.0);
        assert_eq!(fit(&g, 0.0, 500.0, false, 28.0).offset, 128.0);
        assert_eq!(fit(&g, 0.0, -20.0, false, 28.0).offset, 0.0);
    }

    #[test]
    fn the_shown_tab_is_kept_in_view() {
        // The last tab shown: the strip moves so its right edge shows.
        let g = geometry(&[100.0, 100.0, 100.0], 200.0, 0.0, Some(2));
        assert_eq!(fit(&g, 0.0, 0.0, true, 28.0).offset, 128.0);
        // Scrolled to the end, then the first tab is shown: back to 0.
        let g = geometry(&[100.0, 100.0, 100.0], 200.0, 128.0, Some(0));
        assert_eq!(fit(&g, 128.0, 0.0, true, 28.0).offset, 0.0);
        // A tab already in view stays where it is.
        let g = geometry(&[100.0, 100.0, 100.0], 200.0, 50.0, Some(1));
        assert_eq!(fit(&g, 50.0, 0.0, true, 28.0).offset, 50.0);
    }
}
