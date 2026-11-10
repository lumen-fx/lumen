//! Script-driven animations: [`Node::animate`], the Web Animations API's
//! `element.animate(keyframes, options)`.
//!
//! Describe the keyframes and the timing once; the runtime plays them and the
//! returned [`Animation`] controls them. The element raises `animationend`
//! when one finishes and `animationcancel` when one is cancelled, and each
//! carries the handle under `animation` in its detail. Keyframes take
//! browser CSS names and values, the ones a `@keyframes` body takes.
//!
//! ```no_run
//! # extern crate lumenui_engine as lumenui;
//! use lumenui::prelude::*;
//!
//! fn pulse(card: Node) {
//!     let options = AnimateOptions {
//!         duration_ms: 400.0,
//!         easing: "ease-in-out".into(),
//!         ..AnimateOptions::default()
//!     };
//!     let frames = [
//!         Keyframe::new().set("transform", "scale(1)"),
//!         Keyframe::new().set("transform", "scale(1.05)"),
//!         Keyframe::new().set("transform", "scale(1)"),
//!     ];
//!     if let Ok(animation) = card.animate(&frames, &options) {
//!         animation.pause();
//!     }
//! }
//! ```

use std::collections::HashMap;

use crate::dom::Node;
use crate::lumen_script::{ScriptNs, ScriptValue, call_script_fn};

const NAMESPACE: &str = "animations";

fn call(name: &str, args: &[ScriptValue]) -> Result<ScriptValue, String> {
    call_script_fn(&ScriptNs::Named(NAMESPACE.to_string()), name, args).map_err(|e| e.to_string())
}

/// One keyframe: CSS declarations, and optionally where it sits and the
/// timing function to the next one.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct Keyframe {
    declarations: Vec<(String, String)>,
    offset: Option<f32>,
    easing: Option<String>,
}

impl Keyframe {
    /// An empty keyframe.
    pub fn new() -> Self {
        Self::default()
    }

    /// Set `property` (a browser CSS name such as `background-color`) to
    /// `value` at this keyframe.
    pub fn set(mut self, property: &str, value: &str) -> Self {
        self.declarations
            .push((property.to_string(), value.to_string()));
        self
    }

    /// Place the keyframe at `offset`, from `0` to `1`. Without one, the
    /// keyframes spread evenly.
    pub fn offset(mut self, offset: f32) -> Self {
        self.offset = Some(offset);
        self
    }

    /// The timing function from this keyframe to the next.
    pub fn easing(mut self, easing: &str) -> Self {
        self.easing = Some(easing.to_string());
        self
    }

    fn to_value(&self) -> ScriptValue {
        let mut map: HashMap<String, ScriptValue> = self
            .declarations
            .iter()
            .map(|(k, v)| (k.clone(), ScriptValue::Str(v.clone())))
            .collect();
        if let Some(offset) = self.offset {
            map.insert("offset".into(), ScriptValue::Str(offset.to_string()));
        }
        if let Some(easing) = &self.easing {
            map.insert("easing".into(), ScriptValue::Str(easing.clone()));
        }
        ScriptValue::Map(map)
    }
}

/// The timing of an animation, the Web Animations `KeyframeEffect` options.
#[derive(Debug, Clone, PartialEq)]
pub struct AnimateOptions {
    /// One iteration, in milliseconds.
    pub duration_ms: f64,
    /// The wait before it starts, in milliseconds; negative starts part-way.
    pub delay_ms: f64,
    /// How many iterations; `f64::INFINITY` runs forever.
    pub iterations: f64,
    /// `normal`, `reverse`, `alternate` or `alternate-reverse`.
    pub direction: String,
    /// `none`, `forwards`, `backwards` or `both`.
    pub fill: String,
    /// The timing function across each iteration, `spring()` included.
    pub easing: String,
}

impl Default for AnimateOptions {
    fn default() -> Self {
        Self {
            duration_ms: 0.0,
            delay_ms: 0.0,
            iterations: 1.0,
            direction: "normal".into(),
            fill: "none".into(),
            easing: "linear".into(),
        }
    }
}

