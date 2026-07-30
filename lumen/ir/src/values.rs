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

/// Parse a `bg=` value - either a hex color or a `linear-gradient(...)`
/// function. Stops accept the CSS forms `<color>` (auto-distributed) or
/// `<color> <offset%>` (explicit). Parsed stops are sorted by offset
/// ascending so the renderer can hand them straight to peniko.
pub fn parse_bg(
    ctx: &str,
    name: &str,
    value: &str,
) -> Result<crate::layout_ir::BgSpec, ParseError> {
    use crate::layout_ir::BgSpec;
    let trimmed = value.trim();
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

/// Parses `radial-gradient(<color1>, <color2 [stop%]>, ...)`. An optional trailing `<radius>` percentage normalises the radius in `0..1`.
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
    let parts: Vec<&str> = value.split_whitespace().collect();
    let n = |s: &str| parse_edge_term(ctx, name, value, s);
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
        other => {
            return Err(bad(
                ctx,
                name,
                value,
                format!("expected 1, 2, 3, or 4 numbers, got {other}"),
            ));
        }
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
