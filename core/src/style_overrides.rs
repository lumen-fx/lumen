//! The values runtime systems hold on an element over what its styles say.
//!
//! An element's styles are a function of the cascade: the user-agent and skin
//! sheets, the app's stylesheet, what its markup declares and what a script
//! set with `set_style`. A restyle recomputes them from all of that, so a
//! class removed takes with it every property only that class set.
//!
//! Some values are not the cascade's to decide. A hidden `<if>` is
//! `display: none` while its signal is false; an open popup sits where the
//! edge-flip put it; a virtualized row sits in its slot; a hovered button
//! fades toward its tint. The system that owns such a value holds it here,
//! in [`StyleOverrides`], instead of writing the live component behind the
//! cascade's back. The live components are always the cascade's result with
//! every held value laid over it, so a restyle that recomputes the cascade
//! keeps them: it records the new resting values here and leaves the held
//! fields alone, and when a system lets go the field drops back to what the
//! cascade says now, not what it said when the system took hold.
//!
//! Holders stack by [`StyleOwner::RANK`]: a press tint over a hover tint over
//! a toggle's checked fill over the cascade. A holder that blends toward its
//! own value from whatever is under it (a tint fading in) reads that through
//! [`StyleOverrides::below`].

use std::any::TypeId;
use std::ops::DerefMut;

use bevy_ecs::component::Component;
use bevy_ecs::world::EntityWorldMut;

use crate::components::{
    Border, Color, Display, Edges, Fill, Length, Opacity, OutlineColor, Position, ShadowSpec,
    Style, TextStyle, Visuals,
};
use crate::transform::ElementTransform;

bitflags::bitflags! {
    /// The style values a runtime system can hold on an element.
    #[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash)]
    pub struct StyleFields: u32 {
        /// `Style::display`.
        const DISPLAY = 1 << 0;
        /// `Style::position`.
        const POSITION = 1 << 1;
        /// `Style::inset`, all four sides.
        const INSET = 1 << 2;
        /// `Style::width`.
        const WIDTH = 1 << 3;
        /// `Style::height`.
        const HEIGHT = 1 << 4;
        /// `Style::min_height`.
        const MIN_HEIGHT = 1 << 5;
        /// `Visuals::fill`.
        const FILL = 1 << 6;
        /// `Visuals::shadows`.
        const SHADOWS = 1 << 7;
        /// `Visuals::border`.
        const BORDER = 1 << 8;
        /// The [`Opacity`] component, absent meaning fully opaque.
        const OPACITY = 1 << 9;
        /// `TextStyle::color`.
        const TEXT_COLOR = 1 << 10;
        /// `Style::padding`, all four sides.
        const PADDING = 1 << 11;
        /// `Style::basis`, CSS `flex-basis`.
        const BASIS = 1 << 12;
        /// `Visuals::radius` and `Visuals::corner_radii`, CSS `border-radius`.
        const RADIUS = 1 << 13;
        /// The [`ElementTransform`] component, absent meaning untransformed.
        const TRANSFORM = 1 << 14;
        /// The [`OutlineColor`] component, absent meaning the declared color.
        const OUTLINE_COLOR = 1 << 15;
        /// `Style::max_height`.
        const MAX_HEIGHT = 1 << 16;
        /// Every field that lives on [`Style`].
        const LAYOUT = Self::DISPLAY.bits()
            | Self::POSITION.bits()
            | Self::INSET.bits()
            | Self::WIDTH.bits()
            | Self::HEIGHT.bits()
            | Self::MIN_HEIGHT.bits()
            | Self::MAX_HEIGHT.bits()
            | Self::PADDING.bits()
            | Self::BASIS.bits();
        /// Every field that lives on [`Visuals`].
        const VISUALS =
            Self::FILL.bits() | Self::SHADOWS.bits() | Self::BORDER.bits() | Self::RADIUS.bits();
    }
}

