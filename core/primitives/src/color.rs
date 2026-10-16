//! `<input type="color">`: a swatch that opens a color panel.
//!
//! The field holds one sRGB color, written `#rrggbb` the way HTML's color
//! input holds it. With HTML's `alpha` attribute the color has an alpha too,
//! and the value is written `color(srgb r g b / a)`, as HTML serializes a
//! color well that takes one. The face shows the color in its
//! `.color-swatch`; a click, Enter or Space opens the `.color-panel` below it
//! (above it near the window's bottom edge), and a click outside it or
//! Escape closes it again. The panel is a few controls over one color:
//!
//! - `.color-area`, saturation from left to right and value from top to
//!   bottom at the current hue, with a `.color-area-thumb` on the chosen
//!   point,
//! - `.color-hue`, the hue strip, with a `.color-hue-thumb`,
//! - with `alpha`, `.color-alpha`, the alpha strip: a `.color-alpha-fill`
//!   shading from the clear color to the opaque one over the strip's own
//!   backdrop, and a `.color-alpha-thumb`,
//! - `.color-hex`, a text field holding `#rrggbb` (`#rrggbbaa` with
//!   `alpha`), which takes a typed color on Enter.
//!
//! Every change raises [`ControlInput`], the DOM `input` event; closing the
//! panel on a color other than the one it opened with raises it committed,
//! which adds `change`, as a browser's color picker does. A field with
//! `bind-value` writes its signal on every change and follows the signal
//! when something else writes it.
//!
//! Every visual is CSS: the panel, its parts and the thumbs are styled by
//! class, the area's shading and the hue strip are gradients in the
//! user-agent sheet. What this module holds in [`StyleOverrides`] is what
//! the color decides: the swatch's fill, the area's hue, the alpha strip's
//! shading, where the thumbs and the panel sit, and whether the panel shows.

use bevy_ecs::message::{MessageReader, MessageWriter};
use bevy_ecs::prelude::*;
use glam::Vec2;
use lumen_core::components::{
    BindValue, Color, Display, Edges, Fill, Style, TextContent, Visuals, format_color_value,
    format_hex_color, parse_color_value,
};
use lumen_core::input::{ControlInput, FocusedKey};
use lumen_core::prelude::*;
use lumen_core::render_world::{AnimationsActive, Viewport};
use lumen_core::style_overrides::{Live, StyleOverrides, StyleOwner, StyleValues};

use crate::popup::{PopupSide, anchored_popup_origin};

/// Gap between the face and the panel, in logical pixels.
const PANEL_GAP: f32 = 4.0;

/// The state of one `<input type="color">`.
#[derive(Component, Clone, Debug, PartialEq)]
#[require(StyleOverrides)]
pub struct ColorInput {
    /// The color, alpha last. Always opaque without [`Self::alpha`].
    pub rgba: [u8; 4],
    /// Whether the field takes HTML's `alpha`: the alpha can be picked and
    /// the value carries it.
    pub alpha: bool,
    /// The hue (degrees), saturation and value the panel shows. Kept beside
    /// [`Self::rgba`] because a gray has no hue of its own: dragging the area
    /// to its left edge must not throw away the hue the strip was on.
    pub hsv: [f32; 3],
    /// Whether the panel is showing.
    pub open: bool,
    /// The color the panel opened on, for the `change` on close.
    opened_with: [u8; 4],
    /// The value last exchanged with the bound signal.
    synced: Option<String>,
}

impl StyleOwner for ColorInput {
    const RANK: u8 = 20;
}

impl ColorInput {
    /// A closed field holding `rgba`, made opaque unless the field takes
    /// `alpha`.
    pub fn new(rgba: [u8; 4], alpha: bool) -> Self {
        let rgba = if alpha {
            rgba
        } else {
            [rgba[0], rgba[1], rgba[2], 255]
        };
        Self {
            rgba,
            alpha,
            hsv: rgb_to_hsv(rgb_of(rgba)),
            open: false,
            opened_with: rgba,
            synced: None,
        }
    }

