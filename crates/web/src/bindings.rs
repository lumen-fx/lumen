//! What a `bind-*` attribute holds in the state a page is rendered with.
//!
//! A binding names a signal, and the markup beside it is the fallback the app
//! shows until that signal has a value. A build that knows the value writes
//! it, the same way it takes the branch an `<if>` gate resolves to and writes
//! the rows a `<for>` iterates. What it does not know it leaves alone, so the
//! fallback stays exactly where the author put it.
//!
//! The resolved value is written into the attribute the emitter already
//! projects, rather than into a form of its own: a bound text lands on
//! [`Attributes::text`] and reaches the page through the same path an authored
//! `text=` does. That keeps one description of what an element becomes, and it
//! is what makes the runtime's first pass over the page a no-op: it computes
//! the same value from the same signal and finds the document already saying
//! it.
//!
//! Which bindings a build can honour, and what each becomes:
//!
//! - `bind-text` - the element's text, which is its value on a form control.
//! - `bind-checked` - the checked state, from a signal that states a boolean.
//! - `bind-value` - the control's value, clamped the way the widget clamps it.
//! - `bind-disabled` - the disabled state, from a signal that states a boolean.
//! - `bind-scroll` - nothing. Where a container is scrolled to is the
//!   browser's own scroll position, which no attribute in the document sets.
//! - `bind-text` / `bind-value` / `bind-checked` in their `$self.` and
//!   `$parent.` forms - nothing. Those read a property of one entity, and a
//!   page's state is signals and rows; there is no entity to read yet.

use lumen_core::components::SliderValue;
use lumen_core::signals::signal_as_bool;
use lumen_ir::layout_ir::{Attributes, BindKind};
use lumen_primitives::ProgressBar;

use crate::spec::SignalEnv;

/// The attributes to emit an element with, once its bindings are resolved
/// against `signals`.
///
/// `None` when no binding on the element resolved to anything, which is every
/// element of a page with no state and most elements of a page with some.
/// The caller emits from the element's own attributes then, and nothing is
/// copied.
pub fn resolved(ir_tag: &str, attrs: &Attributes, signals: &SignalEnv) -> Option<Attributes> {
    let text = bound(attrs, BindKind::Text, signals).map(str::to_string);
    let checked = bound(attrs, BindKind::Checked, signals).and_then(signal_as_bool);
    let value = bound(attrs, BindKind::Value, signals)
        .and_then(|value| value.parse::<f32>().ok())
        .map(|value| clamp(ir_tag, attrs, value));
    let disabled = attrs
        .bind_disabled
        .as_deref()
        .and_then(|signal| signals.global(signal))
        .and_then(signal_as_bool);
    if text.is_none() && checked.is_none() && value.is_none() && disabled.is_none() {
        return None;
    }

    let mut out = attrs.clone();
    if let Some(text) = text {
        out.text = Some(text);
    }
    if let Some(checked) = checked {
        out.checked = Some(checked);
    }
    if let Some(value) = value {
        out.value = Some(value);
    }
    if let Some(disabled) = disabled {
        out.disabled = disabled;
    }
    Some(out)
}

/// The value of the signal an element's `kind` binding names, when the page's
/// state holds one.
fn bound<'a>(attrs: &Attributes, kind: BindKind, signals: &'a SignalEnv) -> Option<&'a str> {
    let spec = attrs.bind.as_ref().filter(|spec| spec.kind == kind)?;
    signals.global(&spec.name)
}

/// A bound value held to the bounds the widget holds it to, so the page shows
/// what the app would and the runtime finds nothing to correct.
///
/// The widget owns its own rule and its own defaults, and both arrive here
/// through the same conversion the spawner builds the widget with. Reading
/// `min` and `max` off the attributes and clamping here would be a second
/// copy: it would agree today and drift the day a widget's bounds change,
/// and the drift would surface as the runtime correcting nodes on load.
///
/// A tag that carries neither widget has no value to hold, so the value is
/// left as the markup wrote it.
fn clamp(ir_tag: &str, attrs: &Attributes, value: f32) -> f32 {
    match ir_tag {
        "slider" => SliderValue::from(attrs).clamp(value),
        "progress" => ProgressBar::from(attrs).clamp(value),
        _ => value,
    }
}

