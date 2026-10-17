//! Shared value parsers used by both the HTML and CSS parsers.
//!
//! Each returns `Result<T, ParseError>`; on failure `ParseError::BadAttribute`
//! carries the calling context (tag-name in HTML, selector text in CSS),
//! the property name, and the raw value, so error messages are useful
//! either side of the parser boundary.

use crate::layout_ir::{Edges, LengthSpec, ParseError, Rgba};

/// Length: `auto` | `<n>` | `<n>px` | `<n>%`.
pub fn parse_length(ctx: &str, name: &str, value: &str) -> Result<LengthSpec, ParseError> {
    let v = value.trim();
    if v == "auto" {
        return Ok(LengthSpec::Auto);
    }
    if let Some(rest) = v.strip_suffix('%') {
        return Ok(LengthSpec::Percent(parse_num(ctx, name, value, rest)?));
    }
    if let Some(rest) = v.strip_suffix("px") {
        return Ok(LengthSpec::Px(parse_num(ctx, name, value, rest)?));
    }
    Ok(LengthSpec::Px(parse_num(ctx, name, value, v)?))
}

/// `#rrggbb` or `#rrggbbaa`.
pub fn parse_color(ctx: &str, name: &str, value: &str) -> Result<Rgba, ParseError> {
    let v = value.trim();
    let body = v
        .strip_prefix('#')
        .ok_or_else(|| bad(ctx, name, value, "expected '#rrggbb' or '#rrggbbaa'".into()))?;
    if body.len() != 6 && body.len() != 8 {
        return Err(bad(
            ctx,
            name,
            value,
            "expected '#rrggbb' or '#rrggbbaa'".into(),
        ));
    }
    let pair = |i: usize| -> Result<u8, ParseError> {
        u8::from_str_radix(&body[i..i + 2], 16)
            .map_err(|e| bad(ctx, name, value, format!("bad hex: {e}")))
    };
    let r = pair(0)? as f32 / 255.0;
    let g = pair(2)? as f32 / 255.0;
    let b = pair(4)? as f32 / 255.0;
    let a = if body.len() == 8 {
        pair(6)? as f32 / 255.0
    } else {
        1.0
    };
    Ok(Rgba { r, g, b, a })
}

/// Parse a `bg=` value: a hex color, a gradient function, or an image named
/// with `url(...)`. Gradient stops accept the CSS forms `<color>`
/// (auto-distributed) or `<color> <offset%>` (explicit). Parsed stops are
/// sorted by offset ascending so the renderer can hand them straight to
/// peniko.
pub fn parse_bg(
    ctx: &str,
    name: &str,
    value: &str,
) -> Result<crate::layout_ir::BgSpec, ParseError> {
    use crate::layout_ir::BgSpec;
    let trimmed = value.trim();
    if let Some(path) = parse_url(ctx, name, value)? {
        return Ok(BgSpec::Image(path));
    }
    if let Some(inner) = trimmed
        .strip_prefix("linear-gradient(")
        .and_then(|s| s.strip_suffix(')'))
    {
        return parse_linear_gradient(ctx, name, value, inner).map(|(angle, stops)| {
            BgSpec::Linear {
                angle_deg: angle,
                stops,
            }
        });
    }
    if let Some(inner) = trimmed
        .strip_prefix("radial-gradient(")
        .and_then(|s| s.strip_suffix(')'))
    {
        return parse_radial_gradient(ctx, name, value, inner)
            .map(|(radius, stops)| BgSpec::Radial { radius, stops });
    }
    if let Some(inner) = trimmed
        .strip_prefix("conic-gradient(")
        .and_then(|s| s.strip_suffix(')'))
    {
        return parse_conic_gradient(ctx, name, value, inner)
            .map(|(from_deg, stops)| BgSpec::Conic { from_deg, stops });
    }
    Ok(BgSpec::Solid(parse_color(ctx, name, value)?))
}

/// The path inside a CSS `url(...)`, or `None` when `value` is not a
/// `url()` at all. The path may be double-quoted, single-quoted, or bare;
/// an empty one is an error.
pub fn parse_url(ctx: &str, name: &str, value: &str) -> Result<Option<String>, ParseError> {
    let trimmed = value.trim();
    let Some(inner) = trimmed
        .get(..4)
        .filter(|head| head.eq_ignore_ascii_case("url("))
        .and_then(|_| trimmed[4..].strip_suffix(')'))
    else {
        return Ok(None);
    };
    let inner = inner.trim();
    let path = ['"', '\'']
        .iter()
        .find_map(|q| {
            inner
                .strip_prefix(*q)
                .and_then(|rest| rest.strip_suffix(*q))
        })
        .unwrap_or(inner);
    if path.is_empty() {
        return Err(bad(ctx, name, value, "url() names no file".into()));
    }
    Ok(Some(path.to_string()))
}

