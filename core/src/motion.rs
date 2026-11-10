//! The value math every motion in Lumen shares: timing functions and the
//! interpolation of each animatable value type.
//!
//! CSS transitions and keyframe animations both sample a timeline through an
//! [`Easing`] and blend two values with [`Interpolate`]; this module is the one
//! implementation of both, so a `transition` and an `animation` running the
//! same curve between the same values land on the same pixels.
//!
//! The curves follow CSS Easing Functions Level 2: the named curves are the
//! cubic Beziers CSS defines, `steps()` jumps where CSS says it does, and
//! `linear()` is the piecewise-linear function through its stops. Colors
//! blend with premultiplied alpha, as CSS Color 4 does for legacy sRGB colors,
//! so a fade from `transparent` to a color never passes through grey.

use std::sync::Arc;

use glam::Vec2;

use crate::components::{Border, Color, ShadowSpec};
use crate::node_ir::Affine2;
use crate::transform::{ElementTransform, LengthPct, TransformFn};

/// Where a `steps()` timing function jumps.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum StepPosition {
    /// `jump-start` / `start`: the first jump happens at the start.
    Start,
    /// `jump-end` / `end`: the last jump happens at the end.
    End,
    /// `jump-none`: no jump at either end; the first and last steps hold.
    None,
    /// `jump-both`: a jump at both ends.
    Both,
}

/// One stop of a `linear()` timing function: `output` at `input`, both in
/// progress units (`0` to `1`, though either may overshoot).
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct LinearStop {
    /// The input progress this stop sits at.
    pub input: f32,
    /// The output the curve takes there.
    pub output: f32,
}

/// A timing function: maps linear progress through a timeline to eased
/// progress.
#[derive(Clone, Debug, PartialEq)]
pub enum Easing {
    /// `linear`: `f(t) = t`.
    Linear,
    /// `cubic-bezier(p1x, p1y, p2x, p2y)`, anchored at `(0, 0)` and `(1, 1)`.
    /// The named curves (`ease`, `ease-in`, ...) are the constants on this
    /// type.
    CubicBezier(f32, f32, f32, f32),
    /// `steps(n, position)`.
    Steps(u32, StepPosition),
    /// `linear(...)`, with every stop's input resolved and in order.
    Points(Arc<[LinearStop]>),
}

impl Easing {
    /// CSS `ease`.
    pub const EASE: Easing = Easing::CubicBezier(0.25, 0.1, 0.25, 1.0);
    /// CSS `ease-in`.
    pub const EASE_IN: Easing = Easing::CubicBezier(0.42, 0.0, 1.0, 1.0);
    /// CSS `ease-out`.
    pub const EASE_OUT: Easing = Easing::CubicBezier(0.0, 0.0, 0.58, 1.0);
    /// CSS `ease-in-out`.
    pub const EASE_IN_OUT: Easing = Easing::CubicBezier(0.42, 0.0, 0.58, 1.0);
    /// CSS `step-start`.
    pub const STEP_START: Easing = Easing::Steps(1, StepPosition::Start);
    /// CSS `step-end`.
    pub const STEP_END: Easing = Easing::Steps(1, StepPosition::End);