    /// The field's value: `#rrggbb`, or `color(srgb r g b / a)` with
    /// `alpha`.
    pub fn value(&self) -> String {
        format_color_value(self.rgba, self.alpha)
    }

    /// The color as the panel's hex field shows it: `#rrggbb`, or
    /// `#rrggbbaa` with `alpha`.
    pub fn hex(&self) -> String {
        format_hex_color(self.rgba, self.alpha)
    }

    /// `rgba` as this field holds it: opaque unless it takes `alpha`, the
    /// way HTML sanitizes a color well's value.
    pub fn sanitized(&self, rgba: [u8; 4]) -> [u8; 4] {
        if self.alpha {
            rgba
        } else {
            [rgba[0], rgba[1], rgba[2], 255]
        }
    }

    /// Take a color from outside the panel (a typed hex, a signal, the
    /// browser), keeping the hue when the color is a gray.
    pub fn set_color(&mut self, rgba: [u8; 4]) {
        let rgba = self.sanitized(rgba);
        if rgba == self.rgba {
            return;
        }
        let hsv = rgb_to_hsv(rgb_of(rgba));
        let keeps_hue = hsv[1] == 0.0 || hsv[2] == 0.0;
        self.hsv = if keeps_hue {
            [self.hsv[0], hsv[1], hsv[2]]
        } else {
            hsv
        };
        self.rgba = rgba;
    }

    /// Take a color from the panel, as hue, saturation and value, keeping
    /// the alpha.
    fn set_hsv(&mut self, hsv: [f32; 3]) {
        self.hsv = hsv;
        let [r, g, b] = hsv_to_rgb(hsv);
        self.rgba = [r, g, b, self.rgba[3]];
    }

    /// Take an alpha from the panel's alpha strip, 0 to 1.
    fn set_alpha(&mut self, alpha: f32) {
        if self.alpha {
            self.rgba[3] = (alpha.clamp(0.0, 1.0) * 255.0).round() as u8;
        }
    }
}

/// The color channels of `rgba`, without its alpha.
fn rgb_of(rgba: [u8; 4]) -> [u8; 3] {
    [rgba[0], rgba[1], rgba[2]]
}

/// Which part of a color field an element is.
#[derive(Component, Clone, Copy, Debug, PartialEq, Eq)]
#[require(StyleOverrides)]
pub enum ColorPart {
    /// `.color-swatch`, the chosen color on the face.
    Swatch,
    /// `.color-panel`.
    Panel,
    /// `.color-area`, the saturation-value square.
    Area,
    /// `.color-area-thumb`.
    AreaThumb,
    /// `.color-hue`, the hue strip.
    Hue,
    /// `.color-hue-thumb`.
    HueThumb,
    /// `.color-hex`, the hex text field.
    Hex,
    /// `.color-alpha`, the alpha strip.
    Alpha,
    /// `.color-alpha-fill`, the strip's clear-to-opaque shading.
    AlphaFill,
    /// `.color-alpha-thumb`.
    AlphaThumb,
}

impl StyleOwner for ColorPart {
    const RANK: u8 = 20;
}

/// sRGB from hue (degrees), saturation and value (0..1).
pub fn hsv_to_rgb([h, s, v]: [f32; 3]) -> [u8; 3] {
    let h = h.rem_euclid(360.0) / 60.0;
    let c = v * s;
    let x = c * (1.0 - (h % 2.0 - 1.0).abs());
    let (r, g, b) = match h as u32 {
        0 => (c, x, 0.0),
        1 => (x, c, 0.0),
        2 => (0.0, c, x),
        3 => (0.0, x, c),
        4 => (x, 0.0, c),
        _ => (c, 0.0, x),
    };
    let m = v - c;
    let byte = |f: f32| ((f + m) * 255.0).round().clamp(0.0, 255.0) as u8;
    [byte(r), byte(g), byte(b)]
}