/// An image fit: `fill`, `cover`, `contain`, `none`, or `scale-down`. The
/// `fit` of an `<image>` and the `bg-fit` of a background image take the same
/// values.
pub fn parse_image_fit(
    ctx: &str,
    name: &str,
    value: &str,
) -> Result<crate::layout_ir::ImageFitSpec, ParseError> {
    use crate::layout_ir::ImageFitSpec;
    Ok(match value.trim() {
        "fill" => ImageFitSpec::Fill,
        "cover" => ImageFitSpec::Cover,
        "contain" => ImageFitSpec::Contain,
        "none" => ImageFitSpec::None,
        "scale-down" => ImageFitSpec::ScaleDown,
        other => {
            return Err(bad(
                ctx,
                name,
                value,
                format!(
                    "unknown fit '{other}' (supported: fill, cover, contain, none, scale-down)"
                ),
            ));
        }
    })
}

/// Split `value` at the commas that are not inside parentheses or quotes.
fn split_top_level_commas(value: &str) -> Vec<&str> {
    let mut parts = Vec::new();
    let (mut depth, mut quote, mut start) = (0usize, None::<char>, 0usize);
    for (i, c) in value.char_indices() {
        match (quote, c) {
            (Some(q), c) if c == q => quote = None,
            (Some(_), _) => {}
            (None, '"' | '\'') => quote = Some(c),
            (None, '(') => depth += 1,
            (None, ')') => depth = depth.saturating_sub(1),
            (None, ',') if depth == 0 => {
                parts.push(&value[start..i]);
                start = i + 1;
            }
            _ => {}
        }
    }
    parts.push(&value[start..]);
    parts
}

/// CSS `cursor`: any number of `url(<image>) [<x> <y>]` entries, each
/// followed by a comma, then the keyword shown when no image loads.
pub fn parse_cursor(
    ctx: &str,
    name: &str,
    value: &str,
) -> Result<crate::layout_ir::CursorSpec, ParseError> {
    use crate::layout_ir::{CursorImageSpec, CursorSpec};
    use lumen_core::input::CursorKeyword;
    let parts = split_top_level_commas(value);
    let (last, images) = parts.split_last().expect("split yields one part at least");
    let keyword = CursorKeyword::from_css(last).ok_or_else(|| {
        let message = if last.trim_start().to_ascii_lowercase().starts_with("url(") {
            "a cursor image needs a keyword after it to fall back on, as in \
             `url(pen.png) 2 14, crosshair`"
                .to_string()
        } else {
            format!(
                "unknown cursor '{}' (supported: {})",
                last.trim(),
                CursorKeyword::ALL
                    .iter()
                    .map(|k| k.as_css())
                    .collect::<Vec<_>>()
                    .join(", ")
            )
        };
        bad(ctx, name, value, message)
    })?;
    let images = images
        .iter()
        .map(|part| {
            let part = part.trim();
            let close = part
                .find(')')
                .filter(|_| {
                    part.get(..4)
                        .is_some_and(|h| h.eq_ignore_ascii_case("url("))
                })
                .ok_or_else(|| {
                    bad(
                        ctx,
                        name,
                        value,
                        format!("'{part}' is not a url() image; only the last entry is a keyword"),
                    )
                })?;
            let url = parse_url(ctx, name, &part[..=close])?.expect("the entry starts with url(");
            let numbers: Vec<&str> = part[close + 1..].split_whitespace().collect();
            let hotspot = match numbers.as_slice() {
                [] => None,
                [x, y] => {
                    let number = |n: &str| {
                        n.parse::<f32>()
                            .ok()
                            .filter(|v| v.is_finite())
                            .ok_or_else(|| {
                                bad(ctx, name, value, format!("'{n}' is not a hotspot number"))
                            })
                    };
                    Some((number(x)?, number(y)?))
                }
                _ => {
                    return Err(bad(
                        ctx,
                        name,
                        value,
                        "a cursor hotspot is two numbers, x then y".into(),
                    ));
                }
            };
            Ok(CursorImageSpec { url, hotspot })
        })
        .collect::<Result<Vec<_>, ParseError>>()?;
    Ok(CursorSpec {
        images,
        keyword: keyword.as_css().to_string(),
    })
}

/// CSS `mask-image`: `none` or one `url(...)`.
pub fn parse_mask_image(
    ctx: &str,
    name: &str,
    value: &str,
) -> Result<crate::layout_ir::MaskImageSpec, ParseError> {
    use crate::layout_ir::MaskImageSpec;
    if value.trim() == "none" {
        return Ok(MaskImageSpec::None);
    }
    match parse_url(ctx, name, value)? {
        Some(path) => Ok(MaskImageSpec::Url(path)),
        None => Err(bad(
            ctx,
            name,
            value,
            "expected 'none' or one 'url(...)' (gradients and several layers are not \
             supported)"
                .into(),
        )),
    }
}

