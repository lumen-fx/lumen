//! An element's computed style: the components its resolved attributes
//! become, built one way for spawning and for a restyle.
//!
//! [`ComputedStyle::new`] turns the cascade's result for one element into
//! every style component it carries: the layout [`Style`], the paint, the
//! text style, the state patches, the widget palettes. Spawning inserts
//! them; a restyle [commits](ComputedStyle::commit) them over the live
//! components, which removes what the cascade no longer sets, keeps what a
//! runtime system holds in [`StyleOverrides`], and starts a `transition`
//! where one is declared.

use bevy_ecs::prelude::*;
use bevy_ecs::world::EntityWorldMut;
use lumen_core::components::{
    CaretWidth, Color, Fill, ImageFit, ImageRendering, Length, Opacity, PasswordCharacter,
    ShadowSpec, Style, TextInputPaint, TextStyle, Visuals, ZIndex,
};
use lumen_core::input::{Cursor, CursorImageRef, CursorKeyword, PointerEvents, ScrollbarStyle};
use lumen_core::style_overrides::{StyleOverrides, StyleValues};
use lumen_ir::layout_ir::{Attributes, CursorSpec, PointerEventsSpec, WidgetPart};
use lumen_primitives::{
    BackgroundTransition, BorderColorTransition, Interaction, OpacityTransition, StatePatch,
    StateVisuals, TextColorTransition, TransitionProperty, TransitionSpec, TransitionSpecs,
    retarget,
};

use crate::spawn::{DISABLED_OPACITY_DEFAULT, apply_ua_style_defaults, track_style_over};

/// The computed `cursor` for a resolved value: each image's path resolved
/// the way a `bg: url(...)` is.
fn cursor_of(spec: &CursorSpec) -> Cursor {
    Cursor {
        images: spec
            .images
            .iter()
            .map(|image| CursorImageRef {
                path: lumen_assets::resolve_source_path(&image.url),
                hotspot: image.hotspot,
            })
            .collect(),
        keyword: CursorKeyword::from_css(&spec.keyword).unwrap_or_default(),
    }
}

/// What an element is, beyond its styles, that decides how its styles
/// become components. Spawning reads it off the element's attributes; a
/// restyle reads it off the components spawning attached.
#[derive(Clone, Copy, Debug, Default)]
pub struct StyleContext<'a> {
    /// The element's tag.
    pub tag: &'a str,
    /// Disabled with no `bind-disabled` to change it: the `:disabled`
    /// styles apply as the element's own.
    pub static_disabled: bool,
    /// Carries `bind-disabled`: the `:disabled` styles become a state
    /// patch that comes and goes with the marker.
    pub bind_disabled: bool,
    /// The widget part the parser synthesized it as, if any.
    pub part: Option<WidgetPart>,
    /// A `<tabs>` strip button.
    pub tab_strip: bool,
    /// A `<dropdown>`'s header button.
    pub dropdown_button: bool,
    /// A `<radio>` that names its group and value.
    pub radio: bool,
    /// The gate a page of a multi-page app mounts inside.
    pub page_gate: bool,
    /// A virtualized `<for>` block.
    pub virtualized_for: bool,
    /// The element's parent is a scroll container.
    pub in_scroll: bool,
    /// Asked for a relayout boundary in markup (`layout-boundary`).
    pub layout_boundary: bool,
}

impl<'a> StyleContext<'a> {
    /// The context spawning reads off an element's attributes.
    pub fn of_attrs(tag: &'a str, attrs: &Attributes, in_scroll: bool) -> Self {
        Self {
            tag,
            static_disabled: attrs.disabled && attrs.bind_disabled.is_none(),
            bind_disabled: attrs.bind_disabled.is_some(),
            part: attrs.part,
            tab_strip: attrs.tab_strip.is_some(),
            dropdown_button: attrs.dropdown_button.is_some(),
            radio: tag == "radio" && attrs.radio_group.is_some() && attrs.radio_value.is_some(),
            page_gate: tag == "if"
                && attrs.if_signal.as_deref() == Some(lumen_core::nav::PATH_SIGNAL),
            virtualized_for: tag == "for" && attrs.virtualized,
            in_scroll,
            layout_boundary: attrs.layout_boundary,
        }
    }
}

/// Every style component one element carries, computed from its resolved
/// attributes. `None` is a component it does not carry.
#[derive(Clone, Debug)]
pub struct ComputedStyle {
    /// Layout.
    pub style: Style,
    /// Whether layout may stop its dirty walk at this element.
    pub relayout_boundary: bool,
    /// Paint.
    pub visuals: Option<Visuals>,
    /// Text.
    pub text: Option<TextStyle>,
    /// Opacity; absent is fully opaque.
    pub opacity: Option<f32>,
    /// Sibling paint order.
    pub z_index: Option<i32>,
    /// Hover / press tints, focus rings, state borders.
    pub interaction: Option<Interaction>,
    /// `:hover` / `:focus` / `:active` / `:drag-over` / `:disabled` patches.
    pub state_visuals: Option<StateVisuals>,
    /// The `transition` declarations.
    pub transitions: Vec<TransitionSpec>,
    /// Overlay scrollbar styling.
    pub scrollbar: Option<ScrollbarStyle>,
    /// `fit` for an image.
    pub image_fit: Option<ImageFit>,
    /// `image-rendering`, carried only where it is not `auto`.
    pub image_rendering: Option<ImageRendering>,
    /// `cursor`, carried where the element declares one.
    pub cursor: Option<Cursor>,
    /// `pointer-events`, carried where the element declares one.
    pub pointer_events: Option<PointerEvents>,
    /// Caret and selected-glyph paint of a text field.
    pub input_paint: Option<TextInputPaint>,
    /// Caret width of a text field.
    pub caret_width: Option<CaretWidth>,
    /// Mask character of a text field.
    pub password_character: Option<PasswordCharacter>,
    /// `caret-blink`, which lands on the one shared blink clock.
    pub caret_blink_ms: Option<u32>,
    /// A `<toggle>` / `<switch>` track palette.
    pub track: Option<lumen_primitives::TrackStyle>,
    /// A `<checkbox>`'s checked fill.
    pub checkbox: Option<lumen_primitives::CheckboxStyle>,
    /// A `<radio>`'s selected fill.
    pub radio: Option<lumen_primitives::RadioStyle>,
    /// A tab strip button's palette.
    pub tab_button: Option<lumen_primitives::TabButtonStyle>,
    /// An indeterminate `<progress>`'s sweep width.
    pub progress_chunk: Option<lumen_primitives::progress::ProgressChunk>,
    /// The gap a dropdown's panel opens at.
    pub popup_gap: Option<lumen_primitives::popup::PopupGap>,
    /// Knob and thumb geometry of a `<toggle>` / `<switch>` / `<slider>`.
    pub knob_geometry: Option<lumen_primitives::controls::KnobGeometry>,
    /// Knob and thumb fill.
    pub knob_fill: Color,
}