/// A value for some of the [`StyleFields`]; `None` leaves a field out.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct StyleValues {
    /// `Style::display`.
    pub display: Option<Display>,
    /// `Style::position`.
    pub position: Option<Position>,
    /// `Style::inset`.
    pub inset: Option<Edges>,
    /// `Style::width`.
    pub width: Option<Length>,
    /// `Style::height`.
    pub height: Option<Length>,
    /// `Style::min_height`.
    pub min_height: Option<Length>,
    /// `Style::max_height`.
    pub max_height: Option<Length>,
    /// `Style::padding`.
    pub padding: Option<Edges>,
    /// `Style::basis`.
    pub basis: Option<Length>,
    /// `Visuals::fill`.
    pub fill: Option<Option<Fill>>,
    /// `Visuals::shadows`.
    pub shadows: Option<Vec<ShadowSpec>>,
    /// `Visuals::border`.
    pub border: Option<Option<Border>>,
    /// The [`Opacity`] component; `Some(None)` is no component.
    pub opacity: Option<Option<f32>>,
    /// `TextStyle::color`.
    pub text_color: Option<Color>,
    /// `Visuals::radius` and `Visuals::corner_radii`.
    pub radius: Option<(f32, Option<[f32; 4]>)>,
    /// The [`ElementTransform`] component; `Some(None)` is no component.
    pub transform: Option<Option<ElementTransform>>,
    /// The [`OutlineColor`] component; `Some(None)` is no component.
    pub outline_color: Option<Option<Color>>,
}

/// The live components of one element, read when a holder takes a field
/// nobody held yet: at that moment the live value is the cascade's.
#[derive(Clone, Copy, Debug, Default)]
pub struct Live<'a> {
    /// The element's layout style.
    pub style: Option<&'a Style>,
    /// The element's paint.
    pub visuals: Option<&'a Visuals>,
    /// The element's text style.
    pub text: Option<&'a TextStyle>,
    /// The element's opacity; absent is fully opaque.
    pub opacity: Option<&'a Opacity>,
    /// The element's transform; absent is untransformed.
    pub transform: Option<&'a ElementTransform>,
    /// The color a runtime system holds the element's outline at.
    pub outline_color: Option<&'a OutlineColor>,
}

impl StyleValues {
    /// The fields this sets.
    pub fn fields(&self) -> StyleFields {
        let mut f = StyleFields::empty();
        f.set(StyleFields::DISPLAY, self.display.is_some());
        f.set(StyleFields::POSITION, self.position.is_some());
        f.set(StyleFields::INSET, self.inset.is_some());
        f.set(StyleFields::WIDTH, self.width.is_some());
        f.set(StyleFields::HEIGHT, self.height.is_some());
        f.set(StyleFields::MIN_HEIGHT, self.min_height.is_some());
        f.set(StyleFields::MAX_HEIGHT, self.max_height.is_some());
        f.set(StyleFields::PADDING, self.padding.is_some());
        f.set(StyleFields::BASIS, self.basis.is_some());
        f.set(StyleFields::FILL, self.fill.is_some());
        f.set(StyleFields::SHADOWS, self.shadows.is_some());
        f.set(StyleFields::BORDER, self.border.is_some());
        f.set(StyleFields::OPACITY, self.opacity.is_some());
        f.set(StyleFields::TEXT_COLOR, self.text_color.is_some());
        f.set(StyleFields::RADIUS, self.radius.is_some());
        f.set(StyleFields::TRANSFORM, self.transform.is_some());
        f.set(StyleFields::OUTLINE_COLOR, self.outline_color.is_some());
        f
    }

    /// Read `fields` off the live components. A field whose component is
    /// missing is left out, except opacity, whose absence is a value.
    pub fn read(fields: StyleFields, live: &Live<'_>) -> Self {
        let mut v = Self::default();
        if let Some(s) = live.style {
            v.display = fields.contains(StyleFields::DISPLAY).then_some(s.display);
            v.position = fields.contains(StyleFields::POSITION).then_some(s.position);
            v.inset = fields.contains(StyleFields::INSET).then_some(s.inset);
            v.width = fields.contains(StyleFields::WIDTH).then_some(s.width);
            v.height = fields.contains(StyleFields::HEIGHT).then_some(s.height);
            v.min_height = fields
                .contains(StyleFields::MIN_HEIGHT)
                .then_some(s.min_height);
            v.max_height = fields
                .contains(StyleFields::MAX_HEIGHT)
                .then_some(s.max_height);
            v.padding = fields.contains(StyleFields::PADDING).then_some(s.padding);
            v.basis = fields.contains(StyleFields::BASIS).then_some(s.basis);
        }
        if let Some(vis) = live.visuals {
            v.fill = fields.contains(StyleFields::FILL).then(|| vis.fill.clone());
            v.shadows = fields
                .contains(StyleFields::SHADOWS)
                .then(|| vis.shadows.clone());
            v.border = fields.contains(StyleFields::BORDER).then_some(vis.border);
            v.radius = fields
                .contains(StyleFields::RADIUS)
                .then_some((vis.radius, vis.corner_radii));
        }
        if fields.contains(StyleFields::OPACITY) {
            v.opacity = Some(live.opacity.map(|o| o.0));
        }
        if fields.contains(StyleFields::TRANSFORM) {
            v.transform = Some(live.transform.cloned());
        }
        if fields.contains(StyleFields::OUTLINE_COLOR) {
            v.outline_color = Some(live.outline_color.map(|o| o.0));
        }
        if let Some(t) = live.text {
            v.text_color = fields.contains(StyleFields::TEXT_COLOR).then_some(t.color);
        }
        v
    }

