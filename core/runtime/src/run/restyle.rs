use super::*;

/// W4.7: detect a runtime change to the root entity's [`LumenClasses`]
/// (from `set_root_class("theme-dark")`, OS theme follow, etc) and bump
/// [`StyleVersion`] so downstream consumers (cascade re-resolver,
/// `Visuals` / `TextStyle` recompute) know the computed style for any
/// rule whose match set intersects the changed class list is now stale.
///
/// Pre-W4.7 this respawned the entire markup tree, reread `main.css`
/// from disk, and round-tripped every entity through a state
/// snapshot/restore pass. That broke on shipped binaries (no source
/// `main.css`), dropped any ECS state outside the snapshot whitelist,
/// and cost O(tree) per flip. See `docs/audits/theming.md` section 6.
///
/// Today the function:
/// 1. Reads the latest [`lumen_scene::spawn::DocumentRoot`] `LumenClasses` (already mutated
///    in place by [`lumen_core::signals::apply_theme_signal_to_root_classes`]
///    or by `set_root_class`).
/// 2. Diffs against [`RootClassesCache`]; no-ops when unchanged.
/// 3. Consults [`StyleInvalidationCache::diff_affects`] - when no
///    selector in the parsed stylesheet mentions any changed class,
///    nothing can re-resolve, so we update the cache and exit.
/// 4. Otherwise bumps [`StyleVersion`]. The cache + downstream
///    `Changed<StyleVersion>` consumers re-resolve only entities whose
///    style depended on the changed classes / media features. The
///    parsed `Stylesheet` is reused - no disk read, no respawn.
pub(crate) fn reapply_styles_on_root_class_change(world: &mut World) {
    let Some(root) = world
        .get_resource::<lumen_scene::spawn::DocumentRoot>()
        .map(|r| r.0)
    else {
        return;
    };
    // Alloc-free change gate: compare the live `Arc<str>` class list
    // against the cached `Vec<String>` in place. Only when they actually
    // differ do we materialise the owned `Vec<String>` copies the
    // diff/cache paths below need - steady-state ticks (the overwhelming
    // majority) never allocate here.
    let Some(classes) = world.get::<lumen_core::components::LumenClasses>(root) else {
        return;
    };
    let unchanged = match world.get_resource::<RootClassesCache>() {
        Some(c) => {
            c.0.len() == classes.0.len()
                && c.0
                    .iter()
                    .zip(classes.0.iter())
                    .all(|(a, b)| a.as_str() == b.as_ref())
        }
        // Absent cache behaves as an empty one (the old `unwrap_or_default`).
        None => classes.0.is_empty(),
    };
    if unchanged {
        return;
    }
    // Changed - now copy into owned `Vec<String>`s for the diff/cache paths.
    let current: Vec<String> = classes.0.iter().map(|s| s.to_string()).collect();
    let cached = world
        .get_resource::<RootClassesCache>()
        .map(|c| c.0.clone())
        .unwrap_or_default();
    // Fast rejection: when none of the changed classes appears in any
    // CSS selector, no computed style can have re-resolved. Update the
    // cache and exit without bumping `StyleVersion` - saves downstream
    // re-resolution passes from a no-op tick.
    if let Some(inv) = world.get_resource::<StyleInvalidationCache>()
        && !inv.classes.is_empty()
        && !inv.diff_affects(&cached, &current)
    {
        if let Some(mut cache) = world.get_resource_mut::<RootClassesCache>() {
            cache.0 = current;
        }
        return;
    }
    // In-place path: bump `StyleVersion` so downstream consumers know
    // computed style for affected entities is stale, then update the
    // class cache. No despawn, no disk re-read, no IR rebuild - the
    // existing entity tree retains every component the snapshot/restore
    // path used to lose.
    StyleVersion::bump(world);
    if let Some(mut cache) = world.get_resource_mut::<RootClassesCache>() {
        cache.0 = current;
    } else {
        world.insert_resource(RootClassesCache(current));
    }
}