impl ComputedStyle {
    /// Compute the style components of an element from its resolved
    /// attributes.
    pub fn new(ctx: &StyleContext<'_>, attrs: &Attributes) -> Self {
        let tag = ctx.tag;
        let mut style = Style::from(attrs);
        apply_ua_style_defaults(tag, attrs, ctx.page_gate, &mut style);
        // The content of a scroll container keeps its own size instead of
        // being squeezed into the viewport - that is what there is to
        // scroll. Every layout here is flex, so without this a `height:
        // 900` block inside a 200px-tall scroller shrinks to 200 and the
        // scrollbar has nothing to travel over. Qt's `QScrollArea` keeps
        // its widget's size hint and Slint's `ScrollView` its content's
        // preferred size; on the web the same shape needs `flex-shrink: 0`
        // written by hand. An authored `shrink` still wins.
        if attrs.shrink.is_none() && ctx.in_scroll {
            style.shrink = 0.0;
        }
        // A virtualized block pins its rows with `position: absolute`
        // against itself, so left to shrink to content it would shrink to
        // nothing. It fills its scroll parent instead, which gives the rows
        // a real reference rect.
        if ctx.virtualized_for {
            style.width = Length::Percent(100.0);
        }
        let relayout_boundary = is_relayout_boundary(&style, ctx.layout_boundary, attrs);

        let track = matches!(tag, "toggle" | "switch")
            .then(|| track_style_over(lumen_primitives::TrackStyle::default(), attrs));
        let tab_button = ctx.tab_strip.then(|| {
            let defaults = lumen_primitives::TabButtonStyle::default();
            lumen_primitives::TabButtonStyle {
                selected_bg: attrs
                    .selected_bg
                    .map(Into::into)
                    .unwrap_or(defaults.selected_bg),
                unselected_bg: attrs
                    .bg
                    .as_ref()
                    .and_then(|b| Fill::try_from(b).ok()?.as_solid())
                    .unwrap_or(defaults.unselected_bg),
            }
        });

        // Paint, with the fallback each widget paints when no skin or
        // author rule styled it: a track, a groove, a field's chrome.
        let mut visuals = Option::<Visuals>::from(attrs).or_else(|| {
            let fallback = |fill: Color, radius: f32| Visuals {
                fill: Some(Fill::Solid(fill)),
                radius,
                corner_radii: None,
                shadows: Vec::new(),
                border: None,
            };
            use lumen_primitives::controls::{TOGGLE_CHECKED_BG, TOGGLE_UNCHECKED_BG};
            match (tag, ctx.part) {
                ("toggle", _) => Some(fallback(track?.unchecked_bg, 12.0)),
                ("switch", _) => Some(fallback(track?.unchecked_bg, 14.0)),
                ("slider", _) => Some(fallback(TOGGLE_UNCHECKED_BG, 4.0)),
                ("progress", _) => Some(fallback(TOGGLE_UNCHECKED_BG, 3.0)),
                (_, Some(WidgetPart::CheckboxBox)) => Some(fallback(TOGGLE_UNCHECKED_BG, 4.0)),
                (_, Some(WidgetPart::RadioDot)) => Some(fallback(TOGGLE_UNCHECKED_BG, 9.0)),
                (_, Some(WidgetPart::ProgressFill)) => Some(fallback(TOGGLE_CHECKED_BG, 3.0)),
                // An unstyled field is otherwise invisible, with no edge to
                // aim the caret at. A subtle translucent fill works on both
                // light and dark app themes.
                ("input" | "textarea", _) => Some(fallback(Color::rgba(0.5, 0.5, 0.5, 0.18), 4.0)),
                _ if ctx.tab_strip => Some(fallback(tab_button?.unselected_bg, 0.0)),
                _ => None,
            }
        });
        let mut text = Option::<TextStyle>::from(attrs);
        let mut opacity = attrs.opacity;

        // `disabled-opacity` (markup attr or the CSS property) wins;
        // `DISABLED_OPACITY_DEFAULT` is the single Rust-side fallback.
        let disabled_opacity_default = attrs
            .disabled_opacity_default
            .unwrap_or(DISABLED_OPACITY_DEFAULT);
        let state_visuals = state_visuals(ctx, attrs, disabled_opacity_default);
        if let Some(sv) = &state_visuals {
            // A state text-color swap needs a TextStyle slot to write into
            // even when the resting style authored none, and a state shadow
            // or background swap a Visuals.
            let patches = [
                &sv.hover,
                &sv.focus,
                &sv.focus_visible,
                &sv.active,
                &sv.drag_over,
                &sv.disabled,
            ];
            if patches.iter().any(|p| p.text_color.is_some()) && text.is_none() {
                text = Some(TextStyle::default());
            }
            let needs_visuals = patches.iter().any(|p| p.shadows.is_some())
                || sv.drag_over.bg.is_some()
                || sv.disabled.bg.is_some();
            if needs_visuals && visuals.is_none() {
                visuals = Some(Visuals::default());
            }
        }

        // A statically disabled element (no binding to re-enable it) takes
        // its `:disabled` styles as its own.
        if ctx.static_disabled {
            if let Some(c) = attrs.disabled_text_color {
                text.get_or_insert_with(TextStyle::default).color = c.into();
            }
            if let Some(o) = attrs.disabled_opacity {
                opacity = Some(o);
            }
            if let Some(shadows) = &attrs.disabled_shadows {
                visuals.get_or_insert_with(Visuals::default).shadows =
                    shadows.iter().copied().map(Into::into).collect();
            }
            if let Some(c) = attrs.disabled_bg {
                let v = visuals.get_or_insert_with(|| Visuals {
                    radius: attrs.radius.unwrap_or(0.0),
                    corner_radii: attrs.radius_corners,
                    ..Visuals::default()
                });
                v.fill = Some(Fill::Solid(c.into()));
            } else if attrs.opacity.is_none() && attrs.disabled_opacity.is_none() {
                // The default disabled look when no `:disabled { bg }` /
                // `:disabled { opacity }` rule supplied one: dim the whole
                // element.
                opacity = Some(disabled_opacity_default);
            }
        }

        let is_input = matches!(tag, "input" | "textarea");
        let knob = matches!(tag, "toggle" | "switch" | "slider");
        Self {
            style,
            relayout_boundary,
            visuals,
            text,
            opacity,
            z_index: attrs.z_index,
            interaction: Option::<Interaction>::from(attrs),
            state_visuals,
            transitions: attrs
                .effective_transitions()
                .iter()
                .map(Into::into)
                .collect(),
            scrollbar: scrollbar_style(attrs),
            image_fit: attrs.image_fit.map(ImageFit::from),
            image_rendering: attrs
                .image_rendering
                .map(ImageRendering::from)
                .filter(|r| *r != ImageRendering::Auto),
            cursor: attrs.cursor.as_ref().map(cursor_of),
            pointer_events: attrs.pointer_events.map(|p| match p {
                PointerEventsSpec::Auto => PointerEvents::Auto,
                PointerEventsSpec::None => PointerEvents::None,
            }),
            // Caret / selected-glyph paint rides its own component so
            // non-input text never carries the extra row; absent, the
            // renderer falls back (caret = text fill, selected glyphs keep
            // their fill).
            input_paint: (is_input
                && (attrs.caret_color.is_some() || attrs.selection_text_color.is_some()))
            .then(|| TextInputPaint {
                caret_color: attrs.caret_color.map(Into::into),
                selection_foreground: attrs.selection_text_color.map(Into::into),
            }),
            // Each component's own `Default` carries the runtime's
            // fallback when neither markup nor CSS set one.
            caret_width: is_input.then(|| attrs.caret_width.map(CaretWidth).unwrap_or_default()),
            password_character: is_input.then(|| {
                attrs
                    .password_character
                    .map(PasswordCharacter)
                    .unwrap_or_default()
            }),
            caret_blink_ms: is_input.then_some(attrs.caret_blink_ms).flatten(),
            track,
            checkbox: (tag == "checkbox").then(|| lumen_primitives::CheckboxStyle {
                // `checkbox:checked { bg }` routes here via checked_bg; the
                // accent constant is the single Rust fallback.
                checked_bg: attrs
                    .checked_bg
                    .map(Into::into)
                    .unwrap_or(lumen_primitives::CheckboxStyle::default().checked_bg),
            }),
            radio: ctx.radio.then(|| lumen_primitives::RadioStyle {
                // `radio:selected { bg }` routes here via selected_bg.
                selected_bg: attrs
                    .selected_bg
                    .map(Into::into)
                    .unwrap_or(lumen_primitives::RadioStyle::default().selected_bg),
            }),
            tab_button,
            progress_chunk: (tag == "progress").then(|| {
                attrs
                    .progress_chunk
                    .map(lumen_primitives::progress::ProgressChunk)
                    .unwrap_or_default()
            }),
            popup_gap: ctx.dropdown_button.then(|| {
                attrs
                    .popup_gap
                    .map(lumen_primitives::popup::PopupGap)
                    .unwrap_or_default()
            }),
            knob_geometry: knob.then(|| lumen_primitives::controls::KnobGeometry {
                inset: attrs
                    .knob_inset
                    .unwrap_or(lumen_primitives::controls::KNOB_INSET),
                thumb_size: attrs
                    .thumb_size
                    .unwrap_or(lumen_primitives::controls::THUMB_SIZE),
            }),
            // `knob-color` (markup attr or the CSS property, UA-seeded by
            // the skins) wins; `KNOB_FILL` is the single Rust fallback.
            knob_fill: attrs
                .knob_color
                .map(Into::into)
                .unwrap_or(lumen_primitives::controls::KNOB_FILL),
        }
    }

