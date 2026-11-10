//! CSS transforms in the IR: `transform`, `translate`, `rotate`, `scale` and
//! `transform-origin`, parsed into the values an element carries and turned
//! into the runtime's [`ElementTransform`].

use lumen_core::transform::{ElementTransform, LengthPct, TransformFn};

use crate::layout_ir::{Attributes, ParseError};
use crate::values::bad;

/// A length that may mix pixels and a percentage of the element's own box.
#[derive(Debug, Clone, Copy, Default, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct LengthPctSpec {
    /// Pixels.
    pub px: f32,
    /// Percent of the reference size.
    pub pct: f32,
}

/// One function of a CSS `transform` list. Angles are in degrees.
#[derive(Debug, Clone, Copy, PartialEq, serde::Serialize, serde::Deserialize)]
pub enum TransformFnSpec {
    /// `translate()`, `translateX()`, `translateY()`.
    Translate(LengthPctSpec, LengthPctSpec),
    /// `scale()`, `scaleX()`, `scaleY()`.
    Scale(f32, f32),
    /// `rotate()`.
    Rotate(f32),
    /// `skew()`, `skewX()`, `skewY()`.
    Skew(f32, f32),
    /// `matrix(a, b, c, d, e, f)`.
    Matrix([f32; 6]),
}

impl From<LengthPctSpec> for LengthPct {
    fn from(l: LengthPctSpec) -> Self {
        LengthPct {
            px: l.px,
            pct: l.pct,
        }
    }
}

impl From<TransformFnSpec> for TransformFn {
    fn from(f: TransformFnSpec) -> Self {
        match f {
            TransformFnSpec::Translate(x, y) => TransformFn::Translate(x.into(), y.into()),
            TransformFnSpec::Scale(x, y) => TransformFn::Scale(x, y),
            TransformFnSpec::Rotate(a) => TransformFn::Rotate(a),
            TransformFnSpec::Skew(x, y) => TransformFn::Skew(x, y),
            TransformFnSpec::Matrix(m) => TransformFn::Matrix(m),
        }
    }
}

impl Attributes {
    /// The element's transform, or `None` when it declares none that moves
    /// anything.
    pub fn element_transform(&self) -> Option<ElementTransform> {
        if self.transform.is_none()
            && self.translate.is_none()
            && self.rotate.is_none()
            && self.scale.is_none()
        {
            return None;
        }
        let defaults = ElementTransform::default();
        let t = ElementTransform {
            functions: self
                .transform
                .iter()
                .flatten()
                .copied()
                .map(Into::into)
                .collect(),
            translate: self
                .translate
                .map(|(x, y)| (x.into(), y.into()))
                .unwrap_or(defaults.translate),
            rotate: self.rotate.unwrap_or(0.0),
            scale: self.scale.unwrap_or((1.0, 1.0)),
            origin: self
                .transform_origin
                .map(|(x, y)| (x.into(), y.into()))
                .unwrap_or(defaults.origin),
        };
        (!t.is_identity()).then_some(t)
    }
}

/// `<length-percentage>`: `12px`, `12` (pixels), `50%`, or `0`.
pub fn parse_length_pct(ctx: &str, name: &str, value: &str) -> Result<LengthPctSpec, ParseError> {
    let v = value.trim();
    let num = |s: &str| -> Result<f32, ParseError> {
        s.trim()
            .parse::<f32>()
            .map_err(|e| bad(ctx, name, value, format!("bad length '{v}': {e}")))
    };
    if let Some(rest) = v.strip_suffix('%') {
        return Ok(LengthPctSpec {
            px: 0.0,
            pct: num(rest)?,
        });
    }
    let rest = v.strip_suffix("px").unwrap_or(v);
    Ok(LengthPctSpec {
        px: num(rest)?,
        pct: 0.0,
    })
}