/// Make sure the root carries a
/// [`LumenClasses`](lumen_core::components::LumenClasses) list.
///
/// The spawn pass attaches that component only to elements whose markup
/// declared a class, and a root usually declares none. Without it the OS
/// theme follow has nothing to write `theme-dark` / `theme-light` onto. An
/// empty list is the class list a root that declares none has, so giving it
/// one costs nothing and every writer finds it there.
///
/// A window's, and not every assembly's: the theme a class follows is the
/// one the OS reports, and a browser page has no such reading. Writing
/// `theme-light` onto a page's root because nothing said otherwise would
/// hold every visitor to the light theme.
pub(crate) fn install_root_class_list(world: &mut World, root: Entity) {
    use lumen_core::components::LumenClasses;
    if world.get::<LumenClasses>(root).is_none() {
        world
            .entity_mut(root)
            .insert(LumenClasses::from(Vec::<String>::new()));
    }
}

/// Cached snapshot of the root entity's `LumenClasses` so
/// [`reapply_styles_on_root_class_change`] can detect when a
/// `set_root_class(...)` actually changed something (avoids spurious
/// [`StyleVersion`] bumps on no-op writes).
#[derive(Resource, Default, Debug, Clone)]
pub(crate) struct RootClassesCache(pub(crate) Vec<String>);

/// Cross-thread payload pushed by the W4.6 `set_color_scheme(name)`
/// Rhai builtin. Carries the parsed
/// [`lumen_core::components::ColorScheme`] intent over the bounded
/// [`lumen_core::command::CommandQueue`] so the main-thread handler
/// can apply it via [`lumen_core::components::StyleManager::set_scheme`]
/// during [`TickStage::CommandDrain`].
pub struct ColorSchemeIntent(pub lumen_core::components::ColorScheme);

/// Per-tick LRU eviction sweep that reads [`MemoryBudget`] and calls each ECS-resident cache's `evict_until` so its `bytes_used` falls below the cap.
/// `AssetServer` and the text shaper are handled here; a renderer's `lumen_paint::FragmentCache` bounds itself by entry count.
pub(crate) fn enforce_budget(
    budget: Res<lumen_core::components::MemoryBudget>,
    mut server: ResMut<lumen_assets::AssetServer>,
    shaper: Option<NonSendMut<ShaperService>>,
) {
    let image_cap = (budget.images_mb as usize).saturating_mul(1024 * 1024);
    if server.bytes_used() > image_cap {
        server.evict_until(image_cap);
    }
    if let Some(mut s) = shaper {
        let cap = budget.shape_entries as usize;
        if s.cache_len() > cap {
            s.set_capacity(cap);
        }
    }
}
/// Union of every class name mentioned by every selector in the
/// user CSS + active skin CSS, plus per-media-feature flags. Populated
/// once at startup.
///
/// - The class set drives `reapply_styles_on_root_class_change` as a
///   fast-rejection filter: when no class in the symmetric diff
///   (between cached and current root classes) appears in this set,
///   no selector could match anything new, and the expensive
///   disk-reload + respawn is skipped.
/// - The media-feature flags (W4.4) record which `@media` features the
///   stylesheet exercises, so the runtime can decide whether to
///   re-resolve styles when `StyleManager` / viewport width / motion / contrast
///   preferences change. W4.7 lands the actual in-place re-resolve; until
///   then this is a recompute trigger surface.
///
/// Pattern: Blink/Stylo's `RuleFeatureSet` - pre-compute which class
/// changes can possibly affect styling so most class-change events
/// are no-ops at zero cost.
#[derive(Resource, Default, Debug, Clone)]
pub(crate) struct StyleInvalidationCache {
    classes: std::collections::HashSet<String>,
    /// Which `@media` features the stylesheet exercises. Read by
    /// `detect_media_change` to decide whether a theme / viewport flip
    /// can affect styling at all before bumping `StyleVersion`.
    media_features: MediaFeatureUsage,
    /// Sorted, de-duplicated viewport-width thresholds (`min-width` /
    /// `max-width` / `width` px values) mentioned by any `@media` rule.
    /// `detect_media_change` treats a resize as a breakpoint crossing
    /// only when old and new widths land on opposite sides of one of
    /// these - a plain drag-resize inside a band never re-resolves.
    width_breakpoints: Vec<f32>,
    /// Some selector steps across siblings (`+` / `~`), so an element's
    /// class change can restyle the siblings after it.
    sibling_combinators: bool,
}

/// Which `@media` features the stylesheet contains at least one rule
/// guarded by. Each flag, when set, tells the runtime to re-resolve
/// computed styles when the matching context bit changes.
#[derive(Default, Debug, Clone, Copy)]
#[allow(dead_code)]
struct MediaFeatureUsage {
    color_scheme: bool,
    reduced_motion: bool,
    contrast: bool,
    viewport_width: bool,
}