    /// The values a holder in [`StyleOverrides`] can hold, as this
    /// computes them.
    fn holdable(&self) -> StyleValues {
        let v = self.visuals.as_ref();
        StyleValues {
            display: Some(self.style.display),
            position: Some(self.style.position),
            inset: Some(self.style.inset),
            width: Some(self.style.width),
            height: Some(self.style.height),
            min_height: Some(self.style.min_height),
            padding: Some(self.style.padding),
            basis: Some(self.style.basis),
            fill: Some(v.and_then(|v| v.fill.clone())),
            shadows: Some(v.map(|v| v.shadows.clone()).unwrap_or_default()),
            border: Some(v.and_then(|v| v.border)),
            opacity: Some(self.opacity),
            text_color: self.text.as_ref().map(|t| t.color),
        }
    }

    /// Lay what runtime systems hold over this, so it is what the live
    /// components show.
    fn lay_over(&mut self, held: &StyleValues) {
        held.write_style(&mut &mut self.style);
        if held.fill.is_some() || held.shadows.is_some() || held.border.is_some() {
            held.write_visuals(&mut self.visuals.get_or_insert_with(Visuals::default));
        }
        if held.text_color.is_some()
            && let Some(text) = self.text.as_mut()
        {
            held.write_text(&mut &mut *text);
        }
        if let Some(o) = held.opacity {
            self.opacity = o;
        }
    }

