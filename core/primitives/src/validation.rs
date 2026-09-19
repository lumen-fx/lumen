//! Form-field validation driver.
//!
//! Recomputes the `is_valid` flag on every entity carrying a [`Validation`] component and mirrors
//! the result into [`PropertyStore`] under the `valid:<lumen-id>` global key.
//!
//! Supported matchers:
//!
//! - `required`: trimmed text must be non-empty; the slider value must be `> 0`; the toggle must be `checked`.
//! - `pattern`: content must contain the configured literal substring, or match a
//!   named shape when the pattern starts with `shape:` (see [`matches_pattern`]).
//! - `min` / `max`: numeric range applied to slider values and to input text when it parses as a number.

use bevy_ecs::prelude::*;
use lumen_core::components::{LumenId, SliderValue, TextContent, Toggleable, Validation};
use lumen_core::prelude::TickStage;
use lumen_core::property_store::PropertyStore;

/// Plugin: registers [`apply_validation`] in `TickStage::Systems` so
/// validation runs after author-side tick logic has settled.
pub struct ValidationPlugin;

impl lumen_core::prelude::Plugin for ValidationPlugin {
    fn build(self, app: &mut lumen_core::prelude::App) {
        app.add_systems(TickStage::Systems, apply_validation);
    }
}

/// Walk every entity whose validation input changed this tick. Pick the
/// most relevant content source (TextContent -> slider -> toggle), apply
/// the rules, write the result back to [`Validation::is_valid`], and
/// mirror into `PropertyStore[Global("valid:<id>")]` when the entity has a stable
/// [`LumenId`].
///
/// The `Or<(Changed<TextContent>, Changed<SliderValue>, Changed<Toggleable>, Added<Validation>)>`
/// filter gates the walk so steady-state ticks (most ticks, in practice)
/// do zero work. `Added<Validation>` keeps the freshly-spawned entity
/// running through its rules on insert even if no source-of-truth
/// component has mutated yet. Re-validation on `Validation` rule
/// changes (e.g. an author swaps `min` via class flip) is handled by
/// the Wave 1.5 / 1.6 PropertyStore notify side and not gated here.
#[allow(clippy::type_complexity)]
pub fn apply_validation(
    mut q: Query<
        (
            &mut Validation,
            Option<&TextContent>,
            Option<&SliderValue>,
            Option<&Toggleable>,
            Option<&LumenId>,
        ),
        Or<(
            Changed<TextContent>,
            Changed<SliderValue>,
            Changed<Toggleable>,
            Added<Validation>,
        )>,
    >,
    mut store: ResMut<PropertyStore>,
) {
    for (mut v, tc, sv, tg, id) in &mut q {
        let valid = evaluate(&v, tc.map(|t| t.0.as_str()), sv.copied(), tg.copied());
        if v.is_valid != valid {
            v.is_valid = valid;
        }
        if let Some(id) = id {
            let key = format!("valid:{}", id.0);
            // `PropertyStore::set_global_bool` is internally idempotent - it skips the
            // dirty-queue push when the cell is already at the target value.
            if store.get_global_bool(&key) != Some(valid) {
                store.set_global_bool(&key, valid);
            }
        }
    }
}

/// Pure validation function - handy for tests and any author-side
/// script-bridge that wants to validate a value without spawning an
/// entity. Pass the relevant slot for the entity's kind; the other
/// slots stay `None`.
pub fn evaluate(
    v: &Validation,
    text: Option<&str>,
    slider: Option<SliderValue>,
    toggle: Option<Toggleable>,
) -> bool {
    if v.required {
        let has_value = text
            .map(|s| !s.trim().is_empty())
            .or_else(|| toggle.map(|t| t.checked))
            .or_else(|| slider.map(|s| s.value > 0.0))
            .unwrap_or(false);
        if !has_value {
            return false;
        }
    }
    if let (Some(pat), Some(t)) = (v.pattern.as_deref(), text)
        && !matches_pattern(pat, t)
    {
        return false;
    }
    let numeric: Option<f32> = text.and_then(|t| t.trim().parse().ok());
    let candidate: Option<f32> = numeric.or_else(|| slider.map(|s| s.value));
    if let Some(n) = candidate {
        if let Some(lo) = v.min
            && n < lo
        {
            return false;
        }
        if let Some(hi) = v.max
            && n > hi
        {
            return false;
        }
    }
    true
}

/// Apply a `pattern` rule to one value.
///
/// A pattern starting with `shape:` names a built-in structural check;
/// `<date-picker>` and `<time-picker>` compile to inputs carrying one.
/// Any other pattern is the literal-substring rule.
///
/// | Pattern | Shape | Ranges |
/// | --- | --- | --- |
/// | `shape:date` | `YYYY-MM-DD` | month 01-12, day 01-31 |
/// | `shape:time` | `HH:MM` | hour 00-23, minute 00-59 |
///
/// Both are shape checks, not calendar checks: `2026-02-31` passes.
/// Surrounding whitespace is trimmed first. An unknown `shape:` name
/// never matches, so a typo fails loudly instead of silently accepting
/// everything.
pub fn matches_pattern(pattern: &str, text: &str) -> bool {
    match pattern {
        "shape:date" => is_iso_date(text),
        "shape:time" => is_clock_time(text),
        other => match other.strip_prefix("shape:") {
            Some(_) => false,
            None => text.contains(other),
        },
    }
}