    /// Lay `top` over these values: every field it sets wins.
    pub fn overlay(&mut self, top: &StyleValues) {
        if top.display.is_some() {
            self.display = top.display;
        }
        if top.position.is_some() {
            self.position = top.position;
        }
        if top.inset.is_some() {
            self.inset = top.inset;
        }
        if top.width.is_some() {
            self.width = top.width;
        }
        if top.height.is_some() {
            self.height = top.height;
        }
        if top.min_height.is_some() {
            self.min_height = top.min_height;
        }
        if top.max_height.is_some() {
            self.max_height = top.max_height;
        }
        if top.padding.is_some() {
            self.padding = top.padding;
        }
        if top.basis.is_some() {
            self.basis = top.basis;
        }
        if top.fill.is_some() {
            self.fill.clone_from(&top.fill);
        }
        if top.shadows.is_some() {
            self.shadows.clone_from(&top.shadows);
        }
        if top.border.is_some() {
            self.border = top.border;
        }
        if top.opacity.is_some() {
            self.opacity = top.opacity;
        }
        if top.text_color.is_some() {
            self.text_color = top.text_color;
        }
        if top.radius.is_some() {
            self.radius = top.radius;
        }
        if top.transform.is_some() {
            self.transform.clone_from(&top.transform);
        }
        if top.outline_color.is_some() {
            self.outline_color = top.outline_color;
        }
    }

    /// Keep only `fields`.
    pub fn restricted(mut self, fields: StyleFields) -> Self {
        let keep = |f: StyleFields| fields.contains(f);
        if !keep(StyleFields::DISPLAY) {
            self.display = None;
        }
        if !keep(StyleFields::POSITION) {
            self.position = None;
        }
        if !keep(StyleFields::INSET) {
            self.inset = None;
        }
        if !keep(StyleFields::WIDTH) {
            self.width = None;
        }
        if !keep(StyleFields::HEIGHT) {
            self.height = None;
        }
        if !keep(StyleFields::MIN_HEIGHT) {
            self.min_height = None;
        }
        if !keep(StyleFields::MAX_HEIGHT) {
            self.max_height = None;
        }
        if !keep(StyleFields::PADDING) {
            self.padding = None;
        }
        if !keep(StyleFields::BASIS) {
            self.basis = None;
        }
        if !keep(StyleFields::FILL) {
            self.fill = None;
        }
        if !keep(StyleFields::SHADOWS) {
            self.shadows = None;
        }
        if !keep(StyleFields::BORDER) {
            self.border = None;
        }
        if !keep(StyleFields::OPACITY) {
            self.opacity = None;
        }
        if !keep(StyleFields::TEXT_COLOR) {
            self.text_color = None;
        }
        if !keep(StyleFields::RADIUS) {
            self.radius = None;
        }
        if !keep(StyleFields::TRANSFORM) {
            self.transform = None;
        }
        if !keep(StyleFields::OUTLINE_COLOR) {
            self.outline_color = None;
        }
        self
    }