/// `<angle>` in degrees: `45deg`, `0.5turn`, `1.2rad`, `100grad`, or a bare
/// `0`.
pub fn parse_angle(ctx: &str, name: &str, value: &str) -> Result<f32, ParseError> {
    let v = value.trim();
    let num = |s: &str| -> Result<f32, ParseError> {
        s.trim()
            .parse::<f32>()
            .map_err(|e| bad(ctx, name, value, format!("bad angle '{v}': {e}")))
    };
    if let Some(n) = v.strip_suffix("deg") {
        return num(n);
    }
    if let Some(n) = v.strip_suffix("grad") {
        return Ok(num(n)? * 0.9);
    }
    if let Some(n) = v.strip_suffix("rad") {
        return Ok(num(n)?.to_degrees());
    }
    if let Some(n) = v.strip_suffix("turn") {
        return Ok(num(n)? * 360.0);
    }
    match num(v)? {
        0.0 => Ok(0.0),
        _ => Err(bad(
            ctx,
            name,
            value,
            "an angle needs a unit: deg, rad, grad or turn".into(),
        )),
    }
}

/// A scale factor: a number, or a percentage of one.
fn parse_factor(ctx: &str, name: &str, value: &str) -> Result<f32, ParseError> {
    let v = value.trim();
    let (s, scale) = match v.strip_suffix('%') {
        Some(n) => (n, 0.01),
        None => (v, 1.0),
    };
    s.trim()
        .parse::<f32>()
        .map(|n| n * scale)
        .map_err(|e| bad(ctx, name, value, format!("bad number '{v}': {e}")))
}

/// Split a function's arguments on commas, or on whitespace when it has
/// none.
fn args(inner: &str) -> Vec<&str> {
    if inner.contains(',') {
        inner.split(',').map(str::trim).collect()
    } else {
        inner.split_whitespace().collect()
    }
}

/// CSS `transform`: `none`, or a list of transform functions.
pub fn parse_transform(
    ctx: &str,
    name: &str,
    value: &str,
) -> Result<Vec<TransformFnSpec>, ParseError> {
    let v = value.trim();
    if v == "none" {
        return Ok(Vec::new());
    }
    let mut out = Vec::new();
    let mut rest = v;
    while !rest.is_empty() {
        let open = rest
            .find('(')
            .ok_or_else(|| bad(ctx, name, value, "expected a transform function".into()))?;
        let func = rest[..open].trim().to_ascii_lowercase();
        let close = rest[open..]
            .find(')')
            .map(|i| open + i)
            .ok_or_else(|| bad(ctx, name, value, format!("'{func}(' is not closed")))?;
        let a = args(&rest[open + 1..close]);
        rest = rest[close + 1..].trim_start();
        let arity = |n: std::ops::RangeInclusive<usize>| -> Result<(), ParseError> {
            if n.contains(&a.len()) {
                Ok(())
            } else {
                Err(bad(
                    ctx,
                    name,
                    value,
                    format!("{func}() takes {} to {} arguments", n.start(), n.end()),
                ))
            }
        };
        let len = |s: &str| parse_length_pct(ctx, name, s);
        let zero = LengthPctSpec::default();
        let f = match func.as_str() {
            "translate" => {
                arity(1..=2)?;
                TransformFnSpec::Translate(
                    len(a[0])?,
                    a.get(1).map(|s| len(s)).transpose()?.unwrap_or(zero),
                )
            }
            "translatex" => {
                arity(1..=1)?;
                TransformFnSpec::Translate(len(a[0])?, zero)
            }
            "translatey" => {
                arity(1..=1)?;
                TransformFnSpec::Translate(zero, len(a[0])?)
            }
            "scale" => {
                arity(1..=2)?;
                let x = parse_factor(ctx, name, a[0])?;
                let y = a.get(1).map(|s| parse_factor(ctx, name, s)).transpose()?;
                TransformFnSpec::Scale(x, y.unwrap_or(x))
            }
            "scalex" => {
                arity(1..=1)?;
                TransformFnSpec::Scale(parse_factor(ctx, name, a[0])?, 1.0)
            }
            "scaley" => {
                arity(1..=1)?;
                TransformFnSpec::Scale(1.0, parse_factor(ctx, name, a[0])?)
            }
            "rotate" | "rotatez" => {
                arity(1..=1)?;
                TransformFnSpec::Rotate(parse_angle(ctx, name, a[0])?)
            }
            "skew" => {
                arity(1..=2)?;
                let x = parse_angle(ctx, name, a[0])?;
                let y = a.get(1).map(|s| parse_angle(ctx, name, s)).transpose()?;
                TransformFnSpec::Skew(x, y.unwrap_or(0.0))
            }
            "skewx" => {
                arity(1..=1)?;
                TransformFnSpec::Skew(parse_angle(ctx, name, a[0])?, 0.0)
            }
            "skewy" => {
                arity(1..=1)?;
                TransformFnSpec::Skew(0.0, parse_angle(ctx, name, a[0])?)
            }
            "matrix" => {
                arity(6..=6)?;
                let mut m = [0.0f32; 6];
                for (slot, s) in m.iter_mut().zip(&a) {
                    *slot = parse_factor(ctx, name, s)?;
                }
                TransformFnSpec::Matrix(m)
            }
            f if f.ends_with("3d") || f.ends_with('z') || f == "perspective" => {
                return Err(bad(
                    ctx,
                    name,
                    value,
                    format!("{f}() is a 3D transform; only 2D transforms are supported"),
                ));
            }
            other => {
                return Err(bad(
                    ctx,
                    name,
                    value,
                    format!("unknown transform function '{other}()'"),
                ));
            }
        };
        out.push(f);
    }
    Ok(out)
}

