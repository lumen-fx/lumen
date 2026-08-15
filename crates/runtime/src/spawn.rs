//! `LayoutIR::spawn_into` walks the IR tree and spawns entities into the main world.
//!
//! Maps:
//!
//! * `width` / `height` / `flex` / `padding` / `margin` -> [`Style`].
//! * `bg` + `radius` + `shadow` -> [`Visuals`].
//! * `text` -> [`TextContent`]; color/size/align/wrap/max-lines -> [`TextStyle`].
//! * `scroll` -> [`Scroll`] (with inline velocity) + [`ScrollOffset`].
//! * `tab-index` -> [`TabIndex`].
//! * Every spawned entity carries [`DirtyLayout`] so taffy runs on first tick.
//! * Children link to parents via [`ChildOf`].

use lumen_core::signals::signal_is_truthy;
use lumen_ir::layout_ir::{Attributes, BindKind, Element, InterpolationSlot, LayoutIR};

/// `<if mode="...">` policy. `Render` despawn/respawns the subtree on
/// each transition (default; cheap state-wise but loses focus / scroll
/// / per-row signals across show/hide). `Hide` mounts once and toggles
/// `Visible(bool)` on every descendant, preserving state.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Default)]
pub enum IfMode {
    /// Despawn the body on falsy -> truthy -> despawn each transition.
    #[default]
    Render,
    /// Mount once; toggle `Visible` on falsy / truthy.
    Hide,
}

impl From<lumen_ir::layout_ir::IfModeSpec> for IfMode {
    fn from(s: lumen_ir::layout_ir::IfModeSpec) -> Self {
        use lumen_ir::layout_ir::IfModeSpec;
        match s {
            IfModeSpec::Render => IfMode::Render,
            IfModeSpec::Hide => IfMode::Hide,
        }
    }
}

/// Empty marker added alongside [`IfMarker`] for entities spawned from
/// a `<dialog>` tag. Drives Esc-to-close + the focus trap
/// (`lumen_core::components::FocusBoundary` is inserted alongside).
/// The `IfMarker` carries the bound `open` signal name; this marker
/// just distinguishes dialogs from generic `<if>` blocks.
#[derive(bevy_ecs::component::Component, Clone, Copy, Debug, Default)]
pub struct DialogMarker;

/// `autofocus="true"` marker: when the containing `<dialog>` opens,
/// [`manage_dialog_lifecycle`] moves focus here first.
#[derive(bevy_ecs::component::Component, Clone, Copy, Debug, Default)]
pub struct AutoFocus;

/// `<button default="true">` marker: the dialog's DEFAULT button (Qt
/// `QPushButton::setDefault`). Enter anywhere in the dialog activates
/// it ([`activate_dialog_default_on_enter`]), and a close that went
/// through it fires the `accepted` hook instead of `rejected`.
#[derive(bevy_ecs::component::Component, Clone, Copy, Debug, Default)]
pub struct DefaultButton;

/// Per-dialog open/close bookkeeping maintained by
/// [`manage_dialog_lifecycle`] - mirrors
/// `lumen_primitives::PopupNavSession` for popups.
#[derive(bevy_ecs::component::Component, Clone, Debug, Default)]
pub struct DialogSession {
    /// Open state last observed (edge detector).
    pub open: bool,
    /// Initial focus still pending: the dialog body mounts via
    /// deferred commands one tick after the open edge, so the focus
    /// pass retries until a target exists.
    pub needs_focus: bool,
    /// Focus holder at open time, restored on close (Qt/web modal
    /// focus-restore contract).
    pub prev_focus: Option<Entity>,
    /// Whether the pre-open holder carried `FocusVisible`.
    pub prev_focus_visible: bool,
    /// The default button was activated during this open cycle -
    /// resolves the close as `accepted` (exactly one of
    /// accepted/rejected fires, on the close edge).
    pub pending_accept: bool,
}

/// Marker for an `<if signal="...">` block. The reconciler mounts /
/// dismounts the subtree based on the named signal's truthiness, or -
/// when [`Self::eq`] is `Some`- on value equality.
#[derive(bevy_ecs::component::Component, Clone, Debug)]
pub struct IfMarker {
    /// Signal name to evaluate.
    pub signal_name: String,
    /// Markup children kept around as the body template - spawned when
    /// truthy, despawned when not.
    pub body: Vec<Element>,
    /// Last-known mount state so we can detect transitions cheaply.
    pub currently_mounted: bool,
    /// How to handle the transition (despawn vs hide).
    pub mode: IfMode,
    /// When `Some(expected)`, mounts only when `Signals[signal_name] == expected`.
    /// When `None`, mounts when `Signals[signal_name]` is non-empty and not literal `"false"`/`"0"`.
    pub eq: Option<String>,
    /// [`IfMode::Hide`] only: the `Style.display` value the block had
    /// before the last hide, restored on the next show. Hide flows
    /// through `Display::None` (spec section 17.4 - space is released and the
    /// relayout comes free via `Changed<Style>`).
    pub saved_display: lumen_core::components::Display,
    /// [`IfMode::Hide`] only: the visibility state last applied to the
    /// entity (`None` = never applied). The reconciler only touches
    /// `Style` / `Visible` on a transition - re-inserting every tick
    /// kept `FrameDirty` permanently set (D5).
    pub applied_visible: Option<bool>,
}

/// Marker for a `<for each="...">` block. The reconciler reads the referenced [`ArraySignals`] entry each tick,
/// expands `body` once per item (substituting `{field}` placeholders against the item record), and spawns/despawns child entities to match.
/// The reconciler compares the key sequence it built last tick with the new one: an unchanged sequence is a no-op,
/// an appended prefix spawns only the new rows, a truncated prefix despawns only the trailing rows, and anything
/// else (reorder, mid-insert, mid-remove) is a full despawn-and-respawn. `key_field` names the record field the
/// keys come from; without it the item index is the key.
#[derive(bevy_ecs::component::Component, Clone, Debug)]
pub struct ForMarker {
    /// Name of the `ArraySignals` entry to iterate.
    pub array_name: String,
    /// Template body - the markup children of the `<for>` element.
    pub body: Vec<Element>,
    /// Stable-id field within each record (e.g. `"id"`). Optional; without
    /// it the reconciler keys rows by item index.
    pub key_field: Option<String>,
    /// Cache of the keys (or item indices when `key_field` is absent)
    /// currently materialized as children, so the reconciler can detect
    /// no-op frames without re-walking the body template.
    pub cached_keys: Vec<String>,
    /// `<for virtualized="true">` opt-in. When set, the reconciler spawns only rows in the visible scroll window plus a small buffer; each row is absolute-positioned via inline `inset` from `row_height * row_index`.
    pub virtualized: bool,
    /// Per-row pixel height used by the virtualization windowing math.
    /// Required when `virtualized = true`. Default 32 px when authors
    /// turn on `virtualized` without specifying `row-height`.
    pub row_height: f32,
    /// Virtualized only: the `(row_index, key)` pairs currently mounted,
    /// in child-list order (each entry owns `body.len()` consecutive
    /// children). Spec section 15.3 windowed reuse - a window shift keeps every
    /// row whose `(index, key)` is unchanged instead of respawning the
    /// whole band.
    pub win_rows: Vec<(usize, String)>,
    /// Virtualized only: the body template with the CSS cascade already
    /// applied (cascade-once-per-template). `None` until first use and
    /// after every stylesheet change; rebuilt lazily by the reconciler.
    /// Skipped (per-row cascade instead) when the template's `id` /
    /// `class` attrs contain `{...}` placeholders, because then selector
    /// matching depends on per-row substitution results.
    pub cascaded_body: Option<std::sync::Arc<Vec<Element>>>,
}

/// Who mounts the rows of a virtualized `<for>`.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum Virtualization {
    /// The reconciler windows the rows itself: it reads the nearest
    /// `<scroll>` ancestor, mounts the visible band plus an overscan buffer,
    /// and absolute-positions each row. What a desktop app does.
    #[default]
    Enabled,
    /// The presentation layer already windows long lists, so the reconciler
    /// mounts every row and leaves `virtualized="true"` to it.
    HostManaged,
}

/// Who resolves the styles of a `<for>` row.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum RowStyle {
    /// The reconciler runs the CSS cascade over each row it builds, so the
    /// spawned entities carry resolved values. What a desktop app does.
    #[default]
    Cascade,
    /// The presentation layer has its own cascade over the same stylesheet,
    /// so the reconciler leaves row markup unresolved rather than doing the
    /// work twice.
    HostStyled,
}

/// What the presentation layer under this scene already does for itself, so
/// [`reconcile_for_blocks`] can stop doing it.
///
/// The defaults are what a desktop app needs, and a world with no policy
/// resource behaves as the defaults do. A host that brings its own windowing
/// or its own cascade inserts a policy saying so.
#[derive(bevy_ecs::resource::Resource, Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct ScenePolicy {
    /// Who mounts the rows of a virtualized `<for>`.
    pub virtualization: Virtualization,
    /// Who resolves the styles of a `<for>` row.
    pub row_style: RowStyle,
}

use bevy_ecs::prelude::*;
use lumen_core::prelude::*;
use lumen_core::signals::ArrayItem;

// The typography-role -> px table moved to `lumen-ir` (it backs the
// `Attributes -> TextStyle` conversion, which the orphan rule forced into
// `lumen-ir`). Re-export it so `crate::spawn::typography_role_to_px` - used
// by `run::restyle` - keeps resolving.
pub use lumen_ir::typography_role_to_px;

/// Spawn an IR tree into a main world. Defined as a local extension trait
/// because [`LayoutIR`] now lives in `lumen-ir` and an inherent `impl` on a
/// foreign type isn't allowed; the spawn logic (which drives `lumenc`'s ECS
/// walker) stays here.
pub trait SpawnIntoWorld {
    /// Spawn the IR tree into the given main world. Returns the root entity.
    fn spawn_into(&self, world: &mut World) -> Entity;
}

impl SpawnIntoWorld for LayoutIR {
    fn spawn_into(&self, world: &mut World) -> Entity {
        if let Some(sheet) = self.combined_stylesheet.clone() {
            world.insert_resource(LumenStylesheet(sheet));
        }
        spawn_element(world, &self.root, None)
    }
}

/// Spawn an [`Element`] subtree under `parent` (or as a new root when
/// `None`) WITHOUT touching the global [`LumenStylesheet`] resource.
///
/// Used by the dev-only devtools overlay mount: it applies its own isolated
/// stylesheet to the overlay IR before spawning, so it must not clobber the
/// app's `<for>`-reconciler stylesheet the way [`LayoutIR::spawn_into`] does.
pub fn spawn_subtree(world: &mut World, el: &Element, parent: Option<Entity>) -> Entity {
    spawn_element(world, el, parent)
}

/// World-resource wrapper for the combined skin + user CSS so the
/// for-block reconciler can re-apply matching rules to runtime-
/// substituted template clones.
#[derive(Resource, Clone)]
pub struct LumenStylesheet(pub lumen_ir::css::Stylesheet);

/// Backing counter for [`lumen_core::components::DocumentOrder`]. One per
/// world; `next_document_order` bumps it once per spawned element.
#[derive(Resource, Default)]
struct DocumentOrderCounter(u32);

/// Next value for [`lumen_core::components::DocumentOrder`], assigned in
/// the exact sequence `spawn_element` visits the parsed tree (depth-first,
/// parent before children, siblings in markup order). Must run before the
/// entity's `world.spawn(..)` call - `EntityWorldMut` holds `world`
/// exclusively for its lifetime, so the resource bump can't be interleaved
/// once that borrow starts.
fn next_document_order(world: &mut World) -> u32 {
    let mut counter = world.get_resource_or_insert_with(DocumentOrderCounter::default);
    let n = counter.0;
    counter.0 = counter.0.wrapping_add(1);
    n
}

/// The text an element spawns with.
///
/// Without `translatable="key"` this is just the authored `text`. With it,
/// the key is resolved against the app's loaded catalogue; a key the
/// catalogue does not carry falls back to the authored text, and an
/// element with neither renders the key. That ordering means an app whose
/// translations are missing still shows its source strings, and a
/// `translatable` element with no text is never blank.
fn resolve_text(world: &World, el: &Element) -> Option<String> {
    let Some(key) = &el.attrs.translatable else {
        return el.attrs.text.clone();
    };
    let translated = world
        .get_resource::<lumen_i18n::SharedI18n>()
        .and_then(|i18n| i18n.try_t(key));
    Some(
        translated
            .or_else(|| el.attrs.text.clone())
            .unwrap_or_else(|| key.clone()),
    )
}

/// Alpha multiplier applied to a disabled entity when neither
/// `disabled-bg` nor an explicit `:disabled { opacity }` was authored.
/// CSS `disabled-opacity` (`Attributes::disabled_opacity_default`)
/// overrides it; this is the single Rust-side fallback.
const DISABLED_OPACITY_DEFAULT: f32 = 0.5;

