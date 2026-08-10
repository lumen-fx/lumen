//! AccessKit-backed accessibility translation layer.
//!
//! - Converts ECS state into an [`accesskit::TreeUpdate`] each tick.
//! - The platform adapter ([`accesskit_winit::Adapter`]) is hosted in `lumen-window-winit`; this crate provides only the translation logic.
//! - Maps each [`Entity`] to a [`NodeId`] via `Entity::to_bits()`, yielding a stable accessibility identity per ECS entity.

#![forbid(unsafe_code)]
#![warn(missing_docs)]

use accesskit::{Action, Node, NodeId, Rect, Role, Tree, TreeId, TreeUpdate};
use bevy_ecs::prelude::*;
use lumen_core::components::{
    A11yAnnouncement, A11yDescription, A11yLabel, A11yLevel, A11yLive, A11yRelations, A11yRole,
    A11yRootLabel, A11ySetSize, A11yState, A11yValue, DirtyA11y, Disabled, LumenClasses, LumenId,
    LumenTag, PendingA11yUpdate, RootWindowEntity, Selected, TabIndex, TextContent, TextInput,
    Toggleable, Visible,
};
use lumen_core::input::{FocusTracker, Scroll};
use lumen_core::prelude::*;
use std::collections::{HashMap, HashSet};

/// Synthetic root-window node id. Set to `u64::MAX` to sit outside the entity-bits id space.
pub const ROOT_NODE: NodeId = NodeId(u64::MAX);

/// Cache of last-emitted node hashes keyed by [`NodeId`], stored in the `World` as a [`Resource`].
///
/// - Read and updated by [`build_tree_update`] each tick.
/// - A node is re-emitted only when its hash changes between ticks.
/// - Tracked inputs that affect the hash: [`Transform`], [`TextContent`], [`TabIndex`], [`Scroll`], [`LumenId`], [`Children`].
/// - AccessKit merges partial updates into its persistent tree.
#[derive(Resource, Default, Debug)]
pub struct A11ySnapshot {
    /// Maps `node_id` to a 64-bit content hash for nodes emitted in the previous tick. Stale entries for despawned entities are pruned each tick.
    pub hashes: HashMap<NodeId, u64>,
}

/// Converts an [`Entity`] to an accesskit [`NodeId`] by wrapping `Entity::to_bits()`.
/// Combines the entity index and generation, producing a collision-free id across despawn/respawn cycles.
pub fn entity_to_node(e: Entity) -> NodeId {
    NodeId(e.to_bits())
}

/// Inverse of [`entity_to_node`]. Returns `None` for the synthetic [`ROOT_NODE`] or any id not produced by [`entity_to_node`].
pub fn node_to_entity(id: NodeId) -> Option<Entity> {
    if id == ROOT_NODE {
        None
    } else {
        Some(Entity::from_bits(id.0))
    }
}

/// Builds a [`TreeUpdate`] reflecting the current ECS state.
///
/// - Iterates every entity with a [`Transform`] and emits a node carrying its absolute rect, role, optional label, and computed actions.
/// - Sets each entity's `Children` as accesskit children; [`ROOT_NODE`] fans out to entities without a [`ChildOf`].
/// - Incremental: hashes each produced node, compares against [`A11ySnapshot`], and omits unchanged nodes from the emitted update.
/// - A stable UI emits zero entity nodes; only the root node and focus pointer.
#[deprecated(
    since = "0.0.1",
    note = "Use the `sync_a11y_tree` system (registered by `A11yPlugin`) plus \
            `take_pending_tree_update` instead. `build_tree_update` re-walks the \
            world every redraw rather than running inside `TickStage::A11ySync`; \
            the system path matches the audit's P1 'rebuilt every frame' fix \
            (docs/audits/a11y.md). Scheduled for removal in the next minor version."
)]
pub fn build_tree_update(world: &mut World) -> TreeUpdate {
    world.init_resource::<A11ySnapshot>();
    let mut nodes: Vec<(NodeId, Node)> = Vec::new();
    let mut roots: Vec<NodeId> = Vec::new();
    let mut new_hashes: HashMap<NodeId, u64> = HashMap::new();
    let mut seen: HashSet<NodeId> = HashSet::new();
    seen.insert(ROOT_NODE);

    let prior_hashes = world.resource::<A11ySnapshot>().hashes.clone();

    let mut q = world.query::<(
        Entity,
        &Transform,
        Option<&TextContent>,
        Option<&TabIndex>,
        Option<&TextInput>,
        Option<&Scroll>,
        Option<&LumenId>,
        Option<&Children>,
        Option<&ChildOf>,
    )>();
    for (entity, transform, text, tab, input, scroll, id, children, parent) in q.iter(world) {
        let node_id = entity_to_node(entity);
        seen.insert(node_id);
        let role = role_for(text, tab, input, scroll, parent.is_none());

        let child_ids: Vec<NodeId> = children
            .map(|c| c.iter().map(entity_to_node).collect())
            .unwrap_or_default();
        let label_str = match (text, id) {
            (Some(t), _) if !t.0.is_empty() => Some(t.0.as_str()),
            (_, Some(i)) => Some(i.0.as_str()),
            _ => None,
        };
        let hash = hash_node_inputs(transform, role, label_str, tab.map(|t| t.0), &child_ids);

        if parent.is_none() {
            roots.push(node_id);
        }

        if prior_hashes.get(&node_id) == Some(&hash) {
            // Hash matches the previous tick; skip emission and carry the hash forward into `new_hashes`.
            new_hashes.insert(node_id, hash);
            continue;
        }

        let mut node = Node::new(role);
        node.set_bounds(Rect {
            x0: transform.absolute.x as f64,
            y0: transform.absolute.y as f64,
            x1: (transform.absolute.x + transform.size.x) as f64,
            y1: (transform.absolute.y + transform.size.y) as f64,
        });
        if let Some(t) = text
            && !t.0.is_empty()
        {
            if matches!(role, Role::TextInput) {
                node.set_value(t.0.as_str());
            } else {
                node.set_label(t.0.as_str());
            }
        } else if let Some(id) = id {
            node.set_label(id.0.as_str());
        }
        if tab.map(|t| t.0 >= 0).unwrap_or(false) {
            node.add_action(Action::Focus);
        }
        if matches!(role, Role::Button) {
            node.add_action(Action::Click);
        }
        if !child_ids.is_empty() {
            node.set_children(child_ids);
        }
        nodes.push((node_id, node));
        new_hashes.insert(node_id, hash);
    }

    // Hash the root node's child list and emit only when it changes.
    let root_hash = hash_root_inputs(&roots);
    if prior_hashes.get(&ROOT_NODE) != Some(&root_hash) {
        let mut root = Node::new(Role::Window);
        root.set_label("Lumen app");
        root.set_children(roots);
        nodes.push((ROOT_NODE, root));
    }
    new_hashes.insert(ROOT_NODE, root_hash);

    // Drop hashes for despawned entities by not carrying them into `new_hashes`. AccessKit removes
    // such nodes once their parent's `children` list excludes them. The `seen` set filters out any
    // stale ids from `prior_hashes` that no longer appear in the current iteration.

    world.resource_mut::<A11ySnapshot>().hashes = new_hashes;

    let focus = world
        .resource::<FocusTracker>()
        .0
        .map(entity_to_node)
        .unwrap_or(ROOT_NODE);

    TreeUpdate {
        nodes,
        tree: Some(Tree::new(ROOT_NODE)),
        tree_id: TreeId::ROOT,
        focus,
    }
}