/// Hue (degrees), saturation and value (0..1) from sRGB. A gray has hue 0.
pub fn rgb_to_hsv(rgb: [u8; 3]) -> [f32; 3] {
    let [r, g, b] = rgb.map(|c| f32::from(c) / 255.0);
    let max = r.max(g).max(b);
    let min = r.min(g).min(b);
    let d = max - min;
    let h = if d == 0.0 {
        0.0
    } else if max == r {
        60.0 * ((g - b) / d).rem_euclid(6.0)
    } else if max == g {
        60.0 * ((b - r) / d + 2.0)
    } else {
        60.0 * ((r - g) / d + 4.0)
    };
    let s = if max == 0.0 { 0.0 } else { d / max };
    [h, s, max]
}

/// The color field `start` sits in (itself included), if any.
fn field_of(
    start: Entity,
    parents: &Query<&ChildOf>,
    is_field: impl Fn(Entity) -> bool,
) -> Option<Entity> {
    let mut cur = Some(start);
    while let Some(e) = cur {
        if is_field(e) {
            return Some(e);
        }
        cur = parents.get(e).ok().map(ChildOf::parent);
    }
    None
}

/// The nearest panel part `start` sits in (itself included), stopping at
/// the field.
fn part_of(
    start: Entity,
    parents: &Query<&ChildOf>,
    parts: &Query<&ColorPart>,
) -> Option<(Entity, ColorPart)> {
    let mut cur = Some(start);
    while let Some(e) = cur {
        if let Ok(part) = parts.get(e) {
            return Some((e, *part));
        }
        cur = parents.get(e).ok().map(ChildOf::parent);
    }
    None
}

/// Open or close `field`'s panel. Closing on a color other than the one it
/// opened with commits it.
fn set_open(
    entity: Entity,
    field: &mut ColorInput,
    open: bool,
    out: &mut MessageWriter<ControlInput>,
) {
    if field.open == open {
        return;
    }
    field.open = open;
    if open {
        field.opened_with = field.rgba;
    } else if field.rgba != field.opened_with {
        out.write(ControlInput {
            entity,
            value: field.value(),
            committed: true,
        });
    }
}

/// Open and close color panels: a click on the face toggles it, a primary
/// press outside the field closes it, and so does Escape inside it.
pub fn toggle_color_panels(
    (mut clicks, mut presses, mut keys): (
        MessageReader<ClickEvent>,
        MessageReader<PointerPressed>,
        MessageReader<FocusedKey>,
    ),
    hovered: Query<Entity, With<Hovered>>,
    parents: Query<&ChildOf>,
    parts: Query<&ColorPart>,
    mut fields: Query<(Entity, &mut ColorInput)>,
    mut out: MessageWriter<ControlInput>,
) {
    if fields.is_empty() {
        clicks.read().for_each(drop);
        presses.read().for_each(drop);
        keys.read().for_each(drop);
        return;
    }
    let target = hovered.iter().next();
    if presses.read().any(|p| p.is_primary()) {
        for (e, mut field) in &mut fields {
            let inside = target
                .and_then(|t| field_of(t, &parents, |x| x == e))
                .is_some();
            if field.open && !inside {
                set_open(e, &mut field, false, &mut out);
            }
        }
    }
    for click in clicks.read() {
        if click.button != PointerButton::Primary {
            continue;
        }
        let Some(e) = field_of(click.entity, &parents, |x| fields.contains(x)) else {
            continue;
        };
        // A click inside the panel is the panel's.
        let in_panel =
            part_of(click.entity, &parents, &parts).is_some_and(|(_, p)| p != ColorPart::Swatch);
        if in_panel {
            continue;
        }
        if let Ok((_, mut field)) = fields.get_mut(e) {
            let open = !field.open;
            set_open(e, &mut field, open, &mut out);
        }
    }
    for key in keys.read() {
        if key.key != Key::Named(NamedKey::Escape) {
            continue;
        }
        let Some(e) = field_of(key.entity, &parents, |x| fields.contains(x)) else {
            continue;
        };
        if let Ok((_, mut field)) = fields.get_mut(e) {
            set_open(e, &mut field, false, &mut out);
        }
    }
}

