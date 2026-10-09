//! IR -> runtime-component conversions.
//!
//! These `From` impls turn resolved [`Attributes`](crate::layout_ir::Attributes)
//! (and the `BgSpec` / `ShadowSpec` / `OutlineSpec` value types) into their
//! `lumen_core` / `lumen_primitives` component counterparts. They live here -
//! not in `lumenc`'s `spawn` - because the orphan rule requires the impl to
//! sit in the crate that defines the IR source types, exactly like the
//! spec->component `From` impls in [`layout_ir`](crate::layout_ir). The
//! `lumenc` spawn walker calls them via `Style::from(&attrs)` / `.into()`.

use crate::layout_ir::Attributes;
use lumen_core::components::{Fill, FlexDirection, Gap, ShadowSpec, Style, TextStyle, Visuals};

/// Resolve a Material 3-flavored typography role name to a pixel
/// font size. Returns `None` for unknown names so the caller can
/// fall back to the default 16 px.
///
/// This table stays in Rust rather than moving to `skins/ua.css` with
/// the rest of the user-agent defaults: the cascade in `lumen_ir::css`
/// matches selectors on tag, id, class, and pseudo-class only, with no
/// attribute-value selector. There is no CSS rule that can say "an
/// element carrying `style-role="display-xl"` gets `font-size: 128px`" -
/// the mapping from the role keyword to its pixel size has to happen in
/// code, the same way a keyword like `font-weight: bold` resolves to a
/// numeric weight.
pub fn typography_role_to_px(role: &str) -> Option<f32> {
    Some(match role {
        "display-xl" => 128.0,
        "display-lg" => 96.0,
        "display-md" => 64.0,
        "display-sm" => 48.0,
        "headline-lg" => 40.0,
        "headline-md" => 32.0,
        "headline-sm" => 26.0,
        "title-lg" => 22.0,
        "title-md" => 18.0,
        "title-sm" => 16.0,
        "body-lg" => 18.0,
        "body-md" => 16.0,
        "body-sm" => 14.0,
        "label-lg" => 14.0,
        "label-md" => 13.0,
        "label-sm" => 12.0,
        "caption" => 13.0,
        "overline" => 11.0,
        _ => return None,
    })
}

impl From<&Attributes> for Style {
    /// The element's layout style from its resolved attributes: the CSS
    /// initial values, with every property the attributes set written over
    /// them by [`Attributes::patch_style`].
    fn from(attrs: &Attributes) -> Self {
        let mut style = Style::default();
        attrs.patch_style(&mut style);
        style
    }
}