    /// A `linear()` curve through `stops`, given as CSS writes them: an
    /// output and, optionally, one or two input positions each. Missing
    /// inputs spread evenly between the known ones, a first missing one is
    /// `0`, a last missing one is `1`, and an input below an earlier one is
    /// raised to it. `None` for fewer than two stops.
    pub fn linear_points(stops: &[(f32, Vec<f32>)]) -> Option<Easing> {
        if stops.len() < 2 {
            return None;
        }
        // One entry per (output, input) pair; a stop with two inputs is two.
        let mut pts: Vec<(f32, Option<f32>)> = Vec::new();
        for (output, inputs) in stops {
            match inputs.as_slice() {
                [] => pts.push((*output, None)),
                list => pts.extend(list.iter().map(|i| (*output, Some(*i)))),
            }
        }
        let n = pts.len();
        if pts[0].1.is_none() {
            pts[0].1 = Some(0.0);
        }
        if pts[n - 1].1.is_none() {
            pts[n - 1].1 = Some(1.0f32.max(pts.iter().filter_map(|p| p.1).fold(0.0, f32::max)));
        }
        // Monotonic inputs.
        let mut highest = f32::NEG_INFINITY;
        for p in pts.iter_mut() {
            if let Some(i) = p.1.as_mut() {
                if *i < highest {
                    *i = highest;
                }
                highest = *i;
            }
        }
        // Spread the missing inputs between their known neighbours.
        let mut i = 0;
        while i < n {
            if pts[i].1.is_some() {
                i += 1;
                continue;
            }
            let start = i - 1;
            let mut end = i;
            while pts[end].1.is_none() {
                end += 1;
            }
            let (a, b) = (pts[start].1.unwrap_or(0.0), pts[end].1.unwrap_or(1.0));
            let span = (end - start) as f32;
            for (k, p) in pts.iter_mut().enumerate().take(end).skip(i) {
                p.1 = Some(a + (b - a) * (k - start) as f32 / span);
            }
            i = end;
        }
        Some(Easing::Points(
            pts.into_iter()
                .map(|(output, input)| LinearStop {
                    output,
                    input: input.unwrap_or(0.0),
                })
                .collect(),
        ))
    }

    /// The eased progress at linear progress `t`. `t` is clamped to
    /// `[0, 1]`; the output may overshoot that range (a back-out Bezier, a
    /// spring sampled into `linear()`).
    pub fn apply(&self, t: f32) -> f32 {
        let t = t.clamp(0.0, 1.0);
        match self {
            Easing::Linear => t,
            Easing::CubicBezier(p1x, p1y, p2x, p2y) => cubic_bezier(*p1x, *p1y, *p2x, *p2y, t),
            Easing::Steps(n, pos) => steps(*n, *pos, t),
            Easing::Points(stops) => linear_through(stops, t),
        }
    }

    /// The first linear progress after `t` at which the output changes, for
    /// a curve that only changes in jumps: a `steps()` function. `None` for
    /// a continuous curve, whose output changes on every frame, and once no
    /// jump is left.
    pub fn next_jump(&self, t: f32) -> Option<f32> {
        let Easing::Steps(n, _) = self else {
            return None;
        };
        let n = (*n).max(1);
        // Every position jumps at the interior edges k / n; `jump-start` and
        // `jump-both` also jump at 0, which is before any `t` here, and
        // `jump-end` and `jump-both` at 1, which is k = n.
        let k = (t * n as f32).floor() as u32 + 1;
        (k <= n).then(|| k as f32 / n as f32)
    }
}

/// CSS `steps(n, position)` at `t` in `[0, 1]`.
fn steps(n: u32, pos: StepPosition, t: f32) -> f32 {
    let n = n.max(1) as i64;
    let mut step = (t * n as f32).floor() as i64;
    if matches!(pos, StepPosition::Start | StepPosition::Both) {
        step += 1;
    }
    let jumps = match pos {
        StepPosition::Start | StepPosition::End => n,
        StepPosition::None => (n - 1).max(1),
        StepPosition::Both => n + 1,
    };
    let step = step.clamp(0, jumps);
    step as f32 / jumps as f32
}

/// The piecewise-linear `linear()` curve through `stops` at `t`.
fn linear_through(stops: &[LinearStop], t: f32) -> f32 {
    let Some(first) = stops.first() else {
        return t;
    };
    if t <= first.input {
        return first.output;
    }
    for pair in stops.windows(2) {
        let (a, b) = (pair[0], pair[1]);
        if t <= b.input {
            if b.input <= a.input {
                return b.output;
            }
            let f = (t - a.input) / (b.input - a.input);
            return a.output + (b.output - a.output) * f;
        }
    }
    stops.last().map_or(t, |s| s.output)
}