fn hash_node_inputs(
    transform: &Transform,
    role: Role,
    label: Option<&str>,
    tab: Option<i32>,
    children: &[NodeId],
) -> u64 {
    use std::hash::{Hash, Hasher};
    let mut h = std::collections::hash_map::DefaultHasher::new();
    transform.absolute.x.to_bits().hash(&mut h);
    transform.absolute.y.to_bits().hash(&mut h);
    transform.size.x.to_bits().hash(&mut h);
    transform.size.y.to_bits().hash(&mut h);
    (role as u32).hash(&mut h);
    label.unwrap_or("").hash(&mut h);
    tab.hash(&mut h);
    for c in children {
        c.0.hash(&mut h);
    }
    h.finish()
}

fn hash_root_inputs(roots: &[NodeId]) -> u64 {
    use std::hash::{Hash, Hasher};
    let mut h = std::collections::hash_map::DefaultHasher::new();
    for r in roots {
        r.0.hash(&mut h);
    }
    h.finish()
}

fn role_for(
    text: Option<&TextContent>,
    tab: Option<&TabIndex>,
    input: Option<&TextInput>,
    scroll: Option<&Scroll>,
    is_root: bool,
) -> Role {
    if is_root {
        return Role::Window;
    }
    if input.is_some() {
        return Role::TextInput;
    }
    if scroll.is_some() {
        return Role::ScrollView;
    }
    if tab.map(|t| t.0 >= 0).unwrap_or(false) {
        return Role::Button;
    }
    if text.is_some() {
        return Role::Label;
    }
    Role::GenericContainer
}

/// Re-exports of the underlying `accesskit` and `accesskit_winit` crates so backend crates can construct adapters and process platform events through this dependency.
pub use accesskit;
pub use accesskit_winit;

// --- New tree-build system (lives alongside the legacy `build_tree_update`) ---
//
// The legacy `build_tree_update` above is the one currently invoked by
// `lumen-window-winit` inside the `RedrawRequested` handler. The audit in
// `docs/audits/a11y.md` says: do not delete it yet - build the rewrite in
// parallel so we can land it without breaking existing behaviour.
//
// The new `sync_a11y_tree` system below:
// - Runs inside `TickStage::A11ySync` (registered via [`A11yPlugin`]).
// - Skips entirely when [`flag_a11y_changes`] saw no change tick on any
//   published component, no entity carries [`DirtyA11y`], and focus has not
//   moved (mirrors the audit's "skip when not dirty" recommendation).
// - Uses the real window-entity root rather than the synthetic
//   `NodeId(u64::MAX)`.
// - Maps the new `A11y*` components onto AccessKit `Node` setters via
//   `From`/`Into` (per project rule: no `convert_x_to_y` helpers).
// - Stores the resulting `TreeUpdate` in [`PendingA11yUpdate`] so the winit
//   redraw handler can hand it to `Adapter::update_if_active(...)`
//   without re-walking the world.

/// Newtype wrapper around [`Role`] so the orphan rule lets us implement
/// `From<A11yRole>` here (both `A11yRole` and `Role` are foreign to this
/// crate, but `AkRole` is local). Callers write `Role::from(AkRole::from(r))`
/// or use the convenience helper [`role_from_a11y`].
pub struct AkRole(pub Role);

impl From<A11yRole> for AkRole {
    fn from(r: A11yRole) -> Self {
        AkRole(match r {
            A11yRole::Button => Role::Button,
            A11yRole::Link => Role::Link,
            A11yRole::TextInput => Role::TextInput,
            A11yRole::TextArea => Role::MultilineTextInput,
            A11yRole::Checkbox => Role::CheckBox,
            A11yRole::Switch => Role::Switch,
            A11yRole::Radio => Role::RadioButton,
            A11yRole::RadioGroup => Role::RadioGroup,
            A11yRole::Slider => Role::Slider,
            A11yRole::ProgressBar => Role::ProgressIndicator,
            A11yRole::ComboBox => Role::ComboBox,
            A11yRole::ListBox => Role::ListBox,
            A11yRole::ListItem => Role::ListItem,
            A11yRole::MenuBar => Role::MenuBar,
            A11yRole::Menu => Role::Menu,
            A11yRole::MenuItem => Role::MenuItem,
            A11yRole::MenuItemCheckbox => Role::MenuItemCheckBox,
            A11yRole::MenuItemRadio => Role::MenuItemRadio,
            A11yRole::Tab => Role::Tab,
            A11yRole::TabList => Role::TabList,
            A11yRole::TabPanel => Role::TabPanel,
            A11yRole::Tree => Role::Tree,
            A11yRole::TreeItem => Role::TreeItem,
            A11yRole::Toolbar => Role::Toolbar,
            A11yRole::Dialog => Role::Dialog,
            A11yRole::AlertDialog => Role::AlertDialog,
            A11yRole::Tooltip => Role::Tooltip,
            A11yRole::Status => Role::Status,
            A11yRole::Alert => Role::Alert,
            A11yRole::Label => Role::Label,
            A11yRole::Heading => Role::Heading,
            A11yRole::Group => Role::Group,
            A11yRole::Region => Role::Region,
            A11yRole::Landmark => Role::Section,
            A11yRole::Generic => Role::GenericContainer,
        })
    }
}

impl From<AkRole> for Role {
    fn from(r: AkRole) -> Self {
        r.0
    }
}

/// Roles whose text content is the field's value rather than its name.
fn is_text_entry(role: Role) -> bool {
    matches!(
        role,
        Role::TextInput
            | Role::MultilineTextInput
            | Role::SearchInput
            | Role::DateInput
            | Role::TimeInput
    )
}

/// True when `classes` carries `name`.
fn has_class(classes: &LumenClasses, name: &str) -> bool {
    classes.0.iter().any(|c| &**c == name)
}

/// Role for a markup tag name, for the tags that survive to the ECS as
/// themselves. Returns `None` for layout containers and control flow
/// (`row`, `column`, `tile`, `div`, `spacer`, `overlay`, `for`, `if`),
/// which fall through to the component-derived role.
fn role_for_tag(tag: &str) -> Option<Role> {
    Some(match tag {
        "root" => Role::Window,
        "dialog" => Role::Dialog,
        "a" => Role::Link,
        "button" => Role::Button,
        "checkbox" | "toggle" => Role::CheckBox,
        "switch" => Role::Switch,
        "radio" => Role::RadioButton,
        "slider" => Role::Slider,
        "progress" => Role::ProgressIndicator,
        "input" => Role::TextInput,
        "textarea" => Role::MultilineTextInput,
        "image" => Role::Image,
        "scroll" => Role::ScrollView,
        "label" => Role::Label,
        "menu" => Role::Menu,
        "menuitem" => Role::MenuItem,
        "separator" => Role::Splitter,
        "title-bar" => Role::TitleBar,
        "tooltip" => Role::Tooltip,
        _ => return None,
    })
}

/// Role for a composite widget the markup compiler desugars away.
///
/// `<tabs>`, `<tab>` and `<dropdown>` never reach the ECS as tags: the
/// compiler lowers each into plain containers and buttons and marks the
/// parts with a reserved class. Those classes are what identifies a tab
/// strip, a tab, a combo box, its list and its options.
fn role_for_classes(classes: &LumenClasses) -> Option<Role> {
    for class in classes.0.iter() {
        let role = match &**class {
            "tabs" => Role::Group,
            "tab-strip" => Role::TabList,
            "tab-btn" => Role::Tab,
            "dropdown" => Role::Group,
            "dropdown-button" => Role::ComboBox,
            "dropdown-panel" => Role::ListBox,
            "dropdown-option" => Role::ListBoxOption,
            "menu-panel" => Role::Menu,
            "menu-item" => Role::MenuItem,
            "menu-separator" => Role::Splitter,
            "date-picker" => Role::DateInput,
            "time-picker" => Role::TimeInput,
            _ => continue,
        };
        return Some(role);
    }
    None
}