/// CSS `mask-size`: `contain`, `cover`, or one or two of a length, a
/// percentage, or `auto` (one value sets the width, and the height is
/// `auto`).
pub fn parse_mask_size(
    ctx: &str,
    name: &str,
    value: &str,
) -> Result<crate::layout_ir::MaskSizeSpec, ParseError> {
    use crate::layout_ir::MaskSizeSpec;
    let parts: Vec<&str> = value.split_whitespace().collect();
    match parts.as_slice() {
        ["contain"] => Ok(MaskSizeSpec::Contain),
        ["cover"] => Ok(MaskSizeSpec::Cover),
        [w] => Ok(MaskSizeSpec::Size(
            parse_length(ctx, name, w)?,
            LengthSpec::Auto,
        )),
        [w, h] => Ok(MaskSizeSpec::Size(
            parse_length(ctx, name, w)?,
            parse_length(ctx, name, h)?,
        )),
        _ => Err(bad(
            ctx,
            name,
            value,
            "expected 'contain', 'cover', or one or two lengths".into(),
        )),
    }
}

/// CSS `mask-repeat`: `repeat`, `no-repeat`, `repeat-x`, `repeat-y`, or two
/// of `repeat` / `no-repeat`, one per axis.
pub fn parse_mask_repeat(
    ctx: &str,
    name: &str,
    value: &str,
) -> Result<crate::layout_ir::MaskRepeatSpec, ParseError> {
    use crate::layout_ir::MaskRepeatSpec;
    let axis = |v: &str| match v {
        "repeat" => Ok(true),
        "no-repeat" => Ok(false),
        other => Err(bad(
            ctx,
            name,
            value,
            format!(
                "unknown mask-repeat '{other}' (supported: repeat, no-repeat, repeat-x, repeat-y)"
            ),
        )),
    };
    let parts: Vec<&str> = value.split_whitespace().collect();
    match parts.as_slice() {
        ["repeat-x"] => Ok(MaskRepeatSpec { x: true, y: false }),
        ["repeat-y"] => Ok(MaskRepeatSpec { x: false, y: true }),
        [both] => {
            let r = axis(both)?;
            Ok(MaskRepeatSpec { x: r, y: r })
        }
        [x, y] => Ok(MaskRepeatSpec {
            x: axis(x)?,
            y: axis(y)?,
        }),
        _ => Err(bad(
            ctx,
            name,
            value,
            "expected one or two of repeat, no-repeat".into(),
        )),
    }
}

/// CSS `mask-position`: one or two of `left`, `center`, `right`, `top`,
/// `bottom`, a length, or a percentage. One value sets the axis its keyword
/// names (the horizontal one for a length) and centers the other.
pub fn parse_mask_position(
    ctx: &str,
    name: &str,
    value: &str,
) -> Result<crate::layout_ir::MaskPositionSpec, ParseError> {
    use crate::layout_ir::MaskPositionSpec;
    enum Axis {
        X,
        Y,
        Either,
    }
    let term = |v: &str| -> Result<(Axis, LengthSpec), ParseError> {
        Ok(match v {
            "left" => (Axis::X, LengthSpec::Percent(0.0)),
            "right" => (Axis::X, LengthSpec::Percent(100.0)),
            "top" => (Axis::Y, LengthSpec::Percent(0.0)),
            "bottom" => (Axis::Y, LengthSpec::Percent(100.0)),
            "center" => (Axis::Either, LengthSpec::Percent(50.0)),
            other => match parse_length(ctx, name, other)? {
                LengthSpec::Auto => {
                    return Err(bad(ctx, name, value, "'auto' is not a position".into()));
                }
                len => (Axis::Either, len),
            },
        })
    };
    let center = LengthSpec::Percent(50.0);
    let parts: Vec<&str> = value.split_whitespace().collect();
    match parts.as_slice() {
        [one] => Ok(match term(one)? {
            (Axis::Y, y) => MaskPositionSpec { x: center, y },
            (_, x) => MaskPositionSpec { x, y: center },
        }),
        [a, b] => match (term(a)?, term(b)?) {
            ((Axis::Y, y), (Axis::X | Axis::Either, x))
            | ((Axis::Either, x), (Axis::Y, y))
            | ((Axis::X, x), (Axis::Y | Axis::Either, y))
            | ((Axis::Either, x), (Axis::Either, y)) => Ok(MaskPositionSpec { x, y }),
            ((Axis::Either, y), (Axis::X, x)) => Ok(MaskPositionSpec { x, y }),
            _ => Err(bad(
                ctx,
                name,
                value,
                "both values name the same axis".into(),
            )),
        },
        _ => Err(bad(
            ctx,
            name,
            value,
            "expected one or two positions (edge offsets such as 'right 4px top 2px' are not \
             supported)"
                .into(),
        )),
    }
}