impl AnimateOptions {
    fn to_value(&self) -> ScriptValue {
        // A negative count is how the script functions spell `Infinity`.
        let iterations = if self.iterations.is_infinite() {
            ScriptValue::F64(-1.0)
        } else {
            ScriptValue::F64(self.iterations)
        };
        ScriptValue::Map(HashMap::from([
            ("duration".to_string(), ScriptValue::F64(self.duration_ms)),
            ("delay".to_string(), ScriptValue::F64(self.delay_ms)),
            ("iterations".to_string(), iterations),
            (
                "direction".to_string(),
                ScriptValue::Str(self.direction.clone()),
            ),
            ("fill".to_string(), ScriptValue::Str(self.fill.clone())),
            ("easing".to_string(), ScriptValue::Str(self.easing.clone())),
        ]))
    }
}

/// A running animation [`Node::animate`] started. The getters read what the
/// last frame showed.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct Animation(i64);

impl Animation {
    /// The handle, the value an `animationend` event's `animation` detail
    /// holds.
    pub fn id(self) -> i64 {
        self.0
    }

    fn control(self, name: &str) {
        let _ = call(name, &[ScriptValue::I64(self.0)]);
    }

    /// Play it, or start it over once it has finished.
    pub fn play(self) {
        self.control("play");
    }

    /// Hold it where it is.
    pub fn pause(self) {
        self.control("pause");
    }

    /// Play it the other way from where it is.
    pub fn reverse(self) {
        self.control("reverse");
    }

    /// Jump to its end.
    pub fn finish(self) {
        self.control("finish");
    }

    /// Stop it and take its values off the element.
    pub fn cancel(self) {
        self.control("cancel");
    }

    /// Seek to `ms` milliseconds into its timeline.
    pub fn set_current_time(self, ms: f64) {
        let _ = call(
            "set_current_time",
            &[ScriptValue::I64(self.0), ScriptValue::F64(ms)],
        );
    }

    /// Play it at `rate`: `2.0` twice as fast, `-1.0` backwards.
    pub fn set_playback_rate(self, rate: f64) {
        let _ = call(
            "set_playback_rate",
            &[ScriptValue::I64(self.0), ScriptValue::F64(rate)],
        );
    }

    fn read(self, name: &str) -> Option<ScriptValue> {
        call(name, &[ScriptValue::I64(self.0)]).ok()
    }

    /// Milliseconds into its timeline.
    pub fn current_time(self) -> f64 {
        match self.read("current_time") {
            Some(ScriptValue::F64(ms)) => ms,
            _ => 0.0,
        }
    }

    /// The rate it plays at.
    pub fn playback_rate(self) -> f64 {
        match self.read("playback_rate") {
            Some(ScriptValue::F64(rate)) => rate,
            _ => 1.0,
        }
    }

    /// `running`, `paused`, `finished`, or `idle` once cancelled.
    pub fn play_state(self) -> String {
        match self.read("play_state") {
            Some(ScriptValue::Str(state)) => state,
            _ => "idle".to_string(),
        }
    }
}

impl Node {
    /// Animate this element through `keyframes`, timed by `options`. The
    /// element needs an `id`: the animation finds it by that.
    ///
    /// # Errors
    ///
    /// When the element has no `id`, a keyframe list is empty, or the app
    /// has no animations capability.
    pub fn animate(
        self,
        keyframes: &[Keyframe],
        options: &AnimateOptions,
    ) -> Result<Animation, String> {
        let id = self
            .id()
            .filter(|id| !id.is_empty())
            .ok_or("Node::animate: the element has no id")?;
        let frames = ScriptValue::Array(keyframes.iter().map(Keyframe::to_value).collect());
        match call(
            "animate",
            &[ScriptValue::Str(id), frames, options.to_value()],
        )? {
            ScriptValue::I64(id) => Ok(Animation(id)),
            other => Err(format!("animations::animate returned {other:?}")),
        }
    }
}
