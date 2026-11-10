//! The `@keyframes` blocks of the app's stylesheet, read for the desktop.
//!
//! A block's body is browser CSS. [`lumen_ir::animation::parse_keyframes`]
//! splits it into frames; each frame's declarations then go through the same
//! declaration machinery a rule's do ([`apply_inline_declaration`]), with
//! `var()` resolved against the root custom properties, and become a
//! [`FramePatch`]: the values the frame sets, laid over what an element shows
//! when it plays.

use std::collections::HashMap;
use std::sync::Arc;

use bevy_ecs::prelude::*;
use lumen_core::components::{Color, Fill, ShadowSpec};
use lumen_core::motion::Easing;
use lumen_core::style_overrides::{StyleFields, StyleValues};
use lumen_core::transform::{ElementTransform, LengthPct, TransformFn};
use lumen_ir::animation::parse_keyframes;
use lumen_ir::css::{AtRule, MediaContext, apply_inline_declaration};
use lumen_ir::layout_ir::Attributes;

/// What one keyframe sets. Each `Some` is a declared value; the rest come
/// from the element.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct FramePatch {
    /// `background` / `background-color`.
    pub fill: Option<Option<Fill>>,
    /// `opacity`.
    pub opacity: Option<f32>,
    /// `color`.
    pub text_color: Option<Color>,
    /// `border-color`, laid over the element's border.
    pub border_color: Option<Color>,
    /// `box-shadow`.
    pub shadows: Option<Vec<ShadowSpec>>,
    /// `border-radius`: a uniform radius, or per-corner ones.
    pub radius: Option<(f32, Option<[f32; 4]>)>,
    /// `outline-color`.
    pub outline_color: Option<Color>,
    /// `transform`.
    pub functions: Option<Vec<TransformFn>>,
    /// `translate`.
    pub translate: Option<(LengthPct, LengthPct)>,
    /// `rotate`.
    pub rotate: Option<f32>,
    /// `scale`.
    pub scale: Option<(f32, f32)>,
    /// `transform-origin`.
    pub origin: Option<(LengthPct, LengthPct)>,
}

impl FramePatch {
    /// The held fields the frame sets.
    pub fn fields(&self) -> StyleFields {
        let mut f = StyleFields::empty();
        f.set(StyleFields::FILL, self.fill.is_some());
        f.set(StyleFields::OPACITY, self.opacity.is_some());
        f.set(StyleFields::TEXT_COLOR, self.text_color.is_some());
        f.set(StyleFields::BORDER, self.border_color.is_some());
        f.set(StyleFields::SHADOWS, self.shadows.is_some());
        f.set(StyleFields::RADIUS, self.radius.is_some());
        f.set(StyleFields::OUTLINE_COLOR, self.outline_color.is_some());
        f.set(
            StyleFields::TRANSFORM,
            self.functions.is_some()
                || self.translate.is_some()
                || self.rotate.is_some()
                || self.scale.is_some()
                || self.origin.is_some(),
        );
        f
    }

    /// Lay `top` over this: every value it declares wins.
    fn merge(&mut self, top: &FramePatch) {
        fn put<T: Clone>(slot: &mut Option<T>, top: &Option<T>) {
            if top.is_some() {
                slot.clone_from(top);
            }
        }
        put(&mut self.fill, &top.fill);
        put(&mut self.opacity, &top.opacity);
        put(&mut self.text_color, &top.text_color);
        put(&mut self.border_color, &top.border_color);
        put(&mut self.shadows, &top.shadows);
        put(&mut self.radius, &top.radius);
        put(&mut self.outline_color, &top.outline_color);
        put(&mut self.functions, &top.functions);
        put(&mut self.translate, &top.translate);
        put(&mut self.rotate, &top.rotate);
        put(&mut self.scale, &top.scale);
        put(&mut self.origin, &top.origin);
    }