/// CSS `image-rendering`: `auto`, `smooth`, `pixelated`, or `crisp-edges`.
pub fn parse_image_rendering(
    ctx: &str,
    name: &str,
    value: &str,
) -> Result<crate::layout_ir::ImageRenderingSpec, ParseError> {
    use crate::layout_ir::ImageRenderingSpec;
    Ok(match value.trim() {
        "auto" => ImageRenderingSpec::Auto,
        "smooth" => ImageRenderingSpec::Smooth,
        "pixelated" => ImageRenderingSpec::Pixelated,
        "crisp-edges" => ImageRenderingSpec::CrispEdges,
        other => {
            return Err(bad(
                ctx,
                name,
                value,
                format!(
                    "unknown image-rendering '{other}' (supported: auto, smooth, pixelated, \
                     crisp-edges)"
                ),
            ));
        }
    })
}

/// Parses `radial-gradient(<color1>, <color2 [stop%]>, ...)`. Every term is a colour stop; the radius is always 1.0.
/// The centre is fixed at 50% / 50%; explicit position and ellipse shapes are not supported.
fn parse_radial_gradient(
    ctx: &str,
    name: &str,
    full_value: &str,
    inner: &str,
) -> Result<(f32, Vec<(f32, crate::layout_ir::Rgba)>), ParseError> {
    let parts: Vec<&str> = inner
        .split(',')
        .map(|p| p.trim())
        .filter(|p| !p.is_empty())
        .collect();
    if parts.len() < 2 {
        return Err(bad(
            ctx,
            name,
            full_value,
            "radial-gradient needs at least two color stops".into(),
        ));
    }
    let n = parts.len();
    let mut stops: Vec<(f32, crate::layout_ir::Rgba)> = Vec::with_capacity(n);
    for (i, raw) in parts.iter().enumerate() {
        let mut bits = raw.split_whitespace();
        let color_str = bits
            .next()
            .ok_or_else(|| bad(ctx, name, full_value, format!("empty stop #{}", i + 1)))?;
        let color = parse_color(ctx, name, color_str)?;
        let offset = match bits.next() {
            Some(s) => parse_offset(ctx, name, full_value, s)?,
            None => i as f32 / (n - 1).max(1) as f32,
        };
        stops.push((offset, color));
    }
    stops.sort_by(|a, b| a.0.partial_cmp(&b.0).unwrap_or(std::cmp::Ordering::Equal));
    Ok((1.0, stops))
}

/// `conic-gradient([from <angle>], <color1>, <color2>, ...)`.
fn parse_conic_gradient(
    ctx: &str,
    name: &str,
    full_value: &str,
    inner: &str,
) -> Result<(f32, Vec<(f32, crate::layout_ir::Rgba)>), ParseError> {
    let parts: Vec<&str> = inner
        .split(',')
        .map(|p| p.trim())
        .filter(|p| !p.is_empty())
        .collect();
    if parts.is_empty() {
        return Err(bad(
            ctx,
            name,
            full_value,
            "conic-gradient needs at least one stop".into(),
        ));
    }
    let (from_deg, stop_parts): (f32, &[&str]) = if let Some(rest) = parts[0].strip_prefix("from ")
    {
        let num = rest.trim().strip_suffix("deg").unwrap_or(rest.trim());
        let v: f32 = num.trim().parse().map_err(|e| {
            bad(
                ctx,
                name,
                full_value,
                format!("bad angle '{}': {e}", parts[0]),
            )
        })?;
        (v, &parts[1..])
    } else {
        (0.0, &parts[..])
    };
    if stop_parts.len() < 2 {
        return Err(bad(
            ctx,
            name,
            full_value,
            "conic-gradient needs at least two color stops".into(),
        ));
    }
    let n = stop_parts.len();
    let mut stops: Vec<(f32, crate::layout_ir::Rgba)> = Vec::with_capacity(n);
    for (i, raw) in stop_parts.iter().enumerate() {
        let mut bits = raw.split_whitespace();
        let color_str = bits
            .next()
            .ok_or_else(|| bad(ctx, name, full_value, format!("empty stop #{}", i + 1)))?;
        let color = parse_color(ctx, name, color_str)?;
        let offset = match bits.next() {
            Some(s) => parse_offset(ctx, name, full_value, s)?,
            None => i as f32 / (n - 1).max(1) as f32,
        };
        stops.push((offset, color));
    }
    stops.sort_by(|a, b| a.0.partial_cmp(&b.0).unwrap_or(std::cmp::Ordering::Equal));
    Ok((from_deg, stops))
}