    /// Insert every component onto a freshly spawned element.
    pub fn insert(self, ent: &mut EntityWorldMut<'_>) {
        if self.relayout_boundary {
            ent.insert(lumen_core::components::RelayoutBoundary);
        }
        ent.insert(self.style);
        if let Some(v) = self.visuals {
            ent.insert(v);
        }
        if let Some(t) = self.text {
            ent.insert(t);
        }
        if let Some(o) = self.opacity {
            ent.insert(Opacity(o));
        }
        if let Some(z) = self.z_index {
            ent.insert(ZIndex(z));
        }
        if let Some(ix) = self.interaction {
            ent.insert(ix);
        }
        if let Some(sv) = self.state_visuals {
            ent.insert(sv);
        }
        if !self.transitions.is_empty() {
            ent.insert(TransitionSpecs(self.transitions));
        }
        insert_some(ent, self.scrollbar);
        insert_some(ent, self.image_fit);
        insert_some(ent, self.image_rendering);
        insert_some(ent, self.cursor);
        insert_some(ent, self.pointer_events);
        insert_some(ent, self.input_paint);
        insert_some(ent, self.caret_width);
        insert_some(ent, self.password_character);
        insert_some(ent, self.track);
        insert_some(ent, self.checkbox);
        insert_some(ent, self.radio);
        insert_some(ent, self.tab_button);
        insert_some(ent, self.progress_chunk);
        insert_some(ent, self.popup_gap);
        insert_some(ent, self.knob_geometry);
        if let Some(ms) = self.caret_blink_ms {
            set_caret_blink(ent, ms);
        }
    }

    /// Replace a live element's style components with these.
    ///
    /// A component this does not carry is removed, so a property nothing
    /// sets any more goes back to its initial value. A value a runtime
    /// system holds in [`StyleOverrides`] stays on top, and the cascade's
    /// new value becomes what it falls back to when the system lets go.
    /// A property with a `transition` tweens from what is on screen to its
    /// new value when `animate`; a tween already running is retargeted
    /// from where it is. Each component is written only when it changed,
    /// so an unchanged restyle marks nothing changed.
    pub fn commit(mut self, ent: &mut EntityWorldMut<'_>, animate: bool) {
        let held = match ent.get_mut::<StyleOverrides>() {
            Some(mut ov) if !ov.is_empty() => {
                let fresh = self.holdable();
                Some(ov.rebase(&fresh))
            }
            _ => None,
        };
        let held_fields = held.as_ref().map(StyleValues::fields).unwrap_or_default();
        if let Some(held) = &held {
            self.lay_over(held);
        }

        // Transitions: the declarations land first, so the tween checks
        // below read the restyled list.
        let specs = TransitionSpecs(std::mem::take(&mut self.transitions));
        let spec_for = |p: TransitionProperty| {
            if !animate {
                return None;
            }
            specs
                .for_property(p)
                .filter(|s| !s.duration.is_zero())
                .copied()
        };
        let mut tweened = false;
        use lumen_core::style_overrides::StyleFields;

        // A property with a `transition` tweens from what is on screen
        // now toward its new value, and a tween already heading for that
        // value runs on undisturbed. A tween heading anywhere else is
        // retargeted from where it is, or retired when nothing declares a
        // transition any more. A field a system holds is the system's to
        // animate, so it never tweens here.
        let live_visuals = ent.get::<Visuals>().cloned();
        if !held_fields.contains(StyleFields::FILL) {
            let cur = live_visuals
                .as_ref()
                .and_then(|v| v.fill.as_ref())
                .and_then(Fill::as_solid);
            let new = self
                .visuals
                .as_ref()
                .and_then(|v| v.fill.as_ref())
                .and_then(Fill::as_solid);
            let running = ent.get::<BackgroundTransition>().map(|t| t.0.to);
            match (cur, new) {
                (Some(cur), Some(new)) if running == Some(new) => {
                    set_solid_fill(&mut self.visuals, cur);
                }
                (Some(cur), Some(new)) => {
                    match spec_for(TransitionProperty::BackgroundColor)
                        .and_then(|spec| retarget(cur, new, &spec))
                    {
                        Some(t) => {
                            ent.insert(BackgroundTransition(t));
                            set_solid_fill(&mut self.visuals, cur);
                            tweened = true;
                        }
                        None if running.is_some() => {
                            ent.remove::<BackgroundTransition>();
                        }
                        None => {}
                    }
                }
                _ if running.is_some() => {
                    ent.remove::<BackgroundTransition>();
                }
                _ => {}
            }
        }
        if !held_fields.contains(StyleFields::BORDER) {
            let cur = live_visuals.as_ref().and_then(|v| v.border);
            let new = self.visuals.as_ref().and_then(|v| v.border);
            let running = ent.get::<BorderColorTransition>().map(|t| t.0.to);
            match (cur, new) {
                (Some(cur), Some(new))
                    if cur.widths == new.widths && running == Some(new.color) =>
                {
                    set_border_color(&mut self.visuals, cur.color);
                }
                (Some(cur), Some(new)) if cur.widths == new.widths => {
                    match spec_for(TransitionProperty::BorderColor)
                        .and_then(|spec| retarget(cur.color, new.color, &spec))
                    {
                        Some(t) => {
                            ent.insert(BorderColorTransition(t));
                            set_border_color(&mut self.visuals, cur.color);
                            tweened = true;
                        }
                        None if running.is_some() => {
                            ent.remove::<BorderColorTransition>();
                        }
                        None => {}
                    }
                }
                _ if running.is_some() => {
                    ent.remove::<BorderColorTransition>();
                }
                _ => {}
            }
        }
        if !held_fields.contains(StyleFields::TEXT_COLOR) {
            let cur = ent.get::<TextStyle>().map(|t| t.color);
            let new = self.text.as_ref().map(|t| t.color);
            let running = ent.get::<TextColorTransition>().map(|t| t.0.to);
            match (cur, new, self.text.as_mut()) {
                (Some(cur), Some(new), Some(text)) if running == Some(new) => {
                    text.color = cur;
                }
                (Some(cur), Some(new), Some(text)) => {
                    match spec_for(TransitionProperty::TextColor)
                        .and_then(|spec| retarget(cur, new, &spec))
                    {
                        Some(t) => {
                            ent.insert(TextColorTransition(t));
                            text.color = cur;
                            tweened = true;
                        }
                        None if running.is_some() => {
                            ent.remove::<TextColorTransition>();
                        }
                        None => {}
                    }
                }
                _ if running.is_some() => {
                    ent.remove::<TextColorTransition>();
                }
                _ => {}
            }
        }
        // Opacity tweens the same way. No component is fully opaque, so an
        // element that had none starts from `1`, as in CSS, and a mount
        // fade-in that is still running carries on to its value.
        if !held_fields.contains(StyleFields::OPACITY) {
            let cur = ent.get::<Opacity>().map(|o| o.0);
            let target = self.opacity.unwrap_or(1.0);
            let running = ent.get::<OpacityTransition>().map(|t| t.0.to);
            if running == Some(target) {
                self.opacity = cur;
            } else {
                match spec_for(TransitionProperty::Opacity)
                    .and_then(|spec| retarget(cur.unwrap_or(1.0), target, &spec))
                {
                    Some(t) => {
                        ent.insert(OpacityTransition(t));
                        self.opacity = Some(cur.unwrap_or(1.0));
                        tweened = true;
                    }
                    None if running.is_some() => {
                        ent.remove::<OpacityTransition>();
                    }
                    None => {}
                }
            }
        }

        if specs.0.is_empty() {
            if ent.contains::<TransitionSpecs>() {
                ent.remove::<TransitionSpecs>();
            }
        } else if ent.get::<TransitionSpecs>() != Some(&specs) {
            ent.insert(specs);
        }

        // Layout. The layout crate's `Changed<Style>` watcher marks the
        // element dirty on any mutable access, so it is written only when
        // something moved.
        if ent.get::<Style>() != Some(&self.style) {
            ent.insert(self.style);
        }
        put_marker::<lumen_core::components::RelayoutBoundary>(ent, self.relayout_boundary);
        put(ent, self.visuals);
        put(ent, self.text);
        put(ent, self.opacity.map(Opacity));
        put(ent, self.z_index.map(ZIndex));
        put(ent, self.interaction);
        put(ent, self.state_visuals);
        put(ent, self.scrollbar);
        put(ent, self.image_fit);
        put(ent, self.image_rendering);
        put(ent, self.cursor);
        put(ent, self.pointer_events);
        put(ent, self.input_paint);
        put(ent, self.caret_width);
        put(ent, self.password_character);
        put(ent, self.track);
        put(ent, self.checkbox);
        put(ent, self.radio);
        put(ent, self.tab_button);
        put(ent, self.progress_chunk);
        put(ent, self.popup_gap);
        put(ent, self.knob_geometry);
        if let Some(ms) = self.caret_blink_ms {
            set_caret_blink(ent, ms);
        }
        if self.knob_geometry.is_some() {
            set_knob_fill(ent, self.knob_fill);
        }

        // Keep the loop awake while a tween runs; without the wake the app
        // can sleep mid-tween and freeze a theme flip halfway.
        if tweened {
            ent.world_scope(|world| {
                if let Some(anim) =
                    world.get_resource::<lumen_core::render_world::AnimationsActive>()
                {
                    anim.request();
                }
            });
        }
    }
}