    /// The frame's values for the fields in `fields`, each laid over
    /// `under`, what the element shows without the animation. A field the
    /// frame does not declare comes from `under` whole.
    pub fn values(&self, fields: StyleFields, under: &StyleValues) -> StyleValues {
        let mut v = under.clone().restricted(fields);
        if let Some(f) = &self.fill {
            v.fill = Some(f.clone());
        }
        if let Some(o) = self.opacity {
            v.opacity = Some(Some(o));
        }
        if let Some(c) = self.text_color {
            v.text_color = Some(c);
        }
        if let Some(c) = self.border_color
            && let Some(Some(border)) = v.border.as_mut()
        {
            border.color = c;
            border.side_colors = None;
        }
        if let Some(s) = &self.shadows {
            v.shadows = Some(s.clone());
        }
        if let Some(r) = self.radius {
            v.radius = Some(r);
        }
        if let Some(c) = self.outline_color {
            v.outline_color = Some(Some(c));
        }
        if fields.contains(StyleFields::TRANSFORM)
            && (self.functions.is_some()
                || self.translate.is_some()
                || self.rotate.is_some()
                || self.scale.is_some()
                || self.origin.is_some())
        {
            let mut t = v.transform.clone().flatten().unwrap_or_default();
            if let Some(f) = &self.functions {
                t.functions.clone_from(f);
            }
            if let Some(x) = self.translate {
                t.translate = x;
            }
            if let Some(r) = self.rotate {
                t.rotate = r;
            }
            if let Some(s) = self.scale {
                t.scale = s;
            }
            if let Some(o) = self.origin {
                t.origin = o;
            }
            v.transform = Some(Some(t));
        }
        // Only the fields asked for: a frame that sets more must not reach
        // past them into a field another pass samples.
        v.restricted(fields)
    }
}

/// One keyframe: where it sits and what it sets.
#[derive(Clone, Debug, PartialEq)]
pub struct Frame {
    /// Its offset, `0` to `1`.
    pub offset: f32,
    /// What it sets.
    pub patch: FramePatch,
    /// Its own `animation-timing-function`, for the way to the next frame.
    pub timing: Option<Easing>,
}

/// A `@keyframes` block, ready to sample.
#[derive(Clone, Debug, PartialEq)]
pub struct Keyframes {
    /// The frames, in offset order, one per offset.
    pub frames: Vec<Frame>,
    /// Every field a frame sets.
    pub fields: StyleFields,
}

/// The `@keyframes` blocks that apply now, by name: the last block of a
/// name wins, as in CSS, and a block inside `@media` counts only while its
/// query matches.
#[derive(Resource, Default)]
pub struct KeyframeSet {
    /// What the set was built from, to tell when to build it again.
    built_from: Option<(u64, Option<MediaContext>)>,
    by_name: HashMap<String, Arc<Keyframes>>,
}

impl KeyframeSet {
    /// The block called `name`, if one applies.
    pub fn get(&self, name: &str) -> Option<&Arc<Keyframes>> {
        self.by_name.get(name)
    }

    /// Build the set from the stylesheet's at-rules, resolving `var()`
    /// against `root_vars`. A no-op when nothing it reads changed.
    pub fn rebuild(
        &mut self,
        at_rules: &[AtRule],
        media: Option<MediaContext>,
        root_vars: impl FnOnce() -> HashMap<String, String>,
    ) {
        let fingerprint = {
            use std::hash::{Hash, Hasher};
            let mut h = std::collections::hash_map::DefaultHasher::new();
            for r in at_rules.iter().filter(|r| r.name == "keyframes") {
                (&r.prelude, &r.body, format!("{:?}", r.media)).hash(&mut h);
            }
            h.finish()
        };
        if let Some((built, m)) = &self.built_from
            && *built == fingerprint
            && *m == media
        {
            return;
        }
        let vars = root_vars();
        let mut by_name = HashMap::new();
        for rule in at_rules.iter().filter(|r| r.name == "keyframes") {
            if let (Some(q), Some(m)) = (&rule.media, &media)
                && !q.matches(m)
            {
                continue;
            }
            if rule.media.is_some() && media.is_none() {
                continue;
            }
            let name = rule.prelude.trim().trim_matches(['"', '\'']).to_string();
            by_name.insert(name, Arc::new(resolve(&rule.body, &vars)));
        }
        self.by_name = by_name;
        self.built_from = Some((fingerprint, media));
    }
}

/// Read a block's body into frames. What the desktop cannot read is
/// skipped here; `lumenc check` reports it with its place.
pub fn resolve(body: &str, vars: &HashMap<String, String>) -> Keyframes {
    let (parsed, _) = parse_keyframes(body);
    let mut by_offset: Vec<Frame> = Vec::new();
    for frame in parsed {
        let mut patch = FramePatch::default();
        for decl in &frame.declarations {
            let value = match lumen_ir::css_vars::resolve(&decl.value, vars, "@keyframes: ") {
                Ok(v) => v,
                Err(_) => continue,
            };
            if let Some(p) = declaration_patch(&decl.name, &value) {
                patch.merge(&p);
            }
        }
        let timing = frame.timing.as_ref().map(Easing::from);
        for offset in frame.offsets {
            // Frames at one offset merge, the later declarations winning.
            match by_offset.iter_mut().find(|f| f.offset == offset) {
                Some(existing) => {
                    existing.patch.merge(&patch);
                    if timing.is_some() {
                        existing.timing.clone_from(&timing);
                    }
                }
                None => by_offset.push(Frame {
                    offset,
                    patch: patch.clone(),
                    timing: timing.clone(),
                }),
            }
        }
    }
    by_offset.sort_by(|a, b| a.offset.total_cmp(&b.offset));
    let fields = by_offset
        .iter()
        .fold(StyleFields::empty(), |f, fr| f | fr.patch.fields());
    Keyframes {
        frames: by_offset,
        fields,
    }
}