impl StyleInvalidationCache {
    /// Build from the already-combined (skin + user) stylesheet carried on
    /// the IR. Works off the parsed AST - no CSS source string and no
    /// re-parse - so it is identical for the parse-from-source and the
    /// AOT-artifact load paths, and never pulls in the source parser.
    pub(crate) fn from_stylesheet(sheet: &lumen_ir::css::Stylesheet) -> Self {
        let mut media_features = MediaFeatureUsage::default();
        let mut breakpoints: Vec<f32> = Vec::new();
        let classes = sheet.class_invalidation_set();
        collect_media_usage(sheet, &mut media_features);
        collect_width_breakpoints(sheet, &mut breakpoints);
        breakpoints.sort_by(|a, b| a.partial_cmp(b).unwrap_or(std::cmp::Ordering::Equal));
        breakpoints.dedup();
        Self {
            classes,
            media_features,
            width_breakpoints: breakpoints,
            sibling_combinators: sheet.uses_sibling_combinators(),
        }
    }

    /// Symmetric-difference of two class lists includes ANY name in
    /// the invalidation set?
    fn diff_affects(&self, old: &[String], new: &[String]) -> bool {
        let old_set: std::collections::HashSet<&String> = old.iter().collect();
        let new_set: std::collections::HashSet<&String> = new.iter().collect();
        old_set
            .symmetric_difference(&new_set)
            .any(|c| self.classes.contains(c.as_str()))
    }
}

/// Walk a parsed stylesheet and OR-in any media feature it touches.
/// Used by `StyleInvalidationCache::build`.
fn collect_media_usage(sheet: &lumen_ir::css::Stylesheet, out: &mut MediaFeatureUsage) {
    use lumen_ir::css::MediaFeature;
    for rule in &sheet.rules {
        let Some(mq) = &rule.media else { continue };
        for feat in &mq.features {
            match feat {
                MediaFeature::PrefersColorScheme(_) => out.color_scheme = true,
                MediaFeature::PrefersReducedMotion(_) => out.reduced_motion = true,
                MediaFeature::PrefersContrast(_) => out.contrast = true,
                MediaFeature::MinWidth(_) | MediaFeature::MaxWidth(_) | MediaFeature::Width(_) => {
                    out.viewport_width = true
                }
            }
        }
    }
}

/// Collect the px thresholds of every width `@media` feature into `out`.
/// Used to build [`StyleInvalidationCache::width_breakpoints`].
fn collect_width_breakpoints(sheet: &lumen_ir::css::Stylesheet, out: &mut Vec<f32>) {
    use lumen_ir::css::MediaFeature;
    for rule in &sheet.rules {
        let Some(mq) = &rule.media else { continue };
        for feat in &mq.features {
            match feat {
                MediaFeature::MinWidth(px)
                | MediaFeature::MaxWidth(px)
                | MediaFeature::Width(px) => out.push(*px),
                _ => {}
            }
        }
    }
}

/// Live copy of the combined (skin + user) stylesheet, kept as a
/// resource so [`reapply_computed_styles`] can re-run the cascade on a
/// theme / media flip without re-reading `main.css` from disk. Refreshed
/// on every hot reload so an edited stylesheet re-resolves correctly.
#[derive(Resource, Clone)]
pub(crate) struct RuntimeStylesheet(pub(crate) lumen_ir::css::Stylesheet);

/// The last [`MediaContext`] observed by [`detect_media_change`]. Used
/// to detect a color-scheme flip or a viewport-width breakpoint crossing
/// between ticks. `None` until the first post-window tick, which forces
/// one re-resolve against the real OS context (the initial load applied
/// with the default best-guess context before the window existed).
#[derive(Resource, Clone, Copy)]
pub(crate) struct LastMediaContext(pub(crate) lumen_ir::css::MediaContext);

/// The [`StyleVersion`] value [`reapply_computed_styles`] last consumed.
/// The re-resolver re-walks entities only when the live `StyleVersion`
/// has moved past this, so a steady-state tick is a single integer
/// compare.
#[derive(Resource, Clone, Copy)]
pub(crate) struct AppliedStyleVersion(pub(crate) u64);