fn parse_linear_gradient(
    ctx: &str,
    name: &str,
    full_value: &str,
    inner: &str,
) -> Result<(f32, Vec<(f32, crate::layout_ir::Rgba)>), ParseError> {
    let parts: Vec<&str> = inner
        .split(',')
        .map(|p| p.trim())
        .filter(|p| !p.is_empty())
        .collect();
    if parts.len() < 2 {
        return Err(bad(
            ctx,
            name,
            full_value,
            "linear-gradient needs at least one direction and one stop".into(),
        ));
    }
    let (angle_deg, stop_parts): (f32, &[&str]) = match parts[0].strip_suffix("deg") {
        Some(num) => (
            num.trim().parse::<f32>().map_err(|e| {
                bad(
                    ctx,
                    name,
                    full_value,
                    format!("bad angle '{}': {e}", parts[0]),
                )
            })?,
            &parts[1..],
        ),
        None => (180.0, &parts[..]), // default top-to-bottom
    };
    if stop_parts.len() < 2 {
        return Err(bad(
            ctx,
            name,
            full_value,
            "linear-gradient needs at least two color stops".into(),
        ));
    }
    let n = stop_parts.len();
    let mut stops: Vec<(f32, crate::layout_ir::Rgba)> = Vec::with_capacity(n);
    for (i, raw) in stop_parts.iter().enumerate() {
        let mut bits = raw.split_whitespace();
        let color_str = bits
            .next()
            .ok_or_else(|| bad(ctx, name, full_value, format!("empty stop #{}", i + 1)))?;
        let color = parse_color(ctx, name, color_str)?;
        let offset = match bits.next() {
            Some(s) => parse_offset(ctx, name, full_value, s)?,
            None => i as f32 / (n - 1).max(1) as f32,
        };
        stops.push((offset, color));
    }
    stops.sort_by(|a, b| a.0.partial_cmp(&b.0).unwrap_or(std::cmp::Ordering::Equal));
    Ok((angle_deg, stops))
}

fn parse_offset(ctx: &str, name: &str, full_value: &str, raw: &str) -> Result<f32, ParseError> {
    if let Some(num) = raw.strip_suffix('%') {
        let n: f32 = num
            .trim()
            .parse()
            .map_err(|e: std::num::ParseFloatError| bad(ctx, name, full_value, e.to_string()))?;
        Ok(n / 100.0)
    } else {
        raw.trim().parse::<f32>().map_err(|e| {
            bad(
                ctx,
                name,
                full_value,
                format!("bad stop offset '{raw}': {e}"),
            )
        })
    }
}

/// One edge term: `<n>` / `<n>px` (px) or `<n>%` (percent). Returned as
/// `(px, pct)` - exactly one is meaningful: `pct = Some` => px is `0.0`
/// and ignored downstream.
fn parse_edge_term(
    ctx: &str,
    name: &str,
    value: &str,
    s: &str,
) -> Result<(f32, Option<f32>), ParseError> {
    let s = s.trim();
    if let Some(rest) = s.strip_suffix('%') {
        let p: f32 = rest
            .trim()
            .parse()
            .map_err(|e: std::num::ParseFloatError| {
                bad(ctx, name, value, format!("bad percent '{s}': {e}"))
            })?;
        return Ok((0.0, Some(p)));
    }
    Ok((parse_num(ctx, name, value, s)?, None))
}

/// Parse a CSS-style spacing shorthand, top-right-bottom-left.
///
/// * 1 value `n` -> every side `n`.
/// * 2 values `v h` -> top/bottom = `v`, left/right = `h`.
/// * 3 values `t h b` -> top, left/right, bottom.
/// * 4 values `t r b l` -> explicit per-side.
///
/// Each term is `<n>` / `<n>px` / `<n>%` - percent terms land in the
/// `pct_*` fields and resolve per CSS at layout time.
pub fn parse_edges(ctx: &str, name: &str, value: &str) -> Result<Edges, ParseError> {
    edges_with(
        value,
        |s| parse_edge_term(ctx, name, value, s),
        |other| {
            bad(
                ctx,
                name,
                value,
                format!("expected 1, 2, 3, or 4 numbers, got {other}"),
            )
        },
    )
}

/// [`parse_edges`] for `inset`, where a term may also be `auto` (stored as
/// `NaN`, the sentinel the layout backend reads as `auto`).
pub fn parse_inset_edges(ctx: &str, name: &str, value: &str) -> Result<Edges, ParseError> {
    edges_with(
        value,
        |s| {
            if s.trim() == "auto" {
                Ok((f32::NAN, None))
            } else {
                parse_edge_term(ctx, name, value, s)
            }
        },
        |other| {
            bad(
                ctx,
                name,
                value,
                format!("expected 1, 2, 3, or 4 numbers or `auto`, got {other}"),
            )
        },
    )
}