/// What a drag on a panel holds: the field, and whether it is on the area,
/// the hue strip or the alpha strip, with the element measured.
#[derive(Clone, Copy, Debug)]
pub struct ColorGrab {
    field: Entity,
    on: ColorPart,
    control: Entity,
}

/// Pick a color with the pointer: a primary press on the area or the hue
/// strip moves its thumb there, and dragging keeps moving it until the
/// release, wherever the pointer goes.
pub fn drag_color_panels(
    (mut presses, mut moves, mut releases): (
        MessageReader<PointerPressed>,
        MessageReader<PointerMoved>,
        MessageReader<PointerReleased>,
    ),
    hovered: Query<Entity, With<Hovered>>,
    parents: Query<&ChildOf>,
    parts: Query<&ColorPart>,
    transforms: Query<&Transform>,
    (mut fields, mut out): (Query<&mut ColorInput>, MessageWriter<ControlInput>),
    mut grab: Local<Option<ColorGrab>>,
) {
    let mut points: Vec<Vec2> = Vec::new();
    for press in presses.read() {
        if !press.is_primary() {
            continue;
        }
        *grab = hovered.iter().next().and_then(|t| {
            let (part_e, part) = part_of(t, &parents, &parts)?;
            let (on, control) = match part {
                ColorPart::Area | ColorPart::Hue | ColorPart::Alpha => (part, part_e),
                ColorPart::AreaThumb
                | ColorPart::HueThumb
                | ColorPart::AlphaThumb
                | ColorPart::AlphaFill => {
                    let up = parents.get(part_e).ok()?.parent();
                    let on = match part {
                        ColorPart::AreaThumb => ColorPart::Area,
                        ColorPart::HueThumb => ColorPart::Hue,
                        _ => ColorPart::Alpha,
                    };
                    (on, up)
                }
                _ => return None,
            };
            let field = field_of(control, &parents, |x| fields.contains(x))?;
            Some(ColorGrab { field, on, control })
        });
        if grab.is_some() {
            points.push(press.position);
        }
    }
    if grab.is_some() {
        points.extend(
            moves
                .read()
                .filter(|m| m.pointer.is_primary)
                .map(|m| m.position),
        );
    } else {
        moves.read().for_each(drop);
    }
    let released = releases.read().any(|r| r.is_primary());
    if let (Some(g), Some(&at)) = (*grab, points.last())
        && let (Ok(rect), Ok(mut field)) = (transforms.get(g.control), fields.get_mut(g.field))
        && rect.size.x > 0.0
        && rect.size.y > 0.0
    {
        let frac = ((at - rect.absolute) / rect.size).clamp(Vec2::ZERO, Vec2::ONE);
        let [h, s, v] = field.hsv;
        let before = field.rgba;
        match g.on {
            ColorPart::Alpha => field.set_alpha(frac.x),
            on => {
                let hsv = match on {
                    ColorPart::Area => [h, frac.x, 1.0 - frac.y],
                    _ => [frac.x * 360.0, s, v],
                };
                if hsv != field.hsv {
                    field.set_hsv(hsv);
                }
            }
        }
        if field.rgba != before {
            out.write(ControlInput {
                entity: g.field,
                value: field.value(),
                committed: false,
            });
        }
    }
    if released {
        *grab = None;
    }
}

/// A color typed into the hex field and committed with Enter becomes the
/// field's color: six hex digits, or eight with an alpha when the field
/// takes `alpha`, with or without the `#`. Text that is not a color is put
/// back to the color it was.
pub fn commit_color_hex(
    mut commits: MessageReader<TextInputCommitted>,
    parents: Query<&ChildOf>,
    parts: Query<&ColorPart>,
    mut fields: Query<&mut ColorInput>,
    mut texts: Query<&mut TextContent>,
    mut out: MessageWriter<ControlInput>,
) {
    for commit in commits.read() {
        if parts.get(commit.entity) != Ok(&ColorPart::Hex) {
            continue;
        }
        let Some(e) = field_of(commit.entity, &parents, |x| fields.contains(x)) else {
            continue;
        };
        let Ok(mut field) = fields.get_mut(e) else {
            continue;
        };
        let typed = commit.text.trim();
        let with_hash = if typed.starts_with('#') {
            typed.to_string()
        } else {
            format!("#{typed}")
        };
        let typed = parse_color_value(&with_hash).map(|c| field.sanitized(c));
        match typed {
            Some(rgba) if rgba != field.rgba => {
                field.set_color(rgba);
                out.write(ControlInput {
                    entity: e,
                    value: field.value(),
                    committed: false,
                });
            }
            _ => {}
        }
        // The field shows the color in its own form, whether the typed text
        // was one or not.
        if let Ok(mut text) = texts.get_mut(commit.entity) {
            let hex = field.hex();
            if text.0 != hex {
                text.0 = hex;
            }
        }
    }
}