/// CSS `translate`: `none`, or one or two lengths (`y` defaults to `0`).
pub fn parse_translate(
    ctx: &str,
    name: &str,
    value: &str,
) -> Result<(LengthPctSpec, LengthPctSpec), ParseError> {
    let parts: Vec<&str> = value.split_whitespace().collect();
    match parts.as_slice() {
        ["none"] => Ok(Default::default()),
        [x] => Ok((parse_length_pct(ctx, name, x)?, LengthPctSpec::default())),
        [x, y] => Ok((
            parse_length_pct(ctx, name, x)?,
            parse_length_pct(ctx, name, y)?,
        )),
        _ => Err(bad(
            ctx,
            name,
            value,
            "expected 'none' or one or two lengths (3D is not supported)".into(),
        )),
    }
}

/// CSS `rotate`: `none` or an angle.
pub fn parse_rotate(ctx: &str, name: &str, value: &str) -> Result<f32, ParseError> {
    match value.trim() {
        "none" => Ok(0.0),
        v if v.split_whitespace().count() == 1 => parse_angle(ctx, name, v),
        _ => Err(bad(
            ctx,
            name,
            value,
            "expected 'none' or an angle (rotation about an axis is 3D)".into(),
        )),
    }
}

/// CSS `scale`: `none`, or one or two factors (`y` defaults to `x`).
pub fn parse_scale(ctx: &str, name: &str, value: &str) -> Result<(f32, f32), ParseError> {
    let parts: Vec<&str> = value.split_whitespace().collect();
    match parts.as_slice() {
        ["none"] => Ok((1.0, 1.0)),
        [x] => {
            let x = parse_factor(ctx, name, x)?;
            Ok((x, x))
        }
        [x, y] => Ok((parse_factor(ctx, name, x)?, parse_factor(ctx, name, y)?)),
        _ => Err(bad(
            ctx,
            name,
            value,
            "expected 'none' or one or two numbers (3D is not supported)".into(),
        )),
    }
}