/// One declaration, through the cascade's own parser: what it sets, or
/// `None` when the value does not parse.
pub fn declaration_patch(name: &str, value: &str) -> Option<FramePatch> {
    if name == "outline-color" {
        let c = lumen_ir::values::parse_color("@keyframes", name, value).ok()?;
        return Some(FramePatch {
            outline_color: Some(c.into()),
            ..FramePatch::default()
        });
    }
    let mut attrs = Attributes::default();
    if !apply_inline_declaration(name, value, &mut attrs).ok()? {
        return None;
    }
    let canonical = lumen_ir::css::canonical_property_name(name);
    let mut p = FramePatch::default();
    match canonical {
        "bg" => p.fill = Some(attrs.bg.as_ref().and_then(|b| Fill::try_from(b).ok())),
        "opacity" => p.opacity = attrs.opacity,
        "text-color" => p.text_color = attrs.text_color.map(Into::into),
        "border-color" => p.border_color = attrs.border_color.map(Into::into),
        "box-shadow" | "shadow" => {
            p.shadows = Some(attrs.shadows.iter().copied().map(Into::into).collect())
        }
        "radius" => p.radius = Some((attrs.radius.unwrap_or(0.0), attrs.radius_corners)),
        n if n.starts_with("border-") && n.ends_with("-radius") => {
            let corners = attrs.radius_corners?;
            p.radius = Some((corners.iter().copied().fold(0.0, f32::max), Some(corners)));
        }
        "transform" => {
            p.functions = Some(
                attrs
                    .transform
                    .iter()
                    .flatten()
                    .copied()
                    .map(Into::into)
                    .collect(),
            )
        }
        "translate" => p.translate = attrs.translate.map(|(x, y)| (x.into(), y.into())),
        "rotate" => p.rotate = attrs.rotate,
        "scale" => p.scale = attrs.scale,
        "transform-origin" => p.origin = attrs.transform_origin.map(|(x, y)| (x.into(), y.into())),
        _ => return None,
    }
    Some(p)
}

/// The identity transform, for a frame laid over an element that has none.
pub fn untransformed() -> ElementTransform {
    ElementTransform::default()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn frames_read_through_the_cascade_parser_and_merge_by_offset() {
        let vars = HashMap::from([("lift".to_string(), "-8px".to_string())]);
        let k = resolve(
            "from { opacity: 0; transform: translateY(var(--lift)) }\n\
             to { opacity: 1; background: #ff0000 }\n\
             to { border-radius: 4px }",
            &vars,
        );
        assert_eq!(k.frames.len(), 2);
        assert_eq!(k.frames[0].patch.opacity, Some(0.0));
        assert_eq!(
            k.frames[0].patch.functions,
            Some(vec![TransformFn::Translate(
                LengthPct::default(),
                LengthPct::px(-8.0)
            )])
        );
        let to = &k.frames[1].patch;
        assert_eq!(to.radius, Some((4.0, None)));
        assert!(matches!(to.fill, Some(Some(Fill::Solid(_)))));
        assert!(
            k.fields
                .contains(StyleFields::TRANSFORM | StyleFields::OPACITY)
        );
    }

    #[test]
    fn a_frame_lays_its_values_over_the_element() {
        let patch = FramePatch {
            rotate: Some(45.0),
            border_color: Some(Color::rgb(1.0, 0.0, 0.0)),
            ..FramePatch::default()
        };
        let under = StyleValues {
            transform: Some(Some(ElementTransform {
                scale: (2.0, 2.0),
                ..untransformed()
            })),
            border: Some(Some(lumen_core::components::Border::uniform(
                lumen_core::components::Edges::all(2.0),
                Color::rgb(0.0, 0.0, 1.0),
            ))),
            ..StyleValues::default()
        };
        let v = patch.values(StyleFields::TRANSFORM | StyleFields::BORDER, &under);
        let t = v.transform.unwrap().unwrap();
        assert_eq!((t.rotate, t.scale), (45.0, (2.0, 2.0)));
        let b = v.border.unwrap().unwrap();
        assert_eq!(b.color, Color::rgb(1.0, 0.0, 0.0));
        assert_eq!(b.widths.top, 2.0, "the widths stay the element's");
    }
}