/// Hold `values` on an element for [`ColorPart`], writing only what changed.
/// Answers whether anything moved.
fn hold_part(
    overrides: &mut StyleOverrides,
    style: Option<&mut Mut<'_, Style>>,
    visuals: Option<&mut Mut<'_, Visuals>>,
    values: StyleValues,
) -> bool {
    let held = overrides.held_by::<ColorPart>();
    let unchanged = held.is_some_and(|h| {
        values
            .inset
            .is_none_or(|i| h.inset.is_some_and(|hi| edges_close(&hi, &i)))
            && values
                .fill
                .as_ref()
                .is_none_or(|f| h.fill.as_ref() == Some(f))
            && values.display.is_none_or(|d| h.display == Some(d))
    });
    if unchanged {
        return false;
    }
    let live = Live {
        style: style.as_deref().map(|s| &**s),
        visuals: visuals.as_deref().map(|v| &**v),
        ..Live::default()
    };
    let out = overrides.hold::<ColorPart>(values, &live);
    if let Some(style) = style {
        out.write_style(style);
    }
    if let Some(visuals) = visuals {
        out.write_visuals(visuals);
    }
    true
}

/// Two insets within half a pixel on every side, `auto` equal to `auto`.
fn edges_close(a: &Edges, b: &Edges) -> bool {
    let close = |x: f32, y: f32| (x.is_nan() && y.is_nan()) || (x - y).abs() < 0.5;
    close(a.top, b.top)
        && close(a.right, b.right)
        && close(a.bottom, b.bottom)
        && close(a.left, b.left)
}

/// An inset placing an element's top-left corner at `at`, its far edges
/// `auto` so it keeps its own size.
fn inset_at(at: Vec2) -> Edges {
    Edges {
        top: at.y,
        left: at.x,
        right: f32::NAN,
        bottom: f32::NAN,
        ..Default::default()
    }
}

/// What [`sync_color_parts`] writes on a part.
type PartWrite = (
    Entity,
    &'static ColorPart,
    &'static mut StyleOverrides,
    Option<&'static mut Style>,
    Option<&'static mut Visuals>,
    Option<&'static mut TextContent>,
);