/// Sample the CSS cubic Bezier `(0, 0) - (p1x, p1y) - (p2x, p2y) - (1, 1)`
/// at `t`: Newton-Raphson inverts `x(s) = t`, with a bisection fallback,
/// then `y(s)` is the result.
fn cubic_bezier(p1x: f32, p1y: f32, p2x: f32, p2y: f32, t: f32) -> f32 {
    let ax = 3.0 * p1x - 3.0 * p2x + 1.0;
    let bx = -6.0 * p1x + 3.0 * p2x;
    let cx = 3.0 * p1x;
    let ay = 3.0 * p1y - 3.0 * p2y + 1.0;
    let by = -6.0 * p1y + 3.0 * p2y;
    let cy = 3.0 * p1y;
    let x_at = |s: f32| ((ax * s + bx) * s + cx) * s;
    let dx_at = |s: f32| (3.0 * ax * s + 2.0 * bx) * s + cx;
    let y_at = |s: f32| ((ay * s + by) * s + cy) * s;
    let mut s = t;
    for _ in 0..8 {
        let x = x_at(s) - t;
        if x.abs() < 1e-6 {
            return y_at(s);
        }
        let dx = dx_at(s);
        if dx.abs() < 1e-6 {
            break;
        }
        s -= x / dx;
    }
    let (mut lo, mut hi) = (0.0f32, 1.0f32);
    s = t;
    for _ in 0..32 {
        let x = x_at(s);
        if (x - t).abs() < 1e-6 {
            break;
        }
        if x < t {
            lo = s;
        } else {
            hi = s;
        }
        s = 0.5 * (lo + hi);
    }
    y_at(s)
}

/// A value that blends toward another of its kind.
pub trait Interpolate: Sized {
    /// The value a fraction `t` of the way from `self` to `to`. `t` may
    /// leave `[0, 1]` when an easing overshoots.
    fn interpolate(&self, to: &Self, t: f32) -> Self;
}

impl Interpolate for f32 {
    fn interpolate(&self, to: &Self, t: f32) -> Self {
        self + (to - self) * t
    }
}

impl Interpolate for Color {
    /// Premultiplied sRGB, as CSS Color 4 blends legacy colors: a color
    /// fading in from `transparent` keeps its hue on the way.
    fn interpolate(&self, to: &Self, t: f32) -> Self {
        let a = self.a + (to.a - self.a) * t;
        let channel = |x: f32, y: f32| {
            let pre = (x * self.a) + (y * to.a - x * self.a) * t;
            if a > 0.0 { pre / a } else { 0.0 }
        };
        Color {
            r: channel(self.r, to.r).clamp(0.0, 1.0),
            g: channel(self.g, to.g).clamp(0.0, 1.0),
            b: channel(self.b, to.b).clamp(0.0, 1.0),
            a: a.clamp(0.0, 1.0),
        }
    }
}

impl Interpolate for LengthPct {
    fn interpolate(&self, to: &Self, t: f32) -> Self {
        self.lerp(*to, t)
    }
}

impl Interpolate for ShadowSpec {
    fn interpolate(&self, to: &Self, t: f32) -> Self {
        ShadowSpec {
            offset_x: self.offset_x.interpolate(&to.offset_x, t),
            offset_y: self.offset_y.interpolate(&to.offset_y, t),
            blur: self.blur.interpolate(&to.blur, t).max(0.0),
            spread: self.spread.interpolate(&to.spread, t),
            color: self.color.interpolate(&to.color, t),
            inner: if t < 0.5 { self.inner } else { to.inner },
        }
    }
}