/// The [`MediaContext`] of the primary window: the live [`StyleManager`]
/// (color scheme) and the primary window's logical width. Missing
/// resources (pre-window) leave the corresponding field unknown so no
/// `@media` rule matches spuriously.
pub(crate) fn media_context_from_world(world: &World) -> lumen_ir::css::MediaContext {
    media_context_for(world, None)
}

/// The [`MediaContext`] `window` evaluates `@media` rules against (`None`:
/// the primary window): the app-wide color scheme and the window's own
/// logical width.
pub(crate) fn media_context_for(
    world: &World,
    window: Option<Entity>,
) -> lumen_ir::css::MediaContext {
    use lumen_ir::css::{ColorSchemePreference, MediaContext};
    let color_scheme = world
        .get_resource::<lumen_core::components::StyleManager>()
        .map(|sm| {
            if sm.effective_dark {
                ColorSchemePreference::Dark
            } else {
                ColorSchemePreference::Light
            }
        });
    let viewport_width = match window {
        Some(window) => world
            .get::<lumen_core::window::WindowViewport>(window)
            .map(|v| v.size.x),
        None => lumen_core::window::primary_viewport(world).map(|v| v.size.x),
    };
    MediaContext {
        color_scheme,
        viewport_width,
        ..Default::default()
    }
}

/// The media context a surface root's subtree restyles against: its
/// window's. `None` for an element that is not a surface root.
fn surface_media(world: &World, entity: Entity) -> Option<lumen_ir::css::MediaContext> {
    let window = world.get::<lumen_core::window::WindowOf>(entity)?.window();
    Some(media_context_for(world, Some(window)))
}

/// Whether going from `prev` to `media` changes what a stylesheet's
/// `@media` rules match: a color-scheme flip the sheet reacts to, or a width
/// crossing one of its breakpoints.
fn media_crosses(
    cache: Option<&StyleInvalidationCache>,
    prev: &lumen_ir::css::MediaContext,
    media: &lumen_ir::css::MediaContext,
) -> bool {
    let uses_scheme = cache
        .map(|c| c.media_features.color_scheme)
        .unwrap_or(false);
    let scheme_changed = uses_scheme && prev.color_scheme != media.color_scheme;
    let width_crossed = match (prev.viewport_width, media.viewport_width) {
        (Some(o), Some(n)) => cache
            .map(|c| c.width_breakpoints.iter().any(|t| (o >= *t) != (n >= *t)))
            .unwrap_or(false),
        (None, Some(_)) | (Some(_), None) => cache
            .map(|c| !c.width_breakpoints.is_empty())
            .unwrap_or(false),
        (None, None) => false,
    };
    scheme_changed || width_crossed
}

/// The window and media context each surface root last restyled against,
/// by surface root.
#[derive(Resource, Default)]
pub(crate) struct LastWindowMedia(
    std::collections::HashMap<Entity, (Entity, lumen_ir::css::MediaContext)>,
);

/// W4.7 (media half): detect an OS color-scheme flip or a viewport-width
/// resize that crosses a stylesheet breakpoint, and bump [`StyleVersion`]
/// so [`reapply_computed_styles`] re-resolves. The first post-window tick
/// (no [`LastMediaContext`] yet) always bumps once, replacing the
/// best-guess context the pre-window initial load applied with.
///
/// Bounded: a color-scheme flip only bumps when the stylesheet actually
/// contains a `prefers-color-scheme` rule; a resize only bumps when it
/// crosses one of [`StyleInvalidationCache::width_breakpoints`]. A plain
/// drag-resize inside a band, or a theme flip on a stylesheet with no
/// `@media`, is a cheap no-op.
///
/// Every other window evaluates media queries against its own width: a
/// crossing there restyles that window's surface roots only, as does the
/// first sight of a surface root in a window.
pub(crate) fn detect_media_change(world: &mut World) {
    let media = media_context_from_world(world);
    let prev = world.get_resource::<LastMediaContext>().map(|m| m.0);
    // Borrow the invalidation cache in place - the breakpoint list was
    // being cloned into a fresh `Vec` every tick just to be read once.
    let changed = match prev {
        // First observation with a real context: force one re-resolve.
        None => true,
        Some(prev) => media_crosses(
            world.get_resource::<StyleInvalidationCache>(),
            &prev,
            &media,
        ),
    };
    // Refresh the baseline so breakpoint-crossing detection tracks the
    // latest width even on ticks that don't bump. Only a different context
    // is written: readers of the resource take a write as a change.
    if prev != Some(media) {
        world.insert_resource(LastMediaContext(media));
    }
    if changed {
        StyleVersion::bump(world);
    }
    detect_window_media_change(world);
}