/// Show each field's color on its parts: the swatch's fill always, and
/// while the panel is open, the area's hue, the thumbs' places, the hex
/// text, and the panel's own place below the face.
///
/// The thumbs and the panel are placed against the boxes layout gave them,
/// so a tick that moves one asks for another to place it against the
/// layout this one produces.
pub fn sync_color_parts(
    viewport: Option<Res<Viewport>>,
    anim: Option<Res<AnimationsActive>>,
    fields: Query<(Entity, &ColorInput)>,
    parents: Query<&ChildOf>,
    transforms: Query<&Transform>,
    focused: Query<(), With<Focused>>,
    mut parts: Query<PartWrite>,
) {
    if fields.is_empty() {
        return;
    }
    let vp = viewport
        .map(|v| v.size)
        .unwrap_or(Vec2::splat(f32::INFINITY));
    let mut moved = false;
    for (entity, part, mut overrides, mut style, mut visuals, text) in &mut parts {
        let Some(field_e) = field_of(entity, &parents, |x| fields.contains(x)) else {
            continue;
        };
        let Ok((_, field)) = fields.get(field_e) else {
            continue;
        };
        let [r, g, b, a] = field.rgba;
        let size = |e: Entity| transforms.get(e).map(|t| t.size).unwrap_or(Vec2::ZERO);
        let parent_size = parents
            .get(entity)
            .map(|c| size(c.parent()))
            .unwrap_or(Vec2::ZERO);
        let values = match part {
            ColorPart::Swatch => StyleValues {
                fill: Some(Some(Fill::Solid(Color::from_rgba8([r, g, b, a])))),
                ..StyleValues::default()
            },
            ColorPart::Panel => {
                let display = if field.open {
                    Display::Flex
                } else {
                    Display::None
                };
                let face = transforms.get(field_e).ok();
                let at = match (face, field.open) {
                    (Some(face), true) => {
                        let origin = anchored_popup_origin(
                            face.absolute,
                            face.size,
                            size(entity),
                            vp,
                            PANEL_GAP,
                            PopupSide::Below,
                        );
                        origin - face.absolute
                    }
                    _ => Vec2::new(0.0, face.map(|f| f.size.y + PANEL_GAP).unwrap_or(0.0)),
                };
                StyleValues {
                    display: Some(display),
                    inset: Some(inset_at(at)),
                    ..StyleValues::default()
                }
            }
            _ if !field.open => continue,
            ColorPart::Area => {
                let hue = hsv_to_rgb([field.hsv[0], 1.0, 1.0]);
                StyleValues {
                    fill: Some(Some(Fill::Solid(Color::from_rgba8([
                        hue[0], hue[1], hue[2], 255,
                    ])))),
                    ..StyleValues::default()
                }
            }
            ColorPart::AreaThumb => {
                let thumb = size(entity);
                let at = Vec2::new(field.hsv[1], 1.0 - field.hsv[2]) * parent_size - thumb / 2.0;
                StyleValues {
                    inset: Some(inset_at(at)),
                    ..StyleValues::default()
                }
            }
            ColorPart::HueThumb | ColorPart::AlphaThumb => {
                let thumb = size(entity);
                let along = match part {
                    ColorPart::HueThumb => field.hsv[0] / 360.0,
                    _ => f32::from(a) / 255.0,
                };
                let at = Vec2::new(
                    along * parent_size.x - thumb.x / 2.0,
                    (parent_size.y - thumb.y) / 2.0,
                );
                StyleValues {
                    inset: Some(inset_at(at)),
                    ..StyleValues::default()
                }
            }
            // The color from clear to opaque, left to right.
            ColorPart::AlphaFill => StyleValues {
                fill: Some(Some(Fill::Linear {
                    angle_deg: 90.0,
                    stops: vec![
                        (0.0, Color::from_rgba8([r, g, b, 0])),
                        (1.0, Color::from_rgba8([r, g, b, 255])),
                    ],
                })),
                ..StyleValues::default()
            },
            ColorPart::Hex => {
                // A field being typed into keeps what is typed until Enter.
                if !focused.contains(entity)
                    && let Some(mut text) = text
                {
                    let hex = field.hex();
                    if text.0 != hex {
                        text.0 = hex;
                    }
                }
                continue;
            }
            ColorPart::Hue | ColorPart::Alpha => continue,
        };
        moved |= hold_part(&mut overrides, style.as_mut(), visuals.as_mut(), values);
    }
    if moved && let Some(anim) = &anim {
        anim.request();
    }
}