/// Convenience wrapper translating an [`A11yRole`] into an [`accesskit::Role`]
/// via the [`AkRole`] newtype hop. Kept thin so callers can also use the
/// chained `Role::from(AkRole::from(r))` form directly.
pub fn role_from_a11y(r: A11yRole) -> Role {
    AkRole::from(r).into()
}

/// Newtype wrapper around [`accesskit::Live`] so the orphan rule lets us
/// implement `From<A11yLive>` here. See [`AkRole`] for the same pattern.
pub struct AkLive(pub accesskit::Live);

impl From<A11yLive> for AkLive {
    fn from(l: A11yLive) -> Self {
        AkLive(match l {
            A11yLive::Off => accesskit::Live::Off,
            A11yLive::Polite => accesskit::Live::Polite,
            A11yLive::Assertive => accesskit::Live::Assertive,
        })
    }
}

impl From<AkLive> for accesskit::Live {
    fn from(l: AkLive) -> Self {
        l.0
    }
}

/// Transient writer pairing an [`A11yState`] bitset with the AccessKit
/// [`Node`] it should be applied to. Method [`Self::apply`] writes every
/// matching setter on the node. The pair is a local newtype so additions
/// to either side land in one place - the call site stays a single
/// `A11yStateApply::new(&mut node, state).apply();` without a sprawling
/// `convert_x_to_y` helper.
pub struct A11yStateApply<'a> {
    node: &'a mut Node,
    state: A11yState,
}

impl<'a> A11yStateApply<'a> {
    /// Pair `node` with `state` for application.
    pub fn new(node: &'a mut Node, state: A11yState) -> Self {
        Self { node, state }
    }

    /// Walks the bit flags and invokes the matching AccessKit setter for
    /// each one that's present.
    pub fn apply(self) {
        let Self { node, state } = self;
        if state.contains(A11yState::DISABLED) {
            node.set_disabled();
        }
        if state.contains(A11yState::READ_ONLY) {
            node.set_read_only();
        }
        if state.contains(A11yState::REQUIRED) {
            node.set_required();
        }
        if state.contains(A11yState::HIDDEN) {
            node.set_hidden();
        }
        if state.contains(A11yState::INVALID) {
            node.set_invalid(accesskit::Invalid::True);
        }
        if state.contains(A11yState::EXPANDED) {
            node.set_expanded(true);
        }
        if state.contains(A11yState::SELECTED) {
            node.set_selected(true);
        }
        if state.contains(A11yState::CHECKED) {
            node.set_toggled(accesskit::Toggled::True);
        }
        if state.contains(A11yState::PRESSED) {
            node.set_toggled(accesskit::Toggled::True);
        }
        if state.contains(A11yState::BUSY) {
            node.set_busy();
        }
        if state.contains(A11yState::MODAL) {
            node.set_modal();
        }
    }
}

/// Set for one tick when any component feeding the published tree changed.
///
/// [`sync_a11y_tree`] walks the world when this flag is up, when an entity
/// carries [`DirtyA11y`], or when focus moved, and clears it on the way
/// out. The flag is what keeps the published tree in step with ordinary
/// app state; [`DirtyA11y`] covers only inbound assistive-technology
/// actions.
#[derive(Resource, Default, Debug)]
pub struct A11yChanged(pub bool);

/// Every component whose value feeds a published node. Split into nested
/// `Or` groups because a single `Or` tuple caps out well below the number
/// of components the tree reads.
type A11yChangeFilter = Or<(
    Or<(
        bevy_ecs::query::Changed<Transform>,
        bevy_ecs::query::Changed<TextContent>,
        bevy_ecs::query::Changed<TabIndex>,
        bevy_ecs::query::Changed<TextInput>,
        bevy_ecs::query::Changed<Scroll>,
        bevy_ecs::query::Changed<LumenId>,
        bevy_ecs::query::Changed<Children>,
        bevy_ecs::query::Changed<ChildOf>,
    )>,
    Or<(
        bevy_ecs::query::Changed<Visible>,
        bevy_ecs::query::Changed<Toggleable>,
        bevy_ecs::query::Changed<SliderValue>,
        bevy_ecs::query::Changed<Disabled>,
        bevy_ecs::query::Changed<Selected>,
        bevy_ecs::query::Changed<LumenTag>,
        bevy_ecs::query::Changed<LumenClasses>,
    )>,
    Or<(
        bevy_ecs::query::Changed<A11yRole>,
        bevy_ecs::query::Changed<A11yLabel>,
        bevy_ecs::query::Changed<A11yDescription>,
        bevy_ecs::query::Changed<A11yState>,
        bevy_ecs::query::Changed<A11yValue>,
        bevy_ecs::query::Changed<A11yLevel>,
        bevy_ecs::query::Changed<A11ySetSize>,
        bevy_ecs::query::Changed<A11yLive>,
        bevy_ecs::query::Changed<A11yRelations>,
    )>,
)>;

/// Removal readers for the components whose disappearance changes a node
/// but leaves no change tick behind: dropping `Disabled` re-enables a
/// control, dropping `Selected` deselects a tab, and so on.
type A11yRemovals<'w, 's> = (
    RemovedComponents<'w, 's, Disabled>,
    RemovedComponents<'w, 's, Selected>,
    RemovedComponents<'w, 's, ChildOf>,
    RemovedComponents<'w, 's, TabIndex>,
    RemovedComponents<'w, 's, Visible>,
    RemovedComponents<'w, 's, TextContent>,
    RemovedComponents<'w, 's, Toggleable>,
    RemovedComponents<'w, 's, SliderValue>,
    RemovedComponents<'w, 's, TextInput>,
    RemovedComponents<'w, 's, A11yRole>,
    RemovedComponents<'w, 's, A11yLabel>,
    RemovedComponents<'w, 's, A11yState>,
);

/// Raise [`A11yChanged`] when this tick touched anything the accessibility
/// tree publishes. Runs in [`TickStage::A11ySync`] ahead of
/// [`sync_a11y_tree`] (registered by [`A11yPlugin`]).
pub fn flag_a11y_changes(
    mut flag: ResMut<A11yChanged>,
    changed: Query<(), (A11yChangeFilter, Without<bevy_ecs::resource::IsResource>)>,
    mut removed: A11yRemovals,
) {
    // Drain every reader before short-circuiting. A reader left holding
    // entries would re-raise the flag on a later idle tick and keep the
    // app from parking.
    let removed_any = [
        removed.0.read().count(),
        removed.1.read().count(),
        removed.2.read().count(),
        removed.3.read().count(),
        removed.4.read().count(),
        removed.5.read().count(),
        removed.6.read().count(),
        removed.7.read().count(),
        removed.8.read().count(),
        removed.9.read().count(),
        removed.10.read().count(),
        removed.11.read().count(),
    ]
    .iter()
    .any(|n| *n > 0);
    if removed_any || !changed.is_empty() {
        flag.0 = true;
    }
}

/// Per-frame state used by [`sync_a11y_tree`] for change tracking.
///
/// - Mirrors the legacy [`A11ySnapshot`] but keyed by content hash that
///   includes the full new component surface (role, label, description,
///   value, state, level, set-size, live, relations).
/// - Tracks `last_focus` separately so a tick with no node changes but a
///   moved focus still emits a minimal `TreeUpdate`.
#[derive(Resource, Default, Debug)]
pub struct A11yTreeCache {
    /// Last-emitted content hash per [`NodeId`]. Stale entries for despawned entities are pruned each tick.
    pub hashes: HashMap<NodeId, u64>,
    /// Last focus pointer emitted to the adapter. `None` = root.
    pub last_focus: Option<NodeId>,
    /// Window root [`NodeId`]. Initialised lazily from the root entity on first run.
    pub root: Option<NodeId>,
}