/// The other windows' half of [`detect_media_change`]: a surface root that
/// is new, moved to another window, or whose window crossed a breakpoint
/// restyles; one that went back into the primary window restyles against
/// it. With one window it reads one resource and returns.
fn detect_window_media_change(world: &mut World) {
    let roots: Vec<(Entity, Entity)> = world
        .get_resource::<lumen_core::window::WindowPartitions>()
        .map(|p| p.surface_roots().to_vec())
        .unwrap_or_default();
    let has_last = world
        .get_resource::<LastWindowMedia>()
        .is_some_and(|l| !l.0.is_empty());
    if roots.is_empty() && !has_last {
        return;
    }
    let mut last = world
        .remove_resource::<LastWindowMedia>()
        .unwrap_or_default();
    let mut seen: std::collections::HashMap<Entity, (Entity, lumen_ir::css::MediaContext)> =
        std::collections::HashMap::new();
    let mut dirty: Vec<Entity> = Vec::new();
    for &(root, window) in &roots {
        // Only the document's own elements restyle from its stylesheets; a
        // tree outside it (the devtools panel) styles itself.
        if !in_document(world, root) {
            continue;
        }
        let media = media_context_for(world, Some(window));
        let crossed = match last.0.get(&root) {
            Some((was, prev)) => {
                *was != window
                    || media_crosses(world.get_resource::<StyleInvalidationCache>(), prev, &media)
            }
            None => true,
        };
        if crossed {
            dirty.push(root);
        }
        seen.insert(root, (window, media));
    }
    // A root no longer a surface root restyles against the primary window.
    dirty.extend(last.0.keys().filter(|r| !seen.contains_key(r)).copied());
    for root in dirty {
        if let Ok(mut e) = world.get_entity_mut(root) {
            e.insert(lumen_core::components::StyleDirty);
        }
    }
    last.0 = seen;
    world.insert_resource(last);
}

/// Whether `entity` is under the document root, or there is no document
/// root to be under.
fn in_document(world: &World, entity: Entity) -> bool {
    let Some(root) = world
        .get_resource::<lumen_scene::spawn::DocumentRoot>()
        .map(|r| r.0)
    else {
        return true;
    };
    let mut cur = entity;
    loop {
        if cur == root {
            return true;
        }
        match world.get::<ChildOf>(cur) {
            Some(parent) => cur = parent.parent(),
            None => return false,
        }
    }
}