fn spawn_element(world: &mut World, el: &Element, parent: Option<Entity>) -> Entity {
    // Seed any synthetic signal defaults (currently only the
    // `<tabs>` parser pass authors this - picks the first `<tab>` as
    // the default active when no script has written the signal yet).
    if let Some((name, default_value)) = &el.attrs.signal_seed
        && let Some(mut store) =
            world.get_resource_mut::<lumen_core::property_store::PropertyStore>()
        && store.get_global_str(name).is_none()
    {
        store.set_global_str(name, default_value.as_str());
    }
    // Two-way binds with an AUTHORED initial attr (`checked="true"`,
    // `value="42"`, `text="..."`) seed the signal if nothing wrote it yet,
    // so a sibling `bind-text` label shows the initial value without a
    // script. Seed-if-absent only: a script's `signal(name, default)`
    // publish or an earlier write wins, and the spawn-tick `Changed`
    // detection on the widget component is deliberately NOT pushed
    // (`push_*_to_signal` skips `is_added()` rows) - the widget's
    // component default is not an authored value.
    if let Some(spec) = &el.attrs.bind {
        let authored: Option<String> = match spec.kind {
            BindKind::Checked => el
                .attrs
                .checked
                .map(|b| if b { "true" } else { "false" }.to_string()),
            BindKind::Value => el.attrs.value.map(|v| format!("{v}")),
            BindKind::Text => el.attrs.text.clone(),
        };
        if let Some(v) = authored
            && let Some(mut store) =
                world.get_resource_mut::<lumen_core::property_store::PropertyStore>()
            && store.get_global_str(&spec.name).is_none()
        {
            store.set_global_str(&spec.name, v.as_str());
        }
    }
    // Resolve the element's text before the world is borrowed by `spawn`:
    // a `translatable="key"` element reads the loaded catalogue.
    let text = resolve_text(world, el);
    let mut style = Style::from(&el.attrs);
    apply_ua_style_defaults(&el.tag, &el.attrs, &mut style);
    let is_boundary = is_relayout_boundary(&style, el);
    let doc_order = next_document_order(world);
    let mut entity = world.spawn((
        style,
        DirtyLayout,
        lumen_core::components::DocumentOrder(doc_order),
    ));
    if let Some(p) = parent {
        entity.insert(ChildOf(p));
    }
    if is_boundary {
        entity.insert(RelayoutBoundary);
    }
    if let Some(v) = Option::<Visuals>::from(&el.attrs) {
        entity.insert(v);
    }
    if let Some(text) = &text {
        entity.insert(TextContent(text.clone()));
    }
    if matches!(el.tag.as_str(), "input" | "textarea") {
        let default_multiline = el.tag == "textarea";
        entity.insert(TextInput {
            placeholder: el.attrs.placeholder.clone().unwrap_or_default(),
            cursor: 0,
            selection_anchor: None,
            multiline: el.attrs.multiline.unwrap_or(default_multiline),
        });
        // Caret / selected-glyph paint overrides ride a separate
        // component so non-input text never carries the extra row.
        // Absent => the renderer falls back (caret = text fill, selected
        // glyphs keep their fill), so only spawn it when authored.
        if el.attrs.caret_color.is_some() || el.attrs.selection_text_color.is_some() {
            entity.insert(TextInputPaint {
                caret_color: el.attrs.caret_color.map(Into::into),
                selection_foreground: el.attrs.selection_text_color.map(Into::into),
            });
        }
        // `caret-width` / `password-character` (markup attrs or the CSS
        // properties) win; each component's own `Default` carries the
        // runtime's `CARET_WIDTH_PX` / `PASSWORD_MASK_CHAR` fallback.
        // Only meaningful on `<input>` / `<textarea>`, same as
        // `TextInputPaint` above.
        entity.insert(
            el.attrs
                .caret_width
                .map(lumen_core::components::CaretWidth)
                .unwrap_or_default(),
        );
        entity.insert(
            el.attrs
                .password_character
                .map(lumen_core::components::PasswordCharacter)
                .unwrap_or_default(),
        );
    }
    if el.tag == "toggle" {
        entity.insert(Toggleable {
            checked: el.attrs.checked.unwrap_or(false),
        });
        // Track fills: author `bg` (or the built-in gray) while
        // unchecked, `:checked { bg }` (or the built-in accent) while
        // checked. `sync_toggle_visuals` swaps the fill at runtime.
        let defaults = lumen_primitives::ToggleStyle::default();
        entity.insert(lumen_primitives::ToggleStyle {
            checked_bg: el
                .attrs
                .checked_bg
                .map(Into::into)
                .unwrap_or(defaults.checked_bg),
            unchecked_bg: el
                .attrs
                .bg
                .as_ref()
                .and_then(|b| Fill::from(b).as_solid())
                .unwrap_or(defaults.unchecked_bg),
        });
        // The checked/unchecked swap needs a fill to write into even
        // when no skin or author CSS styled the track.
        if entity.get::<Visuals>().is_none() {
            entity.insert(Visuals {
                fill: Some(Fill::Solid(defaults.unchecked_bg)),
                radius: 12.0,
                corner_radii: None,
                shadows: Vec::new(),
                border: None,
            });
        }
    }
    if el.tag == "switch" {
        entity.insert(Toggleable {
            checked: el.attrs.checked.unwrap_or(false),
        });
        // Track fills: author `bg` (or the built-in gray) while unchecked,
        // `switch:checked { bg }` (or the built-in accent) while checked.
        // `sync_switch_visuals` swaps the fill at runtime - same
        // design-token surface as `<toggle>`.
        let defaults = lumen_primitives::SwitchStyle::default();
        entity.insert(lumen_primitives::SwitchStyle {
            checked_bg: el
                .attrs
                .checked_bg
                .map(Into::into)
                .unwrap_or(defaults.checked_bg),
            unchecked_bg: el
                .attrs
                .bg
                .as_ref()
                .and_then(|b| Fill::from(b).as_solid())
                .unwrap_or(defaults.unchecked_bg),
        });
        // Explicit a11y role: a switch is announced as `Role::Switch`, not
        // the `Role::CheckBox` that a bare `Toggleable` would derive.
        entity.insert(lumen_core::components::A11yRole::Switch);
        // The checked/unchecked swap needs a fill to write into (and the
        // track must be a hit-test candidate) even with no skin / CSS.
        if entity.get::<Visuals>().is_none() {
            entity.insert(Visuals {
                fill: Some(Fill::Solid(defaults.unchecked_bg)),
                radius: 14.0,
                corner_radii: None,
                shadows: Vec::new(),
                border: None,
            });
        }
    }
    if el.tag == "slider" {
        let min = el.attrs.min.unwrap_or(0.0);
        let max = el.attrs.max.unwrap_or(1.0);
        let value = el
            .attrs
            .value
            .unwrap_or(min)
            .clamp(min.min(max), min.max(max));
        entity.insert(SliderValue {
            value,
            min,
            max,
            step: el.attrs.step,
        });
        // UA fallback track fill - same rationale as the `<toggle>`
        // block above: with no skin or author CSS the slider must
        // still paint a groove AND be a hit-test candidate (hit-test
        // only considers entities with `Visuals` or `Scroll`), or
        // track clicks / drags can never reach `set_slider_on_click`.
        if entity.get::<Visuals>().is_none() {
            entity.insert(Visuals {
                fill: Some(Fill::Solid(lumen_primitives::controls::TOGGLE_UNCHECKED_BG)),
                radius: 4.0,
                corner_radii: None,
                shadows: Vec::new(),
                border: None,
            });
        }
    }
    if el.tag == "checkbox" {
        entity.insert(Toggleable {
            checked: el.attrs.checked.unwrap_or(false),
        });
        let defaults = lumen_primitives::CheckboxStyle::default();
        entity.insert(lumen_primitives::CheckboxStyle {
            // `checkbox:checked { bg }` routes here via checked_bg;
            // the accent constant is the single Rust fallback.
            checked_bg: el
                .attrs
                .checked_bg
                .map(Into::into)
                .unwrap_or(defaults.checked_bg),
        });
        if el.attrs.indeterminate {
            entity.insert(lumen_primitives::Indeterminate);
        }
        // The row itself must be a hit-test candidate so clicking the
        // label / gap toggles too (hit-test only considers entities
        // with `Visuals` or `Scroll`). A fill-less Visuals paints
        // nothing.
        if entity.get::<Visuals>().is_none() {
            entity.insert(Visuals::default());
        }
    }
    if el.tag == "radio"
        && let (Some(group), Some(value)) = (&el.attrs.radio_group, &el.attrs.radio_value)
    {
        entity.insert(lumen_primitives::RadioButton {
            group: group.clone(),
            value: value.clone(),
        });
        let defaults = lumen_primitives::RadioStyle::default();
        entity.insert(lumen_primitives::RadioStyle {
            // `radio:selected { bg }` routes here via selected_bg.
            selected_bg: el
                .attrs
                .selected_bg
                .map(Into::into)
                .unwrap_or(defaults.selected_bg),
        });
        // Same hit-test candidacy rationale as `<checkbox>` above.
        if entity.get::<Visuals>().is_none() {
            entity.insert(Visuals::default());
        }
    }
    if el.tag == "progress" {
        entity.insert(lumen_primitives::ProgressBar {
            // No `value` and no binding = indeterminate; a later
            // `bind-value` pull flips it determinate.
            value: el.attrs.value,
            max: el.attrs.max.unwrap_or(1.0),
            period_ms: el
                .attrs
                .progress_duration
                .unwrap_or(lumen_primitives::PROGRESS_PERIOD_MS),
        });
        // The accessibility walk reads `A11yValue`, not `ProgressBar`;
        // `apply_progress_bindings` keeps the two in step.
        entity.insert(lumen_core::components::A11yValue {
            now: f64::from(el.attrs.value.unwrap_or(0.0)),
            min: 0.0,
            max: f64::from(el.attrs.max.unwrap_or(1.0)),
            step: 0.0,
            text: None,
        });
        // `progress-chunk` (markup attr or the CSS property) wins;
        // `ProgressChunk::default()` carries the runtime's own
        // indeterminate-fill-fraction constant when unauthored.
        entity.insert(
            el.attrs
                .progress_chunk
                .map(lumen_primitives::progress::ProgressChunk)
                .unwrap_or_default(),
        );
        // UA fallback track fill so a skinless `<progress>` still
        // paints a groove (same rationale as `<slider>`).
        if entity.get::<Visuals>().is_none() {
            entity.insert(Visuals {
                fill: Some(Fill::Solid(lumen_primitives::controls::TOGGLE_UNCHECKED_BG)),
                radius: 3.0,
                corner_radii: None,
                shadows: Vec::new(),
                border: None,
            });
        }
    }
    // Synthetic widget-part markers from the parser desugars.
    if let Some(part) = el.attrs.part {
        match part {
            lumen_ir::layout_ir::WidgetPart::CheckboxBox => {
                entity.insert(lumen_primitives::CheckboxBox);
                // The sync system mutates `Visuals` (fill swap) and
                // needs a slot even when no skin styled the box.
                if entity.get::<Visuals>().is_none() {
                    entity.insert(Visuals {
                        fill: Some(Fill::Solid(lumen_primitives::controls::TOGGLE_UNCHECKED_BG)),
                        radius: 4.0,
                        corner_radii: None,
                        shadows: Vec::new(),
                        border: None,
                    });
                }
            }
            lumen_ir::layout_ir::WidgetPart::RadioDot => {
                entity.insert(lumen_primitives::RadioDot);
                if entity.get::<Visuals>().is_none() {
                    entity.insert(Visuals {
                        fill: Some(Fill::Solid(lumen_primitives::controls::TOGGLE_UNCHECKED_BG)),
                        radius: 9.0,
                        corner_radii: None,
                        shadows: Vec::new(),
                        border: None,
                    });
                }
            }
            lumen_ir::layout_ir::WidgetPart::ProgressFill => {
                entity.insert(lumen_primitives::ProgressFill);
                // The fill needs a paintable Visuals even without a
                // skin; accent-ish fallback, any CSS rule wins.
                if entity.get::<Visuals>().is_none() {
                    entity.insert(Visuals {
                        fill: Some(Fill::Solid(lumen_primitives::controls::TOGGLE_CHECKED_BG)),
                        radius: 3.0,
                        corner_radii: None,
                        shadows: Vec::new(),
                        border: None,
                    });
                }
            }
        }
    }
    // UA fallback field chrome for text inputs: click-to-focus routes
    // through `ClickEvent`, which requires the entity to be a hit-test
    // candidate (`Visuals` or `Scroll`) - an unstyled `<input>` was
    // invisible AND unclickable. A subtle translucent fill works on
    // both light and dark app themes; any skin or author rule wins.
    if matches!(el.tag.as_str(), "input" | "textarea") && entity.get::<Visuals>().is_none() {
        entity.insert(Visuals {
            fill: Some(Fill::Solid(Color::rgba(0.5, 0.5, 0.5, 0.18))),
            radius: 4.0,
            corner_radii: None,
            shadows: Vec::new(),
            border: None,
        });
    }
    // Attach Validation only when validation-specific attrs are set.
    // `<slider min max>` already enforces bounds in SliderValue itself -
    // pulling min/max into Validation for sliders would just duplicate
    // the clamp. So sliders need a `required` or `pattern` opt-in.
    // `<progress min/max>` bounds its own value in `ProgressBar` -
    // same exemption as `<slider>`.
    let validate_min_max = !matches!(el.tag.as_str(), "slider" | "progress")
        && (el.attrs.min.is_some() || el.attrs.max.is_some());
    if el.attrs.required || el.attrs.pattern.is_some() || validate_min_max {
        entity.insert(lumen_core::components::Validation {
            required: el.attrs.required,
            pattern: el.attrs.pattern.clone(),
            min: if validate_min_max { el.attrs.min } else { None },
            max: if validate_min_max { el.attrs.max } else { None },
            is_valid: true,
        });
    }
    if el.attrs.drop_target {
        entity.insert(DropTarget);
        // In-app DnD accept filter (mirrors Qt `dragEnterEvent` +
        // `acceptProposedAction`). No `accept="..."` => accept any payload,
        // matching the file-drop path's "accept anything" default.
        let accept = match &el.attrs.drop_accept {
            Some(mime) => {
                lumen_os_dnd::DropAccept::only([lumen_os_dnd::mime::MimeKind::from(mime.as_str())])
            }
            None => lumen_os_dnd::DropAccept::any(),
        }
        .with_effects(lumen_os_dnd::DropEffectSet::ANY);
        entity.insert(accept);
    }
    // In-app DnD source: `drag-payload="..."` publishes a payload; an empty
    // value derives it from the element `id`. Mirrors HTML5
    // `dataTransfer.setData` / Qt `QMimeData`. Independent of `draggable`
    // (which stays the "physically translate on drag" opt-in).
    if let Some(payload) = &el.attrs.drag_payload {
        let text = if payload.is_empty() {
            el.attrs.id.clone().unwrap_or_default()
        } else {
            payload.clone()
        };
        entity.insert(
            lumen_os_dnd::DragSource::new(text).with_effects(lumen_os_dnd::DropEffectSet::ANY),
        );
    }
    if el.attrs.title_bar_drag {
        entity.insert(lumen_core::components::TitleBarDraggable);
    }
    if let Some(spec) = &el.attrs.tooltip {
        // `None` here means neither the inline attr nor a skin token
        // supplied a value - the runtime defaults (500 ms / 12 px) are
        // the single Rust-side fallback.
        let defaults = lumen_primitives::TooltipSource::default();
        entity.insert(lumen_primitives::TooltipSource {
            text: spec.text.clone(),
            delay_ms: spec.delay_ms.unwrap_or(defaults.delay_ms),
            offset: spec.offset.unwrap_or(defaults.offset),
        });
    }
    if let Some((signal_name, value)) = &el.attrs.tab_strip {
        entity.insert(lumen_primitives::TabStripButton {
            signal_name: signal_name.clone(),
            value: value.clone(),
        });
        // Track fills: author `bg` (or transparent) while unselected,
        // `:selected { bg }` (or the built-in accent) while this button
        // carries `Selected`. `sync_tab_button_visuals` swaps the fill
        // at runtime - same pattern as the `<toggle>` checked/unchecked
        // pair above.
        let defaults = lumen_primitives::TabButtonStyle::default();
        entity.insert(lumen_primitives::TabButtonStyle {
            selected_bg: el
                .attrs
                .selected_bg
                .map(Into::into)
                .unwrap_or(defaults.selected_bg),
            unselected_bg: el
                .attrs
                .bg
                .as_ref()
                .and_then(|b| Fill::from(b).as_solid())
                .unwrap_or(defaults.unselected_bg),
        });
        // The selected/unselected swap needs a fill to write into even
        // when no skin or author CSS styled the button.
        if entity.get::<Visuals>().is_none() {
            entity.insert(Visuals {
                fill: Some(Fill::Solid(defaults.unselected_bg)),
                radius: 0.0,
                corner_radii: None,
                shadows: Vec::new(),
                border: None,
            });
        }
    }
    if let Some(spec) = &el.attrs.dropdown_button {
        entity.insert(lumen_primitives::DropdownButton {
            open_signal: spec.open_signal.clone(),
            value_signal: spec.value_signal.clone(),
            options: spec
                .options
                .iter()
                .map(
                    |(value, label, disabled)| lumen_primitives::DropdownOptionSpec {
                        value: value.clone(),
                        label: label.clone(),
                        disabled: *disabled,
                    },
                )
                .collect(),
        });
        // `popup-gap` (markup attr or the CSS property) wins;
        // `PopupGap::default()` carries the runtime's own gap constant
        // when unauthored. Sits on the trigger (this header entity) -
        // `flip_open_dropdown_panels` reads it off the same `DropdownButton`
        // row it already queries.
        entity.insert(
            el.attrs
                .popup_gap
                .map(lumen_primitives::popup::PopupGap)
                .unwrap_or_default(),
        );
    }
    if let Some((value_signal, value, open_signal)) = &el.attrs.dropdown_option {
        entity.insert(lumen_primitives::DropdownOptionButton {
            value_signal: value_signal.clone(),
            value: value.clone(),
            open_signal: open_signal.clone(),
        });
    }
    if let Some((open_signal, item_id)) = &el.attrs.menu_item {
        entity.insert(lumen_primitives::MenuItemButton {
            open_signal: open_signal.clone(),
            item_id: item_id.clone(),
        });
    }
    if let Some(open_signal) = &el.attrs.popup_panel {
        let inset = el.attrs.inset.unwrap_or_default();
        entity.insert((
            lumen_primitives::PopupPanel {
                open_signal: open_signal.clone(),
                default_top: inset.top,
                default_bottom: inset.bottom,
                positioned: false,
            },
            // Top-layer paint band: the panel must paint over all
            // later-document-order content (inputs, textareas) it
            // floats above.
            lumen_core::render_world::OverlayLayer,
        ));
    }
    if el.tag == "image"
        && let Some(src) = &el.attrs.src
    {
        entity.insert(lumen_assets::ImageSource(std::path::PathBuf::from(src)));
    }
    if let Some(f) = el.attrs.image_fit {
        entity.insert(ImageFit::from(f));
    }
    if let Some(o) = el.attrs.opacity {
        entity.insert(Opacity(o));
    }
    let transitions = el.attrs.effective_transitions();
    if !transitions.is_empty() {
        let specs: Vec<lumen_primitives::TransitionSpec> =
            transitions.iter().map(Into::into).collect();
        entity.insert(lumen_primitives::TransitionSpecs(specs));
    }
    if let Some(ts) = Option::<TextStyle>::from(&el.attrs) {
        entity.insert(ts);
    }
    if let Some(tab) = el.attrs.tab_index {
        entity.insert(TabIndex(tab));
    }
    if el.attrs.autofocus {
        entity.insert(AutoFocus);
    }
    if el.attrs.default_button {
        entity.insert(DefaultButton);
    }
    if let Some(z) = el.attrs.z_index {
        entity.insert(lumen_core::components::ZIndex(z));
    }
    if let Some(spec) = &el.attrs.bind {
        match spec.kind {
            BindKind::Text => {
                entity.insert(BindText::from(spec.name.clone()));
                // `apply_text_bindings` queries `(&BindText, &mut
                // TextContent)`; if the markup omits `text=""` we
                // need to seed an empty TextContent so the first
                // signal write has a target to update.
                if text.is_none() {
                    entity.insert(TextContent(String::new()));
                }
            }
            BindKind::Checked => {
                entity.insert(BindChecked(spec.name.clone()));
            }
            BindKind::Value => {
                entity.insert(BindValue(spec.name.clone()));
            }
        }
    }
    // Per-entity (`$self.<field>`) and parent-entity (`$parent.<field>`)
    // binding markers. The consumer systems are no-op stubs today
    // (W-signal-design step 1) - we install the marker so the spawn
    // surface is final ahead of the follow-up commit that wires the
    // queries.
    if let Some(field) = &el.attrs.bind_self_text {
        entity.insert(BindSelfText::from(field.as_str()));
        if text.is_none() {
            entity.insert(TextContent(String::new()));
        }
    }
    if let Some(field) = &el.attrs.bind_self_value {
        entity.insert(BindSelfValue::from(field.as_str()));
    }
    if let Some(field) = &el.attrs.bind_self_checked {
        entity.insert(BindSelfChecked::from(field.as_str()));
    }
    if let Some(field) = &el.attrs.bind_parent_text {
        entity.insert(BindParentText::from(field.as_str()));
        if text.is_none() {
            entity.insert(TextContent(String::new()));
        }
    }
    if let Some(field) = &el.attrs.bind_parent_value {
        entity.insert(BindParentValue::from(field.as_str()));
    }
    if let Some(field) = &el.attrs.bind_parent_checked {
        entity.insert(BindParentChecked::from(field.as_str()));
    }
    if let Some(id) = &el.attrs.id {
        entity.insert(LumenId(id.clone()));
    }
    // `<a href="page">` - a real anchor. Attach the navigation target so a
    // click on this element switches the active page (file-based pages).
    if el.tag == "a"
        && let Some(href) = &el.attrs.href
    {
        entity.insert(crate::pages::Anchor(href.clone()));
    }
    if !el.attrs.classes.is_empty() {
        entity.insert(LumenClasses::from(el.attrs.classes.clone()));
    }
    // Retain the tag on EVERY spawned element so the runtime can
    // rebuild a `tag.class#id` cascade target and re-resolve computed
    // style in place on a theme / media flip (see
    // `run::reapply_computed_styles`). Pre-skins this was limited to
    // entities with an id / class, which left tag-only rules (the whole
    // per-OS skin surface: `button { ... }`, `root { bg }`, ...) frozen at
    // their spawn-time theme.
    if !el.tag.is_empty() {
        entity.insert(lumen_core::components::LumenTag(el.tag.as_str().into()));
    }
    // W5.4: writing direction + language tag cascade. Both components
    // are inherited at runtime by the respective resolver systems;
    // explicit overrides set here win against the ancestor chain.
    if let Some(dir) = el.attrs.dir {
        entity.insert(dir);
    }
    if let Some(lang) = &el.attrs.lang {
        entity.insert(lumen_core::components::Lang::from(lang.as_str()));
    }
    if let Some(ix) = Option::<lumen_primitives::Interaction>::from(&el.attrs) {
        entity.insert(ix);
    }
    // `disabled-opacity` (markup attr or the CSS property) wins;
    // `DISABLED_OPACITY_DEFAULT` is the single Rust-side fallback, shared
    // by both the runtime-patch path below and the static spawn-time
    // path further down.
    let disabled_opacity_default = el
        .attrs
        .disabled_opacity_default
        .unwrap_or(DISABLED_OPACITY_DEFAULT);
    // State-routed text-color / opacity / box-shadow swaps (`:hover` /
    // `:focus` / `:active` CSS). `:focus-visible` shares the focus slot.
    {
        let to_shadows = |v: &Option<Vec<lumen_ir::layout_ir::ShadowSpec>>| {
            v.as_ref()
                .map(|list| list.iter().copied().map(Into::into).collect())
        };
        // `:disabled` styling swaps at runtime only when the disabled
        // state can change at runtime - i.e. the element carries a
        // `bind-disabled` binding. Statically `disabled="true"` markup
        // (no binding) keeps the spawn-time fast path below; giving it a
        // runtime patch too would double-apply.
        let disabled_patch = if el.attrs.bind_disabled.is_some() {
            lumen_primitives::StatePatch {
                text_color: el.attrs.disabled_text_color.map(Into::into),
                // Default disabled look when no `:disabled { bg }` /
                // `:disabled { opacity }` rule supplied one: dim the
                // whole entity (mirrors the static spawn path).
                opacity: el.attrs.disabled_opacity.or({
                    if el.attrs.disabled_bg.is_none() && el.attrs.opacity.is_none() {
                        Some(disabled_opacity_default)
                    } else {
                        None
                    }
                }),
                shadows: to_shadows(&el.attrs.disabled_shadows),
                bg: el.attrs.disabled_bg.map(Into::into),
            }
        } else {
            lumen_primitives::StatePatch::default()
        };
        let sv = lumen_primitives::StateVisuals {
            hover: lumen_primitives::StatePatch {
                text_color: el.attrs.hover_text_color.map(Into::into),
                opacity: el.attrs.hover_opacity,
                shadows: to_shadows(&el.attrs.hover_shadows),
                bg: None,
            },
            focus: lumen_primitives::StatePatch {
                text_color: el.attrs.focus_text_color.map(Into::into),
                opacity: el.attrs.focus_opacity,
                shadows: to_shadows(&el.attrs.focus_shadows),
                bg: None,
            },
            focus_visible: lumen_primitives::StatePatch {
                text_color: el.attrs.focus_visible_text_color.map(Into::into),
                opacity: el.attrs.focus_visible_opacity,
                shadows: to_shadows(&el.attrs.focus_visible_shadows),
                bg: None,
            },
            active: lumen_primitives::StatePatch {
                text_color: el.attrs.active_text_color.map(Into::into),
                opacity: el.attrs.active_opacity,
                shadows: to_shadows(&el.attrs.active_shadows),
                bg: None,
            },
            drag_over: lumen_primitives::StatePatch {
                text_color: el.attrs.drag_over_text_color.map(Into::into),
                opacity: el.attrs.drag_over_opacity,
                shadows: to_shadows(&el.attrs.drag_over_shadows),
                bg: el.attrs.drag_over_bg.map(Into::into),
            },
            disabled: disabled_patch,
        };
        if !sv.is_empty() {
            // A state text-color swap needs a TextStyle slot to write
            // into even when the resting style authored none.
            let needs_text = sv.hover.text_color.is_some()
                || sv.focus.text_color.is_some()
                || sv.focus_visible.text_color.is_some()
                || sv.active.text_color.is_some()
                || sv.drag_over.text_color.is_some()
                || sv.disabled.text_color.is_some();
            if needs_text && entity.get::<TextStyle>().is_none() {
                entity.insert(TextStyle::default());
            }
            // A state shadow / background swap needs a Visuals to write
            // into.
            let needs_visuals = sv.hover.shadows.is_some()
                || sv.focus.shadows.is_some()
                || sv.focus_visible.shadows.is_some()
                || sv.active.shadows.is_some()
                || sv.drag_over.shadows.is_some()
                || sv.drag_over.bg.is_some()
                || sv.disabled.shadows.is_some()
                || sv.disabled.bg.is_some();
            if needs_visuals && entity.get::<Visuals>().is_none() {
                entity.insert(Visuals::default());
            }
            entity.insert(sv);
        }
    }
    if el.attrs.draggable {
        entity.insert(lumen_primitives::Draggable);
    }
    // Runtime-reactive disabled: `bind-disabled="signal"` mirrors the
    // bind-checked plumbing - `apply_disabled_bindings` (dirty-gated)
    // adds/removes the `Disabled` marker from the signal, and the
    // `:disabled` styling flows through the `StateVisuals.disabled`
    // patch populated above (so it reverses cleanly on re-enable).
    // Two-way scroll-offset binding (W6 T6): `bind-scroll="signal"` on a
    // scroll container. `apply_scroll_bindings` (dirty-gated) drives the
    // vertical offset from the signal; `push_scroll_to_signal` mirrors
    // user scrolling back on settle. The component is inert without a
    // co-resident `Scroll` + `ScrollOffset` (the `<scroll>` tag spawns
    // both).
    if let Some(name) = &el.attrs.bind_scroll {
        entity.insert(lumen_core::components::BindScroll(name.clone()));
    }
    if let Some(name) = &el.attrs.bind_disabled {
        entity.insert(lumen_core::components::BindDisabled(name.clone()));
        if el.attrs.disabled {
            // Initial `disabled="true"` alongside the binding: install
            // the marker now; the runtime patch styles it on tick 1 and
            // the signal takes authority from its first write.
            entity.insert(lumen_core::components::Disabled);
        }
    } else if el.attrs.disabled {
        entity.insert(lumen_core::components::Disabled);
        // `:disabled` text-color / opacity / box-shadow apply once at
        // spawn - without a binding the Disabled marker is static for
        // the entity's whole life.
        if let Some(c) = el.attrs.disabled_text_color {
            match entity.get_mut::<TextStyle>() {
                Some(mut ts) => ts.color = c.into(),
                None => {
                    entity.insert(TextStyle {
                        color: c.into(),
                        ..Default::default()
                    });
                }
            }
        }
        if let Some(o) = el.attrs.disabled_opacity {
            entity.insert(Opacity(o));
        }
        if let Some(shadows) = &el.attrs.disabled_shadows {
            let shadows: Vec<ShadowSpec> = shadows.iter().copied().map(Into::into).collect();
            match entity.get_mut::<Visuals>() {
                Some(mut v) => v.shadows = shadows,
                None => {
                    entity.insert(Visuals {
                        shadows,
                        ..Default::default()
                    });
                }
            }
        }
        if let Some(c) = el.attrs.disabled_bg {
            let fill = Some(Fill::Solid(c.into()));
            match entity.get_mut::<Visuals>() {
                Some(mut v) => v.fill = fill,
                None => {
                    entity.insert(Visuals {
                        fill,
                        radius: el.attrs.radius.unwrap_or(0.0),
                        corner_radii: el.attrs.radius_corners,
                        shadows: Vec::new(),
                        border: None,
                    });
                }
            }
        } else if el.attrs.opacity.is_none() && el.attrs.disabled_opacity.is_none() {
            // Default disabled look when no `:disabled { bg }` /
            // `:disabled { opacity }` rule supplied one: dim the whole
            // entity.
            entity.insert(Opacity(disabled_opacity_default));
        }
    }
    if let Some(axis) = el.attrs.scroll {
        let mut scroll = Scroll {
            axis: axis.into(),
            sensitivity: 1.0,
            inertia: 0.4,
            velocity: glam::Vec2::ZERO,
        };
        if let Some(s) = el.attrs.sensitivity {
            scroll.sensitivity = s;
        }
        if let Some(i) = el.attrs.inertia {
            scroll.inertia = i;
        }
        entity.insert((scroll, ScrollOffset::default()));
    } else {
        // CSS `overflow: scroll` promotes any element to a live scroll
        // container (web semantics): the walker already clips it, and
        // this component makes it wheel-/keyboard-scrollable. Skins use
        // it for the dropdown panel's max-visible-rows internal scroll
        // (`.dropdown-panel { max-height: ...; overflow-y: scroll; }`).
        use lumen_ir::layout_ir::OverflowSpec;
        let ox = el.attrs.overflow_x.or(el.attrs.overflow);
        let oy = el.attrs.overflow_y.or(el.attrs.overflow);
        let sx = matches!(ox, Some(OverflowSpec::Scroll));
        let sy = matches!(oy, Some(OverflowSpec::Scroll));
        if sx || sy {
            let axis = match (sx, sy) {
                (true, true) => lumen_core::input::ScrollAxis::Both,
                (true, false) => lumen_core::input::ScrollAxis::X,
                _ => lumen_core::input::ScrollAxis::Y,
            };
            let mut scroll = Scroll {
                axis,
                sensitivity: 1.0,
                inertia: 0.4,
                velocity: glam::Vec2::ZERO,
            };
            if let Some(s) = el.attrs.sensitivity {
                scroll.sensitivity = s;
            }
            if let Some(i) = el.attrs.inertia {
                scroll.inertia = i;
            }
            entity.insert((scroll, ScrollOffset::default()));
        }
    }
    // CSS `scrollbar-*` properties -> the runtime `ScrollbarStyle`.
    // Inserted whenever any one of them is authored (harmless on
    // non-scroll entities); the component's `Default` carries the
    // no-stylesheet fallback for everything not specified. The guard
    // used to only check `scrollbar_color` / `scrollbar_width`, so an
    // element authoring only e.g. `scrollbar-thickness` silently got no
    // `ScrollbarStyle` at all - the same silent-drop shape as the
    // `TextStyle` guard in `lumen_ir::convert`, just for this component;
    // every scrollbar field the cascade can produce is listed here now.
    //
    // `scrollbar-track-hover` / `scrollbar-hover-boost` are also
    // whitelisted for live reapply (`restyle::apply_reapplied_attrs`);
    // `scrollbar-thickness(-thin)`, `-margin`, `-min-thumb`, and the fade
    // timings are spawn-only (see that function's comment) - this is the
    // only place those ever get set, which is why they were the visible
    // gap.
    if el.attrs.scrollbar_color.is_some()
        || el.attrs.scrollbar_width.is_some()
        || el.attrs.scrollbar_thickness.is_some()
        || el.attrs.scrollbar_thickness_thin.is_some()
        || el.attrs.scrollbar_margin.is_some()
        || el.attrs.scrollbar_min_thumb.is_some()
        || el.attrs.scrollbar_track_hover.is_some()
        || el.attrs.scrollbar_hover_boost.is_some()
        || el.attrs.scrollbar_fade_delay_ms.is_some()
        || el.attrs.scrollbar_fade_duration_ms.is_some()
    {
        let mut sb = lumen_core::input::ScrollbarStyle::default();
        if let Some((thumb, track)) = el.attrs.scrollbar_color {
            sb.thumb = thumb.into();
            sb.track = track.map(Into::into);
        }
        if let Some(w) = el.attrs.scrollbar_width {
            sb.width = w.into();
        }
        if let Some(v) = el.attrs.scrollbar_thickness {
            sb.thickness = v;
        }
        if let Some(v) = el.attrs.scrollbar_thickness_thin {
            sb.thickness_thin = v;
        }
        if let Some(v) = el.attrs.scrollbar_margin {
            sb.margin = v;
        }
        if let Some(v) = el.attrs.scrollbar_min_thumb {
            sb.min_thumb = v;
        }
        if let Some(c) = el.attrs.scrollbar_track_hover {
            sb.hover_track = c.into();
        }
        if let Some(v) = el.attrs.scrollbar_hover_boost {
            sb.hover_boost = v;
        }
        // `ScrollbarStyle` stores fade timings in seconds; the CSS
        // properties parse as milliseconds (`Nms` / `Ns`, shared with
        // `transition-duration`), so both convert here at the single
        // point of consumption.
        if let Some(ms) = el.attrs.scrollbar_fade_delay_ms {
            sb.fade_delay_secs = ms as f32 / 1000.0;
        }
        if let Some(ms) = el.attrs.scrollbar_fade_duration_ms {
            sb.fade_secs = ms as f32 / 1000.0;
        }
        entity.insert(sb);
    }

    // Knob / thumb geometry: `knob-inset` / `thumb-size` (markup attrs or
    // the CSS properties) win; `KnobGeometry::default()`'s own
    // `KNOB_INSET` / `THUMB_SIZE` constants are the single Rust-side
    // fallback per field. Lives on the track entity itself (`<toggle>` /
    // `<switch>` / `<slider>`) - `sync_toggle_visuals` / `sync_switch_visuals`
    // / `sync_slider_thumb` read it off the same parent row they already
    // query for `Toggleable` / `SliderValue`.
    let knob_geometry = lumen_primitives::controls::KnobGeometry {
        inset: el
            .attrs
            .knob_inset
            .unwrap_or(lumen_primitives::controls::KNOB_INSET),
        thumb_size: el
            .attrs
            .thumb_size
            .unwrap_or(lumen_primitives::controls::THUMB_SIZE),
    };
    if matches!(el.tag.as_str(), "toggle" | "switch" | "slider") {
        entity.insert(knob_geometry);
    }
    let id = entity.id();
    // `<toggle>` / `<slider>` spawn a knob / thumb child so their state
    // is visible without author CSS. Both are absolute-positioned small
    // rounded tiles; `sync_toggle_visuals` / `sync_slider_thumb` in
    // lumen-primitives keep them in step with the control's state.
    // Knob / thumb fill: `knob-color` (markup attr or the CSS
    // `knob-color:` property, UA-seeded by the skins) wins; the runtime
    // `KNOB_FILL` constant is the single Rust-side fallback.
    let knob_fill: Color = el
        .attrs
        .knob_color
        .map(Into::into)
        .unwrap_or(lumen_primitives::controls::KNOB_FILL);
    if el.tag == "toggle" {
        world.spawn((
            knob_style(28.0, knob_geometry.inset),
            DirtyLayout,
            ChildOf(id),
            Visuals {
                fill: Some(Fill::Solid(knob_fill)),
                radius: 14.0,
                corner_radii: None,
                shadows: Vec::new(),
                border: None,
            },
            lumen_primitives::ToggleKnob,
        ));
    }
    if el.tag == "switch" {
        // Thumb seed: absolute-positioned tile parked at the off end.
        // `sync_switch_visuals` refines its size / radius / inset once the
        // track's laid-out rect is known and animates it on every flip.
        world.spawn((
            knob_style(20.0, knob_geometry.inset),
            DirtyLayout,
            ChildOf(id),
            Visuals {
                fill: Some(Fill::Solid(knob_fill)),
                radius: 10.0,
                corner_radii: None,
                shadows: Vec::new(),
                border: None,
            },
            lumen_primitives::SwitchThumb::default(),
        ));
    }
    if el.tag == "slider" {
        let thumb = knob_geometry.thumb_size;
        world.spawn((
            knob_style(thumb, 0.0),
            DirtyLayout,
            ChildOf(id),
            Visuals {
                fill: Some(Fill::Solid(knob_fill)),
                radius: thumb / 2.0,
                corner_radii: None,
                shadows: Vec::new(),
                border: None,
            },
            lumen_primitives::SliderThumb,
        ));
    }
    // `<for each="...">` blocks attach a [`ForMarker`] to the spawned entity
    // and DEFER child spawning to the reconciler - children are the
    // body template, instantiated per-item against the named
    // ArraySignals at runtime.
    if el.tag == "for"
        && let Some(name) = &el.attrs.each
    {
        world.entity_mut(id).insert(ForMarker {
            array_name: name.clone(),
            body: el.children.clone(),
            key_field: el.attrs.key.clone(),
            cached_keys: Vec::new(),
            virtualized: el.attrs.virtualized,
            row_height: el.attrs.row_height.unwrap_or(32.0),
            win_rows: Vec::new(),
            cascaded_body: None,
        });
        // Virtualized for-blocks pin rows via `position: absolute` against
        // this entity. Author-supplied flex defaults shrink it to content
        // width (= the absolute children, which then shrink to content
        // themselves, ad infinitum). Force width = 100% so the for-block
        // fills its scroll parent, giving rows a real reference rect.
        if el.attrs.virtualized
            && let Some(mut style) = world
                .entity_mut(id)
                .get_mut::<lumen_core::components::Style>()
        {
            style.width = lumen_core::components::Length::Percent(100.0);
        }
    } else if (el.tag == "if" || el.tag == "dialog")
        && let Some(name) = &el.attrs.if_signal
    {
        world.entity_mut(id).insert(IfMarker {
            signal_name: name.clone(),
            body: el.children.clone(),
            currently_mounted: false,
            mode: el.attrs.if_mode.into(),
            eq: el.attrs.if_eq.clone(),
            saved_display: lumen_core::components::Display::Flex,
            applied_visible: None,
        });
        if el.tag == "dialog" {
            world.entity_mut(id).insert((
                DialogMarker,
                lumen_core::components::FocusBoundary,
                // Modal overlay: paint the dialog subtree in the
                // top-layer band, over all normal content.
                lumen_core::render_world::OverlayLayer,
            ));
        }
    } else {
        for child in &el.children {
            spawn_element(world, child, Some(id));
        }
    }
    id
}