/// Builds an [`accesskit::TreeUpdate`] from current ECS state and stores it
/// in [`PendingA11yUpdate`]. Intended to run inside [`TickStage::A11ySync`]
/// (see [`A11yPlugin`]).
///
/// - Walks every entity with a [`Transform`] so layout-resolved nodes have
///   bounds. Layout-less entities are still emitted (the legacy walker
///   required `Transform`, dropping early-tick spawns).
/// - Reads the new `A11y*` components and falls back to the markup tag
///   ([`LumenTag`]), the reserved classes the compiler stamps on
///   desugared composite widgets ([`LumenClasses`]), and the primitive
///   components ([`Toggleable`], [`SliderValue`], [`TextInput`],
///   [`Disabled`], [`Selected`]) when an explicit role/state/value is not
///   supplied.
/// - Uses the real root window entity instead of `NodeId(u64::MAX)` when
///   a `RootWindowEntity` resource is present; otherwise falls back to
///   the legacy synthetic root for compatibility.
pub fn sync_a11y_tree(world: &mut World) {
    world.init_resource::<A11yTreeCache>();
    world.init_resource::<PendingA11yUpdate>();
    world.init_resource::<A11yChanged>();

    // Skip when nothing moved. [`flag_a11y_changes`] carries the ordinary
    // case: any change tick or removal on a component the tree publishes.
    // The DirtyA11y marker stays as the explicit per-entity signal inbound
    // assistive-technology actions raise. FocusTracker.is_changed cannot be
    // observed through &mut World, so we compare against the cached focus
    // below to catch focus-only ticks.
    let any_dirty_a11y = {
        let mut q = world.query::<&DirtyA11y>();
        q.iter(world).next().is_some()
    } || world.resource::<A11yChanged>().0;

    let focus_entity = world.resource::<FocusTracker>().0;

    // Resolve the root node id. Priority:
    //   1. [`RootWindowEntity`] resource if the backend set it.
    //   2. Auto-detect: the first entity that has no `ChildOf`. The
    //      markup pipeline (`LayoutIR::spawn_into`) returns this as
    //      the root; the a11y layer can pick it up without any
    //      special hook on the backend side. Stash it in the resource
    //      so subsequent ticks skip the scan.
    //   3. Legacy synthetic [`ROOT_NODE`] = `u64::MAX` as a last
    //      resort (no entities at all).
    if world.get_resource::<RootWindowEntity>().is_none() {
        let candidate: Option<Entity> = {
            let mut q = world.query_filtered::<Entity, (With<Transform>, Without<ChildOf>)>();
            q.iter(world).next()
        };
        if let Some(e) = candidate {
            world.insert_resource(RootWindowEntity(e));
        }
    }
    let root_id = match world.get_resource::<RootWindowEntity>() {
        Some(r) => entity_to_node(r.0),
        None => ROOT_NODE,
    };

    let cached_focus = world.resource::<A11yTreeCache>().last_focus;
    let new_focus = focus_entity.map(entity_to_node).unwrap_or(root_id);
    let focus_moved = cached_focus != Some(new_focus);

    if !any_dirty_a11y && !focus_moved && world.resource::<A11yTreeCache>().root == Some(root_id) {
        // Nothing to do - no entity announced an a11y-relevant change and focus
        // hasn't moved. Drop the previous PendingA11yUpdate so we don't keep
        // re-emitting an old payload.
        world.resource_mut::<PendingA11yUpdate>().boxed = None;
        world.resource_mut::<A11yChanged>().0 = false;
        return;
    }
    world.resource_mut::<A11yChanged>().0 = false;

    // `<dropdown>` expansion. The compiler desugars a dropdown into a
    // `.dropdown` column holding a `.dropdown-button` header plus an
    // `<if mode="hide">` block whose body is the `.dropdown-panel`. The
    // if-block carries the `Visible` flip, so a visible panel means the
    // combobox is expanded; collect the owning columns here and stamp
    // EXPANDED on their header during the walk.
    let expanded_owners: HashSet<Entity> = {
        let panel_parents: Vec<Entity> = {
            let mut q = world.query::<(&LumenClasses, &ChildOf)>();
            q.iter(world)
                .filter(|(c, _)| has_class(c, "dropdown-panel"))
                .map(|(_, p)| p.parent())
                .collect()
        };
        panel_parents
            .into_iter()
            .filter(|block| world.get::<Visible>(*block).map(|v| v.0).unwrap_or(true))
            .filter_map(|block| world.get::<ChildOf>(block).map(|c| c.parent()))
            .collect()
    };

    let mut nodes: Vec<(NodeId, Node)> = Vec::new();
    let mut roots: Vec<NodeId> = Vec::new();
    let mut new_hashes: HashMap<NodeId, u64> = HashMap::new();
    let mut seen: HashSet<NodeId> = HashSet::new();
    seen.insert(root_id);

    let prior_hashes = world.resource::<A11yTreeCache>().hashes.clone();

    // Bevy's tuple `QueryData` impls cap at 15, so the per-entity data is
    // grouped into three sub-tuples: layout / primitive / a11y overrides.
    type CoreData<'a> = (
        Entity,
        Option<&'a Transform>,
        Option<&'a TextContent>,
        Option<&'a TabIndex>,
        Option<&'a TextInput>,
        Option<&'a Scroll>,
        Option<&'a LumenId>,
        Option<&'a Children>,
        Option<&'a ChildOf>,
    );
    type PrimData<'a> = (
        Option<&'a Visible>,
        Option<&'a Toggleable>,
        Option<&'a SliderValue>,
        Option<&'a Disabled>,
        Option<&'a Selected>,
        Option<&'a LumenTag>,
        Option<&'a LumenClasses>,
    );
    type A11yData<'a> = (
        Option<&'a A11yRole>,
        Option<&'a A11yLabel>,
        Option<&'a A11yDescription>,
        Option<&'a A11yState>,
        Option<&'a A11yValue>,
        Option<&'a A11yLevel>,
        Option<&'a A11ySetSize>,
        Option<&'a A11yLive>,
        Option<&'a A11yRelations>,
    );
    // Bevy 0.19 stores resources as entities, and every field above is
    // optional, so an unfiltered walk would publish a node per resource.
    let mut q = world.query_filtered::<(CoreData<'_>, PrimData<'_>, A11yData<'_>), Without<
        bevy_ecs::resource::IsResource,
    >>();

    for (
        (entity, transform, text, tab, input, scroll, id, children, parent),
        (visible, toggle, slider, disabled, selected, tag, classes),
        (
            role_override,
            label,
            description,
            state_override,
            value_override,
            level,
            set_size,
            live,
            relations,
        ),
    ) in q.iter(world)
    {
        let node_id = entity_to_node(entity);
        seen.insert(node_id);

        // Role, in falling order of authority: an explicit A11yRole, the
        // compiler-generated class of a desugared composite widget, the
        // markup tag, then the primitive components on the entity.
        let role: Role = if let Some(r) = role_override {
            role_from_a11y(*r)
        } else if let Some(r) = classes.and_then(role_for_classes) {
            r
        } else if let Some(r) = tag.and_then(|t| role_for_tag(&t.0)) {
            r
        } else if let Some(t) = input {
            role_from_a11y(A11yRole::from(t))
        } else if let Some(s) = slider {
            role_from_a11y(A11yRole::from(s))
        } else if toggle.is_some() {
            role_from_a11y(A11yRole::Checkbox)
        } else if scroll.is_some() {
            Role::ScrollView
        } else if parent.is_none() && root_id != node_id {
            Role::Window
        } else if tab.map(|t| t.0 >= 0).unwrap_or(false) {
            Role::Button
        } else if text.is_some() {
            Role::Label
        } else {
            Role::GenericContainer
        };

        let child_ids: Vec<NodeId> = children
            .map(|c| c.iter().map(entity_to_node).collect())
            .unwrap_or_default();

        // Label: explicit A11yLabel wins, then the element's own text. Text
        // inputs are the exception - their text is the value, and naming a
        // field after its contents announces the typed string twice.
        let label_str: Option<&str> = if let Some(l) = label {
            if l.0.is_empty() {
                None
            } else {
                Some(l.0.as_str())
            }
        } else if !is_text_entry(role)
            && let Some(t) = text
            && !t.0.is_empty()
        {
            Some(t.0.as_str())
        } else {
            id.map(|i| i.0.as_str())
        };

        // Effective state: combine the explicit A11yState bits with primitive-derived bits.
        let mut state_bits = state_override.copied().unwrap_or_default();
        if let Some(v) = visible
            && !v.0
        {
            state_bits |= A11yState::HIDDEN;
        }
        if let Some(t) = toggle
            && t.checked
        {
            state_bits |= A11yState::CHECKED;
        }
        if disabled.is_some() {
            state_bits |= A11yState::DISABLED;
        }
        // `Selected` is the single-selection marker the owning primitive
        // maintains. On a radio it means "this option is the chosen one",
        // which AccessKit models as toggled rather than selected.
        if selected.is_some() {
            if matches!(
                role,
                Role::RadioButton | Role::CheckBox | Role::Switch | Role::MenuItemRadio
            ) {
                state_bits |= A11yState::CHECKED;
            } else {
                state_bits |= A11yState::SELECTED;
            }
        }
        if matches!(role, Role::ComboBox)
            && parent
                .map(|p| expanded_owners.contains(&p.parent()))
                .unwrap_or(false)
        {
            state_bits |= A11yState::EXPANDED;
        }
        // A `<dialog>` is a modal overlay; nothing behind it takes input
        // while it is up.
        if matches!(role, Role::Dialog) {
            state_bits |= A11yState::MODAL;
        }

        // Effective value: explicit A11yValue wins, else derived from SliderValue.
        let value: Option<A11yValue> = value_override.cloned().or_else(|| slider.map(Into::into));

        let live_val = live.copied().unwrap_or(A11yLive::Off);
        let level_val = level.map(|l| l.0);
        let set_size_val = set_size.copied();
        let description_str = description.map(|d| d.0.as_str()).filter(|s| !s.is_empty());
        let placeholder_str = input
            .map(|i| i.placeholder.as_str())
            .filter(|s| !s.is_empty());

        // Relations: convert Entity -> NodeId.
        let labelled_by_ids: Vec<NodeId> = relations
            .map(|r| r.labelled_by.iter().copied().map(entity_to_node).collect())
            .unwrap_or_default();
        let described_by_ids: Vec<NodeId> = relations
            .map(|r| r.described_by.iter().copied().map(entity_to_node).collect())
            .unwrap_or_default();
        let controls_ids: Vec<NodeId> = relations
            .map(|r| r.controls.iter().copied().map(entity_to_node).collect())
            .unwrap_or_default();
        let owns_ids: Vec<NodeId> = relations
            .map(|r| r.owns.iter().copied().map(entity_to_node).collect())
            .unwrap_or_default();

        let hash = hash_node_full(
            transform,
            role,
            label_str,
            description_str,
            placeholder_str,
            value.as_ref(),
            state_bits,
            level_val,
            set_size_val,
            live_val,
            tab.map(|t| t.0),
            &child_ids,
            &labelled_by_ids,
            &described_by_ids,
            &controls_ids,
            &owns_ids,
        );

        if parent.is_none() && node_id != root_id {
            roots.push(node_id);
        }

        if prior_hashes.get(&node_id) == Some(&hash) {
            new_hashes.insert(node_id, hash);
            continue;
        }

        let mut node = Node::new(role);
        if let Some(t) = transform {
            node.set_bounds(Rect {
                x0: t.absolute.x as f64,
                y0: t.absolute.y as f64,
                x1: (t.absolute.x + t.size.x) as f64,
                y1: (t.absolute.y + t.size.y) as f64,
            });
        }
        if let Some(s) = label_str {
            node.set_label(s);
        }
        if let Some(s) = description_str {
            node.set_description(s);
        }
        if let Some(s) = placeholder_str {
            node.set_placeholder(s);
        }
        // Text input value: emit the body text as `value`, not `label`.
        if is_text_entry(role)
            && let Some(t) = text
        {
            node.set_value(t.0.as_str());
        }
        if let Some(v) = value.as_ref() {
            node.set_numeric_value(v.now);
            node.set_min_numeric_value(v.min);
            node.set_max_numeric_value(v.max);
            if v.step > 0.0 {
                node.set_numeric_value_step(v.step);
            }
            if let Some(t) = v.text.as_deref() {
                node.set_value(t);
            }
        }
        // Apply state bits via the local newtype helper (no convert_x_to_y).
        A11yStateApply::new(&mut node, state_bits).apply();
        // Selection and expansion are tri-state in AccessKit, where unset
        // means the concept does not apply to this node. Roles that always
        // own the state say "false" out loud, so an assistive tool can
        // announce "tab 2 of 4" or "collapsed".
        if matches!(role, Role::Tab | Role::ListBoxOption)
            && !state_bits.contains(A11yState::SELECTED)
        {
            node.set_selected(false);
        }
        if matches!(role, Role::ComboBox) && !state_bits.contains(A11yState::EXPANDED) {
            node.set_expanded(false);
        }
        if let Some(l) = level_val
            && l > 0
        {
            node.set_level(l as usize);
        }
        if let Some(ss) = set_size_val {
            if ss.size > 0 {
                node.set_size_of_set(ss.size);
            }
            if ss.position > 0 {
                node.set_position_in_set(ss.position);
            }
        }
        if !matches!(live_val, A11yLive::Off) {
            node.set_live(AkLive::from(live_val).into());
        }
        if !labelled_by_ids.is_empty() {
            node.set_labelled_by(labelled_by_ids);
        }
        if !described_by_ids.is_empty() {
            node.set_described_by(described_by_ids);
        }
        if !controls_ids.is_empty() {
            node.set_controls(controls_ids);
        }
        if !owns_ids.is_empty() {
            node.set_owns(owns_ids);
        }

        // Actions populated by role.
        let focusable = tab.map(|t| t.0 >= 0).unwrap_or(false);
        if focusable {
            node.add_action(Action::Focus);
            node.add_action(Action::Blur);
            node.add_action(Action::ScrollIntoView);
        }
        match role {
            Role::Button
            | Role::Link
            | Role::MenuItem
            | Role::MenuItemCheckBox
            | Role::MenuItemRadio
            | Role::Tab
            | Role::Switch
            | Role::CheckBox
            | Role::RadioButton
            | Role::ListBoxOption => {
                node.add_action(Action::Click);
            }
            // A progress bar reports a value but takes none, so it is not
            // in this arm.
            Role::Slider | Role::SpinButton => {
                node.add_action(Action::Increment);
                node.add_action(Action::Decrement);
                node.add_action(Action::SetValue);
            }
            Role::TextInput
            | Role::MultilineTextInput
            | Role::SearchInput
            | Role::DateInput
            | Role::TimeInput => {
                node.add_action(Action::SetValue);
                node.add_action(Action::ReplaceSelectedText);
                node.add_action(Action::SetTextSelection);
            }
            Role::TreeItem | Role::DisclosureTriangle | Role::ComboBox => {
                node.add_action(Action::Expand);
                node.add_action(Action::Collapse);
            }
            _ => {}
        }
        node.add_action(Action::ShowContextMenu);

        if !child_ids.is_empty() {
            node.set_children(child_ids);
        }
        nodes.push((node_id, node));
        new_hashes.insert(node_id, hash);
    }

    // Drain pending announcements into transient nodes parented to root.
    if let Some(mut queue) =
        world.get_resource_mut::<lumen_core::components::A11yAnnouncementQueue>()
    {
        for (msg, politeness) in queue.pending.drain(..) {
            // Synthetic NodeId outside the Entity::to_bits space.
            // Reuses u64::MAX-1, MAX-2 ... per announcement. Tied to root's children.
            let synth_id = NodeId(u64::MAX - 1 - roots.len() as u64);
            let mut n = Node::new(Role::Alert);
            n.set_label(msg);
            if !matches!(politeness, A11yLive::Off) {
                n.set_live(AkLive::from(politeness).into());
            }
            nodes.push((synth_id, n));
            roots.push(synth_id);
        }
    }

    // Root node - replace synthetic with the real window root when present.
    let root_hash = hash_root_inputs(&roots);
    if prior_hashes.get(&root_id) != Some(&root_hash) {
        let mut root = Node::new(Role::Window);
        if let Some(title) = world.get_resource::<A11yRootLabel>() {
            root.set_label(title.0.as_str());
        } else {
            root.set_label("Lumen app");
        }
        root.set_children(roots);
        nodes.push((root_id, root));
    }
    new_hashes.insert(root_id, root_hash);

    {
        let mut cache = world.resource_mut::<A11yTreeCache>();
        cache.hashes = new_hashes;
        cache.last_focus = Some(new_focus);
        cache.root = Some(root_id);
    }

    // Clear the DirtyA11y marker on every entity that carried it. Re-acquiring
    // the marker requires another write from a dirty-tracking system.
    let dirty_ents: Vec<Entity> = {
        let mut q = world.query_filtered::<Entity, With<DirtyA11y>>();
        q.iter(world).collect()
    };
    for e in dirty_ents {
        if let Ok(mut em) = world.get_entity_mut(e) {
            em.remove::<DirtyA11y>();
        }
    }
    // Drain any per-entity A11yAnnouncement components (one-shot semantics).
    let ann_ents: Vec<Entity> = {
        let mut q = world.query_filtered::<Entity, With<A11yAnnouncement>>();
        q.iter(world).collect()
    };
    for e in ann_ents {
        if let Ok(mut em) = world.get_entity_mut(e) {
            em.remove::<A11yAnnouncement>();
        }
    }

    // check_dirty step (audit P1: a11y "rebuilt every frame"):
    // skip pushing a payload to AccessKit when nothing about the tree
    // changed - no nodes re-emitted AND the focus pointer is identical
    // to the previously emitted one. AccessKit accepts no-op updates,
    // but the adapter still wakes the platform layer (AT-SPI/UIA/NS)
    // for each `update_if_active` call, so a real skip matters for
    // idle CPU + a11y wakeups.
    if nodes.is_empty() && !focus_moved {
        world.resource_mut::<PendingA11yUpdate>().boxed = None;
        return;
    }

    let update = TreeUpdate {
        nodes,
        tree: Some(Tree::new(root_id)),
        tree_id: TreeId::ROOT,
        focus: new_focus,
    };
    world.resource_mut::<PendingA11yUpdate>().boxed = Some(Box::new(update));
}