/// A shadow that draws nothing, matching `like` in kind: what a shorter
/// shadow list is padded with so it blends with a longer one.
fn no_shadow(like: &ShadowSpec) -> ShadowSpec {
    ShadowSpec {
        offset_x: 0.0,
        offset_y: 0.0,
        blur: 0.0,
        spread: 0.0,
        color: Color {
            r: 0.0,
            g: 0.0,
            b: 0.0,
            a: 0.0,
        },
        inner: like.inner,
    }
}

/// Blend two `box-shadow` lists, CSS-style: pairwise, the shorter padded
/// with transparent shadows. A pair that disagrees on `inset` cannot blend,
/// and then the whole list flips at the midpoint.
pub fn interpolate_shadows(from: &[ShadowSpec], to: &[ShadowSpec], t: f32) -> Vec<ShadowSpec> {
    let n = from.len().max(to.len());
    let mut out = Vec::with_capacity(n);
    for i in 0..n {
        let a = from.get(i).copied().unwrap_or_else(|| no_shadow(&to[i]));
        let b = to.get(i).copied().unwrap_or_else(|| no_shadow(&from[i]));
        if a.inner != b.inner {
            return discrete(from, to, t).to_vec();
        }
        out.push(a.interpolate(&b, t));
    }
    out
}

/// Blend two borders: the color (and per-side colors) blend when the
/// widths agree; otherwise the border flips at the midpoint, since widths
/// are layout.
pub fn interpolate_border(from: &Border, to: &Border, t: f32) -> Border {
    if from.widths != to.widths {
        return *discrete(from, to, t);
    }
    let sides = match (from.side_colors, to.side_colors) {
        (None, None) => None,
        (a, b) => {
            let a = a.unwrap_or([from.color; 4]);
            let b = b.unwrap_or([to.color; 4]);
            Some([0, 1, 2, 3].map(|i| a[i].interpolate(&b[i], t)))
        }
    };
    Border {
        widths: from.widths,
        color: from.color.interpolate(&to.color, t),
        side_colors: sides,
    }
}

/// The CSS rule for values with no in-between: the first until halfway,
/// the second from there on.
pub fn discrete<'a, T: ?Sized>(from: &'a T, to: &'a T, t: f32) -> &'a T {
    if t < 0.5 { from } else { to }
}

/// Blend two corner radius sets, per corner.
pub fn interpolate_radii(from: [f32; 4], to: [f32; 4], t: f32) -> [f32; 4] {
    [0, 1, 2, 3].map(|i| from[i].interpolate(&to[i], t).max(0.0))
}

/// Blend two element transforms, per CSS Transforms 2: the individual
/// properties component by component, and the `transform` lists function
/// by function when they match in kind (a missing tail counts as identity
/// functions of the other's kinds). Lists that do not match are each
/// reduced to a matrix, decomposed into translation, rotation, skew and
/// scale, and blended in that form; `size` resolves their percentages.
pub fn interpolate_transform(
    from: &ElementTransform,
    to: &ElementTransform,
    t: f32,
    size: Vec2,
) -> ElementTransform {
    let functions = interpolate_functions(&from.functions, &to.functions, t, size);
    ElementTransform {
        functions,
        translate: (
            from.translate.0.interpolate(&to.translate.0, t),
            from.translate.1.interpolate(&to.translate.1, t),
        ),
        rotate: from.rotate.interpolate(&to.rotate, t),
        scale: (
            from.scale.0.interpolate(&to.scale.0, t),
            from.scale.1.interpolate(&to.scale.1, t),
        ),
        origin: (
            from.origin.0.interpolate(&to.origin.0, t),
            from.origin.1.interpolate(&to.origin.1, t),
        ),
    }
}

fn same_kind(a: &TransformFn, b: &TransformFn) -> bool {
    std::mem::discriminant(a) == std::mem::discriminant(b)
}

