use super::*;

/// In-app error banner. Hot-reload parse failures land here; a
/// dedicated system paints a red strip at the top of the window with
/// the error text. Esc / click clears the banner.
#[derive(Resource, Default, Debug)]
pub struct ErrorBanner(pub Option<String>);

/// Marker for the entity spawned to render the [`ErrorBanner`]. Used
/// to find + update + despawn the banner across ticks.
#[derive(bevy_ecs::component::Component, Clone, Copy, Debug, Default)]
pub struct ErrorBannerMarker;

/// Reconcile the on-screen banner against [`ErrorBanner`]:
///
/// * banner present, no entity -> spawn an absolute-positioned red
///   strip with the message at the top of the viewport.
/// * banner present, entity exists -> patch the message in place.
/// * banner cleared, entity exists -> despawn.
///
/// Runs in `TickStage::Systems` so the layout pass picks the
/// banner's new style up the same tick it spawns.
pub fn reconcile_error_banner(
    mut commands: bevy_ecs::system::Commands,
    banner: bevy_ecs::system::Res<ErrorBanner>,
    existing: bevy_ecs::system::Query<
        (Entity, Option<&mut lumen_core::components::TextContent>),
        bevy_ecs::prelude::With<ErrorBannerMarker>,
    >,
) {
    let want = banner.0.as_deref();
    let mut existing = existing;
    let first = existing.iter_mut().next();
    match (want, first) {
        (Some(msg), None) => {
            commands.spawn((
                ErrorBannerMarker,
                lumen_core::components::DirtyLayout,
                lumen_core::components::Style {
                    width: lumen_core::components::Length::Percent(100.0),
                    height: lumen_core::components::Length::Px(36.0),
                    position: lumen_core::components::Position::Absolute,
                    // W2.6: never feed `f32::INFINITY` into taffy. The
                    // engine's `edges_to_lpa` only NaN-checks, and
                    // `LengthPercentageAuto::length(INF)` then poisons
                    // downstream comparisons inside taffy (bug 5 in
                    // `docs/audits/layout.md`). Clamp to `f32::MAX / 2`
                    // which is "effectively unbounded" without breaking
                    // taffy's `MIN..MAX` arithmetic.
                    inset: lumen_core::components::Edges {
                        top: 0.0,
                        right: 0.0,
                        bottom: f32::MAX / 2.0,
                        left: 0.0,
                        // W5.5: logical-edge overrides default to
                        // None so the physical sides above win.
                        ..Default::default()
                    },
                    padding: lumen_core::components::Edges {
                        top: 8.0,
                        bottom: 8.0,
                        left: 16.0,
                        right: 16.0,
                        ..Default::default()
                    },
                    ..Default::default()
                },
                lumen_core::components::Visuals {
                    fill: Some(lumen_core::components::Fill::Solid(
                        lumen_core::components::Color::rgba(0.78, 0.15, 0.15, 0.96),
                    )),
                    ..Default::default()
                },
                lumen_core::components::TextContent(msg.to_string()),
                lumen_core::components::TextStyle {
                    color: lumen_core::components::Color::rgb(1.0, 1.0, 1.0),
                    size_px: 14.0,
                    ..Default::default()
                },
            ));
        }
        (Some(msg), Some((_, Some(mut tc)))) if tc.0 != msg => {
            tc.0 = msg.to_string();
        }
        (None, Some((entity, _))) => {
            commands.entity(entity).despawn();
        }
        _ => {}
    }
}

/// Esc clears the error banner. Wired alongside the dialog Esc
/// handler so a single keystroke dismisses any blocking surface.
pub fn dismiss_error_banner_on_escape(
    mut keys: bevy_ecs::message::MessageReader<lumen_core::input::KeyPressed>,
    mut banner: bevy_ecs::system::ResMut<ErrorBanner>,
) {
    if banner.0.is_none() {
        keys.read().for_each(drop);
        return;
    }
    let pressed = keys.read().any(|k| {
        matches!(
            &k.key,
            lumen_core::input::Key::Named(lumen_core::input::NamedKey::Escape)
        )
    });
    if pressed {
        banner.0 = None;
    }
}

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