    /// Write the layout fields this sets onto `style`, touching only the
    /// ones that differ so an unchanged value marks nothing changed.
    pub fn write_style(&self, style: &mut impl DerefMut<Target = Style>) {
        if let Some(d) = self.display
            && style.display != d
        {
            style.display = d;
        }
        if let Some(p) = self.position
            && style.position != p
        {
            style.position = p;
        }
        if let Some(i) = self.inset
            && !edges_eq(&style.inset, &i)
        {
            style.inset = i;
        }
        if let Some(w) = self.width
            && style.width != w
        {
            style.width = w;
        }
        if let Some(h) = self.height
            && style.height != h
        {
            style.height = h;
        }
        if let Some(h) = self.min_height
            && style.min_height != h
        {
            style.min_height = h;
        }
        if let Some(h) = self.max_height
            && style.max_height != h
        {
            style.max_height = h;
        }
        if let Some(p) = self.padding
            && !edges_eq(&style.padding, &p)
        {
            style.padding = p;
        }
        if let Some(b) = self.basis
            && style.basis != b
        {
            style.basis = b;
        }
    }

    /// Write the paint fields this sets onto `visuals`, touching only the
    /// ones that differ.
    pub fn write_visuals(&self, visuals: &mut impl DerefMut<Target = Visuals>) {
        if let Some(f) = &self.fill
            && visuals.fill != *f
        {
            visuals.fill.clone_from(f);
        }
        if let Some(s) = &self.shadows
            && visuals.shadows != *s
        {
            visuals.shadows.clone_from(s);
        }
        if let Some(b) = self.border
            && visuals.border != b
        {
            visuals.border = b;
        }
        if let Some((r, corners)) = self.radius
            && (visuals.radius != r || visuals.corner_radii != corners)
        {
            visuals.radius = r;
            visuals.corner_radii = corners;
        }
    }

    /// Write the text color this sets onto `text`, if it differs.
    pub fn write_text(&self, text: &mut impl DerefMut<Target = TextStyle>) {
        if let Some(c) = self.text_color
            && text.color != c
        {
            text.color = c;
        }
    }

    /// What to do with the [`ElementTransform`] component to make it hold
    /// the transform this sets, read like [`Self::opacity_change`].
    pub fn transform_change(
        &self,
        current: Option<&ElementTransform>,
    ) -> Option<Option<ElementTransform>> {
        let want = self.transform.as_ref()?;
        (want.as_ref() != current).then(|| want.clone())
    }

    /// What to do with the [`OutlineColor`] component to make it hold the
    /// outline color this sets, read like [`Self::opacity_change`].
    pub fn outline_change(&self, current: Option<&OutlineColor>) -> Option<Option<OutlineColor>> {
        let want = self.outline_color?.map(OutlineColor);
        (want.as_ref() != current).then_some(want)
    }

    /// What to do with the [`Opacity`] component to make it hold the
    /// opacity this sets: `None` leaves it as it is, `Some(None)` removes
    /// it, `Some(Some(o))` writes `o`.
    pub fn opacity_change(&self, current: Option<&Opacity>) -> Option<Option<Opacity>> {
        let want = self.opacity?;
        match (want, current) {
            (None, None) => None,
            (Some(o), Some(cur)) if cur.0 == o => None,
            (want, _) => Some(want.map(Opacity)),
        }
    }
}