/// The state patches for `:hover` / `:focus` / `:focus-visible` /
/// `:active` / `:drag-over`, and `:disabled` on an element whose disabled
/// state can change. `None` when no state styles anything.
fn state_visuals(
    ctx: &StyleContext<'_>,
    attrs: &Attributes,
    disabled_opacity_default: f32,
) -> Option<StateVisuals> {
    let to_shadows = |v: &Option<Vec<lumen_ir::layout_ir::ShadowSpec>>| {
        v.as_ref().map(|list| {
            list.iter()
                .copied()
                .map(Into::into)
                .collect::<Vec<ShadowSpec>>()
        })
    };
    // `:disabled` swaps at runtime only when the disabled state can change
    // at runtime, which is what `bind-disabled` says. Static disabled
    // markup takes those styles as its own instead.
    let disabled = if ctx.bind_disabled {
        StatePatch {
            text_color: attrs.disabled_text_color.map(Into::into),
            // The default disabled look when no `:disabled { bg }` /
            // `:disabled { opacity }` rule supplied one: dim the whole
            // element.
            opacity: attrs.disabled_opacity.or({
                if attrs.disabled_bg.is_none() && attrs.opacity.is_none() {
                    Some(disabled_opacity_default)
                } else {
                    None
                }
            }),
            shadows: to_shadows(&attrs.disabled_shadows),
            bg: attrs.disabled_bg.map(Into::into),
        }
    } else {
        StatePatch::default()
    };
    let sv = StateVisuals {
        hover: StatePatch {
            text_color: attrs.hover_text_color.map(Into::into),
            opacity: attrs.hover_opacity,
            shadows: to_shadows(&attrs.hover_shadows),
            bg: None,
        },
        focus: StatePatch {
            text_color: attrs.focus_text_color.map(Into::into),
            opacity: attrs.focus_opacity,
            shadows: to_shadows(&attrs.focus_shadows),
            bg: None,
        },
        focus_visible: StatePatch {
            text_color: attrs.focus_visible_text_color.map(Into::into),
            opacity: attrs.focus_visible_opacity,
            shadows: to_shadows(&attrs.focus_visible_shadows),
            bg: None,
        },
        active: StatePatch {
            text_color: attrs.active_text_color.map(Into::into),
            opacity: attrs.active_opacity,
            shadows: to_shadows(&attrs.active_shadows),
            bg: None,
        },
        drag_over: StatePatch {
            text_color: attrs.drag_over_text_color.map(Into::into),
            opacity: attrs.drag_over_opacity,
            shadows: to_shadows(&attrs.drag_over_shadows),
            bg: attrs.drag_over_bg.map(Into::into),
        },
        disabled,
    };
    (!sv.is_empty()).then_some(sv)
}

