//! `<input type="number">`: a text field that holds a number and steps it.
//!
//! The field stays an ordinary text field: the user can type any number into
//! it, and the validation driver marks it invalid while the text is not one,
//! lies outside `min` / `max`, or falls between steps. On top of that the
//! value steps the way a browser's number field does:
//!
//! - ArrowUp / ArrowDown on the focused field ([`step_number_on_keys`]),
//! - the wheel over the focused field ([`step_number_on_wheel`]), which then
//!   does not scroll the page under it,
//! - the `.number-step-up` / `.number-step-down` buttons inside it
//!   ([`step_number_on_buttons`]).
//!
//! Each step writes the new text through the text pipeline, so `input`
//! handlers and `bind-value` see it as they see a keystroke.
//!
//! A step follows HTML's `stepUp()` / `stepDown()`: a value off the step grid
//! moves to the nearest grid value in the step's direction, a value on it
//! moves by one step, and the result is kept within `min` and `max`. The
//! grid starts at `min`, else at 0. Empty or unreadable text steps from 0,
//! moved into range.

use std::sync::Arc;

use bevy_ecs::message::{MessageReader, MessageWriter};
use bevy_ecs::prelude::*;
use lumen_core::input::FocusedKey;
use lumen_core::prelude::*;
use lumen_core::text_events::TextEditRequest;
use lumen_core::text_model::TextBuffer;

use crate::controls::{WHEEL_NOTCH_PX, resolve_control};

/// The step rules of an `<input type="number">`.
#[derive(Component, Clone, Copy, Debug, PartialEq)]
pub struct NumberInput {
    /// The lowest value the steps reach.
    pub min: Option<f32>,
    /// The highest value the steps reach.
    pub max: Option<f32>,
    /// The step, `None` for `step="any"`, which steps by one and accepts
    /// any value.
    pub step: Option<f32>,
}

impl Default for NumberInput {
    /// HTML's defaults: no bounds, a step of one.
    fn default() -> Self {
        Self {
            min: None,
            max: None,
            step: Some(1.0),
        }
    }
}

impl NumberInput {
    /// The rules an element's `min`, `max` and `step` attributes give, as
    /// the parser recorded them: no `step` is one, and `0` is `step="any"`.
    pub fn from_attributes(min: Option<f32>, max: Option<f32>, step: Option<f32>) -> Self {
        Self {
            min,
            max,
            step: match step {
                None => Some(1.0),
                Some(s) if s > 0.0 && s.is_finite() => Some(s),
                Some(_) => None,
            },
        }
    }

    /// Where the step grid starts.
    fn base(&self) -> f64 {
        decimal(self.min.unwrap_or(0.0))
    }

    /// The step a single press moves by.
    fn stride(&self) -> f64 {
        decimal(self.step.unwrap_or(1.0))
    }

    /// Decimal places a value on the grid is written with: as many as the
    /// base and the step carry, so `0.1` steps from `0.8` give `0.9`, not the
    /// `0.90000004` binary floating point arrives at.
    /// With `step="any"` there is no grid to follow, so a value keeps the
    /// places it has, cleaned of binary noise past what an `f32` holds.
    fn places(&self) -> i32 {
        match self.step {
            Some(step) => decimal_places(self.min.unwrap_or(0.0)).max(decimal_places(step)),
            None => 9,
        }
    }

    /// The value `n` presses from `text` lead to (`n` negative steps down).
    pub fn stepped(&self, text: &str, n: i32) -> f64 {
        let base = self.base();
        let stride = self.stride();
        let current = text.trim().parse::<f64>().ok().filter(|v| v.is_finite());
        let mut value = current.unwrap_or(0.0);
        if current.is_some() || n != 0 {
            let offset = (value - base) / stride;
            let on_grid = (offset - offset.round()).abs() < GRID_EPSILON;
            value = if self.step.is_some() && !on_grid {
                // Off the grid: the first grid value in the step's direction,
                // and then the rest of the presses.
                let first = if n > 0 { offset.ceil() } else { offset.floor() };
                let rest = f64::from(n) - f64::from(n.signum());
                base + (first + rest) * stride
            } else {
                value + f64::from(n) * stride
            };
        }
        let value = self.clamp(value);
        let scale = 10f64.powi(self.places());
        (value * scale).round() / scale
    }