/// One `inset` longhand term: a px number, or `auto` (`NaN`).
pub fn parse_inset_term(ctx: &str, name: &str, value: &str) -> Result<f32, ParseError> {
    if value.trim() == "auto" {
        return Ok(f32::NAN);
    }
    parse_num(ctx, name, value, value.trim())
}

/// The top-right-bottom-left rotation shared by [`parse_edges`] and
/// [`parse_inset_edges`]; `term` parses one side.
fn edges_with(
    value: &str,
    term: impl Fn(&str) -> Result<(f32, Option<f32>), ParseError>,
    arity: impl Fn(usize) -> ParseError,
) -> Result<Edges, ParseError> {
    let parts: Vec<&str> = value.split_whitespace().collect();
    let n = |s: &str| term(s);
    let (t, r, b, l) = match parts.len() {
        1 => {
            let v = n(parts[0])?;
            (v, v, v, v)
        }
        2 => {
            let v = n(parts[0])?;
            let h = n(parts[1])?;
            (v, h, v, h)
        }
        3 => {
            let t = n(parts[0])?;
            let h = n(parts[1])?;
            let b = n(parts[2])?;
            (t, h, b, h)
        }
        4 => (n(parts[0])?, n(parts[1])?, n(parts[2])?, n(parts[3])?),
        other => return Err(arity(other)),
    };
    Ok(Edges {
        top: t.0,
        right: r.0,
        bottom: b.0,
        left: l.0,
        pct_top: t.1,
        pct_right: r.1,
        pct_bottom: b.1,
        pct_left: l.1,
        ..Edges::default()
    })
}

/// CSS border-width keyword / length term: `thin` (1px), `medium`
/// (3px), `thick` (5px), or a px length. Percent is not valid CSS for
/// border widths and is rejected.
pub fn parse_border_width_term(
    ctx: &str,
    name: &str,
    value: &str,
    s: &str,
) -> Result<f32, ParseError> {
    match s.trim() {
        "thin" => Ok(1.0),
        "medium" => Ok(3.0),
        "thick" => Ok(5.0),
        t if t.ends_with('%') => Err(bad(
            ctx,
            name,
            value,
            "border widths cannot be percentages".into(),
        )),
        t => parse_num(ctx, name, value, t),
    }
}

/// CSS `border-width` shorthand - 1-4 keyword/length terms in the
/// standard top-right-bottom-left rotation.
pub fn parse_border_width_edges(ctx: &str, name: &str, value: &str) -> Result<Edges, ParseError> {
    let parts: Vec<&str> = value.split_whitespace().collect();
    let n = |s: &str| parse_border_width_term(ctx, name, value, s);
    match parts.len() {
        1 => Ok(Edges::all(n(parts[0])?)),
        2 => {
            let v = n(parts[0])?;
            let h = n(parts[1])?;
            Ok(Edges {
                top: v,
                right: h,
                bottom: v,
                left: h,
                ..Edges::default()
            })
        }
        3 => {
            let t = n(parts[0])?;
            let h = n(parts[1])?;
            let b = n(parts[2])?;
            Ok(Edges {
                top: t,
                right: h,
                bottom: b,
                left: h,
                ..Edges::default()
            })
        }
        4 => Ok(Edges {
            top: n(parts[0])?,
            right: n(parts[1])?,
            bottom: n(parts[2])?,
            left: n(parts[3])?,
            ..Edges::default()
        }),
        other => Err(bad(
            ctx,
            name,
            value,
            format!("expected 1, 2, 3, or 4 border widths, got {other}"),
        )),
    }
}

/// `border-style` keyword: `none` | `solid` (v1 subset). Other CSS
/// styles (`dashed`, `dotted`, ...) are rejected so the declaration is
/// skipped with a warning instead of silently misrendering.
pub fn parse_border_style(
    ctx: &str,
    name: &str,
    value: &str,
) -> Result<crate::layout_ir::BorderStyleSpec, ParseError> {
    use crate::layout_ir::BorderStyleSpec;
    match value.trim() {
        "none" | "hidden" => Ok(BorderStyleSpec::None),
        "solid" => Ok(BorderStyleSpec::Solid),
        other => Err(bad(
            ctx,
            name,
            value,
            format!("unsupported border-style '{other}' (supported: none, solid)"),
        )),
    }
}

