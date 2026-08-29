//! Colors, in the two spellings a canvas script uses.
//!
//! A script that computes a color writes the four components it computed
//! (`set_fill_rgba(id, r, g, b, a)`, components 0..1, the same range every
//! other Lumen colour API takes). A script that copies one out of a design
//! writes the CSS text it was given (`set_fill_style(id, "#ff8800")`). Both
//! land here.
//!
//! A buffer pixel is neither: it is one packed `0xRRGGBBAA` integer, because
//! a pixel loop moves millions of them and a four-value call per pixel would
//! cost more in the script host than the drawing does.

/// One straight (not premultiplied) sRGB color, components 0..1.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Rgba {
    /// Red, 0..1.
    pub r: f32,
    /// Green, 0..1.
    pub g: f32,
    /// Blue, 0..1.
    pub b: f32,
    /// Alpha, 0..1.
    pub a: f32,
}

impl Rgba {
    /// Opaque black, which is what a canvas draws with until told otherwise.
    pub const BLACK: Rgba = Rgba {
        r: 0.0,
        g: 0.0,
        b: 0.0,
        a: 1.0,
    };

    /// Build from four components, each clamped into range.
    #[must_use]
    pub fn new(r: f64, g: f64, b: f64, a: f64) -> Rgba {
        let c = |v: f64| v.clamp(0.0, 1.0) as f32;
        Rgba {
            r: c(r),
            g: c(g),
            b: c(b),
            a: c(a),
        }
    }

    /// The same color at `alpha` times its own alpha, which is how
    /// `set_global_alpha` folds into a brush.
    #[must_use]
    pub fn scaled_alpha(self, alpha: f32) -> Rgba {
        Rgba {
            a: (self.a * alpha).clamp(0.0, 1.0),
            ..self
        }
    }

    /// The packed `0xRRGGBBAA` integer a buffer pixel is written as.
    #[must_use]
    pub fn packed(self) -> u32 {
        let c = |v: f32| (v.clamp(0.0, 1.0) * 255.0).round() as u32;
        (c(self.r) << 24) | (c(self.g) << 16) | (c(self.b) << 8) | c(self.a)
    }
}

/// Parse the CSS colors `set_fill_style` accepts, or `None`.
///
/// The subset is what an author writes by hand: `#rgb`, `#rgba`, `#rrggbb`,
/// `#rrggbbaa`, `rgb(..)` / `rgba(..)` with numeric or percentage channels,
/// `transparent`, and the sixteen named colors CSS level 1 defined. Anything
/// else is refused rather than guessed at, and the caller reports it; a
/// silently wrong color is harder to find than a missing one.
#[must_use]
pub fn parse_css(text: &str) -> Option<Rgba> {
    let text = text.trim();
    if let Some(hex) = text.strip_prefix('#') {
        return parse_hex(hex);
    }
    if let Some(rest) = text
        .strip_prefix("rgba(")
        .or_else(|| text.strip_prefix("rgb("))
        && let Some(inner) = rest.strip_suffix(')')
    {
        return parse_rgb_call(inner);
    }
    named(&text.to_ascii_lowercase())
}

/// `#rgb`, `#rgba`, `#rrggbb`, `#rrggbbaa`.
fn parse_hex(hex: &str) -> Option<Rgba> {
    let nibble = |c: char| c.to_digit(16).map(|d| d as f64);
    let byte = |s: &str| u8::from_str_radix(s, 16).ok().map(|v| f64::from(v) / 255.0);
    let parts: Vec<f64> = match hex.len() {
        3 | 4 => hex
            .chars()
            .map(|c| nibble(c).map(|d| d * 17.0 / 255.0))
            .collect::<Option<Vec<f64>>>()?,
        6 | 8 => (0..hex.len() / 2)
            .map(|i| byte(hex.get(i * 2..i * 2 + 2)?))
            .collect::<Option<Vec<f64>>>()?,
        _ => return None,
    };
    let alpha = parts.get(3).copied().unwrap_or(1.0);
    Some(Rgba::new(parts[0], parts[1], parts[2], alpha))
}

