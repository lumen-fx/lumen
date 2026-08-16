//! Navigating between the pages of one app.
//!
//! A page is a key, navigation is a write to a reserved signal, and the
//! `<if>` reconciler does the mounting: the assembled tree is one gate per
//! page, and the resolver decides which gate is open. Nothing here knows
//! where the pages came from, so the same resolver serves an app loaded from
//! `.lmn` files, one loaded from a compiled artifact, and one running in a
//! browser.
//!
//! A requested path resolves by longest existing key ([`lumen_core::nav`]):
//! `/settings` reaches `settings`, and `/user/7` with no `user/7` page
//! reaches `user` with `/7` left on the `route.segment` signal for the page
//! to read.
//!
//! [`RouteHistory`] is the back/forward stack a desktop app keeps in memory.
//! A browser has one of its own; the navigation surface is the same either
//! way, and only the history behind it changes.

use bevy_ecs::prelude::*;
use lumen_core::nav::{self, NavOp};
use lumen_core::property_store::PropertyStore;

/// Navigation target attached to a spawned `<a href="...">` element. A click
/// on the entity navigates the active page.
#[derive(Component, Clone, Debug)]
pub struct Anchor(pub String);

/// Runtime page registry - the resolver's view of the loaded pages.
#[derive(Clone, Debug, Resource)]
pub struct PageRegistry {
    /// Home page key.
    pub entry: String,
    /// Page keys, longest-first.
    pub keys: Vec<String>,
}

/// One entry on the in-memory history stack.
#[derive(Clone, Debug)]
pub struct Location {
    /// Resolved page key.
    pub path: String,
    /// Leftover segment after the matched page prefix.
    pub segment: String,
}

/// In-memory back/forward history (desktop). The web target replaces this
/// with the real History API; the navigation surface is identical.
#[derive(Clone, Debug, Resource)]
pub struct RouteHistory {
    /// Visited locations, oldest first.
    pub stack: Vec<Location>,
    /// Index of the currently-active location within [`Self::stack`].
    pub cursor: usize,
}

impl RouteHistory {
    fn active(&self) -> Option<&Location> {
        self.stack.get(self.cursor)
    }
}

/// Install navigation for a known page set: the registry, the in-memory
/// history, the reserved-signal seeds, and the two navigation systems.
///
/// This is what both an app loaded from source and one loaded from a compiled
/// artifact end up calling; they differ only in where the page set came from,
/// a directory listing in one case and [`lumen_ir::artifact::CompiledPages`]
/// in the other.
pub fn install_routing(app: &mut lumen_core::app::App, entry: String, keys: Vec<String>) {
    use lumen_core::tick::TickStage;

    // Seed the reserved signals so the entry page's `<if>` gate mounts on the
    // first reconcile pass.
    {
        let mut store = app.world.resource_mut::<PropertyStore>();
        store.set_global_str(nav::PATH_SIGNAL, entry.as_str());
        store.set_global_str(nav::SEGMENT_SIGNAL, "");
    }
    nav::set_current(&entry);

    app.world.insert_resource(PageRegistry {
        entry: entry.clone(),
        keys,
    });
    app.world.insert_resource(RouteHistory {
        stack: vec![Location {
            path: entry,
            segment: String::new(),
        }],
        cursor: 0,
    });

    // Resolver runs before the `<if>` reconciler so a navigation this tick
    // swaps the mounted page this tick.
    app.add_systems(
        TickStage::Systems,
        apply_navigation.before(crate::spawn::reconcile_if_blocks),
    );
    app.add_systems(TickStage::Systems, navigate_on_anchor_click);
}

/// The single navigation resolver. Reads the reserved request signal (written
/// by every surface via [`lumen_core::nav::request`]), resolves the target by
/// longest existing-file prefix, updates the reserved `route.path` /
/// `route.segment` cells, and maintains the in-memory history stack.
pub fn apply_navigation(
    mut store: ResMut<PropertyStore>,
    registry: Option<Res<PageRegistry>>,
    mut history: ResMut<RouteHistory>,
    mut last: Local<Option<String>>,
) {
    let Some(registry) = registry else {
        return;
    };
    let Some(request) = store.get_global_str(nav::REQUEST_SIGNAL) else {
        return;
    };
    let request = request.to_string();
    if last.as_deref() == Some(request.as_str()) {
        return; // already processed this exact request
    }
    *last = Some(request.clone());

    let Some((_seq, op)) = nav::parse_request(&request) else {
        return;
    };

    let target: Option<Location> = match op {
        NavOp::Navigate(path) => {
            let (key, segment) = nav::resolve_path(&path, &registry.keys, &registry.entry);
            // Truncate any forward history, then push.
            let keep = history.cursor + 1;
            history.stack.truncate(keep);
            history.stack.push(Location {
                path: key.clone(),
                segment: segment.clone(),
            });
            history.cursor = history.stack.len() - 1;
            Some(Location { path: key, segment })
        }
        NavOp::Back => {
            if history.cursor > 0 {
                history.cursor -= 1;
            }
            history.active().cloned()
        }
        NavOp::Forward => {
            if history.cursor + 1 < history.stack.len() {
                history.cursor += 1;
            }
            history.active().cloned()
        }
    };

    if let Some(loc) = target {
        store.set_global_str(nav::PATH_SIGNAL, loc.path.as_str());
        store.set_global_str(nav::SEGMENT_SIGNAL, loc.segment.as_str());
        nav::set_current(&loc.path);
    }
}

/// Declarative navigation: a click on a spawned `<a href>` navigates the
/// active page. The anchor is a real element; on the web target it is a real
/// DOM `<a href>` and this system's effect is the browser's own default
/// anchor navigation.
pub fn navigate_on_anchor_click(
    mut clicks: bevy_ecs::message::MessageReader<lumen_core::input::ClickEvent>,
    anchors: Query<&Anchor>,
) {
    for click in clicks.read() {
        if let Ok(anchor) = anchors.get(click.entity) {
            // Honor `event.prevent_default()` from a phase-4 click handler:
            // link navigation is the click default action, so a prevented
            // click does not navigate.
            let handle = lumen_core::node::NodeHandle::new(click.entity).pack();
            if lumen_script::event::is_click_default_prevented(handle) {
                continue;
            }
            nav::navigate(anchor.0.clone());
        }
    }
}