fn interpolate_functions(
    from: &[TransformFn],
    to: &[TransformFn],
    t: f32,
    size: Vec2,
) -> Vec<TransformFn> {
    let n = from.len().max(to.len());
    let pad = |list: &[TransformFn], other: &[TransformFn], i: usize| {
        list.get(i)
            .copied()
            .unwrap_or_else(|| other[i].identity_like())
    };
    let matched = (0..n).all(|i| same_kind(&pad(from, to, i), &pad(to, from, i)));
    if matched {
        return (0..n)
            .map(|i| {
                let (a, b) = (pad(from, to, i), pad(to, from, i));
                match (a, b) {
                    (TransformFn::Translate(ax, ay), TransformFn::Translate(bx, by)) => {
                        TransformFn::Translate(ax.interpolate(&bx, t), ay.interpolate(&by, t))
                    }
                    (TransformFn::Scale(ax, ay), TransformFn::Scale(bx, by)) => {
                        TransformFn::Scale(ax.interpolate(&bx, t), ay.interpolate(&by, t))
                    }
                    (TransformFn::Rotate(a), TransformFn::Rotate(b)) => {
                        TransformFn::Rotate(a.interpolate(&b, t))
                    }
                    (TransformFn::Skew(ax, ay), TransformFn::Skew(bx, by)) => {
                        TransformFn::Skew(ax.interpolate(&bx, t), ay.interpolate(&by, t))
                    }
                    (TransformFn::Matrix(a), TransformFn::Matrix(b)) => {
                        matrix_fn(&blend_matrices(&to_affine(&a), &to_affine(&b), t))
                    }
                    _ => unreachable!("kinds were matched above"),
                }
            })
            .collect();
    }
    let product = |list: &[TransformFn]| {
        list.iter()
            .fold(Affine2::IDENTITY, |m, f| m * f.matrix(size))
    };
    vec![matrix_fn(&blend_matrices(&product(from), &product(to), t))]
}

fn to_affine(m: &[f32; 6]) -> Affine2 {
    Affine2 {
        coeffs: m.map(f64::from),
    }
}

fn matrix_fn(m: &Affine2) -> TransformFn {
    TransformFn::Matrix(m.coeffs.map(|c| c as f32))
}

/// A 2D matrix decomposed the CSS way: translation, scale, skew (as the
/// shear factor between the axes) and rotation in radians.
#[derive(Clone, Copy, Debug)]
struct Decomposed {
    translate: (f64, f64),
    scale: (f64, f64),
    skew: f64,
    angle: f64,
}

/// Split a 2D matrix into `translate * rotate * skew * scale`, the form CSS
/// Transforms blends mismatched lists in. A flip lands on the y scale.
fn decompose(m: &Affine2) -> Decomposed {
    let [a, b, c, d, e, f] = m.coeffs;
    let sx = (a * a + b * b).sqrt();
    let angle = if sx > 0.0 { b.atan2(a) } else { 0.0 };
    // Undo the rotation on the second column: what is left is the skew
    // times the y scale, over the y scale.
    let (sin, cos) = angle.sin_cos();
    let shear = cos * c + sin * d;
    let sy = -sin * c + cos * d;
    let skew = if sy != 0.0 { shear / sy } else { 0.0 };
    Decomposed {
        translate: (e, f),
        scale: (sx, sy),
        skew,
        angle,
    }
}

fn recompose(d: &Decomposed) -> Affine2 {
    let (sin, cos) = d.angle.sin_cos();
    let rotate = Affine2 {
        coeffs: [cos, sin, -sin, cos, 0.0, 0.0],
    };
    let skew = Affine2 {
        coeffs: [1.0, 0.0, d.skew, 1.0, 0.0, 0.0],
    };
    let scale = Affine2 {
        coeffs: [d.scale.0, 0.0, 0.0, d.scale.1, 0.0, 0.0],
    };
    Affine2::translate(d.translate.0, d.translate.1) * rotate * skew * scale
}