/// Reconcile every `<if>` block against the current [`PropertyStore`]
/// truthiness. Truthy: property exists AND value is not empty AND is not
/// literal `"false"` / `"0"`.
///
/// Mode policy:
///
/// * `IfMode::Render` (default) - despawn the body on falsy, respawn
///   on truthy. Cheap memory; loses focus / scroll / per-row signals
///   across the toggle.
/// * `IfMode::Hide` - mount the body once on the first truthy
///   transition. Hiding flows through `Style.display = Display::None`
///   on the if-marker entity (spec section 17.4: space is released, siblings
///   reflow on the next tick via `Changed<Style>`) and additionally
///   stamps `Visible(false)` so the render extract, hit-test, and
///   focus paths skip the subtree while all descendant state (focus,
///   scroll, per-row signals) survives. Both writes happen only on an
///   actual transition (D5) - steady ticks touch nothing, so
///   `FrameDirty` stays clear.
pub fn reconcile_if_blocks(
    store: bevy_ecs::system::Res<lumen_core::property_store::PropertyStore>,
    mut markers: bevy_ecs::system::Query<(
        Entity,
        &mut IfMarker,
        Option<&bevy_ecs::hierarchy::Children>,
    )>,
    mut styles: bevy_ecs::system::Query<&mut lumen_core::components::Style>,
    fade_targets: bevy_ecs::system::Query<(
        Option<&lumen_primitives::TransitionSpecs>,
        Option<&lumen_core::components::Opacity>,
    )>,
    mut commands: bevy_ecs::system::Commands,
) {
    for (parent_id, mut marker, children) in markers.iter_mut() {
        let value: Option<std::sync::Arc<str>> = store.get_global_str(&marker.signal_name);
        let value_str: Option<&str> = value.as_deref();
        let now_truthy = match &marker.eq {
            Some(expected) => value_str == Some(expected.as_str()),
            None => value_str.is_some_and(signal_is_truthy),
        };
        match marker.mode {
            IfMode::Render => {
                if now_truthy == marker.currently_mounted {
                    continue;
                }
                if now_truthy {
                    for tmpl in &marker.body {
                        spawn_body_child(&mut commands, tmpl, parent_id);
                    }
                    // Entering transition on the block itself (children
                    // handle their own via `spawn_body_child`).
                    if let Ok((specs, cur)) = fade_targets.get(parent_id)
                        && let Some(bundle) = mount_fade_bundle(specs, cur.copied())
                    {
                        commands.entity(parent_id).insert(bundle);
                    }
                    marker.currently_mounted = true;
                } else {
                    if let Some(kids) = children {
                        for child in kids.iter() {
                            commands.entity(child).despawn();
                        }
                    }
                    marker.currently_mounted = false;
                }
            }
            IfMode::Hide => {
                // First-time mount happens on the first truthy tick:
                // spawn the subtree once; subsequent toggles only flip
                // visibility state.
                if now_truthy && !marker.currently_mounted {
                    for tmpl in &marker.body {
                        spawn_body_child(&mut commands, tmpl, parent_id);
                    }
                    marker.currently_mounted = true;
                }
                // D5: apply visibility only on a transition. The
                // unconditional per-tick re-insert kept Changed<Visible>
                // (and thus FrameDirty) permanently hot.
                if marker.applied_visible == Some(now_truthy) {
                    continue;
                }
                marker.applied_visible = Some(now_truthy);
                commands
                    .entity(parent_id)
                    .insert(lumen_core::components::Visible(now_truthy));
                // Entering transition on show (mount-direction only -
                // hide stays instant; CSS can't run removal transitions
                // without JS either). Check the `<if>` entity itself
                // (e.g. `<dialog>`) and its mounted body roots (e.g.
                // the synthetic dropdown / menu `.dropdown-panel`
                // children, which carry the CSS transition specs).
                if now_truthy {
                    let mut targets: Vec<Entity> = vec![parent_id];
                    if let Some(kids) = children {
                        targets.extend(kids.iter());
                    }
                    for target in targets {
                        if let Ok((specs, cur)) = fade_targets.get(target)
                            && let Some(bundle) = mount_fade_bundle(specs, cur.copied())
                        {
                            commands.entity(target).insert(bundle);
                        }
                    }
                }
                // section 17.4: hide releases the block's layout space via
                // Display::None; show restores the prior display. The
                // Style mutation trips Changed<Style> -> DirtyLayout, so
                // siblings reflow on this same tick's LayoutSync.
                if let Ok(mut style) = styles.get_mut(parent_id) {
                    use lumen_core::components::Display;
                    if now_truthy {
                        if matches!(style.display, Display::None) {
                            style.display = marker.saved_display;
                        }
                    } else if !matches!(style.display, Display::None) {
                        marker.saved_display = style.display;
                        style.display = Display::None;
                    }
                }
            }
        }
    }
}

