//! What an element and the document declare about motion, kept where a
//! subsystem that runs it can read it, and the live stylesheet the cascade
//! resolves against.
//!
//! The cascade resolves `animation-*` declarations like any other property
//! and leaves them here, on the element, as syntax; nothing in the scene runs
//! them. An optional subsystem that does reads them back, along with the
//! `@keyframes` blocks of [`RuntimeStylesheet`] and the [`LastMediaContext`]
//! those blocks are written inside.

use bevy_ecs::prelude::*;
use lumen_core::style_overrides::StyleFields;
use lumen_ir::animation::AnimationDeclsIr;
use lumen_ir::css::{MediaContext, Stylesheet};
use lumen_ir::layout_ir::StateAnimationsIr;

/// The system that re-runs the cascade for a tick's style changes and
/// commits the result onto the elements. A system that reads what the
/// cascade resolved this tick (the animations an element declares) runs
/// after it.
#[derive(bevy_ecs::schedule::SystemSet, Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct Restyle;

/// The combined (UA, skin and app) stylesheet the cascade resolves against,
/// kept live: the restyle re-runs it on a theme or media flip without
/// reading a file, and a hot reload replaces it.
#[derive(Resource, Clone, Default)]
pub struct RuntimeStylesheet(pub Stylesheet);

/// The [`MediaContext`] the primary window's `@media` rules last resolved
/// against.
#[derive(Resource, Clone, Copy, Debug, PartialEq)]
pub struct LastMediaContext(pub MediaContext);

/// An element's `animation` and `animation-*` declarations, as the cascade
/// resolved them. Carried only by an element that declares one. Boxed, as
/// the attributes it comes from box it, so the style a spawn builds per
/// element stays small.
#[derive(Component, Clone, Debug, Default, PartialEq)]
pub struct AnimationDeclarations(pub Box<AnimationDeclsIr>);

/// The `animation-*` declarations an element's state rules set (`:hover {
/// animation: ... }`), each laid over [`AnimationDeclarations`] while the
/// element is in that state. Carried only by an element whose state rules
/// set one. Boxed for the same reason as [`AnimationDeclarations`].
#[derive(Component, Clone, Debug, Default, PartialEq)]
pub struct StateAnimations(pub Box<StateAnimationsIr>);

/// The style fields an element's `!important` declarations set. An
/// animation leaves them alone, as CSS ranks important declarations above
/// animations. Carried only by an element that has one.
#[derive(Component, Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct ImportantFields(pub StyleFields);

/// The held style field a property the cascade files under `name` lives
/// in, for a property a holder can hold.
pub fn style_field_of(name: &str) -> Option<StyleFields> {
    Some(match name {
        "bg" => StyleFields::FILL,
        "opacity" => StyleFields::OPACITY,
        "text-color" => StyleFields::TEXT_COLOR,
        "border"
        | "border-color"
        | "border-top-color"
        | "border-right-color"
        | "border-bottom-color"
        | "border-left-color" => StyleFields::BORDER,
        "shadow" | "box-shadow" => StyleFields::SHADOWS,
        "radius"
        | "border-top-left-radius"
        | "border-top-right-radius"
        | "border-bottom-right-radius"
        | "border-bottom-left-radius" => StyleFields::RADIUS,
        "transform" | "translate" | "rotate" | "scale" | "transform-origin" => {
            StyleFields::TRANSFORM
        }
        _ => return None,
    })
}

/// The class an element mounted by `<if>` or `<for>` gains when it is about
/// to leave the tree. A stylesheet that styles it (`.toast.leaving {
/// animation: slide-out 200ms }`) gets the element held in place, showing
/// that style, until the motion it starts has run; one that never names the
/// class gets elements removed at once, as before.
pub const LEAVING_CLASS: &str = "leaving";

/// What the app's stylesheet asks of elements entering and leaving the
/// tree, read off it whenever it changes.
#[derive(Resource, Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct EntryExitStyles {
    /// A rule sits inside `@starting-style`: an element entering the tree
    /// is resolved against it too, and transitions from what it says.
    pub starting_style: bool,
    /// A selector names [`LEAVING_CLASS`].
    pub leaving: bool,
}

/// Marks an element that has just entered the tree and has not been
/// resolved yet: the restyle that resolves it also resolves its
/// `@starting-style`, which its transitions start from.
#[derive(Component, Clone, Copy, Debug, Default)]
pub struct Entering;