/// Parsed result of a `border:`-family shorthand.
pub struct BorderShorthand {
    /// Width term, uniform on all sides. `None` = keep/default.
    pub width: Option<f32>,
    /// Style keyword. `None` = not authored in the shorthand.
    pub style: Option<crate::layout_ir::BorderStyleSpec>,
    /// Color term.
    pub color: Option<crate::layout_ir::Rgba>,
}

/// CSS `border` shorthand: `<width> || <style> || <color>` in any order
/// (e.g. `1px solid #444`). `border: none` clears the border.
///
/// Lenient extension: when the style keyword is omitted but a width or
/// color is present (`border: 1px #444`), the style resolves to `solid`;
/// the IR stores the normalized form, so a transpile target emits the
/// explicit `1px solid #444` and renders identically in a real browser.
pub fn parse_border_shorthand(
    ctx: &str,
    name: &str,
    value: &str,
) -> Result<BorderShorthand, ParseError> {
    use crate::layout_ir::BorderStyleSpec;
    let mut out = BorderShorthand {
        width: None,
        style: None,
        color: None,
    };
    for tok in value.split_whitespace() {
        match tok {
            "none" | "hidden" => out.style = Some(BorderStyleSpec::None),
            "solid" => out.style = Some(BorderStyleSpec::Solid),
            "dashed" | "dotted" | "double" | "groove" | "ridge" | "inset" | "outset" => {
                return Err(bad(
                    ctx,
                    name,
                    value,
                    format!("unsupported border-style '{tok}' (supported: none, solid)"),
                ));
            }
            t if t.starts_with('#') => out.color = Some(parse_color(ctx, name, t)?),
            t => out.width = Some(parse_border_width_term(ctx, name, value, t)?),
        }
    }
    if out.style.is_none() && (out.width.is_some() || out.color.is_some()) {
        out.style = Some(BorderStyleSpec::Solid);
    }
    if out.style.is_none() {
        return Err(bad(
            ctx,
            name,
            value,
            "expected '<width> [solid|none] <#color>' (any order) or 'none'".into(),
        ));
    }
    Ok(out)
}

/// CSS `font-weight`: `normal` (400), `bold` (700), or a number in
/// `1..=1000` (CSS Fonts 4). The relative keywords `lighter` / `bolder`
/// need the parent's computed weight and are rejected.
pub fn parse_font_weight(ctx: &str, name: &str, value: &str) -> Result<u16, ParseError> {
    match value.trim() {
        "normal" => Ok(400),
        "bold" => Ok(700),
        "lighter" | "bolder" => Err(bad(
            ctx,
            name,
            value,
            "relative weights (lighter/bolder) are not supported - use a number 1..=1000".into(),
        )),
        v => {
            let n = parse_num(ctx, name, value, v)?;
            if !(1.0..=1000.0).contains(&n) {
                return Err(bad(ctx, name, value, "font-weight must be 1..=1000".into()));
            }
            Ok(n.round() as u16)
        }
    }
}

/// CSS `border-radius` multi-value shorthand -> per-corner radii
/// `[top-left, top-right, bottom-right, bottom-left]`:
///
/// * 1 value `a` -> `[a, a, a, a]`
/// * 2 values `a b` -> `[a, b, a, b]`
/// * 3 values `a b c` -> `[a, b, c, b]`
/// * 4 values -> explicit per-corner.
pub fn parse_corner_radii(ctx: &str, name: &str, value: &str) -> Result<[f32; 4], ParseError> {
    let parts: Vec<&str> = value.split_whitespace().collect();
    let n = |s: &str| parse_num(ctx, name, value, s);
    Ok(match parts.as_slice() {
        [a] => {
            let a = n(a)?;
            [a; 4]
        }
        [a, b] => {
            let (a, b) = (n(a)?, n(b)?);
            [a, b, a, b]
        }
        [a, b, c] => {
            let (a, b, c) = (n(a)?, n(b)?, n(c)?);
            [a, b, c, b]
        }
        [a, b, c, d] => [n(a)?, n(b)?, n(c)?, n(d)?],
        other => {
            return Err(bad(
                ctx,
                name,
                value,
                format!("expected 1, 2, 3, or 4 radii, got {}", other.len()),
            ));
        }
    })
}

/// Parse a plain `f32`.
pub fn parse_f32(ctx: &str, name: &str, value: &str) -> Result<f32, ParseError> {
    parse_num(ctx, name, value, value.trim())
}

/// Parse a plain `i32`.
pub fn parse_i32(ctx: &str, name: &str, value: &str) -> Result<i32, ParseError> {
    value
        .trim()
        .parse()
        .map_err(|e: std::num::ParseIntError| bad(ctx, name, value, e.to_string()))
}