/// On Escape, close the top-most open popup layer - one press peels off
/// exactly one layer.
///
/// Priority mirrors paint order (later-painted popups sit on top):
///
/// 1. an open `<dropdown>` panel (`__dropdown_open:*` signal is `true`),
/// 2. otherwise an open `<menu>` panel (`__menu_open:*` signal is `true`),
/// 3. otherwise the top-most open `<dialog>`.
///
/// Dropdown / menu panels are gated by `<if eq="true" mode="hide">`
/// blocks whose [`IfMarker::signal_name`] is the synthetic open signal,
/// so the same `IfMarker` query that drives dialogs finds them too -
/// no dependency on the panel's runtime marker components (which the
/// `<if>`-body template path does not propagate). Closing writes the
/// open signal to `false` (dropdown / menu) or `""` (dialog); the next
/// `reconcile_if_blocks` tick flips `Visible(false)` on the subtree
/// (`IfMode::Hide` preserves child state across the toggle).
///
/// Bare Esc keypresses with nothing open pass through untouched.
///
/// An Escape that cancelled an in-flight press
/// (`lumen_input::cancel_press_on_escape` sets
/// [`EscapePressCancel`](lumen_core::input::EscapePressCancel) earlier
/// in the Input stage) is consumed by the cancel - the dialog stays
/// open on that keystroke.
pub fn close_dialogs_on_escape(
    mut keys: bevy_ecs::message::MessageReader<lumen_core::input::KeyPressed>,
    press_cancel: Option<bevy_ecs::system::Res<lumen_core::input::EscapePressCancel>>,
    mut store: bevy_ecs::system::ResMut<lumen_core::property_store::PropertyStore>,
    blocks: bevy_ecs::system::Query<(
        &IfMarker,
        Option<&lumen_core::components::Visible>,
        Option<&DialogMarker>,
    )>,
) {
    let escape_pressed = keys.read().any(|k| {
        matches!(
            &k.key,
            lumen_core::input::Key::Named(lumen_core::input::NamedKey::Escape)
        )
    });
    if !escape_pressed {
        return;
    }
    if press_cancel.is_some_and(|f| f.0) {
        return;
    }

    // 1. Top-most layer: an open dropdown panel.
    for (marker, _visible, _dialog) in &blocks {
        if marker.signal_name.starts_with("__dropdown_open:")
            && store.get_global_bool(&marker.signal_name) == Some(true)
        {
            store.set_global_bool(&marker.signal_name, false);
            return;
        }
    }
    // 2. Next: an open menu panel.
    for (marker, _visible, _dialog) in &blocks {
        if marker.signal_name.starts_with("__menu_open:")
            && store.get_global_bool(&marker.signal_name) == Some(true)
        {
            store.set_global_bool(&marker.signal_name, false);
            return;
        }
    }
    // 3. Finally: the top-most open `<dialog>`.
    for (marker, visible, dialog) in &blocks {
        if dialog.is_none() {
            continue;
        }
        let open = visible.map(|v| v.0).unwrap_or(true);
        if !open {
            continue;
        }
        let curr = store.get_global_str(&marker.signal_name);
        if curr.as_deref() != Some("") {
            store.set_global_str(&marker.signal_name, "");
            return;
        }
    }
}

/// Collect every descendant of `root` (excluding `root` itself),
/// depth-first via `Children`.
fn collect_descendants(
    root: Entity,
    children: &bevy_ecs::system::Query<&bevy_ecs::hierarchy::Children>,
    out: &mut Vec<Entity>,
) {
    if let Ok(kids) = children.get(root) {
        for kid in kids.iter() {
            out.push(kid);
            collect_descendants(kid, children, out);
        }
    }
}

/// Qt QDialog lifecycle (W5): initial focus on open, focus restore on
/// close, and the exactly-once accepted/rejected close event.
///
/// Open edge (`Visible` flips true):
/// - remember the current focus holder,
/// - focus the first `autofocus` descendant; else the first focusable
///   (`TabIndex >= 0`, enabled) descendant in markup order; else the
///   dialog panel itself. The body mounts via deferred commands one
///   tick after the edge, so the pass retries (`needs_focus`) until a
///   target exists.
///
/// Close edge:
/// - emit exactly one [`lumen_core::input::DialogClosed`] -
///   `accepted` when the default button drove the close
///   ([`DialogSession::pending_accept`]), `rejected` otherwise
///   (Escape, cancel buttons, script writes),
/// - restore focus to the pre-open holder when focus still sits
///   inside the dialog subtree (a close caused by focusing another
///   widget doesn't yank focus back) - mirrors
///   `lumen_primitives::popup_nav_lifecycle`.
///
/// The Tab focus trap itself is `lumen_input::cycle_focus_on_tab` +
/// the `FocusBoundary` the spawner puts on every `<dialog>`.
#[allow(clippy::too_many_arguments, clippy::type_complexity)]
pub fn manage_dialog_lifecycle(
    mut commands: bevy_ecs::system::Commands,
    mut tracker: bevy_ecs::system::ResMut<lumen_core::input::FocusTracker>,
    mut dialogs: bevy_ecs::system::Query<
        (
            Entity,
            Option<&lumen_core::components::Visible>,
            Option<&mut DialogSession>,
            Option<&lumen_core::components::LumenId>,
            &IfMarker,
        ),
        With<DialogMarker>,
    >,
    children: bevy_ecs::system::Query<&bevy_ecs::hierarchy::Children>,
    parents: bevy_ecs::system::Query<&bevy_ecs::hierarchy::ChildOf>,
    focusables: bevy_ecs::system::Query<
        (
            &lumen_core::components::TabIndex,
            Option<&lumen_core::components::DocumentOrder>,
        ),
        Without<lumen_core::components::Disabled>,
    >,
    autofocused: bevy_ecs::system::Query<
        Option<&lumen_core::components::DocumentOrder>,
        (With<AutoFocus>, Without<lumen_core::components::Disabled>),
    >,
    focus_visibles: bevy_ecs::system::Query<(), With<lumen_core::input::FocusVisible>>,
    live: bevy_ecs::system::Query<Entity, Without<bevy_ecs::resource::IsResource>>,
    mut closed_out: bevy_ecs::message::MessageWriter<lumen_core::input::DialogClosed>,
) {
    for (dialog_e, visible, session, lumen_id, marker) in dialogs.iter_mut() {
        let open = visible.map(|v| v.0).unwrap_or(false);
        let Some(mut session) = session else {
            // First sighting: seed the tracker WITHOUT treating the
            // initial state as an edge (a dialog that spawns closed
            // must not fire `rejected`; one that spawns open skips the
            // focus grab - it was never interactively opened).
            commands.entity(dialog_e).insert(DialogSession {
                open,
                ..Default::default()
            });
            continue;
        };
        if open && !session.open {
            // Open edge.
            session.open = true;
            session.needs_focus = true;
            session.pending_accept = false;
            session.prev_focus = tracker.0;
            session.prev_focus_visible = tracker
                .0
                .map(|e| focus_visibles.contains(e))
                .unwrap_or(false);
        } else if !open && session.open {
            // Close edge: exactly one accepted-or-rejected per cycle.
            session.open = false;
            session.needs_focus = false;
            let id = lumen_id
                .map(|i| i.0.clone())
                .unwrap_or_else(|| marker.signal_name.clone());
            closed_out.write(lumen_core::input::DialogClosed {
                entity: dialog_e,
                id,
                accepted: session.pending_accept,
            });
            session.pending_accept = false;
            // Focus restore - only when focus still sits inside the
            // dialog subtree (or on the dialog itself / nowhere).
            let focus_inside = match tracker.0 {
                None => true,
                Some(cur) => {
                    let mut e = cur;
                    loop {
                        if e == dialog_e {
                            break true;
                        }
                        match parents.get(e) {
                            Ok(co) => e = co.parent(),
                            Err(_) => break false,
                        }
                    }
                }
            };
            if focus_inside {
                if let Some(cur) = tracker.0 {
                    commands
                        .entity(cur)
                        .remove::<(lumen_core::input::Focused, lumen_core::input::FocusVisible)>();
                }
                match session.prev_focus.filter(|e| live.get(*e).is_ok()) {
                    Some(prev) => {
                        commands.entity(prev).insert(lumen_core::input::Focused);
                        if session.prev_focus_visible {
                            commands
                                .entity(prev)
                                .insert(lumen_core::input::FocusVisible);
                        }
                        tracker.0 = Some(prev);
                    }
                    None => {
                        tracker.0 = None;
                    }
                }
            }
        }
        if session.open && session.needs_focus {
            // Initial-focus pass: retried until the deferred body
            // mount produced a target.
            let mut descendants = Vec::new();
            collect_descendants(dialog_e, &children, &mut descendants);
            if descendants.is_empty() {
                continue; // body not mounted yet - retry next tick
            }
            let pick = |pred: &mut dyn FnMut(Entity) -> Option<u32>| -> Option<Entity> {
                let mut best: Option<(u32, Entity)> = None;
                for &d in &descendants {
                    if let Some(order) = pred(d) {
                        let candidate = (order, d);
                        if best.map(|b| candidate < b).unwrap_or(true) {
                            best = Some(candidate);
                        }
                    }
                }
                best.map(|(_, e)| e)
            };
            let target = pick(&mut |e| {
                autofocused
                    .get(e)
                    .ok()
                    .map(|doc| doc.map(|d| d.0).unwrap_or(u32::MAX))
            })
            .or_else(|| {
                pick(&mut |e| {
                    focusables
                        .get(e)
                        .ok()
                        .filter(|(ti, _)| ti.0 >= 0)
                        .map(|(_, doc)| doc.map(|d| d.0).unwrap_or(u32::MAX))
                })
            })
            .unwrap_or(dialog_e);
            if let Some(cur) = tracker.0
                && cur != target
            {
                commands
                    .entity(cur)
                    .remove::<(lumen_core::input::Focused, lumen_core::input::FocusVisible)>();
            }
            commands.entity(target).insert(lumen_core::input::Focused);
            tracker.0 = Some(target);
            session.needs_focus = false;
        }
    }
}