/// Re-resolve elements whose styles may have changed and rebuild their
/// style components from scratch.
///
/// Two triggers. A bump of [`StyleVersion`] (a theme, a breakpoint, a root
/// class) restyles every element of the document. An element marked
/// [`StyleDirty`] (its own classes, id, inline style or disabled state
/// changed, or a tree edit changed its children) restyles itself and
/// everything under it, which is every element whose styles such a change
/// can reach. A steady tick costs one
/// integer compare and one empty query.
///
/// Each element is resolved the way the first cascade resolved it, through
/// [`lumen_ir::css::RestyleWalk`]: from its tag, classes, id and place in
/// the tree, with what its markup declares and what a script set with
/// `set_style` ranking above every rule, against the user-agent sheet, the
/// skin and the app's stylesheet. The walk runs top-down, so each element
/// inherits from the parent it just resolved. The result replaces the
/// element's style components through
/// [`ComputedStyle::commit`](lumen_scene::computed::ComputedStyle::commit):
/// a property nothing sets any more goes back to its initial value, and a
/// value a runtime system holds in
/// [`StyleOverrides`](lumen_core::style_overrides::StyleOverrides) stays on
/// top of it.
pub(crate) fn reapply_computed_styles(world: &mut World) {
    use lumen_core::components::{LumenTag, StyleDirty};
    let version = world
        .get_resource::<StyleVersion>()
        .map(|v| v.0)
        .unwrap_or(0);
    let full = world
        .get_resource::<AppliedStyleVersion>()
        .is_none_or(|a| a.0 != version);
    let mut dirty_q = world.query_filtered::<Entity, With<StyleDirty>>();
    let dirty: Vec<Entity> = dirty_q.iter(world).collect();
    if !full && dirty.is_empty() {
        return;
    }
    for &entity in &dirty {
        world.entity_mut(entity).remove::<StyleDirty>();
    }
    world.insert_resource(AppliedStyleVersion(version));

    // The app's stylesheet styles the app's document. A tree mounted as a
    // root of its own beside it (a dev tool's panel, styled by its own
    // sheet when it was spawned) is left alone, as the first cascade left
    // it alone.
    let document = world
        .get_resource::<lumen_scene::spawn::DocumentRoot>()
        .map(|r| r.0);
    let roots: Vec<Entity> = if full {
        match document {
            Some(root) => vec![root],
            None => {
                let mut q = world.query_filtered::<Entity, (With<LumenTag>, Without<ChildOf>)>();
                q.iter(world).collect()
            }
        }
    } else {
        // With a `+` / `~` rule in the sheet, an element's change reaches
        // the siblings after it, so its parent's subtree restyles.
        let siblings = world
            .get_resource::<StyleInvalidationCache>()
            .is_some_and(|c| c.sibling_combinators);
        let dirty: Vec<Entity> = dirty
            .iter()
            .copied()
            .filter(|&e| world.get_entity(e).is_ok())
            .map(|e| match world.get::<ChildOf>(e) {
                Some(c) if siblings && is_sibling_element(world, e) => c.parent(),
                _ => e,
            })
            .collect();
        // The topmost dirty elements: one under another restyles with it.
        let set: std::collections::HashSet<Entity> = dirty.iter().copied().collect();
        dirty
            .iter()
            .copied()
            .filter(|&e| document.is_none_or(|root| topmost_ancestor(world, e) == root))
            .filter(|&e| !ancestors_of(world, e).iter().any(|a| set.contains(a)))
            .collect::<std::collections::BTreeSet<Entity>>()
            .into_iter()
            .collect()
    };
    if roots.is_empty() {
        return;
    }

    let media = world
        .get_resource::<LastMediaContext>()
        .map(|m| m.0)
        .unwrap_or_default();
    // Restyle tweens only make sense while the window can animate: an
    // unfocused or occluded window's redraw scheduler is paused, so a tween
    // started there would freeze mid-flight until focus returns. Snap
    // instead, which is what every native toolkit shows when the OS theme
    // flips a background window.
    let animate = world
        .get_resource::<lumen_core::window_backend::RedrawScheduler>()
        .map(|s| !s.paused)
        .unwrap_or(true);
    let sheet = world
        .remove_resource::<RuntimeStylesheet>()
        .map(|s| s.0)
        .unwrap_or_default();
    let siblings = sheet.uses_sibling_combinators();
    let mut warnings = Vec::new();
    for root in roots {
        // The ancestors resolve root-first, each against the media of the
        // window it shows in: a surface root on the way switches the walk
        // to its window's width.
        let mut walk = lumen_ir::css::RestyleWalk::new(&sheet, &media, &[]);
        for a in ancestors_of(world, root).into_iter().rev() {
            if let Some(window_media) = surface_media(world, a) {
                walk.set_media(window_media);
            }
            let place = place_of(world, a, siblings);
            let info = cascade_node(world, a, place, !tagged_children(world, a).is_empty());
            walk.resolve(&info);
            walk.enter();
        }
        let place = place_of(world, root, siblings);
        restyle_subtree(world, &mut walk, root, place, siblings, animate);
        warnings.extend(walk.take_warnings());
    }
    world.insert_resource(RuntimeStylesheet(sheet));
    for w in warnings {
        tracing::debug!("restyle: {w}");
    }
}

/// Where an element sits among its element siblings, and who they are
/// when the stylesheet steps across siblings.
#[derive(Default)]
struct Place {
    position: lumen_ir::css::SiblingPosition,
    siblings: Option<lumen_ir::css::Siblings>,
}

/// Resolve `entity` against `walk` and everything under it, rebuilding
/// each element's style components. A surface root's subtree resolves
/// against its window's media.
fn restyle_subtree(
    world: &mut World,
    walk: &mut lumen_ir::css::RestyleWalk<'_>,
    entity: Entity,
    place: Place,
    siblings: bool,
    animate: bool,
) {
    match surface_media(world, entity) {
        Some(window_media) => {
            let outer = walk.media();
            walk.set_media(window_media);
            restyle_element(world, walk, entity, place, siblings, animate);
            walk.set_media(outer);
        }
        None => restyle_element(world, walk, entity, place, siblings, animate),
    }
}