impl StyleValues {
    /// The values a fraction `t` of the way from `self` to `to`, field by
    /// field through [`crate::motion`]: colors, opacity, shadows, radii and
    /// transforms blend, and anything without an in-between (a gradient
    /// fill, a border whose widths differ, a layout value) flips at the
    /// midpoint. A field only one side sets comes through as that side has
    /// it. `size` is the element's border box, which resolves the
    /// percentages of a transform that can only blend as a matrix.
    pub fn interpolate(&self, to: &StyleValues, t: f32, size: glam::Vec2) -> StyleValues {
        use crate::motion::{
            Interpolate, discrete, interpolate_border, interpolate_radii, interpolate_shadows,
            interpolate_transform,
        };
        fn pick<T: Clone>(a: &Option<T>, b: &Option<T>, t: f32) -> Option<T> {
            match (a, b) {
                (Some(a), Some(b)) => Some(discrete(a, b, t).clone()),
                (a, b) => a.clone().or_else(|| b.clone()),
            }
        }
        fn both<T, R>(a: &Option<T>, b: &Option<T>, f: impl FnOnce(&T, &T) -> R) -> Option<R> {
            match (a, b) {
                (Some(a), Some(b)) => Some(f(a, b)),
                _ => None,
            }
        }
        let mut out = StyleValues {
            display: pick(&self.display, &to.display, t),
            position: pick(&self.position, &to.position, t),
            inset: pick(&self.inset, &to.inset, t),
            width: pick(&self.width, &to.width, t),
            height: pick(&self.height, &to.height, t),
            min_height: pick(&self.min_height, &to.min_height, t),
            max_height: pick(&self.max_height, &to.max_height, t),
            padding: pick(&self.padding, &to.padding, t),
            basis: pick(&self.basis, &to.basis, t),
            fill: pick(&self.fill, &to.fill, t),
            shadows: pick(&self.shadows, &to.shadows, t),
            border: pick(&self.border, &to.border, t),
            opacity: pick(&self.opacity, &to.opacity, t),
            text_color: pick(&self.text_color, &to.text_color, t),
            radius: pick(&self.radius, &to.radius, t),
            transform: pick(&self.transform, &to.transform, t),
            outline_color: pick(&self.outline_color, &to.outline_color, t),
        };
        // No fill is a transparent one, so a background fades in or out
        // keeping its hue, as CSS fades from `transparent`.
        let clear = |c: &Color| Color { a: 0.0, ..*c };
        if let Some(fill) = both(&self.fill, &to.fill, |a, b| match (a, b) {
            (Some(Fill::Solid(a)), Some(Fill::Solid(b))) => {
                Some(Some(Fill::Solid(a.interpolate(b, t))))
            }
            (None, Some(Fill::Solid(b))) => Some(Some(Fill::Solid(clear(b).interpolate(b, t)))),
            (Some(Fill::Solid(a)), None) => Some(Some(Fill::Solid(a.interpolate(&clear(a), t)))),
            _ => None,
        })
        .flatten()
        {
            out.fill = Some(fill);
        }
        if let Some(s) = both(&self.shadows, &to.shadows, |a, b| {
            interpolate_shadows(a, b, t)
        }) {
            out.shadows = Some(s);
        }
        if let Some(Some(b)) = both(&self.border, &to.border, |a, b| match (a, b) {
            (Some(a), Some(b)) => Some(interpolate_border(a, b, t)),
            _ => None,
        }) {
            out.border = Some(Some(b));
        }
        if let Some(o) = both(&self.opacity, &to.opacity, |a, b| {
            a.unwrap_or(1.0)
                .interpolate(&b.unwrap_or(1.0), t)
                .clamp(0.0, 1.0)
        }) {
            out.opacity = Some(Some(o));
        }
        if let Some(c) = both(&self.text_color, &to.text_color, |a, b| a.interpolate(b, t)) {
            out.text_color = Some(c);
        }
        if let Some(r) = both(&self.radius, &to.radius, |a, b| {
            let corners = |r: &(f32, Option<[f32; 4]>)| r.1.unwrap_or([r.0; 4]);
            let blended = interpolate_radii(corners(a), corners(b), t);
            if a.1.is_none() && b.1.is_none() {
                (blended[0], None)
            } else {
                (blended.iter().copied().fold(0.0, f32::max), Some(blended))
            }
        }) {
            out.radius = Some(r);
        }
        if let Some(x) = both(&self.transform, &to.transform, |a, b| {
            let none = ElementTransform::default();
            let a = a.as_ref().unwrap_or(&none);
            let b = b.as_ref().unwrap_or(&none);
            interpolate_transform(a, b, t, size)
        }) {
            out.transform = Some(Some(x));
        }
        if let Some(Some(c)) = both(&self.outline_color, &to.outline_color, |a, b| {
            match (a, b) {
                (Some(a), Some(b)) => Some(a.interpolate(b, t)),
                _ => None,
            }
        }) {
            out.outline_color = Some(Some(c));
        }
        out
    }
}

/// Inset sides compared the way layout reads them: `NaN` is `auto`, and
/// `auto` equals `auto`.
fn edges_eq(a: &Edges, b: &Edges) -> bool {
    let side = |x: f32, y: f32| (x.is_nan() && y.is_nan()) || x == y;
    side(a.left, b.left)
        && side(a.right, b.right)
        && side(a.top, b.top)
        && side(a.bottom, b.bottom)
        && a.inline_start == b.inline_start
        && a.inline_end == b.inline_end
        && a.block_start == b.block_start
        && a.block_end == b.block_end
        && a.pct_left == b.pct_left
        && a.pct_right == b.pct_right
        && a.pct_top == b.pct_top
        && a.pct_bottom == b.pct_bottom
}