/// Enter anywhere in an open `<dialog>` activates its DEFAULT button
/// (Qt QDialog default-button contract) - unless focus sits on a
/// button (its own Enter->click path wins) or on a multiline text
/// input (Enter inserts a newline). Single-line inputs DO trigger the
/// default (Qt/web: Enter in a line edit fires the default button)
/// alongside their own commit event.
///
/// The default button is the first enabled [`DefaultButton`]
/// descendant; with none marked, the first enabled `<button>` in
/// markup order (Qt autoDefault). The synthesized [`ClickEvent`] flows
/// through the normal click dispatchers, so scripts observe a regular
/// `on_click`. Activation through THIS path always marks the close
/// `accepted` ([`DialogSession::pending_accept`]); direct pointer
/// clicks mark it only for an explicitly `default="true"` button
/// ([`mark_dialog_accept_on_default_click`]).
#[allow(clippy::too_many_arguments, clippy::type_complexity)]
pub fn activate_dialog_default_on_enter(
    mut keys: bevy_ecs::message::MessageReader<lumen_core::input::KeyPressed>,
    tracker: bevy_ecs::system::Res<lumen_core::input::FocusTracker>,
    mut dialogs: bevy_ecs::system::Query<
        (
            Entity,
            Option<&lumen_core::components::Visible>,
            Option<&mut DialogSession>,
        ),
        With<DialogMarker>,
    >,
    children: bevy_ecs::system::Query<&bevy_ecs::hierarchy::Children>,
    tags: bevy_ecs::system::Query<&lumen_core::components::LumenTag>,
    inputs: bevy_ecs::system::Query<&lumen_core::components::TextInput>,
    defaults: bevy_ecs::system::Query<(), With<DefaultButton>>,
    disabled: bevy_ecs::system::Query<(), With<lumen_core::components::Disabled>>,
    orders: bevy_ecs::system::Query<&lumen_core::components::DocumentOrder>,
    mut clicks: bevy_ecs::message::MessageWriter<lumen_core::input::ClickEvent>,
) {
    let enter = keys.read().any(|k| {
        !k.repeat
            && matches!(
                &k.key,
                lumen_core::input::Key::Named(lumen_core::input::NamedKey::Enter)
            )
    });
    if !enter {
        return;
    }
    // Focus exclusions: a focused button consumes Enter itself; a
    // multiline input inserts a newline.
    if let Some(focused) = tracker.0 {
        if tags.get(focused).is_ok_and(|t| t.0.as_ref() == "button") {
            return;
        }
        if inputs.get(focused).is_ok_and(|i| i.multiline) {
            return;
        }
    }
    // Top-most open dialog.
    let Some((dialog_e, session)) = dialogs
        .iter_mut()
        .filter(|(_, vis, _)| vis.map(|v| v.0).unwrap_or(false))
        .map(|(e, _, s)| (e, s))
        .next()
    else {
        return;
    };
    let mut descendants = Vec::new();
    collect_descendants(dialog_e, &children, &mut descendants);
    let best_by_order = |pred: &dyn Fn(Entity) -> bool| -> Option<Entity> {
        descendants
            .iter()
            .copied()
            .filter(|&e| pred(e) && !disabled.contains(e))
            .min_by_key(|&e| (orders.get(e).map(|d| d.0).unwrap_or(u32::MAX), e))
    };
    let target = best_by_order(&|e| defaults.contains(e))
        .or_else(|| best_by_order(&|e| tags.get(e).is_ok_and(|t| t.0.as_ref() == "button")));
    let Some(target) = target else {
        return;
    };
    if let Some(mut session) = session {
        session.pending_accept = true;
    }
    clicks.write(lumen_core::input::ClickEvent {
        entity: target,
        position: glam::Vec2::ZERO,
        button: lumen_core::input::PointerButton::Primary,
    });
}

/// A direct click on an explicitly-marked [`DefaultButton`] inside an
/// open dialog resolves that dialog's eventual close as `accepted`.
/// Text-child hit-shadowing is handled with the same ancestor walk the
/// control dispatchers use.
pub fn mark_dialog_accept_on_default_click(
    mut clicks: bevy_ecs::message::MessageReader<lumen_core::input::ClickEvent>,
    defaults: bevy_ecs::system::Query<(), With<DefaultButton>>,
    parents: bevy_ecs::system::Query<&bevy_ecs::hierarchy::ChildOf>,
    mut sessions: bevy_ecs::system::Query<&mut DialogSession, With<DialogMarker>>,
) {
    for ev in clicks.read() {
        // Resolve the clicked entity to a DefaultButton (self or
        // ancestor), then keep walking to the containing dialog.
        let mut cur = Some(ev.entity);
        let mut hit_default = false;
        while let Some(e) = cur {
            if !hit_default && defaults.contains(e) {
                hit_default = true;
            }
            if hit_default && let Ok(mut session) = sessions.get_mut(e) {
                session.pending_accept = true;
                break;
            }
            cur = parents.get(e).ok().map(|c| c.parent());
        }
    }
}

/// Reconcile every `<for>` block against the current
/// [`ArraySignals`][lumen_core::signals::ArraySignals] state.
///
/// **Reconciliation policy (alpha7):**
/// * Equal key sequence -> no-op.
/// * New keys are an extension of cached (cached is a prefix of new) ->
///   spawn only the appended items. Common case: `signal_array.push(...)`
///   adds one item, only the new row spawns; existing rows keep their
///   focus / scroll / `Signals` state.
/// * Cached keys are an extension of new (new is a prefix of cached) ->
///   despawn only the trailing entities. Common case: pop / clear-tail.
/// * Anything else (reorder, mid-insert, mid-remove) -> full rebuild.
///   Real keyed reconciliation across arbitrary reorders requires
///   re-anchoring `ChildOf` in bevy_ecs and lands as a follow-up.
#[allow(clippy::too_many_arguments)]
pub fn reconcile_for_blocks(
    array_signals: bevy_ecs::system::Res<lumen_core::signals::ArraySignals>,
    // Wave-D: `InterpolationSlot::Global(name)` substitutions read the
    // canonical typed PropertyStore so `{$theme}` inside a `<for>` body
    // resolves correctly. Pre wave-D this consulted the `Signals` resource;
    // every internal write now lands on PropertyStore so reading either
    // layer would yield the same result, but PropertyStore is the
    // canonical source post wave-D.
    store: bevy_ecs::system::Res<lumen_core::property_store::PropertyStore>,
    mut markers: bevy_ecs::system::Query<(
        Entity,
        &mut ForMarker,
        Option<&bevy_ecs::hierarchy::Children>,
    )>,
    mut commands: bevy_ecs::system::Commands,
    world_helper: bevy_ecs::system::ParamSet<(
        bevy_ecs::system::Query<&bevy_ecs::hierarchy::Children>,
    )>,
    // Walk the [`ChildOf`] chain to locate the nearest [`Scroll`] ancestor's [`ScrollOffset`] and [`Transform`].
    // Virtualised for-blocks consult them to spawn only rows in the visible band; plain for-blocks
    // ignore these queries entirely.
    parents: bevy_ecs::system::Query<&bevy_ecs::hierarchy::ChildOf>,
    scroll_state: bevy_ecs::system::Query<
        (
            &lumen_core::input::ScrollOffset,
            &lumen_core::components::Transform,
        ),
        With<lumen_core::input::Scroll>,
    >,
    styles: bevy_ecs::system::Query<&lumen_core::components::Style>,
    stylesheet: Option<bevy_ecs::system::Res<LumenStylesheet>>,
    policy: Option<bevy_ecs::system::Res<ScenePolicy>>,
) {
    let _ = world_helper;
    let policy = policy.map(|p| *p).unwrap_or_default();
    let css_changed = stylesheet.as_ref().map(|s| s.is_changed()).unwrap_or(false);
    // Under `RowStyle::HostStyled` the rows reach a cascade of their own, so
    // every substitution below hands the template on unresolved.
    let row_css = match policy.row_style {
        RowStyle::Cascade => stylesheet.as_ref().map(|s| &s.0),
        RowStyle::HostStyled => None,
    };
    for (parent_id, mut marker, children) in markers.iter_mut() {
        // Borrow the array in place - the old `.to_vec()` deep-cloned
        // every `ArrayItem` HashMap (5 000 rows x per-field Strings) on
        // EVERY tick, dominating the idle reconcile cost on big grids.
        let items: &[ArrayItem] = array_signals
            .get(&marker.array_name)
            .unwrap_or(&[] as &[ArrayItem]);

        // Virtualised branch: derive the visible row range from the nearest `<scroll>` ancestor's [`ScrollOffset`] and [`Transform`],
        // spawning only rows in that window plus an overscan buffer above
        // and below, and absolute-position each row at
        // `top = row_index * row_height` so layout stays predictable
        // regardless of which subset is mounted. Author opts in via
        // `<for virtualized="true" row-height="N">`.
        //
        // Spec section 15.3 windowed reuse: a window shift keeps every row whose
        // `(index, key)` is unchanged - only rows entering the window
        // spawn and rows leaving it despawn. Pre-fix, every 1-row shift
        // despawned + respawned the ENTIRE band (with a full per-row CSS
        // cascade), which is what made 5k-row wheel scrolling lag.
        if marker.virtualized && policy.virtualization == Virtualization::Enabled {
            let row_h = marker.row_height.max(1.0);
            // Walk ChildOf upward from the for-block looking for an
            // ancestor with a `Scroll` component. Read its scroll
            // offset + viewport height so the windowing math knows
            // where the user is looking. No scroll ancestor = window
            // anchored at the top with the viewport defaulting to a
            // tall default so apps still see some rows.
            let mut anc = parents.get(parent_id).ok().map(|c| c.parent());
            let mut offset_y = 0.0_f32;
            let mut viewport_h = 600.0_f32;
            while let Some(e) = anc {
                if let Ok((off, t)) = scroll_state.get(e) {
                    offset_y = off.0.y.max(0.0);
                    viewport_h = t.size.y.max(0.0);
                    break;
                }
                anc = parents.get(e).ok().map(|c| c.parent());
            }
            // Overscan: rows mounted beyond both window edges so a small
            // shift lands on already-mounted rows and the per-frame mount
            // cost amortises across several frames of scrolling.
            //
            // Considered and rejected (wave-3 scroll-p95 work): quantizing
            // the window to N-row pages to batch mounts. Measured on the
            // 10k-row bench it left p95 unchanged (per-row mount spikes
            // were never the p95 tail - external drive-loop jitter is)
            // and regressed p99 ~18.2 -> ~20.5 ms because the batched
            // 12-row mount tick costs more than any per-row tick. Per-row
            // shifting keeps the worst single tick small.
            const BUFFER_ROWS: usize = 8;
            let first = ((offset_y / row_h).floor() as i64 - BUFFER_ROWS as i64).max(0) as usize;
            let last =
                (((offset_y + viewport_h) / row_h).ceil() as usize + BUFFER_ROWS).min(items.len());

            // Pin the for-block's height to total_rows * row_h so taffy
            // gives the scroll content the right extent. Without this
            // the for-block sizes to 0 (no flex children - every row is
            // position: absolute) and rows escape upward into the
            // scroll's siblings (e.g. the toolbar/header) because the
            // containing block has no height to clip against. Preserve
            // every other Style field via copy-then-mutate so author
            // CSS on the for-block (padding, etc.) stays intact.
            let content_h = items.len() as f32 * row_h;
            let mut new_style = styles.get(parent_id).cloned().unwrap_or_default();
            new_style.width = lumen_core::components::Length::Percent(100.0);
            new_style.height = lumen_core::components::Length::Px(content_h);
            // Taffy's default `flex_shrink: 1.0` clamps the for-block
            // down to the parent scroll viewport even when we set an
            // explicit `height: content_h`. Pin `min_height` to the
            // same value so Taffy can't shrink it - that's what gives
            // the scroll a content extent larger than the viewport
            // (without it the user can't scroll down past viewport).
            new_style.min_height = lumen_core::components::Length::Px(content_h);
            // D4: only insert when the computed style actually differs.
            // The unconditional insert sat *before* the cache-signature
            // early-continue below, so every steady tick re-triggered
            // Changed<Style> -> DirtyLayout -> relayout -> FrameDirty - a
            // permanent per-tick relayout/repaint loop on virtualized
            // for-blocks.
            if styles
                .get(parent_id)
                .map(|s| *s != new_style)
                .unwrap_or(true)
            {
                commands.entity(parent_id).insert(new_style);
            }

            // Desired `(index, key)` pairs for the current window. Keys
            // are built for WINDOW rows only - never all 5 000.
            let key_field = marker.key_field.clone();
            let key_of = |i: usize, item: &ArrayItem| -> String {
                key_field
                    .as_ref()
                    .and_then(|f| item.get(f).cloned())
                    .unwrap_or_else(|| i.to_string())
            };
            let desired: Vec<(usize, String)> = items
                .iter()
                .enumerate()
                .skip(first)
                .take(last.saturating_sub(first))
                .map(|(i, item)| (i, key_of(i, item)))
                .collect();

            // Stylesheet changed (hot reload / first insert): drop the
            // cascaded-template cache so rows restyle on next spawn.
            if css_changed {
                marker.cascaded_body = None;
            }

            // Steady state: same window membership -> nothing to do.
            // NOTE: `win_rows` is in MOUNT order (keep-path retains rows
            // in their existing child-slice order, new rows append), which
            // permanently diverges from `desired`'s index order after any
            // scroll that pulls rows in at the front - an ordered `==`
            // here would never match again and the diff below would run
            // every tick forever. Compare membership order-insensitively;
            // `win_rows` itself must stay in mount order because the
            // keep-path slices the children list by its positions.
            if children.is_some() && desired.len() == marker.win_rows.len() {
                let mut mounted_sorted = marker.win_rows.clone();
                mounted_sorted.sort_unstable_by_key(|(idx, _)| *idx);
                if mounted_sorted == desired {
                    continue;
                }
            }

            let body_len = marker.body.len().max(1);
            let kids: Vec<Entity> = children.map(|c| c.iter().collect()).unwrap_or_default();
            // Every mounted row owns `body_len` consecutive children in
            // spawn order. If something else despawned a child out from
            // under us, alignment is gone - fall back to a full rebuild.
            let aligned = kids.len() == marker.win_rows.len() * body_len;

            // Rows to keep: `(index, key)` present in both the mounted
            // set and the desired window.
            let desired_by_idx: std::collections::HashMap<usize, &str> =
                desired.iter().map(|(i, k)| (*i, k.as_str())).collect();
            let mut new_win: Vec<(usize, String)> = Vec::new();
            if aligned {
                for (row_i, (idx, key)) in marker.win_rows.iter().enumerate() {
                    let keep = desired_by_idx.get(idx) == Some(&key.as_str());
                    let slice = &kids[row_i * body_len..(row_i + 1) * body_len];
                    if keep {
                        new_win.push((*idx, key.clone()));
                    } else {
                        for child in slice {
                            commands.entity(*child).despawn();
                        }
                    }
                }
            } else {
                for child in &kids {
                    commands.entity(*child).despawn();
                }
            }
            let mounted: std::collections::HashSet<usize> =
                new_win.iter().map(|(i, _)| *i).collect();

            // Cascade-once-per-template (spec section 15.3): apply the CSS
            // cascade to the body template a single time and substitute
            // per-row placeholders into the pre-cascaded clone. Falls
            // back to per-row cascade when `id` / `class` attrs carry
            // `{...}` placeholders (selector matching would then depend on
            // the substituted values).
            let per_row_cascade = body_has_dynamic_selector_attrs(&marker.body);
            let template: std::sync::Arc<Vec<Element>> = match (per_row_cascade, row_css) {
                (true, _) | (false, None) => std::sync::Arc::new(marker.body.clone()),
                (false, Some(_)) if marker.cascaded_body.is_some() => {
                    marker.cascaded_body.clone().expect("checked above")
                }
                (false, Some(sheet)) => {
                    let cascaded: Vec<Element> = marker
                        .body
                        .iter()
                        .map(|el| {
                            let mut c = el.clone();
                            cascade_element_tree(&mut c, sheet);
                            c
                        })
                        .collect();
                    let arc = std::sync::Arc::new(cascaded);
                    marker.cascaded_body = Some(arc.clone());
                    arc
                }
            };

            for (i, key) in &desired {
                if mounted.contains(i) {
                    continue;
                }
                let item = &items[*i];
                let ctx = RowCtx {
                    item,
                    idx: *i,
                    store: &store,
                    parent_id,
                };
                for tmpl in template.iter() {
                    // Substitute placeholders; run the cascade only on
                    // the per-row fallback path (pre-cascaded otherwise).
                    let mut inst = substitute_in_element_with_css(
                        tmpl,
                        &ctx,
                        if per_row_cascade { row_css } else { None },
                    );
                    // Override the row's positioning so it lands at the
                    // correct absolute slot inside the for-block.
                    // Pin the row vertically at `top = i * row_h` and
                    // left = 0; let `width=100%` + `height=row_h` set
                    // dims explicitly. Setting `right` / `bottom` here
                    // would over-constrain taffy (it expects either
                    // inset OR size, not both on the same axis) and the
                    // row collapses to content width.
                    inst.attrs.position = Some(lumen_ir::layout_ir::PositionSpec::Absolute);
                    inst.attrs.inset = Some(lumen_ir::layout_ir::Edges {
                        top: *i as f32 * row_h,
                        right: f32::NAN,
                        bottom: f32::NAN,
                        left: 0.0,
                        ..lumen_ir::layout_ir::Edges::default()
                    });
                    inst.attrs.height = Some(lumen_ir::layout_ir::LengthSpec::Px(row_h));
                    inst.attrs.width = Some(lumen_ir::layout_ir::LengthSpec::Percent(100.0));
                    spawn_body_child(&mut commands, &inst, parent_id);
                }
                new_win.push((*i, key.clone()));
            }
            marker.win_rows = new_win;
            continue;
        }

        // Build the new key list. If `key_field` is set, pull that field
        // from each record; otherwise fall back to the item's index.
        let new_keys: Vec<String> = items
            .iter()
            .enumerate()
            .map(|(i, item)| {
                marker
                    .key_field
                    .as_ref()
                    .and_then(|f| item.get(f).cloned())
                    .unwrap_or_else(|| i.to_string())
            })
            .collect();

        if new_keys == marker.cached_keys && children.is_some() {
            continue;
        }

        let body_len = marker.body.len();
        let cached = marker.cached_keys.clone();

        // Append-only: new = cached + tail. Spawn just the tail.
        if cached.len() < new_keys.len() && new_keys[..cached.len()] == cached[..] {
            for (i, item) in items.iter().enumerate().skip(cached.len()) {
                let ctx = RowCtx {
                    item,
                    idx: i,
                    store: &store,
                    parent_id,
                };
                for tmpl in &marker.body {
                    let inst = substitute_in_element_with_css(tmpl, &ctx, row_css);
                    spawn_body_child(&mut commands, &inst, parent_id);
                }
            }
            marker.cached_keys = new_keys;
            continue;
        }

        // Trim-from-tail: cached = new + tail. Despawn the tail
        // entities (`body_len` per popped item, last-spawn-first).
        if new_keys.len() < cached.len() && cached[..new_keys.len()] == new_keys[..] {
            if let Some(kids) = children {
                let kids: Vec<Entity> = kids.iter().collect();
                let drop_count = (cached.len() - new_keys.len()) * body_len;
                for child in kids.iter().rev().take(drop_count) {
                    commands.entity(*child).despawn();
                }
            }
            marker.cached_keys = new_keys;
            continue;
        }

        // Fallback: full rebuild. Despawn all children and spawn fresh.
        // Loses focus / scroll / per-row signal state - same caveat as
        // pre-alpha7. Triggered only when the diff isn't a clean append or
        // trim (e.g. midstream insert, reorder, replace).
        if let Some(kids) = children {
            for child in kids.iter() {
                commands.entity(child).despawn();
            }
        }
        for (i, item) in items.iter().enumerate() {
            let ctx = RowCtx {
                item,
                idx: i,
                store: &store,
                parent_id,
            };
            for tmpl in &marker.body {
                let inst = substitute_in_element_with_css(tmpl, &ctx, row_css);
                spawn_body_child(&mut commands, &inst, parent_id);
            }
        }
        marker.cached_keys = new_keys;
    }
}