/// Marks an element on its way out of the tree: it carries
/// [`LEAVING_CLASS`] and is removed once nothing holds it.
#[derive(Component, Clone, Copy, Debug)]
pub struct Leaving {
    /// The tick it started leaving on. It is removed no sooner than the
    /// tick after, so whatever its new class starts has had a tick to say it
    /// holds it.
    pub since: u64,
    /// When it started leaving: a motion that started before is not what
    /// the class started, and holds nothing.
    pub at: lumen_core::time::Instant,
}

/// A motion on a [`Leaving`] element that has to run before it goes: an
/// exit animation. Whoever runs the motion inserts it and takes it off when
/// the motion ends.
#[derive(Component, Clone, Copy, Debug, Default)]
pub struct HoldRemoval;

/// Read [`EntryExitStyles`] off the stylesheet when it changes.
pub fn track_entry_exit_styles(
    sheet: Option<Res<crate::spawn::LumenStylesheet>>,
    mut styles: ResMut<EntryExitStyles>,
) {
    let Some(sheet) = sheet else {
        return;
    };
    if !sheet.is_changed() && !styles.is_added() {
        return;
    }
    let next = EntryExitStyles {
        starting_style: sheet.0.has_starting_style(),
        leaving: sheet.0.class_invalidation_set().contains(LEAVING_CLASS),
    };
    if *styles != next {
        *styles = next;
    }
}

/// Mark `root` and everything under it as entering, when the stylesheet
/// has an `@starting-style` to resolve them against.
pub fn mark_entering(world: &mut World, root: Entity) {
    // A page's browser applies `@starting-style` to what enters it.
    if cfg!(target_arch = "wasm32")
        || !world
            .get_resource::<EntryExitStyles>()
            .is_some_and(|s| s.starting_style)
    {
        return;
    }
    let mut stack = vec![root];
    while let Some(e) = stack.pop() {
        let Ok(mut ent) = world.get_entity_mut(e) else {
            continue;
        };
        if ent.contains::<lumen_core::components::LumenTag>() {
            ent.insert(Entering);
        }
        if let Some(kids) = ent.get::<bevy_ecs::hierarchy::Children>() {
            stack.extend(kids.iter());
        }
    }
}

/// [`begin_leaving`] as a command, for a system that takes elements out.
pub struct BeginLeaving(pub Entity);

impl bevy_ecs::system::Command for BeginLeaving {
    type Out = ();
    fn apply(self, world: &mut World) {
        begin_leaving(world, self.0);
    }
}

/// Take `entity` out of the tree: at once, or, when the stylesheet styles
/// [`LEAVING_CLASS`], by giving it the class and letting
/// [`sweep_leaving`] remove it once nothing holds it. Returns whether it
/// stays for now.
pub fn begin_leaving(world: &mut World, entity: Entity) -> bool {
    let styled = world
        .get_resource::<EntryExitStyles>()
        .is_some_and(|s| s.leaving);
    let Ok(mut ent) = world.get_entity_mut(entity) else {
        return false;
    };
    if !styled || !ent.contains::<lumen_core::components::LumenTag>() {
        ent.despawn();
        return false;
    }
    if ent.contains::<Leaving>() {
        return true;
    }
    let (since, at) = ent
        .world()
        .get_resource::<lumen_core::tick::Tick>()
        .map_or((0, lumen_core::time::Instant::now()), |t| (t.frame, t.now));
    let mut classes = ent
        .take::<lumen_core::components::LumenClasses>()
        .unwrap_or_default();
    if !classes.has(LEAVING_CLASS) {
        classes.0.push(LEAVING_CLASS.into());
    }
    ent.insert((
        classes,
        Leaving { since, at },
        lumen_core::components::StyleDirty,
    ));
    true
}

/// Remove every leaving element nothing holds any more.
pub fn sweep_leaving(
    mut commands: Commands,
    tick: Res<lumen_core::tick::Tick>,
    leaving: Query<(Entity, &Leaving), Without<HoldRemoval>>,
) {
    for (entity, l) in &leaving {
        if l.since < tick.frame {
            commands.entity(entity).try_despawn();
        }
    }
}

/// Install what keeps [`EntryExitStyles`] current and removes leaving
/// elements: every host that runs the reconcilers adds it.
pub fn install_entry_exit(app: &mut lumen_core::app::App) {
    app.world.init_resource::<EntryExitStyles>();
    app.add_systems(
        lumen_core::tick::TickStage::Systems,
        (
            track_entry_exit_styles.before(crate::spawn::reconcile_if_blocks),
            sweep_leaving.before(crate::spawn::reconcile_if_blocks),
        ),
    );
}