/// A runtime system that holds style values on elements.
///
/// Implemented on a type the system owns (usually the component that marks
/// what it drives), which names its layer in [`StyleOverrides`].
pub trait StyleOwner: 'static {
    /// Where the owner's values sit: a higher rank lays over a lower one.
    ///
    /// The shipped systems use, lowest first: 20 for a widget's own state
    /// fill (a toggle's checked track, a selected tab) and for the layout
    /// holders, 30 for the state patches, 40 for the hover tint, 50 for the
    /// press tint, 60 for the hover and focus borders, 70 for a keyframe
    /// animation and 80 for a CSS `transition` tween. Animations and
    /// transitions sit above the rest because CSS orders them above every
    /// normal declaration, and a state rule is one.
    const RANK: u8;
}

#[derive(Clone, Debug)]
struct Claim {
    owner: TypeId,
    rank: u8,
    values: StyleValues,
}

/// The values runtime systems hold on one element; see the module docs.
///
/// Carried by every element a holder can act on (its marker components
/// require it), so a holder writes here in place rather than through a
/// deferred insert another holder could race.
#[derive(Component, Clone, Debug, Default)]
pub struct StyleOverrides {
    /// The cascade's value for every field some claim holds.
    resting: StyleValues,
    /// Ascending by rank; equal ranks keep the order they arrived in.
    claims: Vec<Claim>,
}

impl StyleOverrides {
    /// Every field some holder holds.
    pub fn held(&self) -> StyleFields {
        self.claims
            .iter()
            .fold(StyleFields::empty(), |f, c| f | c.values.fields())
    }

    /// True when nothing is held.
    pub fn is_empty(&self) -> bool {
        self.claims.is_empty()
    }

    /// Every field some holder other than `O` holds.
    pub fn held_by_others<O: StyleOwner>(&self) -> StyleFields {
        let owner = TypeId::of::<O>();
        self.claims
            .iter()
            .filter(|c| c.owner != owner)
            .fold(StyleFields::empty(), |f, c| f | c.values.fields())
    }

    /// Every field a holder ranked below `rank` holds.
    pub fn held_below(&self, rank: u8) -> StyleFields {
        self.claims
            .iter()
            .filter(|c| c.rank < rank)
            .fold(StyleFields::empty(), |f, c| f | c.values.fields())
    }

    /// What `O` holds, if anything.
    pub fn held_by<O: StyleOwner>(&self) -> Option<&StyleValues> {
        let owner = TypeId::of::<O>();
        self.claims
            .iter()
            .find(|c| c.owner == owner)
            .map(|c| &c.values)
    }

    /// What the cascade resolved for the held fields.
    pub fn resting(&self) -> &StyleValues {
        &self.resting
    }

    /// Hold `values` for `O`, replacing whatever `O` held before.
    ///
    /// `live` is read for a field nobody held yet: until then the live
    /// value is the cascade's. Returns the composed value of every field
    /// whose top layer may have moved (what `O` held before and holds now),
    /// to write onto the live components.
    pub fn hold<O: StyleOwner>(&mut self, values: StyleValues, live: &Live<'_>) -> StyleValues {
        self.hold_reading::<O>(values, |fields| StyleValues::read(fields, live))
    }

    /// [`Self::hold`] with the live values read by `read`, which is handed
    /// the fields nobody held yet.
    fn hold_reading<O: StyleOwner>(
        &mut self,
        values: StyleValues,
        read: impl FnOnce(StyleFields) -> StyleValues,
    ) -> StyleValues {
        let owner = TypeId::of::<O>();
        let before = self.held();
        let fresh = values.fields() - before;
        if !fresh.is_empty() {
            self.resting.overlay(&read(fresh).restricted(fresh));
        }
        let previous = match self.claims.iter_mut().find(|c| c.owner == owner) {
            Some(claim) => std::mem::replace(&mut claim.values, values).fields(),
            None => {
                let at = self
                    .claims
                    .iter()
                    .position(|c| c.rank > O::RANK)
                    .unwrap_or(self.claims.len());
                self.claims.insert(
                    at,
                    Claim {
                        owner,
                        rank: O::RANK,
                        values,
                    },
                );
                StyleFields::empty()
            }
        };
        let affected = previous | self.held();
        let out = self.composed(affected);
        self.drop_unheld_resting();
        out
    }