impl Attributes {
    /// Write every layout property these attributes set onto `style`,
    /// leaving the fields they leave unset as they are.
    ///
    /// This is the one mapping from attributes to [`Style`]. Spawning an
    /// element applies it to the initial values (`Style::from`), and a
    /// restyle (a class change, a `@media` breakpoint, a theme flip)
    /// applies the re-resolved cascade to the live component, so the two
    /// can never disagree about which properties reach layout.
    pub fn patch_style(&self, style: &mut Style) {
        if let Some(d) = self.display {
            style.display = d.into();
        }
        if let Some(w) = self.width {
            style.width = w.into();
        }
        if let Some(h) = self.height {
            style.height = h.into();
        }
        if let Some(f) = self.flex {
            style.flex_direction = FlexDirection::from(f);
        }
        if let Some(p) = self.padding {
            style.padding = p.into();
        }
        if let Some(m) = self.margin {
            style.margin = m.into();
        }
        // `gap` sets both axes; `row-gap` / `column-gap` then override
        // their own axis. Percent gaps ride in the `Gap` pct slots.
        if let Some(v) = self.gap {
            style.gap = Gap::from(v);
        }
        if let Some(r) = self.gap_row {
            style.gap.row = r;
        }
        if let Some(c) = self.gap_column {
            style.gap.column = c;
        }
        if let Some(p) = self.gap_row_pct.or(self.gap_pct) {
            style.gap.row_pct = Some(p);
        }
        if let Some(p) = self.gap_column_pct.or(self.gap_pct) {
            style.gap.column_pct = Some(p);
        }
        if let Some(g) = self.grow {
            style.grow = g;
        }
        if let Some(a) = self.align {
            style.align = a.into();
        }
        if let Some(j) = self.justify {
            style.justify = j.into();
        }
        if let Some(a) = self.align_self {
            style.align_self = Some(a.into());
        }
        if let Some(j) = self.justify_items {
            style.justify_items = Some(j.into());
        }
        if let Some(j) = self.justify_self {
            style.justify_self = Some(j.into());
        }
        if let Some(t) = self.grid_template.as_ref() {
            style.grid_template = Some(t.into());
        }
        if let Some(r) = self.grid_row {
            style.grid_row = r;
        }
        if let Some(c) = self.grid_column {
            style.grid_column = c;
        }
        if let Some(p) = self.position {
            style.position = p.into();
        }
        if let Some(i) = self.inset {
            style.inset = i.into();
        }
        if let Some(v) = self.min_width {
            style.min_width = v.into();
        }
        if let Some(v) = self.min_height {
            style.min_height = v.into();
        }
        if let Some(v) = self.max_width {
            style.max_width = v.into();
        }
        if let Some(v) = self.max_height {
            style.max_height = v.into();
        }
        if self.aspect_ratio.is_some() {
            style.aspect_ratio = self.aspect_ratio;
        }
        // `overflow` is shorthand for both axes; `overflow-x` /
        // `overflow-y` override their own axis.
        if let Some(o) = self.overflow_x.or(self.overflow) {
            style.overflow_x = o.into();
        }
        if let Some(o) = self.overflow_y.or(self.overflow) {
            style.overflow_y = o.into();
        }
        if let Some(s) = self.shrink {
            style.shrink = s;
        }
        if let Some(b) = self.basis {
            style.basis = b.into();
        }
        if let Some(w) = self.flex_wrap {
            style.flex_wrap = w.into();
        }
        if let Some(a) = self.align_content {
            style.align_content = Some(a.into());
        }
        // CSS border-style folds into computed widths: no solid style =>
        // zero widths (no layout space, no paint).
        if self.border_style.is_some() || self.border_width.is_some() {
            style.border = self
                .effective_border()
                .map(|(widths, _)| widths.into())
                .unwrap_or_default();
        }
        if let Some(b) = self.box_sizing {
            style.box_sizing = b.into();
        }
    }
}

/// `None` when the element carries no visible-rect inputs (no fill,
/// no radius, no shadow). Keeping `Visuals` absent for plain layout
/// containers preserves the "only entities that paint get a Visuals"
/// invariant the render extract relies on.
impl From<&Attributes> for Option<Visuals> {
    fn from(attrs: &Attributes) -> Self {
        let fill = attrs.bg.as_ref().and_then(|bg| Fill::try_from(bg).ok());
        let radius = attrs.radius.unwrap_or(0.0);
        let corner_radii = attrs.radius_corners;
        let shadows: Vec<ShadowSpec> = attrs.shadows.iter().copied().map(Into::into).collect();
        let border =
            attrs
                .effective_border()
                .map(|(widths, color)| lumen_core::components::Border {
                    widths: widths.into(),
                    color: color.into(),
                    side_colors: attrs
                        .effective_border_colors(color)
                        .map(|cs| cs.map(Into::into)),
                });
        if fill.is_none()
            && radius == 0.0
            && corner_radii.is_none()
            && shadows.is_empty()
            && border.is_none()
        {
            return None;
        }
        Some(Visuals {
            fill,
            radius,
            corner_radii,
            shadows,
            border,
        })
    }
}

/// A colour or gradient `bg` is a [`Fill`]. An image is not one: it paints
/// through the asset pipeline instead, and the conversion hands back the path
/// it names.
impl<'a> TryFrom<&'a crate::layout_ir::BgSpec> for Fill {
    type Error = &'a str;