/// [`restyle_subtree`] for one element, with the walk's media already set.
fn restyle_element(
    world: &mut World,
    walk: &mut lumen_ir::css::RestyleWalk<'_>,
    entity: Entity,
    place: Place,
    siblings: bool,
    animate: bool,
) {
    use lumen_core::components::LumenTag;
    let children: Vec<Entity> = world
        .get::<bevy_ecs::hierarchy::Children>(entity)
        .map(|c| c.iter().collect())
        .unwrap_or_default();
    let element = world.get::<LumenTag>(entity).is_some();
    // A renderer-internal leaf (a toggle's knob, say) has no styles of its
    // own to resolve.
    if !element && children.is_empty() {
        return;
    }
    let tagged: Vec<Entity> = children
        .iter()
        .copied()
        .filter(|&k| is_sibling_element(world, k))
        .collect();
    let node = cascade_node(world, entity, place, !tagged.is_empty());
    let mut attrs = walk.resolve(&node);
    if element {
        commit_resolved(world, entity, &mut attrs, animate);
    }
    if children.is_empty() {
        return;
    }
    // One sibling list per parent, shared by every child of it.
    let peers =
        siblings.then(|| lumen_ir::css::Siblings::new(tagged.iter().map(|&k| identity(world, k))));
    let count = tagged.len() as i32;
    let mut index = 0;
    walk.enter();
    for child in children {
        // `tagged` is `children` with the non-elements left out, so the
        // elements come up in its order.
        let place = if tagged.get(index) == Some(&child) {
            index += 1;
            Place {
                position: lumen_ir::css::SiblingPosition::new(index as i32, count),
                siblings: peers.clone(),
            }
        } else {
            Place::default()
        };
        restyle_subtree(world, walk, child, place, siblings, animate);
    }
    walk.leave();
}

/// Build the element's style components from what the cascade resolved
/// for it and commit them over the live ones.
fn commit_resolved(world: &mut World, entity: Entity, attrs: &mut Attributes, animate: bool) {
    use lumen_core::components::{BindDisabled, Disabled, LumenTag};
    use lumen_ir::layout_ir::WidgetPart;
    use lumen_scene::computed::{ComputedStyle, StyleContext};
    let Some(tag) = world.get::<LumenTag>(entity).map(|t| t.0.clone()) else {
        return;
    };
    // What the element was written with that its styles are computed from
    // but no rule can set.
    attrs.style_role = world
        .get::<lumen_scene::spawn::TypographyRole>(entity)
        .map(|r| r.0.to_string());
    let in_scroll = world
        .get::<ChildOf>(entity)
        .is_some_and(|c| world.get::<lumen_core::input::Scroll>(c.parent()).is_some());
    let has = |world: &World, f: fn(&bevy_ecs::world::EntityRef<'_>) -> bool| {
        world.get_entity(entity).is_ok_and(|e| f(&e))
    };
    let part = if has(world, |e| e.contains::<lumen_primitives::CheckboxBox>()) {
        Some(WidgetPart::CheckboxBox)
    } else if has(world, |e| e.contains::<lumen_primitives::RadioDot>()) {
        Some(WidgetPart::RadioDot)
    } else if has(world, |e| e.contains::<lumen_primitives::ProgressFill>()) {
        Some(WidgetPart::ProgressFill)
    } else {
        None
    };
    let bind_disabled = has(world, |e| e.contains::<BindDisabled>());
    let ctx = StyleContext {
        tag: &tag,
        static_disabled: has(world, |e| e.contains::<Disabled>()) && !bind_disabled,
        bind_disabled,
        part,
        tab_strip: has(world, |e| e.contains::<lumen_primitives::TabStripButton>()),
        dropdown_button: has(world, |e| e.contains::<lumen_primitives::DropdownButton>()),
        radio: has(world, |e| e.contains::<lumen_primitives::RadioButton>()),
        page_gate: world
            .get::<lumen_scene::spawn::IfMarker>(entity)
            .is_some_and(|m| m.signal_name == lumen_core::nav::PATH_SIGNAL),
        virtualized_for: world
            .get::<lumen_scene::spawn::ForMarker>(entity)
            .is_some_and(|m| m.virtualized),
        in_scroll,
        layout_boundary: attrs.layout_boundary,
    };
    let computed = ComputedStyle::new(&ctx, attrs);
    let mut ent = world.entity_mut(entity);
    computed.commit(&mut ent, animate);
    lumen_scene::spawn::apply_background_image(&mut ent, &tag, attrs);
    lumen_scene::spawn::apply_mask_image(&mut ent, attrs);
}