#[cfg(test)]
mod tests {
    use lumen_ir::layout_ir::BindSpec;

    use super::*;

    fn bind(kind: BindKind, name: &str) -> Attributes {
        Attributes {
            bind: Some(BindSpec {
                kind,
                name: name.to_string(),
            }),
            ..Attributes::default()
        }
    }

    #[test]
    fn a_binding_the_state_answers_replaces_the_fallback() {
        let mut attrs = bind(BindKind::Text, "name");
        attrs.text = Some("(unknown)".to_string());
        let signals = SignalEnv::new().with_global("name", "Ada Lovelace");
        let resolved = resolved("label", &attrs, &signals).expect("the state holds `name`");
        assert_eq!(resolved.text.as_deref(), Some("Ada Lovelace"));
    }

    #[test]
    fn a_binding_the_state_has_nothing_for_keeps_it() {
        let mut attrs = bind(BindKind::Text, "name");
        attrs.text = Some("(unknown)".to_string());
        assert!(
            resolved("label", &attrs, &SignalEnv::new()).is_none(),
            "an unset signal leaves the element as the author wrote it"
        );
    }

    #[test]
    fn a_boolean_state_follows_a_signal_that_states_one() {
        let attrs = bind(BindKind::Checked, "on");
        for (value, want) in [("true", true), ("1", true), ("false", false), ("0", false)] {
            let signals = SignalEnv::new().with_global("on", value);
            assert_eq!(
                resolved("checkbox", &attrs, &signals).and_then(|a| a.checked),
                Some(want),
                "`{value}` states a boolean"
            );
        }
        let signals = SignalEnv::new().with_global("on", "maybe");
        assert!(
            resolved("checkbox", &attrs, &signals).is_none(),
            "and a value that states none leaves the control as it was"
        );
    }

    #[test]
    fn a_bound_value_is_held_to_the_bounds_the_widget_holds_it_to() {
        let mut slider = bind(BindKind::Value, "level");
        slider.min = Some(0.0);
        slider.max = Some(100.0);
        let signals = SignalEnv::new().with_global("level", "250");
        assert_eq!(
            resolved("slider", &slider, &signals).and_then(|a| a.value),
            Some(100.0)
        );

        let mut bar = bind(BindKind::Value, "level");
        bar.max = Some(10.0);
        let signals = SignalEnv::new().with_global("level", "-4");
        assert_eq!(
            resolved("progress", &bar, &signals).and_then(|a| a.value),
            Some(0.0),
            "a progress bar starts at zero however far below it the signal is"
        );

        let signals = SignalEnv::new().with_global("level", "loud");
        assert!(
            resolved("slider", &slider, &signals).is_none(),
            "a value that is not a number is not a position"
        );
    }

    #[test]
    fn the_disabled_state_reads_its_own_signal() {
        let mut attrs = bind(BindKind::Text, "name");
        attrs.bind_disabled = Some("busy".to_string());
        let signals = SignalEnv::new().with_global("busy", "true");
        let resolved = resolved("button", &attrs, &signals).expect("the state holds `busy`");
        assert!(
            resolved.disabled,
            "`bind-disabled` sits beside another binding rather than replacing it"
        );
    }

    #[test]
    fn a_binding_with_no_static_form_is_left_to_the_runtime() {
        let mut attrs = Attributes {
            bind_scroll: Some("offset".to_string()),
            bind_self_text: Some("title".to_string()),
            ..Attributes::default()
        };
        attrs.bind_parent_checked = Some("done".to_string());
        let signals = SignalEnv::new()
            .with_global("offset", "120")
            .with_global("title", "Recent")
            .with_global("done", "true");
        assert!(resolved("scroll", &attrs, &signals).is_none());
    }
}
