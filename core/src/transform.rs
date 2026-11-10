//! CSS transforms: what `transform`, `translate`, `rotate`, `scale` and
//! `transform-origin` resolve to on an element, and the matrix they make.
//!
//! A transform moves what an element paints, and where the pointer finds
//! it, without moving its box: layout never reads an [`ElementTransform`].
//! The matrix is applied about the transform origin, a point in the
//! element's border box, in the order CSS Transforms 2 gives:
//! `translate`, then `rotate`, then `scale`, then the `transform` list.

use bevy_ecs::component::Component;
use glam::Vec2;

use crate::node_ir::Affine2;

/// A length that may mix pixels and a percentage of a reference size: CSS
/// `calc(<px> + <percent>)`. A `translate` percentage refers to the
/// element's own border box, as does a `transform-origin` one.
#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct LengthPct {
    /// The pixel part.
    pub px: f32,
    /// The percentage part, `50.0` for half the reference size.
    pub pct: f32,
}

impl LengthPct {
    /// A plain pixel length.
    pub const fn px(px: f32) -> Self {
        Self { px, pct: 0.0 }
    }

    /// A plain percentage.
    pub const fn pct(pct: f32) -> Self {
        Self { px: 0.0, pct }
    }

    /// The length in pixels against `reference`.
    pub fn resolve(self, reference: f32) -> f32 {
        self.px + self.pct * 0.01 * reference
    }

    /// The length a fraction `t` of the way from `self` to `other`. Pixels
    /// and percentages interpolate apart, the way CSS mixes them in a
    /// `calc()`.
    pub fn lerp(self, other: Self, t: f32) -> Self {
        Self {
            px: self.px + (other.px - self.px) * t,
            pct: self.pct + (other.pct - self.pct) * t,
        }
    }
}

/// One function of a CSS `transform` list. Angles are in degrees.
#[derive(Clone, Copy, Debug, PartialEq)]
pub enum TransformFn {
    /// `translate(x, y)`, `translateX`, `translateY`.
    Translate(LengthPct, LengthPct),
    /// `scale(x, y)`, `scaleX`, `scaleY`.
    Scale(f32, f32),
    /// `rotate(a)`: clockwise on screen for a positive angle.
    Rotate(f32),
    /// `skew(x, y)`, `skewX`, `skewY`.
    Skew(f32, f32),
    /// `matrix(a, b, c, d, e, f)`: `x' = a x + c y + e`, `y' = b x + d y + f`.
    Matrix([f32; 6]),
}

impl TransformFn {
    /// The function as a matrix, with percentages resolved against `size`.
    pub fn matrix(&self, size: Vec2) -> Affine2 {
        match *self {
            TransformFn::Translate(x, y) => {
                Affine2::translate(f64::from(x.resolve(size.x)), f64::from(y.resolve(size.y)))
            }
            TransformFn::Scale(x, y) => Affine2 {
                coeffs: [f64::from(x), 0.0, 0.0, f64::from(y), 0.0, 0.0],
            },
            TransformFn::Rotate(deg) => rotation(deg),
            TransformFn::Skew(x, y) => Affine2 {
                coeffs: [
                    1.0,
                    f64::from(y).to_radians().tan(),
                    f64::from(x).to_radians().tan(),
                    1.0,
                    0.0,
                    0.0,
                ],
            },
            TransformFn::Matrix(m) => Affine2 {
                coeffs: m.map(f64::from),
            },
        }
    }

    /// The identity function of the same kind, which a list pads with when
    /// it interpolates against a longer one.
    pub fn identity_like(&self) -> Self {
        match self {
            TransformFn::Translate(..) => {
                TransformFn::Translate(LengthPct::default(), LengthPct::default())
            }
            TransformFn::Scale(..) => TransformFn::Scale(1.0, 1.0),
            TransformFn::Rotate(_) => TransformFn::Rotate(0.0),
            TransformFn::Skew(..) => TransformFn::Skew(0.0, 0.0),
            TransformFn::Matrix(_) => TransformFn::Matrix([1.0, 0.0, 0.0, 1.0, 0.0, 0.0]),
        }
    }
}