/// Build a [`MediaContext`] from the live [`StyleManager`] (color scheme)
/// and [`Viewport`] (logical width). Missing resources (pre-window)
/// leave the corresponding field unknown so no `@media` rule matches
/// spuriously.
pub(crate) fn media_context_from_world(world: &World) -> lumen_ir::css::MediaContext {
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
    let viewport_width = world
        .get_resource::<lumen_core::prelude::Viewport>()
        .map(|v| v.size.x);
    MediaContext {
        color_scheme,
        viewport_width,
        ..Default::default()
    }
}

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
pub(crate) fn detect_media_change(world: &mut World) {
    let media = media_context_from_world(world);
    let prev = world.get_resource::<LastMediaContext>().map(|m| m.0);
    // Borrow the invalidation cache in place - the breakpoint list was
    // being cloned into a fresh `Vec` every tick just to be read once.
    // Scoping the borrow lets us drop it before the mutable insert below.
    let changed = match prev {
        // First observation with a real context: force one re-resolve.
        None => true,
        Some(prev) => {
            let cache = world.get_resource::<StyleInvalidationCache>();
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
    };
    // Always refresh the baseline so breakpoint-crossing detection tracks
    // the latest width even on ticks that don't bump.
    world.insert_resource(LastMediaContext(media));
    if changed {
        StyleVersion::bump(world);
    }
}

/// W4.7 consumer: re-run the CSS cascade against every selector-reachable
/// entity (those carrying a [`LumenTag`] - i.e. spawned with a `class` or
/// `id`) and rewrite the affected style components in place. Runs only
/// after [`StyleVersion`] has moved (theme / media / root-class flip), so
/// steady-state ticks cost one integer compare.
///
/// Reuses [`lumen_ir::css::reapply_single_with_media`] (the same
/// cascade path the `<for>` reconciler uses for runtime-substituted rows)
/// against the live [`RuntimeStylesheet`] and [`MediaContext`]. Every
/// property the cascade resolved is copied back; a property the cascade
/// didn't set is left untouched so non-flipped inline values survive.
///
/// The element's own [`InlineStyle`] (the `set_style` / `element.style`
/// layer) folds on top of the stylesheet result, so an inline value beats
/// every author and skin rule. An app with no stylesheet at all still
/// gets its inline layer applied.
pub(crate) fn reapply_computed_styles(world: &mut World) {
    let version = world
        .get_resource::<StyleVersion>()
        .map(|v| v.0)
        .unwrap_or(0);
    if world
        .get_resource::<AppliedStyleVersion>()
        .is_some_and(|a| a.0 == version)
    {
        return;
    }
    let sheet = world
        .get_resource::<RuntimeStylesheet>()
        .map(|s| s.0.clone());
    let media = world
        .get_resource::<LastMediaContext>()
        .map(|m| m.0)
        .unwrap_or_default();

    use lumen_core::components::{LumenClasses, LumenId, LumenTag};
    // Rebuild a minimal `tag.class#id` cascade target for each reachable
    // entity, then resolve it against the live stylesheet + context.
    // W4.7 (ancestor pass): every entity carrying a `LumenTag` is a
    // cascade target; its ancestor chain (built below from `ChildOf`)
    // lets descendant / child combinators and ancestor-scoped `var()`
    // re-resolve on the flip.
    #[allow(clippy::type_complexity)]
    let mut q = world.query::<(Entity, &LumenTag, Option<&LumenClasses>, Option<&LumenId>)>();
    let entities: Vec<Entity> = q.iter(world).map(|(e, ..)| e).collect();
    // The app's stylesheet styles the app's document. A tree mounted as a
    // root of its own beside it (a dev tool's panel, styled by its own
    // sheet when it was spawned) is left alone, as the first cascade left
    // it alone.
    let document = world
        .get_resource::<lumen_scene::spawn::DocumentRoot>()
        .map(|r| r.0);
    // Sibling positions for the whole tree, once. The structural
    // pseudo-classes read them, and computing them per element would
    // re-walk each parent's child list once per child.
    let positions = sibling_positions(world);

    for entity in entities {
        if document.is_some_and(|root| topmost_ancestor(world, entity) != root) {
            continue;
        }
        // Reconstruct the subject element from its identity components.
        let Some(mut el) = entity_to_element(world, entity) else {
            continue;
        };
        let mut resolved = false;
        if let Some(sheet) = sheet.as_ref() {
            // Walk `ChildOf` up to the root, reading each ancestor's identity
            // components, then reverse to root-first order for the cascade.
            let ancestors = build_ancestor_chain(world, entity, &positions);
            let position = positions.get(&entity).copied().unwrap_or_default();
            if lumen_ir::css::reapply_with_ancestors(&mut el, sheet, &media, &ancestors, position)
                .is_err()
            {
                continue;
            }
            resolved = true;
        }
        // Markup attributes outrank every rule, as in the first cascade.
        resolved |= overlay_markup_style(world, entity, &mut el.attrs);
        // Highest cascade tier, applied last so it wins.
        resolved |= overlay_inline_style(world, entity, &mut el.attrs);
        if resolved {
            apply_reapplied_attrs(world, entity, &el.attrs);
        }
    }
    world.insert_resource(AppliedStyleVersion(version));
}

/// The root of the tree `entity` sits in: the last `ChildOf` parent up
/// from it, or `entity` itself when it has none.
fn topmost_ancestor(world: &World, entity: Entity) -> Entity {
    use bevy_ecs::hierarchy::ChildOf;
    let mut cur = entity;
    // Same cap as the ancestor-chain walk: the cascade must never hang.
    for _ in 0..256 {
        match world.get::<ChildOf>(cur) {
            Some(c) => cur = c.parent(),
            None => break,
        }
    }
    cur
}

/// Fold an entity's [`InlineStyle`] declarations onto an already-cascaded
/// [`Attributes`], returning whether anything landed. This is the DOM
/// `element.style` tier: it runs after the stylesheet pass so an inline
/// value overrides every author and skin rule, matching the precedence
/// `lumen_script::node_query::resolved_attributes` reports through
/// `computed_style`.
///
/// A property the value parser rejects is skipped and logged, mirroring
/// CSS error recovery; one bad `set_style` never discards the rest of the
/// element's inline layer.
fn overlay_inline_style(world: &World, entity: Entity, attrs: &mut Attributes) -> bool {
    let Some(inline) = world.get::<lumen_core::components::InlineStyle>(entity) else {
        return false;
    };
    let mut applied = false;
    for (property, value) in &inline.0 {
        match lumen_ir::css::apply_inline_declaration(property, value, attrs) {
            Ok(true) => applied = true,
            Ok(false) => {
                tracing::debug!("set_style: unknown property {property:?}, ignored")
            }
            Err(e) => tracing::warn!("set_style: {property}: {e}"),
        }
    }
    applied
}

/// Fold an entity's [`MarkupStyle`](lumen_core::components::MarkupStyle)
/// onto an already-cascaded [`Attributes`], returning whether anything
/// landed. The first cascade ranks a styling attribute above every rule;
/// this pass rebuilds the element from its tag, classes and id, so without
/// it a tag rule (`toggle { height: 36 }`) would overwrite the attribute
/// the markup wrote (`<toggle height="28px">`).
fn overlay_markup_style(world: &World, entity: Entity, attrs: &mut Attributes) -> bool {
    let Some(markup) = world.get::<lumen_core::components::MarkupStyle>(entity) else {
        return false;
    };
    let mut applied = false;
    for (property, value) in markup.0.iter() {
        // `flex-direction` stays with the cascade, as in the first one:
        // see `restore_inline_origin` in `lumen_ir::css`.
        if property == "flex-direction" {
            continue;
        }
        match lumen_ir::css::apply_inline_declaration(property, value, attrs) {
            Ok(true) => applied = true,
            Ok(false) => {}
            Err(e) => tracing::debug!("markup style {property}: {e}"),
        }
    }
    applied
}

/// Build a cascade target [`Element`] from an entity's identity
/// components (`LumenTag` / `LumenClasses` / `LumenId`). Returns `None`
/// when the entity carries no `LumenTag` (not a selector-reachable
/// element).
fn entity_to_element(world: &World, entity: Entity) -> Option<Element> {
    use lumen_core::components::{LumenClasses, LumenId, LumenTag};
    let tag = world.get::<LumenTag>(entity)?;
    let mut el = Element {
        tag: tag.0.to_string(),
        ..Default::default()
    };
    if let Some(c) = world.get::<LumenClasses>(entity) {
        el.attrs.classes = c.0.iter().map(|s| s.to_string()).collect();
    }
    if let Some(i) = world.get::<LumenId>(entity) {
        el.attrs.id = Some(i.0.clone());
    }
    Some(el)
}

/// Walk the `ChildOf` chain from `entity` up to the root, collecting each
/// ancestor's identity into an [`lumen_ir::css::AncestorInfo`], and
/// return the chain in root-first order (the order the cascade matcher
/// expects). A plain layout container with no `LumenTag` contributes an
/// empty-tag entry so child-combinator depth stays exact - the immediate
/// parent is always the immediate parent, tagged or not. Each ancestor
/// carries its real sibling position too, so `.row:first-child .cell`
/// selects on the row the cascade is actually standing under.
fn build_ancestor_chain(
    world: &World,
    entity: Entity,
    positions: &SiblingPositions,
) -> Vec<lumen_ir::css::AncestorInfo> {
    use bevy_ecs::hierarchy::ChildOf;
    use lumen_core::components::{LumenClasses, LumenId, LumenTag};
    let mut chain = Vec::new();
    let mut cur = entity;
    // Cap the walk so a pathological cycle can't spin forever (bevy_ecs
    // guards its hierarchy, but the cascade must never hang the tick).
    for _ in 0..256 {
        let Some(parent) = world.get::<ChildOf>(cur).map(|c| c.parent()) else {
            break;
        };
        let tag = world
            .get::<LumenTag>(parent)
            .map(|t| t.0.to_string())
            .unwrap_or_default();
        let classes = world
            .get::<LumenClasses>(parent)
            .map(|c| c.0.iter().map(|s| s.to_string()).collect())
            .unwrap_or_default();
        let id = world.get::<LumenId>(parent).map(|i| i.0.clone());
        chain.push(
            lumen_ir::css::AncestorInfo::new(tag, classes, id)
                .with_position(positions.get(&parent).copied().unwrap_or_default()),
        );
        cur = parent;
    }
    // Walked leaf->root; the matcher wants outer-first (root->parent).
    chain.reverse();
    chain
}

/// Sibling positions keyed by entity. An entity absent from the map has
/// no position to report and falls back to `SiblingPosition::default()`
/// (an only child), which is what a document root is.
type SiblingPositions = std::collections::HashMap<Entity, lumen_ir::css::SiblingPosition>;

/// Where every element sits among the element siblings sharing its
/// parent, for the structural pseudo-classes. Only entities carrying a
/// [`LumenTag`] count as siblings: that is the marker every spawned
/// markup element gets and nothing else does, so a renderer-internal
/// child added beside a row does not shift the row's `:nth-child`.
fn sibling_positions(world: &mut World) -> SiblingPositions {
    use bevy_ecs::hierarchy::Children;
    use bevy_ecs::query::With;
    use lumen_core::components::LumenTag;
    use lumen_ir::css::SiblingPosition;

    let mut tagged = world.query_filtered::<Entity, With<LumenTag>>();
    let tagged: std::collections::HashSet<Entity> = tagged.iter(world).collect();

    let mut parents = world.query::<&Children>();
    let mut out = SiblingPositions::new();
    for children in parents.iter(world) {
        let siblings: Vec<Entity> = children.iter().filter(|e| tagged.contains(e)).collect();
        let count = siblings.len() as i32;
        for (i, entity) in siblings.into_iter().enumerate() {
            out.insert(entity, SiblingPosition::new(i as i32 + 1, count));
        }
    }
    out
}

/// Patch the cascade result from [`reapply_computed_styles`]
/// onto an entity's live style components. Field-level so props the
/// cascade didn't touch keep their spawn-time values; inserts an absent
/// [`Visuals`] / [`TextStyle`] / [`Interaction`] only when the cascade
/// produced a value for it.
fn apply_reapplied_attrs(world: &mut World, entity: Entity, attrs: &Attributes) {
    use lumen_core::components::{
        Color, Fill, Opacity, ShadowSpec, Style, TextInput, TextInputPaint, TextStyle, Visuals,
    };
    use lumen_primitives::{
        BackgroundTransition, BorderColorTransition, OpacityTransition, TextColorTransition,
        TransitionProperty, TransitionSpecs, retarget,
    };
    // Restyle tweens only make sense while the window can actually
    // animate: an unfocused / occluded window's redraw scheduler is
    // paused, so a tween inserted there freezes mid-flight until focus
    // returns (the app parks between events). Snap instead - matching
    // what the user sees from every native toolkit when the OS theme
    // flips a background window.
    let can_animate = world
        .get_resource::<lumen_core::window_backend::RedrawScheduler>()
        .map(|s| !s.paused)
        .unwrap_or(true);
    let mut ent = world.entity_mut(entity);
    // Set when any restyle tween (`*Transition`) is inserted below, so
    // the end of the pass can raise `AnimationsActive` - without the
    // wake the app can sleep mid-tween and freeze a theme flip halfway
    // between the light and dark fills.
    let mut inserted_tween = false;

    // Refresh the entity's transition declarations first so the value
    // writes below observe the restyled `transition:` list.
    let eff_transitions = attrs.effective_transitions();
    if !eff_transitions.is_empty() {
        let specs: Vec<lumen_primitives::TransitionSpec> =
            eff_transitions.iter().map(Into::into).collect();
        ent.insert(TransitionSpecs(specs));
    }
    let specs = ent.get::<TransitionSpecs>().cloned();
    let spec_for = |p: TransitionProperty| {
        if !can_animate {
            return None;
        }
        specs
            .as_ref()
            .and_then(|s| s.for_property(p))
            .filter(|s| !s.duration.is_zero())
            .copied()
    };

    // Layout props on the always-present `Style`, through the same
    // mapping spawning uses (`Attributes::patch_style`), so a restyle
    // reaches every property the initial resolution does. Written only
    // when something moved: the layout crate's `Changed<Style>` watcher
    // marks `DirtyLayout` on any mutable access.
    if let Some(live) = ent.get::<Style>() {
        let mut next = live.clone();
        attrs.patch_style(&mut next);
        if next != *live
            && let Some(mut style) = ent.get_mut::<Style>()
        {
            *style = next;
        }
    }

    // Sibling paint order.
    if let Some(z) = attrs.z_index {
        ent.insert(lumen_core::components::ZIndex(z));
    }

    // Paint props -> `Visuals`. A `bg` image replaces the element's colour,
    // which the asset pipeline then paints in its place.
    let want_fill = attrs.bg.as_ref().and_then(|bg| Fill::try_from(bg).ok());
    if matches!(attrs.bg, Some(lumen_ir::layout_ir::BgSpec::Image(_)))
        && let Some(mut v) = ent.get_mut::<Visuals>()
        && v.fill.is_some()
    {
        v.fill = None;
    }
    let tag = ent
        .get::<lumen_core::components::LumenTag>()
        .map(|t| t.0.to_string())
        .unwrap_or_default();
    lumen_scene::spawn::apply_background_image(&mut ent, &tag, attrs);
    let want_radius = attrs.radius;
    let want_shadows: Vec<ShadowSpec> = attrs.shadows.iter().copied().map(Into::into).collect();
    let border_authored = attrs.border_style.is_some();
    let want_border: Option<lumen_core::components::Border> =
        attrs
            .effective_border()
            .map(|(widths, color)| lumen_core::components::Border {
                widths: widths.into(),
                color: color.into(),
                side_colors: attrs
                    .effective_border_colors(color)
                    .map(|cs| cs.map(Into::into)),
            });
    if want_fill.is_some() || want_radius.is_some() || !want_shadows.is_empty() || border_authored {
        // Hover / press tint FSM currently owns the fill? Snap instead
        // of starting a competing restyle tween.
        let fill_owned_by_state_fsm = ent.get::<lumen_primitives::HoverBaseColor>().is_some()
            || ent.get::<lumen_primitives::PressBaseColor>().is_some();
        let rest_color = want_fill.as_ref().and_then(Fill::as_solid);
        if let Some(current_vis) = ent.get::<Visuals>().cloned() {
            // CSS transition trigger: a computed-style change on an
            // entity with a matching `transition:` declaration tweens
            // from the CURRENT value (retarget semantics - an in-flight
            // tween restarts from its live interpolated value, which is
            // exactly what the component holds right now).
            let mut bg_tween: Option<BackgroundTransition> = None;
            let mut border_tween: Option<BorderColorTransition> = None;
            if let (Some(Fill::Solid(new)), Some(spec)) = (
                want_fill.as_ref(),
                spec_for(TransitionProperty::BackgroundColor),
            ) && !fill_owned_by_state_fsm
                && let Some(cur) = current_vis.fill.as_ref().and_then(Fill::as_solid)
            {
                bg_tween = retarget(cur, *new, &spec).map(BackgroundTransition);
            }
            if border_authored
                && let (Some(new_border), Some(cur_border), Some(spec)) = (
                    want_border.as_ref(),
                    current_vis.border.as_ref(),
                    spec_for(TransitionProperty::BorderColor),
                )
                && new_border.widths == cur_border.widths
            {
                border_tween =
                    retarget(cur_border.color, new_border.color, &spec).map(BorderColorTransition);
            }
            let mut v = ent.get_mut::<Visuals>().expect("checked above");
            if let Some(f) = want_fill
                && bg_tween.is_none()
            {
                v.fill = Some(f);
            }
            // else: the driver animates fill from current -> new.
            if let Some(r) = want_radius {
                v.radius = r;
            }
            if attrs.radius.is_some() || attrs.radius_corners.is_some() {
                v.corner_radii = attrs.radius_corners;
            }
            if !want_shadows.is_empty() {
                v.shadows = want_shadows;
            }
            if border_authored && border_tween.is_none() {
                v.border = want_border;
            }
            // The hover / press tint snapshotted the resting colour on
            // entry and fades back to that snapshot on exit. A new resting
            // colour is what it must fade back to, or leaving the element
            // undoes the restyle.
            if fill_owned_by_state_fsm && let Some(rest) = rest_color {
                if let Some(mut base) = ent.get_mut::<lumen_primitives::HoverBaseColor>() {
                    base.0 = rest;
                }
                if let Some(mut base) = ent.get_mut::<lumen_primitives::PressBaseColor>() {
                    base.0 = rest;
                }
            }
            if let Some(t) = bg_tween {
                ent.insert(t);
                inserted_tween = true;
            }
            if let Some(t) = border_tween {
                ent.insert(t);
                inserted_tween = true;
            }
        } else {
            ent.insert(Visuals {
                fill: want_fill,
                radius: want_radius.unwrap_or(0.0),
                corner_radii: attrs.radius_corners,
                shadows: want_shadows,
                border: want_border,
            });
        }
    }

    // Text props -> `TextStyle`.
    let role_size = attrs
        .style_role
        .as_deref()
        .and_then(crate::spawn::typography_role_to_px);
    let want_size = attrs.font_size.or(role_size);
    // `lumen_ir::layout_ir::LineHeightSpec` -> `lumen_core::components::LineHeightSpec`,
    // a 1:1 variant mapping. `lumen-core` cannot depend on `lumen-ir` (the
    // dependency would cycle), so the two enums are separate types and this
    // is the one place, at the IR/ECS boundary, that converts between them.
    let want_line_height = attrs.line_height.map(|lh| match lh {
        lumen_ir::layout_ir::LineHeightSpec::Multiplier(m) => {
            lumen_core::components::LineHeightSpec::Multiplier(m)
        }
        lumen_ir::layout_ir::LineHeightSpec::Px(px) => {
            lumen_core::components::LineHeightSpec::Px(px)
        }
    });
    if attrs.text_color.is_some()
        || want_size.is_some()
        || attrs.text_align.is_some()
        || attrs.text_wrap.is_some()
        || attrs.max_lines.is_some()
        || attrs.font_family.is_some()
        || attrs.font_weight.is_some()
        || attrs.selection_color.is_some()
        || want_line_height.is_some()
    {
        let text_tween: Option<TextColorTransition> = match (
            attrs.text_color,
            spec_for(TransitionProperty::TextColor),
            ent.get::<TextStyle>(),
        ) {
            (Some(new), Some(spec), Some(ts)) => {
                retarget(ts.color, new.into(), &spec).map(TextColorTransition)
            }
            _ => None,
        };
        if let Some(mut ts) = ent.get_mut::<TextStyle>() {
            if let Some(c) = attrs.text_color
                && text_tween.is_none()
            {
                ts.color = c.into();
            }
            if let Some(s) = want_size {
                ts.size_px = s;
            }
            if let Some(a) = attrs.text_align {
                ts.align = a.into();
            }
            if let Some(w) = attrs.text_wrap {
                ts.wrap = w.into();
            }
            if attrs.max_lines.is_some() {
                ts.max_lines = attrs.max_lines;
            }
            if let Some(f) = attrs.font_family.as_deref() {
                ts.family = Some(std::sync::Arc::<str>::from(f));
            }
            if let Some(w) = attrs.font_weight {
                ts.weight = w;
            }
            if attrs.selection_color.is_some() {
                ts.selection_color = attrs.selection_color.map(Into::into);
            }
            if want_line_height.is_some() {
                ts.line_height = want_line_height;
            }
        } else {
            let d = TextStyle::default();
            ent.insert(TextStyle {
                color: attrs.text_color.map(Into::into).unwrap_or(d.color),
                size_px: want_size.unwrap_or(d.size_px),
                align: attrs.text_align.map(Into::into).unwrap_or(d.align),
                wrap: attrs.text_wrap.map(Into::into).unwrap_or(d.wrap),
                max_lines: attrs.max_lines,
                family: attrs
                    .font_family
                    .as_deref()
                    .map(std::sync::Arc::<str>::from),
                weight: attrs.font_weight.unwrap_or(d.weight),
                selection_color: attrs.selection_color.map(Into::into),
                line_height: want_line_height.or(d.line_height),
            });
        }
        if let Some(t) = text_tween {
            ent.insert(t);
            inserted_tween = true;
        }
    }

    // Text-input caret / selected-glyph paint -> `TextInputPaint`. Gated
    // on the entity actually being a text input (`caret-color` /
    // `selection-text-color` are inert elsewhere) so the restyle flip
    // never adds the component to plain text nodes.
    if ent.contains::<TextInput>()
        && (attrs.caret_color.is_some() || attrs.selection_text_color.is_some())
    {
        let want_caret = attrs.caret_color.map(Into::into);
        let want_sel_fg = attrs.selection_text_color.map(Into::into);
        if let Some(mut paint) = ent.get_mut::<TextInputPaint>() {
            if attrs.caret_color.is_some() {
                paint.caret_color = want_caret;
            }
            if attrs.selection_text_color.is_some() {
                paint.selection_foreground = want_sel_fg;
            }
        } else {
            ent.insert(TextInputPaint {
                caret_color: want_caret,
                selection_foreground: want_sel_fg,
            });
        }
    }

    // `caret-width` / `password-character` -> per-entity override
    // components, split off from `TextInputPaint` rather than added to it
    // so they never touch that component's existing struct literals
    // elsewhere in the tree (same reasoning as `TextInputPaint`'s own doc
    // comment - `core/scene/src/spawn.rs` already constructs one
    // exhaustively). Gated on `TextInput` for the same reason as the
    // paint block above.
    if ent.contains::<TextInput>() {
        if let Some(w) = attrs.caret_width {
            ent.insert(lumen_core::components::CaretWidth(w));
        }
        if let Some(c) = attrs.password_character {
            ent.insert(lumen_core::components::PasswordCharacter(c));
        }
    }

    // Opacity - tweened when a `transition: opacity ...` declaration is
    // present. An element that never had an opacity starts from the
    // computed `1`, as in CSS, and gets the component the tween drives.
    if let Some(o) = attrs.opacity {
        match spec_for(TransitionProperty::Opacity) {
            Some(spec) => {
                let cur = ent.get::<Opacity>().copied().unwrap_or(Opacity(1.0));
                if let Some(t) = retarget(cur.0, o, &spec) {
                    if !ent.contains::<Opacity>() {
                        ent.insert(cur);
                    }
                    ent.insert(OpacityTransition(t));
                    inserted_tween = true;
                } else if !ent.contains::<Opacity>() {
                    ent.insert(Opacity(o));
                }
            }
            None => {
                ent.insert(Opacity(o));
            }
        }
    }

    // Overlay-scrollbar styling (CSS `scrollbar-color` / `scrollbar-width`
    // / `scrollbar-track-hover` / `scrollbar-hover-boost`) - re-resolved on
    // theme / media flips so a dark skin can retint the bars.
    // `scrollbar-thickness(-thin)`, `-margin`, `-min-thumb` and the fade
    // timings are read at spawn only.
    if attrs.scrollbar_color.is_some()
        || attrs.scrollbar_width.is_some()
        || attrs.scrollbar_track_hover.is_some()
        || attrs.scrollbar_hover_boost.is_some()
    {
        let mut sb = ent
            .get::<lumen_core::input::ScrollbarStyle>()
            .copied()
            .unwrap_or_default();
        if let Some((thumb, track)) = attrs.scrollbar_color {
            sb.thumb = thumb.into();
            sb.track = track.map(Into::into);
        }
        if let Some(w) = attrs.scrollbar_width {
            sb.width = w.into();
        }
        if let Some(c) = attrs.scrollbar_track_hover {
            sb.hover_track = c.into();
        }
        if let Some(b) = attrs.scrollbar_hover_boost {
            sb.hover_boost = b;
        }
        ent.insert(sb);
    }

    // Interaction tints + state borders -> `lumen_primitives::Interaction`.
    let want_hover: Option<Color> = attrs.hover_bg.map(Into::into);
    let want_press: Option<Color> = attrs.press_bg.map(Into::into);
    let want_hover_border: Option<lumen_core::components::Border> =
        attrs.hover_border.map(Into::into);
    let want_focus_border: Option<lumen_core::components::Border> =
        attrs.focus_border.map(Into::into);
    let want_focus_outline: Option<lumen_primitives::FocusOutlineSpec> =
        attrs.focus_outline.map(Into::into);
    let want_focus_visible_outline: Option<lumen_primitives::FocusOutlineSpec> =
        attrs.focus_visible_outline.map(Into::into);
    if want_hover.is_some()
        || want_press.is_some()
        || want_hover_border.is_some()
        || want_focus_border.is_some()
        || want_focus_outline.is_some()
        || want_focus_visible_outline.is_some()
    {
        if let Some(mut ix) = ent.get_mut::<lumen_primitives::Interaction>() {
            if let Some(h) = want_hover {
                ix.hover_tint = Some(h);
            }
            if let Some(p) = want_press {
                ix.press_tint = Some(p);
            }
            if want_hover_border.is_some() {
                ix.hover_border = want_hover_border;
            }
            if want_focus_border.is_some() {
                ix.focus_border = want_focus_border;
            }
            if want_focus_outline.is_some() {
                ix.focus_outline = want_focus_outline;
            }
            if want_focus_visible_outline.is_some() {
                ix.focus_visible_outline = want_focus_visible_outline;
            }
        } else {
            ent.insert(lumen_primitives::Interaction {
                hover_tint: want_hover,
                press_tint: want_press,
                focus_outline: want_focus_outline,
                focus_visible_outline: want_focus_visible_outline,
                hover_border: want_hover_border,
                focus_border: want_focus_border,
            });
        }
    }

    // Stateful track fills: `<toggle>` / `<switch>` / tab-strip buttons
    // own their live fill via TrackStyle / TabButtonStyle (the sync
    // systems swap it as state flips). The generic Visuals write above
    // painted the resting `bg` - correct the palette from the re-resolved
    // state fills and repaint per the CURRENT state so a theme flip
    // doesn't stomp a checked track back to the unchecked color.
    {
        use lumen_core::components::Visuals as V;
        let checked = ent
            .get::<lumen_core::components::Toggleable>()
            .map(|t| t.checked);
        let disabled = ent.contains::<lumen_core::components::Disabled>();
        if let Some(checked) = checked
            && let Some(mut ts) = ent.get_mut::<lumen_primitives::TrackStyle>()
        {
            *ts = lumen_scene::spawn::track_style_over(*ts, attrs);
            let live = ts.fill_for(checked, disabled);
            if let Some(mut base) = ent.get_mut::<lumen_primitives::HoverBaseColor>() {
                base.0 = live;
            }
            if let Some(mut base) = ent.get_mut::<lumen_primitives::PressBaseColor>() {
                base.0 = live;
            }
            if let Some(mut v) = ent.get_mut::<V>() {
                v.fill = Some(Fill::Solid(live));
            }
            // The generic pass above may have queued a restyle tween
            // toward the resting `bg` - retire it, the state fill is
            // authoritative here.
            ent.remove::<BackgroundTransition>();
        }
        let selected = ent
            .get::<lumen_primitives::TabStripButton>()
            .is_some()
            .then(|| ent.get::<lumen_core::components::Selected>().is_some());
        if let Some(selected) = selected
            && let Some(mut ts) = ent.get_mut::<lumen_primitives::TabButtonStyle>()
        {
            if let Some(Ok(Fill::Solid(c))) = attrs.bg.as_ref().map(Fill::try_from) {
                ts.unselected_bg = c;
            }
            if let Some(c) = attrs.selected_bg {
                ts.selected_bg = c.into();
            }
            let live = if selected {
                ts.selected_bg
            } else {
                ts.unselected_bg
            };
            if let Some(mut base) = ent.get_mut::<lumen_primitives::HoverBaseColor>() {
                base.0 = live;
            }
            if let Some(mut base) = ent.get_mut::<lumen_primitives::PressBaseColor>() {
                base.0 = live;
            }
            if let Some(mut v) = ent.get_mut::<V>() {
                v.fill = Some(Fill::Solid(live));
            }
            ent.remove::<BackgroundTransition>();
        }
    }

    // State-routed text-color / opacity / box-shadow swaps - mirror the
    // spawn-time StateVisuals assembly so a theme flip restyles them.
    {
        let to_shadows = |v: &Option<Vec<lumen_ir::layout_ir::ShadowSpec>>| {
            v.as_ref()
                .map(|list| list.iter().copied().map(Into::into).collect())
        };
        // The runtime `:disabled` patch exists only on `bind-disabled`
        // entities (spawn parity - static disabled styling applied once
        // at spawn); the marker component tells us which ones.
        let disabled_patch = if ent.get::<lumen_core::components::BindDisabled>().is_some() {
            lumen_primitives::StatePatch {
                text_color: attrs.disabled_text_color.map(Into::into),
                opacity: attrs.disabled_opacity.or({
                    if attrs.disabled_bg.is_none() && attrs.opacity.is_none() {
                        // `disabled-opacity` is the CSS-authored version of
                        // this fallback; `0.5` remains the last-resort
                        // constant when no CSS supplies it either.
                        Some(attrs.disabled_opacity_default.unwrap_or(0.5))
                    } else {
                        None
                    }
                }),
                shadows: to_shadows(&attrs.disabled_shadows),
                bg: attrs.disabled_bg.map(Into::into),
            }
        } else {
            lumen_primitives::StatePatch::default()
        };
        let sv = lumen_primitives::StateVisuals {
            hover: lumen_primitives::StatePatch {
                text_color: attrs.hover_text_color.map(Into::into),
                opacity: attrs.hover_opacity,
                shadows: to_shadows(&attrs.hover_shadows),
                bg: None,
            },
            focus: lumen_primitives::StatePatch {
                text_color: attrs.focus_text_color.map(Into::into),
                opacity: attrs.focus_opacity,
                shadows: to_shadows(&attrs.focus_shadows),
                bg: None,
            },
            focus_visible: lumen_primitives::StatePatch {
                text_color: attrs.focus_visible_text_color.map(Into::into),
                opacity: attrs.focus_visible_opacity,
                shadows: to_shadows(&attrs.focus_visible_shadows),
                bg: None,
            },
            active: lumen_primitives::StatePatch {
                text_color: attrs.active_text_color.map(Into::into),
                opacity: attrs.active_opacity,
                shadows: to_shadows(&attrs.active_shadows),
                bg: None,
            },
            drag_over: lumen_primitives::StatePatch {
                text_color: attrs.drag_over_text_color.map(Into::into),
                opacity: attrs.drag_over_opacity,
                shadows: to_shadows(&attrs.drag_over_shadows),
                bg: attrs.drag_over_bg.map(Into::into),
            },
            disabled: disabled_patch,
        };
        if !sv.is_empty() {
            ent.insert(sv);
        }
    }

    // Entity borrow ends here; the trailing passes go through `world`.
    let _ = ent;

    // `caret-blink` -> the global `CaretBlink` resource. There is no
    // per-entity blink state (one shared phase drives every focused
    // input's caret), so a CSS override only has one place to land; gate
    // on the entity actually being the kind `caret-blink` targets so an
    // unrelated element's cascade never repaints the shared period.
    if let Some(ms) = attrs.caret_blink_ms
        && world.get::<TextInput>(entity).is_some()
        && let Some(mut blink) = world.get_resource_mut::<lumen_core::components::CaretBlink>()
    {
        blink.period = std::time::Duration::from_millis(ms as u64);
    }

    // Knob / thumb child fill (`knob-color`): the knob entities carry no
    // tag of their own, so the flip re-resolves the parent's knob-color
    // and pushes it onto the child here.
    if let Some(knob) = attrs.knob_color {
        let children: Vec<Entity> = world
            .get::<bevy_ecs::hierarchy::Children>(entity)
            .map(|c| c.iter().collect())
            .unwrap_or_default();
        for child in children {
            let is_knob = world.get::<lumen_primitives::ToggleKnob>(child).is_some()
                || world.get::<lumen_primitives::SliderThumb>(child).is_some();
            if is_knob && let Some(mut v) = world.get_mut::<Visuals>(child) {
                v.fill = Some(Fill::Solid(knob.into()));
            }
        }
    }

    // Keep the loop awake while any restyle tween is in flight (the
    // step systems keep re-requesting until done).
    if inserted_tween
        && let Some(anim) = world.get_resource::<lumen_core::render_world::AnimationsActive>()
    {
        anim.request();
    }
}

#[cfg(test)]
mod apply_reapplied_attrs_tests {
    //! `apply_reapplied_attrs` is the live-reapply half of the seven
    //! Phase-2 properties Phase 1 whitelisted in `copy_back_reapplied`
    //! but could not finish wiring (no component field existed yet):
    //! `line_height`, `caret_width`, `caret_blink_ms`,
    //! `password_character`, `scrollbar_track_hover` and
    //! `scrollbar_hover_boost` (`disabled_opacity_default` was already
    //! wired). These tests call the field-level reapply directly - the
    //! cascade itself is exercised elsewhere (`lumen_ir::css` tests).
    use super::*;
    use lumen_core::components::{
        CaretBlink, CaretWidth, LineHeightSpec as CoreLineHeightSpec, PasswordCharacter, TextInput,
        TextStyle,
    };
    use lumen_core::input::ScrollbarStyle;
    use lumen_ir::layout_ir::{Attributes, LineHeightSpec as IrLineHeightSpec, Rgba};

    /// A live `line-height` reapply overwrites an existing `TextStyle`'s
    /// `line_height` field, converting the IR spec to the core mirror
    /// type 1:1.
    #[test]
    fn line_height_reapply_updates_existing_text_style() {
        let mut world = World::new();
        let e = world.spawn(TextStyle::default()).id();
        let attrs = Attributes {
            line_height: Some(IrLineHeightSpec::Multiplier(1.5)),
            ..Default::default()
        };
        apply_reapplied_attrs(&mut world, e, &attrs);
        assert_eq!(
            world.get::<TextStyle>(e).unwrap().line_height,
            Some(CoreLineHeightSpec::Multiplier(1.5))
        );
    }

    /// A CSS `line-height: <px>` reapply carries through as
    /// `LineHeightSpec::Px`, not folded into a multiplier.
    #[test]
    fn line_height_reapply_preserves_px_variant() {
        let mut world = World::new();
        let e = world.spawn(TextStyle::default()).id();
        let attrs = Attributes {
            line_height: Some(IrLineHeightSpec::Px(19.0)),
            ..Default::default()
        };
        apply_reapplied_attrs(&mut world, e, &attrs);
        assert_eq!(
            world.get::<TextStyle>(e).unwrap().line_height,
            Some(CoreLineHeightSpec::Px(19.0))
        );
    }

    /// `caret-width` / `password-character` reapply onto a `TextInput`
    /// entity inserts the override components; an entity that isn't a
    /// `TextInput` never gains them (matches the spawn-time gating on
    /// `TextInputPaint`).
    #[test]
    fn caret_width_and_password_character_reapply_gated_on_text_input() {
        let mut world = World::new();
        let input = world.spawn(TextInput::default()).id();
        let plain = world.spawn(TextStyle::default()).id();
        let attrs = Attributes {
            caret_width: Some(3.5),
            password_character: Some('#'),
            ..Default::default()
        };
        apply_reapplied_attrs(&mut world, input, &attrs);
        apply_reapplied_attrs(&mut world, plain, &attrs);

        assert_eq!(world.get::<CaretWidth>(input), Some(&CaretWidth(3.5)));
        assert_eq!(
            world.get::<PasswordCharacter>(input),
            Some(&PasswordCharacter('#'))
        );
        assert!(
            world.get::<CaretWidth>(plain).is_none(),
            "non-text-input entity must not gain a CaretWidth override"
        );
        assert!(
            world.get::<PasswordCharacter>(plain).is_none(),
            "non-text-input entity must not gain a PasswordCharacter override"
        );
    }

    /// `scrollbar-track-hover` / `scrollbar-hover-boost` reapply patches
    /// the fields on an existing `ScrollbarStyle`, leaving the rest of
    /// the component (thumb / track / width / margin / ...) untouched.
    #[test]
    fn scrollbar_track_hover_and_hover_boost_reapply_patch_existing_style() {
        let mut world = World::new();
        let base = ScrollbarStyle::default();
        let e = world.spawn(base).id();
        let attrs = Attributes {
            scrollbar_track_hover: Some(Rgba {
                r: 1.0,
                g: 0.0,
                b: 0.0,
                a: 0.5,
            }),
            scrollbar_hover_boost: Some(2.0),
            ..Default::default()
        };
        apply_reapplied_attrs(&mut world, e, &attrs);
        let sb = world.get::<ScrollbarStyle>(e).unwrap();
        assert_eq!(sb.hover_boost, 2.0);
        assert_eq!(sb.hover_track.r, 1.0);
        assert_eq!(sb.hover_track.a, 0.5);
        // Untouched fields keep the pre-existing default.
        assert_eq!(sb.thumb, base.thumb);
        assert_eq!(sb.margin, base.margin);
    }

    /// `caret-blink` reapply on a `TextInput` entity updates the shared
    /// `CaretBlink` resource's period; absent a `TextInput`, the global
    /// resource is left alone (there is no per-entity blink state to
    /// target, so an unrelated element must not repaint it).
    #[test]
    fn caret_blink_reapply_updates_global_resource_when_gated() {
        let mut world = World::new();
        world.insert_resource(CaretBlink {
            visible: true,
            phase: std::time::Instant::now(),
            period: std::time::Duration::from_millis(530),
        });
        let plain = world.spawn(TextStyle::default()).id();
        let attrs = Attributes {
            caret_blink_ms: Some(250),
            ..Default::default()
        };
        apply_reapplied_attrs(&mut world, plain, &attrs);
        assert_eq!(
            world.resource::<CaretBlink>().period,
            std::time::Duration::from_millis(530),
            "non-text-input entity must not repaint the shared blink period"
        );

        let input = world.spawn(TextInput::default()).id();
        apply_reapplied_attrs(&mut world, input, &attrs);
        assert_eq!(
            world.resource::<CaretBlink>().period,
            std::time::Duration::from_millis(250)
        );
    }

    /// A restyle writes every layout property the cascade resolved onto the
    /// live `Style`, exactly as spawning would have: justify, align,
    /// position, inset, overflow and the grid properties were once missing
    /// from the restyle path, so a class added from a script or a `@media`
    /// breakpoint crossing never moved them.
    #[test]
    fn restyle_reaches_every_layout_property() {
        use lumen_core::components::Style;
        use lumen_ir::layout_ir::{
            AlignContentSpec, BorderStyleSpec, BoxSizingSpec, DisplaySpec, Edges, FlexAlign,
            FlexAxis, FlexJustify, FlexWrapSpec, GridTemplateSpec, LengthSpec, OverflowSpec,
            PositionSpec, TrackSizeSpec,
        };
        let edges = |v: f32| Edges {
            left: v,
            right: v + 1.0,
            top: v + 2.0,
            bottom: v + 3.0,
            ..Default::default()
        };
        let attrs = Attributes {
            display: Some(DisplaySpec::Grid),
            width: Some(LengthSpec::Px(120.0)),
            height: Some(LengthSpec::Percent(50.0)),
            flex: Some(FlexAxis::Column),
            padding: Some(edges(1.0)),
            margin: Some(edges(5.0)),
            gap: Some(4.0),
            gap_row: Some(6.0),
            gap_pct: Some(3.0),
            grow: Some(1.0),
            align: Some(FlexAlign::Center),
            justify: Some(FlexJustify::End),
            align_self: Some(FlexAlign::End),
            justify_items: Some(FlexAlign::Center),
            justify_self: Some(FlexAlign::Start),
            grid_template: Some(GridTemplateSpec {
                rows: vec![TrackSizeSpec::Auto],
                columns: vec![TrackSizeSpec::Fixed(100.0), TrackSizeSpec::Fr(1.0)],
            }),
            grid_row: Some((1, 2)),
            grid_column: Some((1, 4)),
            position: Some(PositionSpec::Absolute),
            inset: Some(edges(10.0)),
            min_width: Some(LengthSpec::Px(10.0)),
            min_height: Some(LengthSpec::Px(11.0)),
            max_width: Some(LengthSpec::Px(500.0)),
            max_height: Some(LengthSpec::Px(600.0)),
            aspect_ratio: Some(2.0),
            overflow: Some(OverflowSpec::Hidden),
            overflow_y: Some(OverflowSpec::Scroll),
            shrink: Some(0.0),
            basis: Some(LengthSpec::Px(30.0)),
            flex_wrap: Some(FlexWrapSpec::Wrap),
            align_content: Some(AlignContentSpec::Center),
            border_style: Some(BorderStyleSpec::Solid),
            border_width: Some(edges(2.0)),
            box_sizing: Some(BoxSizingSpec::ContentBox),
            ..Default::default()
        };
        let mut world = World::new();
        let e = world.spawn(Style::default()).id();
        apply_reapplied_attrs(&mut world, e, &attrs);
        let live = world.get::<Style>(e).unwrap().clone();
        assert_eq!(live, Style::from(&attrs), "restyle and spawn agree");

        // Every field moved off its initial value, so the attributes above
        // exercise every property kind. Destructured without `..`: a new
        // `Style` field fails to compile here until the test covers it.
        let d = Style::default();
        let Style {
            display,
            width,
            height,
            flex_direction,
            padding,
            margin,
            gap,
            grow,
            align,
            justify,
            align_self,
            justify_items,
            justify_self,
            grid_template,
            grid_row,
            grid_column,
            position,
            inset,
            min_width,
            min_height,
            max_width,
            max_height,
            aspect_ratio,
            overflow_x,
            overflow_y,
            shrink,
            basis,
            flex_wrap,
            align_content,
            border,
            box_sizing,
        } = live;
        assert_ne!(display, d.display, "display");
        assert_ne!(width, d.width, "width");
        assert_ne!(height, d.height, "height");
        assert_ne!(flex_direction, d.flex_direction, "flex-direction");
        assert_ne!(padding, d.padding, "padding");
        assert_ne!(margin, d.margin, "margin");
        assert_ne!(gap, d.gap, "gap");
        assert_ne!(grow, d.grow, "grow");
        assert_ne!(align, d.align, "align");
        assert_ne!(justify, d.justify, "justify");
        assert_ne!(align_self, d.align_self, "align-self");
        assert_ne!(justify_items, d.justify_items, "justify-items");
        assert_ne!(justify_self, d.justify_self, "justify-self");
        assert_ne!(grid_template, d.grid_template, "grid-template");
        assert_ne!(grid_row, d.grid_row, "grid-row");
        assert_ne!(grid_column, d.grid_column, "grid-column");
        assert_ne!(position, d.position, "position");
        assert_ne!(inset, d.inset, "inset");
        assert_ne!(min_width, d.min_width, "min-width");
        assert_ne!(min_height, d.min_height, "min-height");
        assert_ne!(max_width, d.max_width, "max-width");
        assert_ne!(max_height, d.max_height, "max-height");
        assert_ne!(aspect_ratio, d.aspect_ratio, "aspect-ratio");
        assert_ne!(overflow_x, d.overflow_x, "overflow-x");
        assert_ne!(overflow_y, d.overflow_y, "overflow-y");
        assert_ne!(shrink, d.shrink, "shrink");
        assert_ne!(basis, d.basis, "basis");
        assert_ne!(flex_wrap, d.flex_wrap, "flex-wrap");
        assert_ne!(align_content, d.align_content, "align-content");
        assert_ne!(border, d.border, "border");
        assert_ne!(box_sizing, d.box_sizing, "box-sizing");
        assert_eq!(gap.row, 6.0, "row-gap overrides the gap shorthand");
        assert_eq!(gap.column, 4.0, "gap shorthand fills the other axis");
    }

    /// A resting `bg` that changes while the element is hovered or pressed
    /// becomes the colour the hover and press tints fade back to. They
    /// once faded back to the colour they snapshotted on entry, so leaving
    /// a button undid the `active` class its click handler had added.
    #[test]
    fn restyle_while_hovered_rebases_the_tint_snapshots() {
        use lumen_core::components::{Color, Fill, Visuals};
        use lumen_ir::layout_ir::BgSpec;
        use lumen_primitives::{HoverBaseColor, PressBaseColor};
        let old = Color::rgba(0.0, 0.0, 0.0, 0.0);
        let mut world = World::new();
        let e = world
            .spawn((
                Visuals {
                    fill: Some(Fill::Solid(old)),
                    ..Default::default()
                },
                HoverBaseColor(old),
                PressBaseColor(old),
            ))
            .id();
        let blue = Rgba {
            r: 0.0,
            g: 0.0,
            b: 1.0,
            a: 1.0,
        };
        let attrs = Attributes {
            bg: Some(BgSpec::Solid(blue)),
            ..Default::default()
        };
        apply_reapplied_attrs(&mut world, e, &attrs);
        let want: Color = blue.into();
        assert_eq!(world.get::<HoverBaseColor>(e).unwrap().0, want);
        assert_eq!(world.get::<PressBaseColor>(e).unwrap().0, want);
    }

    /// An element with an opacity transition and no authored opacity fades
    /// from the computed `1` the first time a rule gives it one. With no
    /// `Opacity` component to start from, the change once snapped.
    #[test]
    fn opacity_transition_starts_from_one_when_none_was_authored() {
        use lumen_core::components::Opacity;
        use lumen_ir::layout_ir::{EasingIr, TransitionIr, TransitionPropertyIr};
        use lumen_primitives::OpacityTransition;
        let mut world = World::new();
        let e = world.spawn_empty().id();
        let attrs = Attributes {
            opacity: Some(0.2),
            transitions: vec![TransitionIr {
                property: TransitionPropertyIr::Opacity,
                duration_ms: 6000,
                easing: EasingIr::Linear,
            }],
            ..Default::default()
        };
        apply_reapplied_attrs(&mut world, e, &attrs);
        let tween = world
            .get::<OpacityTransition>(e)
            .expect("the change tweens");
        assert_eq!((tween.0.from, tween.0.to), (1.0, 0.2));
        assert_eq!(world.get::<Opacity>(e), Some(&Opacity(1.0)));
    }

    /// A restyle that resolves nothing new leaves `Style` untouched, so it
    /// does not mark the element's layout dirty.
    #[test]
    fn restyle_without_layout_changes_leaves_style_unchanged() {
        use lumen_core::components::Style;
        let mut world = World::new();
        let e = world.spawn(Style::default()).id();
        world.clear_trackers();
        let tick = world.change_tick();
        apply_reapplied_attrs(&mut world, e, &Attributes::default());
        let changed = world
            .entity(e)
            .get_change_ticks::<Style>()
            .unwrap()
            .is_changed(tick, world.change_tick());
        assert!(!changed, "an unchanged restyle must not touch Style");
    }
}