fn blend_matrices(from: &Affine2, to: &Affine2, t: f32) -> Affine2 {
    let (a, b) = (decompose(from), decompose(to));
    let t = f64::from(t);
    let mix = |x: f64, y: f64| x + (y - x) * t;
    // Rotate the short way round.
    let mut angle_b = b.angle;
    let turn = std::f64::consts::TAU;
    while angle_b - a.angle > std::f64::consts::PI {
        angle_b -= turn;
    }
    while angle_b - a.angle < -std::f64::consts::PI {
        angle_b += turn;
    }
    recompose(&Decomposed {
        translate: (
            mix(a.translate.0, b.translate.0),
            mix(a.translate.1, b.translate.1),
        ),
        scale: (mix(a.scale.0, b.scale.0), mix(a.scale.1, b.scale.1)),
        skew: mix(a.skew, b.skew),
        angle: mix(a.angle, angle_b),
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn close(a: f32, b: f32) -> bool {
        (a - b).abs() < 2e-3
    }

    #[test]
    fn named_curves_are_the_css_beziers() {
        // Reference values from the CSS definitions, sampled at t = 0.5.
        assert!(close(Easing::EASE.apply(0.5), 0.8024));
        assert!(close(Easing::EASE_IN.apply(0.5), 0.3153));
        assert!(close(Easing::EASE_OUT.apply(0.5), 0.6847));
        assert!(close(Easing::EASE_IN_OUT.apply(0.5), 0.5));
        for e in [
            Easing::EASE,
            Easing::EASE_IN,
            Easing::EASE_OUT,
            Easing::EASE_IN_OUT,
        ] {
            assert!(close(e.apply(0.0), 0.0));
            assert!(close(e.apply(1.0), 1.0));
        }
    }

    #[test]
    fn steps_jump_where_css_says() {
        let end = Easing::Steps(4, StepPosition::End);
        assert_eq!(end.apply(0.0), 0.0);
        assert_eq!(end.apply(0.24), 0.0);
        assert_eq!(end.apply(0.25), 0.25);
        assert_eq!(end.apply(0.99), 0.75);
        assert_eq!(end.apply(1.0), 1.0);
        let start = Easing::Steps(4, StepPosition::Start);
        assert_eq!(start.apply(0.0), 0.25);
        assert_eq!(start.apply(0.8), 1.0);
        let none = Easing::Steps(3, StepPosition::None);
        assert_eq!(none.apply(0.0), 0.0);
        assert_eq!(none.apply(0.5), 0.5);
        assert_eq!(none.apply(1.0), 1.0);
        let both = Easing::Steps(3, StepPosition::Both);
        assert_eq!(both.apply(0.0), 0.25);
        assert_eq!(both.apply(1.0), 1.0);
        assert_eq!(Easing::STEP_END.apply(0.999), 0.0);
        assert_eq!(Easing::STEP_START.apply(0.001), 1.0);
    }

    #[test]
    fn steps_report_their_next_jump_and_curves_do_not() {
        let e = Easing::Steps(4, StepPosition::End);
        assert_eq!(e.next_jump(0.1), Some(0.25));
        assert_eq!(e.next_jump(0.25), Some(0.5));
        assert_eq!(e.next_jump(0.9), Some(1.0));
        assert_eq!(e.next_jump(1.0), None);
        assert_eq!(Easing::EASE.next_jump(0.1), None);
    }

    #[test]
    fn linear_fills_missing_inputs_evenly() {
        let e = Easing::linear_points(&[(0.0, vec![]), (0.5, vec![]), (1.0, vec![])]).unwrap();
        assert!(close(e.apply(0.25), 0.25));
        // linear(0, 0.25 75%, 1): flat-ish then steep.
        let e = Easing::linear_points(&[(0.0, vec![]), (0.25, vec![0.75]), (1.0, vec![])]).unwrap();
        assert!(close(e.apply(0.75), 0.25));
        assert!(close(e.apply(0.375), 0.125));
        assert!(close(e.apply(0.875), 0.625));
        // A stop with two inputs holds its output across both.
        let e = Easing::linear_points(&[(0.0, vec![]), (0.5, vec![0.25, 0.75]), (1.0, vec![])])
            .unwrap();
        assert!(close(e.apply(0.5), 0.5));
        assert!(Easing::linear_points(&[(0.0, vec![])]).is_none());
    }

    #[test]
    fn colors_blend_premultiplied() {
        let clear = Color {
            r: 0.0,
            g: 0.0,
            b: 0.0,
            a: 0.0,
        };
        let red = Color::rgb(1.0, 0.0, 0.0);
        let mid = clear.interpolate(&red, 0.5);
        assert!(close(mid.a, 0.5));
        assert!(close(mid.r, 1.0), "red stays red on its way in, not grey");
        let blue = Color::rgb(0.0, 0.0, 1.0);
        let m = red.interpolate(&blue, 0.5);
        assert!(close(m.r, 0.5) && close(m.b, 0.5) && close(m.a, 1.0));
    }

    #[test]
    fn shadow_lists_pad_with_transparent_shadows() {
        let a = ShadowSpec {
            offset_x: 0.0,
            offset_y: 4.0,
            blur: 8.0,
            spread: 0.0,
            color: Color::rgb(0.0, 0.0, 0.0),
            inner: false,
        };
        let out = interpolate_shadows(&[a], &[], 0.5);
        assert_eq!(out.len(), 1);
        assert!(close(out[0].offset_y, 2.0));
        assert!(close(out[0].color.a, 0.5));
        let inset = ShadowSpec { inner: true, ..a };
        assert_eq!(interpolate_shadows(&[a], &[inset], 0.4)[0], a);
        assert_eq!(interpolate_shadows(&[a], &[inset], 0.6)[0], inset);
    }

    #[test]
    fn matching_transform_lists_blend_function_by_function() {
        let from = ElementTransform {
            functions: vec![TransformFn::Rotate(0.0)],
            ..ElementTransform::default()
        };
        let to = ElementTransform {
            functions: vec![TransformFn::Rotate(90.0), TransformFn::Scale(2.0, 2.0)],
            translate: (LengthPct::px(10.0), LengthPct::pct(50.0)),
            ..ElementTransform::default()
        };
        let mid = interpolate_transform(&from, &to, 0.5, Vec2::new(10.0, 10.0));
        assert_eq!(
            mid.functions,
            vec![TransformFn::Rotate(45.0), TransformFn::Scale(1.5, 1.5)]
        );
        assert_eq!(mid.translate, (LengthPct::px(5.0), LengthPct::pct(25.0)));
    }

    #[test]
    fn mismatched_lists_blend_through_the_decomposed_matrix() {
        let from = ElementTransform {
            functions: vec![TransformFn::Rotate(0.0)],
            origin: (LengthPct::px(0.0), LengthPct::px(0.0)),
            ..ElementTransform::default()
        };
        let to = ElementTransform {
            functions: vec![TransformFn::Translate(
                LengthPct::px(10.0),
                LengthPct::px(0.0),
            )],
            origin: (LengthPct::px(0.0), LengthPct::px(0.0)),
            ..ElementTransform::default()
        };
        let mid = interpolate_transform(&from, &to, 0.5, Vec2::ZERO);
        let m = mid.matrix(Vec2::ZERO, Vec2::ZERO);
        let p = m.apply(Vec2::ZERO);
        assert!(close(p.x, 5.0) && close(p.y, 0.0), "{p:?}");
        // A pure rotation decomposes and comes back as itself.
        let r = TransformFn::Rotate(30.0).matrix(Vec2::ZERO);
        let back = recompose(&decompose(&r));
        for (a, b) in r.coeffs.iter().zip(back.coeffs) {
            assert!((a - b).abs() < 1e-9);
        }
    }
}