/// Row substitution context bundled together so the wave-C placeholder
/// walker can resolve each [`InterpolationSlot`] against the right
/// scope without threading 5 arguments through every recursion step.
///
/// - `item` - the current iteration's row record. Field lookups via
///   `InterpolationSlot::Row(field)` hit this map.
/// - `idx` - 0-based iteration index, stringified for `RowIndex`.
/// - `store` - global property store. `InterpolationSlot::Global` reads
///   from here (wave-D - pre wave-D this was the legacy `Signals` resource).
/// - `parent_id` - the for-block entity, used only as the
///   one-shot-warn discriminator for missing row fields. We don't have
///   a per-row entity id at substitution time (the row hasn't spawned
///   yet), so the for-block entity stands in.
struct RowCtx<'a> {
    item: &'a lumen_core::signals::ArrayItem,
    idx: usize,
    store: &'a lumen_core::property_store::PropertyStore,
    parent_id: Entity,
}

/// Walk an [`Element`] subtree, replacing every `{...}` placeholder in
/// its attribute string values and inline text with the resolved
/// value from the appropriate scope (iteration row, row index, global
/// signal, ...). Unmatched placeholders are left intact so authoring
/// typos surface as parse / runtime errors downstream rather than
/// silently rendering empty strings.
/// True when any element in the template tree carries a `{...}` placeholder
/// inside a selector-relevant attr (`id` or `class`). Such templates must
/// run the CSS cascade per row (post-substitution) because rule matching
/// depends on the substituted values; everything else can use the
/// cascade-once-per-template cache in [`ForMarker::cascaded_body`].
fn body_has_dynamic_selector_attrs(body: &[Element]) -> bool {
    fn walk(el: &Element) -> bool {
        if el.attrs.id.as_deref().is_some_and(|id| id.contains('{')) {
            return true;
        }
        if el.attrs.classes.iter().any(|c| c.contains('{')) {
            return true;
        }
        el.children.iter().any(walk)
    }
    body.iter().any(walk)
}

/// Apply the CSS cascade to `el` and every descendant, without any
/// placeholder substitution. Used to build the cascade-once template
/// cache for virtualized `<for>` rows.
fn cascade_element_tree(el: &mut Element, sheet: &lumen_ir::css::Stylesheet) {
    let _ = lumen_ir::css::reapply_single(el, sheet);
    for child in el.children.iter_mut() {
        cascade_element_tree(child, sheet);
    }
}

fn substitute_in_element_with_css(
    template: &Element,
    ctx: &RowCtx<'_>,
    css: Option<&lumen_ir::css::Stylesheet>,
) -> Element {
    let mut clone = template.clone();
    substitute_in_attrs_text(&mut clone, ctx);
    if let Some(sheet) = css {
        let _ = lumen_ir::css::reapply_single(&mut clone, sheet);
    }
    clone.children = clone
        .children
        .into_iter()
        .map(|c| {
            let mut child = c;
            substitute_in_attrs_text(&mut child, ctx);
            if let Some(sheet) = css {
                let _ = lumen_ir::css::reapply_single(&mut child, sheet);
            }
            child.children = child
                .children
                .iter()
                .map(|gc| substitute_in_element_with_css(gc, ctx, css))
                .collect();
            child
        })
        .collect();
    clone
}

/// Resolve every placeholder slot recorded on `el` against `ctx` and
/// substitute the result into the element's string-valued attrs.
///
/// Resolution rules (wave-C contract):
/// - [`InterpolationSlot::Row`] - `ctx.item.get(field)`. Missing field
///   -> empty string + a single `tracing::warn!` (deduplicated per
///   `(parent_id, field)` pair so a missing field on a 1000-row list
///   doesn't spam the log).
/// - [`InterpolationSlot::RowIndex`] - `ctx.idx.to_string()`.
/// - [`InterpolationSlot::Global`] - `ctx.store.get_global_str(name)`.
///   Missing -> empty string, no warn (matches the global-signal-not-set
///   semantics elsewhere).
/// - [`InterpolationSlot::SelfField`] / [`InterpolationSlot::ParentField`]:
///   empty string + a `tracing::debug!`. The per-entity consumer
///   system lands in a follow-up wave.
///
/// Crucially: when an element has no `interpolations` recorded
/// (synthetic elements, elements outside `<for>` bodies parsed before
/// wave-C, etc.) we **fall back** to the legacy "replace every
/// `{field}` token with the matching record field" pass so existing
/// for-blocks keep working. Both paths run because a `<for>` with
/// `<row>` containers wraps the slot-bearing leaves several levels
/// deep, and the parser only attaches slots to the bearing leaves
/// (not their ancestors).
fn substitute_in_attrs_text(el: &mut Element, ctx: &RowCtx<'_>) {
    let sub = |s: &str| -> String { resolve_placeholders(s, &el.interpolations, ctx) };
    if let Some(t) = &el.attrs.text {
        el.attrs.text = Some(sub(t));
    }
    if let Some(id) = &el.attrs.id {
        el.attrs.id = Some(sub(id));
    }
    if let Some(src) = &el.attrs.src {
        el.attrs.src = Some(sub(src));
    }
    if let Some(role) = &el.attrs.style_role {
        el.attrs.style_role = Some(sub(role));
    }
    if let Some(placeholder) = &el.attrs.placeholder {
        el.attrs.placeholder = Some(sub(placeholder));
    }
    if let Some(payload) = &el.attrs.drag_payload {
        el.attrs.drag_payload = Some(sub(payload));
    }
    el.attrs.classes = el.attrs.classes.iter().map(|c| sub(c)).collect();
}

/// Single-pass placeholder resolver. Replaces every `{...}` token in
/// `body` with the wave-C scope-aware result for the matching
/// [`InterpolationSlot`]. Tokens that don't classify as a known slot
/// fall through to the legacy "replace `{k}` with `item[k]`" rule so
/// pre-wave-C for-bodies (no `interpolations` populated on their
/// elements) still work.
fn resolve_placeholders(body: &str, slots: &[InterpolationSlot], ctx: &RowCtx<'_>) -> String {
    let mut out = String::with_capacity(body.len());
    let mut i = 0;
    while i < body.len() {
        let Some(rel) = body[i..].find('{') else {
            out.push_str(&body[i..]);
            break;
        };
        let lt = i + rel;
        out.push_str(&body[i..lt]);
        let Some(end_rel) = body[lt..].find('}') else {
            out.push_str(&body[lt..]);
            break;
        };
        let gt = lt + end_rel;
        let inner = &body[lt + 1..gt];
        let trimmed = inner.trim();
        let slot = InterpolationSlot::from(trimmed);
        let resolved = match &slot {
            InterpolationSlot::Row(field) => match ctx.item.get(field) {
                Some(v) => Some(v.clone()),
                None => {
                    warn_missing_row_field_once(ctx.parent_id, field);
                    Some(String::new())
                }
            },
            InterpolationSlot::RowIndex => Some(ctx.idx.to_string()),
            InterpolationSlot::Global(name) => {
                ctx.store.get_global_str(name).map(|s| s.to_string())
            }
            InterpolationSlot::SelfField(field) | InterpolationSlot::ParentField(field) => {
                tracing::debug!(
                    target: "lumenc::spawn::row",
                    "$self / $parent field `{field}` substituted as empty - \
                     per-entity property store consumer lands in a follow-up wave"
                );
                Some(String::new())
            }
            // A fragment argument only has a value at the use site that
            // passed it, which this resolver cannot see: it runs over the
            // tree a fragment already expanded into. Binding arguments is
            // the instantiation path's job, so an `Arg` still standing here
            // is a parameter nothing bound, and it resolves to empty.
            InterpolationSlot::Arg(_) => Some(String::new()),
        };
        // Honour the recorded slot list when present so authoring
        // intent wins over the legacy `{k}` substring rule: if the
        // parser classified this brace as e.g. `Row("name")` we use
        // the row lookup result even when `slots` doesn't carry a
        // matching entry (synthetic / cloned elements). When the slot
        // resolution returned `None` (Global with no signal set), fall
        // back to the legacy lookup so legacy `<for>` markup that
        // referenced `{field}` as a synonym for the row field keeps
        // working - wave-C records that as Global but the row item
        // may still carry the field.
        let final_value = resolved.or_else(|| {
            // Skip the legacy fallback for slots that should never
            // accidentally hit the row record. `RowIndex` resolved
            // above; `Row(field)` already consulted the item.
            if matches!(
                slot,
                InterpolationSlot::RowIndex | InterpolationSlot::Row(_)
            ) {
                return None;
            }
            ctx.item.get(trimmed).cloned()
        });
        // Suppress wave-C participation when the slot list explicitly
        // doesn't claim this placeholder. `slots.is_empty()` means
        // the parser didn't attach any slots to this element (older
        // IR or synthetic), in which case the legacy substring rule
        // governs the fallthrough.
        let _ = slots;
        match final_value {
            Some(v) => out.push_str(&v),
            None => {
                // No resolution - preserve the placeholder verbatim
                // so downstream errors point at the literal `{x}`.
                out.push('{');
                out.push_str(inner);
                out.push('}');
            }
        }
        i = gt + 1;
    }
    out
}

/// Emit a `tracing::warn!` for a missing row field, deduplicated per
/// `(parent_id, field)` pair. A 1000-row `<for>` referencing a typo'd
/// field should warn once, not 1000 times.
fn warn_missing_row_field_once(parent_id: Entity, field: &str) {
    use std::collections::HashSet;
    use std::sync::Mutex;
    static SEEN: std::sync::OnceLock<Mutex<HashSet<(u64, String)>>> = std::sync::OnceLock::new();
    let key = (parent_id.to_bits(), field.to_string());
    let seen = SEEN.get_or_init(|| Mutex::new(HashSet::new()));
    let mut guard = seen.lock().expect("warn-once mutex poisoned");
    if guard.insert(key) {
        tracing::warn!(
            target: "lumenc::spawn::row",
            "<for> row field `{field}` not present on iteration record - substituting empty string"
        );
    }
}

