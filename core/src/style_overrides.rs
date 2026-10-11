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
    Border, Color, Display, Edges, Fill, Length, Opacity, Position, ShadowSpec, Style, TextStyle,
    Visuals,
};

bitflags::bitflags! {
    /// The style values a runtime system can hold on an element.
    #[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash)]
    pub struct StyleFields: u16 {
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
        /// Every field that lives on [`Style`].
        const LAYOUT = Self::DISPLAY.bits()
            | Self::POSITION.bits()
            | Self::INSET.bits()
            | Self::WIDTH.bits()
            | Self::HEIGHT.bits()
            | Self::MIN_HEIGHT.bits();
        /// Every field that lives on [`Visuals`].
        const VISUALS = Self::FILL.bits() | Self::SHADOWS.bits() | Self::BORDER.bits();
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
        f.set(StyleFields::FILL, self.fill.is_some());
        f.set(StyleFields::SHADOWS, self.shadows.is_some());
        f.set(StyleFields::BORDER, self.border.is_some());
        f.set(StyleFields::OPACITY, self.opacity.is_some());
        f.set(StyleFields::TEXT_COLOR, self.text_color.is_some());
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
        }
        if let Some(vis) = live.visuals {
            v.fill = fields.contains(StyleFields::FILL).then(|| vis.fill.clone());
            v.shadows = fields
                .contains(StyleFields::SHADOWS)
                .then(|| vis.shadows.clone());
            v.border = fields.contains(StyleFields::BORDER).then_some(vis.border);
        }
        if fields.contains(StyleFields::OPACITY) {
            v.opacity = Some(live.opacity.map(|o| o.0));
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
    }

    /// Write the text color this sets onto `text`, if it differs.
    pub fn write_text(&self, text: &mut impl DerefMut<Target = TextStyle>) {
        if let Some(c) = self.text_color
            && text.color != c
        {
            text.color = c;
        }
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
    /// The shipped systems use, lowest first: 10 for a CSS `transition`
    /// tween, 20 for a widget's own state fill (a toggle's checked track,
    /// a selected tab) and for the layout holders, 30 for the `:disabled`
    /// / `:drag-over` state patches, 40 for the hover tint, 50 for the
    /// press tint, 60 for the hover and focus borders.
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