fn parse_num(ctx: &str, name: &str, value: &str, s: &str) -> Result<f32, ParseError> {
    let s = s.trim();
    // Authors habitually write `8px` on numeric props (radius, font-size,
    // gap, shadow offsets); px is the native unit, so accept and strip it.
    let s = s.strip_suffix("px").map(str::trim_end).unwrap_or(s);
    s.parse().map_err(|e: std::num::ParseFloatError| {
        bad(ctx, name, value, format!("bad number '{s}': {e}"))
    })
}

/// Construct a `BadAttribute` error.
pub fn bad(ctx: &str, name: &str, value: &str, reason: String) -> ParseError {
    ParseError::BadAttribute {
        name: name.to_string(),
        value: value.to_string(),
        tag: ctx.to_string(),
        reason,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::layout_ir::{CursorImageSpec, CursorSpec};

    fn cursor(value: &str) -> Result<CursorSpec, ParseError> {
        parse_cursor("x", "cursor", value)
    }

    #[test]
    fn a_cursor_is_a_keyword_or_images_then_a_keyword() {
        assert_eq!(
            cursor("Crosshair").unwrap(),
            CursorSpec {
                images: Vec::new(),
                keyword: "crosshair".into(),
            }
        );
        assert_eq!(
            cursor(r#"url("pen.png") 2 14, url(alt.png), not-allowed"#).unwrap(),
            CursorSpec {
                images: vec![
                    CursorImageSpec {
                        url: "pen.png".into(),
                        hotspot: Some((2.0, 14.0)),
                    },
                    CursorImageSpec {
                        url: "alt.png".into(),
                        hotspot: None,
                    },
                ],
                keyword: "not-allowed".into(),
            }
        );
    }

    #[test]
    fn a_cursor_without_a_fallback_keyword_is_refused() {
        for bad in [
            "url(pen.png)",
            "hand",
            "url(pen.png) 2, default",
            "pointer, default",
        ] {
            assert!(cursor(bad).is_err(), "`{bad}` should not parse");
        }
    }

    #[test]
    fn a_cursor_value_writes_back_as_css() {
        let spec = cursor(r#"url("pen.png") 2 14, crosshair"#).unwrap();
        assert_eq!(spec.to_css(), r#"url("pen.png") 2 14, crosshair"#);
    }

    /// The mask longhands take the web's spellings, and the ones this
    /// subset leaves out are refused rather than read as something else.
    #[test]
    fn mask_longhands_read_the_web_values() {
        use crate::layout_ir::{MaskImageSpec, MaskPositionSpec, MaskRepeatSpec, MaskSizeSpec};
        assert_eq!(
            parse_mask_image("x", "mask-image", r#"url("icons/pen.svg")"#).unwrap(),
            MaskImageSpec::Url("icons/pen.svg".into())
        );
        assert_eq!(
            parse_mask_image("x", "mask-image", "none").unwrap(),
            MaskImageSpec::None
        );
        assert!(parse_mask_image("x", "mask-image", "linear-gradient(#000, #fff)").is_err());

        assert_eq!(
            parse_mask_size("x", "mask-size", "contain").unwrap(),
            MaskSizeSpec::Contain
        );
        assert_eq!(
            parse_mask_size("x", "mask-size", "16px").unwrap(),
            MaskSizeSpec::Size(LengthSpec::Px(16.0), LengthSpec::Auto)
        );
        assert_eq!(
            parse_mask_size("x", "mask-size", "50% 8").unwrap(),
            MaskSizeSpec::Size(LengthSpec::Percent(50.0), LengthSpec::Px(8.0))
        );

        assert_eq!(
            parse_mask_repeat("x", "mask-repeat", "repeat-x").unwrap(),
            MaskRepeatSpec { x: true, y: false }
        );
        assert_eq!(
            parse_mask_repeat("x", "mask-repeat", "no-repeat repeat").unwrap(),
            MaskRepeatSpec { x: false, y: true }
        );
        assert!(parse_mask_repeat("x", "mask-repeat", "space").is_err());

        let pos = |v: &str| parse_mask_position("x", "mask-position", v);
        let p = |x, y| MaskPositionSpec { x, y };
        assert_eq!(
            pos("center").unwrap(),
            p(LengthSpec::Percent(50.0), LengthSpec::Percent(50.0))
        );
        assert_eq!(
            pos("top").unwrap(),
            p(LengthSpec::Percent(50.0), LengthSpec::Percent(0.0))
        );
        assert_eq!(
            pos("bottom right").unwrap(),
            p(LengthSpec::Percent(100.0), LengthSpec::Percent(100.0))
        );
        assert_eq!(
            pos("4px 25%").unwrap(),
            p(LengthSpec::Px(4.0), LengthSpec::Percent(25.0))
        );
        assert!(pos("left right").is_err());
        assert!(pos("right 4px top 2px").is_err());
    }
}