    fn try_from(spec: &'a crate::layout_ir::BgSpec) -> Result<Self, Self::Error> {
        Ok(match spec {
            crate::layout_ir::BgSpec::Image(path) => return Err(path),
            crate::layout_ir::BgSpec::Solid(rgba) => Fill::Solid((*rgba).into()),
            crate::layout_ir::BgSpec::Linear { angle_deg, stops } => Fill::Linear {
                angle_deg: *angle_deg,
                stops: stops.iter().map(|(o, c)| (*o, (*c).into())).collect(),
            },
            crate::layout_ir::BgSpec::Radial { radius, stops } => Fill::Radial {
                radius: *radius,
                stops: stops.iter().map(|(o, c)| (*o, (*c).into())).collect(),
            },
            crate::layout_ir::BgSpec::Conic { from_deg, stops } => Fill::Conic {
                from_deg: *from_deg,
                stops: stops.iter().map(|(o, c)| (*o, (*c).into())).collect(),
            },
        })
    }
}

impl From<crate::layout_ir::ShadowSpec> for ShadowSpec {
    fn from(s: crate::layout_ir::ShadowSpec) -> Self {
        ShadowSpec {
            offset_x: s.offset_x,
            offset_y: s.offset_y,
            blur: s.blur,
            spread: s.spread,
            color: s.color.into(),
            inner: s.inner,
        }
    }
}

/// `None` when the element carries no text-style overrides (no color,
/// no font size, no align, no wrap, no max-lines, no typography role,
/// no line-height, ...). Absent => defaults apply at extract time -
/// `<column>` / `<row>` containers stay component-light.
impl From<&Attributes> for Option<TextStyle> {
    fn from(attrs: &Attributes) -> Self {
        let role_size = attrs.style_role.as_deref().and_then(typography_role_to_px);
        let size_px = attrs.font_size.or(role_size);
        let ellipsis = matches!(
            attrs.text_overflow,
            Some(crate::layout_ir::TextOverflowSpec::Ellipsis)
        );
        if attrs.text_color.is_none()
            && size_px.is_none()
            && attrs.text_align.is_none()
            && attrs.text_wrap.is_none()
            && attrs.max_lines.is_none()
            && !ellipsis
            && attrs.font_family.is_none()
            && attrs.font_weight.is_none()
            && attrs.selection_color.is_none()
            && attrs.line_height.is_none()
        {
            return None;
        }
        let defaults = TextStyle::default();
        // `lumen_ir::layout_ir::LineHeightSpec` -> `lumen_core::components::LineHeightSpec`,
        // a 1:1 variant mapping. `lumen-core` cannot depend on `lumen-ir`
        // (the dependency would cycle: `lumen-ir` already depends on
        // `lumen-core`), so the two enums are separate types kept in sync
        // by hand; `run::restyle`'s live-reapply path does the same
        // conversion at its own IR/ECS boundary.
        let line_height = attrs.line_height.map(|lh| match lh {
            crate::layout_ir::LineHeightSpec::Multiplier(m) => {
                lumen_core::components::LineHeightSpec::Multiplier(m)
            }
            crate::layout_ir::LineHeightSpec::Px(px) => {
                lumen_core::components::LineHeightSpec::Px(px)
            }
        });
        // `text-overflow: ellipsis` lowers onto the existing runtime
        // wrap machinery: glyph-wrap at the container width with a
        // 1-line cap makes the shaper truncate and append `...` (the
        // trim-to-fit pass in lumen-text-cosmic keeps the ellipsis
        // inside the box). Author-supplied `wrap` / `max-lines` win -
        // a multi-line clamp (`wrap="word" max-lines="2"`) is already
        // ellipsized by the same shaper path.
        let (wrap, max_lines) = if ellipsis {
            (
                attrs
                    .text_wrap
                    .map(Into::into)
                    .unwrap_or(lumen_core::components::TextWrap::Glyph),
                Some(attrs.max_lines.unwrap_or(1)),
            )
        } else {
            (
                attrs.text_wrap.map(Into::into).unwrap_or(defaults.wrap),
                attrs.max_lines,
            )
        };
        Some(TextStyle {
            color: attrs.text_color.map(Into::into).unwrap_or(defaults.color),
            size_px: size_px.unwrap_or(defaults.size_px),
            align: attrs.text_align.map(Into::into).unwrap_or(defaults.align),
            wrap,
            max_lines,
            family: attrs
                .font_family
                .as_deref()
                .map(std::sync::Arc::<str>::from),
            weight: attrs.font_weight.unwrap_or(defaults.weight),
            selection_color: attrs.selection_color.map(Into::into),
            line_height,
        })
    }
}