    /// `value` kept within `min` and `max`, on the grid where there is one.
    fn clamp(&self, value: f64) -> f64 {
        let base = self.base();
        let stride = self.stride();
        let on_grid = |v: f64, up: bool| {
            if self.step.is_none() {
                return v;
            }
            let offset = (v - base) / stride;
            let k = if up { offset.ceil() } else { offset.floor() };
            base + k * stride
        };
        let mut v = value;
        if let Some(min) = self.min.map(decimal)
            && v < min
        {
            v = on_grid(min, true);
        }
        if let Some(max) = self.max.map(decimal)
            && v > max
        {
            v = on_grid(max, false);
        }
        v
    }

    /// How a stepped value is written into the field.
    pub fn format(&self, value: f64) -> String {
        let places = self.places().max(0) as usize;
        let text = format!("{value:.places$}");
        if text.contains('.') {
            text.trim_end_matches('0').trim_end_matches('.').to_string()
        } else {
            text
        }
    }
}

/// How far from a whole number of steps a value may sit and still count as
/// on the grid: the attributes are `f32`, so a value typed as `0.8` and a
/// `min` of `0.8` differ in the eighth digit.
const GRID_EPSILON: f64 = 1e-6;

/// `x` as the decimal it was written as, rather than the binary value an
/// `f32` holds for it: `0.1` is `0.1`, not `0.10000000149`.
fn decimal(x: f32) -> f64 {
    x.to_string().parse().unwrap_or(f64::from(x))
}

/// How many decimal places `x` is written with in its shortest form.
/// Capped at nine, past what an `f32` holds.
fn decimal_places(x: f32) -> i32 {
    let text = x.abs().to_string();
    let places = text.split_once('.').map_or(0, |(_, frac)| frac.len());
    places.min(9) as i32
}

/// A `.number-step-up` (`up`) or `.number-step-down` button inside a
/// number field.
#[derive(Component, Clone, Copy, Debug, PartialEq, Eq)]
pub struct NumberStepButton {
    /// Whether it steps up.
    pub up: bool,
}

/// Replace the field's whole text with the value `n` steps away, as an
/// edit the text pipeline applies.
fn step(
    entity: Entity,
    number: &NumberInput,
    buffer: &TextBuffer,
    n: i32,
    edits: &mut MessageWriter<TextEditRequest>,
) {
    let current = buffer.to_string();
    let next = number.format(number.stepped(&current, n));
    if next == current {
        return;
    }
    edits.write(TextEditRequest::Replace {
        entity,
        range: 0..current.len(),
        text: Arc::from(next.as_str()),
    });
}

/// ArrowUp and ArrowDown step the focused number field.
pub fn step_number_on_keys(
    mut keys: MessageReader<FocusedKey>,
    fields: Query<(&NumberInput, &TextBuffer)>,
    mut edits: MessageWriter<TextEditRequest>,
) {
    for key in keys.read() {
        let n = match key.key {
            Key::Named(NamedKey::ArrowUp) => 1,
            Key::Named(NamedKey::ArrowDown) => -1,
            _ => continue,
        };
        if let Ok((number, buffer)) = fields.get(key.entity) {
            step(key.entity, number, buffer, n, &mut edits);
        }
    }
}

/// The wheel over a focused number field steps it, a notch per step, wheel
/// up stepping up. An unfocused field under the pointer lets the wheel
/// scroll the page, as a browser does.
pub fn step_number_on_wheel(
    mut wheels: MessageReader<MouseWheel>,
    hovered: Query<Entity, With<Hovered>>,
    parents: Query<&ChildOf>,
    fields: Query<(&NumberInput, &TextBuffer), With<Focused>>,
    mut edits: MessageWriter<TextEditRequest>,
    mut carry: Local<f32>,
) {
    let total: f32 = wheels.read().map(|w| w.delta.y).sum();
    if total == 0.0 {
        return;
    }
    let Some(target) = hovered
        .iter()
        .next()
        .and_then(|h| resolve_control(h, &parents, |e| fields.contains(e)))
    else {
        *carry = 0.0;
        return;
    };
    let Ok((number, buffer)) = fields.get(target) else {
        return;
    };
    // A trackpad sends fractions of a notch; they add up to whole steps.
    *carry -= total / WHEEL_NOTCH_PX;
    let n = carry.trunc();
    *carry -= n;
    if n != 0.0 {
        step(target, number, buffer, n as i32, &mut edits);
    }
}