fn rotation(deg: f32) -> Affine2 {
    let (sin, cos) = f64::from(deg).to_radians().sin_cos();
    Affine2 {
        coeffs: [cos, sin, -sin, cos, 0.0, 0.0],
    }
}

/// An element's CSS transform: the `transform` list, the three individual
/// transform properties, and the origin they apply about.
///
/// Carried only by an element that declares one of them; an element
/// without it is untransformed.
#[derive(Component, Clone, Debug, PartialEq)]
pub struct ElementTransform {
    /// The `transform` list, in source order. Empty is `none`.
    pub functions: Vec<TransformFn>,
    /// `translate`: `(0, 0)` is `none`.
    pub translate: (LengthPct, LengthPct),
    /// `rotate`, in degrees: `0` is `none`.
    pub rotate: f32,
    /// `scale`: `(1, 1)` is `none`.
    pub scale: (f32, f32),
    /// `transform-origin`, against the border box. `50% 50%` by default.
    pub origin: (LengthPct, LengthPct),
}

impl Default for ElementTransform {
    fn default() -> Self {
        Self {
            functions: Vec::new(),
            translate: (LengthPct::default(), LengthPct::default()),
            rotate: 0.0,
            scale: (1.0, 1.0),
            origin: (LengthPct::pct(50.0), LengthPct::pct(50.0)),
        }
    }
}

impl ElementTransform {
    /// Whether this moves nothing, whatever the box.
    pub fn is_identity(&self) -> bool {
        let zero = |l: LengthPct| l.px == 0.0 && l.pct == 0.0;
        zero(self.translate.0)
            && zero(self.translate.1)
            && self.rotate == 0.0
            && self.scale == (1.0, 1.0)
            && self.functions.iter().all(|f| match *f {
                TransformFn::Translate(x, y) => zero(x) && zero(y),
                TransformFn::Scale(x, y) => x == 1.0 && y == 1.0,
                TransformFn::Rotate(a) => a == 0.0,
                TransformFn::Skew(x, y) => x == 0.0 && y == 0.0,
                TransformFn::Matrix(m) => m == [1.0, 0.0, 0.0, 1.0, 0.0, 0.0],
            })
    }

    /// The matrix for a border box whose top-left corner sits at
    /// `top_left` and whose size is `size`, in the coordinates `top_left`
    /// is given in. It maps where the element would paint untransformed to
    /// where it paints.
    pub fn matrix(&self, top_left: Vec2, size: Vec2) -> Affine2 {
        let origin =
            top_left + Vec2::new(self.origin.0.resolve(size.x), self.origin.1.resolve(size.y));
        let (ox, oy) = (f64::from(origin.x), f64::from(origin.y));
        let mut m = Affine2::translate(ox, oy);
        m = m * Affine2::translate(
            f64::from(self.translate.0.resolve(size.x)),
            f64::from(self.translate.1.resolve(size.y)),
        );
        if self.rotate != 0.0 {
            m = m * rotation(self.rotate);
        }
        if self.scale != (1.0, 1.0) {
            m = m * TransformFn::Scale(self.scale.0, self.scale.1).matrix(size);
        }
        for f in &self.functions {
            m = m * f.matrix(size);
        }
        m * Affine2::translate(-ox, -oy)
    }
}

impl Affine2 {
    /// Where `p` lands under this transform.
    pub fn apply(&self, p: Vec2) -> Vec2 {
        let c = self.coeffs;
        let (x, y) = (f64::from(p.x), f64::from(p.y));
        Vec2::new(
            (c[0] * x + c[2] * y + c[4]) as f32,
            (c[1] * x + c[3] * y + c[5]) as f32,
        )
    }