/// `None` when the element opts into no interaction state (no
/// `hover-bg`, no `press-bg`, no `focus-outline`). Keeping the
/// component absent for non-interactive surfaces avoids one row in the
/// inspector and one archetype move per spawn.
impl From<&Attributes> for Option<lumen_primitives::Interaction> {
    fn from(attrs: &Attributes) -> Self {
        let hover_tint = attrs.hover_bg.map(Into::into);
        let press_tint = attrs.press_bg.map(Into::into);
        let focus_outline = attrs.focus_outline.map(Into::into);
        let focus_visible_outline = attrs.focus_visible_outline.map(Into::into);
        let hover_border = attrs.hover_border.map(Into::into);
        let focus_border = attrs.focus_border.map(Into::into);
        if hover_tint.is_none()
            && press_tint.is_none()
            && focus_outline.is_none()
            && focus_visible_outline.is_none()
            && hover_border.is_none()
            && focus_border.is_none()
        {
            return None;
        }
        Some(lumen_primitives::Interaction {
            hover_tint,
            press_tint,
            focus_outline,
            focus_visible_outline,
            hover_border,
            focus_border,
        })
    }
}

/// The slider an element's `min`, `max`, `value` and `step` describe.
///
/// The defaults are the range a `<slider>` has when the markup names none:
/// zero to one, starting at the bottom. They live here rather than at each
/// reader, because a page emitted from one set of defaults and adopted by an
/// app holding another shows the thumb in two places.
impl From<&Attributes> for lumen_core::components::SliderValue {
    fn from(attrs: &Attributes) -> Self {
        let min = attrs.min.unwrap_or(0.0);
        let max = attrs.max.unwrap_or(1.0);
        let slider = Self {
            value: 0.0,
            min,
            max,
            step: attrs.step,
        };
        Self {
            value: slider.clamp(attrs.value.unwrap_or(min)),
            ..slider
        }
    }
}

/// The progress bar an element's `value`, `max` and `duration` describe.
///
/// No `value` is an indeterminate bar, which is a bar with nothing to say
/// about where it is rather than one sitting at zero. A `bind-value` write
/// turns it determinate.
impl From<&Attributes> for lumen_primitives::ProgressBar {
    fn from(attrs: &Attributes) -> Self {
        Self {
            value: attrs.value,
            max: attrs.max.unwrap_or(1.0),
            period_ms: attrs
                .progress_duration
                .unwrap_or(lumen_primitives::PROGRESS_PERIOD_MS),
        }
    }
}