    /// Stop holding anything for `O`. Returns the composed value of every
    /// field it held, which for a field nobody else holds is the cascade's,
    /// to write onto the live components.
    pub fn release<O: StyleOwner>(&mut self) -> StyleValues {
        let owner = TypeId::of::<O>();
        let Some(at) = self.claims.iter().position(|c| c.owner == owner) else {
            return StyleValues::default();
        };
        let fields = self.claims.remove(at).values.fields();
        let out = self.composed(fields);
        self.drop_unheld_resting();
        out
    }

    /// The value of `fields` under `O`'s layer: the cascade's, with every
    /// lower-ranked holder laid over it. A field nobody holds reads `live`.
    pub fn below<O: StyleOwner>(&self, fields: StyleFields, live: &Live<'_>) -> StyleValues {
        let owner = TypeId::of::<O>();
        let held = self.held();
        let mut out = StyleValues::read(fields - held, live);
        out.overlay(&self.resting.clone().restricted(fields & held));
        for claim in &self.claims {
            if claim.rank > O::RANK || claim.owner == owner {
                break;
            }
            out.overlay(&claim.values.clone().restricted(fields));
        }
        out
    }

    /// Record what a restyle resolved: `fresh` is the cascade's value for
    /// every field. Returns the composed value of every held field, to lay
    /// over `fresh` before it is written onto the live components.
    pub fn rebase(&mut self, fresh: &StyleValues) -> StyleValues {
        let held = self.held();
        self.resting = fresh.clone().restricted(held);
        self.composed(held)
    }

    /// The top value of each of `fields`: the cascade's with every holder
    /// laid over it in rank order.
    fn composed(&self, fields: StyleFields) -> StyleValues {
        let mut out = self.resting.clone().restricted(fields);
        for claim in &self.claims {
            out.overlay(&claim.values.clone().restricted(fields));
        }
        out
    }

    /// Forget the resting value of a field nobody holds any more, so the
    /// next holder reads it fresh off the live components.
    fn drop_unheld_resting(&mut self) {
        let held = self.held();
        self.resting = std::mem::take(&mut self.resting).restricted(held);
    }
}

/// Hold `values` for `O` on one element and write what it shows now: the
/// form for a holder that reaches the element through the world rather than
/// through a query. Inserts the [`StyleOverrides`] the element lacks.
pub fn hold_on<O: StyleOwner>(ent: &mut EntityWorldMut<'_>, values: StyleValues) {
    let live = StyleValues::read(
        values.fields(),
        &Live {
            style: ent.get::<Style>(),
            visuals: ent.get::<Visuals>(),
            text: ent.get::<TextStyle>(),
            opacity: ent.get::<Opacity>(),
            transform: ent.get::<ElementTransform>(),
            outline_color: ent.get::<OutlineColor>(),
        },
    );
    let mut overrides = ent.take::<StyleOverrides>().unwrap_or_default();
    let out = overrides.hold_reading::<O>(values, |_| live);
    ent.insert(overrides);
    write_on(ent, &out);
}

/// Stop holding anything for `O` on one element and write what it shows
/// now; see [`hold_on`].
pub fn release_on<O: StyleOwner>(ent: &mut EntityWorldMut<'_>) {
    let Some(mut overrides) = ent.get_mut::<StyleOverrides>() else {
        return;
    };
    let out = overrides.release::<O>();
    write_on(ent, &out);
}