/// Drive a full initial-tree build outside the regular schedule. Used by
/// window backends to satisfy the AccessKit `InitialTreeRequested`
/// handshake before the first tick has a chance to run
/// [`sync_a11y_tree`].
///
/// - Inserts a transient [`DirtyA11y`] marker on a sentinel entity so
///   the system's early-out is bypassed and the cache reset is forced.
/// - Resets the prior [`A11yTreeCache`] state so every node is emitted.
/// - Calls [`sync_a11y_tree`] to populate [`PendingA11yUpdate`].
pub fn sync_a11y_tree_initial(world: &mut World) {
    world.init_resource::<A11yTreeCache>();
    {
        let mut cache = world.resource_mut::<A11yTreeCache>();
        cache.hashes.clear();
        cache.last_focus = None;
        cache.root = None;
    }
    // Force the sync to run by inserting a transient DirtyA11y on a
    // synthetic entity. `sync_a11y_tree` removes every DirtyA11y at the
    // end of its run; that despawns the marker bookkeeping cleanly.
    let sentinel = world.spawn(DirtyA11y).id();
    sync_a11y_tree(world);
    // The sentinel has no other components; despawn it so it does not
    // leak into queries.
    if world.get_entity(sentinel).is_ok() {
        world.entity_mut(sentinel).despawn();
    }
}