/// The overlay-scrollbar style the `scrollbar-*` properties describe, or
/// `None` when none is authored. The component's `Default` carries the
/// no-stylesheet fallback for every field left unset.
fn scrollbar_style(attrs: &Attributes) -> Option<ScrollbarStyle> {
    if attrs.scrollbar_color.is_none()
        && attrs.scrollbar_width.is_none()
        && attrs.scrollbar_thickness.is_none()
        && attrs.scrollbar_thickness_thin.is_none()
        && attrs.scrollbar_margin.is_none()
        && attrs.scrollbar_min_thumb.is_none()
        && attrs.scrollbar_track_hover.is_none()
        && attrs.scrollbar_hover_boost.is_none()
        && attrs.scrollbar_fade_delay_ms.is_none()
        && attrs.scrollbar_fade_duration_ms.is_none()
    {
        return None;
    }
    let mut sb = ScrollbarStyle::default();
    if let Some((thumb, track)) = attrs.scrollbar_color {
        sb.thumb = thumb.into();
        sb.track = track.map(Into::into);
    }
    if let Some(w) = attrs.scrollbar_width {
        sb.width = w.into();
    }
    if let Some(v) = attrs.scrollbar_thickness {
        sb.thickness = v;
    }
    if let Some(v) = attrs.scrollbar_thickness_thin {
        sb.thickness_thin = v;
    }
    if let Some(v) = attrs.scrollbar_margin {
        sb.margin = v;
    }
    if let Some(v) = attrs.scrollbar_min_thumb {
        sb.min_thumb = v;
    }
    if let Some(c) = attrs.scrollbar_track_hover {
        sb.hover_track = c.into();
    }
    if let Some(v) = attrs.scrollbar_hover_boost {
        sb.hover_boost = v;
    }
    // `ScrollbarStyle` keeps fade timings in seconds; the CSS properties
    // parse as milliseconds (`Nms` / `Ns`, shared with
    // `transition-duration`).
    if let Some(ms) = attrs.scrollbar_fade_delay_ms {
        sb.fade_delay_secs = ms as f32 / 1000.0;
    }
    if let Some(ms) = attrs.scrollbar_fade_duration_ms {
        sb.fade_secs = ms as f32 / 1000.0;
    }
    Some(sb)
}

/// Layout may stop its dirty walk at this element:
///
/// 1. the markup asked for it with `layout-boundary`;
/// 2. it is a scroll container, whose content size never feeds back into
///    its own box beyond what the already-known clip box allows;
/// 3. both `width` and `height` are fixed `Px` and the element cannot be
///    flex-shrunk (`shrink: 0`) - its size is then constraint-imposed, so
///    descendants can re-flow internally without changing the parent's
///    measured dimensions.
///
/// The `shrink: 0` half of rule 3 is what keeps the boundary sound. On an
/// overflowing line a shrinkable item is squeezed down to its automatic
/// minimum size, which is the smaller of its content's min-content size and
/// its authored width - so under the CSS default `flex-shrink: 1` a purely
/// internal change can still move the item's own outer box, and stopping
/// the dirty walk there would leave ancestors holding stale geometry.
///
/// Pattern: Flutter's `_relayoutBoundary`, set on `RenderObjects` that pass
/// the same constraint test.
fn is_relayout_boundary(style: &Style, layout_boundary: bool, attrs: &Attributes) -> bool {
    use lumen_core::components::Overflow;
    if layout_boundary {
        return true;
    }
    // `<scroll>` carries its axis on `attrs.scroll` and never sets the
    // `overflow` properties, so the style check alone missed the one tag
    // whose whole job is scrolling.
    if attrs.scroll.is_some()
        || matches!(style.overflow_x, Overflow::Scroll)
        || matches!(style.overflow_y, Overflow::Scroll)
    {
        return true;
    }
    matches!(style.width, Length::Px(_))
        && matches!(style.height, Length::Px(_))
        && style.shrink == 0.0
}

/// Point a solid fill at `color`, leaving any other fill alone.
fn set_solid_fill(visuals: &mut Option<Visuals>, color: Color) {
    if let Some(Some(Fill::Solid(slot))) = visuals.as_mut().map(|v| v.fill.as_mut()) {
        *slot = color;
    }
}

/// Point a border's color at `color`.
fn set_border_color(visuals: &mut Option<Visuals>, color: Color) {
    if let Some(Some(border)) = visuals.as_mut().map(|v| v.border.as_mut()) {
        border.color = color;
    }
}

fn insert_some<C: Component>(ent: &mut EntityWorldMut<'_>, c: Option<C>) {
    if let Some(c) = c {
        ent.insert(c);
    }
}

/// Make the element carry `want`, or not carry the component at all,
/// writing only on a change.
fn put<C: Component + PartialEq>(ent: &mut EntityWorldMut<'_>, want: Option<C>) {
    match want {
        Some(c) => {
            if ent.get::<C>() != Some(&c) {
                ent.insert(c);
            }
        }
        None => {
            if ent.contains::<C>() {
                ent.remove::<C>();
            }
        }
    }
}

fn put_marker<C: Component + Default>(ent: &mut EntityWorldMut<'_>, want: bool) {
    match (want, ent.contains::<C>()) {
        (true, false) => {
            ent.insert(C::default());
        }
        (false, true) => {
            ent.remove::<C>();
        }
        _ => {}
    }
}

/// `caret-blink` lands on the one shared blink clock: one phase drives
/// every focused field's caret, so there is no per-element value to keep.
fn set_caret_blink(ent: &mut EntityWorldMut<'_>, ms: u32) {
    ent.world_scope(|world| {
        if let Some(mut blink) = world.get_resource_mut::<lumen_core::components::CaretBlink>() {
            let period = std::time::Duration::from_millis(u64::from(ms));
            if blink.period != period {
                blink.period = period;
            }
        }
    });
}