impl From<crate::layout_ir::OutlineSpec> for lumen_primitives::FocusOutlineSpec {
    fn from(spec: crate::layout_ir::OutlineSpec) -> Self {
        lumen_primitives::FocusOutlineSpec {
            width: spec.width,
            color: spec.color.into(),
            offset: spec.offset,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::layout_ir::LineHeightSpec as IrLineHeightSpec;
    use lumen_core::components::LineHeightSpec as CoreLineHeightSpec;

    /// The range a `<slider>` gets when the markup names none. Every reader
    /// of a slider's bounds comes through this conversion, so the defaults
    /// are pinned here rather than at each of them: the spawner builds the
    /// widget with it and the web emitter writes the page with it, and a page
    /// written from a different range than the app holds shows the thumb in
    /// the wrong place.
    #[test]
    fn a_slider_with_no_bounds_runs_from_zero_to_one() {
        let slider = lumen_core::components::SliderValue::from(&Attributes::default());
        assert_eq!((slider.min, slider.max), (0.0, 1.0));
        assert_eq!(slider.value, 0.0, "and starts at the bottom of its range");
        assert_eq!(slider.step, None);
    }

    #[test]
    fn a_slider_starts_inside_its_own_bounds() {
        let attrs = Attributes {
            min: Some(0.0),
            max: Some(100.0),
            value: Some(250.0),
            ..Attributes::default()
        };
        assert_eq!(
            lumen_core::components::SliderValue::from(&attrs).value,
            100.0,
            "an authored value past the top is held to it, not carried"
        );
    }

    /// A bar with nothing to say about where it is, which is what an
    /// indeterminate `<progress>` is. Zero would be a bar saying it has not
    /// started, which is a different claim.
    #[test]
    fn a_progress_bar_with_no_value_is_indeterminate() {
        let bar = lumen_primitives::ProgressBar::from(&Attributes::default());
        assert_eq!(bar.value, None);
        assert_eq!(bar.max, 1.0);
        assert_eq!(bar.period_ms, lumen_primitives::PROGRESS_PERIOD_MS);
    }

    /// A `line-height` alone - no color, size, align, wrap, max-lines,
    /// family, weight, or selection-color - must still produce a
    /// `TextStyle`. The early-return guard in `Option<&Attributes> for
    /// Option<TextStyle>` used to check every OTHER text property but not
    /// `line_height`, so an element authoring only `line-height` returned
    /// `None` and the value was silently dropped instead of reaching the
    /// spawned entity.
    #[test]
    fn line_height_alone_produces_a_text_style() {
        let attrs = Attributes {
            line_height: Some(IrLineHeightSpec::Multiplier(1.5)),
            ..Attributes::default()
        };
        let ts = Option::<TextStyle>::from(&attrs).expect("line-height alone must produce Some");
        assert_eq!(
            ts.line_height,
            Some(CoreLineHeightSpec::Multiplier(1.5)),
            "the authored value must survive, not just trigger Some(..)"
        );
    }

    /// The `Px` variant survives the `lumen_ir` -> `lumen_core` conversion
    /// too - the two `LineHeightSpec` enums are distinct types (`lumen-core`
    /// cannot depend on `lumen-ir`), converted 1:1 by hand at this
    /// boundary, so each variant needs its own coverage.
    #[test]
    fn line_height_px_variant_survives_conversion() {
        let attrs = Attributes {
            line_height: Some(IrLineHeightSpec::Px(19.0)),
            ..Attributes::default()
        };
        let ts = Option::<TextStyle>::from(&attrs).expect("line-height alone must produce Some");
        assert_eq!(ts.line_height, Some(CoreLineHeightSpec::Px(19.0)));
    }

    /// An element with no text properties at all - including no
    /// line-height - still returns `None`, preserving the
    /// "component-light plain container" contract the guard exists for.
    #[test]
    fn no_text_properties_returns_none() {
        let attrs = Attributes::default();
        assert!(Option::<TextStyle>::from(&attrs).is_none());
    }

    /// A property already covered by the guard (`font-size`) still
    /// combines correctly with an authored `line-height` on the same
    /// element - the fix to the guard must not regress the
    /// already-working multi-property case.
    #[test]
    fn line_height_combines_with_other_text_properties() {
        let attrs = Attributes {
            font_size: Some(20.0),
            line_height: Some(IrLineHeightSpec::Multiplier(1.4)),
            ..Attributes::default()
        };
        let ts = Option::<TextStyle>::from(&attrs).expect("must produce Some");
        assert_eq!(ts.size_px, 20.0);
        assert_eq!(ts.line_height, Some(CoreLineHeightSpec::Multiplier(1.4)));
    }
}