/// `spawn_element` analogue for the reconciler - must go through
/// `Commands` (deferred mutation) because the reconciler holds a query
/// borrow.
///
/// Implementation: queue a single world-closure that runs the real
/// [`spawn_element`] at command-apply time. The previous version was a
/// hand-mirrored copy of `spawn_element` that had drifted badly - it
/// never attached `TextInput`, `Toggleable` with its knob child,
/// `SliderValue` with its thumb child, `Scroll`/`ScrollOffset`,
/// `TabIndex`, `DropdownButton`/`DropdownOptionButton`/`MenuItemButton`,
/// `TabStripButton`, `TooltipSource`, `Validation`, `DocumentOrder`, ...
/// so every widget mounted inside an `<if>` / `<for>` / `<tab>` /
/// `<dialog>` body (tab panels compile to `<if eq>` gates, i.e. most of
/// a real app) spawned as a dead visual: toggles had no knob and no
/// state, sliders no thumb, inputs weren't focusable or editable,
/// dropdown headers didn't open on click, inner `<scroll>` containers
/// ignored the wheel, and Tab-cycle skipped the whole subtree (audit
/// root causes 5, 8, 9). Routing through `spawn_element` makes the two
/// paths structurally identical, forever. Commands apply in FIFO order,
/// so despawns queued before this in the same reconcile pass still land
/// first, and `parent` (spawned earlier in the queue, or pre-existing)
/// is alive by the time the closure runs.
fn spawn_body_child(commands: &mut bevy_ecs::system::Commands, el: &Element, parent: Entity) {
    let el = el.clone();
    commands.queue(move |world: &mut World| {
        let id = spawn_element(world, &el, Some(parent));
        // Mount-direction transition (the CSS `@starting-style`
        // analogue): an element entering the tree with a
        // `transition: opacity ...` declaration starts fully transparent
        // and fades to its computed opacity. Gives fade-in dialogs /
        // menus / rows for free. Removal transitions (fade-out before
        // despawn) are intentionally NOT implemented - CSS itself can't
        // express them without JS, and holding despawns hostage to
        // animations would complicate every reconciler.
        let specs = world.get::<lumen_primitives::TransitionSpecs>(id).cloned();
        let cur = world.get::<Opacity>(id).copied();
        if let Some(bundle) = mount_fade_bundle(specs.as_ref(), cur) {
            world.entity_mut(id).insert(bundle);
        }
    });
}

/// Build the `(Opacity(0), OpacityTransition(0 -> target))` pair for a
/// freshly-mounted / freshly-shown element that declares a
/// `transition: opacity ...`. `None` when there is no (non-zero-duration)
/// opacity spec or the target opacity is already 0.
fn mount_fade_bundle(
    specs: Option<&lumen_primitives::TransitionSpecs>,
    current: Option<Opacity>,
) -> Option<(Opacity, lumen_primitives::OpacityTransition)> {
    let spec = specs?.for_property(lumen_primitives::TransitionProperty::Opacity)?;
    if spec.duration.is_zero() {
        return None;
    }
    let target = current.map(|o| o.0).unwrap_or(1.0);
    if target <= 0.0 {
        return None;
    }
    Some((
        Opacity(0.0),
        lumen_primitives::OpacityTransition(lumen_primitives::Transition::new(
            0.0,
            target,
            spec.duration,
            spec.easing,
        )),
    ))
}

/// The UA-origin per-tag sizing defaults (D3 / task #29) live in
/// `skins/ua.css` now - an always-on stylesheet folded into the cascade
/// beneath any skin and beneath app CSS (see `crate::skins::UA` and
/// `run::loading::load_ir`), so a skinless app still gets a sizing
/// floor without a Rust-side table to keep in sync.
///
/// Two cases don't fit a plain CSS rule and stay here, running at spawn
/// time after the cascade and inline attrs have been folded into
/// `attrs`:
///
/// - `<switch>` wants its UA `min-width` only when `width` is ALSO
///   unset (an explicit width already gives it a usable footprint).
///   CSS has no "set this property only if that other property is
///   unset" syntax, so this stays a Rust conditional instead of a
///   `ua.css` rule.
/// - `<input>` / `<textarea>` default to `overflow: hidden` only when
///   neither the `overflow` shorthand nor `overflow-x` / `overflow-y`
///   was authored anywhere. `lumen_ir::css` never expands the `overflow`
///   shorthand into the longhands - they're independent properties to
///   the cascade, reconciled only afterward in `lumen_ir::convert` via
///   `.or()`, which doesn't know about origin. A plain `ua.css`
///   `overflow-x` rule would therefore out-rank an author's
///   shorthand-only `overflow: visible`, instead of losing to it as the
///   current behavior requires - so this checks the fully-resolved
///   `attrs` directly instead.
pub(crate) fn apply_ua_style_defaults(tag: &str, attrs: &Attributes, style: &mut Style) {
    if tag == "switch" && attrs.width.is_none() && attrs.min_width.is_none() {
        style.min_width = Length::Px(52.0);
    }
    if matches!(tag, "input" | "textarea") {
        // Long values scroll horizontally under the caret-keep-visible
        // offset (`TextInputScroll`) instead of painting past the field
        // box - clip like every real toolkit does. Author markup / CSS
        // overflow attrs still win.
        if attrs.overflow.is_none() && attrs.overflow_x.is_none() {
            style.overflow_x = Overflow::Hidden;
        }
        if attrs.overflow.is_none() && attrs.overflow_y.is_none() {
            style.overflow_y = Overflow::Hidden;
        }
    }
}

/// Absolute-position [`Style`] seed for the toggle knob / slider thumb
/// child. `size` is the initial square side; `inset_px` is the top/left
/// offset (right / bottom stay `NaN` = auto so taffy doesn't
/// over-constrain against the explicit size). The runtime sync systems
/// refine size + inset once the track's laid-out rect is known.
fn knob_style(size: f32, inset_px: f32) -> Style {
    Style {
        position: lumen_core::components::Position::Absolute,
        width: Length::Px(size),
        height: Length::Px(size),
        inset: lumen_core::components::Edges {
            left: inset_px,
            top: inset_px,
            right: f32::NAN,
            bottom: f32::NAN,
            ..Default::default()
        },
        ..Default::default()
    }
}

/// Should this entity carry [`RelayoutBoundary`]? An entity qualifies
/// when:
///
/// 1. Explicit `layout-boundary="true"` markup attr - author override.
/// 2. Either axis overflows as `Scroll` - the scroll container clips
///    its content, so descendants reflowing don't affect the parent's
///    own size beyond what already-known clip box allows.
/// 3. Both `width` and `height` are fixed `Px` - the entity's size is
///    constraint-imposed, so descendants can re-flow internally
///    without changing the parent's measured dimensions.
///
/// Pattern: Flutter's `_relayoutBoundary`, set on `RenderObjects` that
/// pass the same constraint test.
fn is_relayout_boundary(style: &Style, el: &Element) -> bool {
    if el.attrs.layout_boundary {
        return true;
    }
    if matches!(style.overflow_x, Overflow::Scroll) || matches!(style.overflow_y, Overflow::Scroll)
    {
        return true;
    }
    matches!(style.width, Length::Px(_)) && matches!(style.height, Length::Px(_))
}

#[cfg(test)]
mod body_spawn_tests {
    //! The reconciler spawn path must produce the same widget behavior
    //! as the initial `spawn_element` walk. It used to be a drifted
    //! hand-mirror that skipped `TextInput`, `Toggleable` + knob,
    //! `SliderValue` + thumb, `Scroll`, `TabIndex`, `DropdownButton`,
    //! `DocumentOrder`, ... - so every widget inside an `<if>` / `<for>`
    //! / `<tab>` body spawned inert (audit RC5 / RC8 / RC9).
    use super::*;
    use bevy_ecs::system::RunSystemOnce;
    use lumen_core::property_store::PropertyStore;

    fn el(tag: &str) -> Element {
        Element {
            tag: tag.to_string(),
            ..Element::default()
        }
    }

    #[test]
    fn if_body_children_spawn_with_full_widget_behavior() {
        let mut world = World::new();
        world.insert_resource(PropertyStore::default());
        world
            .resource_mut::<PropertyStore>()
            .set_global_str("open", "1");

        let mut toggle = el("toggle");
        toggle.attrs.tab_index = Some(0);
        let mut slider = el("slider");
        slider.attrs.tab_index = Some(0);
        let mut input = el("input");
        input.attrs.tab_index = Some(0);
        let mut scroll = el("scroll");
        scroll.attrs.scroll = Some(lumen_ir::layout_ir::ScrollAxisSpec::Y);
        let mut header = el("button");
        header.attrs.dropdown_button = Some(lumen_ir::layout_ir::DropdownButtonSpec {
            open_signal: "__dropdown_open:x".to_string(),
            value_signal: "x".to_string(),
            options: vec![("a".to_string(), "A".to_string(), false)],
        });

        world.spawn(IfMarker {
            signal_name: "open".to_string(),
            body: vec![toggle, slider, input, scroll, header],
            currently_mounted: false,
            mode: IfMode::Hide,
            eq: None,
            saved_display: lumen_core::components::Display::Flex,
            applied_visible: None,
        });
        world.run_system_once(reconcile_if_blocks).unwrap();

        // <toggle> mounts stateful + with its knob child.
        let toggle_e = world
            .query::<(Entity, &Toggleable)>()
            .iter(&world)
            .next()
            .expect("toggle in an if-body must carry Toggleable")
            .0;
        let knob_parent = world
            .query_filtered::<&ChildOf, With<lumen_primitives::ToggleKnob>>()
            .iter(&world)
            .next()
            .expect("toggle in an if-body must spawn its knob child")
            .parent();
        assert_eq!(knob_parent, toggle_e, "knob is the toggle's child");

        // <slider> mounts stateful + with its thumb child.
        let slider_e = world
            .query::<(Entity, &SliderValue)>()
            .iter(&world)
            .next()
            .expect("slider in an if-body must carry SliderValue")
            .0;
        let thumb_parent = world
            .query_filtered::<&ChildOf, With<lumen_primitives::SliderThumb>>()
            .iter(&world)
            .next()
            .expect("slider in an if-body must spawn its thumb child")
            .parent();
        assert_eq!(thumb_parent, slider_e, "thumb is the slider's child");

        // <input> is editable/focusable; <scroll> receives the wheel;
        // the dropdown header opens on click.
        assert_eq!(
            world.query::<&TextInput>().iter(&world).count(),
            1,
            "input in an if-body must carry TextInput"
        );
        assert_eq!(
            world
                .query::<(&Scroll, &ScrollOffset)>()
                .iter(&world)
                .count(),
            1,
            "scroll in an if-body must carry Scroll + ScrollOffset"
        );
        assert_eq!(
            world
                .query::<&lumen_primitives::DropdownButton>()
                .iter(&world)
                .count(),
            1,
            "dropdown header in an if-body must carry DropdownButton"
        );
        // Unstyled controls still get UA fallback visuals: without a
        // `Visuals` component they are invisible AND excluded from the
        // hit-test candidate set, so track clicks / click-to-focus
        // could never work in skinless apps.
        for (label, e) in [("slider", slider_e), ("toggle", toggle_e)] {
            assert!(
                world.get::<Visuals>(e).is_some(),
                "unstyled {label} must carry UA fallback Visuals"
            );
        }
        let input_e = world
            .query_filtered::<Entity, With<TextInput>>()
            .iter(&world)
            .next()
            .unwrap();
        assert!(
            world.get::<Visuals>(input_e).is_some(),
            "unstyled input must carry UA fallback Visuals"
        );

        // Tab cycle: focusables carry TabIndex + DocumentOrder.
        assert_eq!(
            world
                .query::<(&TabIndex, &lumen_core::components::DocumentOrder)>()
                .iter(&world)
                .count(),
            3,
            "if-body focusables must carry TabIndex + DocumentOrder"
        );
    }
}

#[cfg(test)]
mod escape_tests {
    //! `close_dialogs_on_escape` - one Esc peels off the top-most open
    //! popup layer (dropdown -> menu -> dialog). Driven directly via
    //! `run_system_once` against a bare `World`.
    use super::*;
    use bevy_ecs::message::Messages;
    use bevy_ecs::system::RunSystemOnce;
    use lumen_core::input::{Key, KeyPressed, Modifiers, NamedKey};
    use lumen_core::property_store::PropertyStore;

    fn if_marker(signal: &str) -> IfMarker {
        IfMarker {
            signal_name: signal.to_string(),
            body: Vec::new(),
            currently_mounted: true,
            mode: IfMode::Hide,
            eq: None,
            saved_display: lumen_core::components::Display::Flex,
            applied_visible: None,
        }
    }

    fn press_escape(world: &mut World) {
        world
            .resource_mut::<Messages<KeyPressed>>()
            .write(KeyPressed {
                key: Key::Named(NamedKey::Escape),
                modifiers: Modifiers::default(),
                repeat: false,
            });
    }

    fn run(world: &mut World) {
        world.run_system_once(close_dialogs_on_escape).unwrap();
        world.resource_mut::<Messages<KeyPressed>>().clear();
    }

    #[test]
    fn escape_closes_dropdown_before_dialog() {
        let mut world = World::new();
        world.init_resource::<Messages<KeyPressed>>();
        world.insert_resource(PropertyStore::default());

        // Dropdown open on top of an open dialog.
        world
            .resource_mut::<PropertyStore>()
            .set_global_bool("__dropdown_open:fruit", true);
        world.spawn(if_marker("__dropdown_open:fruit"));
        world
            .resource_mut::<PropertyStore>()
            .set_global_str("show_dialog", "1");
        world.spawn((if_marker("show_dialog"), DialogMarker));

        // First Esc: only the dropdown collapses.
        press_escape(&mut world);
        run(&mut world);
        let store = world.resource::<PropertyStore>();
        assert_eq!(
            store.get_global_bool("__dropdown_open:fruit"),
            Some(false),
            "dropdown closed first"
        );
        assert_eq!(
            store.get_global_str("show_dialog").as_deref(),
            Some("1"),
            "dialog still open after one press"
        );

        // Second Esc: now the dialog closes.
        press_escape(&mut world);
        run(&mut world);
        assert_eq!(
            world
                .resource::<PropertyStore>()
                .get_global_str("show_dialog")
                .as_deref(),
            Some(""),
            "dialog closes on the second press"
        );
    }

    #[test]
    fn escape_closes_menu_before_dialog() {
        let mut world = World::new();
        world.init_resource::<Messages<KeyPressed>>();
        world.insert_resource(PropertyStore::default());

        world
            .resource_mut::<PropertyStore>()
            .set_global_bool("__menu_open:file", true);
        world.spawn(if_marker("__menu_open:file"));
        world
            .resource_mut::<PropertyStore>()
            .set_global_str("show_dialog", "1");
        world.spawn((if_marker("show_dialog"), DialogMarker));

        press_escape(&mut world);
        run(&mut world);
        let store = world.resource::<PropertyStore>();
        assert_eq!(store.get_global_bool("__menu_open:file"), Some(false));
        assert_eq!(store.get_global_str("show_dialog").as_deref(), Some("1"));
    }

    /// Wave 3: an Escape that cancelled an in-flight press
    /// (`lumen_input::cancel_press_on_escape` set the consumed flag
    /// earlier in the Input stage) must not also close the dialog.
    #[test]
    fn escape_consumed_by_press_cancel_leaves_dialog_open() {
        let mut world = World::new();
        world.init_resource::<Messages<KeyPressed>>();
        world.insert_resource(PropertyStore::default());
        world
            .resource_mut::<PropertyStore>()
            .set_global_str("show_dialog", "1");
        world.spawn((if_marker("show_dialog"), DialogMarker));

        // Press-cancel consumed this Escape.
        world.insert_resource(lumen_core::input::EscapePressCancel(true));
        press_escape(&mut world);
        run(&mut world);
        assert_eq!(
            world
                .resource::<PropertyStore>()
                .get_global_str("show_dialog")
                .as_deref(),
            Some("1"),
            "dialog stays open when the Escape cancelled a press"
        );

        // Next Escape (no press in flight) closes as usual.
        world.insert_resource(lumen_core::input::EscapePressCancel(false));
        press_escape(&mut world);
        run(&mut world);
        assert_eq!(
            world
                .resource::<PropertyStore>()
                .get_global_str("show_dialog")
                .as_deref(),
            Some(""),
            "un-consumed Escape still closes the dialog"
        );
    }
}