/// An element's tag, classes and id, as selectors match them.
fn identity(world: &World, e: Entity) -> (String, Vec<String>, Option<String>) {
    use lumen_core::components::{LumenClasses, LumenId, LumenTag};
    (
        world
            .get::<LumenTag>(e)
            .map(|t| t.0.to_string())
            .unwrap_or_default(),
        world
            .get::<LumenClasses>(e)
            .map(|c| c.0.iter().map(|s| s.to_string()).collect())
            .unwrap_or_default(),
        world.get::<LumenId>(e).map(|i| i.0.clone()),
    )
}

/// Whether `e` counts among its parent's element children: an element that
/// is part of the document. A [`DomHidden`](lumen_core::components::DomHidden)
/// one, such as an open tooltip under the root, is styled but is nobody's
/// sibling, so opening it does not move the root's `:last-child`.
fn is_sibling_element(world: &World, e: Entity) -> bool {
    use lumen_core::components::{DomHidden, LumenTag};
    world
        .get_entity(e)
        .is_ok_and(|r| r.contains::<LumenTag>() && !r.contains::<DomHidden>())
}

/// The children of `e` that are elements. Only those count as siblings: a
/// renderer-internal child added beside a row does not shift the row's
/// `:nth-child`.
fn tagged_children(world: &World, e: Entity) -> Vec<Entity> {
    world
        .get::<bevy_ecs::hierarchy::Children>(e)
        .map(|c| c.iter().filter(|&k| is_sibling_element(world, k)).collect())
        .unwrap_or_default()
}

/// Where `entity` sits under its parent, read off the parent's children.
/// The walk hands this down instead for every element below its root.
fn place_of(world: &World, entity: Entity, siblings: bool) -> Place {
    let Some(parent) = world.get::<ChildOf>(entity).map(|c| c.parent()) else {
        return Place::default();
    };
    let peers = tagged_children(world, parent);
    let Some(i) = peers.iter().position(|&p| p == entity) else {
        return Place::default();
    };
    Place {
        position: lumen_ir::css::SiblingPosition::new(i as i32 + 1, peers.len() as i32),
        siblings: siblings
            .then(|| lumen_ir::css::Siblings::new(peers.iter().map(|&k| identity(world, k)))),
    }
}

/// One element as the cascade sees it: its identity and `place`, whether
/// it has element children and what text it holds (for `:empty`), and its
/// own declarations, its markup's and then its inline style's.
fn cascade_node(
    world: &World,
    entity: Entity,
    place: Place,
    has_element_children: bool,
) -> lumen_ir::css::AncestorInfo {
    use lumen_core::components::{InlineStyle, MarkupStyle};
    let (tag, classes, id) = identity(world, entity);
    let text = world
        .get::<lumen_core::components::TextContent>(entity)
        .map(|t| t.0.clone());
    let mut declarations: Vec<(String, String)> = world
        .get::<MarkupStyle>(entity)
        .map(|m| m.0.to_vec())
        .unwrap_or_default();
    if let Some(inline) = world.get::<InlineStyle>(entity) {
        declarations.extend(inline.0.iter().cloned());
    }
    lumen_ir::css::AncestorInfo::new(tag, classes, id)
        .with_position(place.position)
        .with_siblings(place.siblings)
        .with_content(has_element_children, text)
        .with_declarations(declarations)
}

/// `entity`'s ancestors, nearest first.
fn ancestors_of(world: &World, entity: Entity) -> Vec<Entity> {
    let mut out = Vec::new();
    let mut cur = entity;
    // The same cap the cascade's ancestor walk always had: a restyle must
    // never hang the tick.
    for _ in 0..256 {
        match world.get::<ChildOf>(cur) {
            Some(c) => {
                out.push(c.parent());
                cur = c.parent();
            }
            None => break,
        }
    }
    out
}

/// The root of the tree `entity` sits in: the last `ChildOf` parent up
/// from it, or `entity` itself when it has none.
fn topmost_ancestor(world: &World, entity: Entity) -> Entity {
    ancestors_of(world, entity)
        .last()
        .copied()
        .unwrap_or(entity)
}