    /// The transform that undoes this one, or `None` when it collapses the
    /// plane onto a line or a point (a zero scale), which nothing undoes.
    pub fn inverse(&self) -> Option<Affine2> {
        let [a, b, c, d, e, f] = self.coeffs;
        let det = a * d - b * c;
        if det.abs() < 1e-12 || !det.is_finite() {
            return None;
        }
        let inv = 1.0 / det;
        Some(Affine2 {
            coeffs: [
                d * inv,
                -b * inv,
                -c * inv,
                a * inv,
                (c * f - d * e) * inv,
                (b * e - a * f) * inv,
            ],
        })
    }

    /// The axis-aligned box that holds the rect at `origin` of `size` once
    /// transformed.
    pub fn bounds(&self, origin: Vec2, size: Vec2) -> (Vec2, Vec2) {
        let corners = [
            self.apply(origin),
            self.apply(origin + Vec2::new(size.x, 0.0)),
            self.apply(origin + Vec2::new(0.0, size.y)),
            self.apply(origin + size),
        ];
        let min = corners
            .iter()
            .fold(Vec2::splat(f32::INFINITY), |m, p| m.min(*p));
        let max = corners
            .iter()
            .fold(Vec2::splat(f32::NEG_INFINITY), |m, p| m.max(*p));
        (min, max - min)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn close(a: Vec2, b: Vec2) -> bool {
        (a - b).length() < 1e-3
    }

    #[test]
    fn rotate_turns_about_the_center_by_default() {
        let t = ElementTransform {
            rotate: 90.0,
            ..ElementTransform::default()
        };
        let m = t.matrix(Vec2::new(10.0, 10.0), Vec2::new(20.0, 10.0));
        // The center stays put; the top-left corner swings clockwise.
        assert!(close(m.apply(Vec2::new(20.0, 15.0)), Vec2::new(20.0, 15.0)));
        assert!(close(m.apply(Vec2::new(10.0, 10.0)), Vec2::new(25.0, 5.0)));
    }

    #[test]
    fn translate_percentages_refer_to_the_box() {
        let t = ElementTransform {
            translate: (LengthPct::pct(50.0), LengthPct::px(4.0)),
            ..ElementTransform::default()
        };
        let m = t.matrix(Vec2::ZERO, Vec2::new(40.0, 10.0));
        assert!(close(m.apply(Vec2::ZERO), Vec2::new(20.0, 4.0)));
    }

    #[test]
    fn individual_properties_apply_before_the_list() {
        // translate, then scale, then the list's translate: the list's
        // offset is scaled, the individual one is not.
        let t = ElementTransform {
            translate: (LengthPct::px(10.0), LengthPct::default()),
            scale: (2.0, 2.0),
            functions: vec![TransformFn::Translate(
                LengthPct::px(5.0),
                LengthPct::default(),
            )],
            origin: (LengthPct::default(), LengthPct::default()),
            ..ElementTransform::default()
        };
        let m = t.matrix(Vec2::ZERO, Vec2::new(10.0, 10.0));
        assert!(close(m.apply(Vec2::ZERO), Vec2::new(20.0, 0.0)));
    }

    #[test]
    fn inverse_undoes_and_a_zero_scale_has_none() {
        let t = ElementTransform {
            rotate: 30.0,
            scale: (2.0, 0.5),
            ..ElementTransform::default()
        };
        let m = t.matrix(Vec2::new(3.0, 4.0), Vec2::new(20.0, 10.0));
        let p = Vec2::new(7.0, -2.0);
        assert!(close(m.inverse().unwrap().apply(m.apply(p)), p));
        let flat = ElementTransform {
            scale: (0.0, 1.0),
            ..ElementTransform::default()
        };
        assert!(flat.matrix(Vec2::ZERO, Vec2::ONE).inverse().is_none());
    }

    #[test]
    fn identity_is_recognised() {
        assert!(ElementTransform::default().is_identity());
        let t = ElementTransform {
            functions: vec![TransformFn::Rotate(0.0), TransformFn::Scale(1.0, 1.0)],
            ..ElementTransform::default()
        };
        assert!(t.is_identity());
        assert!(
            !ElementTransform {
                rotate: 1.0,
                ..ElementTransform::default()
            }
            .is_identity()
        );
    }
}