/// The body of `rgb(..)` / `rgba(..)`: three or four comma- or
/// space-separated channels, each a number 0..255 or a percentage. Alpha is
/// 0..1 or a percentage, as CSS spells it.
fn parse_rgb_call(inner: &str) -> Option<Rgba> {
    let fields: Vec<&str> = inner
        .split([',', '/', ' '])
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .collect();
    if fields.len() < 3 || fields.len() > 4 {
        return None;
    }
    let channel = |s: &str| -> Option<f64> {
        match s.strip_suffix('%') {
            Some(pct) => pct.trim().parse::<f64>().ok().map(|v| v / 100.0),
            None => s.parse::<f64>().ok().map(|v| v / 255.0),
        }
    };
    let alpha = match fields.get(3) {
        None => 1.0,
        Some(s) => match s.strip_suffix('%') {
            Some(pct) => pct.trim().parse::<f64>().ok()? / 100.0,
            None => s.parse::<f64>().ok()?,
        },
    };
    Some(Rgba::new(
        channel(fields[0])?,
        channel(fields[1])?,
        channel(fields[2])?,
        alpha,
    ))
}

/// The CSS level 1 names, plus `transparent`.
fn named(name: &str) -> Option<Rgba> {
    let hex = match name {
        "transparent" => return Some(Rgba::new(0.0, 0.0, 0.0, 0.0)),
        "black" => "000000",
        "silver" => "c0c0c0",
        "gray" | "grey" => "808080",
        "white" => "ffffff",
        "maroon" => "800000",
        "red" => "ff0000",
        "purple" => "800080",
        "fuchsia" | "magenta" => "ff00ff",
        "green" => "008000",
        "lime" => "00ff00",
        "olive" => "808000",
        "yellow" => "ffff00",
        "navy" => "000080",
        "blue" => "0000ff",
        "teal" => "008080",
        "aqua" | "cyan" => "00ffff",
        "orange" => "ffa500",
        _ => return None,
    };
    parse_hex(hex)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn approx(a: f32, b: f32) -> bool {
        (a - b).abs() < 0.005
    }

    #[test]
    fn the_hex_spellings_agree_with_each_other() {
        let long = parse_css("#ff8800").expect("six digits");
        let short = parse_css("#f80").expect("three digits");
        assert!(approx(long.r, short.r) && approx(long.g, short.g) && approx(long.b, short.b));
        assert_eq!(long.a, 1.0);
        assert!(approx(
            parse_css("#ff880080").expect("eight digits").a,
            0.502
        ));
    }

    #[test]
    fn the_functional_spellings_take_numbers_and_percentages() {
        let a = parse_css("rgb(255, 136, 0)").expect("rgb");
        let b = parse_css("rgb(100%, 53.3%, 0%)").expect("percentages");
        assert!(approx(a.r, b.r) && approx(a.g, b.g) && approx(a.b, b.b));
        assert!(approx(parse_css("rgba(0,0,0,0.25)").expect("rgba").a, 0.25));
        // CSS spells alpha either way, and a design tool writes the percent.
        assert!(approx(parse_css("rgba(0,0,0,25%)").expect("rgba").a, 0.25));
        // Space-separated, which is how CSS colour level 4 writes it.
        assert!(approx(
            parse_css("rgb(255 136 0 / 50%)").expect("slashed").a,
            0.5
        ));
        // An alpha that is not a number at all is refused with the rest.
        assert!(parse_css("rgba(0,0,0,none)").is_none());
    }

    #[test]
    fn a_color_outside_the_subset_is_refused() {
        for bad in [
            "hsl(200, 50%, 50%)",
            "#12345",
            "rebeccapurple",
            "",
            "rgb(1)",
        ] {
            assert!(parse_css(bad).is_none(), "{bad} parsed");
        }
    }

    #[test]
    fn packing_round_trips_the_bytes() {
        assert_eq!(parse_css("#ff8800").expect("hex").packed(), 0xff8800ff);
        assert_eq!(parse_css("transparent").expect("named").packed(), 0);
    }
}