/// A click on a step button steps the field it sits in.
pub fn step_number_on_buttons(
    mut clicks: MessageReader<ClickEvent>,
    buttons: Query<(&NumberStepButton, &ChildOf)>,
    parents: Query<&ChildOf>,
    fields: Query<(&NumberInput, &TextBuffer)>,
    mut edits: MessageWriter<TextEditRequest>,
) {
    for click in clicks.read() {
        let Some(button) = resolve_control(click.entity, &parents, |e| buttons.contains(e)) else {
            continue;
        };
        let Ok((dir, _)) = buttons.get(button) else {
            continue;
        };
        // The button sits in the steps box, which sits in the field.
        let mut cur = parents.get(button).ok().map(ChildOf::parent);
        while let Some(e) = cur {
            if let Ok((number, buffer)) = fields.get(e) {
                step(e, number, buffer, if dir.up { 1 } else { -1 }, &mut edits);
                break;
            }
            cur = parents.get(e).ok().map(ChildOf::parent);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn rules(min: Option<f32>, max: Option<f32>, step: Option<f32>) -> NumberInput {
        NumberInput::from_attributes(min, max, step)
    }

    fn step_text(n: &NumberInput, text: &str, by: i32) -> String {
        n.format(n.stepped(text, by))
    }

    /// A value on the grid moves by one step and stays in range.
    #[test]
    fn steps_move_by_the_step_within_range() {
        let n = rules(Some(0.0), Some(10.0), Some(2.0));
        assert_eq!(step_text(&n, "4", 1), "6");
        assert_eq!(step_text(&n, "4", -1), "2");
        assert_eq!(step_text(&n, "10", 1), "10");
        assert_eq!(step_text(&n, "0", -1), "0");
    }

    /// A value off the grid moves to the nearest grid value in the step's
    /// direction, as HTML's `stepUp()` does.
    #[test]
    fn an_off_grid_value_moves_onto_the_grid() {
        let n = rules(Some(1.0), None, Some(5.0));
        assert_eq!(step_text(&n, "3", 1), "6");
        assert_eq!(step_text(&n, "3", -1), "1");
        assert_eq!(step_text(&n, "3", 2), "11");
    }

    /// Empty or unreadable text steps from 0, moved into range.
    #[test]
    fn empty_text_steps_from_zero_into_range() {
        let n = rules(Some(5.0), Some(9.0), None);
        assert_eq!(step_text(&n, "", 1), "5");
        let n = rules(None, None, None);
        assert_eq!(step_text(&n, "", 1), "1");
        assert_eq!(step_text(&n, "abc", -1), "-1");
    }

    /// Decimal steps land on clean decimals, and `step="any"` steps by one
    /// from wherever the value is.
    #[test]
    fn decimal_and_any_steps() {
        let n = rules(Some(0.8), None, Some(0.1));
        assert_eq!(step_text(&n, "0.8", 1), "0.9");
        assert_eq!(step_text(&n, "1.2", -3), "0.9");
        let any = rules(None, None, Some(0.0));
        assert_eq!(any.step, None);
        assert_eq!(step_text(&any, "2.5", 1), "3.5");
    }

    /// A maximum off the grid stops at the last grid value under it.
    #[test]
    fn a_maximum_off_the_grid_stops_at_the_grid() {
        let n = rules(Some(0.0), Some(100.0), Some(30.0));
        assert_eq!(step_text(&n, "90", 1), "90");
        assert_eq!(step_text(&n, "500", -1), "90");
    }

    /// ArrowUp on a focused number field asks the text pipeline to replace
    /// its text with the next step; other fields and other keys ask nothing.
    #[test]
    fn arrow_keys_replace_the_text_with_the_next_step() {
        use bevy_ecs::message::Messages;
        use bevy_ecs::system::RunSystemOnce;
        let mut world = World::new();
        world.init_resource::<Messages<FocusedKey>>();
        world.init_resource::<Messages<TextEditRequest>>();
        let field = world
            .spawn((rules(None, Some(5.0), None), TextBuffer::single_line("4")))
            .id();
        let plain = world.spawn(TextBuffer::single_line("4")).id();
        for (entity, key) in [
            (field, NamedKey::ArrowUp),
            (plain, NamedKey::ArrowUp),
            (field, NamedKey::Home),
        ] {
            world.write_message(FocusedKey {
                entity,
                key: Key::Named(key),
                modifiers: Modifiers::default(),
                repeat: false,
            });
        }
        world.run_system_once(step_number_on_keys).unwrap();
        let edits: Vec<_> = world
            .resource::<Messages<TextEditRequest>>()
            .iter_current_update_messages()
            .map(|e| match e {
                TextEditRequest::Replace {
                    entity,
                    range,
                    text,
                } => (*entity, range.clone(), text.to_string()),
                other => panic!("unexpected edit {other:?}"),
            })
            .collect();
        assert_eq!(edits, vec![(field, 0..1, "5".to_string())]);
    }
}