/// Keep each `bind-value` color field and its signal in step: a change made
/// in the field writes the signal, a value written to the signal elsewhere
/// moves the field, and a signal nothing wrote yet starts at the field's
/// color.
pub fn sync_color_bindings(
    mut store: ResMut<PropertyStore>,
    mut fields: Query<(&mut ColorInput, &BindValue)>,
) {
    for (mut field, bind) in &mut fields {
        let value = field.value();
        let current = store
            .get_global_str(&bind.0)
            .map(|s| s.to_string())
            .filter(|s| !s.is_empty());
        match current {
            Some(written) if field.synced.as_deref() != Some(written.as_str()) => {
                // Written elsewhere since the last exchange.
                if let Some(rgba) = parse_color_value(&written) {
                    field.set_color(rgba);
                }
                field.synced = Some(written);
            }
            _ if field.synced.as_deref() != Some(value.as_str()) => {
                store.set_global_str(&bind.0, value.as_str());
                field.synced = Some(value);
            }
            _ => {}
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use bevy_ecs::message::Messages;
    use bevy_ecs::system::RunSystemOnce;

    #[test]
    fn hsv_round_trips_through_rgb() {
        for rgb in [
            [255, 0, 0],
            [51, 102, 255],
            [0, 0, 0],
            [128, 128, 128],
            [12, 200, 77],
        ] {
            assert_eq!(hsv_to_rgb(rgb_to_hsv(rgb)), rgb, "{rgb:?}");
        }
        assert_eq!(hsv_to_rgb([120.0, 1.0, 1.0]), [0, 255, 0]);
        assert_eq!(hsv_to_rgb([240.0, 0.5, 1.0]), [128, 128, 255]);
    }

    /// A gray keeps the hue the panel was on, so dragging the area to its
    /// edge and back finds the same hue.
    #[test]
    fn a_gray_keeps_the_hue() {
        let mut field = ColorInput::new([255, 0, 0, 255], false);
        field.set_hsv([200.0, 1.0, 1.0]);
        field.set_color([90, 90, 90, 255]);
        assert_eq!(field.hsv[0], 200.0);
        assert_eq!(field.value(), "#5a5a5a");
    }

    fn panel_world() -> (World, Entity, Entity) {
        let mut world = World::new();
        world.init_resource::<Messages<ClickEvent>>();
        world.init_resource::<Messages<PointerPressed>>();
        world.init_resource::<Messages<PointerMoved>>();
        world.init_resource::<Messages<PointerReleased>>();
        world.init_resource::<Messages<FocusedKey>>();
        world.init_resource::<Messages<ControlInput>>();
        let field = world
            .spawn((
                ColorInput::new([255, 0, 0, 255], false),
                Transform::new(Vec2::new(10.0, 10.0), Vec2::new(48.0, 28.0)),
            ))
            .id();
        let panel = world.spawn((ColorPart::Panel, ChildOf(field))).id();
        let area = world
            .spawn((
                ColorPart::Area,
                ChildOf(panel),
                Transform::new(Vec2::new(10.0, 50.0), Vec2::new(200.0, 100.0)),
            ))
            .id();
        (world, field, area)
    }

    fn inputs(world: &mut World) -> Vec<(String, bool)> {
        let out = world
            .resource::<Messages<ControlInput>>()
            .iter_current_update_messages()
            .map(|c| (c.value.clone(), c.committed))
            .collect();
        world.resource_mut::<Messages<ControlInput>>().clear();
        out
    }

    /// A click on the face opens the panel; a press outside closes it, and
    /// closing on a new color commits it, which is `change`.
    #[test]
    fn the_face_opens_the_panel_and_closing_commits() {
        let (mut world, field, area) = panel_world();
        world.write_message(ClickEvent {
            entity: field,
            position: Vec2::new(20.0, 20.0),
            button: PointerButton::Primary,
            local: None,
        });
        world.run_system_once(toggle_color_panels).unwrap();
        assert!(world.get::<ColorInput>(field).unwrap().open);
        world.resource_mut::<Messages<ClickEvent>>().clear();

        // Press on the area's right edge, halfway down: full saturation,
        // half value, still red.
        world.entity_mut(area).insert(Hovered);
        world.write_message(PointerPressed {
            position: Vec2::new(210.0, 100.0),
            button: PointerButton::Primary,
            local: None,
            pointer: Default::default(),
        });
        world.run_system_once(drag_color_panels).unwrap();
        assert_eq!(inputs(&mut world), vec![("#800000".to_string(), false)]);
        world.resource_mut::<Messages<PointerPressed>>().clear();
        world.entity_mut(area).remove::<Hovered>();

        // A press on nothing closes it, committing the new color.
        world.write_message(PointerPressed {
            position: Vec2::new(400.0, 400.0),
            button: PointerButton::Primary,
            local: None,
            pointer: Default::default(),
        });
        world.run_system_once(toggle_color_panels).unwrap();
        assert!(!world.get::<ColorInput>(field).unwrap().open);
        assert_eq!(inputs(&mut world), vec![("#800000".to_string(), true)]);
    }

    /// A bound signal starts at the field's color, follows the field's
    /// changes, and moves the field when written elsewhere.
    #[test]
    fn the_bound_signal_and_the_field_stay_in_step() {
        let mut world = World::new();
        world.insert_resource(PropertyStore::default());
        let field = world
            .spawn((
                ColorInput::new([0, 0, 255, 255], false),
                BindValue("tint".to_string()),
            ))
            .id();
        let read = |world: &World| {
            world
                .resource::<PropertyStore>()
                .get_global_str("tint")
                .map(|s| s.to_string())
        };
        world.run_system_once(sync_color_bindings).unwrap();
        assert_eq!(read(&world).as_deref(), Some("#0000ff"));

        world
            .get_mut::<ColorInput>(field)
            .unwrap()
            .set_color([1, 2, 3, 255]);
        world.run_system_once(sync_color_bindings).unwrap();
        assert_eq!(read(&world).as_deref(), Some("#010203"));

        world
            .resource_mut::<PropertyStore>()
            .set_global_str("tint", "#FFAA00");
        world.run_system_once(sync_color_bindings).unwrap();
        assert_eq!(
            world.get::<ColorInput>(field).unwrap().rgba,
            [255, 170, 0, 255]
        );
    }

    /// With `alpha`, a press on the alpha strip picks the alpha, and the
    /// value carries it in HTML's `color(srgb r g b / a)` form.
    #[test]
    fn the_alpha_strip_picks_the_alpha() {
        let (mut world, field, area) = panel_world();
        world
            .entity_mut(field)
            .insert(ColorInput::new([255, 0, 0, 255], true));
        let panel = world.get::<ChildOf>(area).unwrap().parent();
        let strip = world
            .spawn((
                ColorPart::Alpha,
                ChildOf(panel),
                Transform::new(Vec2::new(10.0, 160.0), Vec2::new(200.0, 12.0)),
                Hovered,
            ))
            .id();
        world.write_message(PointerPressed {
            position: Vec2::new(60.0, 166.0),
            button: PointerButton::Primary,
            local: None,
            pointer: Default::default(),
        });
        world.run_system_once(drag_color_panels).unwrap();
        assert_eq!(
            inputs(&mut world),
            vec![("color(srgb 1 0 0 / 0.25098)".to_string(), false)]
        );
        let color = world.get::<ColorInput>(field).unwrap();
        assert_eq!(color.rgba, [255, 0, 0, 64]);
        assert_eq!(color.hex(), "#ff000040");
        world.entity_mut(strip).despawn();
    }

    /// The hex field takes eight digits on a field with `alpha`; one without
    /// keeps its color opaque whatever is typed.
    #[test]
    fn the_hex_field_takes_an_alpha_only_with_alpha() {
        for (alpha, want, shown) in [
            (true, [0x33, 0x66, 0xff, 0x80], "#3366ff80"),
            (false, [0x33, 0x66, 0xff, 0xff], "#3366ff"),
        ] {
            let mut world = World::new();
            world.init_resource::<Messages<TextInputCommitted>>();
            world.init_resource::<Messages<ControlInput>>();
            let field = world.spawn(ColorInput::new([0, 0, 0, 255], alpha)).id();
            let hex = world
                .spawn((ColorPart::Hex, ChildOf(field), TextContent::default()))
                .id();
            world.write_message(TextInputCommitted {
                entity: hex,
                text: "3366ff80".to_string(),
            });
            world.run_system_once(commit_color_hex).unwrap();
            assert_eq!(world.get::<ColorInput>(field).unwrap().rgba, want);
            assert_eq!(world.get::<TextContent>(hex).unwrap().0, shown);
        }
    }
}