/// Write `values` onto an element's live components, touching only what
/// differs.
pub fn write_on(ent: &mut EntityWorldMut<'_>, values: &StyleValues) {
    let fields = values.fields();
    if fields.intersects(StyleFields::LAYOUT)
        && let Some(mut style) = ent.get_mut::<Style>()
    {
        values.write_style(&mut style);
    }
    if fields.intersects(StyleFields::VISUALS)
        && let Some(mut visuals) = ent.get_mut::<Visuals>()
    {
        values.write_visuals(&mut visuals);
    }
    if fields.contains(StyleFields::TEXT_COLOR)
        && let Some(mut text) = ent.get_mut::<TextStyle>()
    {
        values.write_text(&mut text);
    }
    match values.opacity_change(ent.get::<Opacity>()) {
        Some(Some(o)) => {
            ent.insert(o);
        }
        Some(None) => {
            ent.remove::<Opacity>();
        }
        None => {}
    }
    // Only motion holds a transform or an outline color, and a page's
    // browser plays its motion itself.
    if cfg!(target_arch = "wasm32") {
        return;
    }
    match values.transform_change(ent.get::<ElementTransform>()) {
        Some(Some(t)) => {
            ent.insert(t);
        }
        Some(None) => {
            ent.remove::<ElementTransform>();
        }
        None => {}
    }
    match values.outline_change(ent.get::<OutlineColor>()) {
        Some(Some(c)) => {
            ent.insert(c);
        }
        Some(None) => {
            ent.remove::<OutlineColor>();
        }
        None => {}
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    struct Low;
    impl StyleOwner for Low {
        const RANK: u8 = 10;
    }
    struct High;
    impl StyleOwner for High {
        const RANK: u8 = 50;
    }

    fn solid(r: f32) -> Option<Fill> {
        Some(Fill::Solid(Color::rgb(r, 0.0, 0.0)))
    }

    fn visuals(r: f32) -> Visuals {
        Visuals {
            fill: solid(r),
            ..Visuals::default()
        }
    }

    #[test]
    fn a_held_value_survives_a_rebase_and_lets_go_to_the_new_resting_value() {
        let mut ov = StyleOverrides::default();
        let live = visuals(0.1);
        let out = ov.hold::<Low>(
            StyleValues {
                fill: Some(solid(0.5)),
                ..StyleValues::default()
            },
            &Live {
                visuals: Some(&live),
                ..Live::default()
            },
        );
        assert_eq!(out.fill, Some(solid(0.5)));
        assert_eq!(ov.resting().fill, Some(solid(0.1)));

        // The cascade moves the resting fill while the value is held.
        let out = ov.rebase(&StyleValues {
            fill: Some(solid(0.2)),
            display: Some(Display::Flex),
            ..StyleValues::default()
        });
        assert_eq!(out.fill, Some(solid(0.5)), "the held value stays on top");
        assert_eq!(out.display, None, "only held fields come back");

        let out = ov.release::<Low>();
        assert_eq!(
            out.fill,
            Some(solid(0.2)),
            "letting go lands on the new value"
        );
        assert!(ov.is_empty());
        assert_eq!(ov.resting().fill, None);
    }

    #[test]
    fn holders_stack_by_rank_and_below_reads_the_layer_under_one() {
        let mut ov = StyleOverrides::default();
        let live = visuals(0.1);
        let l = Live {
            visuals: Some(&live),
            ..Live::default()
        };
        ov.hold::<High>(
            StyleValues {
                fill: Some(solid(0.9)),
                ..StyleValues::default()
            },
            &l,
        );
        let out = ov.hold::<Low>(
            StyleValues {
                fill: Some(solid(0.5)),
                ..StyleValues::default()
            },
            &l,
        );
        assert_eq!(out.fill, Some(solid(0.9)), "the higher rank stays on top");
        assert_eq!(
            ov.below::<High>(StyleFields::FILL, &l).fill,
            Some(solid(0.5)),
            "under the high layer sits the low one"
        );
        assert_eq!(
            ov.below::<Low>(StyleFields::FILL, &l).fill,
            Some(solid(0.1)),
            "under the low layer sits the cascade"
        );
        let out = ov.release::<High>();
        assert_eq!(out.fill, Some(solid(0.5)));
    }

    #[test]
    fn an_unheld_field_reads_live_below_every_layer() {
        let ov = StyleOverrides::default();
        let style = Style {
            display: Display::None,
            ..Style::default()
        };
        let below = ov.below::<Low>(
            StyleFields::DISPLAY,
            &Live {
                style: Some(&style),
                ..Live::default()
            },
        );
        assert_eq!(below.display, Some(Display::None));
    }

    #[test]
    fn opacity_absent_is_a_value() {
        let mut ov = StyleOverrides::default();
        let out = ov.hold::<Low>(
            StyleValues {
                opacity: Some(Some(0.0)),
                ..StyleValues::default()
            },
            &Live::default(),
        );
        assert_eq!(out.opacity_change(None), Some(Some(Opacity(0.0))));
        let out = ov.release::<Low>();
        assert_eq!(
            out.opacity_change(Some(&Opacity(0.0))),
            Some(None),
            "the cascade had no opacity, so letting go removes it"
        );
    }
}