/// Parse a fixed-width run of ASCII digits into a number, rejecting any
/// other byte. `slice` must already be the exact span to read.
fn fixed_digits(slice: &str, width: usize) -> Option<u32> {
    if slice.len() != width || !slice.bytes().all(|b| b.is_ascii_digit()) {
        return None;
    }
    slice.parse().ok()
}

/// `YYYY-MM-DD` with month 01-12 and day 01-31.
fn is_iso_date(text: &str) -> bool {
    let t = text.trim();
    // ASCII gate first: the byte slicing below is only char-boundary
    // safe once every byte is a single-byte character.
    if !t.is_ascii() || t.len() != 10 || t.as_bytes()[4] != b'-' || t.as_bytes()[7] != b'-' {
        return false;
    }
    let (Some(_year), Some(month), Some(day)) = (
        fixed_digits(&t[0..4], 4),
        fixed_digits(&t[5..7], 2),
        fixed_digits(&t[8..10], 2),
    ) else {
        return false;
    };
    (1..=12).contains(&month) && (1..=31).contains(&day)
}

/// 24-hour `HH:MM` with hour 00-23 and minute 00-59.
fn is_clock_time(text: &str) -> bool {
    let t = text.trim();
    if !t.is_ascii() || t.len() != 5 || t.as_bytes()[2] != b':' {
        return false;
    }
    let (Some(hour), Some(minute)) = (fixed_digits(&t[0..2], 2), fixed_digits(&t[3..5], 2)) else {
        return false;
    };
    hour <= 23 && minute <= 59
}

#[cfg(test)]
mod tests {
    use super::*;

    fn validation(
        required: bool,
        pattern: Option<&str>,
        min: Option<f32>,
        max: Option<f32>,
    ) -> Validation {
        Validation {
            required,
            pattern: pattern.map(|s| s.to_string()),
            min,
            max,
            is_valid: true,
        }
    }

    #[test]
    fn required_rejects_empty_text() {
        let v = validation(true, None, None, None);
        assert!(!evaluate(&v, Some(""), None, None));
        assert!(!evaluate(&v, Some("   "), None, None));
        assert!(evaluate(&v, Some("hi"), None, None));
    }

    #[test]
    fn pattern_requires_substring() {
        let v = validation(false, Some("@"), None, None);
        assert!(!evaluate(&v, Some("plain"), None, None));
        assert!(evaluate(&v, Some("name@host"), None, None));
    }

    #[test]
    fn date_shape_checks_positions_and_ranges() {
        let v = validation(false, Some("shape:date"), None, None);
        assert!(evaluate(&v, Some("2026-08-10"), None, None));
        assert!(evaluate(&v, Some("0001-01-01"), None, None));
        // Shape, not calendar: day 31 of February passes.
        assert!(evaluate(&v, Some("2026-02-31"), None, None));
        // Surrounding whitespace is trimmed, as it is for `required`.
        assert!(evaluate(&v, Some("  2026-08-10\t"), None, None));
        for bad in [
            "",
            "-",
            "not-a-date",
            "2026-8-10",
            "26-08-10",
            "2026/08/10",
            "2026-13-01",
            "2026-00-01",
            "2026-08-32",
            "2026-08-00",
            "2026-08-1o",
            "2026-08-10x",
            "2026-08-10-01",
        ] {
            assert!(
                !evaluate(&v, Some(bad), None, None),
                "{bad:?} must not pass the date shape"
            );
        }
    }

    #[test]
    fn time_shape_checks_positions_and_ranges() {
        let v = validation(false, Some("shape:time"), None, None);
        assert!(evaluate(&v, Some("00:00"), None, None));
        assert!(evaluate(&v, Some("23:59"), None, None));
        for bad in [
            "", ":", "9:05", "24:00", "23:60", "1:2", "12:5", "12-05", "aa:bb",
        ] {
            assert!(
                !evaluate(&v, Some(bad), None, None),
                "{bad:?} must not pass the time shape"
            );
        }
    }

    #[test]
    fn unknown_shape_never_matches() {
        assert!(!matches_pattern("shape:weekday", "monday"));
    }

    #[test]
    fn non_ascii_input_does_not_panic() {
        assert!(!matches_pattern("shape:date", "2026-08-1\u{00e9}"));
        assert!(!matches_pattern("shape:time", "1\u{00e9}:00"));
    }

    #[test]
    fn numeric_bounds_apply_to_text() {
        let v = validation(false, None, Some(0.0), Some(100.0));
        assert!(evaluate(&v, Some("50"), None, None));
        assert!(!evaluate(&v, Some("-1"), None, None));
        assert!(!evaluate(&v, Some("101"), None, None));
    }

    #[test]
    fn slider_value_validated_against_bounds() {
        let v = validation(false, None, Some(0.5), Some(1.0));
        let lo = SliderValue {
            value: 0.4,
            min: 0.0,
            max: 1.0,
            step: None,
        };
        let hi = SliderValue {
            value: 0.8,
            min: 0.0,
            max: 1.0,
            step: None,
        };
        assert!(!evaluate(&v, None, Some(lo), None));
        assert!(evaluate(&v, None, Some(hi), None));
    }

    #[test]
    fn required_toggle_must_be_checked() {
        let v = validation(true, None, None, None);
        assert!(!evaluate(
            &v,
            None,
            None,
            Some(Toggleable { checked: false })
        ));
        assert!(evaluate(&v, None, None, Some(Toggleable { checked: true })));
    }
}