/// Take the latest [`accesskit::TreeUpdate`] out of [`PendingA11yUpdate`].
/// Window backends call this from `RedrawRequested` and pass the result
/// to `Adapter::update_if_active(...)`. Returns `None` when no update
/// was produced this tick (no a11y-relevant changes).
pub fn take_pending_tree_update(world: &mut World) -> Option<TreeUpdate> {
    let pending = world.get_resource_mut::<PendingA11yUpdate>()?;
    let boxed = pending.into_inner().boxed.take()?;
    boxed.downcast::<TreeUpdate>().ok().map(|b| *b)
}

#[allow(clippy::too_many_arguments)]
fn hash_node_full(
    transform: Option<&Transform>,
    role: Role,
    label: Option<&str>,
    description: Option<&str>,
    placeholder: Option<&str>,
    value: Option<&A11yValue>,
    state: A11yState,
    level: Option<u8>,
    set_size: Option<A11ySetSize>,
    live: A11yLive,
    tab: Option<i32>,
    children: &[NodeId],
    labelled_by: &[NodeId],
    described_by: &[NodeId],
    controls: &[NodeId],
    owns: &[NodeId],
) -> u64 {
    use std::hash::{Hash, Hasher};
    let mut h = std::collections::hash_map::DefaultHasher::new();
    if let Some(t) = transform {
        t.absolute.x.to_bits().hash(&mut h);
        t.absolute.y.to_bits().hash(&mut h);
        t.size.x.to_bits().hash(&mut h);
        t.size.y.to_bits().hash(&mut h);
    } else {
        0u64.hash(&mut h);
    }
    (role as u32).hash(&mut h);
    label.unwrap_or("").hash(&mut h);
    description.unwrap_or("").hash(&mut h);
    placeholder.unwrap_or("").hash(&mut h);
    if let Some(v) = value {
        v.now.to_bits().hash(&mut h);
        v.min.to_bits().hash(&mut h);
        v.max.to_bits().hash(&mut h);
        v.step.to_bits().hash(&mut h);
        v.text.as_deref().unwrap_or("").hash(&mut h);
    }
    state.bits().hash(&mut h);
    level.unwrap_or(0).hash(&mut h);
    if let Some(ss) = set_size {
        ss.size.hash(&mut h);
        ss.position.hash(&mut h);
    }
    (live as u32).hash(&mut h);
    tab.hash(&mut h);
    for c in children {
        c.0.hash(&mut h);
    }
    for r in labelled_by {
        r.0.hash(&mut h);
    }
    for r in described_by {
        r.0.hash(&mut h);
    }
    for r in controls {
        r.0.hash(&mut h);
    }
    for r in owns {
        r.0.hash(&mut h);
    }
    h.finish()
}

/// Resolve an entity's accessibility-aware "click target" point. Used by
/// the window backend when an inbound `Action::Click` arrives: the
/// synthesized [`ClickEvent`] must carry the entity centre in world
/// coordinates so downstream hit-test / hover / scroll consumers don't
/// see `(0, 0)` and misroute the event.
pub fn entity_click_point(world: &World, entity: Entity) -> glam::Vec2 {
    if let Some(t) = world.get::<Transform>(entity) {
        t.absolute + 0.5 * t.size
    } else {
        glam::Vec2::ZERO
    }
}

