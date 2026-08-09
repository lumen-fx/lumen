//! Form-field validation driver.
//!
//! Recomputes the `is_valid` flag on every entity carrying a [`Validation`] component and mirrors
//! the result into [`PropertyStore`] under the `valid:<lumen-id>` global key.
//!
//! Supported matchers:
//!
//! - `required`: trimmed text must be non-empty; the slider value must be `> 0`; the toggle must be `checked`.
//! - `pattern`: content must contain the configured literal substring.
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
        && !t.contains(pat)
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