/// CSS `transform-origin`: one or two positions, keywords (`left`,
/// `center`, `bottom`, ...) or lengths.
pub fn parse_transform_origin(
    ctx: &str,
    name: &str,
    value: &str,
) -> Result<(LengthPctSpec, LengthPctSpec), ParseError> {
    #[derive(PartialEq)]
    enum Axis {
        X,
        Y,
        Either,
    }
    let pct = |p: f32| LengthPctSpec { px: 0.0, pct: p };
    let term = |v: &str| -> Result<(Axis, LengthPctSpec), ParseError> {
        Ok(match v {
            "left" => (Axis::X, pct(0.0)),
            "right" => (Axis::X, pct(100.0)),
            "top" => (Axis::Y, pct(0.0)),
            "bottom" => (Axis::Y, pct(100.0)),
            "center" => (Axis::Either, pct(50.0)),
            other => (Axis::Either, parse_length_pct(ctx, name, other)?),
        })
    };
    let parts: Vec<&str> = value.split_whitespace().collect();
    match parts.as_slice() {
        [one] => Ok(match term(one)? {
            (Axis::Y, y) => (pct(50.0), y),
            (_, x) => (x, pct(50.0)),
        }),
        [a, b] => {
            let (a, b) = (term(a)?, term(b)?);
            match (a, b) {
                ((Axis::Y, y), (ax, x)) if ax != Axis::Y => Ok((x, y)),
                ((ax, x), (ay, y)) if ax != Axis::Y && ay != Axis::X => Ok((x, y)),
                _ => Err(bad(
                    ctx,
                    name,
                    value,
                    "both values name the same axis".into(),
                )),
            }
        }
        _ => Err(bad(
            ctx,
            name,
            value,
            "expected one or two positions (a 3D origin is not supported)".into(),
        )),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn px(px: f32) -> LengthPctSpec {
        LengthPctSpec { px, pct: 0.0 }
    }

    #[test]
    fn parses_a_transform_list() {
        let list = parse_transform(
            "x",
            "transform",
            "translate(10px, 50%) rotate(0.25turn) scale(2) skewX(10deg)",
        )
        .unwrap();
        assert_eq!(
            list,
            vec![
                TransformFnSpec::Translate(px(10.0), LengthPctSpec { px: 0.0, pct: 50.0 }),
                TransformFnSpec::Rotate(90.0),
                TransformFnSpec::Scale(2.0, 2.0),
                TransformFnSpec::Skew(10.0, 0.0),
            ]
        );
        assert!(
            parse_transform("x", "transform", "none")
                .unwrap()
                .is_empty()
        );
        assert_eq!(
            parse_transform("x", "transform", "translateY(-4px)").unwrap(),
            vec![TransformFnSpec::Translate(px(0.0), px(-4.0))]
        );
    }

    #[test]
    fn refuses_3d_and_unknown_functions_and_unitless_angles() {
        assert!(parse_transform("x", "transform", "rotateX(10deg)").is_err());
        assert!(parse_transform("x", "transform", "translate3d(1px, 2px, 3px)").is_err());
        assert!(parse_transform("x", "transform", "wobble(1)").is_err());
        assert!(parse_transform("x", "transform", "rotate(45)").is_err());
        assert!(parse_transform("x", "transform", "rotate(0)").is_ok());
    }

    #[test]
    fn parses_the_individual_properties() {
        assert_eq!(
            parse_translate("x", "translate", "4px").unwrap(),
            (px(4.0), px(0.0))
        );
        assert_eq!(parse_scale("x", "scale", "1.5").unwrap(), (1.5, 1.5));
        assert_eq!(parse_scale("x", "scale", "50% 2").unwrap(), (0.5, 2.0));
        assert_eq!(parse_rotate("x", "rotate", "-15deg").unwrap(), -15.0);
        assert_eq!(parse_rotate("x", "rotate", "none").unwrap(), 0.0);
    }

    #[test]
    fn parses_transform_origin_keywords_in_either_order() {
        let pct = |p| LengthPctSpec { px: 0.0, pct: p };
        assert_eq!(
            parse_transform_origin("x", "transform-origin", "top left").unwrap(),
            (pct(0.0), pct(0.0))
        );
        assert_eq!(
            parse_transform_origin("x", "transform-origin", "right").unwrap(),
            (pct(100.0), pct(50.0))
        );
        assert_eq!(
            parse_transform_origin("x", "transform-origin", "10px bottom").unwrap(),
            (px(10.0), pct(100.0))
        );
        assert!(parse_transform_origin("x", "transform-origin", "top bottom").is_err());
    }

    #[test]
    fn attributes_resolve_to_an_element_transform() {
        let mut attrs = Attributes::default();
        assert!(attrs.element_transform().is_none());
        attrs.rotate = Some(0.0);
        assert!(
            attrs.element_transform().is_none(),
            "an identity moves nothing"
        );
        attrs.rotate = Some(30.0);
        attrs.transform_origin = Some((px(0.0), px(0.0)));
        let t = attrs.element_transform().unwrap();
        assert_eq!(t.rotate, 30.0);
        assert_eq!(t.origin, (LengthPct::px(0.0), LengthPct::px(0.0)));
    }
}