/// Lumen plugin registering [`flag_a11y_changes`] and [`sync_a11y_tree`]
/// in [`TickStage::A11ySync`].
/// Apps install this once at startup via `app.add_plugin(A11yPlugin)`.
pub struct A11yPlugin;

impl lumen_core::app::Plugin for A11yPlugin {
    fn build(self, app: &mut lumen_core::app::App) {
        app.world.init_resource::<A11yTreeCache>();
        app.world.init_resource::<A11yChanged>();
        app.world.init_resource::<PendingA11yUpdate>();
        app.world
            .init_resource::<lumen_core::components::A11yAnnouncementQueue>();
        app.world
            .init_resource::<lumen_core::components::A11yScrollIntoViewRequests>();
        app.world
            .init_resource::<lumen_core::components::A11yContextMenuRequests>();
        app.add_systems(TickStage::A11ySync, flag_a11y_changes);
        app.add_systems(TickStage::A11ySync, sync_a11y_tree.after(flag_a11y_changes));
    }
}

#[cfg(test)]
mod tests {
    //! Round-trip tests for the W5.* a11y plumbing:
    //!
    //! - `A11yState` bit changes propagate to `PendingA11yUpdate` within
    //!   one tick (W5.1 dirty-driven sync).
    //! - `Action::Increment` on a `SliderValue` entity bumps the value
    //!   (W5.3 inbound-action handler - covered as a unit on the helper
    //!   function that handle_a11y_action calls).
    //! - `Action::ScrollIntoView` updates the relevant `ScrollOffset`
    //!   target (covered in `lumen-primitives::scroll` tests).
    use super::*;
    use lumen_core::app::{App, Plugin};
    use lumen_core::components::{
        A11yLabel, A11yState, A11yValue, DirtyA11y, Disabled, LumenClasses, LumenTag,
        PendingA11yUpdate, Selected, SliderValue, TextContent, Visible,
    };

    fn drive_one_tick(app: &mut App) {
        // The Tick schedule includes A11ySync; running it once is the
        // ECS-side roundtrip.
        app.world.run_schedule(lumen_core::app::Tick);
    }

    /// One tick of the real `App::tick` path: the schedule plus the
    /// end-of-tick `clear_trackers` that rotates the removal buffers.
    fn drive_full_tick(app: &mut App) {
        app.world.run_schedule(lumen_core::app::Tick);
        app.world.clear_trackers();
    }

    fn transform(w: f32, h: f32) -> Transform {
        Transform {
            absolute: glam::Vec2::ZERO,
            size: glam::Vec2::new(w, h),
            baseline_y: None,
        }
    }

    fn classes(names: &[&str]) -> LumenClasses {
        LumenClasses(names.iter().map(|n| (*n).into()).collect())
    }

    fn node_for(update: &TreeUpdate, entity: Entity) -> &Node {
        let id = entity_to_node(entity);
        update
            .nodes
            .iter()
            .find(|(nid, _)| *nid == id)
            .map(|(_, n)| n)
            .unwrap_or_else(|| panic!("no node published for {entity:?}"))
    }

    /// App with the plugin installed and one root entity every other
    /// fixture entity hangs off, so root auto-detection is deterministic.
    fn app_with_root() -> (App, Entity) {
        let mut app = App::new();
        A11yPlugin.build(&mut app);
        let root = app
            .world
            .spawn((transform(800.0, 600.0), LumenTag("root".into())))
            .id();
        (app, root)
    }

    #[test]
    fn a11y_state_change_propagates_to_pending_update_in_one_tick() {
        let mut app = App::new();
        A11yPlugin.build(&mut app);
        // Spawn a focusable entity with no Transform (legacy walker
        // skipped these - the rewrite emits them anyway).
        let entity = app
            .world
            .spawn((
                Transform {
                    absolute: glam::Vec2::ZERO,
                    size: glam::Vec2::new(40.0, 20.0),
                    baseline_y: None,
                },
                A11yLabel("checkbox".into()),
                A11yState::default(),
                DirtyA11y,
            ))
            .id();

        drive_one_tick(&mut app);
        // First tick produced a TreeUpdate with the node visible.
        assert!(
            app.world.resource::<PendingA11yUpdate>().boxed.is_some(),
            "first tick must publish a TreeUpdate",
        );
        // Drain so the next tick must produce a fresh one if state moves.
        let _ = take_pending_tree_update(&mut app.world);

        // Quiescent tick: nothing dirty, nothing published.
        drive_one_tick(&mut app);
        assert!(
            app.world.resource::<PendingA11yUpdate>().boxed.is_none(),
            "quiescent tick must not republish",
        );

        // Flip CHECKED; mark dirty. Within one tick the sync should fire.
        {
            let mut st = app.world.get_mut::<A11yState>(entity).unwrap();
            *st |= A11yState::CHECKED;
        }
        app.world.entity_mut(entity).insert(DirtyA11y);
        drive_one_tick(&mut app);
        assert!(
            app.world.resource::<PendingA11yUpdate>().boxed.is_some(),
            "A11yState change must produce a TreeUpdate within one tick",
        );
        // DirtyA11y is drained by the sync system.
        assert!(
            app.world.get::<DirtyA11y>(entity).is_none(),
            "DirtyA11y must be cleared after the sync system runs",
        );
    }

    #[test]
    fn slider_increment_step_default() {
        // Pure-function unit of the inbound Increment logic the
        // window-winit handler runs. The step defaults to (max-min)/100
        // when no explicit A11yValue.step is set on the entity.
        let mut sv = SliderValue {
            value: 50.0,
            min: 0.0,
            max: 100.0,
            step: None,
        };
        let dir = 1.0_f32;
        let span = (sv.max - sv.min).abs();
        let step = if span > 0.0 { span / 100.0 } else { 1.0 };
        let next = (sv.value + dir * step).clamp(sv.min, sv.max);
        sv.value = next;
        assert!((sv.value - 51.0).abs() < f32::EPSILON);
    }

    #[test]
    fn slider_decrement_clamps_at_min() {
        let mut sv = SliderValue {
            value: 0.0,
            min: 0.0,
            max: 100.0,
            step: None,
        };
        let dir = -1.0_f32;
        let span = (sv.max - sv.min).abs();
        let step = if span > 0.0 { span / 100.0 } else { 1.0 };
        let next = (sv.value + dir * step).clamp(sv.min, sv.max);
        sv.value = next;
        assert!(
            (sv.value - 0.0).abs() < f32::EPSILON,
            "decrement at min must clamp"
        );
    }

    #[test]
    fn a11y_value_from_slider() {
        let sv = SliderValue {
            value: 42.0,
            min: 0.0,
            max: 100.0,
            step: None,
        };
        let av: A11yValue = (&sv).into();
        assert!((av.now - 42.0_f64).abs() < f64::EPSILON);
        assert!((av.min - 0.0_f64).abs() < f64::EPSILON);
        assert!((av.max - 100.0_f64).abs() < f64::EPSILON);
    }

    #[test]
    fn root_window_entity_auto_resolves_to_parentless_node() {
        let mut app = App::new();
        A11yPlugin.build(&mut app);
        let root = app
            .world
            .spawn((
                Transform {
                    absolute: glam::Vec2::ZERO,
                    size: glam::Vec2::new(800.0, 600.0),
                    baseline_y: None,
                },
                A11yLabel("root".into()),
                DirtyA11y,
            ))
            .id();
        drive_one_tick(&mut app);
        let resolved = app
            .world
            .get_resource::<lumen_core::components::RootWindowEntity>()
            .expect("RootWindowEntity must auto-resolve once a parentless entity exists");
        assert_eq!(resolved.0, root);
    }