/// Repaint the knob or thumb child a `<toggle>` / `<switch>` / `<slider>`
/// spawned. It carries no tag of its own, so no rule reaches it; the
/// track's `knob-color` is its fill.
fn set_knob_fill(ent: &mut EntityWorldMut<'_>, fill: Color) {
    let id = ent.id();
    ent.world_scope(|world| {
        let children: Vec<Entity> = world
            .get::<bevy_ecs::hierarchy::Children>(id)
            .map(|c| c.iter().collect())
            .unwrap_or_default();
        for child in children {
            let is_knob = world.get::<lumen_primitives::ToggleKnob>(child).is_some()
                || world.get::<lumen_primitives::SliderThumb>(child).is_some()
                || world.get::<lumen_primitives::SwitchThumb>(child).is_some();
            if is_knob
                && let Some(mut v) = world.get_mut::<Visuals>(child)
                && v.fill != Some(Fill::Solid(fill))
            {
                v.fill = Some(Fill::Solid(fill));
            }
        }
    });
}

#[cfg(test)]
mod tests {
    //! What one element's resolved attributes become, and how a restyle
    //! lays a new resolution over the live components. The cascade itself
    //! is exercised in `lumen_ir::css`; the whole restyle in the runtime's
    //! `restyle_dynamic` tests.
    use super::*;
    use lumen_core::components::{CaretBlink, LineHeightSpec as CoreLineHeightSpec, TextInput};
    use lumen_core::style_overrides::{Live, StyleOwner};
    use lumen_ir::layout_ir::{BgSpec, LineHeightSpec as IrLineHeightSpec, Rgba};

