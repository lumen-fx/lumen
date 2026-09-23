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
//! [`RouteHistory`] is the back/forward stack an app keeps in memory. A
//! desktop window steps through it; a browser hands back and forward to its
//! own history instead, and a `popstate` comes back here as a navigation.

use bevy_ecs::prelude::*;
use lumen_core::nav::{self, NavOp};
use lumen_core::property_store::PropertyStore;

/// Navigation target attached to a spawned `<a href="...">` element. A click
/// on the entity navigates the active page.
#[derive(Component, Clone, Debug)]
pub struct Anchor(pub String);

/// Host policy: the host follows a link click on its own, so a click on an
/// [`Anchor`] raises no navigation here.
///
/// A host inserts this when its links are real links it lets through, such
/// as a browser loading every page as a document of its own. The click
/// still reaches the app, and navigating from it as well would do the
/// host's work twice. A script's own navigation is unaffected: this covers
/// the anchor click and nothing else.
#[derive(Clone, Copy, Debug, Default, Resource)]
pub struct HostFollowsLinks;

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

impl Location {
    /// The location of a page opened at its own address, with nothing left
    /// over: what a desktop app opens on, and what a build renders.
    pub fn page(key: impl Into<String>) -> Self {
        Self {
            path: key.into(),
            segment: String::new(),
        }
    }
}

/// In-memory back/forward history. A desktop window steps through this; in a
/// browser the browser's own history is what answers back and forward, and
/// this records where the app has been without being stepped.
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
///
/// `entry` and `keys` are the site: its home page and every page it holds,
/// which is what a later navigation resolves against. `at` is where this app
/// opens, which is a question about the address it was asked for: a window
/// opens on the entry with nothing left over, and a document served for
/// `/user/42` opens on `user` with `/42` in hand.
pub fn install_routing(
    app: &mut lumen_core::app::App,
    entry: String,
    keys: Vec<String>,
    at: Location,
) {
    use lumen_core::tick::TickStage;

    // Seed the reserved signals so the opening page's `<if>` gate mounts on
    // the first reconcile pass.
    {
        let mut store = app.world.resource_mut::<PropertyStore>();
        store.set_global_str(nav::PATH_SIGNAL, at.path.as_str());
        store.set_global_str(nav::SEGMENT_SIGNAL, at.segment.as_str());
    }
    nav::set_current(&at.path);

    app.world.insert_resource(PageRegistry { entry, keys });
    app.world.insert_resource(RouteHistory {
        stack: vec![at],
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
/// DOM `<a href>`, and under `[web] navigation = "soft"` the browser's own
/// anchor navigation is prevented so that this system's swap is what the
/// click ends at. A host holding [`HostFollowsLinks`] follows the link
/// itself, and the click raises nothing here.
pub fn navigate_on_anchor_click(
    mut clicks: bevy_ecs::message::MessageReader<lumen_core::input::ClickEvent>,
    anchors: Query<&Anchor>,
    host_follows: Option<Res<HostFollowsLinks>>,
) {
    if host_follows.is_some() {
        clicks.clear();
        return;
    }
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

#[cfg(test)]
mod tests {
    use super::*;
    use lumen_core::app::App;
    use lumen_core::input::{ClickEvent, PointerButton};

    /// An app on the `index` page of a two-page site, with one link to
    /// `settings`.
    fn linked_app() -> (App, Entity) {
        let mut app = App::new();
        app.extract_fns.clear();
        app.world.init_resource::<PropertyStore>();
        install_routing(
            &mut app,
            "index".to_string(),
            vec!["settings".to_string(), "index".to_string()],
            Location::page("index"),
        );
        let link = app.world.spawn(Anchor("settings".to_string())).id();
        (app, link)
    }

    /// A primary click on `entity`, read by the next tick.
    fn click(app: &mut App, entity: Entity) {
        app.world.write_message(ClickEvent {
            entity,
            position: glam::Vec2::ZERO,
            button: PointerButton::Primary,
            local: None,
        });
    }

    /// The page the router has open.
    fn route_path(app: &App) -> Option<String> {
        app.world
            .resource::<PropertyStore>()
            .get_global_str(nav::PATH_SIGNAL)
            .map(|path| path.to_string())
    }

    #[test]
    fn a_host_that_follows_links_gets_no_navigation_from_a_click() {
        let (mut app, link) = linked_app();
        app.world.insert_resource(HostFollowsLinks);
        let before = app
            .world
            .resource::<PropertyStore>()
            .get_global_str(nav::REQUEST_SIGNAL)
            .map(|request| request.to_string());
        click(&mut app, link);
        app.tick();
        app.tick();
        assert_eq!(
            app.world
                .resource::<PropertyStore>()
                .get_global_str(nav::REQUEST_SIGNAL)
                .map(|request| request.to_string()),
            before,
            "the host is following the link, so the app raises no navigation"
        );
        assert_eq!(route_path(&app).as_deref(), Some("index"));

        // The same click in a host that leaves links to the app navigates,
        // which is what makes the silence above the policy's doing.
        app.world.remove_resource::<HostFollowsLinks>();
        click(&mut app, link);
        app.tick();
        app.tick();
        assert_eq!(route_path(&app).as_deref(), Some("settings"));
    }
}