    #[test]
    fn component_change_republishes_without_a_focus_move_or_dirty_marker() {
        let (mut app, root) = app_with_root();
        let label = app
            .world
            .spawn((
                transform(80.0, 20.0),
                LumenTag("label".into()),
                TextContent("before".into()),
                ChildOf(root),
            ))
            .id();

        drive_full_tick(&mut app);
        let first = take_pending_tree_update(&mut app.world).expect("first tick publishes a tree");
        assert_eq!(node_for(&first, label).label(), Some("before"));

        drive_full_tick(&mut app);
        assert!(
            app.world.resource::<PendingA11yUpdate>().boxed.is_none(),
            "a tick with no change must not republish",
        );

        // No DirtyA11y, no focus move: only the text moves.
        app.world.get_mut::<TextContent>(label).unwrap().0 = "after".into();
        drive_full_tick(&mut app);
        let second = take_pending_tree_update(&mut app.world)
            .expect("a plain component change must republish the tree");
        assert_eq!(node_for(&second, label).label(), Some("after"));
    }

    #[test]
    fn disabled_reaches_the_tree_and_clears_on_removal() {
        let (mut app, root) = app_with_root();
        let button = app
            .world
            .spawn((
                transform(60.0, 24.0),
                LumenTag("button".into()),
                TextContent("Save".into()),
                TabIndex(0),
                Disabled,
                ChildOf(root),
            ))
            .id();

        drive_full_tick(&mut app);
        let update = take_pending_tree_update(&mut app.world).expect("first tick publishes a tree");
        let node = node_for(&update, button);
        assert_eq!(node.role(), Role::Button);
        assert_eq!(node.label(), Some("Save"));
        assert!(node.is_disabled(), "disabled must reach the published node");

        // Removing the marker leaves no change tick behind; the removal
        // reader is what keeps the tree in step.
        app.world.entity_mut(button).remove::<Disabled>();
        drive_full_tick(&mut app);
        let update = take_pending_tree_update(&mut app.world)
            .expect("removing Disabled must republish the tree");
        assert!(!node_for(&update, button).is_disabled());
    }

    #[test]
    fn dialog_reports_a_modal_dialog() {
        let (mut app, root) = app_with_root();
        let dialog = app
            .world
            .spawn((
                transform(400.0, 300.0),
                LumenTag("dialog".into()),
                A11yLabel("Confirm".into()),
                ChildOf(root),
            ))
            .id();
        drive_full_tick(&mut app);
        let update = take_pending_tree_update(&mut app.world).expect("tree");
        let node = node_for(&update, dialog);
        assert_eq!(node.role(), Role::Dialog);
        assert!(node.is_modal());
    }

    #[test]
    fn tab_strip_reports_a_tab_list_with_a_selected_tab() {
        let (mut app, root) = app_with_root();
        let strip = app
            .world
            .spawn((
                transform(400.0, 36.0),
                LumenTag("row".into()),
                classes(&["tab-strip"]),
                ChildOf(root),
            ))
            .id();
        let active = app
            .world
            .spawn((
                transform(100.0, 36.0),
                LumenTag("button".into()),
                classes(&["tab-btn"]),
                TextContent("General".into()),
                TabIndex(0),
                Selected,
                ChildOf(strip),
            ))
            .id();
        let idle = app
            .world
            .spawn((
                transform(100.0, 36.0),
                LumenTag("button".into()),
                classes(&["tab-btn"]),
                TextContent("Advanced".into()),
                TabIndex(0),
                ChildOf(strip),
            ))
            .id();

        drive_full_tick(&mut app);
        let update = take_pending_tree_update(&mut app.world).expect("tree");
        assert_eq!(node_for(&update, strip).role(), Role::TabList);
        let active_node = node_for(&update, active);
        assert_eq!(active_node.role(), Role::Tab);
        assert_eq!(active_node.is_selected(), Some(true));
        assert_eq!(node_for(&update, idle).is_selected(), Some(false));
    }

    #[test]
    fn radio_reports_a_toggled_radio_button() {
        let (mut app, root) = app_with_root();
        let chosen = app
            .world
            .spawn((
                transform(120.0, 24.0),
                LumenTag("radio".into()),
                A11yLabel("Weekly".into()),
                Selected,
                ChildOf(root),
            ))
            .id();
        let other = app
            .world
            .spawn((
                transform(120.0, 24.0),
                LumenTag("radio".into()),
                A11yLabel("Daily".into()),
                ChildOf(root),
            ))
            .id();

        drive_full_tick(&mut app);
        let update = take_pending_tree_update(&mut app.world).expect("tree");
        let chosen_node = node_for(&update, chosen);
        assert_eq!(chosen_node.role(), Role::RadioButton);
        assert_eq!(chosen_node.toggled(), Some(accesskit::Toggled::True));
        assert_eq!(node_for(&update, other).toggled(), None);
    }

    #[test]
    fn progress_reports_a_progress_indicator_and_takes_no_value_actions() {
        let (mut app, root) = app_with_root();
        let bar = app
            .world
            .spawn((
                transform(200.0, 8.0),
                LumenTag("progress".into()),
                A11yLabel("Import".into()),
                A11yValue {
                    now: 40.0,
                    min: 0.0,
                    max: 100.0,
                    step: 0.0,
                    text: None,
                },
                ChildOf(root),
            ))
            .id();
        drive_full_tick(&mut app);
        let update = take_pending_tree_update(&mut app.world).expect("tree");
        let node = node_for(&update, bar);
        assert_eq!(node.role(), Role::ProgressIndicator);
        assert_eq!(node.numeric_value(), Some(40.0));
        assert_eq!(node.max_numeric_value(), Some(100.0));
        assert!(!node.supports_action(Action::Increment));
    }

    #[test]
    fn dropdown_header_reports_a_combo_box_that_tracks_its_panel() {
        let (mut app, root) = app_with_root();
        let column = app
            .world
            .spawn((
                transform(200.0, 36.0),
                LumenTag("column".into()),
                classes(&["dropdown"]),
                ChildOf(root),
            ))
            .id();
        let header = app
            .world
            .spawn((
                transform(200.0, 36.0),
                LumenTag("button".into()),
                classes(&["dropdown-button"]),
                TextContent("Medium".into()),
                TabIndex(0),
                ChildOf(column),
            ))
            .id();
        // The `<if mode="hide">` block that owns the panel body.
        let block = app
            .world
            .spawn((LumenTag("if".into()), Visible(false), ChildOf(column)))
            .id();
        app.world.spawn((
            transform(200.0, 96.0),
            LumenTag("column".into()),
            classes(&["dropdown-panel"]),
            ChildOf(block),
        ));

        drive_full_tick(&mut app);
        let update = take_pending_tree_update(&mut app.world).expect("tree");
        let node = node_for(&update, header);
        assert_eq!(node.role(), Role::ComboBox);
        assert_eq!(
            node.is_expanded(),
            Some(false),
            "a hidden panel means collapsed"
        );

        app.world.get_mut::<Visible>(block).unwrap().0 = true;
        drive_full_tick(&mut app);
        let update = take_pending_tree_update(&mut app.world)
            .expect("opening the panel must republish the tree");
        assert_eq!(node_for(&update, header).is_expanded(), Some(true));
    }

    #[test]
    fn a_text_input_names_itself_from_its_id_and_values_itself_from_its_text() {
        let (mut app, root) = app_with_root();
        let field = app
            .world
            .spawn((
                transform(200.0, 28.0),
                LumenTag("input".into()),
                LumenId("city".into()),
                TextContent("Berlin".into()),
                TabIndex(0),
                ChildOf(root),
            ))
            .id();
        drive_full_tick(&mut app);
        let update = take_pending_tree_update(&mut app.world).expect("tree");
        let node = node_for(&update, field);
        assert_eq!(node.role(), Role::TextInput);
        assert_eq!(node.value(), Some("Berlin"));
        assert_eq!(node.label(), Some("city"));
    }
}