    fn ctx(tag: &str) -> StyleContext<'_> {
        StyleContext {
            tag,
            ..StyleContext::default()
        }
    }

    fn rgba(r: f32, g: f32, b: f32) -> Rgba {
        Rgba { r, g, b, a: 1.0 }
    }

    /// Commit `attrs` as the resolution of a `tag` element.
    fn commit(world: &mut World, e: Entity, tag: &str, attrs: &Attributes) {
        ComputedStyle::new(&ctx(tag), attrs).commit(&mut world.entity_mut(e), true);
    }

    #[test]
    fn line_height_reaches_the_text_style_in_both_units() {
        for (ir, core) in [
            (
                IrLineHeightSpec::Multiplier(1.5),
                CoreLineHeightSpec::Multiplier(1.5),
            ),
            (IrLineHeightSpec::Px(19.0), CoreLineHeightSpec::Px(19.0)),
        ] {
            let attrs = Attributes {
                line_height: Some(ir),
                ..Default::default()
            };
            let text = ComputedStyle::new(&ctx("label"), &attrs)
                .text
                .expect("a text style");
            assert_eq!(text.line_height, Some(core));
        }
    }

    /// `caret-width`, `password-character` and `caret-blink` are a text
    /// field's: anything else never carries them, and never repaints the
    /// shared blink clock.
    #[test]
    fn text_field_properties_reach_only_text_fields() {
        let attrs = Attributes {
            caret_width: Some(3.5),
            password_character: Some('#'),
            caret_blink_ms: Some(250),
            ..Default::default()
        };
        let mut world = World::new();
        world.insert_resource(CaretBlink {
            visible: true,
            phase: std::time::Instant::now(),
            period: std::time::Duration::from_millis(530),
        });
        let plain = world.spawn(Style::default()).id();
        commit(&mut world, plain, "label", &attrs);
        assert!(world.get::<CaretWidth>(plain).is_none());
        assert!(world.get::<PasswordCharacter>(plain).is_none());
        assert_eq!(
            world.resource::<CaretBlink>().period,
            std::time::Duration::from_millis(530),
        );

        let input = world.spawn((Style::default(), TextInput::default())).id();
        commit(&mut world, input, "input", &attrs);
        assert_eq!(world.get::<CaretWidth>(input), Some(&CaretWidth(3.5)));
        assert_eq!(
            world.get::<PasswordCharacter>(input),
            Some(&PasswordCharacter('#'))
        );
        assert_eq!(
            world.resource::<CaretBlink>().period,
            std::time::Duration::from_millis(250)
        );
    }

    /// A restyle writes every layout property the cascade resolved, exactly
    /// as spawning would have: justify, align, position, inset, overflow and
    /// the grid properties were once missing from the restyle path.
    #[test]
    fn a_restyle_builds_the_layout_spawning_builds() {
        use lumen_ir::layout_ir::{
            DisplaySpec, Edges, FlexAlign, FlexJustify, LengthSpec, OverflowSpec, PositionSpec,
        };
        let attrs = Attributes {
            display: Some(DisplaySpec::Grid),
            width: Some(LengthSpec::Px(120.0)),
            align: Some(FlexAlign::Center),
            justify: Some(FlexJustify::End),
            position: Some(PositionSpec::Absolute),
            inset: Some(Edges::all(3.0)),
            overflow_y: Some(OverflowSpec::Scroll),
            ..Default::default()
        };
        let mut world = World::new();
        let e = world.spawn(Style::default()).id();
        commit(&mut world, e, "tile", &attrs);
        assert_eq!(
            world.get::<Style>(e),
            Some(&ComputedStyle::new(&ctx("tile"), &attrs).style),
            "restyle and spawn agree"
        );
        assert_eq!(world.get::<Style>(e).unwrap(), &Style::from(&attrs));
    }

    /// What the cascade no longer sets is gone after a restyle, as in CSS:
    /// the paint, the text style, the tints, the layout properties.
    #[test]
    fn a_restyle_removes_what_nothing_sets_any_more() {
        use lumen_ir::layout_ir::{FlexJustify, LengthSpec};
        let styled = Attributes {
            bg: Some(BgSpec::Solid(rgba(1.0, 0.0, 0.0))),
            hover_bg: Some(rgba(0.0, 1.0, 0.0)),
            text_color: Some(rgba(0.0, 0.0, 1.0)),
            opacity: Some(0.5),
            justify: Some(FlexJustify::End),
            height: Some(LengthSpec::Px(40.0)),
            ..Default::default()
        };
        let mut world = World::new();
        let e = world.spawn_empty().id();
        ComputedStyle::new(&ctx("row"), &styled).insert(&mut world.entity_mut(e));
        assert!(world.get::<Visuals>(e).is_some());

        commit(&mut world, e, "row", &Attributes::default());
        assert!(world.get::<Visuals>(e).is_none(), "no bg, no paint");
        assert!(world.get::<TextStyle>(e).is_none(), "no text style");
        assert!(world.get::<Interaction>(e).is_none(), "no hover tint");
        assert!(world.get::<Opacity>(e).is_none(), "fully opaque again");
        let style = world.get::<Style>(e).unwrap();
        assert_eq!(style.justify, Style::default().justify);
        assert_eq!(style.height, Length::Auto);
    }

    struct Holder;
    impl StyleOwner for Holder {
        const RANK: u8 = 40;
    }

    /// A value a runtime system holds stays on screen through a restyle,
    /// and the new resolution is what it lets go onto.
    #[test]
    fn a_held_value_survives_a_restyle_and_lets_go_onto_the_new_one() {
        let old = Attributes {
            bg: Some(BgSpec::Solid(rgba(0.0, 0.0, 0.0))),
            ..Default::default()
        };
        let mut world = World::new();
        let e = world.spawn(StyleOverrides::default()).id();
        ComputedStyle::new(&ctx("button"), &old).insert(&mut world.entity_mut(e));
        let tint = Fill::Solid(Color::rgb(1.0, 0.0, 0.0));
        let mut ent = world.entity_mut(e);
        let vis = ent.get::<Visuals>().unwrap().clone();
        let out = ent.get_mut::<StyleOverrides>().unwrap().hold::<Holder>(
            StyleValues {
                fill: Some(Some(tint.clone())),
                ..Default::default()
            },
            &Live {
                visuals: Some(&vis),
                ..Live::default()
            },
        );
        out.write_visuals(&mut ent.get_mut::<Visuals>().unwrap());

        let blue = rgba(0.0, 0.0, 1.0);
        let new = Attributes {
            bg: Some(BgSpec::Solid(blue)),
            ..Default::default()
        };
        commit(&mut world, e, "button", &new);
        assert_eq!(
            world.get::<Visuals>(e).unwrap().fill,
            Some(tint),
            "the held tint stays on top"
        );
        let mut ent = world.entity_mut(e);
        let out = ent.get_mut::<StyleOverrides>().unwrap().release::<Holder>();
        out.write_visuals(&mut ent.get_mut::<Visuals>().unwrap());
        assert_eq!(
            world.get::<Visuals>(e).unwrap().fill,
            Some(Fill::Solid(blue.into())),
            "letting go lands on the restyled fill"
        );
    }

    /// An element with an opacity transition and no authored opacity fades
    /// from the computed `1` the first time a rule gives it one.
    #[test]
    fn an_opacity_transition_starts_from_one_when_none_was_authored() {
        use lumen_ir::layout_ir::{EasingIr, TransitionIr, TransitionPropertyIr};
        let mut world = World::new();
        let e = world.spawn(Style::default()).id();
        let attrs = Attributes {
            opacity: Some(0.2),
            transitions: vec![TransitionIr {
                property: TransitionPropertyIr::Opacity,
                duration_ms: 6000,
                easing: EasingIr::Linear,
            }],
            ..Default::default()
        };
        commit(&mut world, e, "tile", &attrs);
        let tween = world
            .get::<OpacityTransition>(e)
            .expect("the change tweens");
        assert_eq!((tween.0.from, tween.0.to), (1.0, 0.2));
        assert_eq!(world.get::<Opacity>(e), Some(&Opacity(1.0)));
    }

    /// A tween already heading for the value a restyle resolves keeps
    /// running from where it is rather than restarting or snapping.
    #[test]
    fn a_running_tween_toward_the_same_value_runs_on() {
        use lumen_ir::layout_ir::{EasingIr, TransitionIr, TransitionPropertyIr};
        let red = rgba(1.0, 0.0, 0.0);
        let attrs = Attributes {
            bg: Some(BgSpec::Solid(red)),
            transitions: vec![TransitionIr {
                property: TransitionPropertyIr::BackgroundColor,
                duration_ms: 6000,
                easing: EasingIr::Linear,
            }],
            ..Default::default()
        };
        let mut world = World::new();
        let e = world.spawn_empty().id();
        let start = Attributes {
            bg: Some(BgSpec::Solid(rgba(0.0, 0.0, 0.0))),
            ..attrs.clone()
        };
        ComputedStyle::new(&ctx("tile"), &start).insert(&mut world.entity_mut(e));
        commit(&mut world, e, "tile", &attrs);
        let first = world.get::<BackgroundTransition>(e).expect("a tween").0;
        assert_eq!(first.to, red.into());
        commit(&mut world, e, "tile", &attrs);
        let again = world
            .get::<BackgroundTransition>(e)
            .expect("still tweening")
            .0;
        assert_eq!(again.start, first.start, "the same tween, not a restart");
        assert_eq!(
            world.get::<Visuals>(e).unwrap().fill,
            Some(Fill::Solid(Color::rgb(0.0, 0.0, 0.0))),
            "the tween, not the restyle, moves the fill"
        );
    }

    /// A restyle that resolves nothing new leaves `Style` untouched, so it
    /// does not mark the element's layout dirty.
    #[test]
    fn an_unchanged_restyle_leaves_style_unchanged() {
        let mut world = World::new();
        let e = world.spawn(Style::default()).id();
        world.clear_trackers();
        let tick = world.change_tick();
        commit(&mut world, e, "tile", &Attributes::default());
        let changed = world
            .entity(e)
            .get_change_ticks::<Style>()
            .unwrap()
            .is_changed(tick, world.change_tick());
        assert!(!changed, "an unchanged restyle must not touch Style");
    }
}