/// W5 dialog contract - initial focus, focus restore, default-button
/// Enter, and the exactly-once accepted/rejected close event. Driven
/// via `run_system_once` against a bare `World` (same style as
/// `escape_tests`).
#[cfg(test)]
mod dialog_contract_tests {
    use super::*;
    use bevy_ecs::message::Messages;
    use bevy_ecs::system::RunSystemOnce;
    use lumen_core::components::{DocumentOrder, LumenId, LumenTag, TabIndex, Visible};
    use lumen_core::input::{
        ClickEvent, DialogClosed, FocusTracker, Focused, Key, KeyPressed, Modifiers, NamedKey,
        PointerButton,
    };

    fn if_marker(signal: &str) -> IfMarker {
        IfMarker {
            signal_name: signal.to_string(),
            body: Vec::new(),
            currently_mounted: true,
            mode: IfMode::Hide,
            eq: None,
            saved_display: lumen_core::components::Display::Flex,
            applied_visible: None,
        }
    }

    /// Dialog with a body: an input (autofocus optional), and two
    /// buttons (Cancel, Confirm-with-default-marker).
    struct Fixture {
        dialog: Entity,
        input: Entity,
        cancel: Entity,
        confirm: Entity,
        opener: Entity,
    }

    fn build(world: &mut World, autofocus_input: bool) -> Fixture {
        world.init_resource::<Messages<DialogClosed>>();
        world.init_resource::<Messages<KeyPressed>>();
        world.init_resource::<Messages<ClickEvent>>();
        world.insert_resource(FocusTracker(None));
        let opener = world
            .spawn((LumenTag("button".into()), TabIndex(0), DocumentOrder(0)))
            .id();
        let dialog = world
            .spawn((DialogMarker, if_marker("dlg_open"), Visible(false)))
            .id();
        let mut input = world.spawn((
            LumenTag("input".into()),
            TabIndex(0),
            DocumentOrder(10),
            ChildOf(dialog),
        ));
        if autofocus_input {
            input.insert(AutoFocus);
        }
        let input = input.id();
        let cancel = world
            .spawn((
                LumenTag("button".into()),
                TabIndex(0),
                DocumentOrder(11),
                ChildOf(dialog),
            ))
            .id();
        let confirm = world
            .spawn((
                LumenTag("button".into()),
                TabIndex(0),
                DocumentOrder(12),
                DefaultButton,
                ChildOf(dialog),
            ))
            .id();
        Fixture {
            dialog,
            input,
            cancel,
            confirm,
            opener,
        }
    }

    fn run_lifecycle(world: &mut World) {
        world.run_system_once(manage_dialog_lifecycle).unwrap();
    }

    fn set_open(world: &mut World, dialog: Entity, open: bool) {
        world.entity_mut(dialog).insert(Visible(open));
    }

    fn drain_closed(world: &mut World) -> Vec<DialogClosed> {
        world
            .resource_mut::<Messages<DialogClosed>>()
            .drain()
            .collect()
    }

    #[test]
    fn open_focuses_autofocus_descendant() {
        let mut world = World::new();
        let fx = build(&mut world, true);
        world.insert_resource(FocusTracker(Some(fx.opener)));
        run_lifecycle(&mut world); // seed session (closed)
        set_open(&mut world, fx.dialog, true);
        run_lifecycle(&mut world); // open edge + focus pass
        assert_eq!(world.resource::<FocusTracker>().0, Some(fx.input));
        assert!(world.get::<Focused>(fx.input).is_some());
    }

    #[test]
    fn open_without_autofocus_focuses_first_focusable_descendant() {
        let mut world = World::new();
        let fx = build(&mut world, false);
        world.insert_resource(FocusTracker(Some(fx.opener)));
        run_lifecycle(&mut world);
        set_open(&mut world, fx.dialog, true);
        run_lifecycle(&mut world);
        assert_eq!(
            world.resource::<FocusTracker>().0,
            Some(fx.input),
            "input has the lowest DocumentOrder among focusables"
        );
    }

    #[test]
    fn close_restores_previous_focus_and_fires_rejected_once() {
        let mut world = World::new();
        let fx = build(&mut world, true);
        world.insert_resource(FocusTracker(Some(fx.opener)));
        world.entity_mut(fx.opener).insert(Focused);
        run_lifecycle(&mut world);
        set_open(&mut world, fx.dialog, true);
        run_lifecycle(&mut world);
        assert_eq!(world.resource::<FocusTracker>().0, Some(fx.input));
        // Close (e.g. Escape wrote the signal; Visible flipped).
        set_open(&mut world, fx.dialog, false);
        run_lifecycle(&mut world);
        assert_eq!(
            world.resource::<FocusTracker>().0,
            Some(fx.opener),
            "focus returns to the pre-open holder"
        );
        let closed = drain_closed(&mut world);
        assert_eq!(closed.len(), 1, "exactly one close event");
        assert!(!closed[0].accepted, "non-default close is rejected");
        // Further idle ticks: no repeats.
        run_lifecycle(&mut world);
        run_lifecycle(&mut world);
        assert!(drain_closed(&mut world).is_empty(), "never fires twice");
    }

    #[test]
    fn default_button_click_resolves_close_as_accepted_exactly_once() {
        let mut world = World::new();
        let fx = build(&mut world, true);
        run_lifecycle(&mut world);
        set_open(&mut world, fx.dialog, true);
        run_lifecycle(&mut world);
        // Pointer click lands on the confirm button's TEXT CHILD
        // (hit-shadowing) - the accept marker must still resolve.
        let text_child = world.spawn(ChildOf(fx.confirm)).id();
        world
            .resource_mut::<Messages<ClickEvent>>()
            .write(ClickEvent {
                entity: text_child,
                position: glam::Vec2::ZERO,
                button: PointerButton::Primary,
            });
        world
            .run_system_once(mark_dialog_accept_on_default_click)
            .unwrap();
        set_open(&mut world, fx.dialog, false);
        run_lifecycle(&mut world);
        let closed = drain_closed(&mut world);
        assert_eq!(closed.len(), 1);
        assert!(closed[0].accepted, "default-button close is accepted");
        assert_eq!(closed[0].id, "dlg_open", "falls back to the open signal");
        // Re-open + Escape-close: verdict resets to rejected.
        set_open(&mut world, fx.dialog, true);
        run_lifecycle(&mut world);
        set_open(&mut world, fx.dialog, false);
        run_lifecycle(&mut world);
        let closed = drain_closed(&mut world);
        assert_eq!(closed.len(), 1);
        assert!(
            !closed[0].accepted,
            "pending accept never leaks into the next cycle"
        );
    }

    #[test]
    fn dialog_id_attribute_wins_over_signal_name() {
        let mut world = World::new();
        let fx = build(&mut world, true);
        world
            .entity_mut(fx.dialog)
            .insert(LumenId("demo-dialog".into()));
        run_lifecycle(&mut world);
        set_open(&mut world, fx.dialog, true);
        run_lifecycle(&mut world);
        set_open(&mut world, fx.dialog, false);
        run_lifecycle(&mut world);
        let closed = drain_closed(&mut world);
        assert_eq!(closed[0].id, "demo-dialog");
    }

    fn press_enter(world: &mut World) {
        world
            .resource_mut::<Messages<KeyPressed>>()
            .write(KeyPressed {
                key: Key::Named(NamedKey::Enter),
                modifiers: Modifiers::default(),
                repeat: false,
            });
    }

    fn run_enter(world: &mut World) {
        world
            .run_system_once(activate_dialog_default_on_enter)
            .unwrap();
        world.resource_mut::<Messages<KeyPressed>>().clear();
    }

    fn synthesized_clicks(world: &mut World) -> Vec<Entity> {
        world
            .resource_mut::<Messages<ClickEvent>>()
            .drain()
            .map(|c| c.entity)
            .collect()
    }

    #[test]
    fn enter_on_non_button_focus_activates_default() {
        let mut world = World::new();
        let fx = build(&mut world, true);
        run_lifecycle(&mut world);
        set_open(&mut world, fx.dialog, true);
        run_lifecycle(&mut world); // focus lands on the input
        press_enter(&mut world);
        run_enter(&mut world);
        assert_eq!(
            synthesized_clicks(&mut world),
            vec![fx.confirm],
            "Enter with focus on the (single-line) input clicks the default button"
        );
        assert!(
            world
                .get::<DialogSession>(fx.dialog)
                .unwrap()
                .pending_accept,
            "Enter-default marks the accept path"
        );
    }

    #[test]
    fn enter_on_focused_button_does_not_double_activate() {
        let mut world = World::new();
        let fx = build(&mut world, false);
        run_lifecycle(&mut world);
        set_open(&mut world, fx.dialog, true);
        run_lifecycle(&mut world);
        // Focus the CANCEL button: Enter must go to it (via the normal
        // activate_focused_on_enter path), not the default button.
        world.insert_resource(FocusTracker(Some(fx.cancel)));
        press_enter(&mut world);
        run_enter(&mut world);
        assert!(
            synthesized_clicks(&mut world).is_empty(),
            "focused button consumes Enter itself"
        );
    }

    #[test]
    fn enter_ignores_closed_dialogs() {
        let mut world = World::new();
        let fx = build(&mut world, false);
        run_lifecycle(&mut world);
        world.insert_resource(FocusTracker(None));
        press_enter(&mut world);
        run_enter(&mut world);
        assert!(synthesized_clicks(&mut world).is_empty());
        let _ = fx;
    }

    #[test]
    fn enter_falls_back_to_first_enabled_button_without_default_marker() {
        let mut world = World::new();
        let fx = build(&mut world, false);
        world.entity_mut(fx.confirm).remove::<DefaultButton>();
        run_lifecycle(&mut world);
        set_open(&mut world, fx.dialog, true);
        run_lifecycle(&mut world);
        world.insert_resource(FocusTracker(Some(fx.input)));
        press_enter(&mut world);
        run_enter(&mut world);
        assert_eq!(
            synthesized_clicks(&mut world),
            vec![fx.cancel],
            "Qt autoDefault: first enabled button in markup order"
        );
    }
}

#[cfg(test)]
mod css_spawn_wiring_tests {
    //! Phase 2 (skin-tokens): CSS-authored values resolved into
    //! `Attributes` must reach the runtime at SPAWN, not only via a live
    //! restyle recompute - `restyle::apply_reapplied_attrs` already wires
    //! these, so a value that only shows up after a theme flip and not on
    //! first launch is exactly the bug these tests pin.
    use super::*;

    fn el(tag: &str) -> Element {
        Element {
            tag: tag.to_string(),
            ..Element::default()
        }
    }

    #[test]
    fn input_gets_caret_width_and_password_character_from_css() {
        let mut world = World::new();
        let mut input = el("input");
        input.attrs.caret_width = Some(3.5);
        input.attrs.password_character = Some('#');
        let e = spawn_subtree(&mut world, &input, None);

        assert_eq!(
            world.get::<lumen_core::components::CaretWidth>(e),
            Some(&lumen_core::components::CaretWidth(3.5))
        );
        assert_eq!(
            world.get::<lumen_core::components::PasswordCharacter>(e),
            Some(&lumen_core::components::PasswordCharacter('#'))
        );
    }

    /// An unauthored `<input>` still carries both components, holding the
    /// `Default` fallback - inserted unconditionally, the same contract
    /// pattern as `KnobGeometry` / `PopupGap`, not left absent.
    #[test]
    fn input_gets_default_caret_width_and_password_character_when_unauthored() {
        let mut world = World::new();
        let e = spawn_subtree(&mut world, &el("input"), None);

        assert_eq!(
            world.get::<lumen_core::components::CaretWidth>(e),
            Some(&lumen_core::components::CaretWidth::default())
        );
        assert_eq!(
            world.get::<lumen_core::components::PasswordCharacter>(e),
            Some(&lumen_core::components::PasswordCharacter::default())
        );
    }

    /// A non-text-input element must not gain either component at all -
    /// they are only meaningful on `<input>` / `<textarea>`.
    #[test]
    fn non_input_does_not_get_caret_width_or_password_character() {
        let mut world = World::new();
        let e = spawn_subtree(&mut world, &el("column"), None);

        assert!(world.get::<lumen_core::components::CaretWidth>(e).is_none());
        assert!(
            world
                .get::<lumen_core::components::PasswordCharacter>(e)
                .is_none()
        );
    }

    /// `<textarea>` gets the same wiring as `<input>` - both share the
    /// gate.
    #[test]
    fn textarea_gets_caret_width_from_css() {
        let mut world = World::new();
        let mut textarea = el("textarea");
        textarea.attrs.caret_width = Some(4.0);
        let e = spawn_subtree(&mut world, &textarea, None);

        assert_eq!(
            world.get::<lumen_core::components::CaretWidth>(e),
            Some(&lumen_core::components::CaretWidth(4.0))
        );
    }

    /// `line-height` alone (see `lumen_ir::convert`'s
    /// `line_height_alone_produces_a_text_style` for the same case at the
    /// conversion layer) reaches the spawned `TextStyle`.
    #[test]
    fn text_style_carries_authored_line_height_at_spawn() {
        let mut world = World::new();
        let mut label = el("label");
        label.attrs.line_height = Some(lumen_ir::layout_ir::LineHeightSpec::Multiplier(1.6));
        let e = spawn_subtree(&mut world, &label, None);

        let ts = world
            .get::<TextStyle>(e)
            .expect("line-height alone must still spawn a TextStyle");
        assert_eq!(
            ts.line_height,
            Some(lumen_core::components::LineHeightSpec::Multiplier(1.6))
        );
    }

    /// Every new `scrollbar-*` field spawn was silently dropping (only
    /// `thumb` / `track` / `width` reached `ScrollbarStyle`) now converts
    /// and lands, including the ms -> s duration conversion for the fade
    /// timings.
    #[test]
    fn scrollbar_style_carries_the_new_css_fields_at_spawn() {
        let mut world = World::new();
        let mut column = el("column");
        column.attrs.scrollbar_thickness = Some(10.0);
        column.attrs.scrollbar_thickness_thin = Some(5.0);
        column.attrs.scrollbar_margin = Some(3.0);
        column.attrs.scrollbar_min_thumb = Some(24.0);
        column.attrs.scrollbar_hover_boost = Some(2.5);
        column.attrs.scrollbar_fade_delay_ms = Some(2000);
        column.attrs.scrollbar_fade_duration_ms = Some(500);
        let e = spawn_subtree(&mut world, &column, None);

        let sb = world
            .get::<lumen_core::input::ScrollbarStyle>(e)
            .expect("authored scrollbar-* properties must spawn a ScrollbarStyle");
        assert_eq!(sb.thickness, 10.0);
        assert_eq!(sb.thickness_thin, 5.0);
        assert_eq!(sb.margin, 3.0);
        assert_eq!(sb.min_thumb, 24.0);
        assert_eq!(sb.hover_boost, 2.5);
        assert_eq!(sb.fade_delay_secs, 2.0, "2000ms -> 2.0s");
        assert_eq!(sb.fade_secs, 0.5, "500ms -> 0.5s");
    }

    /// A field from this batch authored ALONE - nothing else
    /// scrollbar-related - must still spawn `ScrollbarStyle`. The guard
    /// used to check only `scrollbar_color` / `scrollbar_width`, so e.g.
    /// `scrollbar-thickness` on its own never got a component at all -
    /// the same silent-drop shape as the `TextStyle` guard bug.
    #[test]
    fn scrollbar_thickness_alone_spawns_scrollbar_style() {
        let mut world = World::new();
        let mut column = el("column");
        column.attrs.scrollbar_thickness = Some(12.0);
        let e = spawn_subtree(&mut world, &column, None);

        assert!(
            world.get::<lumen_core::input::ScrollbarStyle>(e).is_some(),
            "scrollbar-thickness alone must spawn ScrollbarStyle"
        );
    }

    /// A plain element with no scrollbar properties at all must not gain
    /// a `ScrollbarStyle` - the "harmless on non-scroll entities" claim
    /// in the spawn comment depends on absence, not a zeroed component.
    #[test]
    fn no_scrollbar_properties_spawns_no_scrollbar_style() {
        let mut world = World::new();
        let e = spawn_subtree(&mut world, &el("column"), None);

        assert!(world.get::<lumen_core::input::ScrollbarStyle>(e).is_none());
    }
}
